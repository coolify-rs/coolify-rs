//! The `backups`, `volume-backups` and `docker-cleanups` types of `ScheduledJobManager::TYPES`, with
//! their parts of `ScheduledJobDeliveryService::reportMissedOccurrence()`.

use std::time::Duration;

use chrono::{DateTime, Utc};
use coolify_db::database_parts as parts;
use coolify_db::instance_jobs as queries;
use coolify_db::records::Record;
use coolify_db::servers::{self, ServerRow};
use coolify_db::volume_backups as vb;
use serde_json::{Value, json};

use crate::Jobs;
use crate::notifications::{self, Notification, lookup, notify};
use crate::scheduler::{
    BoxFuture, Due, Occurrence, Overlap, ScheduleSource, allowed, server_timezone,
};
use crate::server::ServerInfo;
use crate::server_ops::cleanup;

/// `VOLUME_BACKUP_RECOVERY_INTERVAL_MINUTES`.
const VOLUME_BACKUP_RECOVERY_INTERVAL_MINUTES: u64 = 5;
/// `VOLUME_BACKUP_RECOVERY_MAX_DISPATCHES`.
const VOLUME_BACKUP_RECOVERY_MAX_DISPATCHES: usize = 100;
/// `MISSED_OCCURRENCE_NOTIFICATION`.
const MISSED_OCCURRENCE_NOTIFICATION: &str = "scheduled-occurrence-missed";
/// `MISSED_OCCURRENCE_NOTIFICATION_INTERVAL_MINUTES`.
const MISSED_OCCURRENCE_NOTIFICATION_INTERVAL_MINUTES: i64 = 60;
/// `DockerCleanupJob::INTERRUPTED_MESSAGE`.
const CLEANUP_INTERRUPTED: &str =
    "Docker cleanup was interrupted: its worker stopped before the cleanup finished.";
/// `DockerCleanupJob::INTERRUPTED_EXECUTIONS_CHUNK_SIZE`.
const INTERRUPTED_EXECUTIONS_CHUNK_SIZE: i64 = 100;

/// `notifyMissedOccurrence()`: at most one missed-run notification per schedule each hour.
pub(crate) async fn notify_missed_occurrence(
    jobs: &Jobs,
    morph: &str,
    schedule_id: i64,
    team_id: i64,
    notification: Notification,
) {
    let db = &jobs.0.db;
    let before =
        Utc::now() - chrono::Duration::minutes(MISSED_OCCURRENCE_NOTIFICATION_INTERVAL_MINUTES);
    match notifications::claim_throttle(
        db,
        morph,
        schedule_id,
        MISSED_OCCURRENCE_NOTIFICATION,
        Some(before),
    )
    .await
    {
        Ok(true) => notify(db, jobs.0.encrypter.as_ref(), team_id, notification).await,
        Ok(false) => {}
        Err(e) => {
            tracing::error!(target: "scheduled-errors", error = %e, "Failed to report missed scheduled occurrence");
        }
    }
}

fn int(r: &Record, k: &str) -> Option<i64> {
    r.get(k).and_then(Value::as_i64)
}

fn text<'a>(r: &'a Record, k: &str) -> &'a str {
    r.get(k).and_then(Value::as_str).unwrap_or_default()
}

/// The `backups` type.
#[derive(Debug, Clone, Copy)]
pub struct DatabaseBackupSource;

const BACKUP_MORPH: &str = "App\\Models\\ScheduledDatabaseBackup";

impl ScheduleSource for DatabaseBackupSource {
    fn name(&self) -> &'static str {
        "backups"
    }

    fn job_types(&self) -> &'static [&'static str] {
        &["database-backup"]
    }

    /// `scheduledBackupQuery()`, `dueScheduledBackups()`, `getBackupSkipReason()`: a schedule whose
    /// database or server is gone is deleted.
    fn due<'a>(
        &'a self,
        jobs: &'a Jobs,
        _now: DateTime<Utc>,
    ) -> BoxFuture<'a, Result<Vec<Due>, String>> {
        Box::pin(async move {
            let db = &jobs.0.db;
            let rows = parts::enabled_backups(db)
                .await
                .map_err(|e| e.to_string())?;
            let mut due = Vec::with_capacity(rows.len());
            for (id, _uuid, frequency) in rows {
                let schedule_key = format!("scheduled-backup:{id}");
                if !allowed(&schedule_key) {
                    continue;
                }
                let Ok(Some(backup)) = parts::backup_by_id(db, id).await else {
                    continue;
                };
                let Some(database) = jobs.backup_database(&backup).await else {
                    let _ = parts::delete_backup(db, id).await;
                    tracing::info!(target: "scheduled", backup_id = id, skip_reason = "database_deleted", "Backup skipped");
                    continue;
                };
                let Ok(server) = jobs.server_for(&database).await else {
                    let _ = parts::delete_backup(db, id).await;
                    tracing::info!(target: "scheduled", backup_id = id, skip_reason = "server_deleted", "Backup skipped");
                    continue;
                };
                due.push(Due {
                    schedule_key,
                    frequency,
                    timezone: server_timezone(&server.settings),
                    job_type: "database-backup",
                    resource_id: id,
                    payload: json!([]),
                    skip: (!server.is_functional()).then(|| "server_not_functional".to_owned()),
                });
            }
            Ok(due)
        })
    }

    /// `DatabaseBackupJob::middleware()`.
    fn overlap<'a>(&'a self, jobs: &'a Jobs, resource_id: i64) -> BoxFuture<'a, Option<Overlap>> {
        Box::pin(async move {
            let backup = parts::backup_by_id(&jobs.0.db, resource_id)
                .await
                .ok()
                .flatten()?;
            let timeout = int(&backup, "timeout")
                .unwrap_or(3600)
                .max(0)
                .unsigned_abs();
            Some(Overlap {
                key: format!("database-backup-{resource_id}"),
                ttl: Duration::from_secs(timeout + 300),
            })
        })
    }

    fn run<'a>(
        &'a self,
        jobs: &'a Jobs,
        occurrence: &'a Occurrence,
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            jobs.run_scheduled_database_backup(occurrence.resource_id)
                .await
        })
    }

    fn runs_late(&self) -> bool {
        true
    }

    /// `reportMissedOccurrence()` for `database-backup`.
    fn report_missed<'a>(
        &'a self,
        jobs: &'a Jobs,
        occurrence: &'a Occurrence,
        message: &'a str,
    ) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            let db = &jobs.0.db;
            let Ok(Some(backup)) = parts::backup_by_id(db, occurrence.resource_id).await else {
                return;
            };
            let Some(database) = jobs.backup_database(&backup).await else {
                return;
            };
            // `$backup->dump_all ? 'all' : $backup->databases_to_backup`
            let name = if backup.get("dump_all").and_then(Value::as_bool) == Some(true) {
                Some("all".to_owned())
            } else {
                backup
                    .get("databases_to_backup")
                    .and_then(Value::as_str)
                    .filter(|s| !s.trim().is_empty())
                    .map(str::to_owned)
            };
            let limited = name.as_deref().map(|n| {
                // `str($databaseName)->limit(250)`
                if n.chars().count() > 250 {
                    format!("{}...", n.chars().take(250).collect::<String>())
                } else {
                    n.to_owned()
                }
            });
            if let Err(e) = queries::record_missed_database_backup(
                db,
                occurrence.resource_id,
                limited.as_deref(),
                message,
                Jobs::now(),
            )
            .await
            {
                tracing::error!(target: "scheduled-errors", occurrence_uuid = %occurrence.uuid, error = %e, "Failed to report missed scheduled occurrence");
                return;
            }
            let Some(team_id) = int(&backup, "team_id") else {
                return;
            };
            let details = lookup::database_backup(
                db,
                database.str("name").unwrap_or_default(),
                &database.uuid,
                database.int("environment_id"),
                Some(name.as_deref().unwrap_or("unknown")),
                text(&backup, "frequency"),
            )
            .await;
            notify_missed_occurrence(
                jobs,
                BACKUP_MORPH,
                occurrence.resource_id,
                team_id,
                Notification::BackupFailed {
                    backup: details,
                    output: message.to_owned(),
                },
            )
            .await;
        })
    }
}

/// The `volume-backups` type.
#[derive(Debug, Clone, Copy)]
pub struct VolumeBackupSource;

const VOLUME_MORPH: &str = "App\\Models\\ScheduledVolumeBackup";

impl Jobs {
    /// `$backup->server()`.
    pub(crate) async fn volume_backup_server(&self, backup: &Record) -> Option<ServerInfo> {
        let db = &self.0.db;
        let target = vb::target_of(db, backup).await.ok().flatten()?;
        let morph = target.row.get("resource_type").and_then(Value::as_str)?;
        let resource = target.row.get("resource_id").and_then(Value::as_i64)?;
        let server = vb::resource_server(db, morph, resource)
            .await
            .ok()
            .flatten()?;
        let row = servers::find_by_id(db, server).await.ok().flatten()?;
        Some(ServerInfo::from_row(&row))
    }

    /// `recoverStoppedVolumeBackupContainers()`.
    async fn recover_stopped_volume_backup_containers(&self) -> Result<(), String> {
        let db = &self.0.db;
        let executions = queries::volume_executions_needing_recovery(db)
            .await
            .map_err(|e| e.to_string())?;
        let mut dispatched = 0;
        for (id, backup_id, stop_recovery_pending) in executions {
            if dispatched >= VOLUME_BACKUP_RECOVERY_MAX_DISPATCHES {
                break;
            }
            if !allowed(&format!("volume-backup-recovery:{id}")) {
                continue;
            }
            if stop_recovery_pending {
                let server = match vb::schedule_by_id(db, backup_id).await.ok().flatten() {
                    Some(backup) => self.volume_backup_server(&backup).await,
                    None => None,
                };
                // `?->isFunctional() === false`: an unknown server still gets its attempt.
                if server.is_some_and(|s| !s.is_functional()) {
                    continue;
                }
            }
            // `Cache::add(VolumeBackupRecoveryJob::dispatchCacheKey($execution->id), ...)`
            if crate::server_ops::MARKS.add(
                format!("volume-backup-recovery-dispatch:{id}"),
                Duration::from_secs(VOLUME_BACKUP_RECOVERY_INTERVAL_MINUTES * 60),
            ) {
                dispatched += 1;
                let jobs = self.clone();
                tokio::spawn(async move {
                    if let Err(e) = jobs.retry_volume_recovery(id).await {
                        tracing::warn!(target: "scheduled", execution_id = id, error = %e, "Volume backup recovery failed");
                    }
                });
            }
        }
        Ok(())
    }
}

impl ScheduleSource for VolumeBackupSource {
    fn name(&self) -> &'static str {
        "volume-backups"
    }

    fn job_types(&self) -> &'static [&'static str] {
        &["volume-backup"]
    }

    /// `recoverStoppedVolumeBackupContainers()`, `processScheduledVolumeBackup()`.
    fn due<'a>(
        &'a self,
        jobs: &'a Jobs,
        _now: DateTime<Utc>,
    ) -> BoxFuture<'a, Result<Vec<Due>, String>> {
        Box::pin(async move {
            if let Err(e) = jobs.recover_stopped_volume_backup_containers().await {
                tracing::error!(target: "scheduled-errors", error = %e, "Failed to process scheduled volume backups");
            }
            let db = &jobs.0.db;
            let rows = queries::enabled_volume_backups(db)
                .await
                .map_err(|e| e.to_string())?;
            let mut due = Vec::with_capacity(rows.len());
            for backup in rows {
                let Some(id) = int(&backup, "id") else {
                    continue;
                };
                let schedule_key = format!("scheduled-volume-backup:{id}");
                if !allowed(&schedule_key) {
                    continue;
                }
                let log = |reason: &str| {
                    tracing::info!(target: "scheduled", backup_id = id, skip_reason = reason, "Volume backup skipped");
                };
                if queries::volume_backup_has_stop_recovery_pending(db, id)
                    .await
                    .unwrap_or(false)
                {
                    log("stopped_container_recovery_pending");
                    continue;
                }
                let server = jobs.volume_backup_server(&backup).await;
                if vb::target_of(db, &backup).await.ok().flatten().is_none() {
                    // `DeleteScheduledVolumeBackup::run($backup, $server)`.
                    if let Err(e) = jobs
                        .delete_volume_schedule(&backup, server.as_ref(), true, true)
                        .await
                    {
                        tracing::error!(target: "scheduled-errors", backup_id = id, error = %e, "Error processing volume backup");
                    }
                    log("resource_deleted");
                    continue;
                }
                let Some(server) = server else {
                    log("server_missing");
                    continue;
                };
                due.push(Due {
                    schedule_key,
                    frequency: text(&backup, "frequency").to_owned(),
                    timezone: server_timezone(&server.settings),
                    job_type: "volume-backup",
                    resource_id: id,
                    payload: json!([]),
                    skip: (!server.is_functional()).then(|| "server_not_functional".to_owned()),
                });
            }
            Ok(due)
        })
    }

    /// `VolumeBackupJob::middleware()`.
    fn overlap<'a>(&'a self, jobs: &'a Jobs, resource_id: i64) -> BoxFuture<'a, Option<Overlap>> {
        Box::pin(async move {
            let backup = vb::schedule_by_id(&jobs.0.db, resource_id)
                .await
                .ok()
                .flatten()?;
            let timeout = int(&backup, "timeout")
                .unwrap_or(crate::databases::volume_backup::DEFAULT_TIMEOUT)
                .max(0)
                .unsigned_abs();
            Some(Overlap {
                key: format!("volume-backup-{resource_id}"),
                ttl: Duration::from_secs(timeout + 60),
            })
        })
    }

    fn run<'a>(
        &'a self,
        jobs: &'a Jobs,
        occurrence: &'a Occurrence,
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move { jobs.run_volume_backup(occurrence.resource_id).await })
    }

    fn runs_late(&self) -> bool {
        true
    }

    /// `reportMissedOccurrence()` for `volume-backup`.
    fn report_missed<'a>(
        &'a self,
        jobs: &'a Jobs,
        occurrence: &'a Occurrence,
        message: &'a str,
    ) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            let db = &jobs.0.db;
            let Ok(Some(backup)) = vb::schedule_by_id(db, occurrence.resource_id).await else {
                return;
            };
            if let Err(e) = queries::record_missed_volume_backup(
                db,
                occurrence.resource_id,
                message,
                Jobs::now(),
            )
            .await
            {
                tracing::error!(target: "scheduled-errors", occurrence_uuid = %occurrence.uuid, error = %e, "Failed to report missed scheduled occurrence");
                return;
            }
            let Some(team_id) = int(&backup, "team_id") else {
                return;
            };
            let Ok(Some(target)) = vb::target_of(db, &backup).await else {
                return;
            };
            let details = jobs.volume_backup_details(&backup, &target).await;
            notify_missed_occurrence(
                jobs,
                VOLUME_MORPH,
                occurrence.resource_id,
                team_id,
                Notification::VolumeBackupFailed {
                    backup: details,
                    output: message.to_owned(),
                },
            )
            .await;
        })
    }
}

/// The `docker-cleanups` type.
#[derive(Debug, Clone, Copy)]
pub struct DockerCleanupSource;

const SERVER_MORPH: &str = "App\\Models\\Server";

fn setting_bool(row: &ServerRow, key: &str) -> bool {
    row.settings
        .as_ref()
        .and_then(|s| s.get(key))
        .and_then(Value::as_bool)
        == Some(true)
}

impl Jobs {
    /// `DockerCleanupJob::failInterruptedExecutions()`, `chunkById` pages of
    /// `INTERRUPTED_EXECUTIONS_CHUNK_SIZE`.
    async fn fail_interrupted_cleanups(&self) -> Result<(), String> {
        let db = &self.0.db;
        let lock = i64::try_from(cleanup::OVERLAP_LOCK_SECONDS).unwrap_or(i64::MAX);
        let before = Jobs::now() - chrono::Duration::seconds(lock);
        let mut after_id = 0;
        loop {
            let chunk = queries::running_cleanups_before(
                db,
                before,
                after_id,
                INTERRUPTED_EXECUTIONS_CHUNK_SIZE,
            )
            .await
            .map_err(|e| e.to_string())?;
            let Some(&(last, _)) = chunk.last() else {
                return Ok(());
            };
            let full = chunk.len() == INTERRUPTED_EXECUTIONS_CHUNK_SIZE as usize;
            after_id = last;
            self.fail_interrupted_chunk(chunk).await?;
            if !full {
                return Ok(());
            }
        }
    }

    async fn fail_interrupted_chunk(&self, executions: Vec<(i64, i64)>) -> Result<(), String> {
        let db = &self.0.db;
        for (id, server_id) in executions {
            if !allowed(&format!("docker-cleanup:{server_id}")) {
                continue;
            }
            let updated = queries::fail_running_cleanup(db, id, CLEANUP_INTERRUPTED, Jobs::now())
                .await
                .map_err(|e| e.to_string())?;
            if !updated {
                continue;
            }
            if let Some((team_id, server)) = lookup::server(db, server_id).await {
                notify(
                    db,
                    self.0.encrypter.as_ref(),
                    team_id,
                    Notification::DockerCleanupFailed {
                        server,
                        message: format!(
                            "Docker cleanup job failed with the following error: {CLEANUP_INTERRUPTED}"
                        ),
                    },
                )
                .await;
            }
        }
        Ok(())
    }
}

impl ScheduleSource for DockerCleanupSource {
    fn name(&self) -> &'static str {
        "docker-cleanups"
    }

    fn job_types(&self) -> &'static [&'static str] {
        &["docker-cleanup"]
    }

    /// `failInterruptedExecutions()`, `getServersForCleanupQuery()`, `processDockerCleanup()`.
    fn due<'a>(
        &'a self,
        jobs: &'a Jobs,
        _now: DateTime<Utc>,
    ) -> BoxFuture<'a, Result<Vec<Due>, String>> {
        Box::pin(async move {
            if let Err(e) = jobs.fail_interrupted_cleanups().await {
                tracing::error!(target: "scheduled-errors", error = %e, "Failed to process docker cleanups");
            }
            let rows = servers::all_without_patch_check_result(&jobs.0.db)
                .await
                .map_err(|e| e.to_string())?;
            let mut due = Vec::with_capacity(rows.len());
            for row in rows {
                let schedule_key = format!("docker-cleanup:{}", row.id);
                if !allowed(&schedule_key) {
                    continue;
                }
                let info = ServerInfo::from_row(&row);
                if info.has_placeholder_ip() {
                    continue;
                }
                let frequency = row
                    .settings
                    .as_ref()
                    .and_then(|s| s.get("docker_cleanup_frequency"))
                    .and_then(Value::as_str)
                    .unwrap_or("0 * * * *")
                    .to_owned();
                due.push(Due {
                    schedule_key,
                    frequency,
                    timezone: server_timezone(&info.settings),
                    job_type: "docker-cleanup",
                    resource_id: row.id,
                    payload: json!({
                        "delete_unused_volumes": setting_bool(&row, "delete_unused_volumes"),
                        "delete_unused_networks": setting_bool(&row, "delete_unused_networks"),
                    }),
                    skip: (!info.is_functional()).then(|| "server_not_functional".to_owned()),
                });
            }
            Ok(due)
        })
    }

    /// `DockerCleanupJob::middleware()`.
    fn overlap<'a>(&'a self, jobs: &'a Jobs, resource_id: i64) -> BoxFuture<'a, Option<Overlap>> {
        Box::pin(async move {
            let row = servers::find_by_id(&jobs.0.db, resource_id)
                .await
                .ok()
                .flatten()?;
            Some(Overlap {
                key: format!("docker-cleanup-{}", row.uuid),
                ttl: Duration::from_secs(cleanup::OVERLAP_LOCK_SECONDS),
            })
        })
    }

    fn run<'a>(
        &'a self,
        jobs: &'a Jobs,
        occurrence: &'a Occurrence,
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            let flag = |k: &str| occurrence.payload.get(k).and_then(Value::as_bool) == Some(true);
            cleanup::docker_cleanup_job(
                jobs,
                occurrence.resource_id,
                false,
                flag("delete_unused_volumes"),
                flag("delete_unused_networks"),
            )
            .await
        })
    }

    /// `reportMissedOccurrence()` for `docker-cleanup`.
    fn report_missed<'a>(
        &'a self,
        jobs: &'a Jobs,
        occurrence: &'a Occurrence,
        message: &'a str,
    ) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            let db = &jobs.0.db;
            let Some((team_id, server)) = lookup::server(db, occurrence.resource_id).await else {
                return;
            };
            if let Err(e) =
                queries::record_missed_cleanup(db, occurrence.resource_id, message, Jobs::now())
                    .await
            {
                tracing::error!(target: "scheduled-errors", occurrence_uuid = %occurrence.uuid, error = %e, "Failed to report missed scheduled occurrence");
                return;
            }
            notify_missed_occurrence(
                jobs,
                SERVER_MORPH,
                occurrence.resource_id,
                team_id,
                Notification::DockerCleanupFailed {
                    server,
                    message: format!(
                        "Docker cleanup job failed with the following error: {message}"
                    ),
                },
            )
            .await;
        })
    }
}
