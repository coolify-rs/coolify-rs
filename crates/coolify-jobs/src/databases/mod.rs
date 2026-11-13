//! Standalone database lifecycle: upstream's `StartDatabase` +
//! `DatabaseStartJob` + `StartPostgresql`, `StopDatabase`, `RestartDatabase`,
//! `DeleteResourceJob`, `ResourceStatusRefresher::refreshDatabase` and the
//! database part of `GetContainersStatus`.
//!
//! Commands and the Docker Compose file are built exactly as upstream builds
//! them, so a database started by either implementation runs the same way.

use std::time::Duration;

use chrono::{NaiveDateTime, SecondsFormat, Utc};
use coolify_db::databases::{self as dbs, DatabaseRow, Kind};
use coolify_db::servers::{self, ColumnValue};
use coolify_remote::{RunOptions, Stream, shell::quote};
use serde_json::{Value, json};

use crate::Jobs;
use crate::server::ServerInfo;

pub mod backup;
pub mod backup_download;
pub mod cleanup;
mod clickhouse;
pub(crate) mod common;
pub mod connection;
pub mod docker_options;
pub mod env;
pub mod file_storage;
pub mod import;
mod keydb;
mod mongodb;
mod mysql;
pub mod paths;
mod postgresql;
pub mod proxy;
pub mod rclone;
mod redis;
pub mod s3;
mod sqlite;
pub mod ssl;
pub mod storage;
pub mod storage_type;
mod ui_ops;
pub mod variables;
pub mod volume_backup;

pub(crate) use common::docker_stop_command;
pub use common::slug;
use common::{CONFIGURATION_DIR, hrtime, now};
pub use ui_ops::{ServerFileStream, configuration_dir};

/// Everything a `Start*` action reads, resolved: the database, its decrypted
/// `encrypted` columns and runtime variables, the server and the destination.
#[derive(Debug)]
pub struct StartContext<'a> {
    pub db: &'a DatabaseRow,
    /// Decrypted values of the kind's `encrypted` columns.
    pub secrets: std::collections::HashMap<&'static str, String>,
    /// `runtime_environment_variables`, resolved.
    pub variables: Vec<env::Var>,
    /// `resolvedSecretManagerValuesForRedaction()`: what the start fetched from the secret manager.
    pub fetched_secrets: Vec<String>,
    pub server: &'a ServerInfo,
    pub network: String,
    pub volumes: Vec<(String, String, Option<String>)>,
    pub now: NaiveDateTime,
    /// The SSL commands of the start (`rm -rf ssl`, or the certificate setup).
    pub ssl_commands: Vec<String>,
    /// `EnsureContentFilesOnServer::echoCommands`.
    pub ensure_commands: Vec<String>,
    /// `"$fs_path:$mount_path"` of the file storages.
    pub file_mounts: Vec<String>,
    /// `generate_log_drain_configuration`, when the server and the database use a log drain.
    pub logging: Option<coolify_compat::yaml::PhpValue>,
    /// `ImageStartPin::for(ImageStartPin::take($database), [$database->image])`: the scanned build
    /// of a "Pull latest" security update that this start pulls instead of the tag. Taking them
    /// uses them up.
    pub start_pins: crate::image_scan::update::image_start_pin::Pins,
}

impl StartContext<'_> {
    pub fn secret(&self, column: &str) -> String {
        self.secrets.get(column).cloned().unwrap_or_default()
    }

    /// `$docker_compose['services'][$name]['logging']`.
    pub fn apply_logging(&self, service: &mut coolify_compat::yaml::PhpValue) {
        if let Some(l) = &self.logging {
            service.set("logging", l.clone());
        }
    }

    /// `generateCustomDockerRunOptionsForDatabases`.
    pub fn apply_docker_options(&self, service: &mut coolify_compat::yaml::PhpValue) {
        docker_options::apply_to_service(
            service,
            self.db.str("custom_docker_run_options"),
            &self.network,
        );
    }
}

/// The start commands and the Docker Compose file of a database.
pub fn start_commands(ctx: &StartContext<'_>) -> Result<(Vec<String>, String), String> {
    match ctx.db.kind {
        Kind::Postgresql => Ok(postgresql::start_commands(ctx)),
        Kind::Redis => Ok(redis::start_commands(ctx)),
        Kind::Keydb | Kind::Dragonfly => Ok(keydb::start_commands(ctx)),
        Kind::Mongodb => Ok(mongodb::start_commands(ctx)),
        Kind::Mysql | Kind::Mariadb => Ok(mysql::start_commands(ctx)),
        Kind::Clickhouse => Ok(clickhouse::start_commands(ctx)),
        Kind::Sqlite => Ok(sqlite::start_commands(ctx)),
    }
}

pub const IN_PROGRESS: &str =
    "Another start, restart or import of this database is already in progress.";
const NOT_FUNCTIONAL: &str = "Server is not functional";
/// `ResourceStatusRefresher::HEALTH_WAIT_*`.
const HEALTH_WAIT_ATTEMPTS: u32 = 10;
const HEALTH_WAIT_INTERVAL: Duration = Duration::from_secs(3);

/// `ContainerStatusAggregator::containerStatus`.
fn container_status(c: &Value) -> String {
    let state = c
        .pointer("/State/Status")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if state == "exited" {
        return state.to_owned();
    }
    let health = c
        .pointer("/State/Health/Status")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    format!("{state}:{health}")
}

/// `removeAnsiColors`.
fn remove_ansi(text: &str) -> String {
    static ANSI: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"\x1b[\[][A-Za-z0-9];?[0-9]*m?").unwrap_or_else(|_| unreachable!())
    });
    ANSI.replace_all(text, "").into_owned()
}

/// `format_docker_command_output_to_json`: one JSON object per line.
fn json_lines(out: &str) -> Vec<Value> {
    out.lines()
        .filter_map(|l| serde_json::from_str::<Value>(l.trim()).ok())
        .filter(|v| v.is_object())
        .collect()
}

impl Jobs {
    fn reserve_database(&self, uuid: &str) -> bool {
        self.0
            .db_ops
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(uuid.to_owned())
    }

    fn release_database(&self, uuid: &str) {
        self.0
            .db_ops
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(uuid);
    }

    pub async fn server_for(&self, d: &DatabaseRow) -> Result<ServerInfo, String> {
        let server_id = d.server_id().ok_or("Database destination not found.")?;
        let row = servers::find_by_id(&self.0.db, server_id)
            .await
            .map_err(|e| e.to_string())?
            .ok_or("Server not found.")?;
        Ok(ServerInfo::from_row(&row))
    }

    /// `FlushDatabase::handle`.
    pub async fn flush_database(&self, d: &DatabaseRow) -> Result<(), String> {
        let mut cli = if d.kind == coolify_db::databases::Kind::Keydb {
            "keydb-cli".to_owned()
        } else {
            "redis-cli".to_owned()
        };
        if d.kind == coolify_db::databases::Kind::Dragonfly && d.bool("enable_ssl") {
            cli.push_str(" --tls --cacert /etc/dragonfly/certs/coolify-ca.crt --cert /etc/dragonfly/certs/server.crt --key /etc/dragonfly/certs/server.key");
        }
        let server = self.server_for(d).await?;
        let target = server.target(self).await?;
        let command = format!(
            "docker exec {} sh -c 'REDISCLI_AUTH=\"$REDIS_PASSWORD\" {cli} FLUSHALL ASYNC'",
            d.uuid
        );
        let output = self
            .remote()
            .run(
                &target,
                &[&command],
                coolify_remote::RunOptions::op("database.flush"),
            )
            .await
            .map_err(|e| e.to_string())?
            .unwrap_or_default();
        if output == "OK" {
            Ok(())
        } else if output.is_empty() {
            Err("Failed to flush the database.".to_owned())
        } else {
            Err(output)
        }
    }

    /// `StartDatabase::prerequisiteError`: the CA of a database with SSL.
    pub async fn start_prerequisite_error(&self, d: &DatabaseRow) -> Option<String> {
        if !d.bool("enable_ssl") {
            return None;
        }
        match self.server_for(d).await {
            Ok(server) => self
                .ensure_ca_certificate(&server)
                .await
                .err()
                .map(|_| ssl::MISSING_CA.to_owned()),
            Err(_) => Some(ssl::MISSING_CA.to_owned()),
        }
    }

    /// `StartDatabase::reserveOperation` + `dispatchReserved(StartDatabase::class)`.
    /// The request answers right away; the start runs in the background.
    pub fn spawn_database_start(
        &self,
        d: DatabaseRow,
        user_id: Option<i64>,
    ) -> Result<(), &'static str> {
        if !self.reserve_database(&d.uuid) {
            return Err(IN_PROGRESS);
        }
        let jobs = self.clone();
        tokio::spawn(async move {
            let uuid = d.uuid.clone();
            jobs.start_database(&d, user_id).await;
            jobs.release_database(&uuid);
        });
        Ok(())
    }

    /// `RestartDatabase`: stop (without cleanup), then start, under one reservation.
    pub fn spawn_database_restart(
        &self,
        d: DatabaseRow,
        user_id: Option<i64>,
    ) -> Result<(), &'static str> {
        if !self.reserve_database(&d.uuid) {
            return Err(IN_PROGRESS);
        }
        let jobs = self.clone();
        tokio::spawn(async move {
            let uuid = d.uuid.clone();
            match jobs.server_for(&d).await {
                Ok(server) if server.is_functional() => {
                    // `ensureRemoteSecretsResolvable`: a failing secret manager leaves it running.
                    let resolvable = match jobs.0.encrypter.as_ref() {
                        Some(enc) => env::load(&jobs.0.db, enc, &d, Some(server.id)).await,
                        None => Ok(Vec::new()),
                    };
                    if let Err(e) = resolvable {
                        jobs.record_failed_start(&d, &e, user_id).await;
                    } else {
                        jobs.stop_database(&d, &server, false).await;
                        jobs.start_database(&d, user_id).await;
                    }
                }
                Ok(_) => jobs.record_failed_start(&d, NOT_FUNCTIONAL, user_id).await,
                Err(e) => tracing::warn!(database = %uuid, error = %e, "database restart failed"),
            }
            jobs.release_database(&uuid);
        });
        Ok(())
    }

    /// `StopDatabase::dispatch`.
    pub fn spawn_database_stop(&self, d: DatabaseRow, docker_cleanup: bool) {
        let jobs = self.clone();
        tokio::spawn(async move {
            match jobs.server_for(&d).await {
                Ok(server) if server.is_functional() => {
                    jobs.stop_database(&d, &server, docker_cleanup).await
                }
                Ok(_) => {
                    tracing::info!(database = %d.uuid, "stop skipped: server is not functional")
                }
                Err(e) => tracing::warn!(database = %d.uuid, error = %e, "database stop failed"),
            }
        });
    }

    /// `DeleteResourceJob` for a database: stop, remove configuration and
    /// volumes on the server (best effort), then delete the rows.
    pub fn spawn_database_delete(
        &self,
        d: DatabaseRow,
        delete_volumes: bool,
        delete_configurations: bool,
        docker_cleanup: bool,
    ) -> tokio::task::JoinHandle<()> {
        let jobs = self.clone();
        tokio::spawn(async move {
            let server = jobs.server_for(&d).await;
            if let Ok(server) = &server
                && server.is_functional()
                && let Err(e) = server.target(&jobs).await
            {
                jobs.notify_remote_cleanup_failed(
                    d.str("name").unwrap_or_default(),
                    d.int("environment_id"),
                    server.id,
                    &e,
                )
                .await;
            }
            if let Ok(server) = server
                && server.is_functional()
                && let Ok(t) = server.target(&jobs).await
            {
                jobs.stop_database(&d, &server, docker_cleanup).await;
                let remote = jobs.remote();
                if delete_configurations {
                    remote
                        .run_quiet(
                            &t,
                            &[&format!("rm -rf {CONFIGURATION_DIR}/{}", d.uuid)],
                            RunOptions::op("database.delete"),
                        )
                        .await;
                }
                if delete_volumes
                    && let Ok(volumes) = dbs::persistent_volumes(&jobs.0.db, d.kind, d.id).await
                {
                    for (name, _, _) in volumes {
                        remote
                            .run_quiet(
                                &t,
                                &[&format!("docker volume rm -f {}", quote(&name))],
                                RunOptions::op("database.delete"),
                            )
                            .await;
                    }
                }
            }
            // `HasImageScans` (the model's `deleting` hook): the scans, scan rules, ignores and
            // notification state of the database go with it.
            if let Err(e) =
                coolify_db::image_scans::delete_resource_data(&jobs.0.db, d.morph(), d.id).await
            {
                tracing::warn!(database = %d.uuid, error = %e, "image scan data could not be deleted");
            }
            if let Err(e) = dbs::force_delete(&jobs.0.db, d.kind, d.id, delete_volumes).await {
                tracing::warn!(database = %d.uuid, error = %e, "database rows could not be deleted");
            }
        })
    }

    /// `spawn_database_delete` with upstream's defaults (the UI's delete form has no cleanup switch here).
    pub fn spawn_database_delete_ui(
        &self,
        d: DatabaseRow,
        delete_volumes: bool,
        delete_configurations: bool,
    ) {
        self.spawn_database_delete(d, delete_volumes, delete_configurations, true);
    }

    /// `logs_by_uuid`: `None` when the database has no container.
    pub async fn database_logs(
        &self,
        d: &DatabaseRow,
        lines: &str,
        timestamps: bool,
    ) -> Result<Option<String>, String> {
        let server = self.server_for(d).await?;
        if server.is_swarm() {
            return Ok(None);
        }
        let t = server.target(self).await?;
        let by_label = format!(
            "docker ps -a --filter {} --format {}",
            quote(&format!("label=coolify.databaseUuid={}", d.uuid)),
            quote("{{json .}}")
        );
        let legacy = format!(
            "docker ps -a --filter {} --filter {} --format {}",
            quote("label=coolify.databaseId"),
            quote(&format!("label=com.docker.compose.project={}", d.uuid)),
            quote("{{json .}}")
        );
        let out = self
            .remote()
            .run(
                &t,
                &[by_label.as_str(), legacy.as_str()],
                RunOptions::op("database.logs"),
            )
            .await
            .map_err(|e| e.message)?
            .unwrap_or_default();
        let containers = json_lines(&out);
        let Some(first) = containers.first() else {
            return Ok(None);
        };
        // A stopped container still has logs, like in the UI.
        let id = first.get("ID").and_then(Value::as_str).unwrap_or_default();
        let mut command = format!("docker logs -n {lines}");
        if timestamps {
            command.push_str(" --timestamps");
        }
        let command = format!("{command} {} 2>&1", quote(id));
        let logs = self
            .remote()
            .run(&t, &[command.as_str()], RunOptions::op("database.logs"))
            .await
            .map_err(|e| e.message)?
            .unwrap_or_default();
        let plain = remove_ansi(&logs);
        let enc = self
            .0
            .encrypter
            .as_ref()
            .ok_or("No application encryption key has been specified.")?;
        let credentials = connection::credentials(&self.0.db, enc, d)
            .await
            .map_err(|e| e.to_string())?;
        let mut locked: Vec<String> = d
            .kind
            .encrypted_columns()
            .iter()
            .map(|c| credentials.secret(c))
            .filter(|s| !s.is_empty())
            .collect();
        let redact_all = d
            .raw
            .get("redact_all_env_values_in_logs")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let (known, values) = self
            .owner_log_redaction_secrets(&env::Owner::database(d), redact_all, None, false, true)
            .await;
        locked.extend(values);
        let locked_refs: Vec<&str> = locked.iter().map(String::as_str).collect();
        let known_refs: Vec<&str> = known.iter().map(String::as_str).collect();
        Ok(Some(coolify_compat::redact::redact_text(
            &plain,
            &known_refs,
            &locked_refs,
        )))
    }

    /// `clone_by_uuid`: `CloneVolumeDataWithSourceStopped::run($database, $volumeJobs)`.
    pub async fn spawn_volume_clones(
        &self,
        d: DatabaseRow,
        target_server_id: i64,
        clones: Vec<(String, String)>,
    ) {
        let Some(source_server) = d.server_id() else {
            return;
        };
        let copies = clones
            .into_iter()
            .map(|(source, target)| crate::clone_volume_data::VolumeCopy {
                source_server,
                target_server: target_server_id,
                source_volume: source,
                volume_id: None,
                target_volume: target,
            })
            .collect();
        self.clone_volume_data_with_source_stopped(
            crate::clone_volume_data::CloneSource::Database(d),
            copies,
        )
        .await;
    }

    /// `StopDatabase::handle`.
    pub(crate) async fn stop_database(
        &self,
        d: &DatabaseRow,
        server: &ServerInfo,
        docker_cleanup: bool,
    ) {
        // `NotifyResourceStoppedJob::expectStop($database)`: this stop sends no notification.
        crate::notifications::resource_stopped::expect_stop(
            d.kind.morph(),
            d.id,
            crate::notifications::resource_stopped::DEFAULT_STOP_TIMEOUT_SECONDS,
        );
        if let Ok(t) = server.target(self).await {
            let stop = docker_stop_command(30, &d.uuid, server.docker_version());
            let rm = format!("docker rm -f {}", d.uuid);
            self.remote()
                .run_quiet(
                    &t,
                    &[stop.as_str(), rm.as_str()],
                    RunOptions::op("database.stop"),
                )
                .await;
        }
        let now = now();
        let _ = dbs::update_if_changed(
            &self.0.db,
            d.kind,
            d.id,
            &[(
                "status",
                ColumnValue::Text(Some(normalize_status("exited"))),
            )],
            now,
        )
        .await;
        let _ = dbs::update_if_changed(
            &self.0.db,
            d.kind,
            d.id,
            &[
                ("restart_count", ColumnValue::Int(0)),
                ("last_restart_at", ColumnValue::Text(None)),
                ("last_restart_type", ColumnValue::Text(None)),
            ],
            now,
        )
        .await;
        if docker_cleanup {
            self.spawn_cleanup_after_stop(server.id, server.uuid.clone());
        }
        // The row was read before the stop; a public database loses its proxy too.
        let public = dbs::find_in_team(&self.0.db, d.team_id, &d.uuid)
            .await
            .ok()
            .flatten()
            .map_or(d.bool("is_public"), |f| f.bool("is_public"));
        if public && let Err(e) = self.stop_database_proxy(d).await {
            tracing::warn!(database = %d.uuid, error = %e, "TCP proxy could not be stopped");
        }
    }

    async fn record_failed_start(&self, d: &DatabaseRow, message: &str, user_id: Option<i64>) {
        if let Ok(server) = self.server_for(d).await
            && let Ok(id) = self.queue_activity(d, &server, user_id).await
        {
            self.mark_failed(id, message).await;
        }
    }

    async fn queue_activity(
        &self,
        d: &DatabaseRow,
        server: &ServerInfo,
        _user_id: Option<i64>,
    ) -> Result<i64, String> {
        let props = json!({
            "server_uuid": server.uuid,
            "type": "inline",
            "type_uuid": d.uuid,
            "status": "queued",
            "team_id": server.team_id,
            "operation": "database-start",
        });
        dbs::insert_activity(&self.0.db, d.morph(), d.id, &props, now())
            .await
            .map_err(|e| e.to_string())
    }

    /// `ResourceStartActivity::markFailed`.
    async fn mark_failed(&self, activity_id: i64, message: &str) {
        let message = coolify_compat::redact::redact_text(message, &[], &[]);
        let Ok(Some((props, desc))) = dbs::activity_by_id(&self.0.db, activity_id).await else {
            return;
        };
        let mut entries: Vec<Value> = serde_json::from_str(&desc).unwrap_or_default();
        let last = entries
            .iter()
            .filter_map(|e| e.get("order").and_then(Value::as_i64))
            .max()
            .unwrap_or(0);
        entries.push(json!({"type": "stderr", "output": format!("\n{message}\n"), "timestamp": hrtime(), "batch": 1, "order": last + 1}));
        let mut merge = json!({
            "status": "error",
            "error": message,
            "failed_at": Utc::now().to_rfc3339_opts(SecondsFormat::Secs, false),
        });
        if props.get("exitCode").is_none_or(Value::is_null) {
            merge["exitCode"] = json!(1);
        }
        let description = serde_json::to_string(&entries).unwrap_or_default();
        let _ =
            dbs::update_activity(&self.0.db, activity_id, &merge, Some(&description), now()).await;
    }

    /// `StartDatabase::queueStart` + `DatabaseStartJob` + `StartPostgresql`.
    pub(crate) async fn start_database(&self, d: &DatabaseRow, user_id: Option<i64>) {
        let server = match self.server_for(d).await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(database = %d.uuid, error = %e, "database start failed");
                return;
            }
        };
        if !server.is_functional() {
            self.record_failed_start(d, NOT_FUNCTIONAL, user_id).await;
            return;
        }
        let now_ = now();
        let _ = dbs::update_if_changed(
            &self.0.db,
            d.kind,
            d.id,
            &[
                ("restart_count", ColumnValue::Int(0)),
                ("last_restart_at", ColumnValue::Text(None)),
                ("last_restart_type", ColumnValue::Text(None)),
            ],
            now_,
        )
        .await;
        let activity = match self.queue_activity(d, &server, user_id).await {
            Ok(a) => a,
            Err(e) => {
                tracing::warn!(database = %d.uuid, error = %e, "start activity could not be written");
                return;
            }
        };
        // `queueStart`: the proxy is dispatched next to the start job.
        if d.bool("is_public") && d.int("public_port").is_some_and(|p| p != 0) {
            self.spawn_database_proxy_start(d.clone());
        }
        match self.run_start(d, &server, activity).await {
            Ok(()) => {
                self.refresh_database_status(d, &server).await;
                self.after_start(d).await;
            }
            Err(message) => self.mark_failed(activity, &message).await,
        }
    }

    /// Everything the start commands read, loaded and decrypted.
    pub async fn start_context<'a>(
        &self,
        d: &'a DatabaseRow,
        server: &'a ServerInfo,
    ) -> Result<StartContext<'a>, String> {
        let Some(enc) = self.0.encrypter.as_ref() else {
            return Err("Database start failed.".into());
        };
        let mut secrets = std::collections::HashMap::new();
        for column in d.kind.encrypted_columns() {
            if let Some(cipher) = d.str(column) {
                secrets.insert(
                    *column,
                    enc.decrypt_string(cipher)
                        .map_err(|_| "Database start failed.")?,
                );
            }
        }
        let (variables, fetched_secrets) =
            env::load_for_with_fetched(&self.0.db, enc, &env::Owner::database(d), Some(server.id))
                .await?;
        let network = d
            .destination
            .as_ref()
            .and_then(|x| x.get("network"))
            .and_then(Value::as_str)
            .unwrap_or("coolify")
            .to_owned();
        let volumes = dbs::persistent_volumes(&self.0.db, d.kind, d.id)
            .await
            .map_err(|e| e.to_string())?;
        // The Start* actions do this before they build the Compose file.
        let ssl_commands = if matches!(d.kind, Kind::Clickhouse | Kind::Sqlite) {
            Vec::new()
        } else {
            self.ssl_start_commands(d, server).await?
        };
        let file_rows = coolify_db::database_parts::file_storages(&self.0.db, d.kind.morph(), d.id)
            .await
            .map_err(|e| e.to_string())?;
        let ensure_commands = self.ensure_content_files(&file_rows, d, server).await;
        let file_mounts = file_rows
            .iter()
            .map(|r| {
                format!(
                    "{}:{}",
                    coolify_db::database_parts::str_of(r, "fs_path").unwrap_or_default(),
                    coolify_db::database_parts::str_of(r, "mount_path").unwrap_or_default()
                )
            })
            .collect();
        let start_pins = self.take_start_pins(d).await;
        Ok(StartContext {
            db: d,
            secrets,
            variables,
            fetched_secrets,
            server,
            network,
            volumes,
            now: now(),
            ssl_commands,
            ensure_commands,
            file_mounts,
            logging: cleanup::log_drain_configuration(server, d),
            start_pins,
        })
    }

    /// The database as the owner of image scans (`$database` in the scan calls of the start).
    async fn scan_owner(&self, d: &DatabaseRow) -> Option<coolify_db::image_scans::ScanOwner> {
        match coolify_db::image_scans::find_owner(&self.0.db, d.kind.morph(), d.id).await {
            Ok(owner) => owner,
            Err(e) => {
                tracing::warn!(database = %d.uuid, error = %e, "the database could not be loaded for the image scan");
                None
            }
        }
    }

    /// `ImageStartPin::for(ImageStartPin::take($database), [(string) $database->image])`.
    async fn take_start_pins(
        &self,
        d: &DatabaseRow,
    ) -> crate::image_scan::update::image_start_pin::Pins {
        use crate::image_scan::update::image_start_pin;
        let Some(owner) = self.scan_owner(d).await else {
            return Vec::new();
        };
        let image = d.str("image").unwrap_or_default().to_owned();
        image_start_pin::for_images(&image_start_pin::take(&owner, None), &[image])
    }

    /// The compose file a start would write, with no change on the server (for checks).
    pub fn preview_context<'a>(
        d: &'a DatabaseRow,
        server: &'a ServerInfo,
        secrets: std::collections::HashMap<&'static str, String>,
        variables: Vec<env::Var>,
        network: String,
        volumes: Vec<(String, String, Option<String>)>,
    ) -> StartContext<'a> {
        StartContext {
            db: d,
            secrets,
            variables,
            fetched_secrets: Vec::new(),
            server,
            network,
            volumes,
            now: now(),
            ssl_commands: vec![format!("rm -rf {CONFIGURATION_DIR}/{}/ssl", d.uuid)],
            ensure_commands: Vec::new(),
            file_mounts: Vec::new(),
            logging: None,
            start_pins: Vec::new(),
        }
    }

    async fn run_start(
        &self,
        d: &DatabaseRow,
        server: &ServerInfo,
        activity: i64,
    ) -> Result<(), String> {
        let Some(enc) = self.0.encrypter.as_ref() else {
            return Err("Database start failed.".into());
        };
        let ctx = self.start_context(d, server).await?;
        let (commands, _) = start_commands(&ctx)?;
        // StartRedis writes the model's `redis_username` to each REDIS_USERNAME variable.
        if d.kind == Kind::Redis {
            for (id, username) in redis::username_updates(&ctx) {
                let value = enc.encrypt_serialized(&username);
                let _ = coolify_db::environment_variables::update_value(
                    &self.0.db,
                    id,
                    Some(&value),
                    now(),
                )
                .await;
            }
        }
        let mut locked: Vec<String> = ctx
            .secrets
            .values()
            .filter(|v| !v.is_empty())
            .cloned()
            .collect();
        locked.extend(
            ctx.variables
                .iter()
                .filter_map(|v| v.resolved.clone())
                .filter(|v| v.len() >= 4),
        );
        for value in &ctx.fetched_secrets {
            locked.extend(crate::applications::remote_redaction_values(value));
        }
        let target = server.target(self).await?;

        let _ = dbs::update_activity(
            &self.0.db,
            activity,
            &json!({"status": "in_progress"}),
            None,
            now(),
        )
        .await;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<(Stream, String)>();
        let db = self.0.db.clone();
        let secrets = locked;
        let writer = tokio::spawn(async move {
            let locked: Vec<&str> = secrets.iter().map(String::as_str).collect();
            let mut entries: Vec<Value> = Vec::new();
            // Output is redacted per complete line, so a secret split across two chunks is found.
            let (mut out, mut err) = (String::new(), String::new());
            while let Some((stream, chunk)) = rx.recv().await {
                let pending = if stream == Stream::Err {
                    &mut err
                } else {
                    &mut out
                };
                pending.push_str(&chunk);
                let lines = coolify_compat::redact::take_complete_lines(pending);
                append_start_output(&db, activity, &mut entries, stream, &lines, &locked).await;
            }
            append_start_output(&db, activity, &mut entries, Stream::Out, &out, &locked).await;
            append_start_output(&db, activity, &mut entries, Stream::Err, &err, &locked).await;
        });
        let result = self
            .remote()
            .run_streaming(&target, &commands, RunOptions::op("database.start"), tx)
            .await;
        let _ = writer.await;
        // `executeDatabaseStartCommands()` ends with `ScanResourceImagesJob::dispatchAfterStart()`,
        // whatever the result of the commands.
        if let Some(owner) = self.scan_owner(d).await {
            crate::image_scan::scan_jobs::dispatch_after_start(self, &owner).await;
        }
        match result {
            Ok(_) => {
                let _ = dbs::update_activity(
                    &self.0.db,
                    activity,
                    &json!({"status": "finished", "exitCode": 0}),
                    None,
                    now(),
                )
                .await;
                Ok(())
            }
            Err(e) => {
                let _ = dbs::update_activity(
                    &self.0.db,
                    activity,
                    &json!({"status": "error", "exitCode": e.code}),
                    None,
                    now(),
                )
                .await;
                // DatabaseStartJob::failed: other errors are not shown as they are.
                Err("Database start failed.".into())
            }
        }
    }

    /// `Database\Heading::activityFinished`: `started_at` once running, and the
    /// configuration hash the "restart required" check compares with.
    async fn after_start(&self, d: &DatabaseRow) {
        let Ok(Some(fresh)) = dbs::find_in_team(&self.0.db, d.team_id, &d.uuid).await else {
            return;
        };
        let now = now();
        if fresh.str("status").unwrap_or_default().contains("running")
            && fresh.str("started_at").is_none()
        {
            let _ = dbs::update_if_changed(
                &self.0.db,
                d.kind,
                d.id,
                &[("started_at", ColumnValue::Time(now))],
                now,
            )
            .await;
        }
        let Ok(hash) = self.database_config_hash(&fresh).await else {
            return;
        };
        let _ = dbs::update_if_changed(
            &self.0.db,
            d.kind,
            d.id,
            &[("config_hash", ColumnValue::Text(Some(hash)))],
            now,
        )
        .await;
    }

    /// `config_hash` with the database's variables.
    pub async fn database_config_hash(&self, d: &DatabaseRow) -> Result<String, String> {
        let enc = self
            .0
            .encrypter
            .as_ref()
            .ok_or("No application encryption key has been specified.")?;
        let variables = env::load_stored(&self.0.db, enc, d).await?;
        Ok(config_hash(d, &env::config_hash_values(&variables)))
    }

    /// `ResourceStatusRefresher::refreshDatabase`.
    pub async fn refresh_database_status(&self, d: &DatabaseRow, server: &ServerInfo) {
        if !server.is_functional() || server.is_swarm() {
            return;
        }
        let Ok(t) = server.target(self).await else {
            return;
        };
        let command = format!(
            "docker container inspect {} --format '{{{{json .}}}}'",
            quote(&d.uuid)
        );
        for attempt in 1..=HEALTH_WAIT_ATTEMPTS {
            let out = self
                .remote()
                .run_quiet(&t, &[command.as_str()], RunOptions::op("database.status"))
                .await
                .unwrap_or_default();
            let containers = json_lines(&out);
            let Some(c) = containers
                .iter()
                .find(|c| c.get("Name").and_then(Value::as_str) == Some(&format!("/{}", d.uuid)))
            else {
                return;
            };
            let stored = dbs::find_in_team(&self.0.db, d.team_id, &d.uuid)
                .await
                .ok()
                .flatten();
            let stored = stored
                .as_ref()
                .and_then(|r| r.str("status"))
                .unwrap_or_default()
                .to_owned();
            self.store_status(d, &stored, &container_status(c)).await;
            let starting =
                c.pointer("/State/Health/Status").and_then(Value::as_str) == Some("starting");
            if !starting || attempt >= HEALTH_WAIT_ATTEMPTS {
                return;
            }
            tokio::time::sleep(HEALTH_WAIT_INTERVAL).await;
        }
    }

    /// `ResourceStatusRefresher::store` / `GetContainersStatus`: the stored status
    /// (through the model accessor) is compared with the container's raw status.
    async fn store_status(&self, d: &DatabaseRow, stored: &str, raw: &str) {
        let now = now();
        if normalize_status(stored) != raw {
            let value = ColumnValue::Text(Some(normalize_status(raw)));
            let _ =
                dbs::update_if_changed(&self.0.db, d.kind, d.id, &[("status", value)], now).await;
        } else {
            let _ = dbs::touch_online(&self.0.db, d.kind, d.id, now).await;
        }
    }

    /// The database part of `GetContainersStatus` for one server.
    pub async fn check_server_databases(&self, server_id: i64) {
        let Ok(Some(row)) = servers::find_by_id(&self.0.db, server_id).await else {
            return;
        };
        let server = ServerInfo::from_row(&row);
        if !server.is_functional() || server.is_swarm() {
            return;
        }
        let Ok(databases) = dbs::list_for_server(&self.0.db, server_id).await else {
            return;
        };
        let databases: Vec<DatabaseRow> =
            databases.into_iter().filter(|d| d.kind.ported()).collect();
        if databases.is_empty() {
            return;
        }
        let Ok(t) = server.target(self).await else {
            return;
        };
        // `Server::getContainers`: every container with its inspect data.
        let script = "docker container inspect $(docker container ls -aq) --format '{{json .}}'";
        let opts = RunOptions::op("server.containers").timeout(Duration::from_secs(30));
        let Some(out) = self.remote().run_quiet(&t, &[script], opts).await else {
            return;
        };
        let containers = json_lines(&out);
        self.apply_database_containers(
            &databases,
            &containers,
            crate::applications::status::Snapshot::POLLED,
        )
        .await;
        self.check_database_proxies(&databases, &containers).await;
    }

    /// The TCP proxy part of `GetContainersStatus` (not Swarm): a public database gets its proxy
    /// (again when an older version started it with an older configuration), a private one loses it.
    async fn check_database_proxies(&self, databases: &[DatabaseRow], containers: &[Value]) {
        for c in containers {
            let uuid = c
                .pointer("/Config/Labels/coolify.databaseUuid")
                .and_then(Value::as_str);
            let Some(d) = uuid.and_then(|u| databases.iter().find(|d| d.uuid == u)) else {
                continue;
            };
            let name = format!("/{}-proxy", d.uuid);
            let found = containers
                .iter()
                .find(|p| p.get("Name").and_then(Value::as_str) == Some(name.as_str()));
            let result = if d.bool("is_public") {
                match found {
                    None => self.start_database_proxy(d).await,
                    Some(p)
                        if proxy::claim_outdated_refresh(&d.uuid, p.pointer("/Config/Labels")) =>
                    {
                        self.start_database_proxy(d).await
                    }
                    Some(_) => Ok(()),
                }
            } else if found.is_some() {
                self.stop_database_proxy(d).await
            } else {
                Ok(())
            };
            if let Err(e) = result {
                tracing::warn!(database = %d.uuid, error = %e, "TCP proxy check failed");
            }
        }
    }

    /// The database part of `GetContainersStatus` and `PushServerUpdateJob`.
    pub async fn apply_database_containers(
        &self,
        databases: &[DatabaseRow],
        containers: &[Value],
        snapshot: crate::applications::status::Snapshot,
    ) {
        use crate::notifications::resource_stopped;
        // `NotifyResourceStoppedJob::upResourceIds()` before the statuses change,
        // `dispatchForStopped()` after.
        let up = resource_stopped::up_resources(
            databases
                .iter()
                .map(|d| (d.kind.morph(), d.id, d.str("status"))),
        );
        self.apply_database_container_statuses(databases, containers, snapshot)
            .await;
        resource_stopped::dispatch_for_stopped(self, &up).await;
    }

    async fn apply_database_container_statuses(
        &self,
        databases: &[DatabaseRow],
        containers: &[Value],
        snapshot: crate::applications::status::Snapshot,
    ) {
        let now = now();
        let mut found = Vec::new();
        for c in containers {
            let labels = c.pointer("/Config/Labels");
            let uuid = labels
                .and_then(|l| l.get("coolify.databaseUuid"))
                .and_then(Value::as_str);
            let Some(d) = uuid.and_then(|u| databases.iter().find(|d| d.uuid == u)) else {
                continue;
            };
            found.push(d.id);
            self.store_status(d, d.str("status").unwrap_or_default(), &container_status(c))
                .await;
            let restarts = c.get("RestartCount").and_then(Value::as_i64).unwrap_or(0);
            if restarts > d.int("restart_count").unwrap_or(0) {
                let _ = dbs::update_if_changed(
                    &self.0.db,
                    d.kind,
                    d.id,
                    &[
                        ("restart_count", ColumnValue::Int(restarts)),
                        ("last_restart_at", ColumnValue::Time(now)),
                        ("last_restart_type", ColumnValue::Text(Some("crash".into()))),
                    ],
                    now,
                )
                .await;
            }
        }
        if !snapshot.not_found_counts(containers) {
            return;
        }
        for d in databases.iter().filter(|d| !found.contains(&d.id)) {
            if d.str("status").unwrap_or_default().starts_with("exited") {
                continue;
            }
            let _ = dbs::update_if_changed(
                &self.0.db,
                d.kind,
                d.id,
                &[
                    (
                        "status",
                        ColumnValue::Text(Some(normalize_status("exited"))),
                    ),
                    ("restart_count", ColumnValue::Int(0)),
                    ("last_restart_at", ColumnValue::Text(None)),
                    ("last_restart_type", ColumnValue::Text(None)),
                ],
                now,
            )
            .await;
        }
    }

    /// `ServerManagerJob` (database statuses only): every minute, each functional server.
    pub fn spawn_status_poller(&self) {
        let jobs = self.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(60));
            loop {
                tick.tick().await;
                if let Ok(ids) = dbs::servers_with_databases(&jobs.0.db).await {
                    for id in ids {
                        jobs.check_server_databases(id).await;
                    }
                }
                // GetContainersStatus also covers the services of a server.
                if let Ok(ids) = coolify_db::services::servers_with_services(&jobs.0.db).await {
                    for id in ids {
                        jobs.check_server_services(id).await;
                    }
                }
                // GetContainersStatus also covers the applications of a server.
                if let Ok(ids) =
                    coolify_db::applications::servers_with_applications(&jobs.0.db).await
                {
                    for id in ids {
                        jobs.check_server_applications(id).await;
                    }
                }
            }
        });
    }

    /// The database's latest start activity: `(status, log entries)`.
    pub async fn database_start_log(&self, uuid: &str) -> Option<(String, Value)> {
        let (_, props, description) = dbs::latest_start_activity(&self.0.db, uuid)
            .await
            .ok()
            .flatten()?;
        let status = props
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        Some((
            status,
            serde_json::from_str(&description).unwrap_or(Value::Array(vec![])),
        ))
    }
}

/// `isConfigurationChanged`'s hash; `variables` is `env::config_hash_values` of the
/// database's variables.
pub fn config_hash(d: &DatabaseRow, variables: &str) -> String {
    use md5::Digest as _;
    let s = |k: &str| d.str(k).unwrap_or_default().to_owned();
    let i = |k: &str, default: i64| d.int(k).unwrap_or(default);
    let enabled = d
        .raw
        .get("health_check_enabled")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let health = format!(
        "{}|{}|{}|{}|{}",
        i64::from(enabled),
        i("health_check_interval", 15),
        i("health_check_timeout", 5),
        i("health_check_retries", 5),
        i("health_check_start_period", 5)
    );
    let fields: &[&str] = match d.kind {
        Kind::Postgresql => &["postgres_initdb_args", "postgres_host_auth_method"],
        Kind::Redis => &["redis_conf"],
        Kind::Mongodb => &["mongo_conf"],
        Kind::Mysql => &["mysql_conf"],
        Kind::Mariadb => &["mariadb_conf"],
        Kind::Keydb => &["keydb_conf"],
        Kind::Dragonfly | Kind::Clickhouse | Kind::Sqlite => &[],
    };
    let mut input = format!("{}{}", s("image"), s("ports_mappings"));
    for f in fields {
        input.push_str(&s(f));
    }
    input.push_str(&health);
    input.push_str(variables);
    hex(&md5::Md5::digest(input.as_bytes()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// `ConfigurationChecker`: the hash differs, one was saved before, and the database is not exited.
pub fn configuration_changed(d: &DatabaseRow, variables: &str) -> bool {
    let saved = d.str("config_hash");
    saved.is_some_and(|h| h != config_hash(d, variables))
        && !d.str("status").unwrap_or_default().starts_with("exited")
}

/// The model's `status` mutator: `running (healthy)` / `running:healthy` / `exited` → `status:health`.
pub fn normalize_status(v: &str) -> String {
    let (status, health) = if let Some(i) = v.find('(') {
        let rest = &v[i + 1..];
        (
            v[..i].trim(),
            rest.split(')').next().unwrap_or_default().trim(),
        )
    } else if let Some((s, h)) = v.split_once(':') {
        (s.trim(), h.trim())
    } else {
        (v, "unhealthy")
    };
    format!("{status}:{health}")
}

/// `DatabaseStartCommandExecutor::appendOutput()` with the redacted text.
async fn append_start_output(
    db: &coolify_db::Db,
    activity: i64,
    entries: &mut Vec<Value>,
    stream: Stream,
    text: &str,
    locked: &[&str],
) {
    let output = coolify_compat::redact::redact_text(text, &[], locked);
    if output.is_empty() {
        return;
    }
    entries.push(json!({"type": stream.as_str(), "output": output, "timestamp": hrtime(), "batch": 1, "order": entries.len() + 1}));
    let _ = dbs::update_activity(
        db,
        activity,
        &json!({}),
        Some(&serde_json::to_string(entries).unwrap_or_default()),
        now(),
    )
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs() {
        assert_eq!(slug("postgresql-database-abc"), "postgresql-database-abc");
        assert_eq!(slug("Databases E2E"), "databases-e2e");
        assert_eq!(slug("My_App @ Home!"), "my-app-at-home");
        assert_eq!(slug("  --x--  "), "x");
    }

    #[test]
    fn statuses() {
        assert_eq!(normalize_status("exited"), "exited:unhealthy");
        assert_eq!(normalize_status("running:healthy"), "running:healthy");
        assert_eq!(normalize_status("running (starting)"), "running:starting");
        assert_eq!(normalize_status("exited:unhealthy"), "exited:unhealthy");
    }

    #[test]
    fn stop_flags() {
        assert_eq!(
            docker_stop_command(10, "x", Some("29.1.3")),
            "docker stop --timeout=10 x"
        );
        assert_eq!(
            docker_stop_command(10, "x", Some("27.5.1")),
            "docker stop --time=10 x"
        );
        assert_eq!(
            docker_stop_command(30, "x", None),
            "docker stop --time=30 x"
        );
    }
}
