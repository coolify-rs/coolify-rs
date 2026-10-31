//! `StartDatabaseProxy` and `StopDatabaseProxy`: the public TCP port of a database.

use coolify_compat::yaml::{self, PhpValue};
use coolify_db::databases::{self as dbs, DatabaseRow, Kind};
use coolify_db::servers::ColumnValue;
use coolify_remote::{RunOptions, shell::quote};

use super::common::{CONFIGURATION_DIR, b64, now};
use crate::Jobs;
use crate::server::ServerInfo;

/// The errors after which a retry cannot help (`isNonTransientError`).
const NON_TRANSIENT: &[&str] = &[
    "port is already allocated",
    "address already in use",
    "Bind for",
];

/// `database_proxy_dir($uuid)`.
fn proxy_dir(uuid: &str) -> String {
    format!("{CONFIGURATION_DIR}/{uuid}/proxy")
}

/// The nginx configuration of the proxy.
pub fn nginx_conf(
    container: &str,
    internal_port: i64,
    public_port: i64,
    timeout: Option<i64>,
    ipv6: bool,
) -> String {
    let listen = if ipv6 {
        format!("listen {public_port};\n        listen [::]:{public_port};")
    } else {
        format!("listen {public_port};")
    };
    let timeout = timeout.filter(|t| *t >= 1).unwrap_or(3600);
    format!(
        "user  nginx;\nworker_processes  auto;\n\nerror_log  /var/log/nginx/error.log;\n\nevents {{\n    worker_connections  1024;\n}}\nstream {{\n   resolver 127.0.0.11 valid=10s;\n   server {{\n        {listen}\n        set $upstream {container}:{internal_port};\n        proxy_pass $upstream;\n        proxy_timeout {timeout}s;\n   }}\n}}"
    )
}

/// The Docker Compose file of the proxy.
pub fn proxy_compose(uuid: &str, public_port: i64, network: &str) -> String {
    let name = format!("{uuid}-proxy");
    let dir = proxy_dir(uuid);
    let service = PhpValue::map([
        ("image", PhpValue::from("nginx:stable-alpine")),
        ("container_name", PhpValue::from(name.as_str())),
        ("restart", PhpValue::from("unless-stopped")),
        (
            "ports",
            PhpValue::strings([format!("{public_port}:{public_port}")]),
        ),
        ("networks", PhpValue::strings([network])),
        (
            "volumes",
            PhpValue::list([PhpValue::map([
                ("type", PhpValue::from("bind")),
                ("source", PhpValue::from(format!("{dir}/nginx.conf"))),
                ("target", PhpValue::from("/etc/nginx/nginx.conf")),
            ])]),
        ),
        (
            "healthcheck",
            PhpValue::map([
                (
                    "test",
                    PhpValue::strings(["CMD-SHELL", "stat /etc/nginx/nginx.conf || exit 1"]),
                ),
                ("interval", PhpValue::from("5s")),
                ("timeout", PhpValue::from("5s")),
                ("retries", PhpValue::Int(3)),
                ("start_period", PhpValue::from("1s")),
            ]),
        ),
    ]);
    let compose = PhpValue::map([
        ("services", PhpValue::map([(name.as_str(), service)])),
        ("networks", super::common::networks(network)),
    ]);
    yaml::dump_with_indent(&compose, 4, 2)
}

/// The container port the proxy forwards to.
fn proxy_internal_port(d: &DatabaseRow) -> Result<i64, String> {
    let ssl = d.bool("enable_ssl");
    Ok(match d.kind {
        Kind::Mariadb | Kind::Mysql => 3306,
        Kind::Postgresql => 5432,
        Kind::Redis | Kind::Keydb | Kind::Dragonfly => {
            if ssl {
                6380
            } else {
                6379
            }
        }
        Kind::Clickhouse => 9000,
        Kind::Mongodb => 27017,
        Kind::Sqlite => {
            return Err(format!("Unsupported database type: {}", d.kind.type_name()));
        }
    })
}

impl Jobs {
    /// `StartDatabaseProxy::handle`.
    pub async fn start_database_proxy(&self, d: &DatabaseRow) -> Result<(), String> {
        let server = self.server_for(d).await?;
        self.start_proxy_on(d, &server).await
    }

    async fn start_proxy_on(&self, d: &DatabaseRow, server: &ServerInfo) -> Result<(), String> {
        let internal = proxy_internal_port(d)?;
        let network = d
            .destination
            .as_ref()
            .and_then(|x| x.get("network"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("coolify")
            .to_owned();
        let public_port = d.int("public_port").unwrap_or_default();
        let name = format!("{}-proxy", d.uuid);
        let dir = proxy_dir(&d.uuid);
        let t = server.target(self).await?;
        let inspect = format!(
            "docker network inspect {} --format '{{{{.EnableIPv6}}}}'",
            quote(&network)
        );
        let ipv6 = self
            .remote()
            .run_quiet(&t, &[inspect.as_str()], RunOptions::op("database.proxy"))
            .await
            .unwrap_or_default();
        let conf = nginx_conf(
            &d.uuid,
            internal,
            public_port,
            d.int("public_port_timeout"),
            ipv6.trim() == "true",
        );
        let compose = proxy_compose(&d.uuid, public_port, &network);
        let remove = format!("docker rm -f {name}");
        self.remote()
            .run_quiet(&t, &[remove.as_str()], RunOptions::op("database.proxy"))
            .await;
        let commands = [
            format!("mkdir -p {dir}"),
            format!(
                "echo '{}' | base64 -d | tee {dir}/nginx.conf > /dev/null",
                b64(&conf)
            ),
            format!(
                "echo '{}' | base64 -d | tee {dir}/docker-compose.yaml > /dev/null",
                b64(&compose)
            ),
            format!("docker compose --project-directory {dir} -f {dir}/docker-compose.yaml pull"),
            format!("docker compose --project-directory {dir} -f {dir}/docker-compose.yaml up -d"),
        ];
        let refs: Vec<&str> = commands.iter().map(String::as_str).collect();
        if let Err(e) = self
            .remote()
            .run(&t, &refs, RunOptions::op("database.proxy"))
            .await
        {
            if NON_TRANSIENT.iter().any(|p| e.message.contains(p)) {
                let _ = dbs::update_if_changed(
                    &self.0.db,
                    d.kind,
                    d.id,
                    &[("is_public", ColumnValue::Bool(false))],
                    now(),
                )
                .await;
                tracing::warn!(database = %d.uuid, error = %e.message, "TCP proxy disabled");
                return Ok(());
            }
            return Err(e.message);
        }
        Ok(())
    }

    /// `StartDatabaseProxy::dispatch`.
    pub fn spawn_database_proxy_start(&self, d: DatabaseRow) {
        let jobs = self.clone();
        tokio::spawn(async move {
            if let Err(e) = jobs.start_database_proxy(&d).await {
                tracing::warn!(database = %d.uuid, error = %e, "TCP proxy could not be started");
            }
        });
    }

    /// `StopDatabaseProxy::handle`.
    pub async fn stop_database_proxy(&self, d: &DatabaseRow) -> Result<(), String> {
        let server = self.server_for(d).await?;
        let t = server.target(self).await?;
        let command = docker_remove_command(&format!("{}-proxy", d.uuid));
        self.remote()
            .run(&t, &[command.as_str()], RunOptions::op("database.proxy"))
            .await
            .map_err(|e| e.message)?;
        Ok(())
    }

    /// `StopDatabaseProxy::dispatch`.
    pub fn spawn_database_proxy_stop(&self, d: DatabaseRow) {
        let jobs = self.clone();
        tokio::spawn(async move {
            if let Err(e) = jobs.stop_database_proxy(&d).await {
                tracing::warn!(database = %d.uuid, error = %e, "TCP proxy could not be stopped");
            }
        });
    }
}

/// `dockerRemoveCommand`: `docker rm -f` that ignores a container that does not exist.
pub fn docker_remove_command(container: &str) -> String {
    let inner = format!("docker rm -f {}", quote(container));
    let script = format!(
        "output=$({inner} 2>&1); exit_code=$?; if [ \"$exit_code\" -ne 0 ] && printf '%s' \"$output\" | grep -Eq 'No such container:'; then exit 0; fi; if [ \"$exit_code\" -ne 0 ]; then printf '%s\\n' \"$output\" >&2; else printf '%s\\n' \"$output\"; fi; exit $exit_code"
    );
    format!("bash -c {}", quote(&script))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nginx_matches_upstream_shape() {
        let conf = nginx_conf("abc", 5432, 15432, None, false);
        assert!(conf.contains("listen 15432;\n        set $upstream abc:5432;"));
        assert!(conf.ends_with("proxy_timeout 3600s;\n   }\n}"));
    }
}
