//! Small server operations of the database pages (`Project\Database\*`).

use coolify_remote::exec::RemoteRead;
use coolify_remote::{RunOptions, shell::quote};

use super::common::CONFIGURATION_DIR;
use crate::Jobs;
use crate::server::ServerInfo;

/// `database_configuration_dir()`.
pub fn configuration_dir() -> &'static str {
    CONFIGURATION_DIR
}

/// A file being read from a server.
pub type ServerFileStream = RemoteRead;

impl Jobs {
    /// `docker logs` of a container (`getContainerLogs`, the proxy log dialog).
    pub async fn container_logs(
        &self,
        server: &ServerInfo,
        container: &str,
        lines: i64,
        timestamps: bool,
    ) -> Result<String, String> {
        let t = server.target(self).await?;
        let flags = if timestamps { " --timestamps" } else { "" };
        let command = format!("docker logs -n {lines}{flags} {} 2>&1", quote(container));
        Ok(self
            .remote()
            .run_quiet(&t, &[command.as_str()], RunOptions::op("database.logs"))
            .await
            .unwrap_or_default())
    }

    /// `instant_remote_process(["rm -f {$escapedPath}"], $server)`.
    pub async fn remove_server_file(&self, server: &ServerInfo, path: &str) -> Result<(), String> {
        let t = server.target(self).await?;
        let command = format!("rm -f {}", quote(path));
        self.remote()
            .run(
                &t,
                &[command.as_str()],
                RunOptions::op("database.init_script"),
            )
            .await
            .map(|_| ())
            .map_err(|e| e.message)
    }

    /// `StandaloneClickhouse::anonymousDataVolume`: `(type, name)` of what is mounted at the data directory.
    pub async fn clickhouse_data_mount(
        &self,
        server: &ServerInfo,
        uuid: &str,
    ) -> Option<(String, String)> {
        let t = server.target(self).await.ok()?;
        let format = r#"{{range .Mounts}}{{if eq .Destination "/var/lib/clickhouse"}}{{.Type}} {{.Name}}{{end}}{{end}}"#;
        let command = format!(
            "docker inspect --format {} {} 2>/dev/null || true",
            quote(format),
            quote(uuid)
        );
        let out = self
            .remote()
            .run_quiet(&t, &[command.as_str()], RunOptions::op("database.status"))
            .await?;
        let out = out.trim();
        let (kind, name) = out.split_once(' ').unwrap_or((out, ""));
        Some((kind.to_owned(), name.to_owned()))
    }

    /// `ImportForm::serverFileExists`: `ls -l` answers.
    pub async fn server_file_exists(
        &self,
        server: &ServerInfo,
        path: &str,
    ) -> Result<bool, String> {
        let t = server.target(self).await?;
        let command = format!("ls -l {}", quote(path));
        let out = self
            .remote()
            .run_quiet(&t, &[command.as_str()], RunOptions::op("database.import"))
            .await;
        Ok(out.is_some_and(|o| !o.trim().is_empty()))
    }

    /// `streamBackupFromServer`: `None` when the file is not there (`FileNotFoundException`).
    pub async fn read_server_file(
        &self,
        server: &ServerInfo,
        path: &str,
    ) -> Result<Option<RemoteRead>, String> {
        if !self.server_file_exists(server, path).await? {
            return Ok(None);
        }
        let t = server.target(self).await?;
        self.remote()
            .open_read(&t, path)
            .await
            .map(Some)
            .map_err(|e| e.message)
    }
}
