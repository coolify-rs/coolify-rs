//! `App\Services\RcloneTransfer` (Coolify v4.7.0): copies backup files between a server and an
//! SFTP, FTPS, or WebDAV storage with rclone, in a container on the server. S3 storages use the
//! MinIO client in the helper image instead.

use std::time::Duration;

use coolify_db::database_parts as parts;
use coolify_db::records::Record;
use coolify_remote::shell::quote;
use coolify_remote::{RemoteError, RunOptions, Target};

use super::storage_type::{self as st, BackupStorageType, RCLONE_REMOTE, RcloneSource};
use crate::Jobs;

/// `RcloneTransfer::CONFIG_PATH`.
const CONFIG_PATH: &str = "/tmp/rclone.conf";

/// `RcloneTransfer::LABEL`: Docker label of the container. `CleanupHelperContainersJob` removes
/// labeled containers of jobs that ended.
pub const LABEL: &str = "coolify.type=rclone";

/// `RcloneTransfer::DEFAULT_LIFETIME_SECONDS`: lifetime of a container started without a timeout.
const DEFAULT_LIFETIME_SECONDS: u64 = 86400;
/// `RcloneTransfer::LIFETIME_MARGIN_SECONDS`: the time after the timeout that a container stays.
const LIFETIME_MARGIN_SECONDS: u64 = 300;

/// `config('constants.coolify.rclone_image')`: https://hub.docker.com/r/rclone/rclone/tags (also on
/// ghcr.io with the same digest).
pub const RCLONE_IMAGE: &str = "docker.io/rclone/rclone:1.75.2@sha256:2687085f718d3c628f7fdfb77c52a1d344332aed543110bb88088f2c70d43eb5";

/// One rclone container for one storage.
#[derive(Clone)]
pub struct RcloneTransfer {
    kind: BackupStorageType,
    endpoint: String,
    /// `$storage->rcloneConfig()`.
    config: String,
    container: String,
}

impl std::fmt::Debug for RcloneTransfer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RcloneTransfer")
            .field("container", &self.container)
            .finish_non_exhaustive()
    }
}

/// PHP `addcslashes($name, '\\*?[]{}')`.
fn escape_glob(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        if "\\*?[]{}".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

impl RcloneTransfer {
    pub fn new(kind: BackupStorageType, endpoint: &str, config: String, container: &str) -> Self {
        Self {
            kind,
            endpoint: endpoint.to_owned(),
            config,
            container: container.to_owned(),
        }
    }

    /// `RcloneTransfer::image()`.
    pub fn image() -> &'static str {
        RCLONE_IMAGE
    }

    /// The commands of `start()`: remove a leftover container in the same batch, so a replayed
    /// batch does not fail with a name conflict, then start the container. `volumes` are
    /// `docker run -v` values, for example "/host/path:/path:ro". With a `timeout` of the job, the
    /// container stops itself shortly after it, also when the job dies before it removes the
    /// container with the config.
    pub fn start_commands(
        &self,
        volumes: &[String],
        network: Option<&str>,
        timeout: Option<u64>,
    ) -> [String; 2] {
        let mut options: String = volumes
            .iter()
            .map(|volume| format!(" -v {}", quote(volume)))
            .collect();
        if let Some(network) = network {
            options.push_str(&format!(" --network {}", quote(network)));
        }
        let lifetime = timeout.map_or(DEFAULT_LIFETIME_SECONDS, |t| t + LIFETIME_MARGIN_SECONDS);
        [
            self.remove_command(),
            format!(
                "docker run -d --rm --name {} --label {LABEL}{options} --entrypoint sleep {} {lifetime} >/dev/null",
                quote(&self.container),
                quote(Self::image())
            ),
        ]
    }

    /// `writeConfig()`: the config goes to the container without being an argument of a process on
    /// Coolify or on the server.
    pub fn write_config_command(&self) -> String {
        st::write_in_container_command(&self.container, CONFIG_PATH, &self.config)
    }

    fn remote_path(&self, remote_file: &str) -> Result<String, String> {
        st::rclone_remote_path(self.kind, &self.endpoint, remote_file)
    }

    /// `uploadCommand()`.
    pub fn upload_command(&self, local_path: &str, remote_file: &str) -> Result<String, String> {
        Ok(self.rclone_command(&["copyto", local_path, &self.remote_path(remote_file)?]))
    }

    /// `downloadCommand()`.
    pub fn download_command(&self, remote_file: &str, local_path: &str) -> Result<String, String> {
        Ok(self.rclone_command(&["copyto", &self.remote_path(remote_file)?, local_path]))
    }

    /// `removeCommand()`.
    pub fn remove_command(&self) -> String {
        format!(
            "docker rm -f {} >/dev/null 2>&1 || true",
            quote(&self.container)
        )
    }

    /// `rcloneCommand()`.
    fn rclone_command(&self, arguments: &[&str]) -> String {
        let mut command = format!(
            "docker exec {} rclone --config {CONFIG_PATH} --contimeout 15s",
            quote(&self.container)
        );
        for argument in arguments {
            command.push(' ');
            command.push_str(&quote(argument));
        }
        command
    }

    /// The cleanup of `upload()` after a failed upload. rclone writes
    /// "<name>.<8 hex digits>.partial" and renames it at the end, so this deletes the partial files
    /// of this file, and no other file. An upload that timed out can still run in the container;
    /// it is stopped before its partial file is deleted.
    pub fn partial_cleanup_commands(&self, remote_file: &str) -> Result<[String; 2], String> {
        let remote_path = self.remote_path(remote_file)?;
        let (directory, name) = match remote_path.rsplit_once('/') {
            Some((directory, name)) => (directory.to_owned(), name.to_owned()),
            None => (
                format!("{RCLONE_REMOTE}:"),
                remote_path[RCLONE_REMOTE.len() + 1..].to_owned(),
            ),
        };
        Ok([
            format!(
                "docker exec {} pkill -x rclone >/dev/null 2>&1 || true",
                quote(&self.container)
            ),
            self.rclone_command(&[
                "delete",
                &directory,
                "--max-depth",
                "1",
                "--include",
                &format!("/{}.{}.partial", escape_glob(&name), "[0-9a-f]".repeat(8)),
            ]),
        ])
    }
}

impl Jobs {
    /// `new RcloneTransfer($storage, $server, $container)` for the storage of a row.
    pub(crate) async fn rclone_transfer(
        &self,
        row: &Record,
        container: &str,
    ) -> Result<RcloneTransfer, String> {
        let enc = self
            .0
            .encrypter
            .as_ref()
            .ok_or("No application encryption key has been specified.")?;
        let plain = |k: &str| {
            enc.decrypt_string(parts::str_of(row, k).unwrap_or_default())
                .map_err(|_| "The MAC is invalid.".to_owned())
        };
        let kind = BackupStorageType::of(row);
        let endpoint = parts::str_of(row, "endpoint").unwrap_or_default();
        let user = plain("key")?;
        let password = plain("secret")?;
        let private_key = self.sftp_private_key(row).await?;
        let config = st::rclone_config(
            &RcloneSource {
                kind,
                endpoint,
                user: &user,
                password: &password,
                private_key: private_key.as_deref(),
                sftp_host_key: parts::str_of(row, "sftp_host_key"),
            },
            st::random_iv(),
        )?;
        Ok(RcloneTransfer::new(kind, endpoint, config, container))
    }

    /// `RcloneTransfer::start()`: starts the rclone container and writes the storage config to it.
    pub(crate) async fn rclone_start(
        &self,
        target: &Target,
        transfer: &RcloneTransfer,
        volumes: &[String],
        network: Option<&str>,
        timeout: Option<Duration>,
    ) -> Result<(), RemoteError> {
        let commands = transfer.start_commands(volumes, network, timeout.map(|t| t.as_secs()));
        let refs: Vec<&str> = commands.iter().map(String::as_str).collect();
        let mut opts = RunOptions::op("storage.rclone");
        if let Some(timeout) = timeout {
            opts = opts.timeout(timeout);
        }
        self.remote().run(target, &refs, opts).await?;
        let write = transfer.write_config_command();
        self.remote()
            .run(target, &[write.as_str()], RunOptions::op("storage.rclone"))
            .await
            .map(|_| ())
    }

    /// `RcloneTransfer::upload()`: uploads a local file, and deletes its partial files after a
    /// failed upload.
    pub(crate) async fn rclone_upload(
        &self,
        target: &Target,
        transfer: &RcloneTransfer,
        local_path: &str,
        remote_file: &str,
        timeout: Option<Duration>,
    ) -> Result<(), RemoteError> {
        let invalid = |message: String| RemoteError {
            message,
            code: None,
        };
        let upload = transfer
            .upload_command(local_path, remote_file)
            .map_err(invalid)?;
        let mut opts = RunOptions::op("storage.rclone");
        if let Some(timeout) = timeout {
            opts = opts.timeout(timeout);
        }
        match self.remote().run(target, &[upload.as_str()], opts).await {
            Ok(_) => Ok(()),
            Err(e) => {
                if let Ok(cleanup) = transfer.partial_cleanup_commands(remote_file) {
                    let refs: Vec<&str> = cleanup.iter().map(String::as_str).collect();
                    self.remote()
                        .run_quiet(target, &refs, RunOptions::op("storage.rclone"))
                        .await;
                }
                Err(e)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn transfer() -> RcloneTransfer {
        RcloneTransfer::new(
            BackupStorageType::Sftp,
            "sftp://backup.example.com/srv/backups",
            "[storage]\ntype = sftp\n".to_owned(),
            "backup-of-abc",
        )
    }

    #[test]
    fn the_container_sleeps_until_shortly_after_the_timeout() {
        let t = transfer();
        let commands = t.start_commands(
            &["/data/a.dmp:/data/a.dmp:ro".to_owned()],
            Some("coolify"),
            Some(3600),
        );
        assert_eq!(
            commands[0],
            "docker rm -f 'backup-of-abc' >/dev/null 2>&1 || true"
        );
        assert_eq!(
            commands[1],
            format!(
                "docker run -d --rm --name 'backup-of-abc' --label coolify.type=rclone -v '/data/a.dmp:/data/a.dmp:ro' --network 'coolify' --entrypoint sleep '{RCLONE_IMAGE}' 3900 >/dev/null"
            )
        );
        let commands = t.start_commands(&[], None, None);
        assert_eq!(
            commands[1],
            format!(
                "docker run -d --rm --name 'backup-of-abc' --label coolify.type=rclone --entrypoint sleep '{RCLONE_IMAGE}' 86400 >/dev/null"
            )
        );
    }

    #[test]
    fn files_keep_their_local_path_in_the_storage() {
        let t = transfer();
        assert_eq!(
            t.upload_command("/data/coolify/a.dmp", "/data/coolify/a.dmp")
                .unwrap(),
            "docker exec 'backup-of-abc' rclone --config /tmp/rclone.conf --contimeout 15s 'copyto' '/data/coolify/a.dmp' 'storage:/srv/backups/data/coolify/a.dmp'"
        );
        assert_eq!(
            t.download_command("data/coolify/a.dmp", "/tmp/restore")
                .unwrap(),
            "docker exec 'backup-of-abc' rclone --config /tmp/rclone.conf --contimeout 15s 'copyto' 'storage:/srv/backups/data/coolify/a.dmp' '/tmp/restore'"
        );
    }

    #[test]
    fn a_failed_upload_deletes_only_its_partial_files() {
        let t = transfer();
        let commands = t
            .partial_cleanup_commands("/data/coolify/pg-dump-[1]*.dmp")
            .unwrap();
        assert_eq!(
            commands[0],
            "docker exec 'backup-of-abc' pkill -x rclone >/dev/null 2>&1 || true"
        );
        assert_eq!(
            commands[1],
            "docker exec 'backup-of-abc' rclone --config /tmp/rclone.conf --contimeout 15s 'delete' 'storage:/srv/backups/data/coolify' '--max-depth' '1' '--include' '/pg-dump-\\[1\\]\\*.dmp.[0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f].partial'"
        );
        // A file in the login directory has no directory in its remote path.
        let webdav = RcloneTransfer::new(
            BackupStorageType::Webdav,
            "https://cloud.example.com/dav",
            String::new(),
            "c",
        );
        let commands = webdav.partial_cleanup_commands("a.tar.gz").unwrap();
        assert!(
            commands[1].contains("'delete' 'storage:' '--max-depth' '1' '--include' '/a.tar.gz.")
        );
    }

    #[test]
    fn the_config_is_not_a_command_argument() {
        let command = transfer().write_config_command();
        assert_eq!(
            command,
            "echo 'W3N0b3JhZ2VdCnR5cGUgPSBzZnRwCg==' | base64 -d | docker exec -i 'backup-of-abc' sh -c 'umask 077 && cat > /tmp/rclone.conf'"
        );
    }
}
