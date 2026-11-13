//! `App\Jobs\ScanResourceImagesJob` and `App\Jobs\ImageRescanJob`.
//!
//! `ScanResourceImagesJob` scans the images of a resource one at a time, after it started, so the
//! scan is not in the critical path of a deploy (spec 4.4). `ImageRescanJob` is the daily rescan
//! of the images that run on a server (spec 4.5): it updates the Grype database, runs Grype once
//! per running image ID, updates the scan rows of this server only, and applies the retention of
//! spec 4.3.
//!
//! A queued job is a tokio task here. `release($delay)` is a sleep before the next attempt of the
//! same task, and what upstream keeps in the cache per job UUID (the targets that are done, the
//! progress of a rescan) is kept under the same keys with a UUID of the task.

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;
use std::time::Duration;

use chrono::{NaiveDate, Utc};
use coolify_db::Db;
use coolify_db::image_scans::jobs::{
    self as job_queries, JobDeployment, RetentionApplication, RetentionScan,
};
use coolify_db::image_scans::{
    self, APPLICATION_MORPH, ImageScanRow, SERVICE_APPLICATION_MORPH, SERVICE_DATABASE_MORPH,
    STATUS_FAILED, ScanOwner,
};
use regex::Regex;
use serde_json::{Value, json};

use super::candidates::{self, RefreshError};
use super::commands::{OUTPUT_LIMIT, ScannerCommands, parse_output};
use super::error::ErrorCode;
use super::rules::IgnoreMatcher;
use super::scan_image::{self, ScanDeployment, ScanOptions};
use super::{
    MAX_ATTEMPTS, ScanServer, cache, events, lock_retry_delay, new_run_id, policy, prepare,
    resource_scans, sbom_document,
};
use crate::Jobs;

fn err(e: coolify_db::DbError) -> String {
    e.to_string()
}

/// One attempt of a job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Attempt {
    Done,
    /// `$this->release($seconds)`: run again after this many seconds.
    Release(u64),
}

/// `(int) $value` of a string: the leading integer, 0 without one.
fn php_intval(value: &str) -> i64 {
    let text = value.trim_start_matches([' ', '\t', '\n', '\r', '\x0B', '\x0C']);
    let (negative, digits) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text.strip_prefix('+').unwrap_or(text)),
    };
    let end = digits
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(digits.len());
    let number = digits[..end]
        .parse::<i64>()
        .unwrap_or(if end == 0 { 0 } else { i64::MAX });
    if negative { -number } else { number }
}

/// `(int) $value` of a decoded JSON value.
fn php_int(value: Option<&Value>) -> i64 {
    match value {
        Some(Value::Number(n)) => n
            .as_i64()
            .or_else(|| n.as_f64().map(|f| f as i64))
            .unwrap_or(0),
        Some(Value::String(s)) => php_intval(s),
        Some(Value::Bool(true)) => 1,
        _ => 0,
    }
}

/// `(bool) $value` of a decoded JSON value.
fn php_bool(value: Option<&Value>) -> bool {
    match value {
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty() && s != "0",
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
        _ => false,
    }
}

// ---------------------------------------------------------------------------------------------
// ScanResourceImagesJob
// ---------------------------------------------------------------------------------------------

/// `ScanResourceImagesJob::MAX_DERIVED_IMAGES`: images scanned when the job derives them from the
/// resource.
pub const MAX_DERIVED_IMAGES: usize = 5;
/// `ScanResourceImagesJob::START_DELAY_SECONDS`: a service or database scan waits this long after
/// the start was queued, so the image is pulled first.
pub const START_DELAY_SECONDS: u64 = 30;
/// `ScanResourceImagesJob::START_MAX_ATTEMPTS`: attempts of a job after a start: later ones wait
/// for an image that is still being pulled.
pub const START_MAX_ATTEMPTS: u32 = 3;
/// `ScanResourceImagesJob::START_RETRY_DELAY_SECONDS`.
pub const START_RETRY_DELAY_SECONDS: u64 = 120;
/// `ScanResourceImagesJob::QUEUED_STAGE`: progress stage of a scan that "Scan now" queued and that
/// has not started yet.
pub const QUEUED_STAGE: &str = "queued";

/// The arguments of `ScanResourceImagesJob`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanResourceImages {
    /// `$resource->getMorphClass()`.
    pub resource_type: String,
    /// `$resource->getKey()`.
    pub resource_id: i64,
    pub deployment_queue_id: Option<i64>,
    /// Empty: the images of the resource ([`image_refs_for`]).
    pub image_refs: Vec<String>,
    /// A job after a service or database start retries an image that is not pulled yet.
    pub retry_when_image_missing: bool,
    /// The base image of the Dockerfile build of the application.
    pub base_image_ref: Option<String>,
    /// "Scan now": pull an image that is not on the server.
    pub pull_when_missing: bool,
}

impl ScanResourceImages {
    /// `new ScanResourceImagesJob($resource)`.
    pub fn new(resource: &ScanOwner) -> Self {
        Self {
            resource_type: resource.morph.to_owned(),
            resource_id: resource.id,
            deployment_queue_id: None,
            image_refs: Vec::new(),
            retry_when_image_missing: false,
            base_image_ref: None,
            pull_when_missing: false,
        }
    }

    /// `$this->resource` after `SerializesModels` restored it: `None` when it is gone.
    async fn resource(&self, db: &Db) -> Result<Option<ScanOwner>, String> {
        image_scans::find_owner(db, &self.resource_type, self.resource_id)
            .await
            .map_err(err)
    }

    /// `deployment()`.
    async fn deployment(
        &self,
        db: &Db,
        resource: &ScanOwner,
    ) -> Result<Option<JobDeployment>, String> {
        match self.deployment_queue_id {
            Some(id) if resource.is_application() => {
                job_queries::job_deployment(db, id, resource.id)
                    .await
                    .map_err(err)
            }
            _ => Ok(None),
        }
    }

    /// `server()`: the server of the deployment (an additional server runs its own copy of the
    /// image), else the server the scan policy of the resource uses.
    async fn server(
        jobs: &Jobs,
        resource: &ScanOwner,
        deployment: Option<&JobDeployment>,
    ) -> Option<ScanServer> {
        if let Some(server_id) = deployment.and_then(|d| d.server_id)
            && let Ok(server) = ScanServer::load(jobs, server_id).await
        {
            return Some(server);
        }
        ScanServer::load(jobs, resource.main_server_id?).await.ok()
    }

    /// `$timeout`: the images of the job times the limits of the scan server, not the highest
    /// ones, so a service with many images stays below the limit of a queued job.
    async fn timeout(&self, jobs: &Jobs) -> Duration {
        let db = &jobs.0.db;
        let resource = self.resource(db).await.ok().flatten();
        let mut images = self.image_refs.len();
        let mut per_image = scan_image::MAX_LOCK_TTL;
        if let Some(resource) = &resource {
            if images == 0 {
                images = derived_image_count(db, resource).await;
            }
            let deployment = self.deployment(db, resource).await.ok().flatten();
            if let Some(server) = Self::server(jobs, resource, deployment.as_ref()).await {
                per_image = scan_image::lock_ttl(&server.host());
            }
        } else if images == 0 {
            images = MAX_DERIVED_IMAGES;
        }
        Duration::from_secs(
            u64::try_from(images)
                .unwrap_or(u64::MAX)
                .saturating_mul(per_image.unsigned_abs()),
        )
    }

    /// `scanTargets()`: each image with the resource its result belongs to. A service keeps
    /// results per application and database.
    async fn scan_targets(
        &self,
        db: &Db,
        resource: &ScanOwner,
    ) -> Result<Vec<(ScanOwner, String)>, String> {
        let targets: Vec<(ScanOwner, String)> = if !self.image_refs.is_empty() {
            self.image_refs
                .iter()
                .map(|image_ref| (resource.clone(), image_ref.clone()))
                .collect()
        } else if resource.is_application() {
            image_refs_for(db, resource)
                .await?
                .into_iter()
                .map(|image_ref| (resource.clone(), image_ref))
                .collect()
        } else {
            resource_scans::scan_targets(db, resource).await?
        };
        Ok(unique_targets(targets))
    }

    /// `handle()`.
    async fn handle(&self, jobs: &Jobs, job_uuid: &str, attempt: u32) -> Result<Attempt, String> {
        let db = &jobs.0.db;
        // `$deleteWhenMissingModels`: a resource deleted before the delayed scan runs needs no scan.
        let Some(resource) = self.resource(db).await? else {
            return Ok(Attempt::Done);
        };
        let deployment = self.deployment(db, &resource).await?;
        let Some(server) = Self::server(jobs, &resource, deployment.as_ref()).await else {
            self.clear_queued_marks(jobs, &resource).await;
            return Ok(Attempt::Done);
        };

        let mut done = done_targets(job_uuid);
        let mut missing = false;
        let last_attempt = attempt >= MAX_ATTEMPTS;
        for (owner, image_ref) in self.scan_targets(db, &resource).await? {
            let key = target_key(&owner, &image_ref);
            if done.contains(&key) {
                continue;
            }

            let is_job_resource = owner.morph == resource.morph && owner.id == resource.id;
            let base_image_ref = if is_job_resource && owner.is_application() {
                self.base_image_ref.clone()
            } else {
                None
            };
            // Never sleeps on the server lock: a deploy gate may need it. A busy lock retries the
            // job later.
            let scan = scan_image::handle(
                jobs,
                &server,
                &owner,
                &image_ref,
                ScanOptions {
                    deployment: deployment.map(|d| ScanDeployment {
                        id: d.id,
                        pull_request_id: d.pull_request_id,
                    }),
                    lock_wait_seconds: 0,
                    base_image_ref,
                    queue_candidate_check: true,
                    retry_when_busy: !last_attempt,
                    pull_when_missing: self.pull_when_missing,
                },
            )
            .await?;

            if !last_attempt && scan_image::lock_was_busy(&scan) {
                remember_done_targets(job_uuid, &done);
                return Ok(Attempt::Release(lock_retry_delay(attempt)));
            }

            if should_retry(self.retry_when_image_missing, attempt, &scan) {
                // Only the last attempt leaves the error visible.
                cache::forget(&scan_image::last_error_cache_key(&owner));
                missing = true;
            } else {
                done.push(key);
            }
        }

        if missing {
            remember_done_targets(job_uuid, &done);
            return Ok(Attempt::Release(START_RETRY_DELAY_SECONDS));
        }

        self.clear_queued_marks(jobs, &resource).await;
        Ok(Attempt::Done)
    }

    /// `failed()`.
    async fn failed(&self, jobs: &Jobs) {
        if let Ok(Some(resource)) = self.resource(&jobs.0.db).await {
            self.clear_queued_marks(jobs, &resource).await;
        }
    }

    /// `clearQueuedMarks()`: removes the queued marks that no scan replaced (for example a scan
    /// that could not start), and tells open pages.
    async fn clear_queued_marks(&self, jobs: &Jobs, resource: &ScanOwner) {
        match marked_owners(&jobs.0.db, resource, &self.image_refs).await {
            Ok(owners) => clear_marks(&owners),
            Err(e) => {
                tracing::error!(error = %e, "ScanResourceImagesJob could not clear its queued marks");
            }
        }
    }
}

/// `derivedImageCount()`.
async fn derived_image_count(db: &Db, resource: &ScanOwner) -> usize {
    if !resource.is_service() {
        return MAX_DERIVED_IMAGES;
    }
    let parts = job_queries::service_part_count(db, resource.id)
        .await
        .unwrap_or(0);
    MAX_DERIVED_IMAGES.max(usize::try_from(parts).unwrap_or(0))
}

/// `ScanResourceImagesJob::imageRefsFor($resource)`: the images a resource runs. Applications:
/// the images of their latest completed scans. Services: the images of their applications and
/// databases. Standalone databases: their image.
pub async fn image_refs_for(db: &Db, resource: &ScanOwner) -> Result<Vec<String>, String> {
    if resource.is_application() {
        let refs = job_queries::completed_image_refs(db, resource.morph, resource.id)
            .await
            .map_err(err)?;
        return Ok(derived_image_refs(refs));
    }
    Ok(resource_scans::scan_targets(db, resource)
        .await?
        .into_iter()
        .map(|(_, image_ref)| image_ref)
        .collect())
}

/// `->unique()->take(self::MAX_DERIVED_IMAGES)` of the image refs of the completed scans.
fn derived_image_refs(refs: Vec<String>) -> Vec<String> {
    let mut unique: Vec<String> = Vec::new();
    for image_ref in refs {
        if !unique.contains(&image_ref) {
            unique.push(image_ref);
        }
    }
    unique.truncate(MAX_DERIVED_IMAGES);
    unique
}

/// `targetKey()`.
fn target_key(resource: &ScanOwner, image_ref: &str) -> String {
    format!("{}:{}:{image_ref}", resource.morph, resource.id)
}

/// The end of `scanTargets()`: each target once.
fn unique_targets(targets: Vec<(ScanOwner, String)>) -> Vec<(ScanOwner, String)> {
    let mut seen = HashSet::new();
    targets
        .into_iter()
        .filter(|(resource, image_ref)| seen.insert(target_key(resource, image_ref)))
        .collect()
}

/// `shouldRetry()`: a job after a start waits for an image that is still being pulled.
fn should_retry(retry_when_image_missing: bool, attempt: u32, scan: &ImageScanRow) -> bool {
    retry_when_image_missing
        && attempt < START_MAX_ATTEMPTS.min(MAX_ATTEMPTS)
        && scan.status == STATUS_FAILED
        && scan.error_code.as_deref() == Some(ErrorCode::ImageMissing.value())
}

fn done_cache_key(job_uuid: &str) -> String {
    format!("image-scan:start-done:{job_uuid}")
}

/// `doneTargets()`: the targets of earlier attempts of the job.
fn done_targets(job_uuid: &str) -> Vec<String> {
    match cache::get(&done_cache_key(job_uuid)) {
        Some(Value::Array(keys)) => keys
            .iter()
            .filter_map(|key| key.as_str().map(str::to_owned))
            .collect(),
        _ => Vec::new(),
    }
}

/// `rememberDoneTargets()`.
fn remember_done_targets(job_uuid: &str, done: &[String]) {
    cache::put(
        &done_cache_key(job_uuid),
        json!(done),
        Duration::from_secs(2 * 3600),
    );
}

/// `markedOwners()`: the owners that [`mark_queued`] marks: the application, or the scanned
/// applications and databases of a service.
async fn marked_owners(
    db: &Db,
    resource: &ScanOwner,
    image_refs: &[String],
) -> Result<Vec<ScanOwner>, String> {
    if resource.is_application() {
        return Ok(if image_refs.is_empty() {
            Vec::new()
        } else {
            vec![resource.clone()]
        });
    }
    Ok(resource_scans::scan_targets(db, resource)
        .await?
        .into_iter()
        .map(|(owner, _)| owner)
        .collect())
}

/// `Carbon::toIso8601String()` of now.
fn now_iso8601() -> String {
    Utc::now().format("%Y-%m-%dT%H:%M:%S+00:00").to_string()
}

/// The cache part of [`mark_queued`].
fn mark_owners_queued(owners: &[ScanOwner]) {
    for owner in owners {
        cache::put(
            &scan_image::progress_cache_key(owner),
            json!({
                "run_id": QUEUED_STAGE,
                "stage": QUEUED_STAGE,
                "started_at": now_iso8601(),
            }),
            Duration::from_secs(scan_image::MAX_LOCK_TTL.unsigned_abs()),
        );
        cache::forget(&scan_image::last_error_cache_key(owner));
    }
}

/// The cache part of `clearQueuedMarks()`.
fn clear_marks(owners: &[ScanOwner]) {
    for owner in owners {
        let key = scan_image::progress_cache_key(owner);
        let queued = cache::get(&key).is_some_and(|progress| {
            progress.get("stage").and_then(Value::as_str) == Some(QUEUED_STAGE)
        });
        if !queued {
            continue;
        }
        cache::forget(&key);
        events::dispatch(owner.team_id, &owner.uuid, QUEUED_STAGE, true);
    }
}

/// `ScanResourceImagesJob::markQueued($resource, $imageRefs)`: shows a scan that "Scan now"
/// queued as running at once: marks each scanned owner as queued and hides its last error. The
/// scan replaces the mark with its progress; the job removes marks that are left when it ends.
pub async fn mark_queued(
    jobs: &Jobs,
    resource: &ScanOwner,
    image_refs: &[String],
) -> Result<(), String> {
    mark_owners_queued(&marked_owners(&jobs.0.db, resource, image_refs).await?);
    Ok(())
}

/// `ScanResourceImagesJob::dispatchAfterStart($resource)`: queues the scan of a service or
/// standalone database that was started. Never blocks or fails the start.
pub async fn dispatch_after_start(jobs: &Jobs, resource: &ScanOwner) {
    match policy::is_scan_on(&jobs.0.db, resource).await {
        Ok(true) => {
            let mut job = ScanResourceImages::new(resource);
            job.retry_when_image_missing = true;
            jobs.dispatch_scan_resource_images(job, START_DELAY_SECONDS);
        }
        Ok(false) => {}
        Err(e) => {
            tracing::error!(error = %e, "ScanResourceImagesJob::dispatchAfterStart failed");
        }
    }
}

impl Jobs {
    /// `ScanResourceImagesJob::dispatch(...)->delay($delay_seconds)`. A busy scan lock runs the
    /// job again later (60, 120, then 300 seconds), up to [`MAX_ATTEMPTS`] attempts; a job after
    /// a start also waits for an image that is still being pulled. An unexpected error fails the
    /// job.
    pub fn dispatch_scan_resource_images(&self, job: ScanResourceImages, delay_seconds: u64) {
        let jobs = self.clone();
        tokio::spawn(async move {
            if delay_seconds > 0 {
                tokio::time::sleep(Duration::from_secs(delay_seconds)).await;
            }
            // `$this->job->uuid()`.
            let job_uuid = sbom_document::uuid_v4();
            let timeout = job.timeout(&jobs).await;
            let mut attempt = 1;
            loop {
                match tokio::time::timeout(timeout, job.handle(&jobs, &job_uuid, attempt)).await {
                    Ok(Ok(Attempt::Done)) => break,
                    Ok(Ok(Attempt::Release(delay))) => {
                        tokio::time::sleep(Duration::from_secs(delay)).await;
                        attempt += 1;
                    }
                    Ok(Err(e)) => {
                        tracing::error!(
                            resource_type = %job.resource_type,
                            resource_id = job.resource_id,
                            error = %e,
                            "ScanResourceImagesJob failed"
                        );
                        job.failed(&jobs).await;
                        break;
                    }
                    Err(_) => {
                        tracing::error!(
                            resource_type = %job.resource_type,
                            resource_id = job.resource_id,
                            "ScanResourceImagesJob timed out"
                        );
                        job.failed(&jobs).await;
                        break;
                    }
                }
            }
            cache::forget(&done_cache_key(&job_uuid));
        });
    }
}

// ---------------------------------------------------------------------------------------------
// ImageRescanJob
// ---------------------------------------------------------------------------------------------

/// `ImageRescanJob::KEEP_LATEST_SCANS`: scans kept per resource besides the running image and the
/// rollback list.
pub const KEEP_LATEST_SCANS: usize = 5;
/// `ImageRescanJob::SBOM_UNUSED_DAYS`.
pub const SBOM_UNUSED_DAYS: i64 = 30;
/// `ImageRescanJob::MAX_SBOM_DELETES`: SBOM files removed per run, so the command line stays
/// short. The rest go the next day.
pub const MAX_SBOM_DELETES: i64 = 500;

const TIMEOUT_DB_UPDATE: i64 = 960;
const TIMEOUT_DEFAULT: i64 = 120;
/// `FAILURE_STATE_DAYS`: the failed-day count of a server is kept this long after the last
/// failure.
const FAILURE_STATE_TTL: Duration = Duration::from_secs(7 * 86400);
/// `$timeout` and `$uniqueFor` of the job.
const RESCAN_TIMEOUT: Duration = Duration::from_secs(36000);
const DAY: Duration = Duration::from_secs(86400);

static IMAGE_ID: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\Asha256:[a-f0-9]{64}\z").unwrap_or_else(|_| unreachable!()));

/// `ImageRescanJob::canRunOn($server)`: scanning is on for the server, and this instance can
/// reach the server.
pub fn can_run_on(server: &ScanServer) -> bool {
    server.is_scan_enabled() && !server.is_transferred_away() && server.srv.info.is_functional()
}

/// `ImageRescanJob::failuresCacheKey($server)`.
fn failures_cache_key(server_id: i64) -> String {
    format!("image-rescan:failures:{server_id}")
}

fn date_string(date: NaiveDate) -> String {
    date.format("%Y-%m-%d").to_string()
}

/// The state that `recordRescanResult()` stores after a failed day: `{days, last, notified}`.
fn next_failure_state(state: Option<&Value>, today: NaiveDate) -> Value {
    let today_text = date_string(today);
    let first = || json!({"days": 1, "last": today_text, "notified": false});
    let Some(Value::Object(state)) = state else {
        return first();
    };
    let last = state.get("last").and_then(Value::as_str);
    if last == Some(today_text.as_str()) {
        return Value::Object(state.clone());
    }
    let yesterday = today.pred_opt().map(date_string);
    if last.is_some() && last == yesterday.as_deref() {
        let mut next = state.clone();
        next.insert(
            "days".to_owned(),
            Value::from(php_int(state.get("days")) + 1),
        );
        next.insert("last".to_owned(), Value::String(today_text.clone()));
        return Value::Object(next);
    }
    first()
}

/// `ImageRescanJob::recordRescanResult($server, $succeeded)`: counts the days in a row on which
/// the rescan of the server failed, for the digest (spec 8). A failed day is one on which the
/// running containers could not be listed or no image could be scanned. Success resets it.
pub fn record_rescan_result(server_id: i64, succeeded: bool) {
    let key = failures_cache_key(server_id);
    if succeeded {
        cache::forget(&key);
        return;
    }
    let state = next_failure_state(cache::get(&key).as_ref(), Utc::now().date_naive());
    cache::put(&key, state, FAILURE_STATE_TTL);
}

/// `ImageRescanJob::failedDays($server)`: days in a row on which the rescan of the server failed,
/// 0 after a success.
pub fn failed_days(server_id: i64) -> i64 {
    match cache::get(&failures_cache_key(server_id)) {
        Some(Value::Object(state)) => php_int(state.get("days")),
        _ => 0,
    }
}

/// `ImageRescanJob::isFailureNotified($server)`.
pub fn is_failure_notified(server_id: i64) -> bool {
    cache::get(&failures_cache_key(server_id)).is_some_and(|state| php_bool(state.get("notified")))
}

/// `ImageRescanJob::markFailureNotified($server)`.
pub fn mark_failure_notified(server_id: i64) {
    let key = failures_cache_key(server_id);
    if let Some(Value::Object(mut state)) = cache::get(&key) {
        state.insert("notified".to_owned(), Value::Bool(true));
        cache::put(&key, Value::Object(state), FAILURE_STATE_TTL);
    }
}

/// `progress()`: what earlier attempts of a rescan did: database updated, image IDs rescanned,
/// and the counts of the result.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Progress {
    database: bool,
    done: Vec<String>,
    scanned: i64,
    completed: i64,
    errors: i64,
}

impl Progress {
    /// `[...$empty, ...$stored]`.
    fn from_value(stored: Option<&Value>) -> Self {
        let Some(Value::Object(stored)) = stored else {
            return Self::default();
        };
        Self {
            database: php_bool(stored.get("database")),
            done: match stored.get("done") {
                Some(Value::Array(done)) => done
                    .iter()
                    .filter_map(|id| id.as_str().map(str::to_owned))
                    .collect(),
                _ => Vec::new(),
            },
            scanned: php_int(stored.get("scanned")),
            completed: php_int(stored.get("completed")),
            errors: php_int(stored.get("errors")),
        }
    }

    fn to_value(&self) -> Value {
        json!({
            "database": self.database,
            "done": self.done,
            "scanned": self.scanned,
            "completed": self.completed,
            "errors": self.errors,
        })
    }

    /// The day counts as a success: an image was scanned, or there was nothing to scan and
    /// nothing failed.
    fn succeeded(&self) -> bool {
        self.completed > 0 || (self.scanned == 0 && self.errors == 0)
    }
}

/// `progressCacheKey()`.
fn progress_cache_key(job_uuid: &str) -> String {
    format!("image-rescan:progress:{job_uuid}")
}

/// `retryLater()`.
fn retry_later(job_uuid: &str, progress: &Progress, attempt: u32) -> Attempt {
    cache::put(
        &progress_cache_key(job_uuid),
        progress.to_value(),
        Duration::from_secs(12 * 3600),
    );
    Attempt::Release(lock_retry_delay(attempt))
}

/// The labels of a container: only string keys and scalar values, as strings.
type Labels = HashMap<String, String>;

/// One running container of `ScannerCommands::runningContainers()`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Container {
    /// The image ID (`sha256:{hex}`).
    id: String,
    /// The image ref of the container, empty when it has none.
    image_ref: String,
    labels: Labels,
}

/// The running containers. `truncated`: the list was cut at the output cap.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Listing {
    containers: Vec<Container>,
    truncated: bool,
}

/// PHP turns an array key that is a canonical decimal integer into an integer
/// (`is_string($key)` is false for it).
fn is_php_int_key(key: &str) -> bool {
    let digits = key.strip_prefix('-').unwrap_or(key);
    !digits.is_empty()
        && digits.bytes().all(|b| b.is_ascii_digit())
        && ((digits == "0" && !key.starts_with('-')) || !digits.starts_with('0'))
        && key.parse::<i64>().is_ok()
}

/// `(string) $value` of a scalar label value; `None` for a value that is not a scalar.
fn scalar_label(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(true) => Some("1".to_owned()),
        Value::Bool(false) => Some(String::new()),
        _ => None,
    }
}

/// The parsing of `runningContainers()`: one JSON line per container. Lines of containers
/// without a valid image ID are skipped.
fn parse_containers(payload: &str, limit: usize) -> Listing {
    let truncated = payload.len() > limit;
    let payload = if truncated {
        // A line break is one byte, so the cut is at a character boundary.
        match payload.as_bytes()[..limit]
            .iter()
            .rposition(|b| *b == b'\n')
        {
            Some(last_newline) => &payload[..last_newline],
            None => "",
        }
    } else {
        payload
    };

    let mut containers = Vec::new();
    for line in payload.split('\n') {
        let Ok(Value::Object(container)) =
            serde_json::from_str::<Value>(coolify_remote::text::php_trim(line))
        else {
            continue;
        };
        let Some(id) = container
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| IMAGE_ID.is_match(id))
        else {
            continue;
        };
        let labels: Labels = match container.get("labels") {
            Some(Value::Object(labels)) => labels
                .iter()
                .filter(|(key, _)| !is_php_int_key(key))
                .filter_map(|(key, value)| Some((key.clone(), scalar_label(value)?)))
                .collect(),
            _ => Labels::new(),
        };
        containers.push(Container {
            id: id.to_owned(),
            image_ref: container
                .get("ref")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            labels,
        });
    }
    Listing {
        containers,
        truncated,
    }
}

/// `runningContainers()`: the running containers, or `None` when they could not be listed.
async fn running_containers(server: &ScanServer, commands: &ScannerCommands) -> Option<Listing> {
    let output = server
        .run(&commands.running_containers(), TIMEOUT_DEFAULT)
        .await;
    let parsed = parse_output(output.as_deref());
    if parsed.exit_code("containers") != Some(0) {
        return None;
    }
    Some(parse_containers(&parsed.payload, OUTPUT_LIMIT))
}

/// `filled($labels->get($key))`: the label when it is not blank.
fn filled<'a>(labels: &'a Labels, key: &str) -> Option<&'a str> {
    labels
        .get(key)
        .map(String::as_str)
        .filter(|value| !coolify_remote::text::php_trim(value).is_empty())
}

/// `filter_var($value, FILTER_VALIDATE_BOOLEAN)`.
fn is_true(value: Option<&String>) -> bool {
    value.is_some_and(|value| {
        matches!(
            coolify_remote::text::php_trim(value)
                .to_ascii_lowercase()
                .as_str(),
            "1" | "true" | "on" | "yes"
        )
    })
}

/// `isContainerOfType($labels, $type)`: whether a container belongs to a resource of the given
/// type (by UUID or older numeric id label).
fn is_container_of_type(labels: &Labels, kind: &str) -> bool {
    filled(labels, &format!("coolify.{kind}Uuid")).is_some()
        || filled(labels, &format!("coolify.{kind}Id")).is_some()
}

/// `containerOwnerUuid($labels, $type)`: new containers carry `coolify.{type}Uuid`. Containers
/// created before that label carry a numeric `coolify.{type}Id` instead; for those the owner is
/// read from the compose project (Swarm: stack namespace), which is the resource UUID.
fn container_owner_uuid<'a>(labels: &'a Labels, kind: &str) -> Option<&'a str> {
    if let Some(uuid) = filled(labels, &format!("coolify.{kind}Uuid")) {
        return Some(uuid);
    }
    if !labels.contains_key(&format!("coolify.{kind}Id")) {
        return None;
    }
    let legacy = if labels.contains_key("com.docker.compose.project") {
        "com.docker.compose.project"
    } else {
        "com.docker.stack.namespace"
    };
    filled(labels, legacy)
}

/// `preg_match('/(?:^|-)'.preg_quote($part, '/').'(?:-|$)/', $name)`: `part` is a dash-separated
/// part of `name`.
fn has_dash_part(name: &str, part: &str) -> bool {
    name.match_indices(part).any(|(at, _)| {
        let before = name[..at].chars().next_back();
        let after = name[at + part.len()..].chars().next();
        before.is_none_or(|c| c == '-') && after.is_none_or(|c| c == '-')
    })
}

/// `resolveContainerOwner($resources, $labels, $type)`: the resource that owns a container, from
/// the given candidates, by UUID label or compose project / stack namespace. Some older
/// application containers have another compose project: their container name (`coolify.name`,
/// for Dockerfile-style builds also the compose service) contains the application UUID as a
/// dash-separated part. The numeric id label is never used: another Coolify instance that
/// manages the same server can have a resource with the same id.
fn resolve_container_owner<'a>(
    resources: &'a [ScanOwner],
    labels: &Labels,
    kind: &str,
) -> Option<&'a ScanOwner> {
    let owner = container_owner_uuid(labels, kind)
        .and_then(|uuid| resources.iter().find(|resource| resource.uuid == uuid));
    if owner.is_some()
        || kind != "application"
        || filled(labels, "coolify.applicationUuid").is_some()
        || !labels.contains_key("coolify.applicationId")
    {
        return owner;
    }
    let names = ["com.docker.compose.service", "coolify.name"]
        .map(|key| labels.get(key).map(String::as_str).unwrap_or_default());
    resources.iter().find(|resource| {
        !coolify_remote::text::php_trim(&resource.uuid).is_empty()
            && names.iter().any(|name| has_dash_part(name, &resource.uuid))
    })
}

/// `is_numeric($value) ? (int) $value : null`.
fn numeric_int(value: &str) -> Option<i64> {
    let text = value.trim();
    text.parse::<i64>().ok().or_else(|| {
        text.parse::<f64>()
            .ok()
            .filter(|f| f.is_finite())
            .map(|f| f as i64)
    })
}

/// The resources of a server that containers are mapped to (`ImageRescanJob::targets()`).
#[derive(Debug, Clone, Default)]
struct ServerResources {
    /// `$server->applications()`.
    applications: Vec<ScanOwner>,
    /// `$server->databases()`.
    databases: Vec<ScanOwner>,
    /// `$server->services()`.
    services: Vec<ScanOwner>,
    /// The applications and databases of those services.
    service_parts: Vec<ScanOwner>,
}

impl ServerResources {
    async fn load(db: &Db, server_id: i64) -> Result<Self, String> {
        Ok(Self {
            applications: job_queries::applications_on_server(db, server_id)
                .await
                .map_err(err)?,
            databases: job_queries::databases_on_server(db, server_id)
                .await
                .map_err(err)?,
            services: job_queries::services_on_server(db, server_id)
                .await
                .map_err(err)?,
            service_parts: job_queries::service_parts_on_server(db, server_id)
                .await
                .map_err(err)?,
        })
    }

    /// `resolveServiceContainerOwner($services, $labels)[2]`: the service application or database
    /// that owns a service container. The part is matched by `coolify.service.subUuid`;
    /// containers from before that label are matched by their compose service key (the part
    /// name), then by the older numeric `coolify.service.subId`.
    fn service_part(&self, labels: &Labels) -> Option<&ScanOwner> {
        let service = resolve_container_owner(&self.services, labels, "service")?;
        let morph = if labels.get("coolify.service.subType").map(String::as_str) == Some("database")
        {
            SERVICE_DATABASE_MORPH
        } else {
            SERVICE_APPLICATION_MORPH
        };
        let parts: Vec<&ScanOwner> = self
            .service_parts
            .iter()
            .filter(|part| part.morph == morph && part.service_id == Some(service.id))
            .collect();
        if let Some(sub_uuid) = filled(labels, "coolify.service.subUuid") {
            return parts.into_iter().find(|part| part.uuid == sub_uuid);
        }
        let by_name = labels
            .get("com.docker.compose.service")
            .and_then(|name| parts.iter().find(|part| part.name() == *name));
        let by_id = || {
            let id = numeric_int(labels.get("coolify.service.subId")?)?;
            parts.iter().find(|part| part.id == id)
        };
        by_name.or_else(by_id).copied()
    }

    /// The resource that owns a container and, for an application, the pull request of the
    /// container (0 when it is not a preview). `None` for a container of no resource of this
    /// server.
    fn owner(&self, labels: &Labels) -> Option<(&ScanOwner, i64)> {
        if is_container_of_type(labels, "application") {
            // `applicationOwner()` before its preview check.
            let application = resolve_container_owner(&self.applications, labels, "application")?;
            let pull_request_id = labels
                .get("coolify.pullRequestId")
                .map_or(0, |id| php_intval(id));
            Some((application, pull_request_id))
        } else if is_container_of_type(labels, "service") {
            self.service_part(labels).map(|part| (part, 0))
        } else if is_container_of_type(labels, "database") {
            resolve_container_owner(&self.databases, labels, "database").map(|db| (db, 0))
        } else {
            None
        }
    }
}

/// The result of `ImageRescanJob::targets()`.
#[derive(Debug, Clone, Default)]
struct Targets {
    /// The resources of this server that run each image ID, with the image ref of their
    /// container, in the order of the container list.
    by_image: Vec<(String, Vec<(ScanOwner, String)>)>,
    /// The image refs each resource runs, by `ResourceScans::key()`.
    running_refs: HashMap<String, Vec<String>>,
}

impl Targets {
    fn add(&mut self, container: &Container, resource: &ScanOwner) {
        let key = resource.key();
        let index = match self
            .by_image
            .iter()
            .position(|(image_id, _)| *image_id == container.id)
        {
            Some(index) => index,
            None => {
                self.by_image.push((container.id.clone(), Vec::new()));
                self.by_image.len() - 1
            }
        };
        let image_targets = &mut self.by_image[index].1;
        if !image_targets.iter().any(|(owner, _)| owner.key() == key) {
            let image_ref = if container.image_ref.is_empty() {
                container.id.clone()
            } else {
                container.image_ref.clone()
            };
            image_targets.push((resource.clone(), image_ref));
        }
        self.running_refs
            .entry(key)
            .or_default()
            .push(container.image_ref.clone());
    }

    /// The keys `{morph}:{id}:{image id}` of the images that run.
    fn running(&self) -> HashSet<String> {
        let mut running = HashSet::new();
        for (image_id, image_targets) in &self.by_image {
            for (resource, _) in image_targets {
                running.insert(format!("{}:{image_id}", resource.key()));
            }
        }
        running
    }
}

/// `ImageRescanJob::targets()`: containers of resources on other servers, of previews that no longer exist, and
/// of no resource are skipped.
async fn rescan_targets(
    db: &Db,
    server_id: i64,
    containers: &[Container],
) -> Result<Targets, String> {
    let resources = ServerResources::load(db, server_id).await?;
    let mut targets = Targets::default();
    let mut previews: HashMap<(i64, i64), bool> = HashMap::new();
    for container in containers {
        if is_true(container.labels.get("com.docker.compose.oneoff")) {
            continue;
        }
        let Some((resource, pull_request_id)) = resources.owner(&container.labels) else {
            continue;
        };
        if pull_request_id != 0 {
            let key = (resource.id, pull_request_id);
            let exists = match previews.get(&key) {
                Some(exists) => *exists,
                None => {
                    let exists = job_queries::preview_exists(db, resource.id, pull_request_id)
                        .await
                        .map_err(err)?;
                    previews.insert(key, exists);
                    exists
                }
            };
            if !exists {
                continue;
            }
        }
        targets.add(container, resource);
    }
    Ok(targets)
}

/// `updateDatabase()`: updates the database under the scan lock, so no scan reads it while it
/// changes. A failed update keeps the existing database; the scans report when it is too old.
/// False when the lock is busy.
async fn update_database(server: &ScanServer, commands: &ScannerCommands) -> bool {
    let lock = scan_image::server_lock(server);
    if !scan_image::acquire_server_lock(&lock, server.id(), 0).await {
        return false;
    }
    let output = server.run(&commands.db_update(), TIMEOUT_DB_UPDATE).await;
    if parse_output(output.as_deref()).exit_code("db-update") == Some(0) {
        prepare::store_details(server, commands).await;
    }
    lock.release();
    true
}

/// `refreshCandidates()`: looks for newer versions of each rescanned image that has a relevant
/// finding (spec 4.5, 15.4). The rescan released the lock; each candidate scan takes it again.
/// Runs here, one image after the other; a queued or running check of the same scan does the
/// check instead. When the scan lock is busy, the check moves to a queued job that retries later.
async fn refresh_candidates(jobs: &Jobs, scans: &[ImageScanRow]) {
    for scan in scans {
        match candidates::should_refresh(jobs, scan).await {
            Ok(true) => {}
            Ok(false) => continue,
            Err(e) => {
                tracing::error!(scan_id = scan.id, error = %e, "RefreshCandidates::shouldRefresh failed");
                continue;
            }
        }
        // Upstream takes the unique lock of `RefreshCandidatesJob` here. The queued mark of the
        // job (`RefreshCandidates::isQueued()`) is set for as long as that lock is held.
        if candidates::is_queued(scan) {
            continue;
        }
        match candidates::run(jobs, scan, 0, true).await {
            Ok(()) => {}
            Err(RefreshError::LockBusy) => candidates::queue_check(jobs, scan),
            Err(RefreshError::Other(e)) => {
                tracing::error!(scan_id = scan.id, error = %e, "RefreshCandidates::run failed");
            }
        }
    }
}

/// `ended-ignores` mark of a team and day.
fn ended_ignores_cache_key(team_id: i64, today: NaiveDate) -> String {
    format!("image-scan:ended-ignores:{team_id}:{}", date_string(today))
}

/// `refreshCountsAfterEndedIgnores()`: an ignore that ended counts its findings again: the stored
/// counts of the resources it covered are written again, also for images that no longer run.
/// Once per team and day, for ignores that ended in the last two days: by their end date, or by
/// the 90 days after which an ignore no longer covers a problem with an exploit signal.
async fn refresh_counts_after_ended_ignores(jobs: &Jobs, server: &ScanServer) {
    if let Err(e) = refresh_ended_ignores(jobs, server.team_id()).await {
        tracing::error!(server_id = server.id(), error = %e, "ImageRescanJob could not refresh the counts of ended ignores");
    }
}

async fn refresh_ended_ignores(jobs: &Jobs, team_id: i64) -> Result<(), String> {
    let db = &jobs.0.db;
    let now = Jobs::now();
    let signal_limit = now - chrono::Duration::days(IgnoreMatcher::EXPLOIT_SIGNAL_IGNORE_DAYS);
    let ended = job_queries::ended_ignores(db, team_id, now, signal_limit)
        .await
        .map_err(err)?;
    if ended.is_empty()
        || !cache::add(
            &ended_ignores_cache_key(team_id, now.date()),
            Value::Bool(true),
            DAY,
        )
    {
        return Ok(());
    }

    if ended.iter().any(|ignore| ignore.resource_id.is_none()) {
        jobs.dispatch_refresh_image_scan_summaries(team_id);
        return Ok(());
    }

    let mut seen = HashSet::new();
    for ignore in &ended {
        let (Some(resource_type), Some(resource_id)) =
            (ignore.resource_type.as_deref(), ignore.resource_id)
        else {
            continue;
        };
        if !seen.insert((resource_type, resource_id)) {
            continue;
        }
        let Some(resource) = image_scans::find_owner(db, resource_type, resource_id)
            .await
            .map_err(err)?
        else {
            continue;
        };
        let current = resource_scans::current_for(db, &resource).await?;
        resource_scans::refresh_summaries(jobs, current).await?;
    }
    Ok(())
}

/// The deployments and image refs of the images that CleanupDocker keeps for rollbacks.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Rollback {
    deployments: Vec<i64>,
    refs: Vec<String>,
}

/// `substr($commit, 0, 128)`.
fn commit_tag(commit: &str) -> &str {
    let mut end = commit.len().min(128);
    while !commit.is_char_boundary(end) {
        end -= 1;
    }
    &commit[..end]
}

/// The images to keep of `rollbackList()`: none when the server turned off image retention.
fn images_to_keep(retention_disabled: bool, application: &RetentionApplication) -> usize {
    if retention_disabled {
        return 0;
    }
    usize::try_from(application.docker_images_to_keep.unwrap_or(2).max(0)).unwrap_or(0)
}

/// `rollbackList()`: the deployments and image refs of the images that CleanupDocker keeps for
/// rollbacks besides the running ones: the latest `keep` finished deployments whose image is not
/// running. `deployments` are `(id, commit)` of the finished deployments, the latest first.
fn rollback_list(
    keep: usize,
    repository: &str,
    deployments: &[(i64, Option<String>)],
    running_refs: &[String],
) -> Rollback {
    if keep == 0 {
        return Rollback::default();
    }
    let image = |commit: &str| format!("{repository}:{}", commit_tag(commit));
    let mut commits: Vec<&str> = Vec::new();
    for (_, commit) in deployments {
        let Some(commit) = commit
            .as_deref()
            .filter(|commit| !coolify_remote::text::php_trim(commit).is_empty())
        else {
            continue;
        };
        if !commits.contains(&commit) {
            commits.push(commit);
        }
    }
    let commits: Vec<&str> = commits
        .into_iter()
        .filter(|commit| !running_refs.contains(&image(commit)))
        .take(keep)
        .collect();
    Rollback {
        deployments: deployments
            .iter()
            .filter(|(_, commit)| commit.as_deref().is_some_and(|c| commits.contains(&c)))
            .map(|(id, _)| *id)
            .collect(),
        refs: commits.iter().map(|commit| image(commit)).collect(),
    }
}

/// `previewPullRequestId()`: the pull request of a preview scan: from its deployment, else from
/// the preview tag of its image (`{image}:pr-{id}-{commit}`). `None` for a scan of the
/// application itself.
fn preview_pull_request_id(
    scan: &RetentionScan,
    application: Option<&RetentionApplication>,
) -> Option<i64> {
    let application = application?;
    let from_deployment = scan.pull_request_id.unwrap_or(0);
    if from_deployment > 0 {
        return Some(from_deployment);
    }
    let rest = scan
        .image_ref
        .strip_prefix(application.repository())?
        .strip_prefix(":pr-")?;
    let digits = rest.find(|c: char| !c.is_ascii_digit())?;
    if digits == 0 || !rest[digits..].starts_with('-') {
        return None;
    }
    Some(rest[..digits].parse().unwrap_or(i64::MAX))
}

/// The decision of `applyRetention()` for the scans of one resource on the server (the latest
/// first): keeps the scans of running images, of the rollback list, and the latest scans; deletes
/// scans of previews that no longer exist (spec 4.3). Returns the ids to delete.
fn retention_deletes(
    scans: &[RetentionScan],
    running: &HashSet<String>,
    rollback: &Rollback,
    application: Option<&RetentionApplication>,
    previews: &[i64],
) -> Vec<i64> {
    let mut delete = Vec::new();
    for (index, scan) in scans.iter().enumerate() {
        let preview_gone = preview_pull_request_id(scan, application)
            .is_some_and(|pull_request_id| !previews.contains(&pull_request_id));
        let is_running = running.contains(&format!(
            "{}:{}:{}",
            scan.resource_type, scan.resource_id, scan.image_id
        ));
        let keep = index < KEEP_LATEST_SCANS
            || scan
                .deployment_queue_id
                .is_some_and(|id| rollback.deployments.contains(&id))
            || rollback.refs.contains(&scan.image_ref);
        // A row is per image, so a preview and production can share it: a running image always
        // stays.
        if !is_running && (preview_gone || !keep) {
            delete.push(scan.id);
        }
    }
    delete
}

/// `applyRetention()`. Only rows of this server.
async fn apply_retention(
    jobs: &Jobs,
    server: &ScanServer,
    targets: &Targets,
) -> Result<(), String> {
    let db = &jobs.0.db;
    let running = targets.running();
    let retention_disabled = server
        .srv
        .info
        .setting_bool("disable_application_image_retention")
        == Some(true);
    let no_refs: Vec<String> = Vec::new();
    let scans = job_queries::retention_scans(db, server.id())
        .await
        .map_err(err)?;
    for group in
        scans.chunk_by(|a, b| a.resource_type == b.resource_type && a.resource_id == b.resource_id)
    {
        let Some(first) = group.first() else {
            continue;
        };
        let application = if first.resource_type == APPLICATION_MORPH {
            job_queries::retention_application(db, first.resource_id)
                .await
                .map_err(err)?
        } else {
            None
        };
        let mut rollback = Rollback::default();
        let mut previews = Vec::new();
        if let Some(application) = &application {
            let keep = images_to_keep(retention_disabled, application);
            if keep > 0 {
                let deployments = job_queries::finished_deployments(db, application.id)
                    .await
                    .map_err(err)?;
                let key = format!("{}:{}", first.resource_type, first.resource_id);
                rollback = rollback_list(
                    keep,
                    application.repository(),
                    &deployments,
                    targets.running_refs.get(&key).unwrap_or(&no_refs),
                );
            }
            previews = job_queries::preview_pull_request_ids(db, application.id)
                .await
                .map_err(err)?;
        }

        let delete = retention_deletes(group, &running, &rollback, application.as_ref(), &previews);
        job_queries::delete_scans_on_server(db, server.id(), &delete)
            .await
            .map_err(err)?;
    }
    Ok(())
}

/// `deleteUnusedSboms()`: deletes SBOMs of this server that no scan uses and that were not used
/// for 30 days, files first.
async fn delete_unused_sboms(
    jobs: &Jobs,
    server: &ScanServer,
    commands: &ScannerCommands,
) -> Result<(), String> {
    let db = &jobs.0.db;
    let before = Jobs::now() - chrono::Duration::days(SBOM_UNUSED_DAYS);
    let sboms: Vec<(i64, String)> =
        job_queries::unused_sboms(db, server.id(), before, MAX_SBOM_DELETES)
            .await
            .map_err(err)?
            .into_iter()
            .filter(|(_, image_id)| IMAGE_ID.is_match(image_id))
            .collect();
    if sboms.is_empty() {
        return Ok(());
    }

    let image_ids: Vec<String> = sboms.iter().map(|(_, image_id)| image_id.clone()).collect();
    let delete = commands
        .delete_sboms(&image_ids)
        .map_err(|e| e.to_string())?;
    let output = server.run(&delete, TIMEOUT_DEFAULT).await;
    if parse_output(output.as_deref()).exit_code("sbom-delete") != Some(0) {
        return Ok(());
    }

    let ids: Vec<i64> = sboms.iter().map(|(id, _)| *id).collect();
    job_queries::delete_unused_sboms(db, server.id(), &ids)
        .await
        .map_err(err)
}

/// `cleanUp()`: retention and SBOM deletion under the scan lock, so a scan that starts meanwhile
/// keeps its SBOM and rows. When the lock is busy they skip a day.
async fn clean_up(jobs: &Jobs, server: &ScanServer, commands: &ScannerCommands, targets: &Targets) {
    let lock = scan_image::server_lock(server);
    if !scan_image::acquire_server_lock(&lock, server.id(), 0).await {
        return;
    }
    let cleaned = async {
        apply_retention(jobs, server, targets).await?;
        delete_unused_sboms(jobs, server, commands).await
    }
    .await;
    if let Err(e) = cleaned {
        tracing::error!(server_id = server.id(), error = %e, "ImageRescanJob could not clean up");
    }
    lock.release();
}

/// `ImageRescanJob::handle()`.
async fn rescan(
    jobs: &Jobs,
    server_id: i64,
    job_uuid: &str,
    attempt: u32,
) -> Result<Attempt, String> {
    let Ok(server) = ScanServer::load(jobs, server_id).await else {
        return Ok(Attempt::Done);
    };
    if !can_run_on(&server) {
        return Ok(Attempt::Done);
    }

    let commands = server.commands(&new_run_id()).map_err(|e| e.to_string())?;
    // Never sleeps on the scan lock, so a deploy gate gets it first. A busy lock runs the job
    // again later and it goes on where it stopped; the last attempt runs without the busy steps.
    let last_attempt = attempt >= MAX_ATTEMPTS;
    let progress_key = progress_cache_key(job_uuid);
    let mut progress = Progress::from_value(cache::get(&progress_key).as_ref());

    if !progress.database {
        if !update_database(&server, &commands).await && !last_attempt {
            return Ok(retry_later(job_uuid, &progress, attempt));
        }
        progress.database = true;
    }

    let Some(listing) = running_containers(&server, &commands).await else {
        record_rescan_result(server.id(), false);
        cache::forget(&progress_key);
        jobs.dispatch_scan_host_packages(server.id());
        return Ok(Attempt::Done);
    };

    let targets = rescan_targets(&jobs.0.db, server.id(), &listing.containers).await?;
    for (image_id, image_targets) in &targets.by_image {
        if progress.done.contains(image_id) {
            continue;
        }

        match scan_image::rescan(
            jobs,
            &server,
            image_id,
            image_targets.clone(),
            0,
            !last_attempt,
        )
        .await
        {
            Ok(scans) => {
                if !last_attempt && !scans.is_empty() && scans.iter().all(scan_image::lock_was_busy)
                {
                    return Ok(retry_later(job_uuid, &progress, attempt));
                }
                progress.scanned += i64::try_from(scans.len()).unwrap_or(i64::MAX);
                progress.completed +=
                    i64::try_from(scans.iter().filter(|scan| scan.is_completed()).count())
                        .unwrap_or(i64::MAX);
                refresh_candidates(jobs, &scans).await;
            }
            Err(e) => {
                progress.errors += 1;
                tracing::error!(server_id, image_id = %image_id, error = %e, "ImageRescanJob could not rescan an image");
            }
        }
        progress.done.push(image_id.clone());
    }
    record_rescan_result(server.id(), progress.succeeded());
    cache::forget(&progress_key);
    refresh_counts_after_ended_ignores(jobs, &server).await;

    // A cut list would make running images look stopped.
    if !listing.truncated {
        clean_up(jobs, &server, &commands, &targets).await;
    }
    jobs.dispatch_scan_host_packages(server.id());
    Ok(Attempt::Done)
}

/// The key of the `ShouldBeUnique` lock of the rescan of a server.
fn rescan_unique_key(server_id: i64) -> String {
    format!("image-rescan-job:{server_id}")
}

impl Jobs {
    /// `ImageRescanJob::dispatch($server)`: one job per server at a time. A job that finds the
    /// scan lock busy runs again later and goes on where it stopped, up to [`MAX_ATTEMPTS`]
    /// attempts. An unexpected error fails the job.
    pub fn dispatch_image_rescan(&self, server_id: i64) {
        if !cache::add(
            &rescan_unique_key(server_id),
            Value::Bool(true),
            RESCAN_TIMEOUT,
        ) {
            return;
        }
        let jobs = self.clone();
        tokio::spawn(async move {
            // `$this->job->uuid()`.
            let job_uuid = sbom_document::uuid_v4();
            let mut attempt = 1;
            loop {
                match tokio::time::timeout(
                    RESCAN_TIMEOUT,
                    rescan(&jobs, server_id, &job_uuid, attempt),
                )
                .await
                {
                    Ok(Ok(Attempt::Done)) => break,
                    Ok(Ok(Attempt::Release(delay))) => {
                        tokio::time::sleep(Duration::from_secs(delay)).await;
                        attempt += 1;
                    }
                    Ok(Err(e)) => {
                        tracing::error!(server_id, error = %e, "ImageRescanJob failed");
                        break;
                    }
                    Err(_) => {
                        tracing::error!(server_id, "ImageRescanJob timed out");
                        break;
                    }
                }
            }
            cache::forget(&progress_cache_key(&job_uuid));
            cache::forget(&rescan_unique_key(server_id));
        });
    }
}

#[cfg(test)]
mod tests {
    use coolify_db::image_scans::STATUS_COMPLETED;
    use serde_json::Map;

    use super::super::resource_scans::tests::scan;
    use super::*;

    const HEX: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const OTHER_HEX: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const POSTGRESQL_MORPH: &str = r"App\Models\StandalonePostgresql";

    fn image_id(hex: &str) -> String {
        format!("sha256:{hex}")
    }

    fn owner(morph: &'static str, id: i64, uuid: &str) -> ScanOwner {
        ScanOwner {
            morph,
            id,
            uuid: uuid.to_owned(),
            team_id: Some(1),
            main_server_id: Some(2),
            service_id: None,
            record: Map::new(),
            settings: None,
        }
    }

    fn part(morph: &'static str, id: i64, uuid: &str, service_id: i64, name: &str) -> ScanOwner {
        let mut part = owner(morph, id, uuid);
        part.service_id = Some(service_id);
        part.record
            .insert("name".to_owned(), Value::String(name.to_owned()));
        part
    }

    fn labels(pairs: &[(&str, &str)]) -> Labels {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    }

    fn container(hex: &str, image_ref: &str, pairs: &[(&str, &str)]) -> Container {
        Container {
            id: image_id(hex),
            image_ref: image_ref.to_owned(),
            labels: labels(pairs),
        }
    }

    /// A unique owner per test, so tests do not share cache keys.
    fn cache_owner() -> ScanOwner {
        let id = i64::from(rand::random::<u32>()) + 1_000_000;
        owner(APPLICATION_MORPH, id, "queued")
    }

    fn failed_scan(code: ErrorCode) -> ImageScanRow {
        let mut failed = scan("nginx:1.27", STATUS_FAILED, None);
        failed.error_code = Some(code.value().to_owned());
        failed
    }

    // -- ScanResourceImagesJob ---------------------------------------------------------------

    #[test]
    fn a_new_job_scans_the_images_of_the_resource_without_retries() {
        let job = ScanResourceImages::new(&owner(POSTGRESQL_MORPH, 0, "pg"));
        assert_eq!(
            job,
            ScanResourceImages {
                resource_type: POSTGRESQL_MORPH.to_owned(),
                resource_id: 0,
                deployment_queue_id: None,
                image_refs: Vec::new(),
                retry_when_image_missing: false,
                base_image_ref: None,
                pull_when_missing: false,
            }
        );
        assert_eq!(START_DELAY_SECONDS, 30);
        assert_eq!(START_RETRY_DELAY_SECONDS, 120);
        assert_eq!(MAX_DERIVED_IMAGES, 5);
        assert_eq!(QUEUED_STAGE, "queued");
    }

    #[test]
    fn retries_only_a_missing_image_of_a_job_after_a_start() {
        let missing = failed_scan(ErrorCode::ImageMissing);
        // The first two attempts of a start job wait for the image.
        assert!(should_retry(true, 1, &missing));
        assert!(should_retry(true, 2, &missing));
        // Releases at most twice and leaves the error on the last attempt.
        assert_eq!(START_MAX_ATTEMPTS, 3);
        assert!(!should_retry(true, 3, &missing));
        // A job that was not queued by a start does not retry.
        assert!(!should_retry(false, 1, &missing));
        // Other failures and completed scans are done.
        assert!(!should_retry(true, 1, &failed_scan(ErrorCode::Timeout)));
        assert!(!should_retry(
            true,
            1,
            &scan("nginx:1.27", STATUS_COMPLETED, None)
        ));
    }

    #[test]
    fn a_busy_lock_is_not_a_missing_image() {
        let mut busy = failed_scan(ErrorCode::LockBusy);
        busy.id = 0;
        assert!(scan_image::lock_was_busy(&busy));
        assert!(!should_retry(true, 1, &busy));
        assert_eq!(lock_retry_delay(1), 60);
        assert_eq!(lock_retry_delay(2), 120);
        assert_eq!(lock_retry_delay(3), 300);
    }

    #[test]
    fn scans_every_target_once() {
        let web = owner(SERVICE_APPLICATION_MORPH, 3, "web");
        let db = owner(SERVICE_DATABASE_MORPH, 3, "db");
        let targets = unique_targets(vec![
            (web.clone(), "nginx:1.27".to_owned()),
            (db.clone(), "nginx:1.27".to_owned()),
            (web.clone(), "nginx:1.27".to_owned()),
            (web.clone(), "nginx:1.28".to_owned()),
        ]);
        let keys: Vec<String> = targets
            .iter()
            .map(|(resource, image_ref)| target_key(resource, image_ref))
            .collect();
        assert_eq!(
            keys,
            vec![
                r"App\Models\ServiceApplication:3:nginx:1.27",
                r"App\Models\ServiceDatabase:3:nginx:1.27",
                r"App\Models\ServiceApplication:3:nginx:1.28",
            ]
        );
    }

    #[test]
    fn an_application_derives_its_latest_five_images() {
        let refs = ["a:1", "a:1", "a:2", "a:3", "a:2", "a:4", "a:5", "a:6"]
            .map(str::to_owned)
            .to_vec();
        assert_eq!(
            derived_image_refs(refs),
            vec!["a:1", "a:2", "a:3", "a:4", "a:5"]
        );
        assert!(derived_image_refs(Vec::new()).is_empty());
    }

    #[test]
    fn keeps_the_targets_that_were_scanned_before_the_lock_was_busy() {
        let uuid = sbom_document::uuid_v4();
        assert!(done_targets(&uuid).is_empty());
        remember_done_targets(&uuid, &["a:1:nginx".to_owned(), "a:2:redis".to_owned()]);
        assert_eq!(done_targets(&uuid), vec!["a:1:nginx", "a:2:redis"]);
        assert_eq!(done_cache_key("j1"), "image-scan:start-done:j1".to_owned());
        cache::forget(&done_cache_key(&uuid));
    }

    #[test]
    fn marks_every_scanned_owner_as_queued_and_hides_its_last_error() {
        let web = cache_owner();
        let db = cache_owner();
        cache::put(
            &scan_image::last_error_cache_key(&web),
            json!({"code": "image_missing"}),
            DAY,
        );

        mark_owners_queued(&[web.clone(), db.clone()]);

        for owner in [&web, &db] {
            let progress = cache::get(&scan_image::progress_cache_key(owner)).unwrap_or_default();
            assert_eq!(progress["stage"], json!(QUEUED_STAGE));
            assert_eq!(progress["run_id"], json!(QUEUED_STAGE));
            assert!(
                progress["started_at"]
                    .as_str()
                    .is_some_and(|at| at.ends_with("+00:00"))
            );
        }
        assert!(!cache::has(&scan_image::last_error_cache_key(&web)));

        // A mark that no scan replaced is removed.
        clear_marks(&[web.clone(), db.clone()]);
        assert!(!cache::has(&scan_image::progress_cache_key(&web)));
        assert!(!cache::has(&scan_image::progress_cache_key(&db)));
    }

    #[test]
    fn keeps_the_progress_of_a_scan_that_replaced_the_mark() {
        let web = cache_owner();
        let key = scan_image::progress_cache_key(&web);
        cache::put(&key, json!({"run_id": "r1", "stage": "sbom"}), DAY);
        clear_marks(std::slice::from_ref(&web));
        assert_eq!(
            cache::get(&key),
            Some(json!({"run_id": "r1", "stage": "sbom"}))
        );
        cache::forget(&key);
    }

    // -- ImageRescanJob: failed days ---------------------------------------------------------

    fn day(day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 10, day).unwrap_or_default()
    }

    #[test]
    fn counts_days_in_a_row_on_which_the_rescan_failed() {
        let first = next_failure_state(None, day(20));
        assert_eq!(
            first.to_string(),
            r#"{"days":1,"last":"2026-10-20","notified":false}"#
        );
        // A second failure on the same day changes nothing.
        assert_eq!(next_failure_state(Some(&first), day(20)), first);
        let second = next_failure_state(Some(&first), day(21));
        assert_eq!(
            second.to_string(),
            r#"{"days":2,"last":"2026-10-21","notified":false}"#
        );
        // The notified mark stays while the days go on.
        let notified = json!({"days": 2, "last": "2026-10-21", "notified": true});
        assert_eq!(
            next_failure_state(Some(&notified), day(22)).to_string(),
            r#"{"days":3,"last":"2026-10-22","notified":true}"#
        );
        // A day without a failure in between starts again.
        assert_eq!(
            next_failure_state(Some(&notified), day(24)).to_string(),
            r#"{"days":1,"last":"2026-10-24","notified":false}"#
        );
        assert_eq!(next_failure_state(Some(&json!("junk")), day(20)), first);
    }

    #[test]
    fn a_success_resets_the_failed_days() {
        let server_id = -i64::from(rand::random::<u32>()) - 1;
        assert_eq!(failures_cache_key(7), "image-rescan:failures:7");
        assert_eq!(failed_days(server_id), 0);
        assert!(!is_failure_notified(server_id));
        // Nothing to mark before a failure.
        mark_failure_notified(server_id);
        assert_eq!(failed_days(server_id), 0);

        record_rescan_result(server_id, false);
        record_rescan_result(server_id, false);
        assert_eq!(failed_days(server_id), 1);
        assert!(!is_failure_notified(server_id));
        mark_failure_notified(server_id);
        assert!(is_failure_notified(server_id));
        assert_eq!(failed_days(server_id), 1);

        record_rescan_result(server_id, true);
        assert_eq!(failed_days(server_id), 0);
        assert!(!is_failure_notified(server_id));
    }

    #[test]
    fn a_day_on_which_no_running_image_could_be_scanned_is_failed() {
        // Nothing runs on the server: a success.
        assert!(Progress::default().succeeded());
        let progress = |scanned, completed, errors| Progress {
            database: true,
            done: Vec::new(),
            scanned,
            completed,
            errors,
        };
        assert!(progress(2, 1, 0).succeeded());
        assert!(progress(2, 1, 3).succeeded());
        // Grype failed for the only image.
        assert!(!progress(1, 0, 0).succeeded());
        assert!(!progress(0, 0, 1).succeeded());
    }

    #[test]
    fn a_released_rescan_goes_on_where_it_stopped() {
        assert_eq!(Progress::from_value(None), Progress::default());
        assert_eq!(
            Progress::from_value(Some(&json!({"database": true}))),
            Progress {
                database: true,
                ..Progress::default()
            }
        );
        let progress = Progress {
            database: true,
            done: vec![image_id(HEX)],
            scanned: 2,
            completed: 1,
            errors: 1,
        };
        assert_eq!(Progress::from_value(Some(&progress.to_value())), progress);

        let uuid = sbom_document::uuid_v4();
        assert_eq!(retry_later(&uuid, &progress, 1), Attempt::Release(60));
        assert_eq!(retry_later(&uuid, &progress, 2), Attempt::Release(120));
        assert_eq!(retry_later(&uuid, &progress, 7), Attempt::Release(300));
        assert_eq!(
            Progress::from_value(cache::get(&progress_cache_key(&uuid)).as_ref()),
            progress
        );
        assert_eq!(progress_cache_key("j1"), "image-rescan:progress:j1");
        assert_eq!(rescan_unique_key(7), "image-rescan-job:7");
        cache::forget(&progress_cache_key(&uuid));
    }

    // -- ImageRescanJob: containers ----------------------------------------------------------

    #[test]
    fn reads_the_running_containers() {
        let payload = [
            format!(
                r#"{{"id":"sha256:{HEX}","ref":"nginx:1.27","labels":{{"coolify.type":"service","coolify.serviceId":7,"flag":true,"nested":{{"a":1}},"123":"numeric key"}}}}"#
            ),
            // No labels and no ref.
            format!(r#"  {{"id":"sha256:{OTHER_HEX}","ref":null,"labels":null}}  "#),
            // Not an image ID.
            r#"{"id":"nginx:1.27","ref":"nginx:1.27","labels":{}}"#.to_owned(),
            r#"{"id":"sha256:ABC","ref":"nginx:1.27","labels":{}}"#.to_owned(),
            "not json".to_owned(),
            "[1,2]".to_owned(),
            String::new(),
        ]
        .join("\n");
        let listing = parse_containers(&payload, OUTPUT_LIMIT);
        assert!(!listing.truncated);
        assert_eq!(
            listing.containers,
            vec![
                container(
                    HEX,
                    "nginx:1.27",
                    &[
                        ("coolify.type", "service"),
                        ("coolify.serviceId", "7"),
                        ("flag", "1"),
                    ]
                ),
                container(OTHER_HEX, "", &[]),
            ]
        );
        assert_eq!(parse_containers("", OUTPUT_LIMIT), Listing::default());
    }

    #[test]
    fn a_container_list_that_was_cut_keeps_only_its_complete_lines() {
        let line =
            |hex: &str| format!(r#"{{"id":"sha256:{hex}","ref":"nginx:1.27","labels":{{}}}}"#);
        let payload = format!("{}\n{}", line(HEX), line(OTHER_HEX));
        let listing = parse_containers(&payload, payload.len() - 1);
        assert!(listing.truncated);
        assert_eq!(listing.containers, vec![container(HEX, "nginx:1.27", &[])]);
        // One line over the cap: nothing can be read.
        let cut = parse_containers(&"x".repeat(50), 49);
        assert!(cut.truncated);
        assert!(cut.containers.is_empty());
        // At the cap the list is complete.
        assert!(!parse_containers(&payload, payload.len()).truncated);
    }

    fn server_resources() -> ServerResources {
        ServerResources {
            applications: vec![
                owner(APPLICATION_MORPH, 10, "appuuid"),
                owner(APPLICATION_MORPH, 11, "otherapp"),
            ],
            databases: vec![owner(POSTGRESQL_MORPH, 20, "pguuid")],
            services: vec![
                owner(image_scans::SERVICE_MORPH, 30, "svcuuid"),
                owner(image_scans::SERVICE_MORPH, 31, "othersvc"),
            ],
            service_parts: vec![
                part(SERVICE_APPLICATION_MORPH, 40, "webuuid", 30, "web"),
                part(SERVICE_DATABASE_MORPH, 41, "dbuuid", 30, "db"),
                part(SERVICE_APPLICATION_MORPH, 42, "otherweb", 31, "web"),
            ],
        }
    }

    fn owner_of(resources: &ServerResources, pairs: &[(&str, &str)]) -> Option<(String, i64)> {
        resources
            .owner(&labels(pairs))
            .map(|(owner, pull_request_id)| (owner.key(), pull_request_id))
    }

    #[test]
    fn maps_service_database_and_preview_containers_to_their_resources() {
        let resources = server_resources();
        let key = |morph: &str, id: i64| format!("{morph}:{id}");
        assert_eq!(
            owner_of(
                &resources,
                &[
                    ("coolify.type", "service"),
                    ("coolify.serviceId", "30"),
                    ("coolify.serviceUuid", "svcuuid"),
                    ("coolify.service.subType", "application"),
                    ("coolify.service.subUuid", "webuuid"),
                    ("com.docker.compose.service", "web"),
                ]
            ),
            Some((key(SERVICE_APPLICATION_MORPH, 40), 0))
        );
        assert_eq!(
            owner_of(
                &resources,
                &[
                    ("coolify.type", "database"),
                    ("coolify.databaseId", "20"),
                    ("coolify.databaseUuid", "pguuid"),
                ]
            ),
            Some((key(POSTGRESQL_MORPH, 20), 0))
        );
        // A preview container carries its pull request.
        assert_eq!(
            owner_of(
                &resources,
                &[
                    ("coolify.managed", "true"),
                    ("coolify.type", "application"),
                    ("coolify.applicationId", "10"),
                    ("coolify.applicationUuid", "appuuid"),
                    ("coolify.pullRequestId", "7"),
                ]
            ),
            Some((key(APPLICATION_MORPH, 10), 7))
        );
        assert_eq!(
            owner_of(
                &resources,
                &[
                    ("coolify.applicationUuid", "otherapp"),
                    ("coolify.pullRequestId", "0"),
                ]
            ),
            Some((key(APPLICATION_MORPH, 11), 0))
        );
        // An application of another server, and a container of no resource.
        assert_eq!(
            owner_of(
                &resources,
                &[
                    ("coolify.applicationId", "99"),
                    ("coolify.applicationUuid", "foreign"),
                ]
            ),
            None
        );
        assert_eq!(owner_of(&resources, &[("coolify.managed", "true")]), None);
    }

    #[test]
    fn maps_containers_from_before_the_uuid_labels() {
        let resources = server_resources();
        let key = |morph: &str, id: i64| format!("{morph}:{id}");
        // The compose project is the resource UUID.
        assert_eq!(
            owner_of(
                &resources,
                &[
                    ("coolify.applicationId", "10"),
                    ("com.docker.compose.project", "appuuid"),
                ]
            ),
            Some((key(APPLICATION_MORPH, 10), 0))
        );
        // Swarm: the stack namespace.
        assert_eq!(
            owner_of(
                &resources,
                &[
                    ("coolify.databaseId", "20"),
                    ("com.docker.stack.namespace", "pguuid"),
                ]
            ),
            Some((key(POSTGRESQL_MORPH, 20), 0))
        );
        // The compose project is the deployment directory: the container name has the UUID.
        assert_eq!(
            owner_of(
                &resources,
                &[
                    ("coolify.applicationId", "10"),
                    ("com.docker.compose.project", "deploy-dir"),
                    ("coolify.name", "appuuid-104512123456"),
                ]
            ),
            Some((key(APPLICATION_MORPH, 10), 0))
        );
        assert_eq!(
            owner_of(
                &resources,
                &[
                    ("coolify.applicationId", "10"),
                    ("com.docker.compose.service", "web-appuuid-104512123456"),
                ]
            ),
            Some((key(APPLICATION_MORPH, 10), 0))
        );
        // The UUID must be a whole dash-separated part, and the numeric id is never used.
        assert_eq!(
            owner_of(
                &resources,
                &[
                    ("coolify.applicationId", "10"),
                    ("coolify.name", "xappuuid-104512123456"),
                ]
            ),
            None
        );
        // Service parts: by the compose service key, then by the older numeric id.
        assert_eq!(
            owner_of(
                &resources,
                &[
                    ("coolify.serviceId", "30"),
                    ("com.docker.compose.project", "svcuuid"),
                    ("coolify.service.subType", "database"),
                    ("com.docker.compose.service", "db"),
                ]
            ),
            Some((key(SERVICE_DATABASE_MORPH, 41), 0))
        );
        assert_eq!(
            owner_of(
                &resources,
                &[
                    ("coolify.serviceUuid", "othersvc"),
                    ("coolify.service.subId", "42"),
                    ("com.docker.compose.service", "renamed"),
                ]
            ),
            Some((key(SERVICE_APPLICATION_MORPH, 42), 0))
        );
        // The part of another service with the same name is not taken.
        assert_eq!(
            owner_of(
                &resources,
                &[
                    ("coolify.serviceUuid", "svcuuid"),
                    ("coolify.service.subUuid", "otherweb"),
                ]
            ),
            None
        );
    }

    #[test]
    fn reads_label_values_like_php() {
        assert!(is_true(Some(&"True".to_owned())));
        assert!(is_true(Some(&"1".to_owned())));
        assert!(!is_true(Some(&"False".to_owned())));
        assert!(!is_true(Some(&String::new())));
        assert!(!is_true(None));
        assert_eq!(php_intval("7"), 7);
        assert_eq!(php_intval(" 12abc"), 12);
        assert_eq!(php_intval("-3"), -3);
        assert_eq!(php_intval("abc"), 0);
        assert_eq!(php_intval(""), 0);
        assert_eq!(numeric_int("42"), Some(42));
        assert_eq!(numeric_int("42.0"), Some(42));
        assert_eq!(numeric_int("web"), None);
        assert!(is_php_int_key("123"));
        assert!(is_php_int_key("0"));
        assert!(is_php_int_key("-5"));
        assert!(!is_php_int_key("012"));
        assert!(!is_php_int_key("-0"));
        assert!(!is_php_int_key("coolify.type"));
        assert!(has_dash_part("appuuid", "appuuid"));
        assert!(has_dash_part("web-appuuid", "appuuid"));
        assert!(!has_dash_part("web-appuuid2", "appuuid"));
    }

    #[test]
    fn groups_the_resources_by_the_image_they_run() {
        let app = owner(APPLICATION_MORPH, 10, "appuuid");
        let pg = owner(POSTGRESQL_MORPH, 20, "pguuid");
        let mut targets = Targets::default();
        targets.add(&container(HEX, "appuuid:abc123", &[]), &app);
        targets.add(&container(OTHER_HEX, "postgres:16", &[]), &pg);
        // A second container of the application with the same image, and one without a ref.
        targets.add(&container(HEX, "appuuid:other", &[]), &app);
        targets.add(&container(HEX, "", &[]), &pg);

        let by_image: Vec<(String, Vec<(String, String)>)> = targets
            .by_image
            .iter()
            .map(|(id, image_targets)| {
                (
                    id.clone(),
                    image_targets
                        .iter()
                        .map(|(owner, image_ref)| (owner.key(), image_ref.clone()))
                        .collect(),
                )
            })
            .collect();
        assert_eq!(
            by_image,
            vec![
                (
                    image_id(HEX),
                    vec![
                        (app.key(), "appuuid:abc123".to_owned()),
                        (pg.key(), image_id(HEX)),
                    ]
                ),
                (
                    image_id(OTHER_HEX),
                    vec![(pg.key(), "postgres:16".to_owned())]
                ),
            ]
        );
        assert_eq!(
            targets.running_refs.get(&app.key()),
            Some(&vec![
                "appuuid:abc123".to_owned(),
                "appuuid:other".to_owned()
            ])
        );
        assert_eq!(
            targets.running_refs.get(&pg.key()),
            Some(&vec!["postgres:16".to_owned(), String::new()])
        );
        let running = targets.running();
        assert_eq!(running.len(), 3);
        assert!(running.contains(&format!("{}:{}", app.key(), image_id(HEX))));
        assert!(running.contains(&format!("{}:{}", pg.key(), image_id(OTHER_HEX))));
    }

    // -- ImageRescanJob: retention -----------------------------------------------------------

    fn application(images_to_keep: Option<i64>) -> RetentionApplication {
        RetentionApplication {
            id: 10,
            uuid: "appuuid".to_owned(),
            docker_registry_image_name: None,
            docker_images_to_keep: images_to_keep,
        }
    }

    fn row(id: i64) -> RetentionScan {
        RetentionScan {
            id,
            resource_type: APPLICATION_MORPH.to_owned(),
            resource_id: 10,
            image_id: format!("sha256:{id:064}"),
            image_ref: "appuuid:latest".to_owned(),
            deployment_queue_id: None,
            pull_request_id: None,
        }
    }

    fn running_image(hex: &str) -> HashSet<String> {
        HashSet::from([format!("{APPLICATION_MORPH}:10:{}", image_id(hex))])
    }

    fn deployments(commits: &[(i64, &str)]) -> Vec<(i64, Option<String>)> {
        commits
            .iter()
            .map(|(id, commit)| (*id, Some((*commit).to_owned())))
            .collect()
    }

    #[test]
    fn keeps_the_running_image_the_rollback_list_and_the_last_5_scans() {
        let app = application(Some(2));
        // The finished deployments, the latest first: c1 (id 3), c2 (id 2), c3 (id 1).
        let rollback = rollback_list(
            images_to_keep(false, &app),
            app.repository(),
            &deployments(&[(3, "c1"), (2, "c2"), (1, "c3")]),
            &["appuuid:abc123".to_owned()],
        );
        assert_eq!(
            rollback,
            Rollback {
                deployments: vec![3, 2],
                refs: vec!["appuuid:c1".to_owned(), "appuuid:c2".to_owned()],
            }
        );

        // The scans of the application, the latest first.
        let mut scans: Vec<RetentionScan> = (1..=5).map(row).collect();
        let mut old = row(6);
        old.image_ref = "appuuid:c0".to_owned();
        let mut too_old = row(7);
        too_old.deployment_queue_id = Some(1);
        too_old.pull_request_id = Some(0);
        let mut rollback_by_tag = row(8);
        rollback_by_tag.image_ref = "appuuid:c2".to_owned();
        let mut rollback_by_deployment = row(9);
        rollback_by_deployment.deployment_queue_id = Some(3);
        rollback_by_deployment.pull_request_id = Some(0);
        let mut running = row(10);
        running.image_id = image_id(HEX);
        scans.extend([
            old,
            too_old,
            rollback_by_tag,
            rollback_by_deployment,
            running,
        ]);

        assert_eq!(
            retention_deletes(&scans, &running_image(HEX), &rollback, Some(&app), &[]),
            vec![6, 7]
        );
    }

    #[test]
    fn keeps_the_latest_images_to_keep_besides_the_running_one() {
        let app = application(Some(2));
        // c1 runs: CleanupDocker keeps c2 and c3 for rollbacks.
        let rollback = rollback_list(
            images_to_keep(false, &app),
            app.repository(),
            &deployments(&[(4, "c1"), (3, "c2"), (2, "c3"), (1, "c4")]),
            &["appuuid:c1".to_owned()],
        );
        assert_eq!(rollback.deployments, vec![3, 2]);
        assert_eq!(rollback.refs, vec!["appuuid:c2", "appuuid:c3"]);

        let mut scans: Vec<RetentionScan> = (1..=5).map(row).collect();
        for (id, deployment) in [(11, 1), (12, 2), (13, 3), (14, 4)] {
            let mut scan = row(id);
            scan.deployment_queue_id = Some(deployment);
            scan.pull_request_id = Some(0);
            if deployment == 4 {
                scan.image_id = image_id(HEX);
            }
            scans.push(scan);
        }
        assert_eq!(
            retention_deletes(&scans, &running_image(HEX), &rollback, Some(&app), &[]),
            vec![11]
        );
    }

    #[test]
    fn keeps_no_rollback_images_when_the_server_turned_off_image_retention() {
        let app = application(Some(2));
        assert_eq!(images_to_keep(true, &app), 0);
        assert_eq!(images_to_keep(false, &app), 2);
        // Without a settings row the default of the column counts.
        assert_eq!(images_to_keep(false, &application(None)), 2);
        assert_eq!(images_to_keep(false, &application(Some(-1))), 0);
        let rollback = rollback_list(
            images_to_keep(true, &app),
            app.repository(),
            &deployments(&[(1, "c1")]),
            &[],
        );
        assert_eq!(rollback, Rollback::default());

        let mut scans: Vec<RetentionScan> = (1..=5).map(row).collect();
        let mut of_deployment = row(6);
        of_deployment.deployment_queue_id = Some(1);
        of_deployment.pull_request_id = Some(0);
        scans.push(of_deployment);
        assert_eq!(
            retention_deletes(&scans, &HashSet::new(), &rollback, Some(&app), &[]),
            vec![6]
        );
    }

    #[test]
    fn the_rollback_list_uses_the_registry_image_name_and_each_commit_once() {
        let mut app = application(Some(3));
        app.docker_registry_image_name = Some("ghcr.io/acme/app".to_owned());
        let mut finished = deployments(&[(5, "c1"), (4, "c1"), (3, "c2")]);
        finished.push((2, None));
        finished.push((1, Some(String::new())));
        let rollback = rollback_list(
            images_to_keep(false, &app),
            app.repository(),
            &finished,
            &[],
        );
        assert_eq!(rollback.deployments, vec![5, 4, 3]);
        assert_eq!(
            rollback.refs,
            vec!["ghcr.io/acme/app:c1", "ghcr.io/acme/app:c2"]
        );
        // The tag is the first 128 characters of the commit.
        let long = "a".repeat(140);
        assert_eq!(commit_tag(&long).len(), 128);
        assert_eq!(commit_tag("abc"), "abc");
    }

    #[test]
    fn keeps_the_scan_of_an_image_that_production_runs_when_a_closed_preview_shared_it() {
        let app = application(Some(2));
        let mut scans: Vec<RetentionScan> = (1..=5).map(row).collect();
        let mut shared = row(6);
        shared.image_id = image_id(HEX);
        shared.deployment_queue_id = Some(9);
        shared.pull_request_id = Some(9);
        scans.push(shared);
        assert!(
            retention_deletes(
                &scans,
                &running_image(HEX),
                &Rollback::default(),
                Some(&app),
                &[]
            )
            .is_empty()
        );
        // The preview is closed and nothing runs the image.
        assert_eq!(
            retention_deletes(
                &scans,
                &HashSet::new(),
                &Rollback::default(),
                Some(&app),
                &[]
            ),
            vec![6]
        );
    }

    #[test]
    fn deletes_the_scans_of_a_preview_that_no_longer_exists() {
        let app = application(Some(2));
        let mut live_preview = row(1);
        live_preview.deployment_queue_id = Some(70);
        live_preview.pull_request_id = Some(7);
        let mut closed_by_deployment = row(2);
        closed_by_deployment.deployment_queue_id = Some(80);
        closed_by_deployment.pull_request_id = Some(8);
        let mut closed_by_tag = row(3);
        closed_by_tag.image_ref = "appuuid:pr-9-abc".to_owned();
        let mut production = row(4);
        production.image_ref = "appuuid:abc".to_owned();
        let scans = [
            live_preview,
            closed_by_deployment,
            closed_by_tag,
            production,
        ];

        assert_eq!(
            retention_deletes(
                &scans,
                &HashSet::new(),
                &Rollback::default(),
                Some(&app),
                &[7]
            ),
            vec![2, 3]
        );
        // A resource that is not an application has no previews.
        assert!(
            retention_deletes(&scans, &HashSet::new(), &Rollback::default(), None, &[]).is_empty()
        );
    }

    #[test]
    fn reads_the_pull_request_of_a_preview_scan() {
        let app = application(Some(2));
        let tagged = |image_ref: &str| {
            let mut scan = row(1);
            scan.image_ref = image_ref.to_owned();
            preview_pull_request_id(&scan, Some(&app))
        };
        assert_eq!(tagged("appuuid:pr-9-abc"), Some(9));
        assert_eq!(tagged("appuuid:pr-12-"), Some(12));
        assert_eq!(tagged("appuuid:abc"), None);
        assert_eq!(tagged("appuuid:pr-9"), None);
        assert_eq!(tagged("appuuid:pr--abc"), None);
        assert_eq!(tagged("other:pr-9-abc"), None);
        assert_eq!(tagged("registry/appuuid:pr-9-abc"), None);
        // The deployment wins over the tag.
        let mut of_deployment = row(1);
        of_deployment.image_ref = "appuuid:pr-9-abc".to_owned();
        of_deployment.pull_request_id = Some(4);
        assert_eq!(preview_pull_request_id(&of_deployment, Some(&app)), Some(4));
        assert_eq!(preview_pull_request_id(&of_deployment, None), None);
    }

    #[test]
    fn retention_constants_match_upstream() {
        assert_eq!(KEEP_LATEST_SCANS, 5);
        assert_eq!(SBOM_UNUSED_DAYS, 30);
        assert_eq!(MAX_SBOM_DELETES, 500);
        assert_eq!(RESCAN_TIMEOUT.as_secs(), 36000);
        assert_eq!(
            ended_ignores_cache_key(3, day(20)),
            "image-scan:ended-ignores:3:2026-10-20"
        );
        assert!(IMAGE_ID.is_match(&image_id(HEX)));
        assert!(!IMAGE_ID.is_match("sha256:abc"));
    }
}
