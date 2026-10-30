//! Server work of `LocalFileVolume` for databases: `saveStorageOnServer`,
//! `deleteStorageOnServer`, the remote path checks and `EnsureContentFilesOnServer`.

use coolify_db::database_parts as parts;
use coolify_db::databases::DatabaseRow;
use coolify_db::records::Record;
use coolify_remote::{RunOptions, shell::quote};

use super::common::CONFIGURATION_DIR;
use super::paths::{
    REMOTE_FILE_STATE_SCRIPT, REMOTE_PATH_CONFINEMENT_SCRIPT, inside,
    is_outside_resource_directory, normalize_unix_path, resolve_host_path, resolved_fs_path,
};
use crate::Jobs;
use crate::compose_parser::validate::validate_shell_safe_path;
use crate::server::ServerInfo;

const BINARY_PLACEHOLDER: &str = "[binary file]";
const TOO_LARGE_PLACEHOLDER: &str = "[file too large to display]";

fn workdir(d: &DatabaseRow) -> String {
    format!("{CONFIGURATION_DIR}/{}", d.uuid)
}

impl Jobs {
    fn content_of(&self, row: &Record) -> Option<String> {
        let enc = self.0.encrypter.as_ref()?;
        parts::str_of(row, "content").and_then(|c| enc.decrypt_string(c).ok())
    }

    /// `assertRemotePathIsConfined`.
    pub async fn assert_remote_path_confined(
        &self,
        base: &str,
        path: &str,
        server: &ServerInfo,
    ) -> Result<(), String> {
        let t = server.target(self).await?;
        let command = format!(
            "sh -c {} sh {} {}",
            quote(REMOTE_PATH_CONFINEMENT_SCRIPT),
            quote(base),
            quote(path)
        );
        let out = self
            .remote()
            .run_quiet(&t, &[command.as_str()], RunOptions::op("storage.check"))
            .await
            .unwrap_or_default();
        if out.trim() == "OK" {
            Ok(())
        } else {
            Err("Invalid storage path: resolved path must stay inside the resource configuration directory.".to_owned())
        }
    }

    /// `assertRemotePathIsNotSymlink`.
    pub async fn assert_remote_path_not_symlink(
        &self,
        path: &str,
        server: &ServerInfo,
    ) -> Result<(), String> {
        let t = server.target(self).await?;
        let command = format!("test -L {} && echo LINK || echo OK", quote(path));
        let out = self
            .remote()
            .run(&t, &[command.as_str()], RunOptions::op("storage.check"))
            .await
            .map_err(|e| e.message)?
            .unwrap_or_default();
        if out.trim() == "OK" {
            Ok(())
        } else {
            Err(format!(
                "{path} is a symbolic link on the server. Coolify does not write a file through a symbolic link. Remove the link or use another path."
            ))
        }
    }

    /// `LocalFileVolume::assertHostPathOnServer`.
    pub async fn assert_host_path_on_server(
        &self,
        resource_directory: &str,
        path: &str,
        server: &ServerInfo,
        is_directory: bool,
    ) -> Result<(), String> {
        let directory = normalize_unix_path(resource_directory)?;
        if inside(path, &directory) {
            self.assert_remote_path_confined(resource_directory, path, server)
                .await
        } else if !is_directory {
            self.assert_remote_path_not_symlink(path, server).await
        } else {
            Ok(())
        }
    }

    async fn remote_test(
        &self,
        t: &coolify_remote::Target,
        flag: &str,
        escaped: &str,
    ) -> Result<String, String> {
        let command = format!("test {flag} {escaped} && echo OK || echo NOK");
        Ok(self
            .remote()
            .run(t, &[command.as_str()], RunOptions::op("storage.check"))
            .await
            .map_err(|e| e.message)?
            .unwrap_or_default())
    }

    /// `LocalFileVolume::saveStorageOnServer` for a database's file storage.
    pub async fn save_file_storage(
        &self,
        row: &Record,
        d: &DatabaseRow,
        server: &ServerInfo,
    ) -> Result<(), String> {
        if parts::bool_of(row, "is_host_file") {
            return Ok(());
        }
        let dir = workdir(d);
        let fs_path = parts::str_of(row, "fs_path").unwrap_or_default();
        let is_directory = parts::bool_of(row, "is_directory");
        let host = resolve_host_path(&dir, &resolved_fs_path(fs_path, &dir), "storage path")?;
        let normalized = normalize_unix_path(&dir)?;
        if inside(&host, &normalized) {
            self.assert_remote_path_confined(&normalized, &host, server)
                .await?;
        } else if !is_directory {
            self.assert_remote_path_not_symlink(&host, server).await?;
        }
        let outside = is_outside_resource_directory(fs_path, &dir);
        validate_shell_safe_path(&host, "storage path")?;
        let escaped = quote(&host);
        let mut commands: Vec<String> = Vec::new();
        if is_directory {
            commands.push(format!("mkdir -p {escaped} > /dev/null 2>&1 || true"));
            commands.push(format!("mkdir -p {} > /dev/null 2>&1 || true", quote(&dir)));
        }
        if let Some((parent, _)) = host.rsplit_once('/')
            && !parent.is_empty()
        {
            commands.push(format!(
                "mkdir -p {} > /dev/null 2>&1 || true",
                quote(parent)
            ));
        }
        let t = server.target(self).await?;
        let is_file = self.remote_test(&t, "-f", &escaped).await?;
        let is_dir = self.remote_test(&t, "-d", &escaped).await?;
        let mut replaces_empty = false;
        if is_file == "OK" && is_directory {
            return Err("The following file is a file on the server, but you are trying to mark it as a directory. Please delete the file on the server or mark it as directory.".to_owned());
        } else if is_dir == "OK" && !is_directory {
            if outside
                || self
                    .remote_file_states(std::slice::from_ref(&host), server)
                    .await
                    .first()
                    .map(String::as_str)
                    != Some("empty-directory")
            {
                return Err(format!(
                    "The following file is a directory on the server, but you are trying to mark it as a file: {host}<br><br>Please delete the directory on the server or mark it as directory."
                ));
            }
            replaces_empty = true;
        }
        if (is_dir == "NOK" || replaces_empty) && !is_directory {
            if replaces_empty {
                commands.push(format!("rmdir {escaped}"));
            }
            match self.content_of(row).filter(|c| !c.is_empty() && c != "0") {
                Some(content) => commands.push(format!(
                    "echo '{}' | base64 -d | tee {escaped} > /dev/null",
                    super::common::b64(&content)
                )),
                None => commands.push(format!("touch {escaped}")),
            }
            commands.push(format!("chmod +x {escaped}"));
            if let Some(chown) = parts::str_of(row, "chown").filter(|c| !c.is_empty()) {
                commands.push(format!("chown -- {} {escaped}", quote(chown)));
            }
            if let Some(chmod) = parts::str_of(row, "chmod").filter(|c| !c.is_empty()) {
                commands.push(format!("chmod -- {} {escaped}", quote(chmod)));
            }
        } else if is_dir == "NOK" && is_directory {
            commands.push(format!("mkdir -p {escaped} > /dev/null 2>&1 || true"));
        }
        let refs: Vec<&str> = commands.iter().map(String::as_str).collect();
        self.remote()
            .run(&t, &refs, RunOptions::op("storage.save"))
            .await
            .map_err(|e| e.message)?;
        Ok(())
    }

    /// `ServerStorageSaveJob::dispatch`.
    pub fn spawn_file_storage_save(&self, row: Record, d: DatabaseRow) {
        let jobs = self.clone();
        tokio::spawn(async move {
            match jobs.server_for(&d).await {
                Ok(server) => {
                    if let Err(e) = jobs.save_file_storage(&row, &d, &server).await {
                        tracing::warn!(database = %d.uuid, error = %e, "file storage could not be written");
                    }
                }
                Err(e) => {
                    tracing::warn!(database = %d.uuid, error = %e, "file storage could not be written")
                }
            }
        });
    }

    /// `LocalFileVolume::deleteStorageOnServer`: paths outside the resource directory are kept.
    pub async fn delete_file_storage(&self, row: &Record, d: &DatabaseRow) -> Result<(), String> {
        if parts::bool_of(row, "is_host_file") {
            return Ok(());
        }
        let server = self.server_for(d).await?;
        let dir = workdir(d);
        let fs_path = parts::str_of(row, "fs_path").unwrap_or_default();
        if is_outside_resource_directory(fs_path, &dir) {
            return Ok(());
        }
        let resolved = normalize_unix_path(&resolved_fs_path(fs_path, &dir))?;
        self.assert_remote_path_confined(&dir, &resolved, &server)
            .await?;
        validate_shell_safe_path(&resolved, "storage path")?;
        let escaped = quote(&resolved);
        let t = server.target(self).await?;
        let is_file = self.remote_test(&t, "-f", &escaped).await?;
        let is_dir = self.remote_test(&t, "-d", &escaped).await?;
        let mut commands: Vec<String> = Vec::new();
        if resolved != "/" && resolved != "." && resolved != ".." {
            if is_file == "OK" {
                commands.push(format!("rm -rf {escaped} > /dev/null 2>&1 || true"));
            } else if is_dir == "OK" {
                commands.push(format!("rm -rf {escaped} > /dev/null 2>&1 || true"));
                commands.push(format!("rmdir {escaped} > /dev/null 2>&1 || true"));
            }
        }
        if !commands.is_empty() {
            let refs: Vec<&str> = commands.iter().map(String::as_str).collect();
            self.remote()
                .run(&t, &refs, RunOptions::op("storage.delete"))
                .await
                .map_err(|e| e.message)?;
        }
        Ok(())
    }

    /// `LocalFileVolume::remoteFileStates`.
    async fn remote_file_states(&self, paths: &[String], server: &ServerInfo) -> Vec<String> {
        let Ok(t) = server.target(self).await else {
            return vec!["unknown".to_owned(); paths.len()];
        };
        let args: Vec<String> = paths.iter().map(|p| quote(p)).collect();
        let command = format!(
            "sh -c {} sh {}",
            quote(REMOTE_FILE_STATE_SCRIPT),
            args.join(" ")
        );
        let out = self
            .remote()
            .run_quiet(&t, &[command.as_str()], RunOptions::op("storage.check"))
            .await
            .unwrap_or_default();
        let mut states = vec!["unknown".to_owned(); paths.len()];
        for line in out.trim().lines() {
            if let Some((n, state)) = line.trim().split_once(':')
                && let Ok(n) = n.parse::<usize>()
                && n >= 1
                && n <= states.len()
            {
                states[n - 1] = state.to_owned();
            }
        }
        states
    }

    /// `EnsureContentFilesOnServer::echoCommands`: writes missing content files and returns
    /// its log lines as `echo` commands.
    pub(super) async fn ensure_content_files(
        &self,
        rows: &[Record],
        d: &DatabaseRow,
        server: &ServerInfo,
    ) -> Vec<String> {
        let dir = workdir(d);
        let mut commands = Vec::new();
        let mut candidates: Vec<(&Record, String)> = Vec::new();
        for row in rows {
            let content = self.content_of(row).unwrap_or_default();
            // hasContentToWrite
            if parts::bool_of(row, "is_directory")
                || parts::bool_of(row, "is_host_file")
                || parts::bool_of(row, "is_based_on_git")
                || content == BINARY_PLACEHOLDER
                || content == TOO_LARGE_PLACEHOLDER
                || content.is_empty()
            {
                continue;
            }
            let fs_path = parts::str_of(row, "fs_path").unwrap_or_default();
            match resolve_host_path(&dir, &resolved_fs_path(fs_path, &dir), "storage path") {
                Ok(p) => candidates.push((row, p)),
                Err(e) => commands.push(format!("echo {}", quote(&format!("Warning: {e}")))),
            }
        }
        if candidates.is_empty() {
            return commands;
        }
        let paths: Vec<String> = candidates.iter().map(|(_, p)| p.clone()).collect();
        let states = self.remote_file_states(&paths, server).await;
        let mut to_write = Vec::new();
        for (i, (row, path)) in candidates.iter().enumerate() {
            let mount = parts::str_of(row, "mount_path").unwrap_or_default();
            match states.get(i).map(String::as_str).unwrap_or("unknown") {
                "missing" | "empty-directory" => to_write.push((*row, path.clone())),
                "file" => {}
                "directory" => commands.push(format!("echo {}", quote(&format!(
                    "Warning: A directory that is not empty is at {path} on the server. Coolify did not write the configuration file for {mount}. Remove the directory on the server, then start the resource again."
                )))),
                "other" => commands.push(format!("echo {}", quote(&format!(
                    "Warning: A symbolic link or a special file is at {path} on the server. Coolify did not write the configuration file for {mount}."
                )))),
                _ => commands.push(format!("echo {}", quote(&format!(
                    "Warning: Coolify cannot check the configuration file {path} on the server."
                )))),
            }
        }
        if to_write.is_empty() {
            return commands;
        }
        let count = to_write.len();
        let line = if count == 1 {
            "Writing 1 missing configuration file.".to_owned()
        } else {
            format!("Writing {count} missing configuration files.")
        };
        commands.push(format!("echo {}", quote(&line)));
        for (row, path) in to_write {
            if let Err(e) = self.save_file_storage(row, d, server).await {
                let plain = e.replace("<br>", " ");
                let plain = plain.split_whitespace().collect::<Vec<_>>().join(" ");
                commands.push(format!(
                    "echo {}",
                    quote(&format!(
                        "Warning: Coolify cannot write the configuration file {path}: {plain}"
                    ))
                ));
            }
        }
        commands
    }
}
