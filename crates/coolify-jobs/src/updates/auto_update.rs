//! `App\Services\AutoUpdateNotifier`: tells the root team when an automatic update starts,
//! fails, or finishes. The new version reports the result after it starts, so the attempt is
//! saved in the instance settings (`auto_update_target_version`, `auto_update_started_at`).

use chrono::{DateTime, NaiveDateTime, Utc};

use super::{STATUS_FILE, has_reached_target_version, is_running};
use crate::Jobs;
use crate::notifications;
use crate::server_ops::Srv;

/// `RESULT_TIMEOUT_MINUTES`: an attempt that has not reached the target version after this many
/// minutes is reported as failed.
pub const RESULT_TIMEOUT_MINUTES: i64 = 30;

/// Where the upgrade script writes its log.
const LOG_HINT: &str = "Log: /data/coolify/source/upgrade-*.log";

/// `saveAttempt($targetVersion)`: saves the attempt before the upgrade script runs, so the new
/// version can report the result also when the container restarts before `started()` is called.
pub async fn save_attempt(jobs: &Jobs, target_version: &str) -> Result<(), String> {
    let now = Utc::now().naive_utc();
    sqlx::query(
        "UPDATE instance_settings SET auto_update_target_version = $1, auto_update_started_at = $2, \
         updated_at = $2 WHERE id = 0",
    )
    .bind(target_version)
    .bind(now)
    .execute(jobs.0.db.pool())
    .await
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// `started($currentVersion, $targetVersion)`.
pub fn started(jobs: &Jobs, current_version: &str, target_version: &str) {
    notify(jobs, started_message(current_version, target_version), true);
}

/// `failed($reason)`: the update did not start.
pub async fn failed(jobs: &Jobs, reason: &str) {
    if let Err(e) = clear_attempt(jobs).await {
        tracing::warn!(error = %e, "could not clear the automatic update attempt");
    }
    notify_failure(jobs, reason).await;
}

fn started_message(current_version: &str, target_version: &str) -> String {
    format!("Automatic update started: coolify-rs {current_version} to {target_version}.")
}

fn finished_message(running_version: &str) -> String {
    format!("Automatic update finished: coolify-rs is now on {running_version}.")
}

/// `notifyFailure($reason)`'s message. `base_url` is `base_url()`.
fn failure_message(reason: &str, base_url: &str) -> String {
    format!(
        "Automatic update failed: {reason}\n\nFix the cause, then start the update again: {base_url}/settings/updates"
    )
}

/// What `checkResult()` found for the saved attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttemptResult {
    /// The upgrade is still running, or the result is not known yet.
    Pending,
    /// This version is the target.
    Finished,
    /// The upgrade script wrote an error, or the attempt timed out: the reason.
    Failed(String),
}

/// `isWrittenDuringAttempt($writtenAt, $startedAt)`: an error from an earlier attempt stays in
/// the status file until the new upgrade script writes its first step.
fn is_written_during_attempt(written_at: &str, started_at: Option<DateTime<Utc>>) -> bool {
    let Some(started_at) = started_at else {
        return true;
    };
    // `$startedAt->copy()->startOfSecond()`.
    let started = started_at.timestamp();
    DateTime::parse_from_rfc3339(written_at.trim()).is_ok_and(|at| at.timestamp() >= started)
}

/// The decision of `checkResult()`: success when this version is the target, failure when the
/// upgrade script wrote an error or the attempt timed out. `status` is the content of the
/// upgrade status file.
pub fn attempt_result(
    running_version: &str,
    target_version: &str,
    started_at: Option<DateTime<Utc>>,
    status: &str,
    now: DateTime<Utc>,
) -> AttemptResult {
    if has_reached_target_version(running_version, target_version) {
        return AttemptResult::Finished;
    }
    // `array_pad(explode('|', trim($status)), 3, '')`.
    let mut parts = status.trim().split('|');
    let step = parts.next().unwrap_or_default();
    let message = parts.next().unwrap_or_default();
    let written_at = parts.next().unwrap_or_default();
    if step == "error" && is_written_during_attempt(written_at, started_at) {
        return AttemptResult::Failed(format!(
            "the upgrade to {target_version} stopped with this error: {message}. {LOG_HINT}"
        ));
    }
    let timed_out =
        started_at.is_none_or(|at| at < now - chrono::Duration::minutes(RESULT_TIMEOUT_MINUTES));
    if timed_out && !is_running(status, now) {
        return AttemptResult::Failed(format!(
            "coolify-rs {target_version} did not start within {RESULT_TIMEOUT_MINUTES} minutes. coolify-rs {running_version} is still running. {LOG_HINT}"
        ));
    }
    AttemptResult::Pending
}

/// `checkResult()`: reports the result of the saved attempt, one time.
pub async fn check_result(jobs: &Jobs, running_version: &str) {
    if let Err(e) = check_result_inner(jobs, running_version).await {
        tracing::warn!(error = %e, "could not check the automatic update attempt");
    }
}

async fn check_result_inner(jobs: &Jobs, running_version: &str) -> Result<(), String> {
    let attempt: Option<(Option<String>, Option<NaiveDateTime>)> = sqlx::query_as(
        "SELECT auto_update_target_version::text, auto_update_started_at FROM instance_settings WHERE id = 0",
    )
    .fetch_optional(jobs.0.db.pool())
    .await
    .map_err(|e| e.to_string())?;
    let Some((Some(target_version), started_at)) = attempt else {
        return Ok(());
    };
    if target_version.trim().is_empty() {
        return Ok(());
    }
    let started_at = started_at.map(|at| at.and_utc());
    // The status file is only read when this version is not the target.
    let status = if has_reached_target_version(running_version, &target_version) {
        String::new()
    } else {
        read_upgrade_status(jobs).await
    };
    match attempt_result(
        running_version,
        &target_version,
        started_at,
        &status,
        Utc::now(),
    ) {
        AttemptResult::Pending => {}
        AttemptResult::Finished => {
            if claim_attempt(jobs, &target_version).await? {
                notify(jobs, finished_message(running_version), true);
            }
        }
        AttemptResult::Failed(reason) => {
            if claim_attempt(jobs, &target_version).await? {
                notify_failure(jobs, &reason).await;
            }
        }
    }
    Ok(())
}

/// `readUpgradeStatus()`: empty when the status cannot be read.
async fn read_upgrade_status(jobs: &Jobs) -> String {
    match Srv::load(jobs, 0).await {
        Ok(srv) => srv
            .run_quiet(
                &[format!("cat {STATUS_FILE} 2>/dev/null || true")],
                "upgrade-status",
            )
            .await
            .unwrap_or_default(),
        Err(e) => {
            tracing::warn!(error = %e, "Could not read the upgrade status");
            String::new()
        }
    }
}

/// `claimAttempt($settings, $targetVersion)`: clears the attempt only if it is still saved for
/// this target version. Returns false when another run already reported it, so each result is
/// sent one time.
async fn claim_attempt(jobs: &Jobs, target_version: &str) -> Result<bool, String> {
    let claimed = sqlx::query(
        "UPDATE instance_settings SET auto_update_target_version = NULL, auto_update_started_at = NULL \
         WHERE id = 0 AND auto_update_target_version = $1",
    )
    .bind(target_version)
    .execute(jobs.0.db.pool())
    .await
    .map_err(|e| e.to_string())?;
    Ok(claimed.rows_affected() == 1)
}

/// `clearAttempt()`.
async fn clear_attempt(jobs: &Jobs) -> Result<(), String> {
    sqlx::query(
        "UPDATE instance_settings SET auto_update_target_version = NULL, auto_update_started_at = NULL, \
         updated_at = $1 WHERE id = 0",
    )
    .bind(Utc::now().naive_utc())
    .execute(jobs.0.db.pool())
    .await
    .map_err(|e| e.to_string())?;
    Ok(())
}

async fn notify_failure(jobs: &Jobs, reason: &str) {
    let ctx = notifications::Ctx::load(&jobs.0.db).await;
    notify(jobs, failure_message(reason, &ctx.base_url), false);
}

/// `Team::find(0)?->notify(new GeneralNotification($message, $success))`. Upstream restores a
/// claimed attempt when the notification cannot be queued; here delivery is a task of this
/// process and queueing cannot fail.
fn notify(jobs: &Jobs, message: String, success: bool) {
    notifications::spawn_team_general(&jobs.0.db, jobs.0.encrypter.as_ref(), 0, message, success);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn the_new_version_reports_that_the_update_finished() {
        let now = at("2026-11-10T12:05:00Z");
        let started = Some(at("2026-11-10T12:00:00Z"));
        assert_eq!(
            attempt_result("4.7.5", "4.7.5", started, "", now),
            AttemptResult::Finished
        );
        // A newer version than the target also ends the attempt.
        assert_eq!(
            attempt_result(
                "4.7.6",
                "4.7.5",
                started,
                "error|x|2026-11-10T12:01:00+00:00",
                now
            ),
            AttemptResult::Finished
        );
        assert_eq!(
            finished_message("4.7.5"),
            "Automatic update finished: coolify-rs is now on 4.7.5."
        );
        assert_eq!(
            started_message("4.7.4", "4.7.5"),
            "Automatic update started: coolify-rs 4.7.4 to 4.7.5."
        );
    }

    #[test]
    fn an_error_of_the_upgrade_script_fails_the_attempt() {
        let now = at("2026-11-10T12:05:00Z");
        let started = Some(at("2026-11-10T12:00:00Z"));
        assert_eq!(
            attempt_result(
                "4.7.4",
                "4.7.5",
                started,
                "error|Could not pull the image|2026-11-10T12:01:00+00:00\n",
                now
            ),
            AttemptResult::Failed(
                "the upgrade to 4.7.5 stopped with this error: Could not pull the image. Log: /data/coolify/source/upgrade-*.log"
                    .to_owned()
            )
        );
        // An error from an earlier attempt stays in the file until the new script writes.
        assert_eq!(
            attempt_result(
                "4.7.4",
                "4.7.5",
                started,
                "error|old|2026-11-10T11:00:00+00:00",
                now
            ),
            AttemptResult::Pending
        );
        // An error written in the second the attempt started belongs to it.
        assert!(is_written_during_attempt(
            "2026-11-10T12:00:00+00:00",
            Some(at("2026-11-10T12:00:00.750Z"))
        ));
        assert!(is_written_during_attempt("anything", None));
        assert!(!is_written_during_attempt("not a time", started));
        assert!(!is_written_during_attempt("", started));
    }

    #[test]
    fn an_attempt_that_never_reaches_the_target_times_out() {
        let started = Some(at("2026-11-10T12:00:00Z"));
        let timeout = AttemptResult::Failed(
            "coolify-rs 4.7.5 did not start within 30 minutes. coolify-rs 4.7.4 is still running. Log: /data/coolify/source/upgrade-*.log"
                .to_owned(),
        );
        // Within the timeout the result is not known yet.
        assert_eq!(
            attempt_result("4.7.4", "4.7.5", started, "", at("2026-11-10T12:30:00Z")),
            AttemptResult::Pending
        );
        assert_eq!(
            attempt_result("4.7.4", "4.7.5", started, "", at("2026-11-10T12:30:01Z")),
            timeout
        );
        // A script that still writes its steps is not reported as failed.
        assert_eq!(
            attempt_result(
                "4.7.4",
                "4.7.5",
                started,
                "3|Pulling images|2026-11-10T12:29:00+00:00",
                at("2026-11-10T12:31:00Z")
            ),
            AttemptResult::Pending
        );
        // An attempt without a start time counts as timed out.
        assert_eq!(
            attempt_result("4.7.4", "4.7.5", None, "", at("2026-11-10T12:00:01Z")),
            timeout
        );
    }

    #[test]
    fn a_failure_tells_where_to_start_the_update_again() {
        assert_eq!(
            failure_message("the backup failed", "https://coolify.example"),
            "Automatic update failed: the backup failed\n\nFix the cause, then start the update again: https://coolify.example/settings/updates"
        );
    }
}
