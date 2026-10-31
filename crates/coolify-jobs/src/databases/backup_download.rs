//! `Traits/PreparesBackupDownloads`, `PrepareFileDownloadJob` (host files only), `DeleteFileDownloadJob` and the
//! parts of `FileDownloadStorageService` they use: the server uploads a backup to the instance download storage and
//! the browser downloads it from there. The results live in memory, like the cache entries of upstream.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use coolify_db::database_parts as parts;
use coolify_db::instance_settings;
use coolify_db::records::Record;
use coolify_domain::new_public_id;
use coolify_remote::{RunOptions, Target, shell::quote};
use serde_json::{Value, json};

use super::connection::rawurlencode;
use super::s3::S3Target;
use crate::Jobs;
use crate::compose_parser::validate::validate_shell_safe_path;
use crate::server::ServerInfo;

const PREFIX: &str = "coolify-file-downloads/";
/// `FileDownloadStorageService::LINK_TTL_MINUTES`.
const LINK_TTL: Duration = Duration::from_secs(15 * 60);
/// `OBJECT_TTL_HOURS`.
const OBJECT_TTL: Duration = Duration::from_secs(6 * 3600);
/// `MAX_SIZE`.
const MAX_SIZE: i64 = 5 * 1024 * 1024 * 1024;
/// `RateLimiter::attempt('file-browser-s3-download:'.auth()->id(), 10, ..., 3600)`.
const RATE_LIMIT: usize = 10;
const RATE_WINDOW: Duration = Duration::from_secs(3600);

type Results = HashMap<String, (i64, Value, Instant)>;
static RESULTS: Mutex<Option<Results>> = Mutex::new(None);
static ATTEMPTS: Mutex<Option<HashMap<i64, Vec<Instant>>>> = Mutex::new(None);

/// What `prepareBackupDownload` tells the page.
#[derive(Debug, PartialEq, Eq)]
pub enum PreparedDownload {
    /// `open-backup-download`: the Coolify download route answers.
    Stream,
    /// `Too many downloads are in progress...`.
    TooMany,
    /// The job runs; poll the id.
    Pending { id: String, file: String },
}

/// `contentDisposition`.
fn content_disposition(name: &str) -> String {
    let fallback: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let fallback = if fallback.is_empty() {
        "download".to_owned()
    } else {
        fallback
    };
    format!(
        "attachment; filename=\"{fallback}\"; filename*=UTF-8''{}",
        rawurlencode(name)
    )
}

/// `buildHostFileSizeCommand`.
pub fn host_file_size_command(path: &str) -> Result<String, String> {
    validate_shell_safe_path(path, "download path")?;
    let escaped = quote(path);
    Ok(format!(
        "sh -c {}",
        quote(&format!(
            "if [ -f {escaped} ]; then stat -L -c %s {escaped}; fi"
        ))
    ))
}

/// `buildHostFileUploadCommand`.
pub fn host_file_upload_command(
    path: &str,
    url: &str,
    disposition: &str,
) -> Result<String, String> {
    validate_shell_safe_path(path, "download path")?;
    Ok(format!(
        "sh -c {}",
        quote(&format!(
            "curl -fsS --retry 2 -H {} -T {} {}",
            quote(&format!("Content-Disposition: {disposition}")),
            quote(path),
            quote(url)
        ))
    ))
}

fn store(id: &str, user_id: i64, result: Value) {
    let mut guard = RESULTS.lock().unwrap_or_else(|p| p.into_inner());
    let map = guard.get_or_insert_with(HashMap::new);
    map.retain(|_, (_, _, at)| at.elapsed() < LINK_TTL);
    map.insert(id.to_owned(), (user_id, result, Instant::now()));
}

/// `checkDownloads`: the finished result of the user's download, handed out once.
pub fn take_download_result(id: &str, user_id: i64) -> Option<Value> {
    let mut guard = RESULTS.lock().unwrap_or_else(|p| p.into_inner());
    let map = guard.get_or_insert_with(HashMap::new);
    if map
        .get(id)
        .is_some_and(|(owner, _, at)| *owner == user_id && at.elapsed() < LINK_TTL)
    {
        return map.remove(id).map(|(_, v, _)| v);
    }
    None
}

fn rate_limited(user_id: i64) -> bool {
    let mut guard = ATTEMPTS.lock().unwrap_or_else(|p| p.into_inner());
    let list = guard
        .get_or_insert_with(HashMap::new)
        .entry(user_id)
        .or_default();
    list.retain(|at| at.elapsed() < RATE_WINDOW);
    if list.len() >= RATE_LIMIT {
        return true;
    }
    list.push(Instant::now());
    false
}

impl Jobs {
    /// `FileDownloadStorageService::configuredStorage`.
    pub async fn download_storage(&self) -> Option<Record> {
        let db = &self.0.db;
        let settings = instance_settings::row(db).await.ok()?;
        let id = settings
            .get("file_download_s3_storage_id")
            .and_then(Value::as_i64)?;
        let row = parts::s3_by_id(db, id).await.ok().flatten()?;
        (row.get("team_id").and_then(Value::as_i64) == Some(0)
            && row.get("is_usable").and_then(Value::as_bool) == Some(true))
        .then_some(row)
    }

    /// `prepareBackupDownload`: a missing server or file name means the local file is gone.
    pub async fn prepare_backup_download(
        &self,
        user_id: i64,
        server: Option<&ServerInfo>,
        filename: Option<&str>,
    ) -> Result<PreparedDownload, String> {
        let Some(storage) = self.download_storage().await else {
            return Ok(PreparedDownload::Stream);
        };
        let (Some(server), Some(filename)) = (server, filename.filter(|f| !f.is_empty())) else {
            return Ok(PreparedDownload::Stream);
        };
        let target = server.target(self).await?;
        let command = host_file_size_command(filename)?;
        let size = self
            .remote()
            .run(
                &target,
                &[command.as_str()],
                RunOptions::op("backup.download"),
            )
            .await
            .map_err(|e| e.message)?
            .and_then(|o| o.trim().parse::<i64>().ok());
        let Some(size) = size.filter(|s| *s <= MAX_SIZE) else {
            return Ok(PreparedDownload::Stream);
        };
        if rate_limited(user_id) {
            return Ok(PreparedDownload::TooMany);
        }
        let _ = size;
        let id = super::import::uuid4();
        let jobs = self.clone();
        let (job_id, path) = (id.clone(), filename.to_owned());
        tokio::spawn(async move {
            let result = match jobs.prepare_file_download(&storage, &target, &path).await {
                Ok(url) => json!({"status": "ready", "url": url}),
                Err(e) => {
                    // The message can contain the S3 endpoint or a presigned URL; keep it in the log only.
                    tracing::warn!(download_id = %job_id, error = %e, "S3 download failed");
                    json!({"status": "failed", "message": "Download failed. The file could not be uploaded to the download storage. Try again, or ask your administrator to check the storage settings."})
                }
            };
            store(&job_id, user_id, result);
        });
        let file = filename.rsplit('/').next().unwrap_or(filename).to_owned();
        Ok(PreparedDownload::Pending { id, file })
    }

    /// `PrepareFileDownloadJob::handle` for a file on the server host.
    async fn prepare_file_download(
        &self,
        storage: &Record,
        target: &Target,
        path: &str,
    ) -> Result<String, String> {
        let enc = self
            .0
            .encrypter
            .as_ref()
            .ok_or("No application encryption key has been specified.")?;
        let s3 = S3Target::from_row(enc, storage)?;
        // `newObjectKey`
        let mut key = String::from(PREFIX);
        while key.len() < PREFIX.len() + 48 {
            key.push_str(&new_public_id());
        }
        key.truncate(PREFIX.len() + 48);
        // Queue the delete first, so a partial upload is removed as well.
        let (delete_row, delete_key) = (storage.clone(), key.clone());
        let jobs = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(OBJECT_TTL).await;
            if let Some(enc) = jobs.0.encrypter.as_ref()
                && let Ok(s3) = S3Target::from_row(enc, &delete_row)
                && let Err(e) = s3.delete_objects(&[delete_key]).await
            {
                tracing::warn!(error = %e, "file download object cleanup failed");
            }
        });
        let name = path.rsplit('/').next().unwrap_or(path);
        let disposition = content_disposition(name);
        let url = s3.presigned_put(&key, 3600, &disposition)?;
        let command = host_file_upload_command(path, &url, &disposition)?;
        self.remote()
            .run(
                target,
                &[command.as_str()],
                RunOptions::op("backup.download").timeout(Duration::from_secs(3600)),
            )
            .await
            .map_err(|e| e.message)?;
        s3.presigned_get(&key, LINK_TTL.as_secs())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_disposition_matches_upstream() {
        assert_eq!(
            content_disposition("pg dump.gz"),
            "attachment; filename=\"pg_dump.gz\"; filename*=UTF-8''pg%20dump.gz"
        );
    }

    #[test]
    fn commands_match_upstream_shape() {
        assert_eq!(
            host_file_size_command("/data/a.gz").unwrap(),
            r#"sh -c 'if [ -f '\''/data/a.gz'\'' ]; then stat -L -c %s '\''/data/a.gz'\''; fi'"#
        );
        assert!(host_file_size_command("/data/$(x)").is_err());
        assert!(
            host_file_upload_command("/data/a.gz", "http://s3/x?y=1", "attachment")
                .unwrap()
                .starts_with("sh -c 'curl -fsS --retry 2 -H ")
        );
    }

    #[test]
    fn results_go_to_their_user_once() {
        store("id-1", 7, json!({"status": "ready"}));
        assert_eq!(take_download_result("id-1", 8), None);
        assert!(take_download_result("id-1", 7).is_some());
        assert_eq!(take_download_result("id-1", 7), None);
    }
}
