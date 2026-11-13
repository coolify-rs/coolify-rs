//! `App\Actions\ImageScan\ScanImage`: scans one local image on a server with Syft and Grype and
//! stores the result for a resource (spec 4.1).
//!
//! A scan row exists only for an image ID that was inspected. Progress lives in the cache
//! ([`progress_cache_key`]), the last failure too ([`last_error_cache_key`]).

use std::sync::LazyLock;
use std::time::Duration;

use chrono::{DateTime, NaiveDateTime, SubsecRound, Utc};
use coolify_compat::yaml::PhpValue;
use coolify_db::image_scans::{
    self, ImageSbomRow, ImageScanRow, STATUS_COMPLETED, STATUS_FAILED, SbomValues, ScanOwner,
    ScanValue,
};
use regex::Regex;
use serde_json::{Map, Value, json};

use super::cache::{self, CacheLock};
use super::commands::{self, OUTPUT_LIMIT, ParsedOutput, ScannerCommands, ScannerHost};
use super::error::{ErrorCode, ScanFailed, mb_prefix};
use super::finding::Finding;
use super::grype::{self, EolDistro};
use super::image_ref::ImageRef;
use super::policy::{self, ResolvedPolicy};
use super::resource_scans;
use super::rules::IgnoreMatcher;
use super::{ScanServer, events, new_run_id};
use crate::Jobs;

/// ScannerCommands budgets plus 60 s.
const TIMEOUT_DEFAULT: i64 = 120;
const TIMEOUT_DB_UPDATE: i64 = 960;
const TIMEOUT_SAVE: i64 = 600;
const TIMEOUT_PACKAGES: i64 = 360;
/// SSH time on top of the time limit of a scanner container.
const STEP_MARGIN: i64 = 60;
const TIMEOUT_PULL: i64 = 660;
const DB_MAX_AGE_DAYS: i64 = 5;
const DISK_RESERVE_BYTES: i64 = 1_073_741_824;
/// Exit codes of `timeout`: stopped, or killed after the grace period.
const EXIT_TIMEOUT: [i64; 2] = [124, 137];
/// Exit code of a failed `docker run`, for example a missing bind mount source.
const EXIT_DOCKER_RUN: i64 = 125;

/// `lockTtl()` without its SBOM and its two Grype runs.
const LOCK_TTL_WITHOUT_SCANNERS: i64 = 2 * TIMEOUT_DEFAULT
    + TIMEOUT_DB_UPDATE
    + 2 * TIMEOUT_DEFAULT
    + TIMEOUT_PULL
    + TIMEOUT_DEFAULT
    + TIMEOUT_PACKAGES
    + TIMEOUT_DEFAULT;
/// SBOM, packages, and two Grype runs at the highest Syft and Grype time limits: one candidate
/// scan at most.
pub const MAX_SCANNER_STEPS_SECONDS: i64 = TIMEOUT_SAVE
    + commands::MAX_SYFT_TIMEOUT
    + STEP_MARGIN
    + TIMEOUT_PACKAGES
    + 2 * (commands::MAX_GRYPE_TIMEOUT + STEP_MARGIN);
/// `lockTtl()` at the highest Syft and Grype time limits: for job timeouts and marks that do not
/// know the server.
pub const MAX_LOCK_TTL: i64 = LOCK_TTL_WITHOUT_SCANNERS
    + TIMEOUT_SAVE
    + commands::MAX_SYFT_TIMEOUT
    + STEP_MARGIN
    + 2 * (commands::MAX_GRYPE_TIMEOUT + STEP_MARGIN);
/// One wait of a deploy gate for the server lock (spec 5); background scans never wait. The gate
/// waits again while the lock is busy, see `DeploymentImageScan::gateLockWaitSeconds()`.
pub const GATE_LOCK_WAIT_SECONDS: u64 = 120;
pub const LAST_ERROR_TTL_DAYS: u64 = 7;

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).unwrap_or_else(|_| unreachable!())
}

static IMAGE_ID: LazyLock<Regex> = LazyLock::new(|| re(r"\Asha256:([a-f0-9]{64})\z"));
static PLATFORM: LazyLock<Regex> = LazyLock::new(|| re(r"\A[a-z0-9]+/[a-z0-9_]+\z"));
static SIZE: LazyLock<Regex> = LazyLock::new(|| re(r"\A\d{1,18}\z"));
static BUILT: LazyLock<Regex> =
    LazyLock::new(|| re(r"\A\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d+)?(Z|[+-]\d{2}:\d{2})\z"));
static DISK_FREE: LazyLock<Regex> = LazyLock::new(|| re(r"(\d+) +(\d+)\s*\z"));
static TAG_SUFFIX: LazyLock<Regex> = LazyLock::new(|| re(r":[^/]*$"));

/// The deployment a scan belongs to (`?ApplicationDeploymentQueue $deployment`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanDeployment {
    /// `application_deployment_queues.id`.
    pub id: i64,
    /// 0 unless the deployment is a preview.
    pub pull_request_id: i64,
}

/// The optional arguments of [`handle`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanOptions {
    pub deployment: Option<ScanDeployment>,
    /// A deploy gate waits for the server lock; 0 tries once (background scans).
    pub lock_wait_seconds: u64,
    /// The base image of the final stage of a Dockerfile build (spec 7.4), stored on the scan.
    pub base_image_ref: Option<String>,
    /// Queue the check for newer versions; the deploy gate queues it itself.
    pub queue_candidate_check: bool,
    /// The caller retries later when the lock is busy: a busy lock is not recorded as the last
    /// error.
    pub retry_when_busy: bool,
    /// Pull an image that is not on the server ("Scan now"). Images that Coolify builds are never
    /// pulled.
    pub pull_when_missing: bool,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            deployment: None,
            lock_wait_seconds: 0,
            base_image_ref: None,
            queue_candidate_check: true,
            retry_when_busy: false,
            pull_when_missing: false,
        }
    }
}

/// What `docker image inspect` says about an image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InspectedImage {
    /// `sha256:{hex}`.
    pub id: String,
    pub hex: String,
    pub repo_digest: Option<String>,
    /// `{os}/{arch}`.
    pub platform: String,
    pub size: i64,
}

/// The result of one command batch, see [`run_step`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepResult {
    /// The exit code of the step that was asked for.
    pub code: i64,
    /// The output size in bytes that the step reported.
    pub size: Option<i64>,
    pub payload: String,
    /// Every marker of the batch.
    pub parsed: ParsedOutput,
}

/// A scan that stopped: with an error code, or with an error that is not a scan problem (the
/// database, a bug), which callers pass on.
#[derive(Debug)]
enum Stop {
    Failed(ScanFailed),
    Other(String),
}

impl From<ScanFailed> for Stop {
    fn from(e: ScanFailed) -> Self {
        Stop::Failed(e)
    }
}

impl From<coolify_db::DbError> for Stop {
    fn from(e: coolify_db::DbError) -> Self {
        Stop::Other(e.to_string())
    }
}

impl From<String> for Stop {
    fn from(e: String) -> Self {
        Stop::Other(e)
    }
}

/// One run of the action.
#[derive(Debug)]
struct Run<'a> {
    jobs: &'a Jobs,
    server: &'a ScanServer,
    run_id: String,
    lock_ttl: i64,
    started_at: DateTime<Utc>,
    pull_when_missing: bool,
}

/// The parsed Grype output of a scan, sorted by risk.
#[derive(Debug, Clone)]
struct Report {
    findings: Vec<Finding>,
    eol: Option<EolDistro>,
    truncated: bool,
}

/// `ScanImage::handle()`. Never fails for scan problems. Callers such as the deploy gate must use
/// the returned scan: on a failure it is an unsaved scan with status failed and an error code,
/// and a completed scan row of the same image keeps its results. A failed row is saved only when
/// no row exists yet for the inspected image.
pub async fn handle(
    jobs: &Jobs,
    server: &ScanServer,
    resource: &ScanOwner,
    image_ref: &str,
    options: ScanOptions,
) -> Result<ImageScanRow, String> {
    let run = Run::new(jobs, server, options.pull_when_missing);
    let policy = policy::resolve(&jobs.0.db, resource, Some(server.id())).await?;
    let context = server_context(server);
    if server.is_transferred_away() || !policy.scan_enabled {
        return Ok(run.unsaved_failure(
            resource,
            image_ref,
            None,
            ErrorCode::ScanningOff,
            &context,
            None,
        ));
    }
    if let Err(e) = ImageRef::parse(image_ref) {
        let failure =
            ScanFailed::detail(ErrorCode::Unknown, format!("invalid image reference: {e}"));
        failure.log(&server.name(), server.uuid(), image_ref);
        return Ok(run.fail_before_row(
            resource,
            image_ref,
            None,
            ErrorCode::Unknown,
            &failure.error_context(&context),
            None,
        ));
    }

    let lock = server_lock(server);
    if !acquire_server_lock(&lock, server.id(), options.lock_wait_seconds).await {
        return Ok(if options.retry_when_busy {
            run.unsaved_failure(
                resource,
                image_ref,
                None,
                ErrorCode::LockBusy,
                &context,
                None,
            )
        } else {
            run.fail_before_row(
                resource,
                image_ref,
                None,
                ErrorCode::LockBusy,
                &context,
                None,
            )
        });
    }
    let scanned = run
        .scan(
            resource,
            image_ref,
            options.deployment,
            &policy,
            &context,
            valid_base_image_ref(options.base_image_ref.as_deref()),
        )
        .await;
    lock.release();
    let scan = scanned?;

    notify_after_scan(jobs, &scan, options.deployment.map(|d| d.id)).await;
    if options.queue_candidate_check {
        queue_candidate_check(jobs, &scan).await;
    }
    Ok(scan)
}

/// `ScanImage::rescan()`: rescans one local image for every resource that runs it on the server
/// (spec 4.5): one SBOM and one Grype run, then the row of each resource, with its own summary.
/// Never fails for scan problems; a failure is recorded per resource like in [`handle`], and
/// completed rows keep their results. Each target is a resource with the image ref of its
/// container.
pub async fn rescan(
    jobs: &Jobs,
    server: &ScanServer,
    image_id: &str,
    targets: Vec<(ScanOwner, String)>,
    lock_wait_seconds: u64,
    retry_when_busy: bool,
) -> Result<Vec<ImageScanRow>, String> {
    let run = Run::new(jobs, server, false);
    let context = server_context(server);
    if server.is_transferred_away() {
        return Ok(Vec::new());
    }
    let mut enabled: Vec<(ScanOwner, String, ResolvedPolicy)> = Vec::new();
    for (resource, image_ref) in targets {
        let policy = policy::resolve(&jobs.0.db, &resource, Some(server.id())).await?;
        if policy.scan_enabled {
            enabled.push((resource, image_ref, policy));
        }
    }
    if enabled.is_empty() {
        return Ok(Vec::new());
    }
    if !IMAGE_ID.is_match(image_id) {
        let failure = ScanFailed::detail(ErrorCode::Unknown, "invalid image ID");
        failure.log(&server.name(), server.uuid(), image_id);
        let error_context = failure.error_context(&context);
        return Ok(enabled
            .iter()
            .map(|(resource, image_ref, _)| {
                run.fail_before_row(
                    resource,
                    image_ref,
                    None,
                    ErrorCode::Unknown,
                    &error_context,
                    None,
                )
            })
            .collect());
    }

    let lock = server_lock(server);
    if !acquire_server_lock(&lock, server.id(), lock_wait_seconds).await {
        return Ok(enabled
            .iter()
            .map(|(resource, image_ref, _)| {
                if retry_when_busy {
                    run.unsaved_failure(
                        resource,
                        image_ref,
                        Some(image_id),
                        ErrorCode::LockBusy,
                        &context,
                        None,
                    )
                } else {
                    run.fail_before_row(
                        resource,
                        image_ref,
                        Some(image_id),
                        ErrorCode::LockBusy,
                        &context,
                        None,
                    )
                }
            })
            .collect());
    }
    let scanned = run.scan_shared(image_id, &enabled, &context).await;
    lock.release();
    let scans = scanned?;

    for scan in &scans {
        notify_after_scan(jobs, scan, None).await;
    }
    Ok(scans)
}

/// `['server' => (string) $server->name]`.
fn server_context(server: &ScanServer) -> Map<String, Value> {
    let mut context = Map::new();
    context.insert("server".to_owned(), Value::String(server.name()));
    context
}

/// `notifyAfterScan()`: records what the notifications covered and alerts for new problems that
/// attackers are using or are likely to use (spec 15.8). A failure is reported, never passed on.
async fn notify_after_scan(jobs: &Jobs, scan: &ImageScanRow, deployment_id: Option<i64>) {
    if let Err(e) = super::notify::after_scan(jobs, scan, deployment_id, false, false).await {
        tracing::error!(error = %e, "ScanNotifications::afterScan failed");
    }
}

/// `queueCandidateCheck()`: looks for a newer version that fixes the findings in a queued job,
/// after the lock is released, so a deploy never waits for it (spec 7.1).
async fn queue_candidate_check(jobs: &Jobs, scan: &ImageScanRow) {
    match super::candidates::should_refresh(jobs, scan).await {
        Ok(true) => super::candidates::queue_check(jobs, scan),
        Ok(false) => {}
        Err(e) => tracing::error!(error = %e, "RefreshCandidates::shouldRefresh failed"),
    }
}

/// `ScanImage::lockTtl($server)`: the longest run of [`handle`] under the lock: database status,
/// update, and status again, inspect, pull, and inspect again, Grype on a stored SBOM that is
/// gone, disk check, SBOM, packages, Grype, cleanup. The lock must not expire under a scan that
/// still runs.
pub fn lock_ttl(host: &ScannerHost) -> i64 {
    LOCK_TTL_WITHOUT_SCANNERS + sbom_step_timeout(host) + 2 * grype_step_timeout(host)
}

/// `ScanImage::gateScanReserveSeconds($server)`: the deploy time that a gate keeps for its own
/// scan (SBOM and Grype) after it waited for the lock.
pub fn gate_scan_reserve_seconds(host: &ScannerHost) -> i64 {
    sbom_step_timeout(host) + grype_step_timeout(host)
}

/// `ScanImage::sbomStepTimeout($server)`: SSH timeout of one SBOM step on the server:
/// `docker save` of a local image, then Syft.
pub fn sbom_step_timeout(host: &ScannerHost) -> i64 {
    TIMEOUT_SAVE + host.syft_timeout() + STEP_MARGIN
}

/// `ScanImage::grypeStepTimeout($server)`: SSH timeout of one Grype run on the server.
pub fn grype_step_timeout(host: &ScannerHost) -> i64 {
    host.grype_timeout() + STEP_MARGIN
}

/// `ScanImage::serverLock($server)`: one scan or scan data change at a time per server
/// (spec 4.1).
pub fn server_lock(server: &ScanServer) -> CacheLock {
    CacheLock::new(
        format!("image-scan:{}", server.id()),
        Duration::from_secs(lock_ttl(&server.host()).unsigned_abs()),
    )
}

/// `ScanImage::acquireServerLock()`: takes the server lock. A deploy gate (`lock_wait_seconds`
/// above 0) that has to wait marks it with [`gate_waiting_cache_key`], so background scans leave
/// the lock to it. A background run (0) tries once and does not take the lock while a gate
/// waits; it retries later.
pub async fn acquire_server_lock(lock: &CacheLock, server_id: i64, lock_wait_seconds: u64) -> bool {
    if lock_wait_seconds == 0 {
        return !is_gate_waiting(server_id) && lock.get();
    }
    if lock.get() {
        return true;
    }
    let key = gate_waiting_cache_key(server_id);
    cache::put(
        &key,
        Value::Bool(true),
        Duration::from_secs(lock_wait_seconds + 5),
    );
    let taken = lock.block(lock_wait_seconds).await;
    cache::forget(&key);
    taken
}

/// `ScanImage::gateWaitingCacheKey($server)`.
pub fn gate_waiting_cache_key(server_id: i64) -> String {
    format!("image-scan:gate-waiting:{server_id}")
}

/// `ScanImage::isGateWaiting($server)`: a deploy gate waits for the server lock.
pub fn is_gate_waiting(server_id: i64) -> bool {
    cache::has(&gate_waiting_cache_key(server_id))
}

/// `ScanImage::lockWasBusy($scan)`: the scan could not run because the server lock was busy and
/// the caller retries later (see `retry_when_busy`).
pub fn lock_was_busy(scan: &ImageScanRow) -> bool {
    !scan.exists()
        && scan.status == STATUS_FAILED
        && scan.error_code.as_deref() == Some(ErrorCode::LockBusy.value())
}

/// `ScanImage::progressCacheKey($resource)`: holds `{run_id, stage, started_at}` while a scan of
/// the resource runs. Stages: database, inspect, pull, sbom, vulnerabilities.
pub fn progress_cache_key(resource: &ScanOwner) -> String {
    format!("image-scan:progress:{}:{}", resource.morph, resource.id)
}

/// `ScanImage::lastErrorCacheKey($resource)`: holds `{code, context, at}` of the last failure,
/// for 7 days or until a scan completes.
pub fn last_error_cache_key(resource: &ScanOwner) -> String {
    format!("image-scan:last-error:{}:{}", resource.morph, resource.id)
}

/// `validBaseImageRef()`: a base image reference that `ImageRef` accepts, else `None`.
fn valid_base_image_ref(reference: Option<&str>) -> Option<String> {
    let reference = reference?;
    (reference.chars().count() <= 512 && ImageRef::parse(reference).is_ok())
        .then(|| reference.to_owned())
}

/// `Carbon::toIso8601String()` of a UTC time.
fn iso8601(at: DateTime<Utc>) -> String {
    at.format("%Y-%m-%dT%H:%M:%S+00:00").to_string()
}

impl<'a> Run<'a> {
    fn new(jobs: &'a Jobs, server: &'a ScanServer, pull_when_missing: bool) -> Self {
        Self {
            jobs,
            server,
            run_id: new_run_id(),
            lock_ttl: lock_ttl(&server.host()),
            started_at: Utc::now(),
            pull_when_missing,
        }
    }

    fn db(&self) -> &coolify_db::Db {
        &self.jobs.0.db
    }

    /// `scan()`.
    async fn scan(
        &self,
        resource: &ScanOwner,
        image_ref: &str,
        deployment: Option<ScanDeployment>,
        policy: &ResolvedPolicy,
        context: &Map<String, Value>,
        base_image_ref: Option<String>,
    ) -> Result<ImageScanRow, String> {
        let commands = self
            .server
            .commands(&self.run_id)
            .map_err(|e| e.to_string())?;
        let mut image: Option<InspectedImage> = None;
        let outcome = self
            .scan_steps(
                &commands,
                resource,
                image_ref,
                deployment,
                policy,
                base_image_ref,
                &mut image,
            )
            .await;
        let result = match outcome {
            Ok(scan) => Ok(scan),
            Err(Stop::Failed(e)) => {
                e.log(&self.server.name(), self.server.uuid(), image_ref);
                let error_context = e.error_context(context);
                match &image {
                    None => Ok(self.fail_before_row(
                        resource,
                        image_ref,
                        None,
                        e.code,
                        &error_context,
                        None,
                    )),
                    Some(image) => {
                        self.fail_after_inspect(
                            resource,
                            image_ref,
                            deployment,
                            image,
                            e.code,
                            &error_context,
                        )
                        .await
                    }
                }
            }
            Err(Stop::Other(message)) => {
                let failure = ScanFailed::detail(ErrorCode::Unknown, message.clone());
                failure.log(&self.server.name(), self.server.uuid(), image_ref);
                self.remember_error(
                    resource,
                    ErrorCode::Unknown,
                    &failure.error_context(context),
                );
                Err(message)
            }
        };
        self.forget_progress(resource);
        self.server.run(&commands.cleanup(), TIMEOUT_DEFAULT).await;
        result
    }

    #[allow(clippy::too_many_arguments)]
    async fn scan_steps(
        &self,
        commands: &ScannerCommands,
        resource: &ScanOwner,
        image_ref: &str,
        deployment: Option<ScanDeployment>,
        policy: &ResolvedPolicy,
        base_image_ref: Option<String>,
        image: &mut Option<InspectedImage>,
    ) -> Result<ImageScanRow, Stop> {
        let is_baseline =
            !image_scans::has_completed_scan(self.db(), resource.morph, resource.id).await?;
        self.progress(resource, "database");
        let database_built_at = database_built_at(self.server, commands, Jobs::now()).await?;
        self.progress(resource, "inspect");
        let inspected = self.inspect_or_pull(commands, resource, image_ref).await?;
        *image = Some(inspected.clone());
        self.progress(resource, "sbom");
        let (sbom, grype) = self
            .sbom_and_report(
                commands,
                std::slice::from_ref(resource),
                image_ref,
                &inspected,
            )
            .await?;
        let report = report(&grype);
        self.complete(
            resource,
            image_ref,
            deployment,
            policy,
            &sbom,
            &report,
            &inspected,
            database_built_at,
            is_baseline,
            base_image_ref,
        )
        .await
    }

    /// `scanShared()`.
    async fn scan_shared(
        &self,
        image_id: &str,
        targets: &[(ScanOwner, String, ResolvedPolicy)],
        context: &Map<String, Value>,
    ) -> Result<Vec<ImageScanRow>, String> {
        let commands = self
            .server
            .commands(&self.run_id)
            .map_err(|e| e.to_string())?;
        let resources: Vec<ScanOwner> = targets.iter().map(|(r, _, _)| r.clone()).collect();
        let mut image: Option<InspectedImage> = None;
        let outcome = self
            .scan_shared_steps(&commands, image_id, targets, &resources, &mut image)
            .await;
        let result = match outcome {
            Ok(scans) => Ok(scans),
            Err(Stop::Failed(e)) => {
                e.log(&self.server.name(), self.server.uuid(), image_id);
                let error_context = e.error_context(context);
                let mut scans = Vec::with_capacity(targets.len());
                let mut failed = None;
                for (resource, image_ref, _) in targets {
                    let scan = match &image {
                        None => Ok(self.fail_before_row(
                            resource,
                            image_ref,
                            None,
                            e.code,
                            &error_context,
                            None,
                        )),
                        Some(image) => {
                            self.fail_after_inspect(
                                resource,
                                image_ref,
                                None,
                                image,
                                e.code,
                                &error_context,
                            )
                            .await
                        }
                    };
                    match scan {
                        Ok(scan) => scans.push(scan),
                        Err(message) => {
                            failed = Some(message);
                            break;
                        }
                    }
                }
                match failed {
                    Some(message) => Err(message),
                    None => Ok(scans),
                }
            }
            Err(Stop::Other(message)) => {
                let failure = ScanFailed::detail(ErrorCode::Unknown, message.clone());
                failure.log(&self.server.name(), self.server.uuid(), image_id);
                for resource in &resources {
                    self.remember_error(
                        resource,
                        ErrorCode::Unknown,
                        &failure.error_context(context),
                    );
                }
                Err(message)
            }
        };
        for resource in &resources {
            self.forget_progress(resource);
        }
        self.server.run(&commands.cleanup(), TIMEOUT_DEFAULT).await;
        result
    }

    async fn scan_shared_steps(
        &self,
        commands: &ScannerCommands,
        image_id: &str,
        targets: &[(ScanOwner, String, ResolvedPolicy)],
        resources: &[ScanOwner],
        image: &mut Option<InspectedImage>,
    ) -> Result<Vec<ImageScanRow>, Stop> {
        let mut is_baseline = Vec::with_capacity(resources.len());
        for resource in resources {
            is_baseline.push(
                !image_scans::has_completed_scan(self.db(), resource.morph, resource.id).await?,
            );
        }
        self.progress_all(resources, "database");
        let database_built_at = database_built_at(self.server, commands, Jobs::now()).await?;
        self.progress_all(resources, "inspect");
        // By ID, not by a tag that can point to another image by now.
        let inspected = inspect(self.server, commands, image_id).await?;
        if inspected.id != image_id {
            return Err(ScanFailed::new(ErrorCode::ImageMissing).into());
        }
        *image = Some(inspected.clone());
        self.progress_all(resources, "sbom");
        let (sbom, grype) = self
            .sbom_and_report(commands, resources, image_id, &inspected)
            .await?;
        let report = report(&grype);
        let mut scans = Vec::with_capacity(targets.len());
        for (index, (resource, image_ref, policy)) in targets.iter().enumerate() {
            scans.push(
                self.complete(
                    resource,
                    image_ref,
                    None,
                    policy,
                    &sbom,
                    &report,
                    &inspected,
                    database_built_at,
                    is_baseline[index],
                    None,
                )
                .await?,
            );
        }
        Ok(scans)
    }

    /// `progress()`.
    fn progress(&self, resource: &ScanOwner, stage: &str) {
        cache::put(
            &progress_cache_key(resource),
            json!({
                "run_id": self.run_id,
                "stage": stage,
                "started_at": iso8601(self.started_at),
            }),
            Duration::from_secs(self.lock_ttl.unsigned_abs()),
        );
        self.broadcast(resource, &self.run_id, false);
    }

    fn progress_all(&self, resources: &[ScanOwner], stage: &str) {
        for resource in resources {
            self.progress(resource, stage);
        }
    }

    /// `inspectOrPull()`: inspects the image. With `pull_when_missing`, an image that is not on
    /// the server and that Coolify does not build is pulled first.
    async fn inspect_or_pull(
        &self,
        commands: &ScannerCommands,
        resource: &ScanOwner,
        image_ref: &str,
    ) -> Result<InspectedImage, ScanFailed> {
        match inspect(self.server, commands, image_ref).await {
            Ok(image) => return Ok(image),
            Err(e) => {
                if e.code != ErrorCode::ImageMissing
                    || !self.pull_when_missing
                    || builds_image(resource, image_ref)
                {
                    return Err(e);
                }
            }
        }
        self.progress(resource, "pull");
        let pull = commands.pull(image_ref).map_err(invalid_argument)?;
        let result = run_step(self.server, &pull, "pull", TIMEOUT_PULL).await?;
        if result.code != 0 {
            let code = if EXIT_TIMEOUT.contains(&result.code) {
                ErrorCode::Timeout
            } else {
                ErrorCode::PullFailed
            };
            return Err(ScanFailed::detail(
                code,
                format!("step pull exited with code {}", result.code),
            ));
        }
        self.progress(resource, "inspect");
        inspect(self.server, commands, image_ref).await
    }

    /// `sbomAndReport()`: reuses the SBOM of the image on this server. When its file is gone,
    /// Grype cannot mount it: the file is created once more and the row is updated in place
    /// (scans of other resources keep their link).
    async fn sbom_and_report(
        &self,
        commands: &ScannerCommands,
        resources: &[ScanOwner],
        image_ref: &str,
        image: &InspectedImage,
    ) -> Result<(ImageSbomRow, StepResult), Stop> {
        let grype_commands = commands.grype(&image.hex).map_err(invalid_argument)?;
        let grype_timeout = grype_step_timeout(&commands.host);
        let stored = image_scans::find_sbom(self.db(), self.server.id(), &image.id).await?;
        if let Some(sbom) = &stored {
            self.progress_all(resources, "vulnerabilities");
            let grype = run_step(self.server, &grype_commands, "grype", grype_timeout).await?;
            if grype.code != EXIT_DOCKER_RUN {
                assert_succeeded(&grype, "grype")?;
                image_scans::touch_sbom(self.db(), sbom.id, Jobs::now()).await?;
                return Ok((sbom.clone(), grype));
            }
            self.progress_all(resources, "sbom");
        }
        let sbom = self.create_sbom(commands, image_ref, image, stored).await?;
        self.progress_all(resources, "vulnerabilities");
        let grype = run_step(self.server, &grype_commands, "grype", grype_timeout).await?;
        assert_succeeded(&grype, "grype")?;
        Ok((sbom, grype))
    }

    /// `createSbom()`.
    async fn create_sbom(
        &self,
        commands: &ScannerCommands,
        image_ref: &str,
        image: &InspectedImage,
        sbom: Option<ImageSbomRow>,
    ) -> Result<ImageSbomRow, Stop> {
        check_disk_space(self.server, commands, image.size).await?;
        let save = commands
            .sbom_from_local(image_ref, &image.hex)
            .map_err(invalid_argument)?;
        let result = run_step(
            self.server,
            &save,
            "save",
            sbom_step_timeout(&commands.host),
        )
        .await?;
        // `docker save` has no memory limit: its 137 is always a timeout kill.
        assert_succeeded(&result, "save")?;
        assert_succeeded(&result, "sbom")?;
        let list = commands.packages(&image.hex).map_err(invalid_argument)?;
        let packages = run_step(self.server, &list, "packages", TIMEOUT_PACKAGES).await?;
        assert_succeeded(&packages, "packages")?;
        let (payload, cut) = read_payload(&packages);
        let parsed = grype::parse_syft_packages(&payload);
        let stored_packages = Value::Array(parsed.packages.iter().map(|p| p.to_value()).collect());
        let values = SbomValues {
            platform: &image.platform,
            packages: &stored_packages,
            packages_truncated: parsed.truncated || cut,
            last_used_at: Jobs::now(),
        };
        if let Some(sbom) = sbom
            && let Some(updated) = image_scans::update_sbom(self.db(), sbom.id, &values).await?
        {
            return Ok(updated);
        }
        Ok(image_scans::create_sbom(self.db(), self.server.id(), &image.id, &values).await?)
    }

    /// `complete()`.
    #[allow(clippy::too_many_arguments)]
    async fn complete(
        &self,
        resource: &ScanOwner,
        image_ref: &str,
        deployment: Option<ScanDeployment>,
        _policy: &ResolvedPolicy,
        sbom: &ImageSbomRow,
        report: &Report,
        image: &InspectedImage,
        database_built_at: NaiveDateTime,
        is_baseline: bool,
        base_image_ref: Option<String>,
    ) -> Result<ImageScanRow, Stop> {
        let now = Jobs::now();
        let existing =
            image_scans::find_scan_of_image(self.db(), resource.morph, resource.id, &image.id)
                .await?;
        let ignores = IgnoreMatcher::for_resource(self.db(), resource, now).await?;
        let summary = resource_scans::summary_for(
            &report.findings,
            &ignores,
            report.eol.as_ref(),
            report.truncated,
        );
        let mut values: Vec<(&'static str, ScanValue)> = vec![
            ("server_id", ScanValue::Int(Some(self.server.id()))),
            ("image_sbom_id", ScanValue::Int(Some(sbom.id))),
            ("repo_digest", ScanValue::Text(image.repo_digest.clone())),
            (
                "image_ref",
                ScanValue::Text(Some(mb_prefix(image_ref, 512))),
            ),
        ];
        if let Some(id) = self
            .deployment_link(resource, existing.as_ref(), deployment)
            .await?
        {
            values.push(("deployment_queue_id", ScanValue::Int(Some(id))));
        }
        if let Some(base) = base_image_ref {
            values.push(("base_image_ref", ScanValue::Text(Some(base))));
        }
        values.extend([
            ("status", ScanValue::Text(Some(STATUS_COMPLETED.to_owned()))),
            ("error_code", ScanValue::Text(None)),
            ("error_context", ScanValue::Json(None)),
            ("summary", ScanValue::Json(Some(summary))),
            (
                "findings",
                ScanValue::Json(Some(Value::Array(
                    report.findings.iter().map(Finding::to_value).collect(),
                ))),
            ),
            (
                "builds_image",
                ScanValue::Bool(builds_image(resource, image_ref)),
            ),
            ("is_baseline", ScanValue::Bool(is_baseline)),
            (
                "grype_db_built_at",
                ScanValue::Time(Some(database_built_at)),
            ),
            ("scanned_at", ScanValue::Time(Some(now))),
        ]);
        let mut scan = self.upsert(resource, &image.id, &values, now).await?;
        resource_scans::store_security_status(self.jobs, &mut scan, resource).await?;
        if let Some(saved) = image_scans::update_scan(
            self.db(),
            scan.id,
            &[("summary", ScanValue::Json(scan.summary.clone()))],
            now,
        )
        .await?
        {
            scan = saved;
        }
        cache::forget(&last_error_cache_key(resource));
        self.forget_progress(resource);
        self.broadcast(resource, &scan.uuid, true);
        Ok(scan)
    }

    /// `deploymentLink()`: the deployment a scan row belongs to, which decides the current scans
    /// of an application (all images of one deployment). A scan without a deployment (rescan,
    /// Scan now) joins the latest finished deployment of the application, unless the row belongs
    /// to a newer deployment. A preview never takes over the row of the application. `None`
    /// leaves the link as it is.
    async fn deployment_link(
        &self,
        resource: &ScanOwner,
        existing: Option<&ImageScanRow>,
        deployment: Option<ScanDeployment>,
    ) -> Result<Option<i64>, Stop> {
        let linked = existing.and_then(|scan| scan.deployment_queue_id);
        let linked_to_preview = match linked {
            Some(id) => image_scans::deployment_facts(self.db(), id)
                .await?
                .is_some_and(|d| d.pull_request_id != 0),
            None => false,
        };
        if let Some(deployment) = deployment {
            return Ok(
                if deployment.pull_request_id != 0 && existing.is_some() && !linked_to_preview {
                    None
                } else {
                    Some(deployment.id)
                },
            );
        }
        if !resource.is_application() || linked_to_preview {
            return Ok(None);
        }
        let latest = image_scans::latest_finished_deployment(self.db(), resource.id)
            .await?
            .map(|(id, _)| id);
        Ok(match (latest, linked) {
            (None, _) => None,
            (Some(latest), Some(linked)) if linked >= latest => None,
            (Some(latest), _) => Some(latest),
        })
    }

    /// `upsert()`: creates or updates the row of (resource, image ID). A row that another run
    /// inserts at the same time is updated.
    async fn upsert(
        &self,
        resource: &ScanOwner,
        image_id: &str,
        values: &[(&'static str, ScanValue)],
        now: NaiveDateTime,
    ) -> Result<ImageScanRow, Stop> {
        let (scan, created) = image_scans::first_or_create_scan(
            self.db(),
            resource.morph,
            resource.id,
            image_id,
            values,
            now,
        )
        .await?;
        if created {
            return Ok(scan);
        }
        Ok(image_scans::update_scan(self.db(), scan.id, values, now)
            .await?
            .unwrap_or(scan))
    }

    /// `failAfterInspect()`: keeps an existing row of the image (and its results). Saves a failed
    /// row only for an image without one.
    async fn fail_after_inspect(
        &self,
        resource: &ScanOwner,
        image_ref: &str,
        deployment: Option<ScanDeployment>,
        image: &InspectedImage,
        code: ErrorCode,
        context: &Map<String, Value>,
    ) -> Result<ImageScanRow, String> {
        let values: Vec<(&'static str, ScanValue)> = vec![
            ("server_id", ScanValue::Int(Some(self.server.id()))),
            ("repo_digest", ScanValue::Text(image.repo_digest.clone())),
            (
                "image_ref",
                ScanValue::Text(Some(mb_prefix(image_ref, 512))),
            ),
            (
                "deployment_queue_id",
                ScanValue::Int(deployment.map(|d| d.id)),
            ),
            ("status", ScanValue::Text(Some(STATUS_FAILED.to_owned()))),
            ("error_code", ScanValue::Text(Some(code.value().to_owned()))),
            (
                "error_context",
                ScanValue::Json(Some(Value::Object(context.clone()))),
            ),
            (
                "builds_image",
                ScanValue::Bool(builds_image(resource, image_ref)),
            ),
        ];
        let (scan, created) = image_scans::first_or_create_scan(
            self.db(),
            resource.morph,
            resource.id,
            &image.id,
            &values,
            Jobs::now(),
        )
        .await
        .map_err(|e| e.to_string())?;
        if !created {
            return Ok(self.fail_before_row(
                resource,
                image_ref,
                Some(&image.id),
                code,
                context,
                image.repo_digest.clone(),
            ));
        }
        self.remember_error(resource, code, context);
        self.forget_progress(resource);
        self.broadcast(resource, &scan.uuid, true);
        Ok(scan)
    }

    /// `failBeforeRow()`.
    fn fail_before_row(
        &self,
        resource: &ScanOwner,
        image_ref: &str,
        image_id: Option<&str>,
        code: ErrorCode,
        context: &Map<String, Value>,
        repo_digest: Option<String>,
    ) -> ImageScanRow {
        let scan = self.unsaved_failure(resource, image_ref, image_id, code, context, repo_digest);
        // Another run of the resource is scanning: its progress stays, and a busy lock is not
        // its error.
        let other_run_active = cache::get(&progress_cache_key(resource))
            .and_then(|p| p.get("run_id").and_then(Value::as_str).map(str::to_owned))
            .is_some_and(|run_id| run_id != self.run_id);
        if !(other_run_active && code == ErrorCode::LockBusy) {
            self.remember_error(resource, code, context);
        }
        self.forget_progress(resource);
        self.broadcast(resource, &self.run_id, true);
        scan
    }

    /// `forgetProgress()`: removes the progress of the resource only when it is the progress of
    /// this run.
    fn forget_progress(&self, resource: &ScanOwner) {
        let key = progress_cache_key(resource);
        let own = cache::get(&key)
            .is_some_and(|p| p.get("run_id").and_then(Value::as_str) == Some(self.run_id.as_str()));
        if own {
            cache::forget(&key);
        }
    }

    /// `unsavedFailure()`.
    fn unsaved_failure(
        &self,
        resource: &ScanOwner,
        image_ref: &str,
        image_id: Option<&str>,
        code: ErrorCode,
        context: &Map<String, Value>,
        repo_digest: Option<String>,
    ) -> ImageScanRow {
        ImageScanRow {
            id: 0,
            uuid: String::new(),
            server_id: self.server.id(),
            image_sbom_id: None,
            image_id: image_id.unwrap_or_default().to_owned(),
            repo_digest,
            resource_type: resource.morph.to_owned(),
            resource_id: resource.id,
            image_ref: mb_prefix(image_ref, 512),
            deployment_queue_id: None,
            status: STATUS_FAILED.to_owned(),
            error_code: Some(code.value().to_owned()),
            error_context: Some(Value::Object(context.clone())),
            summary: None,
            findings: None,
            candidates: None,
            base_image_ref: None,
            builds_image: false,
            is_baseline: true,
            grype_db_built_at: None,
            scanned_at: None,
            created_at: None,
            updated_at: None,
        }
    }

    /// `rememberError()`.
    fn remember_error(&self, resource: &ScanOwner, code: ErrorCode, context: &Map<String, Value>) {
        cache::put(
            &last_error_cache_key(resource),
            json!({
                "code": code.value(),
                "context": context,
                "at": iso8601(Utc::now()),
            }),
            Duration::from_secs(LAST_ERROR_TTL_DAYS * 86400),
        );
    }

    /// `broadcast()`: `is_final` is false for a progress update while the scan runs.
    fn broadcast(&self, resource: &ScanOwner, scan_uuid: &str, is_final: bool) {
        events::dispatch(resource.team_id, &resource.uuid, scan_uuid, is_final);
    }
}

fn invalid_argument(e: commands::InvalidArgument) -> ScanFailed {
    ScanFailed::detail(ErrorCode::Unknown, format!("InvalidArgumentException: {e}"))
}

/// `report()`: the findings of the Grype output, sorted by risk.
fn report(grype: &StepResult) -> Report {
    let (payload, cut) = read_payload(grype);
    let parsed = grype::parse_grype_report(&payload);
    let mut findings = parsed.findings;
    findings.sort_by(|a, b| b.risk.unwrap_or(0.0).total_cmp(&a.risk.unwrap_or(0.0)));
    Report {
        findings,
        eol: parsed.eol,
        truncated: parsed.truncated || cut,
    }
}

/// `ScanImage::databaseBuiltAt($commands)`: the build time (UTC) of the vulnerability database
/// on the server. Updates a missing database once. Grype refuses a database older than 5 days.
pub async fn database_built_at(
    server: &ScanServer,
    commands: &ScannerCommands,
    now: NaiveDateTime,
) -> Result<NaiveDateTime, ScanFailed> {
    let built_at = match database_status(server, commands).await? {
        Some(built_at) => built_at,
        None => {
            server.run(&commands.db_update(), TIMEOUT_DB_UPDATE).await;
            database_status(server, commands).await?.ok_or_else(|| {
                ScanFailed::detail(
                    ErrorCode::DbMissing,
                    "no valid vulnerability database after the update",
                )
            })?
        }
    };
    database_age_check(built_at, now)?;
    Ok(built_at)
}

/// The age rule of [`database_built_at`].
fn database_age_check(built_at: NaiveDateTime, now: NaiveDateTime) -> Result<(), ScanFailed> {
    if built_at < now - chrono::Duration::days(DB_MAX_AGE_DAYS) {
        let mut context = Map::new();
        context.insert("n".to_owned(), Value::from((now - built_at).num_days()));
        return Err(ScanFailed::with(ErrorCode::DbTooOld, context, None));
    }
    Ok(())
}

async fn database_status(
    server: &ScanServer,
    commands: &ScannerCommands,
) -> Result<Option<NaiveDateTime>, ScanFailed> {
    let result = run_step(server, &commands.db_status(), "db-status", TIMEOUT_DEFAULT).await?;
    if EXIT_TIMEOUT.contains(&result.code) {
        return Err(ScanFailed::detail(
            ErrorCode::Timeout,
            format!("step db-status exited with code {}", result.code),
        ));
    }
    Ok(if result.code == 0 {
        built_time(&result.payload)
    } else {
        None
    })
}

/// The `built` time of `grype db status -o json` when the database is valid.
fn built_time(payload: &str) -> Option<NaiveDateTime> {
    let status: Value = serde_json::from_str(payload).ok()?;
    if status.get("valid") != Some(&Value::Bool(true)) {
        return None;
    }
    let built = status.get("built")?.as_str()?;
    if !BUILT.is_match(built) {
        return None;
    }
    DateTime::parse_from_rfc3339(built)
        .ok()
        .map(|at| at.with_timezone(&Utc).naive_utc().trunc_subsecs(0))
}

/// `inspect()`.
async fn inspect(
    server: &ScanServer,
    commands: &ScannerCommands,
    image_ref: &str,
) -> Result<InspectedImage, ScanFailed> {
    let list = commands.inspect(image_ref).map_err(invalid_argument)?;
    let result = run_step(server, &list, "inspect", TIMEOUT_DEFAULT).await?;
    if result.code != 0 {
        return Err(ScanFailed::detail(
            ErrorCode::ImageMissing,
            format!("step inspect exited with code {}", result.code),
        ));
    }
    parse_inspect(&result.payload)
}

/// The line `{Id}|{RepoDigest or empty}|{os}/{arch}|{size}` of `inspect`.
fn parse_inspect(payload: &str) -> Result<InspectedImage, ScanFailed> {
    let unexpected = || {
        ScanFailed::detail(
            ErrorCode::Unknown,
            format!(
                "unexpected docker image inspect output: {}",
                ScanFailed::output_tail(Some(payload))
            ),
        )
    };
    let parts: Vec<&str> = coolify_remote::text::php_trim(payload).split('|').collect();
    if parts.len() != 4 || !PLATFORM.is_match(parts[2]) || !SIZE.is_match(parts[3]) {
        return Err(unexpected());
    }
    let Some(id) = IMAGE_ID.captures(parts[0]) else {
        return Err(unexpected());
    };
    let repo_digest = parts[1]
        .rsplit_once('@')
        .map(|(_, digest)| digest)
        .filter(|digest| ImageRef::is_valid_digest(digest))
        .map(str::to_owned);
    Ok(InspectedImage {
        id: parts[0].to_owned(),
        hex: id[1].to_owned(),
        repo_digest,
        platform: parts[2].to_owned(),
        size: parts[3].parse().map_err(|_| unexpected())?,
    })
}

/// `ScanImage::checkDiskSpace($commands, $imageSize)`: `docker save` and the SBOM need image size
/// × 2 + 1 GiB on the Docker root and on the scan directory. Candidate scans from a registry use
/// the same rule (Syft writes the layers to the scratch directory), and host scans use it with
/// the size of the copied package databases.
pub async fn check_disk_space(
    server: &ScanServer,
    commands: &ScannerCommands,
    image_size: i64,
) -> Result<(), ScanFailed> {
    let output = server.run(&commands.disk_free(), TIMEOUT_DEFAULT).await;
    disk_space_check(output.as_deref(), image_size)
}

/// The rule of [`check_disk_space`] over the output of `diskFree()`.
fn disk_space_check(output: Option<&str>, image_size: i64) -> Result<(), ScanFailed> {
    let free = DISK_FREE
        .captures(output.unwrap_or_default())
        .and_then(|c| Some((c[1].parse::<i128>().ok()?, c[2].parse::<i128>().ok()?)));
    let Some((docker_root, scan_dir)) = free else {
        return Err(ScanFailed::detail(
            ErrorCode::Unknown,
            format!(
                "could not read the free disk space: {}",
                ScanFailed::output_tail(output)
            ),
        ));
    };
    let needed = i128::from(image_size) * 2 + i128::from(DISK_RESERVE_BYTES);
    if docker_root < needed || scan_dir < needed {
        let reserve = i128::from(DISK_RESERVE_BYTES);
        let gigabytes = (needed + reserve - 1) / reserve;
        let mut context = Map::new();
        context.insert("n".to_owned(), Value::from(gigabytes as i64));
        return Err(ScanFailed::with(
            ErrorCode::DiskSpace,
            context,
            Some(format!(
                "needed {needed} bytes, free {docker_root} and {scan_dir} bytes"
            )),
        ));
    }
    Ok(())
}

/// `ScanImage::buildsImage($resource, $imageRef)`: whether the team builds the image: Coolify
/// builds the images of Dockerfile, Nixpacks, Railpack, and static apps, and the Compose images
/// of a service with `build`. Docker images of the team are built by the team unless it says
/// otherwise; Docker Hub official images are not. Other images are pulled.
pub fn builds_image(resource: &ScanOwner, image_ref: &str) -> bool {
    if !resource.is_application() {
        return false;
    }
    match resource.build_pack() {
        Some("dockercompose") => compose_builds_image(resource, image_ref),
        Some("dockerimage") => match resource.image_builds_by_team() {
            Some(by_team) => by_team,
            None => !ImageRef::parse(image_ref).is_ok_and(|r| r.is_docker_official()),
        },
        _ => true,
    }
}

/// `composeBuildsImage()`: a Compose image is built by Coolify when its name is
/// `{app uuid}_{service}` (the name the parser gives a service with `build` and no `image`), or
/// when a service with `build` in the app's Compose file names the same image (tag ignored).
/// Every other image of the file is pulled.
fn compose_builds_image(application: &ScanOwner, image_ref: &str) -> bool {
    let name = image_name(image_ref);
    if !application.uuid.trim().is_empty()
        && name.starts_with(&format!("{}_", application.uuid.to_lowercase()))
    {
        return true;
    }
    let Some(compose) = resource_scans::parsed_compose(application) else {
        return false;
    };
    resource_scans::compose_services(&compose)
        .into_iter()
        .any(|(_, service)| {
            resource_scans::service_value(service, "build").is_some()
                && matches!(
                    resource_scans::service_value(service, "image"),
                    Some(PhpValue::Str(image)) if !image.is_empty() && image_name(image) == name
                )
        })
}

/// `ScanImage::imageName($imageRef)`: the image reference without tag and digest, lowercase.
pub fn image_name(image_ref: &str) -> String {
    let name = image_ref.split('@').next().unwrap_or_default();
    let name = coolify_remote::text::php_trim(name).to_lowercase();
    TAG_SUFFIX.replace(&name, "").into_owned()
}

/// `ScanImage::runStep()`: runs one command batch and reads the exit code of `step`. No output or
/// no marker means the batch did not run.
pub async fn run_step(
    server: &ScanServer,
    commands: &[String],
    step: &str,
    timeout_seconds: i64,
) -> Result<StepResult, ScanFailed> {
    let output = server.run(commands, timeout_seconds).await;
    step_result(output.as_deref(), step)
}

/// The pure part of [`run_step`].
pub fn step_result(output: Option<&str>, step: &str) -> Result<StepResult, ScanFailed> {
    let parsed = commands::parse_output(output);
    let blank = output.is_none_or(|o| coolify_remote::text::php_trim(o).is_empty());
    match parsed.exit_code(step) {
        Some(code) if !blank => Ok(StepResult {
            code,
            size: parsed.size(step),
            payload: parsed.payload.clone(),
            parsed,
        }),
        _ => Err(ScanFailed::detail(
            ErrorCode::Unknown,
            format!(
                "step {step} did not report an exit code (the command did not run or the SSH connection failed), output: {}",
                ScanFailed::output_tail(output)
            ),
        )),
    }
}

/// `ScanImage::assertSucceeded($result, $step)`: fails for a failed `step` of the batch. Only
/// Docker's own `OOMKilled` (the `coolify-scan-oom` line of the memory-limited Syft and Grype
/// containers) means out of memory; a 137 without it was killed by `timeout`.
pub fn assert_succeeded(result: &StepResult, step: &str) -> Result<(), ScanFailed> {
    let code = result.parsed.exit_code(step);
    let step_context = || {
        let mut context = Map::new();
        context.insert("step".to_owned(), Value::String(step.to_owned()));
        context
    };
    if code != Some(0) && step != "save" && result.parsed.oom.iter().any(|s| s == step) {
        return Err(ScanFailed::with(
            ErrorCode::OutOfMemory,
            step_context(),
            Some(format!(
                "step {step} was killed because it ran out of memory"
            )),
        ));
    }
    match code {
        Some(0) => Ok(()),
        Some(code) if EXIT_TIMEOUT.contains(&code) => Err(ScanFailed::with(
            ErrorCode::Timeout,
            step_context(),
            Some(format!("step {step} exited with code {code} (timeout)")),
        )),
        other => Err(ScanFailed::detail(
            ErrorCode::Unknown,
            format!(
                "step {step} exited with code {}, all exit codes: {}",
                other.map_or_else(|| "none".to_owned(), |c| c.to_string()),
                result.parsed.exit_codes_json()
            ),
        )),
    }
}

/// `ScanImage::readPayload($result)`: output over the limit was cut by `head -c`: keep only the
/// complete lines that were read. The boolean says whether the payload was cut.
pub fn read_payload(result: &StepResult) -> (String, bool) {
    let payload = &result.payload;
    let cut = result.size.unwrap_or(0) > OUTPUT_LIMIT as i64 || payload.len() > OUTPUT_LIMIT;
    if !cut {
        return (payload.clone(), false);
    }
    let bytes = &payload.as_bytes()[..payload.len().min(OUTPUT_LIMIT)];
    let kept = match bytes.iter().rposition(|b| *b == b'\n') {
        // A line break is one byte, so the cut is at a character boundary.
        Some(last_newline) => payload[..last_newline].to_owned(),
        None => String::new(),
    };
    (kept, true)
}

#[cfg(test)]
mod tests {
    use chrono::NaiveDate;
    use coolify_db::image_scans::APPLICATION_MORPH;

    use super::super::resource_scans::tests::owner;
    use super::*;

    fn at(day: u32, hour: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 11, day)
            .unwrap()
            .and_hms_opt(hour, 0, 0)
            .unwrap()
    }

    fn step(output: &str, name: &str) -> StepResult {
        step_result(Some(output), name).unwrap()
    }

    #[test]
    fn lock_budgets() {
        let host = ScannerHost::default();
        assert_eq!(sbom_step_timeout(&host), 600 + 600 + 60);
        assert_eq!(grype_step_timeout(&host), 660);
        assert_eq!(gate_scan_reserve_seconds(&host), 1260 + 660);
        assert_eq!(lock_ttl(&host), 2700 + 1260 + 2 * 660);
        let slow = ScannerHost {
            syft_timeout: Some(3600),
            grype_timeout: Some(3600),
            ..ScannerHost::default()
        };
        assert_eq!(lock_ttl(&slow), MAX_LOCK_TTL);
        assert_eq!(MAX_SCANNER_STEPS_SECONDS, 600 + 3600 + 60 + 360 + 2 * 3660);
    }

    #[test]
    fn a_step_needs_its_marker() {
        let ok = step(
            "coolify-scan-exit grype 0 12\ncoolify-scan-payload\n{}",
            "grype",
        );
        assert_eq!(ok.code, 0);
        assert_eq!(ok.size, Some(12));
        assert_eq!(ok.payload, "{}");

        for output in [None, Some("  \n"), Some("coolify-scan-exit sbom 0")] {
            let failed = step_result(output, "grype").unwrap_err();
            assert_eq!(failed.code, ErrorCode::Unknown);
            assert!(failed.detail.unwrap().starts_with(
                "step grype did not report an exit code (the command did not run or the SSH connection failed), output: "
            ));
        }
    }

    #[test]
    fn failed_steps_get_their_error_code() {
        let ok = step("coolify-scan-exit save 0\ncoolify-scan-exit sbom 0", "save");
        assert!(assert_succeeded(&ok, "save").is_ok());
        assert!(assert_succeeded(&ok, "sbom").is_ok());

        let oom = step(
            "coolify-scan-oom sbom\ncoolify-scan-exit save 0\ncoolify-scan-exit sbom 137",
            "save",
        );
        let failed = assert_succeeded(&oom, "sbom").unwrap_err();
        assert_eq!(failed.code, ErrorCode::OutOfMemory);
        assert_eq!(failed.context.get("step"), Some(&json!("sbom")));
        assert_eq!(
            failed.detail.as_deref(),
            Some("step sbom was killed because it ran out of memory")
        );

        // A 137 without Docker's OOMKilled was a timeout kill.
        let killed = step("coolify-scan-exit grype 137 0", "grype");
        let failed = assert_succeeded(&killed, "grype").unwrap_err();
        assert_eq!(failed.code, ErrorCode::Timeout);
        assert_eq!(
            failed.detail.as_deref(),
            Some("step grype exited with code 137 (timeout)")
        );
        assert_eq!(
            assert_succeeded(&step("coolify-scan-exit grype 124 0", "grype"), "grype")
                .unwrap_err()
                .code,
            ErrorCode::Timeout
        );
        // `docker save` has no memory limit.
        let save = step("coolify-scan-oom save\ncoolify-scan-exit save 137", "save");
        assert_eq!(
            assert_succeeded(&save, "save").unwrap_err().code,
            ErrorCode::Timeout
        );

        let other = step("coolify-scan-exit save 0\ncoolify-scan-exit sbom 1", "save");
        let failed = assert_succeeded(&other, "sbom").unwrap_err();
        assert_eq!(failed.code, ErrorCode::Unknown);
        assert_eq!(
            failed.detail.as_deref(),
            Some(r#"step sbom exited with code 1, all exit codes: {"save":0,"sbom":1}"#)
        );
        let missing = assert_succeeded(&other, "packages").unwrap_err();
        assert_eq!(
            missing.detail.as_deref(),
            Some(r#"step packages exited with code none, all exit codes: {"save":0,"sbom":1}"#)
        );
    }

    #[test]
    fn a_cut_payload_keeps_complete_lines() {
        let whole = step(
            "coolify-scan-exit grype 0 7\ncoolify-scan-payload\n{\"a\":1}",
            "grype",
        );
        assert_eq!(read_payload(&whole), ("{\"a\":1}".to_owned(), false));

        // The step reported more bytes than the limit: the last line may be cut.
        let reported = step(
            &format!(
                "coolify-scan-exit grype 0 {}\ncoolify-scan-payload\n{{\"a\":1}}\n{{\"b\":",
                OUTPUT_LIMIT + 1
            ),
            "grype",
        );
        assert_eq!(read_payload(&reported), ("{\"a\":1}".to_owned(), true));

        let mut long = step(
            "coolify-scan-exit grype 0\ncoolify-scan-payload\nx",
            "grype",
        );
        long.payload = format!("{}\n{}", "é".repeat(OUTPUT_LIMIT / 2 - 1), "é".repeat(8));
        let (kept, cut) = read_payload(&long);
        assert!(cut);
        assert_eq!(kept.len(), OUTPUT_LIMIT - 2);
        long.payload = "x".repeat(OUTPUT_LIMIT + 1);
        assert_eq!(read_payload(&long), (String::new(), true));
    }

    #[test]
    fn inspect_output() {
        let hex = "a".repeat(64);
        let digest = format!("sha256:{}", "b".repeat(64));
        let image = parse_inspect(&format!(
            "sha256:{hex}|ghcr.io/a/b@{digest}|linux/amd64|12345\n"
        ))
        .unwrap();
        assert_eq!(
            image,
            InspectedImage {
                id: format!("sha256:{hex}"),
                hex: hex.clone(),
                repo_digest: Some(digest),
                platform: "linux/amd64".to_owned(),
                size: 12345,
            }
        );
        // A local image has no repo digest.
        assert_eq!(
            parse_inspect(&format!("sha256:{hex}||linux/arm64|1"))
                .unwrap()
                .repo_digest,
            None
        );
        for bad in [
            String::new(),
            format!("sha256:{hex}||linux/amd64"),
            format!("{hex}||linux/amd64|1"),
            format!("sha256:{hex}||linux|1"),
            format!("sha256:{hex}||linux/amd64|-1"),
            format!("sha256:{hex}||linux/amd64|1|x"),
        ] {
            let failed = parse_inspect(&bad).unwrap_err();
            assert_eq!(failed.code, ErrorCode::Unknown, "{bad}");
            assert!(
                failed
                    .detail
                    .unwrap()
                    .starts_with("unexpected docker image inspect output: ")
            );
        }
    }

    #[test]
    fn disk_space_needs_twice_the_image_and_a_reserve() {
        let gib = DISK_RESERVE_BYTES;
        assert!(disk_space_check(Some(&format!("{} {}", 3 * gib, 3 * gib)), gib).is_ok());
        let failed =
            disk_space_check(Some(&format!("{} {}\n", 3 * gib, 3 * gib - 1)), gib).unwrap_err();
        assert_eq!(failed.code, ErrorCode::DiskSpace);
        assert_eq!(failed.context.get("n"), Some(&json!(3)));
        assert_eq!(
            failed.detail,
            Some(format!(
                "needed {} bytes, free {} and {} bytes",
                3 * gib,
                3 * gib,
                3 * gib - 1
            ))
        );
        // The needed size is rounded up to whole GB.
        let failed = disk_space_check(Some("0 0"), 1).unwrap_err();
        assert_eq!(failed.context.get("n"), Some(&json!(2)));
        let unreadable = disk_space_check(Some("df: no such file"), gib).unwrap_err();
        assert_eq!(unreadable.code, ErrorCode::Unknown);
        assert_eq!(
            unreadable.detail.as_deref(),
            Some("could not read the free disk space: df: no such file")
        );
        assert_eq!(
            disk_space_check(None, gib).unwrap_err().code,
            ErrorCode::Unknown
        );
    }

    #[test]
    fn the_database_status() {
        assert_eq!(
            built_time(r#"{"schemaVersion":"v6.1.3","built":"2026-11-09T04:10:11Z","valid":true}"#),
            Some(at(9, 4) + chrono::Duration::seconds(611))
        );
        assert_eq!(
            built_time(r#"{"built":"2026-11-09T06:00:00.123+02:00","valid":true}"#),
            Some(at(9, 4))
        );
        assert_eq!(
            built_time(r#"{"built":"2026-11-09T04:10:11Z","valid":false}"#),
            None
        );
        assert_eq!(built_time(r#"{"built":"yesterday","valid":true}"#), None);
        assert_eq!(built_time(r#"{"valid":true}"#), None);
        assert_eq!(built_time("not json"), None);

        assert!(database_age_check(at(5, 12), at(10, 12)).is_ok());
        let old = database_age_check(at(3, 11), at(10, 12)).unwrap_err();
        assert_eq!(old.code, ErrorCode::DbTooOld);
        assert_eq!(old.context.get("n"), Some(&json!(7)));
    }

    #[test]
    fn image_names() {
        assert_eq!(image_name("Nginx:1.27"), "nginx");
        assert_eq!(image_name(" ghcr.io/a/b@sha256:abc "), "ghcr.io/a/b");
        assert_eq!(
            image_name("localhost:5000/team/app"),
            "localhost:5000/team/app"
        );
        assert_eq!(
            image_name("localhost:5000/team/app:1"),
            "localhost:5000/team/app"
        );
    }

    #[test]
    fn who_builds_an_image() {
        let app = |record: Value| owner(APPLICATION_MORPH, record);
        assert!(builds_image(
            &app(json!({"build_pack": "nixpacks"})),
            "appuuid:abc"
        ));
        assert!(builds_image(
            &app(json!({"build_pack": "dockerfile"})),
            "appuuid:abc"
        ));
        assert!(!builds_image(
            &owner(r"App\Models\ServiceApplication", json!({})),
            "ghcr.io/a/b:1"
        ));

        // A Docker image of the team counts as built by the team; an official image does not.
        let image = app(json!({"build_pack": "dockerimage"}));
        assert!(builds_image(&image, "ghcr.io/acme/app:1"));
        assert!(!builds_image(&image, "nginx:1.27"));
        assert!(builds_image(&image, "not an image"));
        let mut says_no = image.clone();
        says_no.settings = json!({"image_builds_by_team": false}).as_object().cloned();
        assert!(!builds_image(&says_no, "ghcr.io/acme/app:1"));
        let mut says_yes = image;
        says_yes.settings = json!({"image_builds_by_team": true}).as_object().cloned();
        assert!(builds_image(&says_yes, "nginx:1.27"));

        let compose = app(json!({
            "build_pack": "dockercompose",
            "docker_compose_raw": "services:\n  web:\n    image: 'nginx:1.27'\n  worker:\n    build: .\n    image: ghcr.io/acme/worker:2\n",
        }));
        assert!(builds_image(&compose, "appuuid_api:latest"));
        assert!(builds_image(&compose, "ghcr.io/acme/worker:3"));
        assert!(!builds_image(&compose, "nginx:1.27"));
        assert!(!builds_image(
            &app(json!({"build_pack": "dockercompose", "docker_compose_raw": "a: [b"})),
            "nginx:1.27"
        ));
    }

    #[test]
    fn base_image_refs_are_validated() {
        assert_eq!(
            valid_base_image_ref(Some("node:22-alpine")).as_deref(),
            Some("node:22-alpine")
        );
        assert_eq!(valid_base_image_ref(Some("node:$TAG")), None);
        assert_eq!(valid_base_image_ref(None), None);
    }

    #[test]
    fn a_busy_lock_is_an_unsaved_failed_scan() {
        let mut scan = super::super::resource_scans::tests::scan("nginx:1", STATUS_FAILED, None);
        scan.id = 0;
        scan.error_code = Some("lock_busy".to_owned());
        assert!(lock_was_busy(&scan));
        scan.id = 5;
        assert!(!lock_was_busy(&scan));
        scan.id = 0;
        scan.error_code = Some("timeout".to_owned());
        assert!(!lock_was_busy(&scan));
    }
}
