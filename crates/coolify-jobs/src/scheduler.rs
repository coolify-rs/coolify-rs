//! The dispatch of `ScheduledJobManager` and `ScheduledJobDeliveryService`: every minute each
//! registered [`ScheduleSource`] says which schedules exist, the scheduler decides which are due
//! (cron, server time zone, catch-up window), records the occurrence so that it runs once, and
//! runs it on a tokio task.
//!
//! Dedupe lives in `scheduled_job_states` and `scheduled_job_deliveries`, the rows upstream uses,
//! so two schedulers on one database never run the same occurrence twice: the state row is locked
//! and moved by whichever records it first, and only one worker can claim a delivery.
//!
//! Upstream's Redis pieces have no counterpart: `onOneServer` and the manager's overlap lock are
//! covered by the one loop in the process, and the job overlap lock is held in the process.
//!
//! This is the only scheduler: [`crate::kernel`] holds the rest of `Kernel::schedule()`, including
//! `ServerManagerJob`, and [`crate::schedule_sources`] and [`crate::scheduled_task`] the types of
//! `ScheduledJobManager::TYPES`.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use chrono::{DateTime, Timelike, Utc};
use chrono_tz::Tz;
use coolify_compat::cron::translate_cron_expression;
use coolify_compat::cron_expression::{CronError, CronExpression, in_zone, time_zone};
use coolify_compat::timezones::valid_timezone;
use coolify_db::scheduled_jobs::{self as deliveries, Delivery, NewDelivery};
use coolify_domain::new_public_id;
use serde_json::Value;

use crate::Jobs;

/// `ScheduledJobDeliveryService::CATCH_UP_WINDOW_MINUTES`.
pub const CATCH_UP_WINDOW_MINUTES: i64 = 10;
/// `ENQUEUED_STALE_AFTER_MINUTES`.
const ENQUEUED_STALE_AFTER_MINUTES: i64 = 60;
/// `RECOVERY_BATCH_SIZE`.
const RECOVERY_BATCH_SIZE: i64 = 500;
/// `INTERRUPTED_CLAIM_MARGIN_MINUTES`.
const INTERRUPTED_CLAIM_MARGIN_MINUTES: i64 = 15;
/// `config('horizon.worker_timeout')`.
const WORKER_TIMEOUT_SECONDS: i64 = 39600;
/// `LATE_RUN_MAX_AGE_HOURS`.
const LATE_RUN_MAX_AGE_HOURS: i64 = 24;
/// `reportMissedOccurrence()`.
pub const MISSED_MESSAGE: &str = "Skipped: the queued job did not start within 60 minutes.";

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// One schedule that may be due.
#[derive(Debug, Clone)]
pub struct Due {
    /// `scheduled-task:{id}`, `scheduled-backup:{id}`, ...; the dedupe key.
    pub schedule_key: String,
    /// The stored frequency; `VALID_CRON_STRINGS` names are accepted.
    pub frequency: String,
    /// From [`server_timezone`].
    pub timezone: String,
    /// `scheduled_job_deliveries.job_type`.
    pub job_type: &'static str,
    pub resource_id: i64,
    pub payload: Value,
    /// A skip reason (`server_not_functional`, `application_not_running`, ...). The occurrence is
    /// recorded as seen and nothing runs (`recordSkipped()`).
    pub skip: Option<String>,
}

/// One claimed occurrence given to [`ScheduleSource::run`].
#[derive(Debug, Clone)]
pub struct Occurrence {
    pub uuid: String,
    pub schedule_key: String,
    pub scheduled_for: DateTime<Utc>,
    pub job_type: String,
    pub resource_id: i64,
    pub payload: Value,
}

impl From<&Delivery> for Occurrence {
    fn from(d: &Delivery) -> Self {
        Self {
            uuid: d.uuid.clone(),
            schedule_key: d.schedule_key.clone(),
            scheduled_for: d.scheduled_for,
            job_type: d.job_type.clone(),
            resource_id: d.resource_id,
            payload: d.payload_value(),
        }
    }
}

/// `WithoutOverlapping($key)->expireAfter($ttl)->dontRelease()` of a job.
#[derive(Debug, Clone)]
pub struct Overlap {
    pub key: String,
    pub ttl: Duration,
}

/// A kind of scheduled work. Register one with [`Scheduler::register`].
pub trait ScheduleSource: Send + Sync + 'static {
    /// A key of `ScheduledJobManager::TYPES`, for logs.
    fn name(&self) -> &'static str;

    /// The delivery job types this source owns (`TYPES[$type]`), for recovery.
    fn job_types(&self) -> &'static [&'static str];

    /// All enabled schedules at `now`, with their skip reasons. The scheduler filters by cron, so
    /// a source does not need to. Housekeeping such as deleting a schedule whose resource is gone
    /// belongs here. An error is logged and the next minute tries again.
    fn due<'a>(
        &'a self,
        jobs: &'a Jobs,
        now: DateTime<Utc>,
    ) -> BoxFuture<'a, Result<Vec<Due>, String>>;

    /// The job's overlap lock, or `None` when the resource no longer exists: `publish()` then
    /// marks the occurrence skipped.
    fn overlap<'a>(&'a self, jobs: &'a Jobs, resource_id: i64) -> BoxFuture<'a, Option<Overlap>>;

    /// The job. `Err` marks the occurrence failed; the source has already recorded its own
    /// execution row and notification.
    fn run<'a>(
        &'a self,
        jobs: &'a Jobs,
        occurrence: &'a Occurrence,
    ) -> BoxFuture<'a, Result<(), String>>;

    /// Whether a lost occurrence runs once when late (`LATE_RUN_JOB_TYPES`: the backups).
    fn runs_late(&self) -> bool {
        false
    }

    /// `reportMissedOccurrence()`: show a lost occurrence as a failed execution.
    fn report_missed<'a>(
        &'a self,
        _jobs: &'a Jobs,
        _occurrence: &'a Occurrence,
        _message: &'a str,
    ) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }
}

/// `serverTimezone()`: the server's setting when PHP knows it, otherwise `config('app.timezone')`.
pub fn server_timezone(settings: &coolify_db::records::Record) -> String {
    settings
        .get("server_timezone")
        .and_then(Value::as_str)
        .filter(|tz| valid_timezone(tz))
        .unwrap_or("UTC")
        .to_owned()
}

fn zone(name: &str) -> Tz {
    time_zone(name).unwrap_or(Tz::UTC)
}

/// `normalizeFrequency()` and `new CronExpression(...)`.
pub fn parse_frequency(frequency: &str) -> Result<CronExpression, CronError> {
    CronExpression::new(translate_cron_expression(frequency))
}

/// The occurrence `getPreviousRunDate($executionTime, allowCurrentDate: true)` finds, when it is
/// inside the catch-up window (`isDueCandidateBeforeExpensiveChecks()`, `recordAndPublish()`).
pub fn due_occurrence(
    cron: &CronExpression,
    timezone: &str,
    now: DateTime<Utc>,
) -> Result<Option<DateTime<Utc>>, CronError> {
    let previous = cron
        .previous_run_date(&in_zone(now, zone(timezone)), true)?
        .with_timezone(&Utc);
    let window_start = now - chrono::Duration::minutes(CATCH_UP_WINDOW_MINUTES);
    Ok((previous >= window_start).then_some(previous))
}

/// `isDuplicateOccurrence()`: a schedule with fixed hours runs once when daylight saving time ends
/// and a local time exists twice; an interval schedule runs in both hours.
pub fn is_duplicate_occurrence(
    last: Option<DateTime<Utc>>,
    scheduled_for: DateTime<Utc>,
    cron: &CronExpression,
    timezone: &str,
) -> bool {
    let Some(last) = last else {
        return false;
    };
    if last >= scheduled_for {
        return true;
    }
    let hour = cron.hour_part();
    let fixed = !hour.is_empty()
        && hour
            .bytes()
            .all(|b| b.is_ascii_digit() || b == b',' || b == b'-');
    if !fixed {
        return false;
    }
    let tz = zone(timezone);
    let format = |d: DateTime<Utc>| in_zone(d, tz).format("%Y-%m-%d %H:%M").to_string();
    format(last) == format(scheduled_for)
}

/// `COOLIFY_RS_SCHEDULER_ONLY`: a comma separated list of schedule keys (`scheduled-task:12`,
/// `server:3`, `kernel:cleanup:database`). When set, nothing else is touched: a controlled test on
/// a database other schedules and other agents share. Unset in production.
pub fn parse_only(value: Option<&str>) -> Option<HashSet<String>> {
    value.map(|v| {
        v.split(',')
            .map(str::trim)
            .filter(|k| !k.is_empty())
            .map(str::to_owned)
            .collect()
    })
}

static ONLY: LazyLock<Option<HashSet<String>>> =
    LazyLock::new(|| parse_only(std::env::var("COOLIFY_RS_SCHEDULER_ONLY").ok().as_deref()));

/// Whether this schedule key may be acted on, housekeeping included.
pub fn allowed(key: &str) -> bool {
    ONLY.as_ref().is_none_or(|only| only.contains(key))
}

static HEARTBEAT: AtomicI64 = AtomicI64::new(0);

/// `Cache::put('scheduled-job-manager:heartbeat', now()->toIso8601String(), 300)`.
pub fn record_heartbeat(now: DateTime<Utc>) {
    HEARTBEAT.store(now.timestamp(), Ordering::Relaxed);
}

/// `Cache::get('scheduled-job-manager:heartbeat') !== null`.
pub fn heartbeat_alive(now: DateTime<Utc>) -> bool {
    let last = HEARTBEAT.load(Ordering::Relaxed);
    last > 0 && now.timestamp() - last < 300
}

static CRON_DEDUPE: LazyLock<Mutex<HashMap<String, DateTime<Utc>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// `shouldRunCronNow($frequency, $timezone, $dedupKey, $executionTime)`: a missed minute is caught up
/// on the next run; the first run only fires on an exact match and seeds the key.
pub fn should_run_cron_now(frequency: &str, timezone: &str, key: &str, at: DateTime<Utc>) -> bool {
    let Ok(cron) = parse_frequency(frequency) else {
        return false;
    };
    let local = in_zone(at, zone(timezone));
    let Ok(previous) = cron.previous_run_date(&local, true) else {
        return false;
    };
    let previous = previous.with_timezone(&Utc);
    let mut seen = CRON_DEDUPE.lock().unwrap_or_else(|p| p.into_inner());
    let fire = match seen.get(key) {
        None => cron.is_due(&local),
        Some(last) => previous > *last,
    };
    seen.insert(key.to_owned(), if fire { at } else { previous });
    fire
}

/// Whether the setting turns the scheduler on. `COOLIFY_RS_SCHEDULER` unset means on, the
/// production default; `scripts/dev-server.sh` sets it to `off`.
pub fn enabled_by_setting(value: Option<&str>) -> bool {
    !matches!(
        value.map(|v| v.trim().to_ascii_lowercase()).as_deref(),
        Some("off" | "0" | "false" | "no")
    )
}

/// Held while a job runs; dropping it is the lock's release.
#[derive(Debug)]
pub(crate) struct OverlapGuard {
    jobs: Jobs,
    key: String,
    token: String,
}

impl Drop for OverlapGuard {
    fn drop(&mut self) {
        if let Ok(mut locks) = self.jobs.0.overlap_locks.lock()
            && locks.get(&self.key).is_some_and(|(_, t)| *t == self.token)
        {
            locks.remove(&self.key);
        }
    }
}

impl Jobs {
    /// `WithoutOverlapping::handle()`: `None` when another run holds an unexpired lock.
    pub(crate) fn try_overlap(&self, overlap: &Overlap) -> Option<OverlapGuard> {
        let token = new_public_id();
        let mut locks: std::sync::MutexGuard<'_, HashMap<String, (Instant, String)>> =
            self.0.overlap_locks.lock().ok()?;
        let now = Instant::now();
        if locks
            .get(&overlap.key)
            .is_some_and(|(expires, _)| *expires > now)
        {
            return None;
        }
        locks.insert(overlap.key.clone(), (now + overlap.ttl, token.clone()));
        Some(OverlapGuard {
            jobs: self.clone(),
            key: overlap.key.clone(),
            token,
        })
    }
}

/// `ScheduledJobManager` and its `ScheduledJobDeliveryService`.
pub struct Scheduler {
    jobs: Jobs,
    sources: Vec<Arc<dyn ScheduleSource>>,
}

impl std::fmt::Debug for Scheduler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let names: Vec<&str> = self.sources.iter().map(|s| s.name()).collect();
        f.debug_struct("Scheduler")
            .field("sources", &names)
            .finish()
    }
}

/// What one tick did (`dispatchedCount`, `skippedCount`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TickSummary {
    pub dispatched: usize,
    pub skipped: usize,
}

impl Scheduler {
    pub fn new(jobs: Jobs) -> Self {
        Self {
            jobs,
            sources: Vec::new(),
        }
    }

    /// Adds a source; call before [`Scheduler::spawn`].
    pub fn register(&mut self, source: impl ScheduleSource) -> &mut Self {
        self.sources.push(Arc::new(source));
        self
    }

    /// Every source the process runs. A new area adds its line here.
    pub fn with_default_sources(jobs: Jobs) -> Self {
        let mut scheduler = Self::new(jobs);
        scheduler
            .register(crate::schedule_sources::DatabaseBackupSource)
            .register(crate::scheduled_task::ScheduledTaskSource)
            .register(crate::schedule_sources::VolumeBackupSource)
            .register(crate::schedule_sources::DockerCleanupSource);
        scheduler
    }

    /// `schedule:work`: one run at the start of every minute. A tick only dispatches, so it does
    /// not outlive its minute in normal work; a late tick is caught up by the window in
    /// [`due_occurrence`]. The rest of `Kernel::schedule()` runs beside the manager, so a slow
    /// manager cannot make it skip a minute.
    pub fn spawn(self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                let now = Utc::now();
                let wait = 60 - u64::from(now.second());
                tokio::time::sleep(Duration::from_secs(wait)).await;
                let now = Utc::now();
                crate::kernel::run_due(&self.jobs, now).await;
                let summary = self.tick(now).await;
                tracing::info!(
                    target: "scheduled",
                    dispatched = summary.dispatched,
                    skipped = summary.skipped,
                    "ScheduledJobManager completed"
                );
                record_heartbeat(Utc::now());
            }
        })
    }

    /// `ScheduledJobManager::handle()` frozen at `now`.
    pub async fn tick(&self, now: DateTime<Utc>) -> TickSummary {
        let mut summary = TickSummary::default();
        for source in &self.sources {
            self.recover(source, now).await;
            let due = match source.due(&self.jobs, now).await {
                Ok(due) => due,
                Err(e) => {
                    tracing::error!(target: "scheduled-errors", source = source.name(), error = %e, "Failed to process schedules");
                    continue;
                }
            };
            for item in due {
                if !allowed(&item.schedule_key) {
                    continue;
                }
                if let Err(e) = self.process(source, &item, now, &mut summary).await {
                    tracing::error!(target: "scheduled-errors", schedule_key = %item.schedule_key, error = %e, "Error processing schedule");
                }
            }
        }
        summary
    }

    /// `processScheduledTask()` and its counterparts, after `isDueCandidateBeforeExpensiveChecks()`.
    async fn process(
        &self,
        source: &Arc<dyn ScheduleSource>,
        item: &Due,
        now: DateTime<Utc>,
        summary: &mut TickSummary,
    ) -> Result<(), String> {
        let cron = parse_frequency(&item.frequency).map_err(|e| e.to_string())?;
        let Some(scheduled_for) =
            due_occurrence(&cron, &item.timezone, now).map_err(|e| e.to_string())?
        else {
            return Ok(());
        };
        let duplicate =
            |last| is_duplicate_occurrence(last, scheduled_for, &cron, item.timezone.as_str());
        let db = &self.jobs.0.db;
        let stamp = now.naive_utc();
        if let Some(reason) = &item.skip {
            let recorded = deliveries::record_occurrence(
                db,
                &item.schedule_key,
                scheduled_for,
                duplicate,
                None,
                stamp,
            )
            .await
            .map_err(|e| e.to_string())?;
            if recorded.is_some() {
                summary.skipped += 1;
                tracing::info!(target: "scheduled", schedule_key = %item.schedule_key, skip_reason = %reason, "schedule skipped");
            }
            return Ok(());
        }
        let recorded = deliveries::record_occurrence(
            db,
            &item.schedule_key,
            scheduled_for,
            duplicate,
            Some(NewDelivery {
                job_type: item.job_type,
                resource_id: item.resource_id,
                payload: &item.payload,
            }),
            stamp,
        )
        .await
        .map_err(|e| e.to_string())?;
        if let Some(Some(delivery)) = recorded
            && self
                .publish(source, &delivery)
                .await
                .map_err(|e| e.to_string())?
        {
            summary.dispatched += 1;
            tracing::info!(target: "scheduled", schedule_key = %item.schedule_key, "schedule dispatched");
        }
        Ok(())
    }

    /// `publish()`: `dispatch($job)` is a tokio task.
    async fn publish(
        &self,
        source: &Arc<dyn ScheduleSource>,
        delivery: &Delivery,
    ) -> coolify_db::DbResult<bool> {
        let db = &self.jobs.0.db;
        let Some(overlap) = source.overlap(&self.jobs, delivery.resource_id).await else {
            deliveries::mark_skipped(db, delivery.id).await?;
            tracing::warn!(target: "scheduled", occurrence_uuid = %delivery.uuid, schedule_key = %delivery.schedule_key, "Scheduled occurrence skipped: resource not found");
            return Ok(false);
        };
        let jobs = self.jobs.clone();
        let source = Arc::clone(source);
        let occurrence = Occurrence::from(delivery);
        tokio::spawn(async move {
            run_occurrence(jobs, source, occurrence, overlap).await;
        });
        deliveries::mark_enqueued(db, delivery.id, Utc::now()).await?;
        Ok(true)
    }

    /// `recoverStaleEnqueued()`, `failInterruptedClaims()`, `publishPending()`.
    async fn recover(&self, source: &Arc<dyn ScheduleSource>, now: DateTime<Utc>) {
        let db = &self.jobs.0.db;
        let types = source.job_types();
        let stale_before = now - chrono::Duration::minutes(ENQUEUED_STALE_AFTER_MINUTES);
        match deliveries::stale_enqueued(db, types, stale_before, RECOVERY_BATCH_SIZE).await {
            Ok(rows) => {
                for row in rows {
                    if allowed(&row.schedule_key) {
                        self.recover_unstarted(source, &row, now).await;
                    }
                }
            }
            Err(e) => {
                tracing::error!(target: "scheduled-errors", error = %e, "Failed to recover stale enqueued occurrences");
            }
        }
        let started_before = now
            - chrono::Duration::seconds(WORKER_TIMEOUT_SECONDS)
            - chrono::Duration::minutes(INTERRUPTED_CLAIM_MARGIN_MINUTES);
        // The update has no per-row step: with a key filter on, it is left to whoever owns the rows.
        if ONLY.is_none()
            && let Err(e) = deliveries::fail_interrupted_claims(
                db,
                types,
                started_before,
                RECOVERY_BATCH_SIZE,
                now.naive_utc(),
            )
            .await
        {
            tracing::error!(target: "scheduled-errors", error = %e, "Failed to recover interrupted occurrences");
        }
        match deliveries::pending(db, types, RECOVERY_BATCH_SIZE).await {
            Ok(rows) => {
                for row in rows {
                    if !allowed(&row.schedule_key) {
                        continue;
                    }
                    let old = row.created_at.is_some_and(|c| c < stale_before.naive_utc());
                    if old {
                        self.recover_unstarted(source, &row, now).await;
                    } else if let Err(e) = self.publish(source, &row).await {
                        tracing::error!(target: "scheduled-errors", occurrence_uuid = %row.uuid, error = %e, "Failed to publish pending scheduled occurrence");
                    }
                }
            }
            Err(e) => {
                tracing::error!(target: "scheduled-errors", error = %e, "Failed to publish pending occurrences");
            }
        }
    }

    /// `recoverUnstartedOccurrence()`.
    async fn recover_unstarted(
        &self,
        source: &Arc<dyn ScheduleSource>,
        row: &Delivery,
        now: DateTime<Utc>,
    ) {
        let db = &self.jobs.0.db;
        let stale_before = now - chrono::Duration::minutes(ENQUEUED_STALE_AFTER_MINUTES);
        let newer = deliveries::has_newer_occurrence(db, &row.schedule_key, row.scheduled_for)
            .await
            .unwrap_or(false);
        let too_old = row.scheduled_for < now - chrono::Duration::hours(LATE_RUN_MAX_AGE_HOURS);
        let run_late = source.runs_late() && !newer && !too_old;
        let status = if run_late { "pending" } else { "failed" };
        let moved =
            deliveries::move_unstarted(db, row, stale_before, status, now.naive_utc()).await;
        if !moved.unwrap_or(false) {
            return;
        }
        if run_late {
            let mut pending = row.clone();
            pending.status = "pending".into();
            if let Err(e) = self.publish(source, &pending).await {
                tracing::error!(target: "scheduled-errors", occurrence_uuid = %row.uuid, error = %e, "Failed to publish scheduled occurrence again");
            }
            return;
        }
        tracing::warn!(target: "scheduled", occurrence_uuid = %row.uuid, previous_status = %row.status, "Scheduled occurrence missed: its queued job was not started");
        source
            .report_missed(&self.jobs, &Occurrence::from(row), MISSED_MESSAGE)
            .await;
    }
}

/// The queued job: its overlap middleware, then `claim()`, the work, `complete()` or `fail()`.
async fn run_occurrence(
    jobs: Jobs,
    source: Arc<dyn ScheduleSource>,
    occurrence: Occurrence,
    overlap: Overlap,
) {
    let db = jobs.0.db.clone();
    let Some(_guard) = jobs.try_overlap(&overlap) else {
        if deliveries::skip(&db, &occurrence.uuid, Utc::now().naive_utc())
            .await
            .unwrap_or(false)
        {
            tracing::warn!(target: "scheduled", occurrence_uuid = %occurrence.uuid, "Scheduled occurrence skipped: the previous run of this schedule is still running");
        }
        return;
    };
    let token = new_public_id();
    match deliveries::claim(&db, &occurrence.uuid, &token, Utc::now()).await {
        Ok(true) => {
            tracing::info!(target: "scheduled", occurrence_uuid = %occurrence.uuid, "Scheduled occurrence claimed");
        }
        Ok(false) => {
            tracing::warn!(target: "scheduled", occurrence_uuid = %occurrence.uuid, "Scheduled occurrence claim rejected");
            return;
        }
        Err(e) => {
            tracing::error!(target: "scheduled-errors", occurrence_uuid = %occurrence.uuid, error = %e, "Scheduled occurrence claim failed");
            return;
        }
    }
    let result = source.run(&jobs, &occurrence).await;
    let done = match &result {
        Ok(()) => deliveries::complete(&db, &occurrence.uuid, &token).await,
        Err(_) => deliveries::fail(&db, &occurrence.uuid, &token, Utc::now().naive_utc()).await,
    };
    if let Err(e) = done {
        tracing::error!(target: "scheduled-errors", occurrence_uuid = %occurrence.uuid, error = %e, "Could not finish the scheduled occurrence");
    }
    tracing::info!(target: "scheduled", occurrence_uuid = %occurrence.uuid, failed = result.is_err(), "Scheduled occurrence finished");
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, s).unwrap()
    }

    #[test]
    fn the_key_filter_is_a_comma_separated_list() {
        assert_eq!(parse_only(None), None);
        let only = parse_only(Some("scheduled-task:1, server:2,,")).unwrap();
        assert!(only.contains("scheduled-task:1") && only.contains("server:2"));
        assert_eq!(only.len(), 2);
    }

    #[test]
    fn should_run_cron_now_catches_up_a_missed_minute() {
        let key = "test-should-run-catch-up";
        let hourly = "0 * * * *";
        // First run: only an exact match fires, and it seeds the key.
        assert!(!should_run_cron_now(
            hourly,
            "UTC",
            key,
            utc(2026, 10, 1, 12, 1, 0)
        ));
        assert!(!should_run_cron_now(
            hourly,
            "UTC",
            key,
            utc(2026, 10, 1, 12, 30, 0)
        ));
        // 13:00 was missed: 13:02 catches it up, once.
        assert!(should_run_cron_now(
            hourly,
            "UTC",
            key,
            utc(2026, 10, 1, 13, 2, 0)
        ));
        assert!(!should_run_cron_now(
            hourly,
            "UTC",
            key,
            utc(2026, 10, 1, 13, 3, 0)
        ));
    }

    #[test]
    fn should_run_cron_now_uses_the_time_zone() {
        // 03:00 in Karachi is 22:00 UTC.
        let at = utc(2026, 7, 1, 22, 0, 0);
        assert!(should_run_cron_now(
            "0 3 * * *",
            "Asia/Karachi",
            "test-zone-karachi",
            at
        ));
        assert!(!should_run_cron_now(
            "0 3 * * *",
            "UTC",
            "test-zone-utc",
            at
        ));
    }

    #[test]
    fn the_heartbeat_expires_after_five_minutes() {
        let at = utc(2026, 10, 1, 12, 0, 0);
        record_heartbeat(at);
        assert!(heartbeat_alive(at + chrono::Duration::seconds(299)));
        assert!(!heartbeat_alive(at + chrono::Duration::seconds(300)));
    }

    #[test]
    fn the_setting_is_on_unless_turned_off() {
        assert!(enabled_by_setting(None));
        assert!(enabled_by_setting(Some("on")));
        assert!(enabled_by_setting(Some("1")));
        for off in ["off", "OFF", "0", "false", "no", " off "] {
            assert!(!enabled_by_setting(Some(off)), "{off}");
        }
    }

    #[test]
    fn frequency_names_are_normalized() {
        let now = utc(2026, 10, 1, 12, 0, 20);
        for name in ["every_minute", "hourly", "@hourly"] {
            let cron = parse_frequency(name).unwrap();
            assert_eq!(
                due_occurrence(&cron, "UTC", now).unwrap(),
                Some(utc(2026, 10, 1, 12, 0, 0)),
                "{name}"
            );
        }
        assert!(parse_frequency("garbage").is_err());
    }

    #[test]
    fn the_catch_up_window_is_ten_minutes() {
        let cron = parse_frequency("0 12 * * *").unwrap();
        let exact = utc(2026, 10, 1, 12, 10, 0);
        assert!(due_occurrence(&cron, "UTC", exact).unwrap().is_some());
        let late = utc(2026, 10, 1, 12, 10, 1);
        assert!(due_occurrence(&cron, "UTC", late).unwrap().is_none());
        let early = utc(2026, 10, 1, 11, 59, 59);
        assert!(due_occurrence(&cron, "UTC", early).unwrap().is_none());
    }

    #[test]
    fn the_server_time_zone_decides_the_occurrence() {
        let cron = parse_frequency("0 3 * * *").unwrap();
        // 03:00 in Karachi (+05:00) is 22:00 UTC.
        let now = utc(2026, 7, 1, 22, 0, 30);
        assert_eq!(
            due_occurrence(&cron, "Asia/Karachi", now).unwrap(),
            Some(utc(2026, 7, 1, 22, 0, 0))
        );
        assert!(due_occurrence(&cron, "UTC", now).unwrap().is_none());
    }

    #[test]
    fn server_timezone_falls_back_to_utc() {
        let settings = |tz: &str| {
            let mut r = coolify_db::records::Record::new();
            r.insert("server_timezone".into(), Value::String(tz.into()));
            r
        };
        assert_eq!(server_timezone(&settings("Asia/Karachi")), "Asia/Karachi");
        // Only the canonical names of timezone_identifiers_list() are valid.
        assert_eq!(server_timezone(&settings("US/Eastern")), "UTC");
        assert_eq!(server_timezone(&settings("Mars/Base")), "UTC");
        assert_eq!(server_timezone(&coolify_db::records::Record::new()), "UTC");
    }

    #[test]
    fn dedupe_of_an_occurrence() {
        let cron = parse_frequency("*/5 * * * *").unwrap();
        let at = utc(2026, 10, 1, 12, 5, 0);
        assert!(!is_duplicate_occurrence(None, at, &cron, "UTC"));
        assert!(is_duplicate_occurrence(Some(at), at, &cron, "UTC"));
        assert!(is_duplicate_occurrence(
            Some(utc(2026, 10, 1, 12, 10, 0)),
            at,
            &cron,
            "UTC"
        ));
        assert!(!is_duplicate_occurrence(
            Some(utc(2026, 10, 1, 12, 0, 0)),
            at,
            &cron,
            "UTC"
        ));
    }

    #[test]
    fn a_fixed_hour_runs_once_when_daylight_saving_time_ends() {
        // New York 01:30 exists twice on 2026-11-01: 05:30 and 06:30 UTC.
        let (first, second) = (utc(2026, 11, 1, 5, 30, 0), utc(2026, 11, 1, 6, 30, 0));
        let tz = "America/New_York";
        let fixed = parse_frequency("30 1 * * *").unwrap();
        assert!(is_duplicate_occurrence(Some(first), second, &fixed, tz));
        let interval = parse_frequency("30 * * * *").unwrap();
        assert!(!is_duplicate_occurrence(Some(first), second, &interval, tz));
    }
}
