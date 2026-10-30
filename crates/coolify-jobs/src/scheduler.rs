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

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
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
        scheduler.register(crate::scheduled_task::ScheduledTaskSource);
        scheduler
    }

    /// Runs one tick at the start of every minute (`schedule:work` with `everyMinute()`). A tick
    /// only dispatches, so it does not outlive its minute in normal work; a late tick is caught up
    /// by the window in [`due_occurrence`].
    pub fn spawn(self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut cleaned_on: Option<chrono::NaiveDate> = None;
            loop {
                let now = Utc::now();
                let wait = 60 - u64::from(now.second());
                tokio::time::sleep(Duration::from_secs(wait)).await;
                let now = Utc::now();
                let summary = self.tick(now).await;
                tracing::info!(
                    target: "scheduled",
                    dispatched = summary.dispatched,
                    skipped = summary.skipped,
                    "ScheduledJobManager completed"
                );
                // `dailyAt('04:00')`: `deleteOldOccurrences()`.
                if now.hour() == 4 && cleaned_on != Some(now.date_naive()) {
                    cleaned_on = Some(now.date_naive());
                    if let Err(e) = deliveries::delete_old(&self.jobs.0.db, now).await {
                        tracing::error!(target: "scheduled-errors", error = %e, "Failed to delete old occurrences");
                    }
                }
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
                    self.recover_unstarted(source, &row, now).await;
                }
            }
            Err(e) => {
                tracing::error!(target: "scheduled-errors", error = %e, "Failed to recover stale enqueued occurrences");
            }
        }
        let started_before = now
            - chrono::Duration::seconds(WORKER_TIMEOUT_SECONDS)
            - chrono::Duration::minutes(INTERRUPTED_CLAIM_MARGIN_MINUTES);
        if let Err(e) = deliveries::fail_interrupted_claims(
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
        // Notifications are not ported: `notifyMissedOccurrence()` belongs in each report_missed.
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
