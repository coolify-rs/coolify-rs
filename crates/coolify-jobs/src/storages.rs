//! `LocalFileVolume`: its path rules (`bootstrap/helpers/shared.php`) and what it does on servers
//! (`saveStorageOnServer`, `deleteStorageOnServer`, `assertHostPathOnServer`), generic over the
//! owning resource through [`StorageOwner`].

use coolify_compat::yaml::PhpValue;
use coolify_db::applications::ApplicationRow;
use coolify_db::servers::{self, ColumnValue};
use coolify_db::storages::{self as db_storages, StorageKind};
use coolify_remote::RunOptions;
use coolify_remote::shell::quote;

use crate::Jobs;
use crate::compose_parser::php;
use crate::compose_parser::validate::{
    parse_docker_compose_yaml, parse_docker_volume_string, validate_shell_safe_path,
};
use crate::compose_parser::volumes::{
    legacy_replace_local_source, replace_local_source, source_is_local,
};
use crate::server::ServerInfo;

/// `LocalFileVolume::REMOTE_PATH_CONFINEMENT_SCRIPT`.
const REMOTE_PATH_CONFINEMENT_SCRIPT: &str = r#"resolve() {
    path=$1
    rest=
    case $path in /*) ;; *) return 1 ;; esac
    while [ ! -e "$path" ]; do
        if [ -L "$path" ]; then return 1; fi
        rest=/${path##*/}$rest
        path=${path%/*}
        [ -n "$path" ] || path=/
    done
    case "$rest/" in */./*|*/../*) return 1 ;; esac
    path=$(readlink -f "$path") || return 1
    printf "%s\n" "${path%/}$rest"
}
base=$(resolve "$1") || exit 1
target=$(resolve "$2") || exit 1
case $target in "$base"|"$base"/*) echo OK ;; *) echo NOK ;; esac"#;

/// `LocalFileVolume::REMOTE_FILE_STATE_SCRIPT`.
const REMOTE_FILE_STATE_SCRIPT: &str = r#"i=0
for path in "$@"; do
    i=$((i + 1))
    if [ -L "$path" ]; then
        if [ -f "$path" ]; then state=file; else state=other; fi
    elif [ -f "$path" ]; then
        state=file
    elif [ -d "$path" ]; then
        state=directory
        if entries=$(ls -A "$path" 2>/dev/null); then
            if [ -z "$entries" ]; then state=empty-directory; fi
        fi
    elif [ -e "$path" ]; then
        state=other
    else
        state=missing
    fi
    printf "%s:%s\n" "$i" "$state"
done"#;

/// `LocalFileVolume::MAX_CONTENT_SIZE`.
const MAX_CONTENT_SIZE: u64 = 5_242_880;
/// `LocalFileVolume::TOO_LARGE_PLACEHOLDER`.
const TOO_LARGE_PLACEHOLDER: &str = "[file too large to display]";

// ---------------------------------------------------------------------------
// Path rules

/// `normalizeUnixPath($path)`.
pub fn normalize_unix_path(path: &str) -> Result<String, String> {
    validate_shell_safe_path(path, "path")?;
    if path.contains('\0') {
        return Err("Invalid path: contains null byte.".into());
    }
    if path.contains('\\') {
        return Err("Invalid path: backslash directory separators are not allowed.".into());
    }
    let absolute = path.starts_with('/');
    let mut segments: Vec<&str> = Vec::new();
    for segment in path.split('/') {
        if segment.is_empty() || segment == "." {
            continue;
        }
        if segment == ".." {
            if segments.is_empty() || segments.last() == Some(&"..") {
                if absolute {
                    return Err("Invalid path: resolved path escapes the base directory.".into());
                }
                segments.push(segment);
                continue;
            }
            segments.pop();
            continue;
        }
        segments.push(segment);
    }
    let normalized = segments.join("/");
    Ok(match (absolute, normalized.is_empty()) {
        (true, true) => "/".to_owned(),
        (true, false) => format!("/{normalized}"),
        (false, true) => ".".to_owned(),
        (false, false) => normalized,
    })
}

fn collapse_slashes(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    let mut slash = false;
    for c in path.chars() {
        if c != '/' || !slash {
            out.push(c);
        }
        slash = c == '/';
    }
    out
}

/// `validateFileMountPath($input, $context)`.
pub fn validate_file_mount_path(input: &str, context: &str) -> Result<String, String> {
    validate_shell_safe_path(input, context)?;
    if input.contains('\0') {
        return Err(format!(
            "Invalid {context}: contains null byte. Null bytes are not allowed in file mount paths for security reasons."
        ));
    }
    if input.contains('\\') {
        return Err(format!(
            "Invalid {context}: backslash directory separators are not allowed."
        ));
    }
    let path = collapse_slashes(&format!("/{}", input.trim().trim_start_matches('/')));
    if path
        .trim_matches('/')
        .split('/')
        .any(|s| s == "." || s == "..")
    {
        return Err(format!(
            "Invalid {context}: relative path segments ('.' or '..') are not allowed."
        ));
    }
    Ok(path)
}

/// `validateHostFileMountPath($input, $context)`.
pub fn validate_host_file_mount_path(input: &str, context: &str) -> Result<String, String> {
    validate_shell_safe_path(input, context)?;
    if input.contains('\0') {
        return Err(format!("Invalid {context}: contains null byte."));
    }
    if input.contains('\\') {
        return Err(format!(
            "Invalid {context}: backslash directory separators are not allowed."
        ));
    }
    let path = collapse_slashes(input.trim());
    if path.is_empty() || !path.starts_with('/') {
        return Err(format!("Invalid {context}: must be an absolute path."));
    }
    if path == "/" || path.ends_with('/') {
        return Err(format!(
            "Invalid {context}: must point to a file, not a directory."
        ));
    }
    if path
        .trim_matches('/')
        .split('/')
        .any(|s| s == "." || s == "..")
    {
        return Err(format!(
            "Invalid {context}: relative path segments ('.' or '..') are not allowed."
        ));
    }
    normalize_unix_path(&path)
}

/// `confineFileMountPath($baseDirectory, $path, $context)`.
pub fn confine_file_mount_path(base: &str, path: &str, context: &str) -> Result<String, String> {
    let base = normalize_unix_path(base)?;
    let mount = validate_file_mount_path(path, context)?;
    let resolved = normalize_unix_path(&format!("{base}/{mount}"))?;
    if resolved != base && !resolved.starts_with(&format!("{base}/")) {
        return Err(format!(
            "Invalid {context}: resolved path must stay inside the resource configuration directory."
        ));
    }
    Ok(resolved)
}

/// `confinePathToBase($baseDirectory, $path, $context)`.
pub fn confine_path_to_base(base: &str, path: &str, context: &str) -> Result<String, String> {
    let base = normalize_unix_path(base)?;
    let path = path.trim();
    let joined = if let Some(rest) = path.strip_prefix('.') {
        format!("{base}/{rest}")
    } else if path.starts_with('/') {
        path.to_owned()
    } else {
        format!("{base}/{path}")
    };
    let resolved = normalize_unix_path(&joined)?;
    if resolved != base && !resolved.starts_with(&format!("{base}/")) {
        return Err(format!(
            "Invalid {context}: resolved path must stay inside the resource configuration directory."
        ));
    }
    Ok(resolved)
}

/// `LocalFileVolume::resourceDirectoryContaining($path, $directories)`.
fn resource_directory_containing<'a>(path: &str, directories: &'a [String]) -> Option<&'a str> {
    directories
        .iter()
        .find(|d| path == d.as_str() || path.starts_with(&format!("{d}/")))
        .map(String::as_str)
}

/// `LocalFileVolume::resolveHostPath($resourceDirectories, $path, $context)`.
pub fn resolve_host_path(
    resource_directories: &[String],
    path: &str,
    context: &str,
) -> Result<String, String> {
    let directories = resource_directories
        .iter()
        .map(|d| normalize_unix_path(d))
        .collect::<Result<Vec<_>, _>>()?;
    let path = path.trim();
    if path.is_empty() {
        return Err(format!("Invalid {context}: the path is empty."));
    }
    if path.starts_with('~') {
        return Err(format!(
            "Invalid {context}: use an absolute path instead of ~."
        ));
    }
    validate_shell_safe_path(path, context)?;
    let absolute = path.starts_with('/');
    let first = directories.first().map_or("", String::as_str);
    let resolved = normalize_unix_path(&if absolute {
        path.to_owned()
    } else {
        format!("{first}/{path}")
    })?;
    if resource_directory_containing(&resolved, &directories).is_some() {
        return Ok(resolved);
    }
    if !absolute {
        return Err(format!(
            "Invalid {context}: a relative path must stay inside the resource directory."
        ));
    }
    if path.split('/').any(|s| s == "." || s == "..") {
        return Err(format!(
            "Invalid {context}: '.' and '..' segments are not allowed."
        ));
    }
    if resolved == "/" {
        return Err(format!(
            "Invalid {context}: the root directory cannot be mounted."
        ));
    }
    Ok(resolved)
}

/// `LocalFileVolume::resolvedFsPath($workdir)`.
pub fn resolved_fs_path(fs_path: &str, workdir: &str) -> String {
    let path = fs_path.trim();
    if path.starts_with('/') || path.starts_with('~') {
        return path.to_owned();
    }
    let path = path.strip_prefix('.').unwrap_or(path);
    let path = path.trim_start_matches('/');
    let workdir = workdir.trim_end_matches('/');
    if path.is_empty() {
        workdir.to_owned()
    } else {
        format!("{workdir}/{path}")
    }
}

/// `LocalFileVolume::isOutsideResourceDirectory()`: a path that cannot be resolved counts as outside.
pub fn is_outside_resource_directory(fs_path: &str, base_directories: &[String]) -> bool {
    let first = base_directories.first().map_or("", String::as_str);
    let Ok(path) = normalize_unix_path(&resolved_fs_path(fs_path, first)) else {
        return true;
    };
    let Ok(directories) = base_directories
        .iter()
        .map(|d| normalize_unix_path(d))
        .collect::<Result<Vec<_>, _>>()
    else {
        return true;
    };
    path.starts_with('~') || resource_directory_containing(&path, &directories).is_none()
}

// ---------------------------------------------------------------------------
// The owner and the row

/// What `LocalFileVolume` reads from the resource that owns it.
#[derive(Debug, Clone)]
pub struct StorageOwner {
    /// `$resource->workdir()`.
    pub workdir: String,
    /// `contentBaseDirectories()`.
    pub base_directories: Vec<String>,
    /// `composeSourceDirectory()`.
    pub compose_source_directory: String,
    /// `data_get($this->resource, 'docker_compose_raw')`.
    pub docker_compose_raw: Option<String>,
    /// `servers()`: the main server and the additional servers.
    pub server_ids: Vec<i64>,
}

impl StorageOwner {
    /// The owner of an application's storages.
    pub async fn for_application(
        db: &coolify_db::Db,
        a: &ApplicationRow,
    ) -> coolify_db::DbResult<Self> {
        let workdir = format!("/data/coolify/applications/{}", a.uuid);
        let mut server_ids: Vec<i64> = a.server_id().into_iter().collect();
        for id in db_storages::additional_server_ids(db, a.id).await? {
            if !server_ids.contains(&id) {
                server_ids.push(id);
            }
        }
        Ok(Self {
            base_directories: vec![workdir.clone()],
            compose_source_directory: workdir.clone(),
            workdir,
            docker_compose_raw: a.str("docker_compose_raw").map(str::to_owned),
            server_ids,
        })
    }
}

/// The columns of a `local_file_volumes` row these operations use, with `content` decrypted.
#[derive(Clone)]
pub struct FileStorage {
    pub id: i64,
    pub fs_path: String,
    pub mount_path: Option<String>,
    pub content: Option<String>,
    pub is_directory: bool,
    pub is_host_file: bool,
    pub chmod: Option<String>,
    pub chown: Option<String>,
}

impl std::fmt::Debug for FileStorage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileStorage")
            .field("id", &self.id)
            .field("fs_path", &self.fs_path)
            .finish_non_exhaustive()
    }
}

/// `isAdminControlledComposeMount()`: raw Compose bind mounts keep the host path the administrator wrote.
fn is_admin_controlled_compose_mount(owner: &StorageOwner, file: &FileStorage) -> bool {
    let Some(compose) = owner
        .docker_compose_raw
        .as_deref()
        .filter(|c| !c.is_empty())
    else {
        return false;
    };
    let Ok(parsed) = parse_docker_compose_yaml(compose) else {
        return false;
    };
    let Some(PhpValue::Array(services)) = php::data_get(&parsed, "services") else {
        return false;
    };
    let Ok(fs_path) = normalize_unix_path(&file.fs_path) else {
        return false;
    };
    let source_directory = owner.compose_source_directory.as_str();
    for (_, service) in services {
        let Some(PhpValue::Array(volumes)) = php::data_get(service, "volumes") else {
            continue;
        };
        for (_, volume) in volumes {
            let (source, target) = if let PhpValue::Str(s) = volume {
                let Ok(parsed) = parse_docker_volume_string(s) else {
                    return false;
                };
                (
                    parsed.source.unwrap_or_default(),
                    parsed.target.unwrap_or_default(),
                )
            } else {
                let text = |k: &str| php::data_get(volume, k).map(php::to_string);
                (
                    text("source").unwrap_or_default(),
                    text("target").unwrap_or_default(),
                )
            };
            if file.mount_path.as_deref() != Some(target.as_str()) || !source_is_local(&source) {
                continue;
            }
            let Ok(legacy) =
                normalize_unix_path(&legacy_replace_local_source(&source, source_directory))
            else {
                return false;
            };
            if legacy == fs_path {
                return true;
            }
            let Ok(resolved) = replace_local_source(&source, source_directory) else {
                continue;
            };
            let Ok(resolved) = normalize_unix_path(&resolved) else {
                return false;
            };
            if resolved == fs_path {
                return true;
            }
        }
    }
    false
}

/// `hostPathAndResourceDirectory()`.
fn host_path_and_resource_directory(
    owner: &StorageOwner,
    file: &FileStorage,
) -> Result<(String, Option<String>), String> {
    let first = owner.base_directories.first().map_or("", String::as_str);
    let path = resolve_host_path(
        &owner.base_directories,
        &resolved_fs_path(&file.fs_path, first),
        "storage path",
    )?;
    let directories = owner
        .base_directories
        .iter()
        .map(|d| normalize_unix_path(d))
        .collect::<Result<Vec<_>, _>>()?;
    let directory = resource_directory_containing(&path, &directories).map(str::to_owned);
    Ok((path, directory))
}

/// `sh -c <script> sh <args>`: one line, so the sudo parser only prefixes it.
fn sh_command(script: &str, args: &[&str]) -> String {
    let args: Vec<String> = args.iter().map(|a| quote(a)).collect();
    format!("sh -c {} sh {}", quote(script), args.join(" "))
}

impl Jobs {
    async fn storage_server(
        &self,
        id: i64,
    ) -> Result<(ServerInfo, coolify_remote::Target), String> {
        let row = servers::find_by_id(&self.0.db, id)
            .await
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("No query results for model [App\\Models\\Server] {id}"))?;
        let server = ServerInfo::from_row(&row);
        let target = server.target(self).await?;
        Ok((server, target))
    }

    /// `instant_remote_process([...], $server)`.
    async fn storage_run(
        &self,
        target: &coolify_remote::Target,
        commands: &[String],
    ) -> Result<String, String> {
        let refs: Vec<&str> = commands.iter().map(String::as_str).collect();
        self.remote()
            .run(target, &refs, RunOptions::op("storage"))
            .await
            .map(Option::unwrap_or_default)
            .map_err(|e| e.message)
    }

    /// `LocalFileVolume::assertRemotePathIsConfined($baseDirectory, $path, $server)`.
    async fn assert_remote_path_is_confined(
        &self,
        target: &coolify_remote::Target,
        base: &str,
        path: &str,
    ) -> Result<(), String> {
        let command = sh_command(REMOTE_PATH_CONFINEMENT_SCRIPT, &[base, path]);
        let result = self
            .remote()
            .run_quiet(target, &[&command], RunOptions::op("storage"))
            .await
            .unwrap_or_default();
        if result.trim() == "OK" {
            Ok(())
        } else {
            Err("Invalid storage path: resolved path must stay inside the resource configuration directory.".into())
        }
    }

    /// `LocalFileVolume::assertRemotePathIsNotSymlink($path, $server)`.
    async fn assert_remote_path_is_not_symlink(
        &self,
        target: &coolify_remote::Target,
        path: &str,
    ) -> Result<(), String> {
        let escaped = quote(path);
        let result = self
            .storage_run(
                target,
                &[format!("test -L {escaped} && echo LINK || echo OK")],
            )
            .await?;
        if result.trim() == "OK" {
            Ok(())
        } else {
            Err(format!(
                "{path} is a symbolic link on the server. Coolify does not write a file through a symbolic link. Remove the link or use another path."
            ))
        }
    }

    /// `LocalFileVolume::assertHostPathOnServer($resourceDirectory, $path, $server, $isDirectory)`.
    pub async fn assert_host_path_on_server(
        &self,
        resource_directory: &str,
        path: &str,
        server_id: i64,
        is_directory: bool,
    ) -> Result<(), String> {
        let (_, target) = self.storage_server(server_id).await?;
        let normalized = normalize_unix_path(resource_directory)?;
        if resource_directory_containing(path, std::slice::from_ref(&normalized)).is_some() {
            self.assert_remote_path_is_confined(&target, resource_directory, path)
                .await
        } else if is_directory {
            Ok(())
        } else {
            self.assert_remote_path_is_not_symlink(&target, path).await
        }
    }

    /// `LocalFileVolume::remoteFileStates([$path], $server)[0]`.
    async fn remote_file_state(&self, target: &coolify_remote::Target, path: &str) -> String {
        let command = sh_command(REMOTE_FILE_STATE_SCRIPT, &[path]);
        let output = self
            .remote()
            .run_quiet(target, &[&command], RunOptions::op("storage"))
            .await
            .unwrap_or_default();
        output
            .lines()
            .find_map(|l| l.trim().strip_prefix("1:").map(str::to_owned))
            .unwrap_or_else(|| "unknown".to_owned())
    }

    /// `remoteFileExceedsLimit` and `readRemoteFileContent` for a file marked as a directory.
    async fn read_for_mismatch(&self, target: &coolify_remote::Target, escaped: &str) -> String {
        let size = self
            .remote()
            .run_quiet(
                target,
                &[&format!("stat -c%s {escaped} 2>/dev/null")],
                RunOptions::op("storage"),
            )
            .await
            .unwrap_or_default();
        if size.trim().parse::<u64>().unwrap_or(0) > MAX_CONTENT_SIZE {
            return TOO_LARGE_PLACEHOLDER.to_owned();
        }
        let limit = MAX_CONTENT_SIZE + 1;
        let content = self
            .remote()
            .run_quiet(
                target,
                &[&format!("head -c {limit} {escaped}")],
                RunOptions::op("storage"),
            )
            .await
            .unwrap_or_default();
        if content.len() as u64 > MAX_CONTENT_SIZE {
            TOO_LARGE_PLACEHOLDER.to_owned()
        } else {
            content
        }
    }

    /// `$this->is_directory = ...; $this->content = ...; $this->save()` when the server disagrees.
    async fn save_file_state(
        &self,
        file: &FileStorage,
        content: Option<&str>,
        is_directory: bool,
    ) -> Result<(), String> {
        let mut dirty = vec![("is_directory", ColumnValue::Bool(is_directory))];
        if let (Some(c), Some(enc)) = (content, self.0.encrypter.as_ref()) {
            dirty.push(("content", ColumnValue::Text(Some(enc.encrypt_string(c)))));
        }
        db_storages::save_dirty(&self.0.db, StorageKind::File, file.id, &dirty, Self::now())
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// `saveStorageOnServer($server)`.
    #[allow(clippy::too_many_lines)]
    async fn save_storage_on(
        &self,
        owner: &StorageOwner,
        file: &FileStorage,
        server_id: i64,
    ) -> Result<(), String> {
        let (_, target) = self.storage_server(server_id).await?;
        let workdir = owner.workdir.as_str();
        let mut commands: Vec<String> = Vec::new();
        let mut path = resolved_fs_path(&file.fs_path, workdir);
        if !is_admin_controlled_compose_mount(owner, file) {
            let (host_path, directory) = host_path_and_resource_directory(owner, file)?;
            match directory {
                None => {
                    if !file.is_directory {
                        self.assert_remote_path_is_not_symlink(&target, &host_path)
                            .await?;
                    }
                }
                Some(d) => {
                    self.assert_remote_path_is_confined(&target, &d, &host_path)
                        .await?;
                }
            }
            path = host_path;
        }
        let outside = is_outside_resource_directory(&file.fs_path, &owner.base_directories);

        if file.is_directory {
            validate_shell_safe_path(&path, "storage path")?;
            commands.push(format!(
                "mkdir -p {} > /dev/null 2>&1 || true",
                quote(&path)
            ));
            commands.push(format!(
                "mkdir -p {} > /dev/null 2>&1 || true",
                quote(workdir)
            ));
        }
        if path.starts_with('/') || path.starts_with('~') {
            let parent = path.rfind('/').map_or(path.as_str(), |i| &path[..i]);
            if !parent.is_empty() {
                commands.push(format!(
                    "mkdir -p {} > /dev/null 2>&1 || true",
                    quote(parent)
                ));
            }
        }
        validate_shell_safe_path(&path, "storage path")?;
        let escaped = quote(&path);
        let is_file = self
            .storage_run(
                &target,
                &[format!("test -f {escaped} && echo OK || echo NOK")],
            )
            .await?;
        let is_dir = self
            .storage_run(
                &target,
                &[format!("test -d {escaped} && echo OK || echo NOK")],
            )
            .await?;
        let mut replaces_empty_directory = false;
        if is_file == "OK" && file.is_directory {
            let content = self.read_for_mismatch(&target, &escaped).await;
            self.save_file_state(file, Some(&content), false).await?;
            return Err("The following file is a file on the server, but you are trying to mark it as a directory. Please delete the file on the server or mark it as directory.".into());
        } else if is_dir == "OK" && !file.is_directory {
            if matches!(path.as_str(), "/" | "." | ".." | "") {
                self.save_file_state(file, None, true).await?;
                return Err("The following file is a directory on the server, but you are trying to mark it as a file. <br><br>Please delete the directory on the server or mark it as directory.".into());
            }
            if outside || self.remote_file_state(&target, &path).await != "empty-directory" {
                return Err(format!(
                    "The following file is a directory on the server, but you are trying to mark it as a file: {path}<br><br>Please delete the directory on the server or mark it as directory."
                ));
            }
            replaces_empty_directory = true;
        }
        if (is_dir == "NOK" || replaces_empty_directory) && !file.is_directory {
            if replaces_empty_directory {
                commands.push(format!("rmdir {escaped}"));
            }
            match file
                .content
                .as_deref()
                .filter(|c| !c.is_empty() && *c != "0")
            {
                Some(content) => {
                    use base64::Engine as _;
                    let encoded = base64::engine::general_purpose::STANDARD.encode(content);
                    commands.push(format!(
                        "echo '{encoded}' | base64 -d | tee {escaped} > /dev/null"
                    ));
                }
                None => commands.push(format!("touch {escaped}")),
            }
            commands.push(format!("chmod +x {escaped}"));
            if let Some(chown) = file.chown.as_deref().filter(|c| !c.is_empty() && *c != "0") {
                commands.push(format!("chown -- {} {escaped}", quote(chown)));
            }
            if let Some(chmod) = file.chmod.as_deref().filter(|c| !c.is_empty() && *c != "0") {
                commands.push(format!("chmod -- {} {escaped}", quote(chmod)));
            }
        } else if is_dir == "NOK" && file.is_directory {
            commands.push(format!("mkdir -p {escaped} > /dev/null 2>&1 || true"));
        }
        self.storage_run(&target, &commands).await.map(|_| ())
    }

    /// `saveStorageOnServer()`: every server, also when one fails; the first error is returned.
    pub async fn save_storage_on_server(
        &self,
        owner: &StorageOwner,
        file: &FileStorage,
    ) -> Result<(), String> {
        if file.is_host_file {
            return Ok(());
        }
        let mut first_error = None;
        for id in &owner.server_ids {
            if let Err(e) = self.save_storage_on(owner, file, *id).await {
                first_error.get_or_insert(e);
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    /// `ServerStorageSaveJob::dispatch($fileVolume)`.
    pub fn spawn_storage_save(&self, owner: StorageOwner, file: FileStorage) {
        let jobs = self.clone();
        tokio::spawn(async move {
            if let Err(e) = jobs.save_storage_on_server(&owner, &file).await {
                tracing::warn!(storage = file.id, error = %e, "ServerStorageSaveJob failed");
            }
        });
    }

    /// `deleteStorageOnServer($server)`.
    async fn delete_storage_on(
        &self,
        owner: &StorageOwner,
        file: &FileStorage,
        server_id: i64,
    ) -> Result<(), String> {
        let workdir = owner.workdir.as_str();
        let mut path = resolved_fs_path(&file.fs_path, workdir);
        if is_outside_resource_directory(&file.fs_path, &owner.base_directories) {
            return Ok(());
        }
        let (_, target) = self.storage_server(server_id).await?;
        if !is_admin_controlled_compose_mount(owner, file) {
            path = confine_path_to_base(workdir, &path, "storage path")?;
            self.assert_remote_path_is_confined(&target, workdir, &path)
                .await?;
        }
        validate_shell_safe_path(&path, "storage path")?;
        let escaped = quote(&path);
        let is_file = self
            .storage_run(
                &target,
                &[format!("test -f {escaped} && echo OK || echo NOK")],
            )
            .await?;
        let is_dir = self
            .storage_run(
                &target,
                &[format!("test -d {escaped} && echo OK || echo NOK")],
            )
            .await?;
        let mut commands = Vec::new();
        if !path.is_empty() && !matches!(path.as_str(), "/" | "." | "..") {
            if is_file == "OK" {
                commands.push(format!("rm -rf {escaped} > /dev/null 2>&1 || true"));
            } else if is_dir == "OK" {
                commands.push(format!("rm -rf {escaped} > /dev/null 2>&1 || true"));
                commands.push(format!("rmdir {escaped} > /dev/null 2>&1 || true"));
            }
        }
        if !commands.is_empty() {
            self.storage_run(&target, &commands).await?;
        }
        Ok(())
    }

    /// `deleteStorageOnServer()`: every server, also when one fails; the first error is returned.
    pub async fn delete_storage_on_server(
        &self,
        owner: &StorageOwner,
        file: &FileStorage,
    ) -> Result<(), String> {
        if file.is_host_file {
            return Ok(());
        }
        let mut first_error = None;
        for id in &owner.server_ids {
            if let Err(e) = self.delete_storage_on(owner, file, *id).await {
                first_error.get_or_insert(e);
            }
        }
        first_error.map_or(Ok(()), Err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mount_paths() {
        assert_eq!(
            validate_file_mount_path("etc//nginx.conf", "file storage path").unwrap(),
            "/etc/nginx.conf"
        );
        assert!(
            validate_file_mount_path("/a/../b", "c")
                .unwrap_err()
                .contains("relative path segments")
        );
        assert_eq!(
            confine_file_mount_path("/data/coolify/applications/u", "/a/b", "x").unwrap(),
            "/data/coolify/applications/u/a/b"
        );
    }

    #[test]
    fn host_paths() {
        let dirs = ["/data/coolify/applications/u".to_owned()];
        assert_eq!(
            resolve_host_path(&dirs, "sub/x", "c").unwrap(),
            "/data/coolify/applications/u/sub/x"
        );
        assert_eq!(resolve_host_path(&dirs, "/opt/x", "c").unwrap(), "/opt/x");
        assert!(resolve_host_path(&dirs, "../x", "c").is_err());
        assert!(resolve_host_path(&dirs, "/", "c").is_err());
        assert!(is_outside_resource_directory("/opt/x", &dirs));
        assert!(!is_outside_resource_directory("./x", &dirs));
    }

    #[test]
    fn host_file_paths() {
        assert_eq!(
            validate_host_file_mount_path("/etc//hosts", "c").unwrap(),
            "/etc/hosts"
        );
        assert!(validate_host_file_mount_path("etc/hosts", "c").is_err());
        assert!(validate_host_file_mount_path("/etc/", "c").is_err());
    }
}
