//! Server work of the service pages: `LocalFileVolume::loadStorageOnServer`,
//! `validateComposeFile`, the TCP proxy of a service database and `StopService` with cleanup.

use coolify_compat::yaml::{Key, PhpValue};
use coolify_db::services::{self as svc, ServiceRow, SubRow};
use coolify_remote::{RunOptions, Target, shell::quote};
use serde_json::Value;

use crate::Jobs;
use crate::compose_parser::{php, validate};
use crate::databases::common::b64;
use crate::databases::proxy;
use crate::services::storage::{is_admin_controlled, normalize_unix_path, resolved_fs_path};
use crate::services::workdir;

/// `LocalFileVolume::MAX_CONTENT_SIZE`.
pub const MAX_CONTENT_SIZE: usize = 5_242_880;
/// `LocalFileVolume::BINARY_PLACEHOLDER`.
pub const BINARY_PLACEHOLDER: &str = "[binary file]";
/// `LocalFileVolume::TOO_LARGE_PLACEHOLDER`.
pub const TOO_LARGE_PLACEHOLDER: &str = "[file too large to display]";

fn is_key(k: &Key, name: &str) -> bool {
    matches!(k, Key::Str(s) if s == name)
}

/// `stripCoolifyCustomFields($yamlCompose)`.
fn strip_custom_fields(yaml: &mut PhpValue) {
    let PhpValue::Array(top) = yaml else { return };
    let Some(services) = top.iter_mut().find(|(k, _)| is_key(k, "services")) else {
        return;
    };
    let PhpValue::Array(services) = &mut services.1 else {
        return;
    };
    for (_, service) in services.iter_mut() {
        let PhpValue::Array(service) = service else {
            continue;
        };
        service.retain(|(k, _)| !is_key(k, "exclude_from_hc"));
        let Some((_, PhpValue::Array(volumes))) =
            service.iter_mut().find(|(k, _)| is_key(k, "volumes"))
        else {
            continue;
        };
        for (_, volume) in volumes.iter_mut() {
            if let PhpValue::Array(v) = volume {
                v.retain(|(k, _)| {
                    !["content", "isDirectory", "is_directory"]
                        .iter()
                        .any(|n| is_key(k, n))
                });
            }
        }
    }
}

impl Jobs {
    /// `validateComposeFile($compose, $server_id)`.
    pub async fn validate_compose_file(&self, s: &ServiceRow, compose: &str) -> Result<(), String> {
        let (_, target) = self.service_target(s).await?;
        let mut yaml = validate::parse_docker_compose_yaml(compose).map_err(|e| e.to_string())?;
        strip_custom_fields(&mut yaml);
        let encoded = b64(&coolify_compat::yaml::dump(&yaml, 2));
        let uuid = php::str_random(18);
        let commands = [
            format!("echo {encoded} | base64 -d | tee /tmp/{uuid}.yml > /dev/null"),
            format!("chmod 600 /tmp/{uuid}.yml"),
            format!(
                "docker compose -f /tmp/{uuid}.yml config --no-interpolate --no-path-resolution -q"
            ),
            format!("rm /tmp/{uuid}.yml"),
        ];
        let refs: Vec<&str> = commands.iter().map(String::as_str).collect();
        let result = self
            .remote()
            .run(&target, &refs, RunOptions::op("service.validate"))
            .await;
        let cleanup = format!("rm /tmp/{uuid}.yml");
        self.remote()
            .run_quiet(
                &target,
                &[cleanup.as_str()],
                RunOptions::op("service.validate"),
            )
            .await;
        result.map(|_| ()).map_err(|e| e.message)
    }

    /// `LocalFileVolume::loadStorageOnServer()` for a mount of a service part.
    pub async fn load_service_storage(
        &self,
        s: &ServiceRow,
        target: &Target,
        id: i64,
    ) -> Result<(), String> {
        let enc = self
            .0
            .encrypter
            .as_ref()
            .ok_or("No application encryption key has been specified.")?;
        let row = svc::file_storage_by_id(&self.0.db, id)
            .await
            .map_err(|e| e.to_string())?
            .ok_or("Storage not found.")?;
        let text = |k: &str| row.get(k).and_then(Value::as_str).unwrap_or_default();
        if row
            .get("is_host_file")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            return Ok(());
        }
        let dir = workdir(&s.uuid);
        let mut path = resolved_fs_path(text("fs_path"), &dir);
        if !is_admin_controlled(
            s.str("docker_compose_raw"),
            text("fs_path"),
            text("mount_path"),
            &dir,
        ) {
            path = normalize_unix_path(&path)?;
        }
        validate::validate_shell_safe_path(&path, "storage path")?;
        let escaped = quote(&path);
        let probe = format!("test -f {escaped} && echo OK || echo NOK");
        let is_file = self
            .remote()
            .run(target, &[probe.as_str()], RunOptions::op("service.storage"))
            .await
            .map_err(|e| e.message)?
            .unwrap_or_default();
        if is_file.trim() != "OK" {
            return Ok(());
        }
        let stat = format!("stat -c%s {escaped} 2>/dev/null");
        let size = self
            .remote()
            .run_quiet(target, &[stat.as_str()], RunOptions::op("service.storage"))
            .await
            .unwrap_or_default();
        let content = if size.trim().parse::<usize>().unwrap_or(0) > MAX_CONTENT_SIZE {
            TOO_LARGE_PLACEHOLDER.to_owned()
        } else {
            let read = format!("head -c {} {escaped}", MAX_CONTENT_SIZE + 1);
            let c = self
                .remote()
                .run_quiet(target, &[read.as_str()], RunOptions::op("service.storage"))
                .await
                .unwrap_or_default();
            if c.len() > MAX_CONTENT_SIZE {
                TOO_LARGE_PLACEHOLDER.to_owned()
            } else if c
                .chars()
                .any(|ch| matches!(ch, '\0'..='\x08' | '\x0E'..='\x1F' | '\x7F'))
            {
                BINARY_PLACEHOLDER.to_owned()
            } else {
                c
            }
        };
        svc::storage_update(
            &self.0.db,
            "local_file_volumes",
            id,
            &[
                (
                    "content",
                    coolify_db::servers::ColumnValue::Text(Some(enc.encrypt_string(&content))),
                ),
                (
                    "is_directory",
                    coolify_db::servers::ColumnValue::Bool(false),
                ),
            ],
            Jobs::now(),
        )
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
    }

    /// `StartDatabaseProxy::handle` with a `ServiceDatabase`.
    pub async fn start_service_database_proxy(
        &self,
        s: &ServiceRow,
        sub: &SubRow,
        database_type: &str,
    ) -> Result<(), String> {
        let internal = match database_type {
            "standalone-mariadb" | "standalone-mysql" => 3306,
            "standalone-postgresql" | "standalone-supabase/postgres" => 5432,
            "standalone-redis" | "standalone-keydb" | "standalone-dragonfly" => 6379,
            "standalone-clickhouse" => 9000,
            "standalone-mongodb" => 27017,
            other => return Err(format!("Unsupported database type: {other}")),
        };
        let (_, t) = self.service_target(s).await?;
        let network = s.uuid.clone();
        let container = format!("{}-{}", sub.name, s.uuid);
        let port = sub.int("public_port").unwrap_or_default();
        let dir = proxy::proxy_dir(&sub.uuid);
        let name = format!("{}-proxy", sub.uuid);
        let inspect = format!(
            "docker network inspect {} --format '{{{{.EnableIPv6}}}}'",
            quote(&network)
        );
        let ipv6 = self
            .remote()
            .run_quiet(&t, &[inspect.as_str()], RunOptions::op("database.proxy"))
            .await
            .unwrap_or_default();
        let conf = proxy::nginx_conf(
            &container,
            internal,
            port,
            sub.int("public_port_timeout"),
            ipv6.trim() == "true",
        );
        let compose = proxy::proxy_compose(&sub.uuid, port, &network);
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
        self.remote()
            .run(&t, &refs, RunOptions::op("database.proxy"))
            .await
            .map(|_| ())
            .map_err(|e| e.message)
    }

    /// `StopDatabaseProxy::handle` with a `ServiceDatabase`.
    pub async fn stop_service_database_proxy(
        &self,
        s: &ServiceRow,
        sub: &SubRow,
    ) -> Result<(), String> {
        let (_, t) = self.service_target(s).await?;
        let command = proxy::docker_remove_command(&format!("{}-proxy", sub.uuid));
        self.remote()
            .run(&t, &[command.as_str()], RunOptions::op("database.proxy"))
            .await
            .map(|_| ())
            .map_err(|e| e.message)
    }

    /// `DeleteService::removeSubresourceContainer`: true when the container was removed.
    pub async fn remove_sub_container(&self, s: &ServiceRow, sub: &SubRow) -> bool {
        let Ok((server, target)) = self.service_target(s).await else {
            return false;
        };
        if !server.is_functional() {
            return false;
        }
        let name = quote(&format!("{}-{}", sub.name, s.uuid));
        let stop = format!("docker rm -f {name}");
        self.remote()
            .run(&target, &[stop.as_str()], RunOptions::op("service.stop"))
            .await
            .is_ok()
    }

    /// `StopService::dispatch($service, false, $dockerCleanup)`.
    pub fn spawn_service_stop_with(&self, s: ServiceRow, cleanup: bool) {
        let jobs = self.clone();
        tokio::spawn(async move {
            jobs.stop_service(&s, false).await;
            if cleanup && let Some(id) = s.server_id() {
                // `CleanupDocker::dispatchAfterStop`: skipped while the last run is recent.
                if let Ok(Some(row)) = coolify_db::servers::find_by_id(&jobs.0.db, id).await {
                    jobs.spawn_cleanup_after_stop(id, row.uuid);
                }
            }
        });
    }
}
