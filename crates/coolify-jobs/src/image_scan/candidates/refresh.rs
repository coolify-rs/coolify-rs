//! `App\Actions\ImageScan\RefreshCandidates`: finds newer versions of a scanned image that fix
//! vulnerabilities (spec 7.2 and the candidate rules of spec 15.4): the same tag with a newer
//! build, the newest tag of the same line, and the next major for information.
//!
//! Candidates are scanned from the registry on the server: the newer build of the same tag, plus
//! at most [`MAX_CANDIDATE_SCANS`] other tags per image, and [`DAILY_CANDIDATE_SCANS`] per server
//! and day; the SBOM of a candidate digest is kept per server and reused.
//!
//! Writes `image_scans.candidates` as `{status, checked_at, error, tags_truncated, entries}`,
//! with status `skipped` when only the base image was checked. The base image of a Dockerfile
//! build (spec 7.4) is scanned from the registry with the newest tag of its line; the result is
//! under `base`: `{image, status, checked_at, error, tags_truncated, attributable,
//! attributable_ids, current_digest, entries}`. A relevant finding of the app is attributable to
//! the base image when the current base image has it too. `fixed_ids` and the attributable
//! findings count every medium finding, so a change of "Count medium problems" needs no new
//! check; the line candidate fixes the attributable findings it does not have. Base entries never
//! make a finding fixable now.
//!
//! Registry and scan problems never fail the check: they become the status. Registry credentials
//! are read only after an anonymous request failed, only for the registry of the image, and are
//! never stored or logged.

use std::sync::LazyLock;
use std::time::Duration;

use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
use coolify_db::image_scans::candidates::{self as budget, CandidateScanCounters};
use coolify_db::image_scans::{
    self, ImageScanRow, STATUS_COMPLETED, SbomValues, ScanOwner, ScanValue,
};
use regex::Regex;
use serde_json::{Value, json};

use super::evaluator;
use super::registry_login::{self, RegistryLogin};
use super::tag_selector::{self, MAX_TAGS, Selection};
use crate::Jobs;
use crate::image_scan::cache::{self, CacheLock};
use crate::image_scan::commands::{InvalidArgument, ScannerCommands};
use crate::image_scan::finding::Finding;
use crate::image_scan::grype::{self, EolDistro};
use crate::image_scan::image_ref::{ImageRef, InvalidImageRef};
use crate::image_scan::rules::{self, RelevanceRules};
use crate::image_scan::scan_image::{self, StepResult};
use crate::image_scan::{
    ScanServer, events, new_run_id, notify, policy, resource_scans, scan_jobs,
};

/// `RefreshCandidates::MAX_CANDIDATE_SCANS`: scans of other tags per check (the patch and the
/// line tag, or the major); the same-tag build is extra.
pub const MAX_CANDIDATE_SCANS: usize = 2;
/// `RefreshCandidates::DAILY_CANDIDATE_SCANS`: registry scans per server and day.
pub const DAILY_CANDIDATE_SCANS: i64 = 20;
/// `RefreshCandidates::MAJOR_SCAN_DAILY_RESERVE`: the information-only scan of the newest major
/// leaves this many daily scans for the checks that can fix.
pub const MAJOR_SCAN_DAILY_RESERVE: i64 = 5;
/// `RefreshCandidates::FRESH_HOURS`: a check newer than this is not repeated after a scan. The
/// daily rescan is always older.
pub const FRESH_HOURS: i64 = 20;

/// ScannerCommands budgets plus 60 s.
const TIMEOUT_REGISTRY: i64 = 90;
const TIMEOUT_PACKAGES: i64 = 360;
const TIMEOUT_DEFAULT: i64 = 120;
/// Exit codes of `timeout`: stopped, or killed after the grace period.
const EXIT_TIMEOUT: [i64; 2] = [124, 137];
/// Exit code of a failed `docker run`, for example a missing bind mount source.
const EXIT_DOCKER_RUN: i64 = 125;

/// The statuses of a check that does not need a retry.
const FRESH_STATUSES: [&str; 4] = ["ok", "no_better", "cannot_compare", "skipped"];

static IMAGE_ID: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\Asha256:[a-f0-9]{64}\z").unwrap_or_else(|_| unreachable!()));
static SIZE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\A\d{1,18}\z").unwrap_or_else(|_| unreachable!()));

/// Why [`run`] stored no result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefreshError {
    /// `ScanLockBusy`: the scan lock of the server was busy and the caller retries later.
    LockBusy,
    /// An error that is not a registry or scan problem (the database, a bug).
    Other(String),
}

impl std::fmt::Display for RefreshError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RefreshError::LockBusy => f.write_str("The scan lock of the server is busy."),
            RefreshError::Other(e) => f.write_str(e),
        }
    }
}

impl std::error::Error for RefreshError {}

impl From<String> for RefreshError {
    fn from(e: String) -> Self {
        RefreshError::Other(e)
    }
}

impl From<coolify_db::DbError> for RefreshError {
    fn from(e: coolify_db::DbError) -> Self {
        RefreshError::Other(e.to_string())
    }
}

// ---------------------------------------------------------------------------------------------
// The queued mark
// ---------------------------------------------------------------------------------------------

/// `queuedCacheKey($scan)`.
pub(super) fn queued_cache_key(scan_id: i64) -> String {
    format!("image-scan:version-check-queued:{scan_id}")
}

/// `RefreshCandidates::markQueued($scan, $seconds)`: marks the check of the scan as queued, so
/// the Security page says that Coolify is checking for newer versions instead of "Check needed"
/// until the check ends.
pub fn mark_queued(scan: &ImageScanRow, seconds: u64) {
    cache::put(
        &queued_cache_key(scan.id),
        Value::Bool(true),
        Duration::from_secs(seconds),
    );
}

/// `RefreshCandidates::isQueued($scan)`: a queued or running `RefreshCandidatesJob` checks the
/// scan for newer versions.
pub fn is_queued(scan: &ImageScanRow) -> bool {
    cache::has(&queued_cache_key(scan.id))
}

/// `RefreshCandidates::forgetQueued($scan)`: the check ended. Called before the page refresh, so
/// the page shows the result.
pub fn forget_queued(scan: &ImageScanRow) {
    cache::forget(&queued_cache_key(scan.id));
}

// ---------------------------------------------------------------------------------------------
// Whether a check is needed
// ---------------------------------------------------------------------------------------------

/// The checks a scan needs: of the image, and of the base image of a Dockerfile build.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Needs {
    image: bool,
    base: bool,
}

impl Needs {
    fn any(self) -> bool {
        self.image || self.base
    }
}

/// The first part of `needsImageCandidates()`: candidates are looked for only for images the
/// user does not build in Coolify (a Docker image app, or an image a Compose app pulls).
fn image_candidates_apply(scan: &ImageScanRow, owner: &ScanOwner) -> bool {
    if scan.status != STATUS_COMPLETED {
        return false;
    }
    if owner.is_application() {
        let build_pack = owner.build_pack();
        if build_pack != Some("dockerimage")
            && (build_pack != Some("dockercompose") || scan.builds_image)
        {
            return false;
        }
    }
    true
}

/// The first part of `needsBaseCandidates()`: a Dockerfile build with a known base image.
/// Nixpacks, Railpack, and static builds have no base image.
fn base_candidates_apply(scan: &ImageScanRow, owner: &ScanOwner) -> bool {
    scan.status == STATUS_COMPLETED
        && owner.is_application()
        && owner.build_pack() == Some("dockerfile")
        && scan
            .base_image_ref
            .as_deref()
            .is_some_and(|reference| !reference.trim().is_empty())
}

/// `hasEolDistro($scan)`.
fn has_eol_distro(scan: &ImageScanRow) -> bool {
    scan.summary
        .as_ref()
        .and_then(|summary| summary.get("eol_distro"))
        .is_some_and(|eol| eol.is_object() || eol.is_array())
}

/// `needsImageCandidates()` and `needsBaseCandidates()` with the rules of the resource: both
/// need a relevant finding or an end-of-life distro (spec 15.4).
fn needs_with(scan: &ImageScanRow, owner: &ScanOwner, rules: &RelevanceRules) -> Needs {
    let image = image_candidates_apply(scan, owner);
    let base = base_candidates_apply(scan, owner);
    let problem = (image || base)
        && (has_eol_distro(scan)
            || resource_scans::findings(scan)
                .iter()
                .any(|finding| rules::is_relevant(finding, rules)));
    Needs {
        image: image && problem,
        base: base && problem,
    }
}

/// [`needs_with`]; the rules are read only when they decide.
async fn needs(jobs: &Jobs, scan: &ImageScanRow, owner: &ScanOwner) -> Result<Needs, String> {
    let image = image_candidates_apply(scan, owner);
    let base = base_candidates_apply(scan, owner);
    if !image && !base {
        return Ok(Needs::default());
    }
    if has_eol_distro(scan) {
        return Ok(Needs { image, base });
    }
    let rules = RelevanceRules::for_resource(&jobs.0.db, owner, Jobs::now()).await?;
    Ok(needs_with(scan, owner, &rules))
}

/// `$scan->resource`.
async fn owner_of(jobs: &Jobs, scan: &ImageScanRow) -> Result<Option<ScanOwner>, String> {
    image_scans::find_owner(&jobs.0.db, &scan.resource_type, scan.resource_id)
        .await
        .map_err(|e| e.to_string())
}

/// `RefreshCandidates::needsCandidates($scan)`.
pub async fn needs_candidates(jobs: &Jobs, scan: &ImageScanRow) -> Result<bool, String> {
    let Some(owner) = owner_of(jobs, scan).await? else {
        return Ok(false);
    };
    Ok(needs(jobs, scan, &owner).await?.any())
}

/// `RefreshCandidates::relevantFindings($scan, $resource, $rules)`: the relevant findings of the
/// scan (spec 15.2), by the rules of the resource unless `rules` is given.
pub async fn relevant_findings(
    jobs: &Jobs,
    scan: &ImageScanRow,
    resource: &ScanOwner,
    rules: Option<&RelevanceRules>,
) -> Result<Vec<Finding>, String> {
    let findings = resource_scans::findings(scan);
    Ok(match rules {
        Some(rules) => evaluator::relevant(&findings, rules),
        None => {
            let rules = RelevanceRules::for_resource(&jobs.0.db, resource, Jobs::now()).await?;
            evaluator::relevant(&findings, &rules)
        }
    })
}

/// The time of the last check when its status does not need a retry.
fn fresh_checked_at(scan: &ImageScanRow) -> Option<&str> {
    let checked_at = scan
        .candidates
        .as_ref()?
        .as_object()?
        .get("checked_at")?
        .as_str()?;
    FRESH_STATUSES
        .contains(&scan.candidate_status()?)
        .then_some(checked_at)
}

/// `in_array(data_get($candidates, 'base.status'), $fresh, true)`.
fn base_result_is_fresh(scan: &ImageScanRow) -> bool {
    scan.candidates
        .as_ref()
        .and_then(|candidates| candidates.pointer("/base/status"))
        .and_then(Value::as_str)
        .is_some_and(|status| FRESH_STATUSES.contains(&status))
}

/// `now()->subHours(FRESH_HOURS)->lt($checkedAt)`; false for a time that cannot be read.
fn checked_recently(checked_at: &str, now: DateTime<Utc>) -> bool {
    let at = DateTime::parse_from_rfc3339(checked_at)
        .map(|at| at.with_timezone(&Utc))
        .ok()
        .or_else(|| {
            NaiveDateTime::parse_from_str(checked_at, "%Y-%m-%d %H:%M:%S")
                .ok()
                .map(|at| at.and_utc())
        });
    at.is_some_and(|at| now - chrono::Duration::hours(FRESH_HOURS) < at)
}

/// `RefreshCandidates::isFresh($scan)`: the last check finished less than [`FRESH_HOURS`] ago
/// with a result that does not need a retry, also for the base image.
pub async fn is_fresh(jobs: &Jobs, scan: &ImageScanRow) -> Result<bool, String> {
    let Some(checked_at) = fresh_checked_at(scan) else {
        return Ok(false);
    };
    if !base_result_is_fresh(scan) {
        let needs_base = match owner_of(jobs, scan).await? {
            Some(owner) => needs(jobs, scan, &owner).await?.base,
            None => false,
        };
        if needs_base {
            return Ok(false);
        }
    }
    Ok(checked_recently(checked_at, Utc::now()))
}

/// `RefreshCandidates::shouldRefresh($scan)`.
pub async fn should_refresh(jobs: &Jobs, scan: &ImageScanRow) -> Result<bool, String> {
    Ok(scan.exists() && !is_fresh(jobs, scan).await? && needs_candidates(jobs, scan).await?)
}

/// `RefreshCandidates::message($scan, $base)` for a caller that does not know the server of the
/// scan: the texts name it "the server". Callers that have the server use [`message_on`].
pub fn message(scan: &ImageScanRow, base: bool) -> Option<String> {
    message_on(scan, base, None)
}

/// `RefreshCandidates::message($scan, $base)`: text for a version check (or, with `base`, a base
/// check) without a better candidate (spec 7.3, 11). `None` when a candidate is better or
/// nothing was checked. `server_name` is `$scan->server?->name`.
pub fn message_on(scan: &ImageScanRow, base: bool, server_name: Option<&str>) -> Option<String> {
    let candidates = scan.candidates.as_ref();
    let result = if base {
        candidates.and_then(|candidates| candidates.get("base"))
    } else {
        candidates
    };
    let status = result
        .and_then(|result| result.get("status"))
        .and_then(Value::as_str);
    let reference = if base {
        scan.base_image_ref.as_deref().unwrap_or_default()
    } else {
        scan.image_ref.as_str()
    };
    let image = ImageRef::parse(reference).ok()?;
    let name = ImageRef::parse(&image.qualified_name()).ok()?.to_string();
    let server = server_name.unwrap_or("the server");
    match status? {
        "no_better" => Some(format!(
            "No newer version of {name} is better yet. Coolify checks again every day."
        )),
        "cannot_compare" => Some(
            "Coolify cannot compare versions of this image. Check the image page for a newer version."
                .to_owned(),
        ),
        "auth_required" => Some(format!(
            "Coolify cannot read the tags of {name}. Log in to {} on {server}.",
            image.registry
        )),
        "failed" => {
            let daily_limit = result
                .and_then(|result| result.get("error"))
                .and_then(Value::as_str)
                == Some("daily_limit");
            Some(if daily_limit {
                format!(
                    "Coolify reached the daily limit of version checks on {server}. It checks again tomorrow."
                )
            } else {
                format!(
                    "Coolify could not check for newer versions of {name}. It tries again tomorrow."
                )
            })
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------------------------
// The check
// ---------------------------------------------------------------------------------------------

/// `Carbon::toIso8601String()` of a UTC time.
fn iso8601(at: DateTime<Utc>) -> String {
    at.format("%Y-%m-%dT%H:%M:%S+00:00").to_string()
}

/// The result of one check: of the image, or of the base image (which also fills the last three
/// fields).
#[derive(Debug, Clone, PartialEq)]
struct Outcome {
    status: &'static str,
    error: Option<&'static str>,
    entries: Vec<Value>,
    tags_truncated: bool,
    /// The relevant findings of the app that the current base image has too; `None` when that
    /// is not known.
    attributable: Option<usize>,
    attributable_ids: Vec<String>,
    current_digest: Option<String>,
}

impl Outcome {
    fn new(status: &'static str, error: Option<&'static str>, entries: Vec<Value>) -> Self {
        Self {
            status,
            error,
            entries,
            tags_truncated: false,
            attributable: None,
            attributable_ids: Vec::new(),
            current_digest: None,
        }
    }
}

/// The findings of one candidate scan.
#[derive(Debug, Clone, PartialEq)]
struct CandidateReport {
    findings: Vec<Finding>,
    eol: Option<EolDistro>,
    truncated: bool,
}

/// `report($grype)`.
fn report_of(result: &StepResult) -> CandidateReport {
    let (payload, cut) = scan_image::read_payload(result);
    let report = grype::parse_grype_report(&payload);
    CandidateReport {
        findings: report.findings,
        eol: report.eol,
        truncated: report.truncated || cut,
    }
}

/// Why a check stopped without a result.
#[derive(Debug)]
enum Stop {
    /// `ScanLockBusy`.
    LockBusy,
    /// Any other `Throwable`: the check is stored as failed with the error `unknown`.
    Unexpected(String),
}

impl From<String> for Stop {
    fn from(e: String) -> Self {
        Stop::Unexpected(e)
    }
}

impl From<InvalidArgument> for Stop {
    fn from(e: InvalidArgument) -> Self {
        Stop::Unexpected(e.to_string())
    }
}

impl From<InvalidImageRef> for Stop {
    fn from(e: InvalidImageRef) -> Self {
        Stop::Unexpected(e.to_string())
    }
}

/// What a check reads of the scan.
#[derive(Debug, Clone, Copy)]
struct Subject<'a> {
    scan: &'a ImageScanRow,
    /// `RelevanceRules::forResource($resource)`.
    rules: &'a RelevanceRules,
    /// `$scan->sbom?->platform`.
    platform: Option<&'a str>,
}

/// What a check does on the server and in the database.
trait Backend {
    /// `runStep()`: `None` when the batch did not run or printed no marker for `step`.
    async fn run_step(
        &mut self,
        commands: &[String],
        step: &str,
        timeout_seconds: i64,
    ) -> Option<StepResult>;
    /// `DockerRegistryLogins::registryAuthConfig($server, $registry)`.
    async fn registry_login(&mut self, registry: &str) -> RegistryLogin;
    /// `ScanImage::acquireServerLock()`.
    async fn lock(&mut self, lock_wait_seconds: u64) -> bool;
    /// `$lock->release()`.
    fn unlock(&mut self);
    /// The ID of the SBOM row of an image digest on this server.
    async fn find_sbom(&mut self, digest: &str) -> Result<Option<i64>, String>;
    /// `$sbom->forceFill(['last_used_at' => now()])->save()`.
    async fn touch_sbom(&mut self, id: i64) -> Result<(), String>;
    /// Creates the SBOM row of a digest, or fills the row `id` again.
    async fn save_sbom(
        &mut self,
        id: Option<i64>,
        digest: &str,
        platform: &str,
        packages: &Value,
        packages_truncated: bool,
    ) -> Result<(), String>;
    /// `ScanImage::checkDiskSpace($commands, $imageSize)` did not fail.
    async fn has_disk_space(&mut self, commands: &ScannerCommands, image_size: i64) -> bool;
    /// `reserveDailyScan($server)`.
    async fn reserve_daily_scan(&mut self) -> Result<bool, String>;
    /// `dailyScansLeft($server)`.
    async fn daily_scans_left(&mut self) -> Result<i64, String>;
}

/// The registry state of one check: `$auth`, `$loginRead`, `$authRequired`, `$anonymousWorked`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct RegistryState {
    /// The base64 Docker config of the login of the registry, once it was read.
    auth: Option<String>,
    login_read: bool,
    auth_required: bool,
    /// A registry call without credentials worked, so a later failure is not a missing login.
    anonymous_worked: bool,
}

/// What follows the first run of a registry command.
#[derive(Debug, PartialEq, Eq)]
enum FirstCall {
    Payload(String),
    /// A registry error: no login is read.
    Failed,
    /// Read the login of the registry and repeat the command with it.
    ReadLogin,
}

impl RegistryState {
    /// The first run of a registry command, without credentials unless they were read before.
    fn after_first_call(&mut self, result: Option<&StepResult>) -> FirstCall {
        match result {
            Some(result) if result.code == 0 => {
                self.anonymous_worked = self.anonymous_worked || self.auth.is_none();
                FirstCall::Payload(scan_image::read_payload(result).0)
            }
            None => FirstCall::Failed,
            Some(result) if EXIT_TIMEOUT.contains(&result.code) => FirstCall::Failed,
            Some(_) if self.login_read || self.anonymous_worked => FirstCall::Failed,
            Some(_) => {
                self.login_read = true;
                FirstCall::ReadLogin
            }
        }
    }

    /// The login of the registry was read. Returns whether the command is repeated with it.
    /// Without a usable login, a private image needs one.
    fn after_login(&mut self, login: RegistryLogin, docker_official: bool) -> bool {
        match login {
            RegistryLogin::Ok(config) => {
                self.auth = Some(config);
                true
            }
            other => {
                self.auth_required = other == RegistryLogin::Helper || !docker_official;
                false
            }
        }
    }

    /// The run with the login.
    fn after_second_call(&mut self, result: Option<&StepResult>) -> Option<String> {
        match result {
            Some(result) if result.code == 0 => Some(scan_image::read_payload(result).0),
            _ => {
                self.auth_required = true;
                None
            }
        }
    }
}

/// A registry command of [`Check::registry`].
#[derive(Debug, Clone, Copy)]
enum RegistryCall<'a> {
    Tags,
    Digest(&'a str),
}

impl RegistryCall<'_> {
    fn step(self) -> &'static str {
        match self {
            RegistryCall::Tags => "tags",
            RegistryCall::Digest(_) => "digest",
        }
    }
}

/// The lines of a tag list that are not empty.
fn tag_lines(payload: &str) -> Vec<&str> {
    payload
        .split('\n')
        .map(coolify_remote::text::php_trim)
        .filter(|line| !line.is_empty())
        .collect()
}

/// The size of the line `{Id}|{RepoDigest or empty}|{os}/{arch}|{size}` of `inspect`.
fn inspected_size(payload: &str) -> Option<i64> {
    let parts: Vec<&str> = coolify_remote::text::php_trim(payload).split('|').collect();
    (parts.len() == 4 && SIZE.is_match(parts[3]))
        .then(|| parts[3].parse().ok())
        .flatten()
}

fn is_better_entry(entry: &Value) -> bool {
    entry.get("better") == Some(&Value::Bool(true))
}

/// The status of a version check, from its entries and the problems it met.
fn check_status(
    any_better: bool,
    auth_required: bool,
    has_error: bool,
    is_version: bool,
    scanned_any: bool,
) -> &'static str {
    if any_better {
        "ok"
    } else if auth_required {
        "auth_required"
    } else if has_error {
        "failed"
    } else if !is_version && !scanned_any {
        "cannot_compare"
    } else {
        "no_better"
    }
}

/// One entry of `evaluate()`: what the candidate `tag` at `digest` changes for the scan.
#[allow(clippy::too_many_arguments)]
fn candidate_entry(
    image: &ImageRef,
    kind: &str,
    tag: &str,
    digest: &str,
    current: &[Finding],
    current_eol: bool,
    rules: &RelevanceRules,
    report: &CandidateReport,
    scanned_at: &str,
) -> Result<Value, InvalidImageRef> {
    let reference = image.with_tag(tag)?.to_string();
    let current_relevant = evaluator::relevant(current, rules);
    let findings = &report.findings;
    let better = !report.truncated
        && evaluator::is_better(current, findings, rules, current_eol, report.eol.is_some());
    Ok(json!({
        "ref": reference,
        "digest": digest,
        "kind": kind,
        "better": better,
        "fixed_ids": evaluator::fixed_ids(current, findings, &rules.with_all_mediums()),
        "fixes": evaluator::counts(&evaluator::fixed(&current_relevant, findings)).to_value(),
        "added": evaluator::added(&current_relevant, findings, rules),
        "left": evaluator::counts(&evaluator::relevant(findings, rules)).to_value(),
        "eol": report.eol.as_ref().map(EolDistro::to_value),
        "scanned_at": scanned_at,
    }))
}

/// The entry of the newest tag of a higher major that was not scanned.
fn unscanned_major_entry(image: &ImageRef, major_tag: &str) -> Result<Value, InvalidImageRef> {
    let reference = image.with_tag(major_tag)?.to_string();
    Ok(json!({
        "ref": reference,
        "digest": null,
        "kind": "major",
        "better": false,
        "fixed_ids": [],
        "fixes": null,
        "added": null,
        "left": null,
        "eol": null,
        "scanned_at": null,
    }))
}

/// `[...$scanned[0], 'better' => false, 'fixes_problems' => $scanned[0]['better']]`.
fn scanned_major_entry(mut scanned: Value) -> Value {
    let fixes_problems = is_better_entry(&scanned);
    if let Some(entry) = scanned.as_object_mut() {
        entry.insert("better".to_owned(), Value::Bool(false));
        entry.insert("fixes_problems".to_owned(), Value::Bool(fixes_problems));
    }
    scanned
}

/// The relevant findings of the app that the current base image has too. A truncated scan of the
/// current base image cannot tell which findings come from it.
fn attributable_of(
    app_relevant: &[Finding],
    current_base: &CandidateReport,
) -> Option<Vec<Finding>> {
    (!current_base.truncated).then(|| evaluator::shared(app_relevant, &current_base.findings))
}

/// What is written to `image_scans.candidates`.
fn candidates_value(
    base_image_ref: Option<&str>,
    result: &Outcome,
    base: Option<&Outcome>,
    checked_at: &str,
) -> Value {
    let mut value = json!({
        "status": result.status,
        "checked_at": checked_at,
        "error": result.error,
        "tags_truncated": result.tags_truncated,
        "entries": result.entries,
    });
    if let (Some(base), Some(map)) = (base, value.as_object_mut()) {
        map.insert(
            "base".to_owned(),
            json!({
                "image": base_image_ref.unwrap_or_default(),
                "status": base.status,
                "checked_at": checked_at,
                "error": base.error,
                "tags_truncated": base.tags_truncated,
                "attributable": base.attributable,
                "attributable_ids": base.attributable_ids,
                "current_digest": base.current_digest,
                "entries": base.entries,
            }),
        );
    }
    value
}

/// `dailyScansLeft($server)`: the registry scans the server has left today.
fn daily_scans_left(counters: Option<&CandidateScanCounters>, today: NaiveDate) -> i64 {
    match counters {
        Some(counters) if counters.date == Some(today) => {
            (DAILY_CANDIDATE_SCANS - counters.scans_today).max(0)
        }
        _ => DAILY_CANDIDATE_SCANS,
    }
}

/// One run of the action on one server.
#[derive(Debug)]
struct Check<B> {
    backend: B,
    commands: ScannerCommands,
    /// Stop with [`Stop::LockBusy`] instead of storing `lock_busy`, so the caller retries later.
    retry_when_busy: bool,
    registry: RegistryState,
    tags_truncated: bool,
    /// `$this->error`: the first problem of the check.
    error: Option<&'static str>,
    current_image_id: Option<String>,
    current_image_size: Option<i64>,
    /// A command ran on the server, so its work directory is cleaned up.
    ran_on_server: bool,
}

impl<B: Backend> Check<B> {
    fn new(backend: B, commands: ScannerCommands, retry_when_busy: bool) -> Self {
        Self {
            backend,
            commands,
            retry_when_busy,
            registry: RegistryState::default(),
            tags_truncated: false,
            error: None,
            current_image_id: None,
            current_image_size: None,
            ran_on_server: false,
        }
    }

    /// The body of `handle()` between the checks that it is needed and `store()`.
    async fn both(
        &mut self,
        subject: &Subject<'_>,
        needs: Needs,
        lock_wait_seconds: u64,
    ) -> Result<(Outcome, Option<Outcome>), RefreshError> {
        let result = if needs.image {
            self.guarded_image(subject, lock_wait_seconds).await?
        } else {
            Outcome::new("skipped", None, Vec::new())
        };
        let base = if needs.base {
            Some(self.guarded_base(subject, lock_wait_seconds).await?)
        } else {
            None
        };
        Ok((result, base))
    }

    /// `guarded(fn () => $this->check(…))`.
    async fn guarded_image(
        &mut self,
        subject: &Subject<'_>,
        lock_wait_seconds: u64,
    ) -> Result<Outcome, RefreshError> {
        self.reset();
        let result = self.check(subject, lock_wait_seconds).await;
        self.guard(result)
    }

    /// `guarded(fn () => $this->checkBase(…))`.
    async fn guarded_base(
        &mut self,
        subject: &Subject<'_>,
        lock_wait_seconds: u64,
    ) -> Result<Outcome, RefreshError> {
        self.reset();
        let result = self.check_base(subject, lock_wait_seconds).await;
        self.guard(result)
    }

    /// `guarded()`: one check starts with a fresh registry state.
    fn reset(&mut self) {
        self.registry = RegistryState::default();
        self.tags_truncated = false;
        self.error = None;
    }

    /// `guarded()`: an unexpected error becomes a failed check; the result says whether the tag
    /// list was cut.
    fn guard(&self, result: Result<Outcome, Stop>) -> Result<Outcome, RefreshError> {
        let mut outcome = match result {
            Ok(outcome) => outcome,
            Err(Stop::LockBusy) => return Err(RefreshError::LockBusy),
            Err(Stop::Unexpected(e)) => {
                tracing::error!(error = %e, "RefreshCandidates failed");
                Outcome::new("failed", Some("unknown"), Vec::new())
            }
        };
        outcome.tags_truncated = self.tags_truncated;
        Ok(outcome)
    }

    /// `$this->error ??= $error`.
    fn fail(&mut self, error: &'static str) {
        self.error.get_or_insert(error);
    }

    /// `failure()`.
    fn failure(&self) -> Outcome {
        if self.registry.auth_required {
            Outcome::new("auth_required", Some("registry_auth"), Vec::new())
        } else {
            Outcome::new("failed", Some(self.error.unwrap_or("registry")), Vec::new())
        }
    }

    /// The disk check of the registry scans uses the size of the scanned image.
    fn begin(&mut self, image_id: &str, forget_size: bool) {
        self.ran_on_server = true;
        self.current_image_id = Some(image_id.to_owned());
        if forget_size {
            self.current_image_size = None;
        }
    }

    /// `check()`: the version check of the scanned image.
    async fn check(
        &mut self,
        subject: &Subject<'_>,
        lock_wait_seconds: u64,
    ) -> Result<Outcome, Stop> {
        let scan = subject.scan;
        let cannot_compare = || Outcome::new("cannot_compare", None, Vec::new());
        let Ok(image) = ImageRef::parse(&scan.image_ref) else {
            return Ok(cannot_compare());
        };
        if image.digest.is_some() {
            return Ok(cannot_compare());
        }
        let Some(platform) = subject.platform.filter(|platform| !platform.is_empty()) else {
            return Ok(Outcome::new("failed", Some("unknown"), Vec::new()));
        };

        self.begin(&scan.image_id, false);
        let tag = image.tag.clone().unwrap_or_else(|| "latest".to_owned());
        let is_version = tag_selector::is_version(&tag);
        let mut selected = Selection::default();
        if is_version {
            let Some(tags) = self.registry(&image, RegistryCall::Tags).await? else {
                return Ok(self.failure());
            };
            let tags = tag_lines(&tags);
            self.tags_truncated = tags.len() > MAX_TAGS;
            selected = tag_selector::select(&tag, &tags);
        }

        // The smallest change first: a newer build of the same tag, the newest patch of the same
        // minor, then the newest tag of the major. A safer patch ends the search of the line. The
        // same-tag build does not take one of the MAX_CANDIDATE_SCANS slots, so the patch and the
        // line tag always fit.
        let mut entries: Vec<Value> = Vec::new();
        let mut scanned_digests: Vec<String> = Vec::new();
        let mut tag_scans = 0;
        if let Some(repo_digest) = scan.repo_digest.as_deref() {
            match self.digest(&image, &tag).await? {
                None if self.registry.auth_required || !is_version => return Ok(self.failure()),
                Some(digest) if digest != repo_digest => {
                    scanned_digests.push(digest.clone());
                    entries.extend(
                        self.evaluate(
                            &image,
                            subject,
                            "same_tag",
                            &tag,
                            &digest,
                            platform,
                            lock_wait_seconds,
                        )
                        .await?,
                    );
                }
                _ => {}
            }
        }

        let line_tags: Vec<String> = [selected.patch.clone(), selected.line.clone()]
            .into_iter()
            .flatten()
            .filter(|line_tag| *line_tag != tag)
            .collect();
        for line_tag in line_tags {
            let Some(digest) = self.digest(&image, &line_tag).await? else {
                self.fail("registry");
                continue;
            };
            if scan.repo_digest.as_deref() == Some(digest.as_str())
                || scanned_digests.contains(&digest)
            {
                continue;
            }
            scanned_digests.push(digest.clone());
            tag_scans += 1;
            let entry = self
                .evaluate(
                    &image,
                    subject,
                    "line",
                    &line_tag,
                    &digest,
                    platform,
                    lock_wait_seconds,
                )
                .await?;
            let better = entry.as_ref().is_some_and(is_better_entry);
            entries.extend(entry);
            if better {
                break;
            }
        }

        if let Some(major) = selected.major.as_deref() {
            let with_scan = !entries.iter().any(is_better_entry) && tag_scans < MAX_CANDIDATE_SCANS;
            let entry = self
                .major_entry(
                    &image,
                    subject,
                    major,
                    platform,
                    lock_wait_seconds,
                    with_scan,
                )
                .await?;
            entries.push(entry);
        }

        let scanned_any = entries
            .iter()
            .any(|entry| entry.get("kind").and_then(Value::as_str) != Some("major"));
        let status = check_status(
            entries.iter().any(is_better_entry),
            self.registry.auth_required,
            self.error.is_some(),
            is_version,
            scanned_any,
        );
        Ok(Outcome::new(status, self.error, entries))
    }

    /// `majorEntry()`: the newest tag of a higher major. It is never offered as an update: a
    /// major version can need a data migration or code changes. When nothing of the same major is
    /// safer, the check has a scan left, and the server has at least
    /// [`MAJOR_SCAN_DAILY_RESERVE`] daily scans left, Coolify scans it, so the page can say
    /// whether it would fix the problems (`fixes_problems`, the "better" rule). Its registry and
    /// scan problems, also a busy lock, never change the status of the check.
    async fn major_entry(
        &mut self,
        image: &ImageRef,
        subject: &Subject<'_>,
        major_tag: &str,
        platform: &str,
        lock_wait_seconds: u64,
        with_scan: bool,
    ) -> Result<Value, Stop> {
        let entry = unscanned_major_entry(image, major_tag)?;
        if !with_scan || self.backend.daily_scans_left().await? < MAJOR_SCAN_DAILY_RESERVE {
            return Ok(entry);
        }

        let (error, auth_required) = (self.error, self.registry.auth_required);
        let scanned = match self.digest(image, major_tag).await? {
            None => None,
            Some(digest) => {
                match self
                    .evaluate(
                        image,
                        subject,
                        "major",
                        major_tag,
                        &digest,
                        platform,
                        lock_wait_seconds,
                    )
                    .await
                {
                    Ok(scanned) => scanned,
                    Err(Stop::LockBusy) => None,
                    Err(stop) => return Err(stop),
                }
            }
        };
        self.error = error;
        self.registry.auth_required = auth_required;

        Ok(scanned.map_or(entry, scanned_major_entry))
    }

    /// `checkBase()`: scans the current base image and the newest tag of its line from the
    /// registry (spec 7.4): two candidate scans with the usual lock, disk, reuse, and daily
    /// rules. Only a different tag can change the `FROM` line, so a newer build of the same tag
    /// is not checked. The current base image is scanned also when no newer tag exists or the tag
    /// cannot be compared (not a version, or pinned by digest), so `attributable` is set on every
    /// result but a failure, an unparsable reference, or a truncated scan of the current base
    /// image.
    async fn check_base(
        &mut self,
        subject: &Subject<'_>,
        lock_wait_seconds: u64,
    ) -> Result<Outcome, Stop> {
        let scan = subject.scan;
        let rules = subject.rules;
        let Ok(image) = ImageRef::parse(scan.base_image_ref.as_deref().unwrap_or_default()) else {
            return Ok(self.base_result("cannot_compare", Vec::new(), None, None));
        };

        let tag = image.tag.clone().unwrap_or_else(|| "latest".to_owned());
        let comparable = image.digest.is_none() && tag_selector::is_version(&tag);

        let Some(platform) = subject.platform.filter(|platform| !platform.is_empty()) else {
            self.error = Some("unknown");
            return Ok(self.base_result("failed", Vec::new(), None, None));
        };

        // The disk check of the registry scans uses the size of the app image, an upper bound of
        // its base image.
        self.begin(&scan.image_id, true);
        // Every medium counts, so a change of "Count medium problems" needs no new check.
        let app_relevant =
            evaluator::relevant(&resource_scans::findings(scan), &rules.with_all_mediums());

        if !comparable {
            // No newer tag can be chosen, but the current base image (by tag or pinned digest)
            // still tells the origin.
            let current_digest = match image.digest.clone() {
                Some(digest) => Some(digest),
                None => self.digest(&image, &tag).await?,
            };
            let Some(current_digest) = current_digest else {
                return Ok(self.base_failure(None, None));
            };
            let Some(current_base) = self
                .scan_candidate(&image, &current_digest, platform, lock_wait_seconds)
                .await?
            else {
                return Ok(self.base_failure(None, Some(current_digest)));
            };
            let attributable = attributable_of(&app_relevant, &current_base);
            return Ok(self.base_result(
                "cannot_compare",
                Vec::new(),
                attributable.as_deref(),
                Some(current_digest),
            ));
        }

        let Some(tags) = self.registry(&image, RegistryCall::Tags).await? else {
            return Ok(self.base_failure(None, None));
        };
        let tags = tag_lines(&tags);
        self.tags_truncated = tags.len() > MAX_TAGS;
        let line = tag_selector::select(&tag, &tags)
            .line
            .filter(|line| *line != tag);

        let current_digest = self.digest(&image, &tag).await?;
        let line_digest = match (&current_digest, &line) {
            (Some(_), Some(line)) => self.digest(&image, line).await?,
            _ => None,
        };
        let Some(current_digest) = current_digest else {
            return Ok(self.base_failure(None, None));
        };
        if line.is_some() && line_digest.is_none() {
            return Ok(self.base_failure(None, Some(current_digest)));
        }

        // The current base image is scanned also without a newer tag, so the actions know which
        // findings come from it.
        let Some(current_base) = self
            .scan_candidate(&image, &current_digest, platform, lock_wait_seconds)
            .await?
        else {
            return Ok(self.base_failure(None, Some(current_digest)));
        };

        let attributable = attributable_of(&app_relevant, &current_base);
        let (line, line_digest) = match (line, line_digest) {
            (Some(line), Some(line_digest)) if line_digest != current_digest => (line, line_digest),
            _ => {
                return Ok(self.base_result(
                    "no_better",
                    Vec::new(),
                    attributable.as_deref(),
                    Some(current_digest),
                ));
            }
        };

        let Some(candidate) = self
            .scan_candidate(&image, &line_digest, platform, lock_wait_seconds)
            .await?
        else {
            return Ok(self.base_failure(attributable.as_deref(), Some(current_digest)));
        };

        let candidate_findings = &candidate.findings;
        let better = !current_base.truncated
            && !candidate.truncated
            && evaluator::is_better(
                &current_base.findings,
                candidate_findings,
                rules,
                current_base.eol.is_some(),
                candidate.eol.is_some(),
            );
        let attributed = attributable.clone().unwrap_or_default();
        let reference = image.with_tag(&line)?.to_string();
        let entry = json!({
            "ref": reference,
            "digest": line_digest,
            "kind": "line",
            "better": better,
            "fixed_ids": evaluator::fixed_ids(&attributed, candidate_findings, &rules.with_all_mediums()),
            "fixes": evaluator::counts(&evaluator::fixed(&attributed, candidate_findings)).to_value(),
            "added": evaluator::added(
                &evaluator::relevant(&current_base.findings, rules),
                candidate_findings,
                rules,
            ),
            "left": evaluator::counts(&evaluator::relevant(candidate_findings, rules)).to_value(),
            "eol": candidate.eol.as_ref().map(EolDistro::to_value),
            "scanned_at": iso8601(Utc::now()),
        });

        Ok(self.base_result(
            if better { "ok" } else { "no_better" },
            vec![entry],
            attributable.as_deref(),
            Some(current_digest),
        ))
    }

    /// The `$result` closure of `checkBase()`.
    fn base_result(
        &self,
        status: &'static str,
        entries: Vec<Value>,
        attributable: Option<&[Finding]>,
        current_digest: Option<String>,
    ) -> Outcome {
        let mut attributable_ids: Vec<String> = Vec::new();
        for finding in attributable.unwrap_or_default() {
            if !attributable_ids.contains(&finding.id) {
                attributable_ids.push(finding.id.clone());
            }
        }
        Outcome {
            status,
            error: if status == "cannot_compare" {
                None
            } else {
                self.error
            },
            entries,
            tags_truncated: false,
            attributable: attributable.map(<[Finding]>::len),
            attributable_ids,
            current_digest,
        }
    }

    /// The `$failure` closure of `checkBase()`.
    fn base_failure(
        &self,
        attributable: Option<&[Finding]>,
        current_digest: Option<String>,
    ) -> Outcome {
        let failure = self.failure();
        Outcome {
            status: failure.status,
            error: failure.error,
            entries: failure.entries,
            ..self.base_result("failed", Vec::new(), attributable, current_digest)
        }
    }

    /// `evaluate()` for one candidate: its entry, or `None` when it could not be scanned.
    #[allow(clippy::too_many_arguments)]
    async fn evaluate(
        &mut self,
        image: &ImageRef,
        subject: &Subject<'_>,
        kind: &'static str,
        tag: &str,
        digest: &str,
        platform: &str,
        lock_wait_seconds: u64,
    ) -> Result<Option<Value>, Stop> {
        let Some(report) = self
            .scan_candidate(image, digest, platform, lock_wait_seconds)
            .await?
        else {
            return Ok(None);
        };
        let current = resource_scans::findings(subject.scan);
        let entry = candidate_entry(
            image,
            kind,
            tag,
            digest,
            &current,
            has_eol_distro(subject.scan),
            subject.rules,
            &report,
            &iso8601(Utc::now()),
        )?;
        Ok(Some(entry))
    }

    /// `scanCandidate()`: runs Grype on the SBOM of a candidate digest; makes the SBOM from the
    /// registry first when this server has none. Takes the scan lock of the server, so a deploy
    /// scan waits at most one candidate scan. A background check never waits for the lock and
    /// leaves it to a deploy gate that waits.
    async fn scan_candidate(
        &mut self,
        image: &ImageRef,
        digest: &str,
        platform: &str,
        lock_wait_seconds: u64,
    ) -> Result<Option<CandidateReport>, Stop> {
        if !self.backend.lock(lock_wait_seconds).await {
            if self.retry_when_busy {
                return Err(Stop::LockBusy);
            }
            self.fail("lock_busy");
            return Ok(None);
        }
        let report = self.scan_candidate_locked(image, digest, platform).await;
        self.backend.unlock();
        report
    }

    /// The part of `scanCandidate()` under the server lock.
    async fn scan_candidate_locked(
        &mut self,
        image: &ImageRef,
        digest: &str,
        platform: &str,
    ) -> Result<Option<CandidateReport>, Stop> {
        let hex = digest.strip_prefix("sha256:").unwrap_or(digest);
        let grype_commands = self.commands.grype(hex)?;
        let grype_timeout = scan_image::grype_step_timeout(&self.commands.host);
        let sbom = self.backend.find_sbom(digest).await?;

        if let Some(id) = sbom {
            let result = self
                .backend
                .run_step(&grype_commands, "grype", grype_timeout)
                .await;
            match result {
                Some(result) if result.code == 0 => {
                    self.backend.touch_sbom(id).await?;
                    return Ok(Some(report_of(&result)));
                }
                // The file of the SBOM is gone: it is made once more.
                Some(result) if result.code == EXIT_DOCKER_RUN => {}
                _ => {
                    self.fail("scan");
                    return Ok(None);
                }
            }
        }

        if !self.has_disk_space().await? {
            self.fail("disk");
            return Ok(None);
        }

        if !self.backend.reserve_daily_scan().await? {
            self.fail("daily_limit");
            return Ok(None);
        }

        let create = self.commands.sbom_from_registry(
            &format!("{}@{digest}", image.qualified_name()),
            platform,
            hex,
            self.registry.auth.as_deref(),
        )?;
        let created = self
            .backend
            .run_step(
                &create,
                "sbom",
                scan_image::sbom_step_timeout(&self.commands.host),
            )
            .await;
        let packages = match created {
            Some(created) if created.code == 0 => {
                let list = self.commands.packages(hex)?;
                self.backend
                    .run_step(&list, "packages", TIMEOUT_PACKAGES)
                    .await
            }
            _ => None,
        };
        let Some(packages) = packages.filter(|packages| packages.code == 0) else {
            self.fail("scan");
            return Ok(None);
        };

        let (payload, cut) = scan_image::read_payload(&packages);
        let parsed = grype::parse_syft_packages(&payload);
        let stored = Value::Array(parsed.packages.iter().map(|p| p.to_value()).collect());
        self.backend
            .save_sbom(sbom, digest, platform, &stored, parsed.truncated || cut)
            .await?;

        let result = self
            .backend
            .run_step(&grype_commands, "grype", grype_timeout)
            .await;
        let Some(result) = result.filter(|result| result.code == 0) else {
            self.fail("scan");
            return Ok(None);
        };
        Ok(Some(report_of(&result)))
    }

    /// `hasDiskSpace()`: the disk rule of local scans (image size × 2 + 1 GB), with the size of
    /// the current image: Syft writes the candidate's layers to the scratch directory. A current
    /// image that is no longer on the server counts as 0 bytes.
    async fn has_disk_space(&mut self) -> Result<bool, Stop> {
        let size = match self.current_image_size {
            Some(size) => size,
            None => {
                let mut size = 0;
                let image_id = self
                    .current_image_id
                    .clone()
                    .filter(|image_id| IMAGE_ID.is_match(image_id));
                if let Some(image_id) = image_id {
                    let inspect = self.commands.inspect(&image_id)?;
                    let result = self
                        .backend
                        .run_step(&inspect, "inspect", TIMEOUT_DEFAULT)
                        .await;
                    if let Some(inspected) = result
                        .filter(|result| result.code == 0)
                        .and_then(|result| inspected_size(&result.payload))
                    {
                        size = inspected;
                    }
                }
                self.current_image_size = Some(size);
                size
            }
        };
        Ok(self.backend.has_disk_space(&self.commands, size).await)
    }

    /// `digest($tag)`: the registry digest of a tag of the image.
    async fn digest(&mut self, image: &ImageRef, tag: &str) -> Result<Option<String>, Stop> {
        let payload = self.registry(image, RegistryCall::Digest(tag)).await?;
        let digest = coolify_remote::text::php_trim(payload.as_deref().unwrap_or_default());
        Ok(ImageRef::is_valid_digest(digest).then(|| digest.to_owned()))
    }

    /// The command batch of a registry call, with the login when it was read.
    fn registry_commands(
        &self,
        image: &ImageRef,
        call: RegistryCall<'_>,
    ) -> Result<Vec<String>, Stop> {
        let auth = self.registry.auth.as_deref();
        Ok(match call {
            RegistryCall::Tags => self.commands.tags(&image.qualified_name(), auth)?,
            RegistryCall::Digest(tag) => {
                self.commands
                    .digest(&image.with_tag(tag)?.to_string(), None, auth)?
            }
        })
    }

    /// `registry()`: runs a registry command anonymously first. After a failure it reads the
    /// server's login for the registry of the image once and repeats the command with it.
    /// Without a usable login, a private image needs one. When an anonymous call already worked,
    /// a failure is a registry error, not a missing login.
    async fn registry(
        &mut self,
        image: &ImageRef,
        call: RegistryCall<'_>,
    ) -> Result<Option<String>, Stop> {
        let commands = self.registry_commands(image, call)?;
        let result = self
            .backend
            .run_step(&commands, call.step(), TIMEOUT_REGISTRY)
            .await;
        match self.registry.after_first_call(result.as_ref()) {
            FirstCall::Payload(payload) => return Ok(Some(payload)),
            FirstCall::Failed => {
                self.fail("registry");
                return Ok(None);
            }
            FirstCall::ReadLogin => {}
        }

        let login = self.backend.registry_login(&image.registry).await;
        if !self.registry.after_login(login, image.is_docker_official()) {
            self.fail("registry");
            return Ok(None);
        }

        let commands = self.registry_commands(image, call)?;
        let result = self
            .backend
            .run_step(&commands, call.step(), TIMEOUT_REGISTRY)
            .await;
        let payload = self.registry.after_second_call(result.as_ref());
        if payload.is_none() {
            self.fail("registry");
        }
        Ok(payload)
    }
}

// ---------------------------------------------------------------------------------------------
// On a server
// ---------------------------------------------------------------------------------------------

/// The server and the database of a check.
#[derive(Debug)]
struct ServerBackend<'a> {
    jobs: &'a Jobs,
    server: &'a ScanServer,
    /// The scan lock of the server while this check holds it.
    lock: Option<CacheLock>,
}

impl Backend for ServerBackend<'_> {
    async fn run_step(
        &mut self,
        commands: &[String],
        step: &str,
        timeout_seconds: i64,
    ) -> Option<StepResult> {
        scan_image::run_step(self.server, commands, step, timeout_seconds)
            .await
            .ok()
    }

    async fn registry_login(&mut self, registry: &str) -> RegistryLogin {
        registry_login::registry_auth_config(self.server, registry).await
    }

    async fn lock(&mut self, lock_wait_seconds: u64) -> bool {
        let lock = scan_image::server_lock(self.server);
        if !scan_image::acquire_server_lock(&lock, self.server.id(), lock_wait_seconds).await {
            return false;
        }
        self.lock = Some(lock);
        true
    }

    fn unlock(&mut self) {
        if let Some(lock) = self.lock.take() {
            lock.release();
        }
    }

    async fn find_sbom(&mut self, digest: &str) -> Result<Option<i64>, String> {
        image_scans::find_sbom(&self.jobs.0.db, self.server.id(), digest)
            .await
            .map(|sbom| sbom.map(|sbom| sbom.id))
            .map_err(|e| e.to_string())
    }

    async fn touch_sbom(&mut self, id: i64) -> Result<(), String> {
        image_scans::touch_sbom(&self.jobs.0.db, id, Jobs::now())
            .await
            .map_err(|e| e.to_string())
    }

    async fn save_sbom(
        &mut self,
        id: Option<i64>,
        digest: &str,
        platform: &str,
        packages: &Value,
        packages_truncated: bool,
    ) -> Result<(), String> {
        let db = &self.jobs.0.db;
        let values = SbomValues {
            platform,
            packages,
            packages_truncated,
            last_used_at: Jobs::now(),
        };
        if let Some(id) = id
            && image_scans::update_sbom(db, id, &values)
                .await
                .map_err(|e| e.to_string())?
                .is_some()
        {
            return Ok(());
        }
        image_scans::create_sbom(db, self.server.id(), digest, &values)
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    async fn has_disk_space(&mut self, commands: &ScannerCommands, image_size: i64) -> bool {
        scan_image::check_disk_space(self.server, commands, image_size)
            .await
            .is_ok()
    }

    async fn reserve_daily_scan(&mut self) -> Result<bool, String> {
        let now = Jobs::now();
        budget::reserve_candidate_scan(
            &self.jobs.0.db,
            self.server.id(),
            now.date(),
            DAILY_CANDIDATE_SCANS,
            now,
        )
        .await
        .map_err(|e| e.to_string())
    }

    async fn daily_scans_left(&mut self) -> Result<i64, String> {
        let counters = budget::candidate_scan_counters(&self.jobs.0.db, self.server.id())
            .await
            .map_err(|e| e.to_string())?;
        Ok(daily_scans_left(counters.as_ref(), Jobs::now().date()))
    }
}

/// `ScanNotifications::reevaluate($scan, $checkGaveUp)`: never fails.
pub(super) async fn reevaluate(jobs: &Jobs, scan: &ImageScanRow, check_gave_up: bool) {
    if let Err(e) = notify::reevaluate(jobs, scan, check_gave_up).await {
        tracing::error!(scan_id = scan.id, error = %e, "ScanNotifications::reevaluate failed");
    }
}

/// `RefreshCandidates::run($scan, $lockWaitSeconds, $retryWhenBusy)`.
///
/// `lock_wait_seconds`: a deploy gate waits for the server lock; 0 tries once (background
/// checks). `retry_when_busy`: return [`RefreshError::LockBusy`] instead of storing `lock_busy`,
/// so the caller retries later.
pub async fn run(
    jobs: &Jobs,
    scan: &ImageScanRow,
    lock_wait_seconds: u64,
    retry_when_busy: bool,
) -> Result<(), RefreshError> {
    let db = &jobs.0.db;
    let retry_when_busy = retry_when_busy && lock_wait_seconds == 0;
    let Some(scan) = image_scans::find_scan(db, scan.id).await? else {
        return Ok(());
    };
    let owner = owner_of(jobs, &scan).await?;
    let needs = match &owner {
        Some(owner) => needs(jobs, &scan, owner).await?,
        None => Needs::default(),
    };
    let Some(owner) = owner.filter(|_| needs.any()) else {
        reevaluate(jobs, &scan, false).await;
        return Ok(());
    };

    // `$scan->server`: a server that is gone cannot be checked.
    let server = ScanServer::load(jobs, scan.server_id).await.ok();
    let mut can_run = false;
    if let Some(server) = &server
        && scan_jobs::can_run_on(server)
    {
        can_run = policy::resolve(db, &owner, Some(server.id()))
            .await?
            .scan_enabled;
    }
    let Some(server) = server.filter(|_| can_run) else {
        // No check runs, so an alert that waits for it is sent now.
        reevaluate(jobs, &scan, true).await;
        return Ok(());
    };

    let commands = server
        .commands(&new_run_id())
        .map_err(|e| RefreshError::Other(e.to_string()))?;
    let rules = RelevanceRules::for_resource(db, &owner, Jobs::now()).await?;
    let sbom = match scan.image_sbom_id {
        Some(id) => image_scans::find_sbom_by_id(db, id).await?,
        None => None,
    };
    let subject = Subject {
        scan: &scan,
        rules: &rules,
        platform: sbom.as_ref().map(|sbom| sbom.platform.as_str()),
    };
    let backend = ServerBackend {
        jobs,
        server: &server,
        lock: None,
    };
    let mut check = Check::new(backend, commands.clone(), retry_when_busy);
    let checked = check.both(&subject, needs, lock_wait_seconds).await;
    if check.ran_on_server {
        server.run(&commands.cleanup(), TIMEOUT_DEFAULT).await;
    }
    let (result, base) = checked?;

    store(jobs, &scan, &owner, &result, base.as_ref()).await
}

/// `store()`: writes the result on the current row and recomputes its summary: a better
/// candidate makes findings of images the user does not build fixable now (spec 7.5).
async fn store(
    jobs: &Jobs,
    scan: &ImageScanRow,
    owner: &ScanOwner,
    result: &Outcome,
    base: Option<&Outcome>,
) -> Result<(), RefreshError> {
    let db = &jobs.0.db;
    if image_scans::find_scan(db, scan.id).await?.is_none() {
        return Ok(());
    }
    let candidates = candidates_value(
        scan.base_image_ref.as_deref(),
        result,
        base,
        &iso8601(Utc::now()),
    );
    let Some(fresh) = image_scans::update_scan(
        db,
        scan.id,
        &[("candidates", ScanValue::Json(Some(candidates)))],
        Jobs::now(),
    )
    .await?
    else {
        return Ok(());
    };
    resource_scans::refresh_summaries(jobs, vec![fresh.clone()]).await?;
    // The row with the summary that was just written.
    let fresh = image_scans::find_scan(db, fresh.id).await?.unwrap_or(fresh);

    // The alert for a scan waits for its version check, which can also bring a fix (spec 15.8).
    if let Err(e) = notify::after_scan(jobs, &fresh, None, true, false).await {
        tracing::error!(scan_id = fresh.id, error = %e, "ScanNotifications::afterScan failed");
    }

    forget_queued(&fresh);
    events::dispatch(owner.team_id, &owner.uuid, &fresh.uuid, true);
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use chrono::TimeZone;
    use coolify_db::image_scans::{APPLICATION_MORPH, STATUS_FAILED};

    use super::super::evaluator::tests::{
        GRYPE, GRYPE_15_11, GRYPE_15_ALPINE, finding, fixture, ignores,
    };
    use super::super::registry_login::single_registry_config;
    use super::*;
    use crate::image_scan::commands::ScannerHost;
    use crate::image_scan::resource_scans::tests::{owner, scan as scan_row};

    const SYFT: &str = include_str!("../../../tests/fixtures/image-scan/syft-packages.ndjson");
    const CURRENT_HEX: &str = "a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f90";
    const REPO_DIGEST: &str =
        "sha256:1111111111111111111111111111111111111111111111111111111111111111";
    const NEW_BUILD: &str =
        "sha256:2222222222222222222222222222222222222222222222222222222222222222";
    const LINE: &str = "sha256:3333333333333333333333333333333333333333333333333333333333333333";
    const OUTPUT_LIMIT: usize = crate::image_scan::commands::OUTPUT_LIMIT;

    static DIGEST_TAG: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"'digest' '[^']*:([A-Za-z0-9_.-]+)'").unwrap());
    static SBOM_FILE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"sboms/([a-f0-9]{64})\.json").unwrap());

    fn hex(digest: &str) -> String {
        digest["sha256:".len()..].to_owned()
    }

    fn marker(step: &str, payload: &str, code: i64, size: Option<usize>) -> String {
        let size = size.unwrap_or(payload.len());
        format!("coolify-scan-exit {step} {code} {size}\ncoolify-scan-payload\n{payload}")
    }

    /// `candidatesFake()`: registry calls answer from `tags` and `digests` (by tag), candidate
    /// scans from `grype` (by SBOM name). With `anonymous_fails`, registry calls without
    /// credentials fail.
    #[derive(Debug)]
    struct Fake {
        tags: Vec<String>,
        digests: HashMap<String, String>,
        grype: HashMap<String, &'static str>,
        docker_config: String,
        anonymous_fails: bool,
        /// The free bytes of the Docker root and of the scan directory.
        disk_free: (i64, i64),
        grype_cut: bool,
        /// Replaces text in the Grype output of a candidate.
        grype_edit: Option<(&'static str, &'static str)>,
        /// No batch runs: the SSH connection fails.
        unreachable: bool,
        /// Another scan holds the server lock.
        lock_busy: bool,
        /// A deploy gate starts to wait for the lock when the digest of this tag is asked.
        busy_at_digest_of: Option<&'static str>,
        scans_today: i64,
        /// SBOM rows of the server: image digest to row ID.
        sboms: HashMap<String, i64>,
        // What ran.
        ran: Vec<String>,
        logins_read: usize,
        touched: Vec<i64>,
        saved: Vec<(String, String, usize)>,
        locked: bool,
        registry_refs: Vec<String>,
        configs: Vec<String>,
    }

    impl Fake {
        fn new() -> Self {
            Self {
                tags: [
                    "15.0-alpine",
                    "15.11-alpine",
                    "15.15-alpine",
                    "15-alpine",
                    "16.4-alpine",
                    "15.16-rc1-alpine",
                ]
                .map(str::to_owned)
                .to_vec(),
                digests: HashMap::from([
                    ("15.0-alpine".to_owned(), REPO_DIGEST.to_owned()),
                    ("15.15-alpine".to_owned(), LINE.to_owned()),
                ]),
                grype: HashMap::from([(hex(LINE), GRYPE_15_ALPINE), (hex(NEW_BUILD), GRYPE_15_11)]),
                docker_config: String::new(),
                anonymous_fails: false,
                disk_free: (50_000_000_000, 50_000_000_000),
                grype_cut: false,
                grype_edit: None,
                unreachable: false,
                lock_busy: false,
                busy_at_digest_of: None,
                scans_today: 0,
                sboms: HashMap::new(),
                ran: Vec::new(),
                logins_read: 0,
                touched: Vec::new(),
                saved: Vec::new(),
                locked: false,
                registry_refs: Vec::new(),
                configs: Vec::new(),
            }
        }

        fn tags(mut self, tags: &[&str]) -> Self {
            self.tags = tags.iter().map(|tag| (*tag).to_owned()).collect();
            self
        }

        fn digests(mut self, digests: &[(&str, &str)]) -> Self {
            self.digests = digests
                .iter()
                .map(|(tag, digest)| ((*tag).to_owned(), (*digest).to_owned()))
                .collect();
            self
        }

        fn digest(mut self, tag: &str, digest: &str) -> Self {
            self.digests.insert(tag.to_owned(), digest.to_owned());
            self
        }

        fn grype(mut self, digest: &str, fixture: &'static str) -> Self {
            self.grype.insert(hex(digest), fixture);
            self
        }

        fn sbom(mut self, digest: &str) -> Self {
            let id = self.sboms.len() as i64 + 1;
            self.sboms.insert(digest.to_owned(), id);
            self
        }

        /// How often a step ran.
        fn count(&self, step: &str) -> usize {
            self.ran.iter().filter(|ran| *ran == step).count()
        }
    }

    impl Backend for Fake {
        async fn run_step(
            &mut self,
            commands: &[String],
            step: &str,
            _timeout_seconds: i64,
        ) -> Option<StepResult> {
            // The quotes of `sh -c '…'` scripts undone, so arguments read `'tag' 'ls' 'repo'`.
            let script = commands.join("\n").replace("'\\''", "'");
            self.ran.push(step.to_owned());
            if self.unreachable {
                return None;
            }
            let with_auth = script.contains("DOCKER_CONFIG=/auth");
            if with_auth {
                self.configs.push(script.clone());
            }
            let output = match step {
                "tags" | "digest" if self.anonymous_fails && !with_auth => {
                    marker(step, "", 1, None)
                }
                "tags" => marker("tags", &self.tags.join("\n"), 0, None),
                "digest" => {
                    let tag = DIGEST_TAG
                        .captures(&script)
                        .map(|c| c[1].to_owned())
                        .unwrap_or_default();
                    if self.busy_at_digest_of == Some(tag.as_str()) {
                        self.lock_busy = true;
                    }
                    match self.digests.get(&tag) {
                        Some(digest) => marker("digest", digest, 0, None),
                        None => marker("digest", "", 1, None),
                    }
                }
                "inspect" => format!(
                    "coolify-scan-exit inspect 0\ncoolify-scan-payload\nsha256:{CURRENT_HEX}|postgres@{REPO_DIGEST}|linux/amd64|2000000000"
                ),
                "sbom" => {
                    self.registry_refs.push(script.clone());
                    "coolify-scan-exit sbom 0".to_owned()
                }
                "packages" => marker("packages", SYFT, 0, None),
                "grype" => {
                    let name = SBOM_FILE
                        .captures(&script)
                        .map(|c| c[1].to_owned())
                        .unwrap_or_default();
                    match self.grype.get(&name) {
                        Some(fixture) => {
                            let payload = match self.grype_edit {
                                Some((from, to)) => fixture.replace(from, to),
                                None => (*fixture).to_owned(),
                            };
                            let size = self.grype_cut.then_some(OUTPUT_LIMIT + 1);
                            marker("grype", &payload, 0, size)
                        }
                        None => marker("grype", "", 125, None),
                    }
                }
                _ => String::new(),
            };
            scan_image::step_result(Some(&output), step).ok()
        }

        async fn registry_login(&mut self, registry: &str) -> RegistryLogin {
            self.logins_read += 1;
            single_registry_config(Some(&self.docker_config), registry)
        }

        async fn lock(&mut self, _lock_wait_seconds: u64) -> bool {
            if self.lock_busy {
                return false;
            }
            assert!(!self.locked, "the lock is taken once");
            self.locked = true;
            true
        }

        fn unlock(&mut self) {
            self.locked = false;
        }

        async fn find_sbom(&mut self, digest: &str) -> Result<Option<i64>, String> {
            Ok(self.sboms.get(digest).copied())
        }

        async fn touch_sbom(&mut self, id: i64) -> Result<(), String> {
            self.touched.push(id);
            Ok(())
        }

        async fn save_sbom(
            &mut self,
            id: Option<i64>,
            digest: &str,
            platform: &str,
            packages: &Value,
            _packages_truncated: bool,
        ) -> Result<(), String> {
            let id = id.unwrap_or(self.sboms.len() as i64 + 1);
            self.sboms.insert(digest.to_owned(), id);
            self.saved.push((
                digest.to_owned(),
                platform.to_owned(),
                packages.as_array().map_or(0, Vec::len),
            ));
            Ok(())
        }

        async fn has_disk_space(&mut self, _commands: &ScannerCommands, image_size: i64) -> bool {
            self.ran.push("df".to_owned());
            let needed = image_size * 2 + 1_073_741_824;
            self.disk_free.0 >= needed && self.disk_free.1 >= needed
        }

        async fn reserve_daily_scan(&mut self) -> Result<bool, String> {
            if self.scans_today >= DAILY_CANDIDATE_SCANS {
                return Ok(false);
            }
            self.scans_today += 1;
            Ok(true)
        }

        async fn daily_scans_left(&mut self) -> Result<i64, String> {
            Ok((DAILY_CANDIDATE_SCANS - self.scans_today).max(0))
        }
    }

    fn findings_value(findings: &[Finding]) -> Value {
        Value::Array(findings.iter().map(Finding::to_value).collect())
    }

    /// `candidatesScan()`: a completed scan of `postgres:15.0-alpine` (the grype fixture, on an
    /// end-of-life Alpine) of an image the team does not build.
    fn candidates_scan(image_ref: &str, repo_digest: Option<&str>) -> ImageScanRow {
        let mut row = scan_row(image_ref, STATUS_COMPLETED, None);
        row.image_id = format!("sha256:{CURRENT_HEX}");
        row.repo_digest = repo_digest.map(str::to_owned);
        row.findings = Some(findings_value(&fixture(GRYPE)));
        row.summary = Some(
            json!({"eol_distro": {"name": "alpine", "version": "3.16.2"}, "truncated": false}),
        );
        row.builds_image = false;
        row
    }

    fn postgres_scan() -> ImageScanRow {
        candidates_scan("postgres:15.0-alpine", Some(REPO_DIGEST))
    }

    /// `candidatesDockerfileScan()`: a Dockerfile build from `postgres:15.0-alpine`: the app has
    /// the findings of the base image (grype fixture) and one more of a package that the
    /// Dockerfile added (CVE-2099-0001).
    fn dockerfile_scan(base_image_ref: &str) -> ImageScanRow {
        let mut findings = fixture(GRYPE);
        findings.push(Finding::from_value(&json!({
            "id": "CVE-2099-0001", "aliases": [], "severity": "High", "package": "curl",
            "version": "8.0.0-r0", "type": "apk", "artifactId": "added-curl", "fixedIn": "8.0.1-r0",
            "fixState": "fixed", "kev": false, "epss": null, "risk": 9.0, "summary": "", "layer": "app",
        })));
        let mut row = postgres_scan();
        row.image_ref = "appuuid:abc123".to_owned();
        row.repo_digest = None;
        row.builds_image = true;
        row.base_image_ref = Some(base_image_ref.to_owned());
        row.findings = Some(findings_value(&findings));
        row
    }

    /// The fake of a Dockerfile scan: the registry scan of the current base image answers with
    /// the grype fixture, the line candidate 15.15-alpine with the 15-alpine fixture.
    fn base_fake() -> Fake {
        Fake::new().grype(REPO_DIGEST, GRYPE)
    }

    /// `candidatesRelevantFindings()`: the findings that base attribution stores: every medium
    /// counts.
    fn attributable_findings() -> Vec<Finding> {
        evaluator::relevant(&fixture(GRYPE), &ignores(&[]).with_all_mediums())
    }

    fn check_with(fake: Fake, retry_when_busy: bool) -> Check<Fake> {
        let commands = ScannerCommands::new(ScannerHost::default(), "abcdefgh12345678").unwrap();
        Check::new(fake, commands, retry_when_busy)
    }

    /// Runs the version check of a scan as `handle()` does.
    async fn image_check(fake: Fake, scan: &ImageScanRow) -> (Outcome, Check<Fake>) {
        image_check_waiting(fake, scan, 0, false).await.unwrap()
    }

    async fn image_check_waiting(
        fake: Fake,
        scan: &ImageScanRow,
        lock_wait_seconds: u64,
        retry_when_busy: bool,
    ) -> Result<(Outcome, Check<Fake>), RefreshError> {
        let rules = ignores(&[]);
        let subject = Subject {
            scan,
            rules: &rules,
            platform: Some("linux/amd64"),
        };
        let mut check = check_with(fake, retry_when_busy && lock_wait_seconds == 0);
        let outcome = check.guarded_image(&subject, lock_wait_seconds).await?;
        assert!(!check.backend.locked, "the lock is released");
        Ok((outcome, check))
    }

    /// Runs the base check of a scan as `handle()` does.
    async fn base_check(fake: Fake, scan: &ImageScanRow) -> (Outcome, Check<Fake>) {
        let rules = ignores(&[]);
        let subject = Subject {
            scan,
            rules: &rules,
            platform: Some("linux/amd64"),
        };
        let mut check = check_with(fake, false);
        let outcome = check.guarded_base(&subject, 0).await.unwrap();
        assert!(!check.backend.locked, "the lock is released");
        (outcome, check)
    }

    fn entry_of<'a>(outcome: &'a Outcome, kind: &str) -> Option<&'a Value> {
        outcome
            .entries
            .iter()
            .find(|entry| entry["kind"] == json!(kind))
    }

    fn fixed_ids_of(entry: &Value) -> Vec<&str> {
        entry["fixed_ids"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect()
    }

    fn sum(counts: &Value) -> u64 {
        ["critical", "high", "medium"]
            .iter()
            .map(|severity| counts[*severity].as_u64().unwrap())
            .sum()
    }

    /// The scan with the stored result of a check.
    fn stored(scan: &ImageScanRow, result: &Outcome, base: Option<&Outcome>) -> ImageScanRow {
        let mut row = scan.clone();
        row.candidates = Some(candidates_value(
            scan.base_image_ref.as_deref(),
            result,
            base,
            "2026-11-10T12:00:00+00:00",
        ));
        row
    }

    #[tokio::test]
    async fn recommends_the_newest_tag_of_the_line_and_lists_what_it_fixes() {
        let scan = postgres_scan();
        let (outcome, check) = image_check(Fake::new(), &scan).await;
        let line = entry_of(&outcome, "line").unwrap();
        let major = entry_of(&outcome, "major").unwrap();

        assert_eq!(outcome.status, "ok");
        assert_eq!(outcome.error, None);
        assert!(!outcome.tags_truncated);
        assert_eq!(line["ref"], json!("postgres:15.15-alpine"));
        assert_eq!(line["digest"], json!(LINE));
        assert_eq!(line["better"], json!(true));
        assert_eq!(line["eol"], Value::Null);
        assert!(fixed_ids_of(line).contains(&"CVE-2025-1094"));
        assert!(fixed_ids_of(line).contains(&"CVE-2023-2650"));
        assert_eq!(
            line["left"],
            json!({"critical": 3, "high": 32, "medium": 0})
        );
        assert!(
            line["fixes"]["critical"].as_u64().unwrap() + line["fixes"]["high"].as_u64().unwrap()
                > 0
        );
        assert!(line["scanned_at"].is_string());
        assert_eq!(
            major,
            &json!({"ref": "postgres:16.4-alpine", "digest": null, "kind": "major", "better": false,
                "fixed_ids": [], "fixes": null, "added": null, "left": null, "eol": null, "scanned_at": null})
        );
        assert_eq!(entry_of(&outcome, "same_tag"), None);

        let fake = &check.backend;
        assert_eq!(fake.scans_today, 1);
        assert_eq!(fake.saved.len(), 1);
        assert_eq!(fake.saved[0].0, LINE);
        assert_eq!(fake.saved[0].1, "linux/amd64");
        assert!(fake.saved[0].2 > 0);
        assert!(check.ran_on_server);
        // The registry is read without credentials.
        assert_eq!(fake.logins_read, 0);
        assert!(fake.configs.is_empty());
        assert_eq!(fake.registry_refs.len(), 1);
        assert!(fake.registry_refs[0].contains(&format!("'registry:postgres@{LINE}'")));
        assert!(fake.registry_refs[0].contains("'--platform' 'linux/amd64'"));
        assert_eq!(fake.count("tags"), 1);
        assert_eq!(fake.count("inspect"), 1);

        // What is stored.
        let row = stored(&scan, &outcome, None);
        assert_eq!(row.candidate_status(), Some("ok"));
        assert_eq!(row.candidate_entries().len(), 2);
        let value = row.candidates.as_ref().unwrap();
        assert_eq!(
            value
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            vec!["status", "checked_at", "error", "tags_truncated", "entries"]
        );
        assert_eq!(
            line.as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            vec![
                "ref",
                "digest",
                "kind",
                "better",
                "fixed_ids",
                "fixes",
                "added",
                "left",
                "eol",
                "scanned_at"
            ]
        );
        assert_eq!(message_on(&row, false, Some("server-1")), None);
    }

    #[tokio::test]
    async fn says_that_no_newer_version_is_better_when_the_line_candidate_adds_exploit_signals() {
        let scan = postgres_scan();
        let (outcome, _) = image_check(Fake::new().grype(LINE, GRYPE_15_11), &scan).await;
        assert_eq!(outcome.status, "no_better");
        assert_eq!(entry_of(&outcome, "line").unwrap()["better"], json!(false));
        assert_eq!(
            message(&stored(&scan, &outcome, None), false).as_deref(),
            Some("No newer version of postgres is better yet. Coolify checks again every day.")
        );
    }

    #[tokio::test]
    async fn offers_the_newest_patch_and_does_not_scan_the_line_when_the_patch_is_safer() {
        let fake = Fake::new()
            .tags(&["15.0.1-alpine", "15.0.4-alpine", "15.2.0-alpine"])
            .digests(&[
                ("15.0.1-alpine", REPO_DIGEST),
                ("15.0.4-alpine", LINE),
                ("15.2.0-alpine", NEW_BUILD),
            ]);
        let scan = candidates_scan("postgres:15.0.1-alpine", Some(REPO_DIGEST));
        let (outcome, check) = image_check(fake, &scan).await;
        assert_eq!(outcome.status, "ok");
        assert_eq!(outcome.entries.len(), 1);
        let line = entry_of(&outcome, "line").unwrap();
        assert_eq!(line["ref"], json!("postgres:15.0.4-alpine"));
        assert_eq!(line["better"], json!(true));
        assert_eq!(check.backend.count("sbom"), 1);
    }

    #[tokio::test]
    async fn scans_the_newest_tag_of_the_line_when_the_newest_patch_is_not_safer() {
        let fake = Fake::new()
            .tags(&["15.0.1-alpine", "15.0.4-alpine", "15.2.0-alpine"])
            .digests(&[
                ("15.0.1-alpine", REPO_DIGEST),
                ("15.0.4-alpine", NEW_BUILD),
                ("15.2.0-alpine", LINE),
            ]);
        let scan = candidates_scan("postgres:15.0.1-alpine", Some(REPO_DIGEST));
        let (outcome, _) = image_check(fake, &scan).await;
        assert_eq!(outcome.status, "ok");
        let lines: Vec<(&Value, &Value)> = outcome
            .entries
            .iter()
            .filter(|entry| entry["kind"] == json!("line"))
            .map(|entry| (&entry["ref"], &entry["better"]))
            .collect();
        assert_eq!(
            lines,
            vec![
                (&json!("postgres:15.0.4-alpine"), &json!(false)),
                (&json!("postgres:15.2.0-alpine"), &json!(true)),
            ]
        );
    }

    #[tokio::test]
    async fn scans_the_newest_major_for_information_when_nothing_of_the_same_major_is_safer() {
        let fake = Fake::new()
            .grype(LINE, GRYPE_15_11)
            .digest("16.4-alpine", NEW_BUILD)
            .grype(NEW_BUILD, GRYPE_15_ALPINE);
        let scan = postgres_scan();
        let (outcome, check) = image_check(fake, &scan).await;
        let major = entry_of(&outcome, "major").unwrap();
        assert_eq!(outcome.status, "no_better");
        assert_eq!(major["ref"], json!("postgres:16.4-alpine"));
        assert_eq!(major["better"], json!(false));
        assert_eq!(major["fixes_problems"], json!(true));
        assert!(fixed_ids_of(major).contains(&"CVE-2025-1094"));
        assert_eq!(
            major
                .as_object()
                .unwrap()
                .keys()
                .next_back()
                .map(String::as_str),
            Some("fixes_problems")
        );
        assert_eq!(check.backend.count("sbom"), 2);
    }

    #[tokio::test]
    async fn keeps_the_status_of_the_check_when_the_scan_of_the_newest_major_fails() {
        let unknown = format!("sha256:{}", "4".repeat(64));
        let fake = Fake::new()
            .grype(LINE, GRYPE_15_11)
            .digest("16.4-alpine", &unknown);
        let (outcome, _) = image_check(fake, &postgres_scan()).await;
        let major = entry_of(&outcome, "major").unwrap();
        assert_eq!(outcome.status, "no_better");
        assert_eq!(outcome.error, None);
        assert_eq!(major["ref"], json!("postgres:16.4-alpine"));
        assert_eq!(major["scanned_at"], Value::Null);
        assert_eq!(major.get("fixes_problems"), None);
    }

    #[tokio::test]
    async fn checks_the_newest_tag_of_the_line_also_after_a_newer_build_and_a_newer_patch() {
        let patch = format!("sha256:{}", "5".repeat(64));
        let mut fake = Fake::new()
            .tags(&["15.0.1-alpine", "15.0.4-alpine", "15.2.0-alpine"])
            .digests(&[
                ("15.0.1-alpine", NEW_BUILD),
                ("15.0.4-alpine", &patch),
                ("15.2.0-alpine", LINE),
            ]);
        fake.grype = HashMap::from([
            (hex(NEW_BUILD), GRYPE_15_11),
            (hex(&patch), GRYPE_15_11),
            (hex(LINE), GRYPE_15_ALPINE),
        ]);
        let scan = candidates_scan("postgres:15.0.1-alpine", Some(REPO_DIGEST));
        let (outcome, check) = image_check(fake, &scan).await;
        let entries: Vec<(&Value, &Value, &Value)> = outcome
            .entries
            .iter()
            .map(|entry| (&entry["kind"], &entry["ref"], &entry["better"]))
            .collect();
        assert_eq!(
            entries,
            vec![
                (
                    &json!("same_tag"),
                    &json!("postgres:15.0.1-alpine"),
                    &json!(false)
                ),
                (
                    &json!("line"),
                    &json!("postgres:15.0.4-alpine"),
                    &json!(false)
                ),
                (
                    &json!("line"),
                    &json!("postgres:15.2.0-alpine"),
                    &json!(true)
                ),
            ]
        );
        assert_eq!(outcome.status, "ok");
        assert_eq!(check.backend.count("sbom"), 3);
        assert_eq!(check.backend.scans_today, 3);
    }

    #[tokio::test]
    async fn does_not_scan_the_newest_major_when_fewer_than_5_scans_are_left_today() {
        let mut fake = Fake::new()
            .grype(LINE, GRYPE_15_11)
            .digest("16.4-alpine", NEW_BUILD);
        fake.scans_today = 15;
        let (outcome, check) = image_check(fake, &postgres_scan()).await;
        let major = entry_of(&outcome, "major").unwrap();
        assert_eq!(outcome.status, "no_better");
        assert_eq!(major["ref"], json!("postgres:16.4-alpine"));
        assert_eq!(major["scanned_at"], Value::Null);
        assert_eq!(major.get("fixes_problems"), None);
        assert_eq!(check.backend.count("sbom"), 1);
        assert_eq!(check.backend.scans_today, 16);
    }

    #[tokio::test]
    async fn keeps_the_result_when_the_server_lock_is_busy_for_the_scan_of_the_newest_major() {
        let mut fake = Fake::new()
            .grype(LINE, GRYPE_15_11)
            .digest("16.4-alpine", NEW_BUILD)
            .grype(NEW_BUILD, GRYPE_15_ALPINE);
        // A deploy gate starts to wait for the lock while Coolify asks for the digest of the major.
        fake.busy_at_digest_of = Some("16.4-alpine");
        let (outcome, check) = image_check_waiting(fake, &postgres_scan(), 0, true)
            .await
            .unwrap();
        assert_eq!(outcome.status, "no_better");
        assert_eq!(outcome.error, None);
        assert_eq!(
            entry_of(&outcome, "major").unwrap().get("fixes_problems"),
            None
        );
        assert_eq!(check.backend.count("sbom"), 1);
    }

    #[tokio::test]
    async fn checks_the_same_tag_for_a_newer_build_and_scans_at_most_2_candidates() {
        let fake = Fake::new().digest("15.0-alpine", NEW_BUILD);
        let (outcome, check) = image_check(fake, &postgres_scan()).await;
        let same_tag = entry_of(&outcome, "same_tag").unwrap();
        assert_eq!(same_tag["ref"], json!("postgres:15.0-alpine"));
        assert_eq!(same_tag["digest"], json!(NEW_BUILD));
        assert_eq!(same_tag["better"], json!(false));
        assert_eq!(outcome.status, "ok");
        assert_eq!(check.backend.count("sbom"), 2);
        assert_eq!(check.backend.scans_today, 2);
    }

    #[tokio::test]
    async fn reuses_the_sbom_of_a_candidate_digest_on_the_server_and_does_not_count_it() {
        let (outcome, check) = image_check(Fake::new().sbom(LINE), &postgres_scan()).await;
        assert_eq!(outcome.status, "ok");
        assert_eq!(check.backend.count("sbom"), 0);
        assert_eq!(check.backend.count("grype"), 1);
        assert_eq!(check.backend.count("df"), 0);
        assert_eq!(check.backend.scans_today, 0);
        assert_eq!(check.backend.touched, vec![1]);
    }

    #[tokio::test]
    async fn makes_the_sbom_once_more_when_its_file_is_gone() {
        // Grype cannot mount the SBOM of the row (exit code 125 of `docker run`).
        let mut fake = Fake::new().sbom(LINE);
        let report = fake.grype.remove(&hex(LINE));
        assert!(report.is_some());
        let (outcome, check) = image_check(fake, &postgres_scan()).await;
        // The second Grype run fails too in this fake: the scan is a failure, not a crash.
        assert_eq!(outcome.status, "failed");
        assert_eq!(outcome.error, Some("scan"));
        assert_eq!(check.backend.count("sbom"), 1);
        assert_eq!(check.backend.count("grype"), 2);
        assert_eq!(check.backend.saved.len(), 1);
        assert!(check.backend.touched.is_empty());
    }

    #[tokio::test]
    async fn scans_no_new_candidate_after_20_scans_on_the_server_today() {
        let mut fake = Fake::new();
        fake.scans_today = 20;
        let scan = postgres_scan();
        let (outcome, check) = image_check(fake, &scan).await;
        assert_eq!(check.backend.count("sbom"), 0);
        assert_eq!(check.backend.scans_today, 20);
        assert_eq!(outcome.status, "failed");
        assert_eq!(outcome.error, Some("daily_limit"));
        let row = stored(&scan, &outcome, None);
        assert_eq!(
            message_on(&row, false, Some("server-1")).as_deref(),
            Some(
                "Coolify reached the daily limit of version checks on server-1. It checks again tomorrow."
            )
        );
        assert_eq!(
            message(&row, false).as_deref(),
            Some(
                "Coolify reached the daily limit of version checks on the server. It checks again tomorrow."
            )
        );
    }

    #[tokio::test]
    async fn uses_the_login_of_the_image_registry_only_after_an_anonymous_request_fails() {
        use base64::Engine as _;
        let b64 = |text: &str| base64::engine::general_purpose::STANDARD.encode(text);
        let mut fake = Fake::new()
            .tags(&["1.0", "1.1"])
            .digests(&[("1.0", REPO_DIGEST), ("1.1", LINE)]);
        fake.anonymous_fails = true;
        fake.docker_config = json!({"auths": {
            "https://index.docker.io/v1/": {"auth": b64("hub:S3cr3t-hub")},
            "ghcr.io": {"auth": b64("acme:S3cr3t")},
            "registry.example.com": {"auth": b64("other:S3cr3t-other")},
        }})
        .to_string();
        let scan = candidates_scan("ghcr.io/acme/api:1.0", Some(REPO_DIGEST));
        let (outcome, check) = image_check(fake, &scan).await;

        assert_eq!(outcome.status, "ok");
        let fake = &check.backend;
        assert_eq!(fake.logins_read, 1);
        // The first call ran without credentials, every call after the login with them.
        assert_eq!(fake.count("tags"), 2);
        assert!(!fake.configs.is_empty());
        let config = b64(&format!(
            "{{\"auths\":{{\"ghcr.io\":{{\"auth\":\"{}\"}}}}}}",
            b64("acme:S3cr3t")
        ));
        for script in &fake.configs {
            assert!(script.contains(&config));
            assert!(!script.contains(&b64("hub:S3cr3t-hub")));
            assert!(!script.contains(&b64("other:S3cr3t-other")));
        }
        // The registry scan of the candidate uses the login too.
        assert_eq!(fake.registry_refs.len(), 1);
        assert!(fake.registry_refs[0].contains("DOCKER_CONFIG=/auth"));
        // Nothing of the login is stored.
        let text = stored(&scan, &outcome, None)
            .candidates
            .unwrap()
            .to_string();
        assert!(!text.contains("S3cr3t"));
        assert!(!text.contains(&b64("acme:S3cr3t")));
    }

    #[tokio::test]
    async fn asks_for_a_login_when_the_registry_login_uses_a_credential_helper() {
        let mut fake = Fake::new();
        fake.anonymous_fails = true;
        fake.docker_config = json!({"credHelpers": {"ghcr.io": "gh"}}).to_string();
        let scan = candidates_scan("ghcr.io/acme/api:1.0", Some(REPO_DIGEST));
        let (outcome, check) = image_check(fake, &scan).await;
        assert_eq!(outcome.status, "auth_required");
        assert_eq!(outcome.error, Some("registry_auth"));
        assert!(outcome.entries.is_empty());
        assert!(check.backend.configs.is_empty());
        assert_eq!(
            message_on(&stored(&scan, &outcome, None), false, Some("server-1")).as_deref(),
            Some(
                "Coolify cannot read the tags of ghcr.io/acme/api. Log in to ghcr.io on server-1."
            )
        );
    }

    #[tokio::test]
    async fn reads_the_tags_of_an_image_from_a_registry_with_a_port() {
        let fake = Fake::new()
            .tags(&["1.2", "1.3", "2.0"])
            .digests(&[("1.2", REPO_DIGEST), ("1.3", LINE)]);
        let scan = candidates_scan("localhost:5000/team/app:1.2", Some(REPO_DIGEST));
        let (outcome, check) = image_check(fake, &scan).await;
        let refs: Vec<&Value> = outcome.entries.iter().map(|entry| &entry["ref"]).collect();
        assert_eq!(
            refs,
            vec![
                &json!("localhost:5000/team/app:1.3"),
                &json!("localhost:5000/team/app:2.0")
            ]
        );
        assert!(
            check.backend.registry_refs[0]
                .contains(&format!("'registry:localhost:5000/team/app@{LINE}'"))
        );
    }

    #[tokio::test]
    async fn cannot_compare_versions_of_a_tag_that_is_not_a_version() {
        let fake = Fake::new().digests(&[("main", REPO_DIGEST)]);
        let scan = candidates_scan("ghcr.io/acme/api:main", Some(REPO_DIGEST));
        let (outcome, check) = image_check(fake, &scan).await;
        assert_eq!(outcome.status, "cannot_compare");
        assert_eq!(check.backend.count("tags"), 0);
        assert_eq!(
            message(&stored(&scan, &outcome, None), false).as_deref(),
            Some(
                "Coolify cannot compare versions of this image. Check the image page for a newer version."
            )
        );
    }

    #[tokio::test]
    async fn a_newer_build_of_a_tag_that_is_not_a_version_is_compared() {
        let fake = Fake::new().digests(&[("main", LINE)]);
        let scan = candidates_scan("ghcr.io/acme/api:main", Some(REPO_DIGEST));
        let (outcome, _) = image_check(fake, &scan).await;
        assert_eq!(outcome.status, "ok");
        assert_eq!(
            entry_of(&outcome, "same_tag").unwrap()["ref"],
            json!("ghcr.io/acme/api:main")
        );
        // Its digest cannot be read: not "cannot compare" but `failure()`. Without a usable login
        // on the server an image that is not a Docker official image needs one (`registry()`).
        let scan = candidates_scan("ghcr.io/acme/api:main", Some(REPO_DIGEST));
        let (outcome, _) = image_check(Fake::new().digests(&[]), &scan).await;
        assert_eq!(outcome.status, "auth_required");
        assert_eq!(outcome.error, Some("registry_auth"));
    }

    #[tokio::test]
    async fn cannot_compare_an_image_pinned_by_digest_and_runs_nothing_on_the_server() {
        let scan = candidates_scan(
            &format!("ghcr.io/acme/api@{REPO_DIGEST}"),
            Some(REPO_DIGEST),
        );
        let (outcome, check) = image_check(Fake::new(), &scan).await;
        assert_eq!(outcome.status, "cannot_compare");
        assert!(check.backend.ran.is_empty());
        assert!(!check.ran_on_server);

        // A reference that cannot be read.
        let scan = candidates_scan("Not An Image", Some(REPO_DIGEST));
        let (outcome, check) = image_check(Fake::new(), &scan).await;
        assert_eq!(outcome.status, "cannot_compare");
        assert!(check.backend.ran.is_empty());
    }

    #[tokio::test]
    async fn fails_without_the_platform_of_the_scanned_image() {
        let scan = postgres_scan();
        let rules = ignores(&[]);
        let subject = Subject {
            scan: &scan,
            rules: &rules,
            platform: None,
        };
        let mut check = check_with(Fake::new(), false);
        let outcome = check.guarded_image(&subject, 0).await.unwrap();
        assert_eq!(outcome.status, "failed");
        assert_eq!(outcome.error, Some("unknown"));
        assert!(check.backend.ran.is_empty());
        // `checkBase()` reads the base image reference before the platform: a scan without one
        // cannot be compared.
        let base = check.guarded_base(&subject, 0).await.unwrap();
        assert_eq!(base.status, "cannot_compare");
        let scan = dockerfile_scan("postgres:15.0-alpine");
        let subject = Subject {
            scan: &scan,
            rules: &rules,
            platform: None,
        };
        let mut check = check_with(Fake::new(), false);
        let base = check.guarded_base(&subject, 0).await.unwrap();
        assert_eq!(base.status, "failed");
        assert_eq!(base.error, Some("unknown"));
        assert_eq!(base.attributable, None);
        assert!(check.backend.ran.is_empty());
    }

    #[tokio::test]
    async fn records_a_failure_when_the_registry_cannot_be_read() {
        let mut fake = Fake::new();
        fake.unreachable = true;
        let scan = postgres_scan();
        let (outcome, check) = image_check(fake, &scan).await;
        assert_eq!(outcome.status, "failed");
        assert_eq!(outcome.error, Some("registry"));
        // A batch that did not run is not a missing login.
        assert_eq!(check.backend.logins_read, 0);
        assert_eq!(
            message(&stored(&scan, &outcome, None), false).as_deref(),
            Some(
                "Coolify could not check for newer versions of postgres. It tries again tomorrow."
            )
        );
    }

    #[tokio::test]
    async fn does_not_recommend_a_line_candidate_that_adds_a_finding_with_an_exploit_signal() {
        // GO-2026-4337 (CVSS 10) of gosu, which 15.0-alpine does not have, gets an EPSS of 0.5:
        // a likely exploit.
        let mut fake = Fake::new();
        fake.grype_edit = Some(("\"epss\":0.00915", "\"epss\":0.5"));
        let (outcome, _) = image_check(fake, &postgres_scan()).await;
        let line = entry_of(&outcome, "line").unwrap();
        assert_eq!(outcome.status, "no_better");
        assert_eq!(line["better"], json!(false));
        assert!(fixed_ids_of(line).contains(&"CVE-2025-1094"));
    }

    #[tokio::test]
    async fn looks_for_candidates_for_a_fixable_high_known_exploited_finding() {
        let mut scan = postgres_scan();
        scan.findings = Some(json!([{
            "id": "CVE-2025-1094", "aliases": [], "severity": "high", "package": "postgresql",
            "version": "15.0", "type": "binary", "artifactId": "pg", "fixedIn": "15.11",
            "fixState": "fixed", "kev": true,
        }]));
        scan.summary = Some(json!({"eol_distro": null, "truncated": false}));
        let (outcome, _) = image_check(Fake::new(), &scan).await;
        let line = entry_of(&outcome, "line").unwrap();
        assert_eq!(outcome.status, "ok");
        assert_eq!(line["better"], json!(true));
        assert_eq!(line["fixed_ids"], json!(["CVE-2025-1094"]));
        assert_eq!(
            line["fixes"],
            json!({"critical": 0, "high": 1, "medium": 0})
        );
        // Every relevant critical, high, or medium finding of 15.15 is new: the current image had
        // only the exploited one.
        assert_eq!(line["added"].as_u64().unwrap(), sum(&line["left"]));
    }

    #[tokio::test]
    async fn checks_the_disk_space_for_the_current_image_size_before_a_registry_scan() {
        let mut fake = Fake::new();
        fake.disk_free = (4_000_000_000, 50_000_000_000);
        let (outcome, check) = image_check(fake, &postgres_scan()).await;
        assert_eq!(check.backend.count("sbom"), 0);
        assert_eq!(check.backend.count("df"), 1);
        assert_eq!(check.backend.count("inspect"), 1);
        assert_eq!(outcome.status, "failed");
        assert_eq!(outcome.error, Some("disk"));
        assert_eq!(check.backend.scans_today, 0);

        // Image size × 2 + 1 GB is enough.
        let mut fake = Fake::new();
        fake.disk_free = (5_100_000_000, 5_100_000_000);
        let (_, check) = image_check(fake, &postgres_scan()).await;
        assert_eq!(check.backend.count("sbom"), 1);
    }

    #[tokio::test]
    async fn does_not_ask_for_a_login_when_anonymous_registry_access_worked() {
        let fake = Fake::new()
            .tags(&["1.0", "1.1"])
            .digests(&[("1.0", REPO_DIGEST)]);
        let scan = candidates_scan("ghcr.io/acme/api:1.0", Some(REPO_DIGEST));
        let (outcome, check) = image_check(fake, &scan).await;
        assert_eq!(outcome.status, "failed");
        assert_eq!(outcome.error, Some("registry"));
        assert_eq!(check.backend.logins_read, 0);
    }

    #[tokio::test]
    async fn does_not_ask_for_a_login_for_a_docker_hub_official_image() {
        let mut fake = Fake::new();
        fake.anonymous_fails = true;
        let (outcome, check) = image_check(fake, &postgres_scan()).await;
        assert_eq!(outcome.status, "failed");
        assert_eq!(outcome.error, Some("registry"));
        assert_eq!(check.backend.logins_read, 1);
    }

    #[tokio::test]
    async fn never_recommends_a_candidate_whose_scan_output_was_cut() {
        let mut fake = Fake::new();
        fake.grype_cut = true;
        let (outcome, _) = image_check(fake, &postgres_scan()).await;
        assert_eq!(entry_of(&outcome, "line").unwrap()["better"], json!(false));
        assert_eq!(outcome.status, "no_better");
    }

    #[tokio::test]
    async fn records_a_busy_server_lock_and_scans_nothing() {
        let mut fake = Fake::new();
        fake.lock_busy = true;
        let (outcome, check) = image_check_waiting(fake, &postgres_scan(), 1, false)
            .await
            .unwrap();
        assert_eq!(check.backend.count("sbom"), 0);
        assert_eq!(outcome.status, "failed");
        assert_eq!(outcome.error, Some("lock_busy"));

        // A deploy gate that waited does not retry later, also when the caller asked for it.
        let mut fake = Fake::new();
        fake.lock_busy = true;
        let (outcome, _) = image_check_waiting(fake, &postgres_scan(), 1, true)
            .await
            .unwrap();
        assert_eq!(outcome.error, Some("lock_busy"));
    }

    /// `it('does not wait for a busy server lock in the queued job and runs again later')`.
    #[tokio::test]
    async fn a_background_check_that_retries_stores_nothing_for_a_busy_lock() {
        let mut fake = Fake::new();
        fake.lock_busy = true;
        let result = image_check_waiting(fake, &postgres_scan(), 0, true).await;
        assert_eq!(result.err(), Some(RefreshError::LockBusy));
    }

    #[tokio::test]
    async fn records_that_the_tag_list_was_cut_at_10_000_tags() {
        let (outcome, _) = image_check(Fake::new(), &postgres_scan()).await;
        assert!(!outcome.tags_truncated);

        let mut fake = Fake::new();
        fake.tags = (100..=10_100)
            .map(|patch| format!("15.{patch}-alpine"))
            .collect();
        fake.tags.push("15.0-alpine".to_owned());
        let scan = postgres_scan();
        let (outcome, _) = image_check(fake, &scan).await;
        assert!(outcome.tags_truncated);
        assert_eq!(
            stored(&scan, &outcome, None).candidates.unwrap()["tags_truncated"],
            json!(true)
        );
    }

    #[tokio::test]
    async fn attributes_findings_to_the_base_image_and_recommends_the_newer_tag() {
        let scan = dockerfile_scan("postgres:15.0-alpine");
        let (base, check) = base_check(base_fake(), &scan).await;
        let attributable = attributable_findings();
        let mut ids: Vec<&str> = attributable.iter().map(|f| f.id.as_str()).collect();
        ids.dedup();
        let unique: std::collections::HashSet<&str> = ids.iter().copied().collect();

        assert_eq!(base.status, "ok");
        assert!(!base.tags_truncated);
        assert_eq!(base.current_digest.as_deref(), Some(REPO_DIGEST));
        assert_eq!(base.attributable, Some(attributable.len()));
        assert!(!base.attributable_ids.iter().any(|id| id == "CVE-2099-0001"));
        assert_eq!(base.attributable_ids.len(), unique.len());
        assert!(base.attributable_ids.iter().any(|id| id == "CVE-2023-2650"));
        assert_eq!(base.entries.len(), 1);
        let line = &base.entries[0];
        assert_eq!(line["ref"], json!("postgres:15.15-alpine"));
        assert_eq!(line["digest"], json!(LINE));
        assert_eq!(line["kind"], json!("line"));
        assert_eq!(line["better"], json!(true));
        assert_eq!(
            line["left"],
            json!({"critical": 3, "high": 32, "medium": 0})
        );
        assert!(fixed_ids_of(line).contains(&"CVE-2025-1094"));
        assert!(!fixed_ids_of(line).contains(&"CVE-2099-0001"));
        assert!(sum(&line["fixes"]) > 0);
        assert!(sum(&line["fixes"]) <= attributable.len() as u64);
        assert_eq!(check.backend.scans_today, 2);
        assert_eq!(check.backend.count("tags"), 1);
        assert_eq!(check.backend.registry_refs.len(), 2);
        assert!(
            check.backend.registry_refs[0].contains(&format!("'registry:postgres@{REPO_DIGEST}'"))
        );
        assert!(check.backend.registry_refs[0].contains("'--platform' 'linux/amd64'"));
        assert!(check.backend.registry_refs[1].contains(&format!("'registry:postgres@{LINE}'")));

        // What is stored: the version check of the built image is skipped.
        let skipped = Outcome::new("skipped", None, Vec::new());
        let row = stored(&scan, &skipped, Some(&base));
        assert_eq!(row.candidate_status(), Some("skipped"));
        assert!(row.candidate_entries().is_empty());
        let value = row.candidates.as_ref().unwrap();
        assert_eq!(
            value["base"]
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            vec![
                "image",
                "status",
                "checked_at",
                "error",
                "tags_truncated",
                "attributable",
                "attributable_ids",
                "current_digest",
                "entries"
            ]
        );
        assert_eq!(value["base"]["image"], json!("postgres:15.0-alpine"));
        assert_eq!(value["base"]["status"], json!("ok"));
        assert_eq!(value["base"]["current_digest"], json!(REPO_DIGEST));
        assert_eq!(value["base"]["attributable"], json!(attributable.len()));
        assert!(fresh_checked_at(&row).is_some());
        assert!(base_result_is_fresh(&row));
        assert_eq!(message(&row, true), None);

        // The stored check gives the better base image of the Dockerfile.
        let better = super::super::base_image_check::better_base(&row).unwrap();
        assert_eq!(better.from, "postgres:15.0-alpine");
        assert_eq!(better.to, "postgres:15.15-alpine");
    }

    #[tokio::test]
    async fn counts_only_findings_that_the_current_base_image_has_as_attributable_and_fixed() {
        // The current base image has 37 of the findings the base check stores; the line candidate
        // has none of the app's.
        let scan = dockerfile_scan("postgres:15.0-alpine");
        let fake = base_fake().grype(REPO_DIGEST, GRYPE_15_11);
        let current_base = fixture(GRYPE_15_11);
        let base_keys: std::collections::HashSet<String> = current_base
            .iter()
            .flat_map(|finding| finding.ids().map(str::to_lowercase).collect::<Vec<_>>())
            .collect();
        let attributable: Vec<Finding> = attributable_findings()
            .into_iter()
            .filter(|finding| {
                finding
                    .ids()
                    .any(|id| base_keys.contains(&id.to_lowercase()))
            })
            .collect();

        let (base, _) = base_check(fake, &scan).await;
        let line = &base.entries[0];
        assert_eq!(attributable.len(), 37);
        assert_eq!(base.status, "ok");
        assert_eq!(base.attributable, Some(37));
        assert_eq!(sum(&line["fixes"]), 37);
        assert!(!fixed_ids_of(line).contains(&"CVE-2099-0001"));
        assert!(
            fixed_ids_of(line)
                .iter()
                .all(|id| base_keys.contains(&id.to_lowercase()))
        );
    }

    #[tokio::test]
    async fn says_that_no_newer_base_image_is_better_when_the_candidate_has_as_many_findings() {
        let scan = dockerfile_scan("postgres:15.0-alpine");
        let (base, _) = base_check(base_fake().grype(LINE, GRYPE), &scan).await;
        assert_eq!(base.status, "no_better");
        assert_eq!(base.attributable, Some(attributable_findings().len()));
        assert_eq!(base.entries[0]["better"], json!(false));
        assert_eq!(
            message(
                &stored(
                    &scan,
                    &Outcome::new("skipped", None, Vec::new()),
                    Some(&base)
                ),
                true
            )
            .as_deref(),
            Some("No newer version of postgres is better yet. Coolify checks again every day.")
        );
    }

    #[tokio::test]
    async fn learns_which_findings_come_from_the_base_image_also_without_a_newer_tag() {
        // No newer tag in the line.
        let scan = dockerfile_scan("postgres:15.15-alpine");
        let fake = base_fake().grype(LINE, GRYPE);
        let (base, check) = base_check(fake, &scan).await;
        assert_eq!(base.status, "no_better");
        assert!(base.entries.is_empty());
        assert_eq!(base.attributable, Some(attributable_findings().len()));
        assert!(!base.attributable_ids.iter().any(|id| id == "CVE-2099-0001"));
        assert_eq!(base.current_digest.as_deref(), Some(LINE));
        assert_eq!(check.backend.count("digest"), 1);

        // The newer tag has the same digest.
        let scan = dockerfile_scan("postgres:15.0-alpine");
        let fake = base_fake().digest("15.15-alpine", REPO_DIGEST);
        let (base, check) = base_check(fake, &scan).await;
        assert_eq!(base.status, "no_better");
        assert!(base.entries.is_empty());
        assert_eq!(base.attributable, Some(attributable_findings().len()));
        assert_eq!(check.backend.count("sbom"), 1);
    }

    #[tokio::test]
    async fn reuses_the_sbom_of_the_current_base_image() {
        // Without a newer tag.
        let scan = dockerfile_scan("postgres:15.0-alpine");
        let fake = base_fake()
            .digest("15.15-alpine", REPO_DIGEST)
            .sbom(REPO_DIGEST);
        let (base, check) = base_check(fake, &scan).await;
        assert_eq!(base.status, "no_better");
        assert_eq!(base.attributable, Some(attributable_findings().len()));
        assert_eq!(check.backend.count("sbom"), 0);
        assert_eq!(check.backend.count("grype"), 1);
        assert_eq!(check.backend.scans_today, 0);

        // With a newer tag only the candidate is scanned from the registry.
        let (base, check) = base_check(base_fake().sbom(REPO_DIGEST), &scan).await;
        assert_eq!(base.status, "ok");
        assert_eq!(check.backend.count("sbom"), 1);
        assert_eq!(check.backend.scans_today, 1);
    }

    #[tokio::test]
    async fn counts_both_base_image_scans_toward_the_daily_limit_and_records_the_limit() {
        // (scans today, registry scans, whether the current base image was scanned)
        for (scans_today, registry_scans, attributed) in [(20, 0, false), (19, 1, true)] {
            let scan = dockerfile_scan("postgres:15.0-alpine");
            let mut fake = base_fake();
            fake.scans_today = scans_today;
            let (base, check) = base_check(fake, &scan).await;
            assert_eq!(base.status, "failed");
            assert_eq!(base.error, Some("daily_limit"));
            assert!(base.entries.is_empty());
            assert_eq!(
                base.attributable,
                attributed.then(|| attributable_findings().len())
            );
            assert_eq!(check.backend.count("sbom"), registry_scans);
            assert_eq!(check.backend.scans_today, 20);
            assert_eq!(
                message_on(
                    &stored(
                        &scan,
                        &Outcome::new("skipped", None, Vec::new()),
                        Some(&base)
                    ),
                    true,
                    Some("server-1")
                )
                .as_deref(),
                Some(
                    "Coolify reached the daily limit of version checks on server-1. It checks again tomorrow."
                )
            );
        }
    }

    #[tokio::test]
    async fn checks_the_disk_space_for_the_app_image_size_before_the_base_image_scans() {
        let scan = dockerfile_scan("postgres:15.0-alpine");
        let mut fake = base_fake();
        fake.disk_free = (4_000_000_000, 50_000_000_000);
        let (base, check) = base_check(fake, &scan).await;
        assert_eq!(check.backend.count("sbom"), 0);
        assert_eq!(check.backend.count("inspect"), 1);
        assert_eq!(base.status, "failed");
        assert_eq!(base.error, Some("disk"));
        assert_eq!(check.backend.scans_today, 0);
    }

    #[tokio::test]
    async fn cannot_compare_a_base_image_without_a_version_tag_but_learns_the_origin() {
        let scan = dockerfile_scan("postgres:latest");
        let (base, check) = base_check(base_fake().digest("latest", REPO_DIGEST), &scan).await;
        assert_eq!(base.status, "cannot_compare");
        assert_eq!(base.error, None);
        assert!(base.entries.is_empty());
        assert_eq!(base.attributable, Some(attributable_findings().len()));
        assert_eq!(base.current_digest.as_deref(), Some(REPO_DIGEST));
        assert_eq!(check.backend.count("sbom"), 1);
        assert_eq!(check.backend.count("tags"), 0);
        let row = stored(
            &scan,
            &Outcome::new("skipped", None, Vec::new()),
            Some(&base),
        );
        assert_eq!(
            row.candidates.as_ref().unwrap()["base"]["image"],
            json!("postgres:latest")
        );
    }

    #[tokio::test]
    async fn scans_a_base_image_pinned_by_digest_without_asking_the_registry_for_its_digest() {
        let scan = dockerfile_scan(&format!("postgres@{REPO_DIGEST}"));
        let (base, check) = base_check(base_fake(), &scan).await;
        assert_eq!(base.status, "cannot_compare");
        assert_eq!(base.attributable, Some(attributable_findings().len()));
        assert_eq!(base.current_digest.as_deref(), Some(REPO_DIGEST));
        assert_eq!(check.backend.count("digest"), 0);
    }

    #[tokio::test]
    async fn leaves_the_base_origin_unknown_when_the_base_image_could_not_be_scanned() {
        // The registry does not know the tag.
        let scan = dockerfile_scan("postgres:latest");
        let (base, _) = base_check(base_fake(), &scan).await;
        assert_eq!(base.status, "failed");
        assert_eq!(base.error, Some("registry"));
        assert_eq!(base.attributable, None);
        assert_eq!(base.current_digest, None);

        // A base image reference that cannot be read.
        let scan = dockerfile_scan("Not An Image");
        let (base, check) = base_check(base_fake(), &scan).await;
        assert_eq!(base.status, "cannot_compare");
        assert_eq!(base.error, None);
        assert_eq!(base.attributable, None);
        assert!(check.backend.ran.is_empty());

        // The digest of the newer tag cannot be read: the current base image is not scanned.
        let scan = dockerfile_scan("postgres:15.0-alpine");
        let fake = base_fake().digests(&[("15.0-alpine", REPO_DIGEST)]);
        let (base, check) = base_check(fake, &scan).await;
        assert_eq!(base.status, "failed");
        assert_eq!(base.error, Some("registry"));
        assert_eq!(base.attributable, None);
        assert_eq!(base.current_digest.as_deref(), Some(REPO_DIGEST));
        assert_eq!(check.backend.count("grype"), 0);
    }

    #[tokio::test]
    async fn leaves_the_base_origin_unknown_when_the_scan_of_the_current_base_image_was_cut() {
        let scan = dockerfile_scan("postgres:15.0-alpine");
        let mut fake = base_fake();
        fake.grype_cut = true;
        let (base, _) = base_check(fake, &scan).await;
        assert_eq!(base.status, "no_better");
        assert_eq!(base.attributable, None);
        assert!(base.attributable_ids.is_empty());
        assert_eq!(base.entries[0]["better"], json!(false));
    }

    #[tokio::test]
    async fn both_checks_start_with_a_fresh_registry_state() {
        // A Compose app is not a Dockerfile build: `both` runs what `needs` says.
        let scan = dockerfile_scan("postgres:15.0-alpine");
        let rules = ignores(&[]);
        let subject = Subject {
            scan: &scan,
            rules: &rules,
            platform: Some("linux/amd64"),
        };
        let mut check = check_with(base_fake(), false);
        let (result, base) = check
            .both(
                &subject,
                Needs {
                    image: false,
                    base: true,
                },
                0,
            )
            .await
            .unwrap();
        assert_eq!(result, Outcome::new("skipped", None, Vec::new()));
        assert_eq!(base.unwrap().status, "ok");

        let mut check = check_with(Fake::new(), false);
        let (result, base) = check.both(&subject, Needs::default(), 0).await.unwrap();
        assert_eq!(result.status, "skipped");
        assert_eq!(base, None);
        assert!(!check.ran_on_server);
    }

    // The pure decisions.

    fn application(build_pack: &str) -> ScanOwner {
        owner(APPLICATION_MORPH, json!({"build_pack": build_pack}))
    }

    fn database() -> ScanOwner {
        owner(
            "App\\Models\\StandalonePostgresql",
            json!({"image": "postgres:15.0-alpine"}),
        )
    }

    #[test]
    fn candidates_are_looked_for_only_for_images_the_user_does_not_build() {
        let rules = ignores(&[]);
        let scan = postgres_scan();
        let image_only = Needs {
            image: true,
            base: false,
        };
        assert_eq!(needs_with(&scan, &database(), &rules), image_only);
        assert_eq!(
            needs_with(&scan, &application("dockerimage"), &rules),
            image_only
        );
        // An image a Compose app pulls, not one it builds.
        assert_eq!(
            needs_with(&scan, &application("dockercompose"), &rules),
            image_only
        );
        let mut built = scan.clone();
        built.builds_image = true;
        assert_eq!(
            needs_with(&built, &application("dockercompose"), &rules),
            Needs::default()
        );
        // `it('does nothing for an application that Coolify builds')`.
        for build_pack in ["dockerfile", "nixpacks", "railpack", "static"] {
            assert_eq!(
                needs_with(&scan, &application(build_pack), &rules),
                Needs::default(),
                "{build_pack}"
            );
        }
        // Only a completed scan.
        let mut failed = scan.clone();
        failed.status = STATUS_FAILED.to_owned();
        assert_eq!(needs_with(&failed, &database(), &rules), Needs::default());
    }

    /// `it('does nothing for an image without relevant findings on a supported distro')` and
    /// `it('looks for candidates for an end-of-life distro or a relevant finding without a fix')`.
    #[test]
    fn candidates_need_a_relevant_finding_or_an_end_of_life_distro() {
        let rules = ignores(&[]);
        let supported = json!({"eol_distro": null, "truncated": false});

        let mut harmless = postgres_scan();
        harmless.findings = Some(findings_value(&[
            finding("CVE-1", "medium"),
            finding("CVE-2", "low"),
        ]));
        harmless.summary = Some(supported.clone());
        assert_eq!(needs_with(&harmless, &database(), &rules), Needs::default());
        // Every medium counts with "Count medium problems".
        assert!(needs_with(&harmless, &database(), &rules.with_all_mediums()).image);

        let mut eol = postgres_scan();
        eol.findings = Some(json!([]));
        assert!(needs_with(&eol, &database(), &rules).image);

        let mut not_fixed = postgres_scan();
        let mut unfixed = finding("CVE-2025-1094", "high");
        unfixed.fix_state = "not-fixed".to_owned();
        unfixed.fixed_in = None;
        not_fixed.findings = Some(findings_value(&[unfixed]));
        not_fixed.summary = Some(supported.clone());
        assert!(needs_with(&not_fixed, &database(), &rules).image);
        // An ignored finding does not count.
        assert_eq!(
            needs_with(&not_fixed, &database(), &ignores(&["CVE-2025-1094"])),
            Needs::default()
        );

        let mut no_summary = postgres_scan();
        no_summary.summary = None;
        no_summary.findings = None;
        assert_eq!(
            needs_with(&no_summary, &database(), &rules),
            Needs::default()
        );
    }

    /// `it('looks for no base image candidates without relevant findings or for another build
    /// pack')`.
    #[test]
    fn a_base_check_needs_a_dockerfile_build_with_a_known_base_image() {
        let rules = ignores(&[]);
        let scan = dockerfile_scan("postgres:15.0-alpine");
        let base_only = Needs {
            image: false,
            base: true,
        };
        assert_eq!(
            needs_with(&scan, &application("dockerfile"), &rules),
            base_only
        );
        // No findings.
        let mut clean = scan.clone();
        clean.findings = Some(json!([]));
        clean.summary = Some(json!({"eol_distro": null, "truncated": false}));
        assert_eq!(
            needs_with(&clean, &application("dockerfile"), &rules),
            Needs::default()
        );
        // No base image.
        let mut unknown = scan.clone();
        unknown.base_image_ref = None;
        assert_eq!(
            needs_with(&unknown, &application("dockerfile"), &rules),
            Needs::default()
        );
        unknown.base_image_ref = Some("  ".to_owned());
        assert_eq!(
            needs_with(&unknown, &application("dockerfile"), &rules),
            Needs::default()
        );
        // Nixpacks.
        assert_eq!(
            needs_with(&scan, &application("nixpacks"), &rules),
            Needs::default()
        );
        // A database has no Dockerfile; its image is checked.
        assert_eq!(
            needs_with(&scan, &database(), &rules),
            Needs {
                image: true,
                base: false
            }
        );
    }

    #[test]
    fn a_check_is_fresh_for_20_hours_when_its_status_needs_no_retry() {
        let now = Utc.with_ymd_and_hms(2026, 11, 10, 12, 0, 0).unwrap();
        assert!(checked_recently("2026-11-10T11:59:00+00:00", now));
        assert!(checked_recently("2026-11-09T16:00:01+00:00", now));
        assert!(!checked_recently("2026-11-09T16:00:00+00:00", now));
        assert!(!checked_recently("2026-11-09T12:00:00+00:00", now));
        // Another offset, and the format of the database.
        assert!(checked_recently("2026-11-09T18:30:00+02:00", now));
        assert!(checked_recently("2026-11-10 08:00:00", now));
        assert!(!checked_recently("yesterday-ish", now));
        assert!(!checked_recently("", now));

        let mut scan = postgres_scan();
        assert_eq!(fresh_checked_at(&scan), None);
        for status in ["ok", "no_better", "cannot_compare", "skipped"] {
            scan.candidates = Some(
                json!({"status": status, "checked_at": "2026-11-10T11:00:00+00:00", "error": null, "entries": []}),
            );
            assert_eq!(
                fresh_checked_at(&scan),
                Some("2026-11-10T11:00:00+00:00"),
                "{status}"
            );
        }
        for status in ["failed", "auth_required"] {
            scan.candidates =
                Some(json!({"status": status, "checked_at": "2026-11-10T11:00:00+00:00"}));
            assert_eq!(fresh_checked_at(&scan), None, "{status}");
        }
        scan.candidates = Some(json!({"status": "ok", "checked_at": null}));
        assert_eq!(fresh_checked_at(&scan), None);
        // Older rows hold a bare list of entries.
        scan.candidates = Some(json!([{"kind": "line"}]));
        assert_eq!(fresh_checked_at(&scan), None);

        // The base check needs a retry.
        scan.candidates = Some(
            json!({"status": "skipped", "checked_at": "2026-11-10T11:00:00+00:00",
            "base": {"status": "failed", "error": "registry"}}),
        );
        assert!(fresh_checked_at(&scan).is_some());
        assert!(!base_result_is_fresh(&scan));
        scan.candidates = Some(
            json!({"status": "skipped", "checked_at": "2026-11-10T11:00:00+00:00",
            "base": {"status": "cannot_compare"}}),
        );
        assert!(base_result_is_fresh(&scan));
        scan.candidates = Some(json!({"status": "ok", "checked_at": "2026-11-10T11:00:00+00:00"}));
        assert!(!base_result_is_fresh(&scan));
    }

    #[test]
    fn messages_of_a_check_without_a_better_candidate() {
        let mut scan = candidates_scan("ghcr.io/acme/api:1.0", Some(REPO_DIGEST));
        assert_eq!(message(&scan, false), None);
        let with = |scan: &mut ImageScanRow, status: &str, error: Value| {
            scan.candidates = Some(
                json!({"status": status, "checked_at": "2026-11-10T11:00:00+00:00",
                "error": error, "tags_truncated": false, "entries": []}),
            );
        };
        with(&mut scan, "ok", Value::Null);
        assert_eq!(message(&scan, false), None);
        with(&mut scan, "skipped", Value::Null);
        assert_eq!(message(&scan, false), None);
        with(&mut scan, "no_better", Value::Null);
        assert_eq!(
            message(&scan, false).as_deref(),
            Some(
                "No newer version of ghcr.io/acme/api is better yet. Coolify checks again every day."
            )
        );
        with(&mut scan, "auth_required", json!("registry_auth"));
        assert_eq!(
            message(&scan, false).as_deref(),
            Some(
                "Coolify cannot read the tags of ghcr.io/acme/api. Log in to ghcr.io on the server."
            )
        );
        with(&mut scan, "failed", json!("registry"));
        assert_eq!(
            message_on(&scan, false, Some("server-1")).as_deref(),
            Some(
                "Coolify could not check for newer versions of ghcr.io/acme/api. It tries again tomorrow."
            )
        );
        // The base check has its own result and its own image.
        assert_eq!(message(&scan, true), None);
        scan.base_image_ref = Some("docker.io/library/node:22".to_owned());
        scan.candidates = Some(json!({"status": "skipped", "base": {"status": "auth_required"}}));
        assert_eq!(message(&scan, false), None);
        assert_eq!(
            message_on(&scan, true, Some("server-1")).as_deref(),
            Some("Coolify cannot read the tags of node. Log in to docker.io on server-1.")
        );
        // An image reference that cannot be read has no message.
        scan.image_ref = "Not An Image".to_owned();
        scan.candidates = Some(json!({"status": "no_better"}));
        assert_eq!(message(&scan, false), None);
    }

    #[test]
    fn the_status_of_a_version_check() {
        // (any better, auth required, error, version tag, scanned any)
        assert_eq!(check_status(true, true, true, true, true), "ok");
        assert_eq!(check_status(false, true, true, true, true), "auth_required");
        assert_eq!(check_status(false, false, true, true, true), "failed");
        assert_eq!(
            check_status(false, false, false, false, false),
            "cannot_compare"
        );
        // A newer build of a tag that is not a version was scanned.
        assert_eq!(check_status(false, false, false, false, true), "no_better");
        assert_eq!(check_status(false, false, false, true, false), "no_better");
    }

    #[test]
    fn the_daily_count_starts_again_on_a_new_day() {
        let today = NaiveDate::from_ymd_opt(2026, 11, 10).unwrap();
        let counters =
            |scans_today: i64, date: Option<NaiveDate>| CandidateScanCounters { scans_today, date };
        assert_eq!(daily_scans_left(None, today), 20);
        assert_eq!(daily_scans_left(Some(&counters(7, None)), today), 20);
        assert_eq!(daily_scans_left(Some(&counters(7, Some(today))), today), 13);
        assert_eq!(daily_scans_left(Some(&counters(20, Some(today))), today), 0);
        assert_eq!(daily_scans_left(Some(&counters(25, Some(today))), today), 0);
        assert_eq!(
            daily_scans_left(Some(&counters(20, today.pred_opt())), today),
            20
        );
        assert_eq!(MAX_CANDIDATE_SCANS, 2);
        assert_eq!(DAILY_CANDIDATE_SCANS, 20);
        assert_eq!(MAJOR_SCAN_DAILY_RESERVE, 5);
        assert_eq!(FRESH_HOURS, 20);
    }

    #[test]
    fn the_registry_login_is_read_once_and_only_after_an_anonymous_failure() {
        let ok = scan_image::step_result(Some(&marker("tags", "1.0\n1.1", 0, None)), "tags").ok();
        let denied = scan_image::step_result(Some(&marker("tags", "", 1, None)), "tags").ok();
        let timeout = scan_image::step_result(Some(&marker("tags", "", 124, None)), "tags").ok();
        let killed = scan_image::step_result(Some(&marker("tags", "", 137, None)), "tags").ok();

        // An anonymous call that works.
        let mut state = RegistryState::default();
        assert_eq!(
            state.after_first_call(ok.as_ref()),
            FirstCall::Payload("1.0\n1.1".to_owned())
        );
        assert!(state.anonymous_worked);
        // A later failure is a registry error, not a missing login.
        assert_eq!(state.after_first_call(denied.as_ref()), FirstCall::Failed);
        assert!(!state.login_read);
        assert!(!state.auth_required);

        // A batch that did not run, or a timeout, reads no login.
        let mut state = RegistryState::default();
        assert_eq!(state.after_first_call(None), FirstCall::Failed);
        assert_eq!(state.after_first_call(timeout.as_ref()), FirstCall::Failed);
        assert_eq!(state.after_first_call(killed.as_ref()), FirstCall::Failed);
        assert!(!state.login_read);

        // A refused anonymous call reads the login once.
        assert_eq!(
            state.after_first_call(denied.as_ref()),
            FirstCall::ReadLogin
        );
        assert!(state.login_read);
        assert!(state.after_login(RegistryLogin::Ok("e30=".to_owned()), false));
        assert_eq!(state.auth.as_deref(), Some("e30="));
        assert_eq!(
            state.after_second_call(ok.as_ref()),
            Some("1.0\n1.1".to_owned())
        );
        assert!(!state.auth_required);
        // A call with the login that works is not an anonymous call.
        assert_eq!(
            state.after_first_call(ok.as_ref()),
            FirstCall::Payload("1.0\n1.1".to_owned())
        );
        assert!(!state.anonymous_worked);
        // A failure with the login does not read it again.
        assert_eq!(state.after_first_call(denied.as_ref()), FirstCall::Failed);

        // The login does not work either: a login is needed.
        let mut state = RegistryState::default();
        assert_eq!(
            state.after_first_call(denied.as_ref()),
            FirstCall::ReadLogin
        );
        assert!(state.after_login(RegistryLogin::Ok("e30=".to_owned()), true));
        assert_eq!(state.after_second_call(denied.as_ref()), None);
        assert!(state.auth_required);

        // Without a login: a private image needs one, a Docker Hub official image does not; a
        // credential helper always needs a login that Coolify can read.
        let mut state = RegistryState::default();
        assert!(!state.after_login(RegistryLogin::None, false));
        assert!(state.auth_required);
        let mut state = RegistryState::default();
        assert!(!state.after_login(RegistryLogin::None, true));
        assert!(!state.auth_required);
        let mut state = RegistryState::default();
        assert!(!state.after_login(RegistryLogin::Helper, true));
        assert!(state.auth_required);
    }

    #[test]
    fn small_parsers() {
        assert_eq!(
            tag_lines("1.0\n 1.1 \r\n\n\t\n2.0"),
            vec!["1.0", "1.1", "2.0"]
        );
        assert!(tag_lines("").is_empty());
        assert_eq!(
            inspected_size(&format!(
                "sha256:{CURRENT_HEX}|postgres@{REPO_DIGEST}|linux/amd64|2000000000\n"
            )),
            Some(2_000_000_000)
        );
        assert_eq!(inspected_size("sha256:abc||linux/amd64|12"), Some(12));
        assert_eq!(inspected_size("sha256:abc|linux/amd64|12"), None);
        assert_eq!(inspected_size("a|b|c|-1"), None);
        assert_eq!(inspected_size("a|b|c|1234567890123456789"), None);
        assert_eq!(inspected_size(""), None);
        assert_eq!(
            iso8601(Utc.with_ymd_and_hms(2026, 11, 10, 9, 5, 7).unwrap()),
            "2026-11-10T09:05:07+00:00"
        );
        assert_eq!(queued_cache_key(42), "image-scan:version-check-queued:42");
    }

    #[test]
    fn the_queued_mark_lasts_until_the_check_ends() {
        let mut scan = postgres_scan();
        scan.id = -4712;
        assert!(!is_queued(&scan));
        mark_queued(&scan, 60);
        assert!(is_queued(&scan));
        forget_queued(&scan);
        assert!(!is_queued(&scan));
    }

    #[test]
    fn a_scanned_major_is_never_better() {
        let image = ImageRef::parse("postgres:15.0-alpine").unwrap();
        let report = CandidateReport {
            findings: Vec::new(),
            eol: Some(EolDistro {
                name: "alpine".to_owned(),
                version: "3.16.2".to_owned(),
            }),
            truncated: false,
        };
        let current = [finding("CVE-1", "high")];
        let entry = candidate_entry(
            &image,
            "major",
            "16.4-alpine",
            LINE,
            &current,
            false,
            &ignores(&[]),
            &report,
            "2026-11-10T12:00:00+00:00",
        )
        .unwrap();
        // The candidate fixes the finding but runs an end-of-life distro: as many problems.
        assert_eq!(
            entry,
            json!({"ref": "postgres:16.4-alpine", "digest": LINE, "kind": "major", "better": false,
                "fixed_ids": ["CVE-1"], "fixes": {"critical": 0, "high": 1, "medium": 0}, "added": 0,
                "left": {"critical": 0, "high": 0, "medium": 0},
                "eol": {"name": "alpine", "version": "3.16.2"}, "scanned_at": "2026-11-10T12:00:00+00:00"})
        );
        let major = scanned_major_entry(entry);
        assert_eq!(major["better"], json!(false));
        assert_eq!(major["fixes_problems"], json!(false));

        let supported = CandidateReport {
            eol: None,
            ..report.clone()
        };
        let entry = candidate_entry(
            &image,
            "major",
            "16.4-alpine",
            LINE,
            &current,
            false,
            &ignores(&[]),
            &supported,
            "2026-11-10T12:00:00+00:00",
        )
        .unwrap();
        assert_eq!(entry["better"], json!(true));
        let major = scanned_major_entry(entry);
        assert_eq!(major["better"], json!(false));
        assert_eq!(major["fixes_problems"], json!(true));

        // A candidate whose scan output was cut is never better.
        let cut = CandidateReport {
            truncated: true,
            ..supported
        };
        let entry = candidate_entry(
            &image,
            "line",
            "15.15-alpine",
            LINE,
            &current,
            false,
            &ignores(&[]),
            &cut,
            "2026-11-10T12:00:00+00:00",
        )
        .unwrap();
        assert_eq!(entry["better"], json!(false));
    }
}
