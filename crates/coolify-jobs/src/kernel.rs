//! `App\Console\Kernel::schedule()`, apart from `scheduled:dispatch`, which is
//! [`crate::scheduler::Scheduler::tick`], and the jobs and commands only it starts.
//!
//! Not scheduled here, because there is nothing to run:
//! `cleanup:ssh-mux` (coolify-remote opens one connection for each command, `ControlMaster=no`),
//! `versions:refresh-local` (Coolify Cloud only), `cleanup:redis --clear-locks` and
//! `horizon:snapshot` (no Redis, no Horizon), `uploads:clear` (the import route appends to the
//! final file, so there is no chunk directory), and the `isDev()` branch of the schedule.
//! `dns:release-orphaned-records`, `ReconcileGithubRunnersJob` and `UpdateCoolifyJob` belong to
//! their own areas (`docs/KNOWN_LIMITATIONS.md`).

use std::path::PathBuf;
use std::time::Duration;

use chrono::{DateTime, Timelike, Utc};
use coolify_compat::cron_expression::{in_zone, time_zone};
use coolify_compat::timezones::valid_timezone;
use coolify_db::Db;
use coolify_db::instance_jobs as queries;
use coolify_db::scheduled_jobs as deliveries;
use coolify_db::server_ops as ops;
use coolify_db::servers;
use serde_json::{Value, json};

use crate::Jobs;
use crate::notifications::{self, Notification, notify};
use crate::scheduler::{Overlap, allowed, parse_frequency};
use crate::server::ServerInfo;
use crate::server_ops::scheduler::{crc32, server_manager_tick};
use crate::server_ops::{checks, version_compare};

/// `Kernel::MAX_CDN_DELAY_SECONDS`.
const MAX_CDN_DELAY_SECONDS: u64 = 3600;
/// `CleanupDatabase`: the default of `--keep-days`.
const KEEP_DAYS: i64 = 60;
/// `CleanupDatabase::DELETE_BATCH_SIZE`.
const DELETE_BATCH_SIZE: i64 = 1000;
/// `CleanupDatabaseImportUploads::MAX_AGE_HOURS`.
const IMPORT_UPLOAD_MAX_AGE_HOURS: u64 = 24;

/// What a schedule entry runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KernelJob {
    ServerManager,
    CleanupInstanceStuffs,
    DeleteOldOccurrences,
    CleanupStuckedResources,
    PruneExpiredTokens,
    CleanupDatabaseImportUploads,
    ApiTokenExpirationWarning,
    CheckMissingDatabaseBackups,
    CheckMissingVolumeBackups,
    RevalidateUnusableS3Storages,
    CleanupUnreachableServers,
    PullTemplatesFromCdn,
    PullChangelog,
    CheckForUpdates,
    RegenerateSslCert,
    CleanupDatabase,
    CleanupOrphanedPreviewContainers,
}

/// One line of `Kernel::schedule()`.
#[derive(Debug, Clone)]
pub struct Entry {
    /// The command's or job's name; `kernel:{name}` for [`allowed`].
    pub name: &'static str,
    pub frequency: String,
    pub timezone: String,
    /// `withCdnDelay()`.
    pub delay_seconds: u64,
    /// `withoutOverlapping()`, `ShouldBeUnique` or the job's own `WithoutOverlapping`.
    pub lock_seconds: u64,
    pub job: KernelJob,
}

/// `Kernel::cdnDelaySeconds($cron)`: a stable offset for this instance inside the cron interval.
pub fn cdn_delay_seconds(frequency: &str, instance_id: &str, now: DateTime<Utc>) -> u64 {
    let interval = parse_frequency(frequency).ok().and_then(|cron| {
        let local = in_zone(now, time_zone("UTC")?);
        let first = cron.next_run_date(&local, false).ok()?;
        let second = cron.next_run_date(&first, false).ok()?;
        u64::try_from((second - first).num_seconds()).ok()
    });
    let window = interval
        .unwrap_or(MAX_CDN_DELAY_SECONDS)
        .clamp(1, MAX_CDN_DELAY_SECONDS);
    u64::from(crc32(format!("cdn-delay:{instance_id}").as_bytes())) % window
}

/// `config('app.id') ?: config('app.key')`.
fn instance_id() -> String {
    std::env::var("APP_ID")
        .ok()
        .filter(|v| !v.is_empty())
        .or_else(|| std::env::var("APP_KEY").ok())
        .unwrap_or_default()
}

/// The production branch of `Kernel::schedule()`.
pub fn schedule(
    update_check_frequency: Option<&str>,
    instance_timezone: Option<&str>,
) -> Vec<Entry> {
    let update = update_check_frequency
        .filter(|f| !f.is_empty())
        .unwrap_or("0 * * * *")
        .to_owned();
    let instance_tz = instance_timezone
        .filter(|tz| valid_timezone(tz))
        .unwrap_or("UTC")
        .to_owned();
    let delay = cdn_delay_seconds(&update, &instance_id(), Utc::now());
    let entry = |name, frequency: &str, lock_seconds, job| Entry {
        name,
        frequency: frequency.to_owned(),
        timezone: "UTC".to_owned(),
        delay_seconds: 0,
        lock_seconds,
        job,
    };
    let cdn = |name, job| Entry {
        name,
        frequency: update.clone(),
        timezone: instance_tz.clone(),
        delay_seconds: delay,
        lock_seconds: 3600,
        job,
    };
    vec![
        entry(
            "cleanup:scheduled-job-occurrences",
            "0 4 * * *",
            3600,
            KernelJob::DeleteOldOccurrences,
        ),
        entry(
            "cleanup:stucked-resources",
            "17 3 * * *",
            3600,
            KernelJob::CleanupStuckedResources,
        ),
        entry(
            "sanctum:prune-expired",
            "0 * * * *",
            3600,
            KernelJob::PruneExpiredTokens,
        ),
        entry(
            "cleanup:database-import-uploads",
            "0 * * * *",
            3600,
            KernelJob::CleanupDatabaseImportUploads,
        ),
        entry(
            "ApiTokenExpirationWarningJob",
            "0 * * * *",
            120,
            KernelJob::ApiTokenExpirationWarning,
        ),
        entry(
            "CheckMissingDatabaseBackupsJob",
            "0 * * * *",
            1800,
            KernelJob::CheckMissingDatabaseBackups,
        ),
        entry(
            "CheckMissingVolumeBackupsJob",
            "0 * * * *",
            1800,
            KernelJob::CheckMissingVolumeBackups,
        ),
        entry(
            "RevalidateUnusableS3StoragesJob",
            "0 * * * *",
            1800,
            KernelJob::RevalidateUnusableS3Storages,
        ),
        entry(
            "cleanup:unreachable-servers",
            "0 0 * * *",
            3600,
            KernelJob::CleanupUnreachableServers,
        ),
        cdn("PullTemplatesFromCDN", KernelJob::PullTemplatesFromCdn),
        cdn("PullChangelog", KernelJob::PullChangelog),
        entry(
            "CleanupInstanceStuffsJob",
            "*/2 * * * *",
            60,
            KernelJob::CleanupInstanceStuffs,
        ),
        cdn("CheckForUpdatesJob", KernelJob::CheckForUpdates),
        entry(
            "ServerManagerJob",
            "* * * * *",
            60,
            KernelJob::ServerManager,
        ),
        // `twiceDaily()`.
        entry(
            "RegenerateSslCertJob",
            "0 1,13 * * *",
            3600,
            KernelJob::RegenerateSslCert,
        ),
        entry(
            "cleanup:database",
            "0 0 * * *",
            3600,
            KernelJob::CleanupDatabase,
        ),
        entry(
            "CleanupOrphanedPreviewContainersJob",
            "0 0 * * *",
            600,
            KernelJob::CleanupOrphanedPreviewContainers,
        ),
    ]
}

/// `isDue()` of one entry at this minute.
fn is_due(entry: &Entry, now: DateTime<Utc>) -> bool {
    let Ok(cron) = parse_frequency(&entry.frequency) else {
        return false;
    };
    let Some(zone) = time_zone(&entry.timezone) else {
        return false;
    };
    cron.is_due(&in_zone(
        now.with_second(0)
            .unwrap_or(now)
            .with_nanosecond(0)
            .unwrap_or(now),
        zone,
    ))
}

/// `schedule:run` for this minute: every due entry starts on its own task.
pub async fn run_due(jobs: &Jobs, now: DateTime<Utc>) {
    let settings = ops::instance_server_settings(&jobs.0.db).await;
    let frequency = queries::update_check_frequency(&jobs.0.db).await;
    let (settings, frequency) = match (settings, frequency) {
        (Ok(s), Ok(f)) => (s, f),
        (Err(e), _) | (_, Err(e)) => {
            tracing::error!(target: "scheduled-errors", error = %e, "Could not load the instance settings");
            return;
        }
    };
    let entries = schedule(frequency.as_deref(), settings.instance_timezone.as_deref());
    for entry in entries {
        if !allowed(&format!("kernel:{}", entry.name)) || !is_due(&entry, now) {
            continue;
        }
        let jobs = jobs.clone();
        tokio::spawn(async move {
            let Some(_guard) = jobs.try_overlap(&Overlap {
                key: format!("kernel:{}", entry.name),
                ttl: Duration::from_secs(entry.lock_seconds),
            }) else {
                return;
            };
            if entry.delay_seconds > 0 {
                tokio::time::sleep(Duration::from_secs(entry.delay_seconds)).await;
            }
            if let Err(e) = run_job(&jobs, entry.job, now).await {
                tracing::error!(target: "scheduled-errors", job = entry.name, error = %e, "Scheduled instance job failed");
            }
        });
    }
}

/// Runs one job now.
pub async fn run_job(jobs: &Jobs, job: KernelJob, now: DateTime<Utc>) -> Result<(), String> {
    let db = &jobs.0.db;
    match job {
        KernelJob::ServerManager => server_manager_tick(jobs, now).await,
        KernelJob::CleanupInstanceStuffs => {
            cleanup_instance_stuffs(jobs).await;
            Ok(())
        }
        KernelJob::DeleteOldOccurrences => deliveries::delete_old(db, now)
            .await
            .map_err(|e| e.to_string()),
        KernelJob::CleanupStuckedResources => {
            crate::kernel_resources::cleanup_stucked_resources(jobs).await;
            Ok(())
        }
        KernelJob::PruneExpiredTokens => queries::prune_expired_tokens(db, Jobs::now())
            .await
            .map(|_| ())
            .map_err(|e| e.to_string()),
        KernelJob::CleanupDatabaseImportUploads => {
            cleanup_database_import_uploads(&data_dir());
            Ok(())
        }
        KernelJob::ApiTokenExpirationWarning => api_token_expiration_warning(jobs).await,
        KernelJob::CheckMissingDatabaseBackups => check_missing_database_backups(jobs).await,
        KernelJob::CheckMissingVolumeBackups => check_missing_volume_backups(jobs).await,
        KernelJob::RevalidateUnusableS3Storages => revalidate_unusable_s3_storages(jobs).await,
        KernelJob::CleanupUnreachableServers => queries::force_disable_abandoned_servers(db)
            .await
            .map(|_| ())
            .map_err(|e| e.to_string()),
        KernelJob::PullTemplatesFromCdn => {
            pull_templates_from_cdn().await;
            Ok(())
        }
        KernelJob::PullChangelog => {
            pull_changelog(now).await;
            Ok(())
        }
        KernelJob::CheckForUpdates => {
            check_for_updates(jobs, 3).await;
            Ok(())
        }
        KernelJob::RegenerateSslCert => jobs.regenerate_ssl_certificates(None, None, false).await,
        KernelJob::CleanupDatabase => cleanup_database(db).await,
        KernelJob::CleanupOrphanedPreviewContainers => {
            crate::kernel_resources::cleanup_orphaned_preview_containers(jobs).await
        }
    }
}

/// `{COOLIFY_RS_DATA_DIR}`, upstream's `base_path()` for files it replaces at run time.
pub fn data_dir() -> PathBuf {
    std::env::var_os("COOLIFY_RS_DATA_DIR")
        .map_or_else(|| PathBuf::from("/data/coolify"), PathBuf::from)
}

/// `CleanupInstanceStuffsJob::handle`.
async fn cleanup_instance_stuffs(jobs: &Jobs) {
    let db = &jobs.0.db;
    // `cleanupInvitationLink`: `isValid()` removes an expired link and its unfinished user.
    match queries::cleanup_expired_invitations(db, Jobs::now()).await {
        Ok(users) => {
            for user in users {
                if let Err(e) = coolify_db::team_pages::delete_user(db, user).await {
                    tracing::warn!(user, error = ?e, "CleanupInstanceStuffsJob could not delete an invited user");
                }
            }
        }
        Err(e) => tracing::error!(error = %e, "CleanupInstanceStuffsJob failed"),
    }
    // `cleanupExpiredEmailChangeRequests`.
    if let Err(e) = queries::cleanup_expired_email_changes(db, Jobs::now()).await {
        tracing::error!(error = %e, "CleanupInstanceStuffsJob failed");
    }
    // `enforceBackupRetention`: at most once in 30 minutes.
    if crate::server_ops::MARKS.add(
        "backup-retention-enforcement".to_owned(),
        Duration::from_secs(1800),
    ) {
        match coolify_db::database_parts::enabled_backups(db).await {
            Ok(rows) => {
                for (id, _, _) in rows {
                    if let Err(e) = jobs.enforce_backup_retention(id).await {
                        tracing::warn!(backup = id, error = %e, "Failed to enforce retention");
                    }
                }
            }
            Err(e) => {
                crate::server_ops::MARKS.forget("backup-retention-enforcement");
                tracing::error!(error = %e, "Failed to enforce backup retention");
            }
        }
    }
    // `deleteOrphanedNotificationThrottles`: at most once an hour.
    if crate::server_ops::MARKS.add(
        "notification-throttle-orphan-cleanup".to_owned(),
        Duration::from_secs(3600),
    ) && let Err(e) = queries::delete_orphaned_notification_throttles(db).await
    {
        crate::server_ops::MARKS.forget("notification-throttle-orphan-cleanup");
        tracing::error!(error = %e, "Failed to delete orphaned notification throttles");
    }
}

/// `CleanupDatabase::handle` with `--yes`.
async fn cleanup_database(db: &Db) -> Result<(), String> {
    let text = |e: coolify_db::DbError| e.to_string();
    let now = Jobs::now();
    let keep = now - chrono::Duration::days(KEEP_DAYS);
    queries::delete_older_in_batches(
        db,
        "failed_jobs",
        "failed_at",
        now - chrono::Duration::days(1),
        DELETE_BATCH_SIZE,
    )
    .await
    .map_err(text)?;
    queries::delete_old_sessions(db, keep.and_utc().timestamp(), DELETE_BATCH_SIZE)
        .await
        .map_err(text)?;
    for table in [
        "activity_log",
        "application_deployment_queues",
        "scheduled_task_executions",
    ] {
        queries::delete_older_in_batches(db, table, "created_at", keep, DELETE_BATCH_SIZE)
            .await
            .map_err(text)?;
    }
    // `AuditEvent::pruneExpired()`.
    queries::delete_older_in_batches(
        db,
        "audit_events",
        "created_at",
        now - chrono::Duration::days(90),
        DELETE_BATCH_SIZE,
    )
    .await
    .map_err(text)?;
    queries::delete_finished_runner_executions(db, keep, DELETE_BATCH_SIZE)
        .await
        .map_err(text)?;
    Ok(())
}

/// `CleanupDatabaseImportUploads::handle`. `Cache::lock(StartDatabaseImport::lockKey(...))` is held
/// by a running import in [`Jobs`], whose upload is younger than a day in every case that matters.
pub fn cleanup_database_import_uploads(data_dir: &std::path::Path) -> usize {
    fn children(p: &std::path::Path) -> Vec<PathBuf> {
        std::fs::read_dir(p)
            .map(|d| {
                d.flatten()
                    .map(|e| e.path())
                    .filter(|p| p.is_dir())
                    .collect()
            })
            .unwrap_or_default()
    }
    fn newest(p: &std::path::Path) -> Option<std::time::SystemTime> {
        let mut latest = None;
        for entry in std::fs::read_dir(p).ok()?.flatten() {
            let path = entry.path();
            let time = if path.is_dir() {
                newest(&path)
            } else {
                entry.metadata().ok()?.modified().ok()
            };
            latest = latest.max(time);
        }
        latest.or_else(|| std::fs::metadata(p).ok()?.modified().ok())
    }
    let cutoff =
        std::time::SystemTime::now() - Duration::from_secs(IMPORT_UPLOAD_MAX_AGE_HOURS * 3600);
    let mut deleted = 0;
    for team in children(&data_dir.join("upload").join("imports")) {
        for database in children(&team) {
            for upload in children(&database) {
                if newest(&upload).is_some_and(|t| t < cutoff)
                    && std::fs::remove_dir_all(&upload).is_ok()
                {
                    deleted += 1;
                }
            }
            // `deleteIfEmpty`: `remove_dir` only removes an empty directory.
            let _ = std::fs::remove_dir(&database);
        }
        let _ = std::fs::remove_dir(&team);
    }
    deleted
}

/// `ApiTokenExpirationWarningJob::handle`.
async fn api_token_expiration_warning(jobs: &Jobs) -> Result<(), String> {
    let db = &jobs.0.db;
    let now = Jobs::now();
    let tokens = queries::tokens_expiring_within_a_day(db, now)
        .await
        .map_err(|e| e.to_string())?;
    for token in tokens {
        notify(
            db,
            jobs.0.encrypter.as_ref(),
            token.team_id,
            Notification::ApiTokenExpiring {
                token_name: token.name,
                expires_at: Some(token.expires_at.format("%Y-%m-%d %H:%M:%S").to_string()),
            },
        )
        .await;
        queries::mark_token_warning_sent(db, token.id, Jobs::now())
            .await
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// `$lastActivityAt->isAfter(now()->subDays($days))`, and the throttle of "once for each period
/// without backup activity".
async fn send_missing_once(
    jobs: &Jobs,
    morph: &str,
    notification_class: &str,
    id: i64,
    team_id: i64,
    last_activity: chrono::NaiveDateTime,
    notification: Notification,
) {
    let db = &jobs.0.db;
    match notifications::claim_throttle(
        db,
        morph,
        id,
        notification_class,
        Some(last_activity.and_utc()),
    )
    .await
    {
        Ok(true) => notify(db, jobs.0.encrypter.as_ref(), team_id, notification).await,
        Ok(false) => {}
        Err(e) => tracing::warn!(error = %e, "could not claim the missing backup throttle"),
    }
}

/// `CheckMissingDatabaseBackupsJob::handle`.
async fn check_missing_database_backups(jobs: &Jobs) -> Result<(), String> {
    let db = &jobs.0.db;
    let now = Jobs::now();
    for b in queries::database_backups_with_missing_check(db)
        .await
        .map_err(|e| e.to_string())?
    {
        let last_execution = b.last_execution_at.or(b.latest_log_at);
        let Some(last_activity) = last_execution.or(b.created_at) else {
            continue;
        };
        if last_activity > now - chrono::Duration::days(b.days) {
            continue;
        }
        if !notifications::backup_failure_channel_enabled(db, jobs.0.encrypter.as_ref(), b.team_id)
            .await
        {
            continue;
        }
        let Ok(Some(record)) = coolify_db::database_parts::backup_by_id(db, b.id).await else {
            continue;
        };
        // `$backup->database` may be gone; its name then stays empty, as `data_get` gives null.
        let database = jobs.backup_database(&record).await;
        let server = match &database {
            Some(d) => jobs.server_for(d).await.ok(),
            None => None,
        };
        if server.is_some_and(|s: ServerInfo| s.transferred) {
            continue;
        }
        send_missing_once(
            jobs,
            "App\\Models\\ScheduledDatabaseBackup",
            "App\\Notifications\\Database\\BackupMissing",
            b.id,
            b.team_id,
            last_activity,
            Notification::BackupMissing {
                database_name: database
                    .as_ref()
                    .and_then(|d| d.str("name"))
                    .unwrap_or_default()
                    .to_owned(),
                backup_uuid: b.uuid,
                days: json!(b.days),
                last_execution_at: last_execution
                    .map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string()),
            },
        )
        .await;
    }
    Ok(())
}

/// `CheckMissingVolumeBackupsJob::handle`.
async fn check_missing_volume_backups(jobs: &Jobs) -> Result<(), String> {
    let db = &jobs.0.db;
    let now = Jobs::now();
    for b in queries::volume_backups_with_missing_check(db)
        .await
        .map_err(|e| e.to_string())?
    {
        let last_execution = b.latest_log_at;
        let Some(last_activity) = last_execution.or(b.created_at) else {
            continue;
        };
        if last_activity > now - chrono::Duration::days(b.days) {
            continue;
        }
        if !notifications::backup_failure_channel_enabled(db, jobs.0.encrypter.as_ref(), b.team_id)
            .await
        {
            continue;
        }
        let Ok(Some(record)) = coolify_db::volume_backups::schedule_by_id(db, b.id).await else {
            continue;
        };
        // `$backup->server()?->isTransferredAway()`
        if jobs
            .volume_backup_server(&record)
            .await
            .is_some_and(|server| server.transferred)
        {
            continue;
        }
        let Ok(Some(target)) = coolify_db::volume_backups::target_of(db, &record).await else {
            continue;
        };
        let notification = Notification::VolumeBackupMissing {
            backup: jobs.volume_backup_details(&record, &target).await,
            backup_uuid: b.uuid.clone(),
            days: json!(b.days),
            last_execution_at: last_execution.map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string()),
        };
        send_missing_once(
            jobs,
            "App\\Models\\ScheduledVolumeBackup",
            "App\\Notifications\\VolumeBackup\\BackupMissing",
            b.id,
            b.team_id,
            last_activity,
            notification,
        )
        .await;
    }
    Ok(())
}

/// `RevalidateUnusableS3StoragesJob::TIME_BUDGET_SECONDS`.
const S3_TIME_BUDGET_SECONDS: u64 = 1200;

static S3_CURSOR: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(-1);

/// `RevalidateUnusableS3StoragesJob::handle`, with the cursor kept in the process.
async fn revalidate_unusable_s3_storages(jobs: &Jobs) -> Result<(), String> {
    use std::sync::atomic::Ordering;
    let db = &jobs.0.db;
    let deadline = std::time::Instant::now() + Duration::from_secs(S3_TIME_BUDGET_SECONDS);
    let after = S3_CURSOR.load(Ordering::Relaxed);
    let storages = queries::unusable_s3_storages_after(db, after)
        .await
        .map_err(|e| e.to_string())?;
    for row in storages {
        if std::time::Instant::now() >= deadline {
            return Ok(());
        }
        let id = row.get("id").and_then(Value::as_i64).unwrap_or(-1);
        let result = jobs.check_s3_connection(&row).await;
        if let Err(e) = jobs.store_s3_connection_result(&row, result).await {
            tracing::warn!(s3 = id, error = %e, "could not save the storage's result");
        }
        S3_CURSOR.store(id, Ordering::Relaxed);
    }
    S3_CURSOR.store(-1, Ordering::Relaxed);
    Ok(())
}

/// `config('constants.services.official')`. Overridable for tests, which use a local fake.
fn templates_url() -> String {
    std::env::var("COOLIFY_RS_TEMPLATES_URL")
        .unwrap_or_else(|_| "https://cdn.coollabs.io/coolify/service-templates-latest.json".into())
}

/// `config('constants.coolify.releases_url')`.
fn releases_url() -> String {
    std::env::var("RELEASES_URL")
        .unwrap_or_else(|_| "https://cdn.coollabs.io/coolify/releases.json".into())
}

/// `config('constants.coolify.versions_url')`.
fn versions_url() -> String {
    std::env::var("VERSIONS_URL").unwrap_or_else(|_| {
        let cdn = std::env::var("CDN_URL").unwrap_or_else(|_| "https://cdn.coollabs.io".into());
        format!("{cdn}/coolify/versions.json")
    })
}

/// `Http::retry($tries, ...)->timeout($seconds)->get($url)`: a connection error, a 429 or a 5xx
/// tries again.
async fn cdn_get(url: &str, tries: u32, timeout_seconds: u64) -> Result<(u16, String), String> {
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(timeout_seconds))
        .build()
        .map_err(|e| e.to_string())?;
    let mut last = Err("no attempt".to_owned());
    for attempt in 0..tries.max(1) {
        if attempt > 0 {
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        match client.get(url).send().await {
            Ok(response) => {
                let status = response.status().as_u16();
                let body = response.text().await.map_err(|e| e.to_string())?;
                last = Ok((status, body));
                if status != 429 && status < 500 {
                    return last;
                }
            }
            Err(e) => last = Err(e.to_string()),
        }
    }
    last
}

/// `PullTemplatesFromCDN::handle` and `store_service_templates_bundle`.
pub async fn pull_templates_from_cdn() {
    fetch_templates_bundle(&templates_url()).await;
}

/// `handle()` for one URL: whether a bundle was stored.
pub async fn fetch_templates_bundle(url: &str) -> bool {
    match cdn_get(url, 3, 60).await {
        Ok((status, body)) if (200..300).contains(&status) => {
            let stored = crate::services::templates::store_bundle(&body);
            if !stored {
                let limited: String = body.chars().take(500).collect();
                tracing::error!(body = %limited, "PullTemplatesFromCDN received an invalid bundle");
            }
            stored
        }
        Ok((status, body)) => {
            let limited: String = body.chars().take(500).collect();
            tracing::error!(status, body = %limited, "PullTemplatesFromCDN failed");
            false
        }
        Err(e) => {
            tracing::error!(message = %e, "PullTemplatesFromCDN exception");
            false
        }
    }
}

/// `PullChangelog::handle`.
pub async fn pull_changelog(now: DateTime<Utc>) {
    fetch_changelog(&releases_url(), &data_dir(), now).await;
}

/// `handle()` for one URL and directory.
pub async fn fetch_changelog(url: &str, dir: &std::path::Path, now: DateTime<Utc>) {
    let body = match cdn_get(url, 3, 30).await {
        Ok((status, body)) if (200..300).contains(&status) => body,
        Ok((status, _)) => {
            tracing::error!(status, url = %url, "PullChangelogFromGitHub: Failed to fetch from CDN");
            return;
        }
        Err(e) => {
            tracing::error!(message = %e, "PullChangelogFromGitHub: Exception occurred");
            return;
        }
    };
    let Ok(Value::Array(releases)) = serde_json::from_str::<Value>(&body) else {
        tracing::error!("PullChangelogFromGitHub: Exception occurred");
        return;
    };
    if let Err(e) = save_changelog_entries(dir, &releases, now) {
        tracing::error!(message = %e, "PullChangelogFromGitHub: Exception occurred");
    }
}

/// `transformReleasesToChangelog` and `saveChangelogEntries`, for the first 10 releases.
pub fn save_changelog_entries(
    data_dir: &std::path::Path,
    releases: &[Value],
    now: DateTime<Utc>,
) -> Result<(), String> {
    let text = |r: &Value, k: &str| {
        r.get(k)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    let mut months: std::collections::BTreeMap<String, Vec<(DateTime<Utc>, Value)>> =
        std::collections::BTreeMap::new();
    for release in releases.iter().take(10) {
        if release.get("draft").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let Ok(published) = DateTime::parse_from_rfc3339(&text(release, "published_at")) else {
            continue;
        };
        let published = published.with_timezone(&Utc);
        let tag = text(release, "tag_name");
        let name = text(release, "name");
        let body = text(release, "body");
        let entry = json!({
            "tag_name": tag,
            "title": if name.is_empty() { tag.clone() } else { name },
            "content": if body.is_empty() { "No release notes available.".to_owned() } else { body },
            "published_at": published.format("%Y-%m-%dT%H:%M:%S%.6fZ").to_string(),
        });
        months
            .entry(published.format("%Y-%m").to_string())
            .or_default()
            .push((published, entry));
    }
    let dir = data_dir.join("changelogs");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    for (month, mut entries) in months {
        entries.sort_by_key(|(published, _)| std::cmp::Reverse(*published));
        let data = json!({
            "entries": entries.into_iter().map(|(_, e)| e).collect::<Vec<_>>(),
            "last_updated": now.format("%Y-%m-%dT%H:%M:%S%.6fZ").to_string(),
        });
        let text = serde_json::to_string_pretty(&data).map_err(|e| e.to_string())?;
        std::fs::write(dir.join(format!("{month}.json")), text).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// `refresh_versions_file($tries)`: the data that was stored, or `None` when the CDN failed.
pub async fn refresh_versions_file(jobs: &Jobs, tries: u32) -> Option<Value> {
    refresh_versions_from(jobs, &versions_url(), &data_dir(), tries).await
}

/// `refresh_versions_file` for one URL and directory.
pub async fn refresh_versions_from(
    jobs: &Jobs,
    url: &str,
    dir: &std::path::Path,
    tries: u32,
) -> Option<Value> {
    let (status, body) = match cdn_get(url, tries, 10).await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "Could not fetch versions.json from the CDN, using the local file");
            return None;
        }
    };
    let parsed = serde_json::from_str::<Value>(&body)
        .ok()
        .filter(|v| v.is_object() || v.is_array());
    let (true, Some(mut versions)) = ((200..300).contains(&status), parsed) else {
        tracing::warn!(
            status,
            "Could not fetch versions.json from the CDN, using the local file"
        );
        return None;
    };
    let current = jobs.version();
    let path = dir.join("versions.json");
    let existing = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| {
            v.pointer("/coolify/v4/version")
                .and_then(Value::as_str)
                .map(str::to_owned)
        });
    let latest = versions
        .pointer("/coolify/v4/version")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let best = best_version(&latest, existing.as_deref(), &current);
    if let Some(slot) = versions.pointer_mut("/coolify/v4/version") {
        *slot = Value::String(best);
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let text = serde_json::to_string_pretty(&versions).ok()?;
    if let Err(e) = std::fs::write(&path, text) {
        tracing::warn!(error = %e, "Could not store versions.json");
    }
    // `update_helper_version($versions)`.
    if let Some(helper) = versions
        .pointer("/coolify/helper/version")
        .and_then(Value::as_str)
    {
        let stored = ops::instance_server_settings(&jobs.0.db)
            .await
            .ok()
            .and_then(|s| s.helper_version)
            .unwrap_or_default();
        if version_compare(helper, &stored) == std::cmp::Ordering::Greater
            && let Err(e) = queries::set_helper_version(&jobs.0.db, helper).await
        {
            tracing::warn!(error = %e, "Could not store the helper version");
        }
    }
    Some(versions)
}

/// The downgrade rules of `refresh_versions_file`.
pub fn best_version(latest: &str, existing: Option<&str>, current: &str) -> String {
    use std::cmp::Ordering::{Greater, Less};
    let mut best = latest.to_owned();
    if let Some(existing) = existing.filter(|e| !e.is_empty())
        && version_compare(existing, &best) == Greater
    {
        best = existing.to_owned();
    }
    if version_compare(&best, current) == Less {
        best = current.to_owned();
    }
    best
}

/// `CheckForUpdatesJob::handle`: whether `new_version_available` is up to date.
pub async fn check_for_updates(jobs: &Jobs, tries: u32) -> bool {
    let Some(versions) = refresh_versions_file(jobs, tries).await else {
        return false;
    };
    check_traefik_versions(jobs).await;
    let latest = versions
        .pointer("/coolify/v4/version")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let available = version_compare(latest, &jobs.version()) == std::cmp::Ordering::Greater;
    match queries::set_new_version_available(&jobs.0.db, available).await {
        Ok(()) => true,
        Err(e) => {
            tracing::warn!(error = %e, "Could not check for Coolify updates");
            false
        }
    }
}

/// `CheckTraefikVersionJob::handle`.
pub async fn check_traefik_versions(jobs: &Jobs) {
    let db = &jobs.0.db;
    if let Err(e) = queries::clear_traefik_info_of_other_proxies(db, Jobs::now()).await {
        tracing::warn!(error = %e, "Could not clear the Traefik version info");
    }
    let Ok(rows) = servers::all(db).await else {
        return;
    };
    for row in rows {
        if !allowed(&format!("server:{}", row.id)) {
            continue;
        }
        let setting = |k: &str| {
            row.settings
                .as_ref()
                .and_then(|s| s.get(k))
                .and_then(Value::as_bool)
                == Some(true)
        };
        let traefik = row
            .record
            .get("proxy")
            .map(|p| match p {
                Value::String(s) => serde_json::from_str::<Value>(s).unwrap_or(Value::Null),
                other => other.clone(),
            })
            .and_then(|p| p.get("type").and_then(Value::as_str).map(str::to_owned))
            .is_some_and(|t| t == "TRAEFIK");
        if traefik && setting("is_reachable") && setting("is_usable") {
            jobs.spawn_server_job("check-traefik-version", row.id, |j, i| async move {
                checks::check_traefik_version(&j, i).await
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(h: u32, m: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 1, h, m, 0).unwrap()
    }

    fn due_names(now: DateTime<Utc>) -> Vec<&'static str> {
        schedule(None, None)
            .iter()
            .filter(|e| is_due(e, now))
            .map(|e| e.name)
            .collect()
    }

    /// `Kernel::schedule()` line by line, at the minutes that tell them apart.
    #[test]
    fn the_schedule_matches_the_kernel() {
        assert_eq!(
            due_names(at(4, 0)),
            [
                "cleanup:scheduled-job-occurrences",
                "sanctum:prune-expired",
                "cleanup:database-import-uploads",
                "ApiTokenExpirationWarningJob",
                "CheckMissingDatabaseBackupsJob",
                "CheckMissingVolumeBackupsJob",
                "RevalidateUnusableS3StoragesJob",
                "PullTemplatesFromCDN",
                "PullChangelog",
                "CleanupInstanceStuffsJob",
                "CheckForUpdatesJob",
                "ServerManagerJob",
            ]
        );
        assert_eq!(
            due_names(at(3, 17)),
            ["cleanup:stucked-resources", "ServerManagerJob"]
        );
        assert_eq!(
            due_names(at(12, 2)),
            ["CleanupInstanceStuffsJob", "ServerManagerJob"]
        );
        assert_eq!(due_names(at(12, 3)), ["ServerManagerJob"]);
        for hour in [1, 13] {
            assert!(due_names(at(hour, 0)).contains(&"RegenerateSslCertJob"));
        }
        let midnight = due_names(at(0, 0));
        for name in [
            "cleanup:unreachable-servers",
            "cleanup:database",
            "CleanupOrphanedPreviewContainersJob",
        ] {
            assert!(midnight.contains(&name), "{name}");
        }
    }

    #[test]
    fn the_update_check_frequency_and_time_zone_are_the_instance_settings() {
        let entries = schedule(Some("30 2 * * *"), Some("Asia/Karachi"));
        for name in [
            "PullTemplatesFromCDN",
            "PullChangelog",
            "CheckForUpdatesJob",
        ] {
            let entry = entries.iter().find(|e| e.name == name).unwrap();
            assert_eq!(entry.frequency, "30 2 * * *");
            assert_eq!(entry.timezone, "Asia/Karachi");
            // 02:30 in Karachi (+05:00) is 21:30 UTC.
            assert!(is_due(entry, at(21, 30)));
            assert!(!is_due(entry, at(2, 30)));
        }
        let invalid = schedule(None, Some("Mars/Base"));
        assert!(invalid.iter().all(|e| e.timezone == "UTC"));
    }

    /// `crc32('cdn-delay:instance') % window`, from PHP: `crc32('cdn-delay:instance')` = 2196238131.
    #[test]
    fn the_cdn_delay_is_stable_and_inside_the_interval() {
        let hourly = cdn_delay_seconds("0 * * * *", "instance", at(0, 0));
        assert_eq!(
            hourly,
            u64::from(crc32(b"cdn-delay:instance")) % MAX_CDN_DELAY_SECONDS
        );
        assert!(hourly < 3600);
        assert!(cdn_delay_seconds("* * * * *", "instance", at(0, 0)) < 60);
        assert!(cdn_delay_seconds("garbage", "instance", at(0, 0)) < 3600);
    }

    #[test]
    fn a_version_never_goes_down() {
        assert_eq!(best_version("4.1.0", None, "4.0.0"), "4.1.0");
        assert_eq!(best_version("4.1.0", Some("4.2.0"), "4.0.0"), "4.2.0");
        assert_eq!(best_version("4.1.0", Some("4.0.5"), "4.3.0"), "4.3.0");
    }

    #[test]
    fn changelog_files_are_grouped_by_month() {
        let dir = std::env::temp_dir().join(format!(
            "coolify-changelog-{}",
            coolify_domain::new_public_id()
        ));
        let releases = vec![
            json!({"tag_name": "v1", "name": "", "body": "", "draft": false, "published_at": "2026-09-01T10:00:00Z"}),
            json!({"tag_name": "v2", "name": "Two", "body": "notes", "draft": false, "published_at": "2026-09-05T10:00:00Z"}),
            json!({"tag_name": "v3", "name": "x", "body": "x", "draft": true, "published_at": "2026-08-05T10:00:00Z"}),
            json!({"tag_name": "v4", "name": "Four", "body": "b", "draft": false, "published_at": "2026-07-05T10:00:00Z"}),
        ];
        save_changelog_entries(&dir, &releases, at(0, 0)).unwrap();
        let september: Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join("changelogs/2026-09.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(september["entries"][0]["tag_name"], "v2");
        assert_eq!(september["entries"][1]["title"], "v1");
        assert_eq!(
            september["entries"][1]["content"],
            "No release notes available."
        );
        assert!(dir.join("changelogs/2026-07.json").exists());
        assert!(!dir.join("changelogs/2026-08.json").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn only_old_import_uploads_are_removed() {
        let dir = std::env::temp_dir().join(format!(
            "coolify-uploads-{}",
            coolify_domain::new_public_id()
        ));
        let fresh = dir.join("upload/imports/1/db/fresh");
        std::fs::create_dir_all(&fresh).unwrap();
        std::fs::write(fresh.join("a.sql"), "x").unwrap();
        let old = dir.join("upload/imports/2/db/old");
        std::fs::create_dir_all(&old).unwrap();
        let file = old.join("b.sql");
        std::fs::write(&file, "x").unwrap();
        let past = std::time::SystemTime::now() - Duration::from_secs(25 * 3600);
        std::fs::File::options()
            .write(true)
            .open(&file)
            .unwrap()
            .set_modified(past)
            .unwrap();
        assert_eq!(cleanup_database_import_uploads(&dir), 1);
        assert!(fresh.exists());
        assert!(!old.exists());
        assert!(!dir.join("upload/imports/2").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
