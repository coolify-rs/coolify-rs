//! `App\Jobs\RefreshCandidatesJob`: looks for newer versions of a scanned image after the scan,
//! so deploys never wait for it. The job never sleeps on the scan lock of the server: when it is
//! busy (or a deploy gate waits for it), the job runs again later.

use std::time::Duration;

use coolify_db::image_scans::{self, ImageScanRow};
use serde_json::Value;

use super::refresh::{self, MAX_CANDIDATE_SCANS, RefreshError};
use crate::Jobs;
use crate::image_scan::scan_image::MAX_SCANNER_STEPS_SECONDS;
use crate::image_scan::{MAX_ATTEMPTS, cache, events, lock_retry_delay};

/// `$timeout` and `$uniqueFor`: two candidate scans and the scan of the current base image at the
/// highest scanner limits, plus the registry calls.
pub const JOB_SECONDS: u64 =
    ((MAX_CANDIDATE_SCANS as i64 + 1) * MAX_SCANNER_STEPS_SECONDS + 1800) as u64;

/// The key of the `ShouldBeUnique` lock of the job (`uniqueId()` is the scan id).
fn unique_key(scan_id: i64) -> String {
    format!("image-scan-refresh-candidates-job:{scan_id}")
}

/// One attempt of the job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Attempt {
    Done,
    /// The scan lock was busy: run again after this many seconds.
    Release(u64),
}

/// What an attempt does after `RefreshCandidates::run()`: the busy lock of an attempt that may
/// retry releases the job, anything else ends it. An error that is not a busy lock fails it.
fn after_run(result: Result<(), RefreshError>, attempt: u32) -> Result<Attempt, String> {
    match result {
        Ok(()) => Ok(Attempt::Done),
        Err(RefreshError::LockBusy) => Ok(Attempt::Release(lock_retry_delay(attempt))),
        Err(RefreshError::Other(e)) => Err(e),
    }
}

/// `handle()`.
async fn handle(jobs: &Jobs, scan: &ImageScanRow, attempt: u32) -> Result<Attempt, String> {
    let fresh = image_scans::find_scan(&jobs.0.db, scan.id)
        .await
        .map_err(|e| e.to_string())?;
    // The deploy gate or the daily rescan checked it meanwhile.
    let Some(fresh) = fresh else {
        refresh::forget_queued(scan);
        return Ok(Attempt::Done);
    };
    if refresh::is_fresh(jobs, &fresh).await? {
        refresh::forget_queued(scan);
        return Ok(Attempt::Done);
    }
    let outcome = after_run(
        refresh::run(jobs, &fresh, 0, attempt < MAX_ATTEMPTS).await,
        attempt,
    )?;
    if outcome == Attempt::Done {
        refresh::forget_queued(&fresh);
    }
    Ok(outcome)
}

/// `failed()`: the check stored no result, so an alert that waits for it is sent now
/// (spec 15.8).
async fn failed(jobs: &Jobs, scan: &ImageScanRow) {
    refresh::forget_queued(scan);
    let db = &jobs.0.db;
    let Ok(Some(fresh)) = image_scans::find_scan(db, scan.id).await else {
        return;
    };
    refresh::reevaluate(jobs, &fresh, true).await;
    if let Ok(Some(owner)) =
        image_scans::find_owner(db, &fresh.resource_type, fresh.resource_id).await
    {
        events::dispatch(owner.team_id, &owner.uuid, &fresh.uuid, true);
    }
}

/// `RefreshCandidatesJob::queueCheck($scan)`: queues the check and marks it as queued
/// (`RefreshCandidates::isQueued()`). One job per scan at a time.
pub fn queue_check(jobs: &Jobs, scan: &ImageScanRow) {
    refresh::mark_queued(scan, JOB_SECONDS);
    if !cache::add(
        &unique_key(scan.id),
        Value::Bool(true),
        Duration::from_secs(JOB_SECONDS),
    ) {
        return;
    }
    let jobs = jobs.clone();
    let scan = scan.clone();
    tokio::spawn(async move {
        let mut attempt = 1;
        loop {
            let run = tokio::time::timeout(
                Duration::from_secs(JOB_SECONDS),
                handle(&jobs, &scan, attempt),
            )
            .await;
            match run {
                Ok(Ok(Attempt::Done)) => break,
                Ok(Ok(Attempt::Release(delay))) if attempt < MAX_ATTEMPTS => {
                    tokio::time::sleep(Duration::from_secs(delay)).await;
                    attempt += 1;
                }
                Ok(Ok(Attempt::Release(_))) => {
                    failed(&jobs, &scan).await;
                    break;
                }
                Ok(Err(e)) => {
                    tracing::warn!(scan_id = scan.id, error = %e, "RefreshCandidatesJob failed");
                    failed(&jobs, &scan).await;
                    break;
                }
                Err(_) => {
                    tracing::warn!(scan_id = scan.id, "RefreshCandidatesJob timed out");
                    failed(&jobs, &scan).await;
                    break;
                }
            }
        }
        cache::forget(&unique_key(scan.id));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_job_budget_matches_upstream() {
        assert_eq!(JOB_SECONDS, (3 * MAX_SCANNER_STEPS_SECONDS + 1800) as u64);
    }

    #[test]
    fn a_busy_lock_releases_the_job() {
        assert_eq!(after_run(Ok(()), 1), Ok(Attempt::Done));
        assert_eq!(
            after_run(Err(RefreshError::LockBusy), 1),
            Ok(Attempt::Release(60))
        );
        assert_eq!(
            after_run(Err(RefreshError::LockBusy), 2),
            Ok(Attempt::Release(120))
        );
        assert_eq!(
            after_run(Err(RefreshError::LockBusy), 7),
            Ok(Attempt::Release(300))
        );
        assert_eq!(
            after_run(Err(RefreshError::Other("boom".into())), 1),
            Err("boom".to_owned())
        );
    }
}
