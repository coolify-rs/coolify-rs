//! `App\Jobs\PrepareImageScannerJob` and `App\Actions\ImageScan\SetServerImageScanning`.
//!
//! The job pulls the pinned scanner images on a server and updates the Grype database. The
//! progress is kept in the cache (`image-scan-prepare:{server_id}`: pulling, updating_database,
//! ready, failed) and sent as `ImageScanUpdated` with the scan UUID `prepare`. All steps run
//! under the scan lock of the server, so a deletion of the scanner data cannot run at the same
//! time; when the lock is busy, the job runs again later instead of waiting for it.

use std::time::Duration;

use chrono::{DateTime, Utc};
use coolify_db::image_scans;
use serde_json::Value;

use super::commands::{self, ParsedOutput, ScannerCommands};
use super::error::ImageScanningRefused;
use super::{MAX_ATTEMPTS, ScanServer, cache, events, lock_retry_delay, new_run_id, scan_image};
use crate::Jobs;

pub const SCAN_UUID: &str = "prepare";
pub const STATE_PULLING: &str = "pulling";
pub const STATE_UPDATING_DATABASE: &str = "updating_database";
pub const STATE_READY: &str = "ready";
pub const STATE_FAILED: &str = "failed";
pub const FAILED_PULLING: &str = "pulling";
pub const FAILED_SETUP: &str = "setup";
pub const FAILED_CONNECTION: &str = "connection";

/// `$timeout` and `$uniqueFor` of the job.
const JOB_TIMEOUT: Duration = Duration::from_secs(3600);
const TIMEOUT_PREPARE: i64 = 1900;
const TIMEOUT_DB_UPDATE: i64 = 400;
const TIMEOUT_DB_STATUS: i64 = 120;
const HOUR: Duration = Duration::from_secs(3600);
const WEEK: Duration = Duration::from_secs(7 * 86400);

/// `PrepareImageScannerJob::stateCacheKey($server)`.
pub fn state_cache_key(server_id: i64) -> String {
    format!("image-scan-prepare:{server_id}")
}

/// `PrepareImageScannerJob::builtAtCacheKey($server)`.
pub fn built_at_cache_key(server_id: i64) -> String {
    format!("{}:built-at", state_cache_key(server_id))
}

/// `PrepareImageScannerJob::diskUsedCacheKey($server)`.
pub fn disk_used_cache_key(server_id: i64) -> String {
    format!("{}:disk-used", state_cache_key(server_id))
}

/// `PrepareImageScannerJob::failedStepCacheKey($server)`.
pub fn failed_step_cache_key(server_id: i64) -> String {
    format!("{}:failed-step", state_cache_key(server_id))
}

fn cached_text(key: &str) -> Option<String> {
    cache::get(key).and_then(|v| v.as_str().map(str::to_owned))
}

/// The state of the scanner of a server: pulling, updating_database, ready or failed. `None`
/// when nothing is known (never prepared, or the cache was lost).
pub fn state(server_id: i64) -> Option<String> {
    cached_text(&state_cache_key(server_id))
}

/// The step a failed preparation stopped at: pulling, updating_database, setup or connection.
pub fn failed_step(server_id: i64) -> Option<String> {
    cached_text(&failed_step_cache_key(server_id))
}

/// The build time of the vulnerability database on the server (`2026-11-09T04:10:11Z`).
pub fn database_built_at(server_id: i64) -> Option<String> {
    cached_text(&built_at_cache_key(server_id))
}

/// `PrepareImageScannerJob::diskUsedBytes($server)`.
pub fn disk_used_bytes(server_id: i64) -> Option<i64> {
    cache::get(&disk_used_cache_key(server_id)).and_then(|v| v.as_i64())
}

/// `PrepareImageScannerJob::forget($server)`.
pub fn forget(server_id: i64) {
    cache::forget(&state_cache_key(server_id));
    cache::forget(&failed_step_cache_key(server_id));
    cache::forget(&built_at_cache_key(server_id));
    cache::forget(&disk_used_cache_key(server_id));
}

/// What `setState()` writes: the failed step to store (`Some(Some(step))`), to forget
/// (`Some(None)`) or to keep (`None`), and the lifetime of the state.
fn state_change(
    state: &str,
    previous: Option<&str>,
    failed_step: Option<&str>,
) -> (Option<Option<String>>, Duration) {
    let is_running = |s: &str| s == STATE_PULLING || s == STATE_UPDATING_DATABASE;
    let running = is_running(state);
    let failed_step = failed_step.or(previous.filter(|p| is_running(p)));
    let step = match failed_step {
        Some(step) if state == STATE_FAILED => Some(Some(step.to_owned())),
        _ if running => Some(None),
        _ => None,
    };
    (step, if running { HOUR } else { WEEK })
}

/// `setState()`. Without a failed step, a failure is put on the running step.
fn set_state(server: &ScanServer, state: &str, failed_step: Option<&str>) {
    let id = server.id();
    let previous = self::state(id);
    let (step, ttl) = state_change(state, previous.as_deref(), failed_step);
    match step {
        Some(Some(step)) => cache::put(&failed_step_cache_key(id), Value::String(step), WEEK),
        Some(None) => cache::forget(&failed_step_cache_key(id)),
        None => {}
    }
    cache::put(&state_cache_key(id), Value::String(state.to_owned()), ttl);
    events::dispatch(Some(server.team_id()), server.uuid(), SCAN_UUID, true);
}

async fn run(server: &ScanServer, list: &[String], timeout: i64) -> ParsedOutput {
    commands::parse_output(server.run(list, timeout).await.as_deref())
}

/// `isStillEnabled()`: the scanner data can be deleted and scanning turned off while the job
/// waits or runs.
async fn is_still_enabled(server: &ScanServer) -> Result<bool, String> {
    let enabled = image_scans::server_scan_settings(server.db(), server.id())
        .await
        .map_err(|e| e.to_string())?
        .is_some_and(|s| s.is_image_scan_enabled);
    if !enabled {
        forget(server.id());
    }
    Ok(enabled)
}

/// One attempt of the job.
enum Attempt {
    Done,
    /// The scan lock was busy: run again after this many seconds.
    Release(u64),
}

/// `handle()`.
async fn handle(jobs: &Jobs, server_id: i64, attempt: u32) -> Attempt {
    let Ok(server) = ScanServer::load(jobs, server_id).await else {
        return Attempt::Done;
    };
    if server.is_transferred_away() || !server.is_scan_enabled() {
        return Attempt::Done;
    }
    match prepare(&server, attempt).await {
        Ok(outcome) => outcome,
        Err(e) => {
            tracing::error!(server_id, error = %e, "PrepareImageScannerJob failed");
            set_state(&server, STATE_FAILED, Some(FAILED_CONNECTION));
            Attempt::Done
        }
    }
}

/// `prepare()`.
async fn prepare(server: &ScanServer, attempt: u32) -> Result<Attempt, String> {
    let commands = server.commands(&new_run_id()).map_err(|e| e.to_string())?;
    set_state(server, STATE_PULLING, None);
    let lock = scan_image::server_lock(server);
    if !scan_image::acquire_server_lock(&lock, server.id(), 0).await {
        if attempt >= MAX_ATTEMPTS {
            set_state(server, STATE_FAILED, None);
            return Ok(Attempt::Done);
        }
        return Ok(Attempt::Release(lock_retry_delay(attempt)));
    }
    let result = prepare_locked(server, &commands).await;
    lock.release();
    result.map(|()| Attempt::Done)
}

/// `prepareLocked()`: runs under the server lock, so a scan or a deletion of the scanner data
/// cannot run at the same time.
async fn prepare_locked(server: &ScanServer, commands: &ScannerCommands) -> Result<(), String> {
    // The scanner data can be deleted and scanning turned off before this job got the lock.
    if !is_still_enabled(server).await? {
        return Ok(());
    }
    let pulled = run(server, &commands.prepare(), TIMEOUT_PREPARE).await;
    if pulled.exit_codes.is_empty() {
        set_state(server, STATE_FAILED, Some(FAILED_CONNECTION));
        return Ok(());
    }
    for step in ["pull-syft", "pull-grype", "pull-regctl", "pull-init"] {
        if pulled.exit_code(step) != Some(0) {
            set_state(server, STATE_FAILED, Some(FAILED_PULLING));
            return Ok(());
        }
    }
    if pulled.exit_code("db-volume") != Some(0) {
        set_state(server, STATE_FAILED, Some(FAILED_SETUP));
        return Ok(());
    }
    if !is_still_enabled(server).await? {
        return Ok(());
    }
    set_state(server, STATE_UPDATING_DATABASE, None);
    let updated = run(server, &commands.db_update(), TIMEOUT_DB_UPDATE).await;
    if updated.exit_code("db-update") != Some(0) {
        set_state(server, STATE_FAILED, None);
        return Ok(());
    }
    if !is_still_enabled(server).await? {
        return Ok(());
    }
    store_details(server, commands).await;
    set_state(server, STATE_READY, None);
    Ok(())
}

/// The `built` time of a valid `grype db status -o json`, as `toIso8601ZuluString()`.
fn built_zulu(status: &ParsedOutput) -> Option<String> {
    if status.exit_code("db-status") != Some(0) {
        return None;
    }
    let report: Value = serde_json::from_str(&status.payload).ok()?;
    if report.get("valid") != Some(&Value::Bool(true)) {
        return None;
    }
    let built = DateTime::parse_from_rfc3339(report.get("built")?.as_str()?).ok()?;
    Some(
        built
            .with_timezone(&Utc)
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string(),
    )
}

/// `PrepareImageScannerJob::storeDetails($server, $commands)`: caches the database build date
/// and the disk use of the scanner data that the server page shows. A valid database and all
/// scanner images on the server prove that the scanner is ready, so this also marks it as ready,
/// unless a preparation is running and sets the state itself.
pub async fn store_details(server: &ScanServer, commands: &ScannerCommands) {
    let id = server.id();
    cache::forget(&built_at_cache_key(id));
    cache::forget(&disk_used_cache_key(id));

    let status = run(server, &commands.db_status(), TIMEOUT_DB_STATUS).await;
    if let Some(built) = built_zulu(&status) {
        cache::put(&built_at_cache_key(id), Value::String(built), WEEK);
        let preparing =
            state(id).is_some_and(|s| s == STATE_PULLING || s == STATE_UPDATING_DATABASE);
        if !preparing
            && run(server, &commands.scanner_images(), TIMEOUT_DB_STATUS)
                .await
                .exit_code("scanner-images")
                == Some(0)
        {
            cache::forget(&failed_step_cache_key(id));
            cache::put(
                &state_cache_key(id),
                Value::String(STATE_READY.to_owned()),
                WEEK,
            );
            events::dispatch(Some(server.team_id()), server.uuid(), SCAN_UUID, true);
        }
    }

    let usage = run(server, &commands.disk_usage(), TIMEOUT_DB_STATUS).await;
    let (database, sboms) = (usage.size("disk-db"), usage.size("disk-sboms"));
    if database.is_some() || sboms.is_some() {
        cache::put(
            &disk_used_cache_key(id),
            Value::from(database.unwrap_or(0) + sboms.unwrap_or(0)),
            WEEK,
        );
    }
}

/// The key of the `ShouldBeUnique` lock of the job.
fn unique_key(server_id: i64) -> String {
    format!("image-scan-prepare-job:{server_id}")
}

impl Jobs {
    /// `PrepareImageScannerJob::dispatch($server)`: one job per server at a time. A job that
    /// finds the scan lock busy runs again later, up to [`MAX_ATTEMPTS`] times.
    pub fn dispatch_prepare_image_scanner(&self, server_id: i64) {
        if !cache::add(&unique_key(server_id), Value::Bool(true), JOB_TIMEOUT) {
            return;
        }
        let jobs = self.clone();
        tokio::spawn(async move {
            let mut attempt = 1;
            loop {
                match tokio::time::timeout(JOB_TIMEOUT, handle(&jobs, server_id, attempt)).await {
                    Ok(Attempt::Done) => break,
                    Ok(Attempt::Release(delay)) => {
                        tokio::time::sleep(Duration::from_secs(delay)).await;
                        attempt += 1;
                    }
                    Err(_) => {
                        // `failed()`.
                        if let Ok(server) = ScanServer::load(&jobs, server_id).await {
                            set_state(&server, STATE_FAILED, None);
                        }
                        break;
                    }
                }
            }
            cache::forget(&unique_key(server_id));
        });
    }
}

/// Why [`set_server_image_scanning`] did not change the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetScanningError {
    /// The message is shown to the user.
    Refused(ImageScanningRefused),
    Other(String),
}

impl std::fmt::Display for SetScanningError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SetScanningError::Refused(e) => e.fmt(f),
            SetScanningError::Other(e) => f.write_str(e),
        }
    }
}

impl std::error::Error for SetScanningError {}

/// `SetServerImageScanning::handle($server, $enabled)`: turns image scanning of a server on or
/// off. Turning it on gets the scanner on the server. Refused when another instance manages the
/// server.
pub async fn set_server_image_scanning(
    jobs: &Jobs,
    server: &ScanServer,
    enabled: bool,
) -> Result<(), SetScanningError> {
    let save = |enabled: bool| async move {
        image_scans::set_server_scan_enabled(server.db(), server.id(), enabled, Jobs::now())
            .await
            .map(|_| ())
            .map_err(|e| SetScanningError::Other(e.to_string()))
    };
    if !enabled {
        save(false).await?;
        forget(server.id());
        return Ok(());
    }
    ensure_managed_here(server).map_err(SetScanningError::Refused)?;
    save(true).await?;
    prepare_scanner(jobs, server.id());
    Ok(())
}

/// `SetServerImageScanning::prepareScanner($server)`: gets the scanner on the server again, for
/// a server that scans images.
pub fn prepare_scanner(jobs: &Jobs, server_id: i64) {
    cache::put(
        &state_cache_key(server_id),
        Value::String(STATE_PULLING.to_owned()),
        HOUR,
    );
    jobs.dispatch_prepare_image_scanner(server_id);
}

/// `SetServerImageScanning::ensureManagedHere($server)`.
pub fn ensure_managed_here(server: &ScanServer) -> Result<(), ImageScanningRefused> {
    if server.is_transferred_away() {
        return Err(ImageScanningRefused(
            "This server is managed by another Coolify instance.".to_owned(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_keys() {
        assert_eq!(state_cache_key(7), "image-scan-prepare:7");
        assert_eq!(built_at_cache_key(7), "image-scan-prepare:7:built-at");
        assert_eq!(disk_used_cache_key(7), "image-scan-prepare:7:disk-used");
        assert_eq!(failed_step_cache_key(7), "image-scan-prepare:7:failed-step");
    }

    #[test]
    fn a_failure_is_put_on_the_running_step() {
        // A running state forgets the failed step and lasts an hour.
        assert_eq!(
            state_change(STATE_PULLING, Some(STATE_FAILED), None),
            (Some(None), HOUR)
        );
        assert_eq!(
            state_change(STATE_UPDATING_DATABASE, Some(STATE_PULLING), None),
            (Some(None), HOUR)
        );
        // A failure without a step is put on the step that ran.
        assert_eq!(
            state_change(STATE_FAILED, Some(STATE_UPDATING_DATABASE), None),
            (Some(Some("updating_database".to_owned())), WEEK)
        );
        assert_eq!(
            state_change(STATE_FAILED, Some(STATE_PULLING), Some(FAILED_SETUP)),
            (Some(Some("setup".to_owned())), WEEK)
        );
        // Nothing ran: the stored step stays.
        assert_eq!(
            state_change(STATE_FAILED, Some(STATE_READY), None),
            (None, WEEK)
        );
        assert_eq!(state_change(STATE_FAILED, None, None), (None, WEEK));
        assert_eq!(
            state_change(STATE_READY, Some(STATE_UPDATING_DATABASE), None),
            (None, WEEK)
        );
    }

    #[test]
    fn forget_clears_every_key() {
        let id = -4711;
        cache::put(
            &state_cache_key(id),
            Value::String(STATE_READY.into()),
            WEEK,
        );
        cache::put(
            &failed_step_cache_key(id),
            Value::String(FAILED_SETUP.into()),
            WEEK,
        );
        cache::put(
            &built_at_cache_key(id),
            Value::String("2026-11-09T04:10:11Z".into()),
            WEEK,
        );
        cache::put(&disk_used_cache_key(id), Value::from(1024), WEEK);
        assert_eq!(state(id).as_deref(), Some("ready"));
        assert_eq!(failed_step(id).as_deref(), Some("setup"));
        assert_eq!(
            database_built_at(id).as_deref(),
            Some("2026-11-09T04:10:11Z")
        );
        assert_eq!(disk_used_bytes(id), Some(1024));
        forget(id);
        assert_eq!(state(id), None);
        assert_eq!(failed_step(id), None);
        assert_eq!(database_built_at(id), None);
        assert_eq!(disk_used_bytes(id), None);
    }

    #[test]
    fn the_database_build_time_is_cached_in_utc() {
        let status = |payload: &str| {
            commands::parse_output(Some(&format!(
                "coolify-scan-exit db-status 0 80\ncoolify-scan-payload\n{payload}"
            )))
        };
        assert_eq!(
            built_zulu(&status(
                r#"{"built":"2026-11-09T06:10:11+02:00","valid":true}"#
            ))
            .as_deref(),
            Some("2026-11-09T04:10:11Z")
        );
        assert_eq!(
            built_zulu(&status(r#"{"built":"2026-11-09T04:10:11Z","valid":false}"#)),
            None
        );
        assert_eq!(built_zulu(&status("{}")), None);
        let failed = commands::parse_output(Some(
            "coolify-scan-exit db-status 1 0\ncoolify-scan-payload\n{\"built\":\"2026-11-09T04:10:11Z\",\"valid\":true}",
        ));
        assert_eq!(built_zulu(&failed), None);
    }
}
