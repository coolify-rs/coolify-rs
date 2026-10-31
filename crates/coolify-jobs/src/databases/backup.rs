//! `DatabaseBackupJob` for standalone databases, `removeOldBackups` and the retention
//! helpers of bootstrap/helpers/databases.php.
//!
//! The scheduler plugs in with [`Jobs::dispatch_scheduled_backup`]: it does what
//! `ScheduledJobManager::processScheduledBackup` does once a schedule is due.

use std::collections::HashSet;
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use chrono::NaiveDateTime;
use coolify_db::database_parts as parts;
use coolify_db::databases::{self as dbs, DatabaseRow, Kind};
use coolify_db::records::Record;
use coolify_db::servers::ColumnValue;
use coolify_domain::new_public_id;
use coolify_remote::{RunOptions, Target, shell::quote};
use serde_json::Value;

use super::common::{helper_image, now, slug};
use super::connection as conn;
use super::s3::{S3Target, minio_resolve_options};
use crate::Jobs;
use crate::compose_parser::validate::validate_shell_safe_path;
use crate::server::ServerInfo;

/// `withoutOverlapping('database-backup-'.$id)`.
static RUNNING: LazyLock<Mutex<HashSet<i64>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

/// `StandaloneSqlite::DATA_DIRECTORY`.
const SQLITE_DATA_DIRECTORY: &str = "/var/lib/sqlite";

/// How one database of a backup job ended, for its notification.
enum BackupOutcome {
    Success,
    Failed(String),
    S3Warning(String),
}

/// `pipeToFileKeepingExitStatus`.
fn pipe_to_file(source: &str, sink: &str, escaped_file: &str) -> String {
    format!(
        "status=$( {{ {{ {source}; echo $? >&3; }} | {sink} > {escaped_file}; }} 3>&1 ) && [ \"$status\" -eq 0 ]"
    )
}

/// `BackupCompression::compressorCommand`.
pub(crate) fn compressor_command(percentage: i64) -> String {
    let p = if [25, 50, 75, 100].contains(&percentage) {
        percentage
    } else {
        25
    };
    format!(
        "if command -v pigz >/dev/null 2>&1; then printf 'pigz -3 -p %s' \"$(( ($(nproc) * {p} + 99) / 100 ))\"; else printf 'gzip -3'; fi"
    )
}

/// `backupFilenamePart`.
fn filename_part(database: &str) -> String {
    database
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "_.-".contains(c) {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// `exitCodeReason`.
fn exit_code_reason(code: Option<i32>) -> Option<&'static str> {
    match code {
        Some(137) => Some(
            "The backup stopped with exit code 137: the dump process or the database container was killed (SIGKILL). This is usually the out-of-memory killer: the dump runs inside the database container and shares its memory limit. Check the memory limit of the database and the kernel log (dmesg) on the server.",
        ),
        Some(143) => Some(
            "The backup stopped with exit code 143: the dump process or the database container was terminated (SIGTERM), for example because the database was stopped or restarted during the backup.",
        ),
        Some(255) => Some(
            "The backup stopped with exit code 255: the SSH connection to the server closed. Check the network connection and the SSH server log on the server.",
        ),
        _ => None,
    }
}

/// The state of one job run.
struct Run<'a> {
    backup: &'a Record,
    database: &'a DatabaseRow,
    server: &'a ServerInfo,
    target: Target,
    backup_dir: String,
    secrets: Vec<String>,
    /// `dump_all`.
    dump_all: bool,
    timeout: Duration,
}

impl Run<'_> {
    fn redact(&self, text: &str) -> String {
        let locked: Vec<&str> = self.secrets.iter().map(String::as_str).collect();
        coolify_compat::redact::redact_text(text, &[], &locked)
    }

    /// `backupDirectoryCommands`.
    fn directory_commands(&self) -> Vec<String> {
        let dir = quote(&self.backup_dir);
        let mut out = vec![format!("mkdir -p {dir}")];
        if self.server.is_non_root() {
            out.push(format!(
                "find {dir} -user root -exec chown -h {user}:{user} {{}} + && chmod o-rwx {dir}",
                user = self.server.user
            ));
        }
        out
    }

    /// `buildCompressedDumpCommand`.
    fn compressed_dump(&self, dump: &str, escaped_location: &str, jobs: &Jobs) -> String {
        let percentage = self
            .server
            .settings
            .get("backup_compression_cpu_percentage")
            .and_then(Value::as_i64)
            .unwrap_or(25);
        let script = format!(
            "compressor=$({}); exec $compressor",
            compressor_command(percentage)
        );
        pipe_to_file(
            dump,
            &format!(
                "docker run --rm -i {} sh -c {}",
                quote(&helper_image(jobs)),
                quote(&script)
            ),
            escaped_location,
        )
    }
}

impl Jobs {
    /// `dispatch(new DatabaseBackupJob($backup))`: runs in the background.
    pub fn spawn_database_backup(&self, backup_id: i64) {
        let jobs = self.clone();
        tokio::spawn(async move {
            jobs.run_database_backup(backup_id).await;
        });
    }

    /// What `ScheduledJobManager::processScheduledBackup` does for a due schedule: deletes a
    /// schedule whose database or server is gone, skips a server that is not functional, then
    /// queues the job. Returns whether a backup was queued.
    pub async fn dispatch_scheduled_backup(&self, backup_id: i64) -> bool {
        let Ok(Some(backup)) = parts::backup_by_id(&self.0.db, backup_id).await else {
            return false;
        };
        let Some(database) = self.backup_database(&backup).await else {
            let _ = parts::delete_backup(&self.0.db, backup_id).await;
            tracing::warn!(
                backup = backup_id,
                skip_reason = "database_deleted",
                "backup skipped"
            );
            return false;
        };
        let Ok(server) = self.server_for(&database).await else {
            let _ = parts::delete_backup(&self.0.db, backup_id).await;
            tracing::warn!(
                backup = backup_id,
                skip_reason = "server_deleted",
                "backup skipped"
            );
            return false;
        };
        if !server.is_functional() {
            tracing::warn!(
                backup = backup_id,
                skip_reason = "server_not_functional",
                "backup skipped"
            );
            return false;
        }
        self.spawn_database_backup(backup_id);
        true
    }

    /// `$backup->database` of a schedule.
    async fn backup_database(&self, backup: &Record) -> Option<DatabaseRow> {
        let morph = parts::str_of(backup, "database_type")?;
        let id = backup.get("database_id").and_then(Value::as_i64)?;
        let team = backup.get("team_id").and_then(Value::as_i64)?;
        if morph == dbs::SERVICE_DATABASE_MORPH {
            return dbs::service_database_by_id(&self.0.db, id)
                .await
                .ok()
                .flatten();
        }
        let kind = Kind::ALL.into_iter().find(|k| k.morph() == morph)?;
        let uuid: String = dbs::uuid_by_id(&self.0.db, kind, id).await.ok().flatten()?;
        dbs::find_in_team(&self.0.db, team, &uuid)
            .await
            .ok()
            .flatten()
    }

    /// `DatabaseBackupJob::handle`.
    pub async fn run_database_backup(&self, backup_id: i64) {
        {
            let mut running = RUNNING.lock().unwrap_or_else(|p| p.into_inner());
            if !running.insert(backup_id) {
                return;
            }
        }
        if let Err(e) = self.run_backup_inner(backup_id).await {
            tracing::warn!(backup = backup_id, error = %e, "database backup job failed");
        }
        RUNNING
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&backup_id);
    }

    async fn run_backup_inner(&self, backup_id: i64) -> Result<(), String> {
        let db = &self.0.db;
        let backup = parts::backup_by_id(db, backup_id)
            .await
            .map_err(|e| e.to_string())?
            .ok_or("backup not found")?;
        let team_id = backup.get("team_id").and_then(Value::as_i64).unwrap_or(-1);
        let Some(team_name) = coolify_db::teams::find_name(db, team_id)
            .await
            .map_err(|e| e.to_string())?
        else {
            // logSkippedRun('team_not_found'), then the schedule goes.
            parts::delete_backup(db, backup_id)
                .await
                .map_err(|e| e.to_string())?;
            return Ok(());
        };
        let mut database = self
            .backup_database(&backup)
            .await
            .ok_or("Database not found?!")?;
        let server = self.server_for(&database).await?;
        let timeout = backup
            .get("timeout")
            .and_then(Value::as_i64)
            .unwrap_or(3600);
        let _ = parts::fail_stale_executions(db, backup_id, timeout * 2, now()).await;

        let status = database.str("status").unwrap_or_default();
        if !status.starts_with("running") && database.id != 0 {
            tracing::warn!(
                backup = backup_id,
                skip_reason = "database_not_running",
                database_status = status,
                "database backup job ended without a backup"
            );
            return Ok(());
        }
        let dump_all = parts::bool_of(&backup, "dump_all");
        if self.database_operation_in_progress(&database).await {
            // recordSkippedBackup
            let names = if dump_all {
                Some("all".to_owned())
            } else {
                parts::str_of(&backup, "databases_to_backup").map(str::to_owned)
            };
            let names = names
                .filter(|n| !n.trim().is_empty())
                .map(|n| n.chars().take(250).collect::<String>());
            parts::create_execution(
                db,
                backup_id,
                &new_public_id(),
                names.as_deref(),
                None,
                Some("failed"),
                Some(
                    "Skipped: a start, restart or import of this database was in progress. A backup taken now could contain a partly restored database.",
                ),
                now(),
            )
            .await
            .map_err(|e| e.to_string())?;
            return Ok(());
        }

        let kind = database.kind;
        let mut to_backup = parts::str_of(&backup, "databases_to_backup")
            .filter(|s| !s.trim().is_empty())
            .map(str::to_owned);
        let mut service_secrets = std::collections::HashMap::new();
        if database.is_service() {
            let target = server.target(self).await?;
            let Some(found) = self
                .service_database_environment(&mut database, &backup, &target, &mut service_secrets)
                .await?
            else {
                return Ok(());
            };
            to_backup = found;
        }
        if to_backup.is_none() {
            to_backup = match kind {
                Kind::Postgresql => database.str("postgres_db").map(str::to_owned),
                Kind::Mongodb => Some("*".to_owned()),
                Kind::Mysql => database.str("mysql_database").map(str::to_owned),
                Kind::Mariadb => database.str("mariadb_database").map(str::to_owned),
                Kind::Clickhouse => database.str("clickhouse_db").map(str::to_owned),
                Kind::Sqlite => database.str("sqlite_databases").map(str::to_owned),
                _ => {
                    tracing::warn!(
                        backup = backup_id,
                        skip_reason = "unsupported_database_type",
                        "database backup job ended without a backup"
                    );
                    return Ok(());
                }
            };
        }
        // databasesToBackup
        let names: Vec<String> =
            if dump_all && matches!(kind, Kind::Postgresql | Kind::Mysql | Kind::Mariadb) {
                vec!["all".to_owned()]
            } else {
                let text = to_backup.unwrap_or_default();
                let separator = if kind == Kind::Mongodb { '|' } else { ',' };
                text.split(separator).map(|s| s.trim().to_owned()).collect()
            };
        if names.is_empty() {
            return Ok(());
        }
        let enc = self
            .0
            .encrypter
            .as_ref()
            .ok_or("No application encryption key has been specified.")?;
        let credentials = if database.is_service() {
            conn::Credentials {
                secrets: service_secrets,
                variables: Vec::new(),
                locked: false,
            }
        } else {
            conn::credentials(db, enc, &database)
                .await
                .map_err(|e| e.to_string())?
        };
        let mut secrets: Vec<String> = kind
            .encrypted_columns()
            .iter()
            .map(|c| credentials.secret(c))
            .filter(|s| !s.is_empty())
            .collect();
        let storages = self.selected_storages(&backup).await?;
        for row in &storages {
            if let Ok(t) = S3Target::from_row(enc, row) {
                secrets.push(t.key);
                secrets.push(t.secret);
            }
        }
        // The container name is validated first in a service, so it is a safe folder name.
        let directory = if database.is_service() {
            format!(
                "{}-{}",
                slug(database.str("__service_name").unwrap_or_default()),
                database.container()
            )
        } else {
            format!(
                "{}-{}",
                slug(database.str("name").unwrap_or_default()),
                database.uuid
            )
        };
        let backup_dir = format!(
            "/data/coolify/backups/databases/{}-{team_id}/{directory}",
            slug(&team_name)
        );
        let target = server.target(self).await?;
        let run = Run {
            backup: &backup,
            database: &database,
            server: &server,
            target,
            backup_dir,
            secrets,
            dump_all,
            timeout: Duration::from_secs(timeout.max(1) as u64),
        };
        let prefix = new_public_id().chars().take(16).collect::<String>();
        let mut last_success = false;
        for name in names {
            let uuid = format!(
                "{prefix}{}",
                new_public_id().chars().take(8).collect::<String>()
            );
            last_success = match self
                .backup_one(&run, &name, &uuid, &credentials, &storages)
                .await
            {
                Ok(done) => done,
                Err(e) => {
                    // `failed()`: the job ended with an exception.
                    let output = run.redact(&e);
                    self.notify_backup(&run, &name, BackupOutcome::Failed(output))
                        .await;
                    false
                }
            };
        }
        if last_success && let Err(e) = self.remove_old_backups(&backup, &database, &server).await {
            tracing::warn!(backup = backup_id, error = %e, "database backup retention cleanup failed");
        }
        Ok(())
    }

    async fn selected_storages(&self, backup: &Record) -> Result<Vec<Record>, String> {
        let db = &self.0.db;
        let id = parts::id_of(backup);
        let primary = backup.get("s3_storage_id").and_then(Value::as_i64);
        let mut rows = parts::backup_destinations(db, id)
            .await
            .map_err(|e| e.to_string())?;
        if rows.is_empty() {
            if let Some(p) = primary
                && let Some(r) = parts::s3_by_id(db, p).await.map_err(|e| e.to_string())?
            {
                rows.push(r);
            }
            return Ok(rows);
        }
        // `sortBy` keeps the name order among the rest.
        rows.sort_by_key(|r| i32::from(Some(parts::id_of(r)) != primary));
        Ok(rows)
    }

    /// One database of the job: dump, size check, S3 upload, execution row.
    async fn backup_one(
        &self,
        run: &Run<'_>,
        name: &str,
        uuid: &str,
        credentials: &conn::Credentials,
        storages: &[Record],
    ) -> Result<bool, String> {
        let db = &self.0.db;
        let d = run.database;
        let timestamp = chrono::Utc::now().timestamp();
        let mut database_name = name.to_owned();
        let mut mongo_argument = name.to_owned();
        let file = match d.kind {
            Kind::Postgresql => {
                if run.dump_all {
                    format!("/pg-dump-all-{timestamp}-{uuid}.gz")
                } else {
                    format!("/pg-dump-{}-{timestamp}-{uuid}.dmp", filename_part(name))
                }
            }
            Kind::Mongodb => {
                let part = if name == "*" {
                    mongo_argument = "all".to_owned();
                    database_name = "all".to_owned();
                    "all".to_owned()
                } else {
                    database_name = name.split(':').next().unwrap_or(name).to_owned();
                    database_name.clone()
                };
                format!(
                    "/mongo-dump-{}-{timestamp}-{uuid}.tar.gz",
                    filename_part(&part)
                )
            }
            Kind::Mysql => {
                if run.dump_all {
                    format!("/mysql-dump-all-{timestamp}-{uuid}.gz")
                } else {
                    format!("/mysql-dump-{}-{timestamp}-{uuid}.dmp", filename_part(name))
                }
            }
            Kind::Mariadb => {
                if run.dump_all {
                    format!("/mariadb-dump-all-{timestamp}-{uuid}.gz")
                } else {
                    format!(
                        "/mariadb-dump-{}-{timestamp}-{uuid}.dmp",
                        filename_part(name)
                    )
                }
            }
            Kind::Clickhouse => format!("/clickhouse-backup-{timestamp}-{uuid}.zip"),
            Kind::Sqlite => format!("/sqlite-backup-{name}-{timestamp}-{uuid}.gz"),
            _ => return Err("Unsupported database type".into()),
        };
        let location = format!("{}{file}", run.backup_dir);
        let backup_id = parts::id_of(run.backup);
        let execution = parts::create_execution(
            db,
            backup_id,
            uuid,
            Some(&database_name),
            Some(&location),
            None,
            None,
            now(),
        )
        .await
        .map_err(|e| e.to_string())?;

        let dump = self
            .dump(run, &mongo_argument, &location, &file, credentials)
            .await;
        let size: i64 = match &dump {
            Ok(_) => self.remote_size(run, &location).await,
            Err(_) => 0,
        };
        let finished = now();
        let dump_output = match dump {
            Ok(output) if size > 0 => output,
            other => {
                // Local backup failed: the partial file is not tracked, so retention would not remove it.
                self.delete_local_backups(std::slice::from_ref(&location), run.server)
                    .await;
                let (message, code) = match other {
                    Err(e) => (e.0, e.1),
                    Ok(_) => (
                        "Local backup file is empty or was not created".to_owned(),
                        None,
                    ),
                };
                let mut failure = run.redact(&message);
                if let Some(reason) = exit_code_reason(code) {
                    failure = format!("{reason}\n\n{failure}");
                }
                let notify_failure = failure.clone();
                parts::update_execution(
                    db,
                    execution,
                    &[
                        ("status", ColumnValue::Text(Some("failed".to_owned()))),
                        ("message", ColumnValue::Text(Some(failure))),
                        ("size", ColumnValue::Text(Some(size.to_string()))),
                        ("filename", ColumnValue::Text(None)),
                        ("s3_uploaded", ColumnValue::Text(None)),
                        ("finished_at", ColumnValue::Time(finished)),
                    ],
                    finished,
                )
                .await
                .map_err(|e| e.to_string())?;
                self.notify_backup(run, &database_name, BackupOutcome::Failed(notify_failure))
                    .await;
                return Ok(false);
            }
        };

        // Step 2: Upload to S3 if enabled
        let save_s3 = parts::bool_of(run.backup, "save_s3");
        let mut s3_error: Option<String> = None;
        let mut s3_uploaded = false;
        let mut local_deleted = false;
        if save_s3 {
            match self
                .upload_to_destinations(run, storages, execution, uuid, &location)
                .await
            {
                Ok(failures) => {
                    s3_uploaded = failures.is_none();
                    s3_error = failures;
                    if s3_error.is_none() && parts::bool_of(run.backup, "disable_local_backup") {
                        self.delete_local_backups(std::slice::from_ref(&location), run.server)
                            .await;
                        local_deleted = true;
                    }
                }
                Err(e) => s3_error = Some(run.redact(&e)),
            }
        }

        // Step 3: Update the status.
        let mut message = dump_output.map(|o| run.redact(&o));
        if let Some(e) = &s3_error {
            message = Some(match message {
                Some(m) => format!("{m}\n\nWarning: S3 upload failed: {e}"),
                None => format!("Warning: S3 upload failed: {e}"),
            });
        }
        let done = now();
        let uploaded = if save_s3 {
            ColumnValue::Bool(s3_uploaded)
        } else {
            ColumnValue::Text(None)
        };
        parts::update_execution(
            db,
            execution,
            &[
                ("status", ColumnValue::Text(Some("success".to_owned()))),
                ("message", ColumnValue::Text(message)),
                ("size", ColumnValue::Text(Some(size.to_string()))),
                ("s3_uploaded", uploaded),
                ("local_storage_deleted", ColumnValue::Bool(local_deleted)),
                ("finished_at", ColumnValue::Time(done)),
            ],
            done,
        )
        .await
        .map_err(|e| e.to_string())?;
        let outcome = match s3_error {
            Some(e) => BackupOutcome::S3Warning(e),
            None => BackupOutcome::Success,
        };
        self.notify_backup(run, &database_name, outcome).await;
        Ok(true)
    }

    /// `$this->team->notify(new BackupSuccess|BackupSuccessWithS3Warning|BackupFailed(...))`.
    async fn notify_backup(&self, run: &Run<'_>, database_name: &str, outcome: BackupOutcome) {
        use crate::notifications::{Notification, lookup, notify};
        let db = &self.0.db;
        let Some(team_id) = run.backup.get("team_id").and_then(Value::as_i64) else {
            return;
        };
        let d = run.database;
        let backup = lookup::database_backup(
            db,
            d.str("name").unwrap_or_default(),
            &d.uuid,
            d.int("environment_id"),
            Some(database_name),
            parts::str_of(run.backup, "frequency").unwrap_or_default(),
        )
        .await;
        let notification = match outcome {
            BackupOutcome::Success => Notification::BackupSuccess(backup),
            BackupOutcome::Failed(output) => Notification::BackupFailed { backup, output },
            BackupOutcome::S3Warning(s3_error) => Notification::BackupSuccessWithS3Warning {
                backup,
                s3_error,
                s3_uuid: lookup::s3_uuid(
                    db,
                    run.backup.get("s3_storage_id").and_then(Value::as_i64),
                )
                .await,
            },
        };
        notify(db, self.0.encrypter.as_ref(), team_id, notification).await;
    }

    /// `backup_standalone_*`: the output of the commands, or the error text with the exit code.
    async fn dump(
        &self,
        run: &Run<'_>,
        name: &str,
        location: &str,
        file: &str,
        credentials: &conn::Credentials,
    ) -> Result<Option<String>, (String, Option<i32>)> {
        let d = run.database;
        let container = quote(d.container());
        let escaped_location = quote(location);
        let mut commands = run.directory_commands();
        let bad = |e: String| (e, None);
        match d.kind {
            Kind::Postgresql => {
                let user = quote(d.str("postgres_user").unwrap_or_default());
                if run.dump_all {
                    let dump = format!("docker exec {container} pg_dumpall --username {user}");
                    commands.push(run.compressed_dump(&dump, &escaped_location, self));
                } else {
                    validate_shell_safe_path(name, "database name").map_err(bad)?;
                    commands.push(format!(
                        "docker exec {container} pg_dump --format=custom --username {user} {} > {escaped_location}",
                        quote(name)
                    ));
                }
            }
            Kind::Mysql | Kind::Mariadb => {
                let mysql = d.kind == Kind::Mysql;
                let tool = if mysql { "mysqldump" } else { "mariadb-dump" };
                let password = quote(&credentials.secret(if mysql {
                    "mysql_root_password"
                } else {
                    "mariadb_root_password"
                }));
                if run.dump_all {
                    let dump = format!(
                        "docker exec {container} {tool} -u root -p{password} --all-databases --single-transaction --quick --lock-tables=false"
                    );
                    commands.push(run.compressed_dump(&dump, &escaped_location, self));
                } else {
                    validate_shell_safe_path(name, "database name").map_err(bad)?;
                    commands.push(format!(
                        "docker exec {container} {tool} -u root -p{password} {} > {escaped_location}",
                        quote(name)
                    ));
                }
            }
            Kind::Mongodb => {
                // Service databases build the URL from the container's environment.
                let url = if d.is_service() {
                    let (user, password) = (
                        credentials.secret("mongo_root_username"),
                        credentials.secret("mongo_root_password"),
                    );
                    if user.is_empty() || password.is_empty() {
                        return Err((
                            "MongoDB credentials not found. Ensure MONGO_INITDB_ROOT_USERNAME and MONGO_INITDB_ROOT_PASSWORD environment variables are available in the container."
                                .to_owned(),
                            None,
                        ));
                    }
                    quote(&format!(
                        "mongodb://{}:{}@{}:27017",
                        conn::rawurlencode(&user),
                        conn::rawurlencode(&password),
                        d.container()
                    ))
                } else {
                    quote(&conn::internal_url(d, credentials))
                };
                let old_image = d.str("image").unwrap_or_default().starts_with("mongo:4");
                if name == "all" {
                    if old_image {
                        commands.push(format!(
                            "docker exec {container} mongodump --uri={url} --gzip --archive > {escaped_location}"
                        ));
                    } else {
                        commands.push(format!(
                            "docker exec {container} mongodump --authenticationDatabase=admin --uri={url} --gzip --archive > {escaped_location}"
                        ));
                    }
                } else {
                    let (database, excluded): (&str, Vec<&str>) = match name.split_once(':') {
                        Some((a, b)) => (a, b.split(',').collect()),
                        None => (name, Vec::new()),
                    };
                    validate_shell_safe_path(database, "database name").map_err(bad)?;
                    let escaped = quote(database);
                    if excluded.is_empty() {
                        if old_image {
                            commands.push(format!(
                                "docker exec {container} mongodump --uri={url} --gzip --archive > {escaped_location}"
                            ));
                        } else {
                            commands.push(format!(
                                "docker exec {container} mongodump --authenticationDatabase=admin --uri={url} --db {escaped} --gzip --archive > {escaped_location}"
                            ));
                        }
                    } else {
                        let mut collections = Vec::new();
                        for c in excluded {
                            let c = c.trim();
                            validate_shell_safe_path(c, "collection name").map_err(bad)?;
                            collections.push(quote(c));
                        }
                        let list = collections.join(" --excludeCollection ");
                        if old_image {
                            commands.push(format!(
                                "docker exec {container} mongodump --uri={url} --gzip --excludeCollection {list} --archive > {escaped_location}"
                            ));
                        } else {
                            commands.push(format!(
                                "docker exec {container} mongodump --authenticationDatabase=admin --uri={url} --db {escaped} --gzip --excludeCollection {list} --archive > {escaped_location}"
                            ));
                        }
                    }
                }
            }
            Kind::Clickhouse => {
                validate_shell_safe_path(name, "database name").map_err(bad)?;
                let archive = file.trim_start_matches('/');
                let query = format!("BACKUP DATABASE `{name}` TO File('{archive}')");
                let dir = run.backup_dir.trim_end_matches('/');
                commands.push(format!("mkdir -p {}", quote(dir)));
                commands.push(format!(
                    "docker exec {container} clickhouse-client --query {}",
                    quote(&query)
                ));
                commands.push(format!(
                    "docker cp {} {}",
                    quote(&format!("{}:/var/lib/clickhouse/backups/{archive}", d.uuid)),
                    quote(&format!("{dir}/{archive}"))
                ));
            }
            Kind::Sqlite => {
                if !sqlite_file_valid(name) {
                    return Err((format!("Invalid database file name: {name}"), None));
                }
                let script = format!(
                    "f=$(mktemp) && sqlite3 -readonly {} '.timeout 10000' \"VACUUM INTO '$f'\" && cat \"$f\"; s=$?; rm -f \"$f\"; exit $s",
                    quote(&format!("{SQLITE_DATA_DIRECTORY}/{name}"))
                );
                let dump = format!("docker exec {container} sh -c {}", quote(&script));
                commands.push(run.compressed_dump(&dump, &escaped_location, self));
            }
            _ => return Err(("Unsupported database type".to_owned(), None)),
        }
        // writeBackupFileAsRoot
        let redirect = format!("> {escaped_location}");
        let lines: Vec<String> = commands
            .into_iter()
            .map(|c| {
                if c.contains(&redirect) {
                    format!("sh -c {}", quote(&c))
                } else {
                    c
                }
            })
            .collect();
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let result = self
            .remote()
            .run(
                &run.target,
                &refs,
                RunOptions::op("database.backup").timeout(run.timeout),
            )
            .await;
        // backup_standalone_clickhouse: `finally`.
        if d.kind == Kind::Clickhouse {
            let cleanup = format!(
                "docker exec {container} rm -f {}",
                quote(&format!(
                    "/var/lib/clickhouse/backups/{}",
                    file.trim_start_matches('/')
                ))
            );
            self.remote()
                .run_quiet(
                    &run.target,
                    &[cleanup.as_str()],
                    RunOptions::op("database.backup"),
                )
                .await;
        }
        match result {
            Ok(output) => Ok(output
                .map(|o| o.trim().to_owned())
                .filter(|o| !o.is_empty())),
            Err(e) => Err((e.message, e.code)),
        }
    }

    /// `calculate_size`.
    async fn remote_size(&self, run: &Run<'_>, location: &str) -> i64 {
        let command = format!("du -b {} | cut -f1", quote(location));
        self.remote()
            .run_quiet(
                &run.target,
                &[command.as_str()],
                RunOptions::op("database.backup"),
            )
            .await
            .and_then(|o| o.trim().parse().ok())
            .unwrap_or(0)
    }

    /// `uploadToS3Destinations`: the failed destinations and their errors, or `None`.
    async fn upload_to_destinations(
        &self,
        run: &Run<'_>,
        storages: &[Record],
        execution: i64,
        uuid: &str,
        location: &str,
    ) -> Result<Option<String>, String> {
        let db = &self.0.db;
        let enc = self
            .0
            .encrypter
            .as_ref()
            .ok_or("No application encryption key has been specified.")?;
        if storages.is_empty() {
            let previous = run
                .backup
                .get("s3_storage_id")
                .and_then(Value::as_i64)
                .map_or("null".to_owned(), |v| v.to_string());
            parts::update_backup(
                db,
                parts::id_of(run.backup),
                &[
                    ("save_s3", ColumnValue::Bool(false)),
                    ("s3_storage_id", ColumnValue::Text(None)),
                ],
                &[],
                now(),
            )
            .await
            .map_err(|e| e.to_string())?;
            return Err(format!(
                "S3 storage configuration is missing or has been deleted (S3 storage ID: {previous}). S3 backup has been disabled for this schedule."
            ));
        }
        let network = run
            .database
            .destination
            .as_ref()
            .and_then(|x| x.get("network"))
            .and_then(Value::as_str)
            .unwrap_or("coolify")
            .to_owned();
        let mut failures = Vec::new();
        for row in storages {
            let name = parts::str_of(row, "name").unwrap_or_default().to_owned();
            let result = match S3Target::from_row(enc, row) {
                Ok(t) => {
                    self.upload_one(run, row, &t, uuid, location, &network)
                        .await
                }
                Err(e) => Err(e),
            };
            let error = result.err().map(|e| run.redact(&e));
            if let Some(e) = &error {
                failures.push(format!("{name}: {e}"));
            }
            if let Err(e) = parts::create_replica(
                db,
                execution,
                parts::id_of(row),
                error.is_none(),
                error.as_deref(),
                now(),
            )
            .await
            {
                failures.push(format!("{name}: {}", run.redact(&e.to_string())));
            }
        }
        let _ = parts::refresh_s3_summary(db, execution, now()).await;
        Ok(if failures.is_empty() {
            None
        } else {
            Some(failures.join("\n"))
        })
    }

    /// `upload_to_s3`.
    async fn upload_one(
        &self,
        run: &Run<'_>,
        row: &Record,
        s3: &S3Target,
        uuid: &str,
        location: &str,
        network: &str,
    ) -> Result<(), String> {
        self.test_s3_connection(row).await?;
        let helper = format!("backup-of-{uuid}");
        let mount = quote(&format!("{location}:{location}:ro"));
        let resolve = minio_resolve_options(&s3.endpoint)
            .await
            .iter()
            .map(|o| format!("--resolve {}", quote(o)))
            .collect::<Vec<_>>()
            .join(" ");
        let resolve = if resolve.is_empty() {
            String::new()
        } else {
            format!(" {resolve}")
        };
        let destination = quote(&format!("temporary/{}{}/", s3.bucket, run.backup_dir));
        let commands = [
            format!("docker rm -f {helper} >/dev/null 2>&1 || true"),
            format!(
                "docker run -d --network {} --name {helper} --rm -e {} -v {mount} {}",
                quote(network),
                quote(&format!("MC_REGION={}", s3.region)),
                helper_image(self)
            ),
            format!(
                "docker exec {helper} mc alias set{resolve} temporary {} {} {}",
                quote(&s3.endpoint),
                quote(&s3.key),
                quote(&s3.secret)
            ),
            format!(
                "docker exec {helper} mc cp{resolve} {} {destination}",
                quote(location)
            ),
        ];
        let refs: Vec<&str> = commands.iter().map(String::as_str).collect();
        let result = self
            .remote()
            .run(
                &run.target,
                &refs,
                RunOptions::op("database.backup").timeout(run.timeout),
            )
            .await;
        let remove = super::proxy::docker_remove_command(&helper);
        let _ = self
            .remote()
            .run(
                &run.target,
                &[remove.as_str()],
                RunOptions::op("database.backup"),
            )
            .await;
        result.map(|_| ()).map_err(|e| e.message)
    }

    /// `deleteBackupsLocally`.
    pub async fn delete_local_backups(&self, files: &[String], server: &ServerInfo) {
        if files.is_empty() {
            return;
        }
        let Ok(t) = server.target(self).await else {
            return;
        };
        let quoted: Vec<String> = files.iter().map(|f| quote(f)).collect();
        let remove = format!("rm -f {}", quoted.join(" "));
        self.remote()
            .run_quiet(&t, &[remove.as_str()], RunOptions::op("database.backup"))
            .await;
        let mut folders: Vec<String> = Vec::new();
        for f in files {
            let dir = f.rsplit_once('/').map_or("", |(d, _)| d).to_owned();
            if !folders.contains(&dir) {
                folders.push(dir);
            }
        }
        for folder in folders {
            if self.remove_if_empty(&t, &folder).await {
                let parent = folder.rsplit_once('/').map_or("/", |(d, _)| d).to_owned();
                self.remove_if_empty(&t, &parent).await;
            }
        }
    }

    /// `deleteEmptyBackupFolder`.
    async fn remove_if_empty(&self, t: &Target, folder: &str) -> bool {
        let escaped = quote(folder);
        let check = format!(
            "[ -d {escaped} ] && [ -z \"$(ls -A {escaped})\" ] && echo 'empty' || echo 'not empty'"
        );
        let out = self
            .remote()
            .run_quiet(t, &[check.as_str()], RunOptions::op("database.backup"))
            .await
            .unwrap_or_default();
        if out.trim() == "empty" {
            let rm = format!("rmdir {escaped}");
            self.remote()
                .run_quiet(t, &[rm.as_str()], RunOptions::op("database.backup"))
                .await;
            return true;
        }
        false
    }

    /// `deleteS3Copies`: the backup file from every destination that still has a copy.
    pub async fn delete_s3_copies(&self, execution: &Record) -> Result<(), String> {
        let db = &self.0.db;
        let id = parts::id_of(execution);
        let enc = self
            .0
            .encrypter
            .as_ref()
            .ok_or("No application encryption key has been specified.")?;
        let replicas = parts::live_replicas(db, id)
            .await
            .map_err(|e| e.to_string())?;
        let filename = parts::str_of(execution, "filename").filter(|f| !f.trim().is_empty());
        let mut result = Ok(());
        for (replica, storage) in replicas {
            let row = match storage {
                Some(s) => parts::s3_by_id(db, s).await.map_err(|e| e.to_string())?,
                None => None,
            };
            let Some(row) = row else {
                result =
                    Err("The S3 storage used by an existing backup is unavailable.".to_owned());
                break;
            };
            if let Some(file) = filename {
                let target = S3Target::from_row(enc, &row)?;
                if let Err(e) = target.delete_objects(&[file.to_owned()]).await {
                    result = Err(e);
                    break;
                }
            }
            parts::mark_replica_deleted(db, replica, now())
                .await
                .map_err(|e| e.to_string())?;
        }
        let _ = parts::refresh_s3_summary(db, id, now()).await;
        result
    }

    /// `removeOldBackups`.
    async fn remove_old_backups(
        &self,
        backup: &Record,
        database: &DatabaseRow,
        server: &ServerInfo,
    ) -> Result<(), String> {
        let db = &self.0.db;
        let id = parts::id_of(backup);
        let only_uploaded = parts::bool_of(backup, "disable_local_backup");
        let successful = parts::successful_local_executions(db, id, only_uploaded)
            .await
            .map_err(|e| e.to_string())?;
        let to_delete = outside_retention(
            &successful,
            backup
                .get("database_backup_retention_amount_locally")
                .and_then(Value::as_i64),
            backup
                .get("database_backup_retention_days_locally")
                .and_then(Value::as_i64),
            number(backup.get("database_backup_retention_max_storage_locally")),
        );
        let files: Vec<String> = to_delete
            .iter()
            .filter_map(|e| {
                parts::str_of(e, "filename")
                    .filter(|f| !f.is_empty())
                    .map(str::to_owned)
            })
            .collect();
        let _ = database;
        if !files.is_empty() {
            self.delete_local_backups(&files, server).await;
            let ids: Vec<i64> = to_delete.iter().map(parts::id_of).collect();
            parts::mark_local_deleted(db, id, &ids, now())
                .await
                .map_err(|e| e.to_string())?;
        }
        if parts::bool_of(backup, "save_s3") {
            self.delete_old_from_s3(backup).await?;
        }
        parts::delete_exhausted_executions(db, id)
            .await
            .map_err(|e| e.to_string())
    }

    /// `deleteOldBackupsFromS3`: the S3 retention per destination.
    async fn delete_old_from_s3(&self, backup: &Record) -> Result<(), String> {
        let db = &self.0.db;
        let id = parts::id_of(backup);
        let enc = self
            .0
            .encrypter
            .as_ref()
            .ok_or("No application encryption key has been specified.")?;
        let live = parts::schedule_live_replicas(db, id)
            .await
            .map_err(|e| e.to_string())?;
        let mut storages: Vec<i64> = Vec::new();
        for (_, _, s) in &live {
            if let Some(s) = s
                && !storages.contains(s)
            {
                storages.push(*s);
            }
        }
        let mut deleted: Vec<(i64, i64)> = Vec::new();
        for (replica, execution, s) in &live {
            let gone = match s {
                Some(s) => parts::s3_by_id(db, *s)
                    .await
                    .map_err(|e| e.to_string())?
                    .is_none(),
                None => true,
            };
            if gone {
                deleted.push((*replica, *execution));
            }
        }
        for storage in storages {
            let Some(row) = parts::s3_by_id(db, storage)
                .await
                .map_err(|e| e.to_string())?
            else {
                continue;
            };
            let in_storage: Vec<&(i64, i64, Option<i64>)> = live
                .iter()
                .filter(|(_, _, s)| *s == Some(storage))
                .collect();
            let ids: Vec<i64> = in_storage.iter().map(|(_, e, _)| *e).collect();
            let executions = parts::executions_by_ids(db, id, &ids)
                .await
                .map_err(|e| e.to_string())?;
            let to_delete = outside_retention(
                &executions,
                backup
                    .get("database_backup_retention_amount_s3")
                    .and_then(Value::as_i64),
                backup
                    .get("database_backup_retention_days_s3")
                    .and_then(Value::as_i64),
                number(backup.get("database_backup_retention_max_storage_s3")),
            );
            let files: Vec<String> = to_delete
                .iter()
                .filter_map(|e| {
                    parts::str_of(e, "filename")
                        .filter(|f| !f.is_empty())
                        .map(str::to_owned)
                })
                .collect();
            if files.is_empty() {
                continue;
            }
            let target = S3Target::from_row(enc, &row)?;
            match target.delete_objects(&files).await {
                Ok(()) => {
                    let gone: Vec<i64> = to_delete.iter().map(parts::id_of).collect();
                    for (replica, execution, _) in &in_storage {
                        if gone.contains(execution) {
                            deleted.push((*replica, *execution));
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(backup = id, error = %e, "old S3 backups could not be deleted")
                }
            }
        }
        for (replica, _) in &deleted {
            let _ = parts::mark_replica_deleted(db, *replica, now()).await;
        }
        let mut seen: Vec<i64> = Vec::new();
        for (_, execution) in deleted {
            if !seen.contains(&execution) {
                seen.push(execution);
                let _ = parts::refresh_s3_summary(db, execution, now()).await;
            }
        }
        Ok(())
    }

    /// `databaseOperationInProgress`: the reservation or a live start/import activity.
    pub(super) async fn database_operation_in_progress(&self, d: &DatabaseRow) -> bool {
        let reserved = self
            .0
            .db_ops
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .contains(&d.uuid);
        if reserved
            || dbs::live_operation(&self.0.db, &d.uuid)
                .await
                .unwrap_or(false)
        {
            return true;
        }
        // A start of the whole service counts too.
        match d.str("__service_uuid") {
            Some(service) => coolify_db::services::has_running_start(&self.0.db, service)
                .await
                .unwrap_or(false),
            None => false,
        }
    }

    /// The service branch of `DatabaseBackupJob::handle`: the user, passwords and database names come from the
    /// container's environment. `Ok(None)` when the run ended here.
    async fn service_database_environment(
        &self,
        d: &mut DatabaseRow,
        backup: &Record,
        target: &Target,
        secrets: &mut std::collections::HashMap<&'static str, String>,
    ) -> Result<Option<Option<String>>, String> {
        let container = d.container().to_owned();
        if !valid_container_name(&container) {
            return Err("Invalid database container name.".to_owned());
        }
        let escaped = quote(&container);
        let command = match d.kind {
            Kind::Postgresql => {
                format!("docker exec {escaped} env | grep -E '^(POSTGRES|PGUSER=)'")
            }
            Kind::Mysql => format!("docker exec {escaped} env | grep MYSQL_"),
            Kind::Mariadb => format!("docker exec {escaped} env"),
            Kind::Mongodb => format!("docker exec {escaped} env | grep MONGO_INITDB_"),
            _ => {
                tracing::warn!(
                    skip_reason = "unsupported_database_type",
                    "database backup job ended without a backup"
                );
                return Ok(None);
            }
        };
        let output = self
            .remote()
            .run(
                target,
                &[command.as_str()],
                RunOptions::op("database.backup"),
            )
            .await;
        let output = match output {
            Ok(o) => o.unwrap_or_default(),
            // Mongo continues without the variables; the dump then reports them as missing.
            Err(_) if d.kind == Kind::Mongodb => String::new(),
            Err(e) => return Err(e.message),
        };
        let first = |name: &str| -> Option<String> {
            output
                .lines()
                .find_map(|l| l.strip_prefix(name)?.strip_prefix('=').map(str::to_owned))
        };
        let configured = || -> Option<String> {
            if parts::bool_of(backup, "dump_all") {
                return Some("all".to_owned());
            }
            parts::str_of(backup, "databases_to_backup")
                .filter(|s| !s.trim().is_empty())
                .map(str::to_owned)
        };
        let names = match d.kind {
            Kind::Postgresql => {
                // `PostgresUser::fromEnvLines`
                let user = ["POSTGRES_USER", "PGUSER", "POSTGRESQL_USERNAME"]
                    .iter()
                    .find_map(|k| first(k).filter(|v| !v.is_empty()))
                    .unwrap_or_else(|| "postgres".to_owned());
                d.raw.insert("postgres_user".into(), user.clone().into());
                if let Some(p) = first("POSTGRES_PASSWORD") {
                    secrets.insert("postgres_password", p);
                }
                Some(first("POSTGRES_DB").unwrap_or(user))
            }
            Kind::Mysql | Kind::Mariadb => {
                let maria = d.kind == Kind::Mariadb;
                let password = if maria {
                    first("MARIADB_ROOT_PASSWORD").or_else(|| first("MYSQL_ROOT_PASSWORD"))
                } else {
                    first("MYSQL_ROOT_PASSWORD")
                };
                if let Some(p) = password {
                    secrets.insert(
                        if maria {
                            "mariadb_root_password"
                        } else {
                            "mysql_root_password"
                        },
                        p,
                    );
                }
                let database = if maria {
                    first("MARIADB_DATABASE").or_else(|| first("MYSQL_DATABASE"))
                } else {
                    first("MYSQL_DATABASE")
                };
                match database.or_else(configured) {
                    Some(n) => Some(n),
                    None => {
                        let env_names = if maria {
                            "MARIADB_DATABASE or MYSQL_DATABASE"
                        } else {
                            "MYSQL_DATABASE"
                        };
                        self.record_undetermined_databases(backup, d, env_names)
                            .await?;
                        return Ok(None);
                    }
                }
            }
            _ => {
                if let Some(p) = first("MONGO_INITDB_ROOT_PASSWORD") {
                    secrets.insert("mongo_root_password", p);
                }
                if let Some(u) = first("MONGO_INITDB_ROOT_USERNAME") {
                    secrets.insert("mongo_root_username", u);
                }
                Some("*".to_owned())
            }
        };
        Ok(Some(names))
    }

    /// `recordUndeterminedDatabases`: shows the run in the execution list and notifies the team.
    async fn record_undetermined_databases(
        &self,
        backup: &Record,
        d: &DatabaseRow,
        env_names: &str,
    ) -> Result<(), String> {
        use crate::notifications::{Notification, lookup, notify};
        let message = format!(
            "No database to back up: the container does not set {env_names}. Select \"All databases\" or enter the databases to back up in the backup settings."
        );
        let names = if parts::bool_of(backup, "dump_all") {
            Some("all".to_owned())
        } else {
            parts::str_of(backup, "databases_to_backup").map(str::to_owned)
        };
        parts::create_execution(
            &self.0.db,
            parts::id_of(backup),
            &new_public_id(),
            names
                .filter(|n| !n.trim().is_empty())
                .map(|n| n.chars().take(250).collect::<String>())
                .as_deref(),
            None,
            Some("failed"),
            Some(&message),
            now(),
        )
        .await
        .map_err(|e| e.to_string())?;
        let Some(team_id) = backup.get("team_id").and_then(Value::as_i64) else {
            return Ok(());
        };
        let details = lookup::database_backup(
            &self.0.db,
            d.str("name").unwrap_or_default(),
            &d.uuid,
            d.int("environment_id"),
            Some("unknown"),
            parts::str_of(backup, "frequency").unwrap_or_default(),
        )
        .await;
        notify(
            &self.0.db,
            self.0.encrypter.as_ref(),
            team_id,
            Notification::BackupFailed {
                backup: details,
                output: message,
            },
        )
        .await;
        Ok(())
    }
}

/// `ValidationPatterns::isValidContainerName`.
fn valid_container_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

fn number(v: Option<&Value>) -> f64 {
    match v {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        Some(Value::String(s)) => s.parse().unwrap_or(0.0),
        _ => 0.0,
    }
}

fn created_at(e: &Record) -> Option<NaiveDateTime> {
    parts::str_of(e, "created_at")
        .and_then(|s| NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S").ok())
}

/// `backupExecutionsOutsideRetention`; `executions` are sorted newest first.
pub fn outside_retention(
    executions: &[Record],
    amount: Option<i64>,
    days: Option<i64>,
    max_storage_gb: f64,
) -> Vec<Record> {
    let mut out: Vec<Record> = Vec::new();
    let mut add = |e: &Record| {
        if !out.iter().any(|o| parts::id_of(o) == parts::id_of(e)) {
            out.push(e.clone());
        }
    };
    if executions.is_empty() {
        return Vec::new();
    }
    if let Some(n) = amount.filter(|n| *n > 0) {
        for e in executions.iter().skip(n as usize) {
            add(e);
        }
    }
    if let Some(d) = days.filter(|d| *d > 0)
        && let Some(first) = created_at(&executions[0])
    {
        let oldest = first - chrono::Duration::days(d);
        for e in executions {
            if created_at(e).is_some_and(|c| c < oldest) {
                add(e);
            }
        }
    }
    if max_storage_gb > 0.0 {
        let max_bytes = max_storage_gb * 1024f64.powi(3);
        let mut total = 0f64;
        for e in executions.iter().skip(1) {
            let size = e
                .get("size")
                .and_then(|v| {
                    v.as_str()
                        .map(str::to_owned)
                        .or_else(|| v.as_i64().map(|i| i.to_string()))
                })
                .and_then(|s| s.parse::<f64>().ok())
                .unwrap_or(0.0);
            total += size;
            if total > max_bytes {
                let limit = created_at(e);
                let matching: Vec<&Record> = executions
                    .iter()
                    .filter(|b| created_at(b) <= limit)
                    .skip(1)
                    .collect();
                for b in matching {
                    add(b);
                }
                break;
            }
        }
    }
    out
}

/// `StandaloneSqlite::DATABASES_PATTERN` for one file name.
pub fn sqlite_file_valid(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && chars.all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exec(id: i64, created: &str, size: i64) -> Record {
        let mut r = Record::new();
        r.insert("id".into(), id.into());
        r.insert("created_at".into(), created.into());
        r.insert("size".into(), size.to_string().into());
        r
    }

    #[test]
    fn retention_by_amount_and_days() {
        let list = vec![
            exec(3, "2026-10-30T00:00:00", 10),
            exec(2, "2026-10-29T00:00:00", 10),
            exec(1, "2026-10-01T00:00:00", 10),
        ];
        let ids = |v: Vec<Record>| v.iter().map(parts::id_of).collect::<Vec<_>>();
        assert_eq!(ids(outside_retention(&list, Some(2), None, 0.0)), vec![1]);
        assert_eq!(ids(outside_retention(&list, None, Some(7), 0.0)), vec![1]);
        assert!(outside_retention(&list, None, None, 0.0).is_empty());
    }

    #[test]
    fn pipe_keeps_status() {
        assert_eq!(
            pipe_to_file("a", "b", "'f'"),
            "status=$( { { a; echo $? >&3; } | b > 'f'; } 3>&1 ) && [ \"$status\" -eq 0 ]"
        );
    }
}
