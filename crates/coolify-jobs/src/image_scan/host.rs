//! `App\Actions\ImageScan\ScanHostPackages` and `App\Jobs\ScanHostPackagesJob`: scans the OS
//! packages of a server with the image scanner tools and saves the result on its
//! `server_host_scans` row (`ServerHostScan`).
//!
//! The scan runs under the image scan lock of the server, so it never runs at the same time as an
//! image scan. A busy lock runs the job again later; after the last try it records `lock_busy`.
//! A failed scan keeps the findings of the last completed scan.

use std::cmp::Ordering;
use std::sync::LazyLock;
use std::time::Duration;

use chrono::NaiveDateTime;
use coolify_db::image_scans::host::{self as host_scans, CompletedHostScan, MAX_STORED_FINDINGS};
use regex::Regex;
use serde_json::{Map, Value};

use super::commands::{HOST_SBOM, ScannerCommands};
use super::error::{ErrorCode, ScanFailed};
use super::finding::Finding;
use super::grype::{self, EolDistro, GrypeReport};
use super::rules::{self, ExploitSignal, RelevanceRules};
use super::scan_image::{self, StepResult};
use super::{ScanServer, cache, new_run_id, sbom_document, scan_jobs};
use crate::Jobs;

const TIMEOUT_DEFAULT: i64 = 120;
/// A host has more findings than an image; all of them are counted.
const MAX_PARSED_FINDINGS: usize = 20000;
/// `ScanHostPackagesJob::RETRY_DELAY_SECONDS`.
const RETRY_DELAY_SECONDS: u64 = 300;
/// `ScanHostPackagesJob::$tries`.
const TRIES: u32 = 6;

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).unwrap_or_else(|_| unreachable!())
}

static DEBIAN_KERNEL_PACKAGE: LazyLock<Regex> =
    LazyLock::new(|| re(r"^linux-.*?-(\d+\.\d+(?:\.\d+)?-\d+)(?:-|$)"));
static DEBIAN_KERNEL_RELEASE: LazyLock<Regex> = LazyLock::new(|| re(r"^(\d+\.\d+(?:\.\d+)?-\d+)"));
static EPOCH: LazyLock<Regex> = LazyLock::new(|| re(r"^\d+:"));
static SUSE_RELEASE: LazyLock<Regex> = LazyLock::new(|| re(r"^(.+)-[a-z][a-z0-9_]*$"));
static LAST_NUMBER: LazyLock<Regex> = LazyLock::new(|| re(r"\.\d+$"));
static LAST_PART: LazyLock<Regex> = LazyLock::new(|| re(r"\.[^.]+$"));

/// `CheckUpdates::isOtherKernelPackage($package, $version, $runningRelease)`: true for a package
/// of an installed kernel that the server does not run (`uname -r`), older or newer. Its
/// vulnerabilities are no risk now. Debian kernel packages carry the release in the name
/// (`linux-modules-6.8.0-139-generic`, `linux-headers-6.8.0-139`), RPM kernel packages the
/// version.
///
/// Ported here because the host scan is its only caller in the scan engine; the patches page of
/// upstream v4.7.5 uses the same rule.
pub fn is_other_kernel_package(package: &str, version: &str, running_release: &str) -> bool {
    if running_release.trim().is_empty() {
        return false;
    }
    if let Some(package_kernel) = DEBIAN_KERNEL_PACKAGE.captures(package) {
        return DEBIAN_KERNEL_RELEASE
            .captures(running_release)
            .is_some_and(|running_kernel| package_kernel[1] != running_kernel[1]);
    }
    if !is_rpm_kernel_package(package) {
        return false;
    }
    let version = EPOCH.replace(version, "");
    // SUSE: uname -r 6.4.0-150600.23.17-default for kernel-default 6.4.0-150600.23.17.1 (the
    // last number is the rebuild).
    if let Some(suse_release) = SUSE_RELEASE.captures(running_release) {
        return LAST_NUMBER.replace(&version, "") != suse_release[1];
    }
    version != LAST_PART.replace(running_release, "")
}

/// `CheckUpdates::isRpmKernelPackage($package)`: packages of a bootable RPM kernel (dnf, zypper).
/// Userspace packages such as kernel-tools or kernel-headers have their own version and are not
/// part of a kernel.
fn is_rpm_kernel_package(package: &str) -> bool {
    use crate::server_ops::patches::risk_for;
    risk_for("dnf", package) == Some("kernel") || risk_for("zypper", package) == Some("kernel")
}

/// What a completed host scan stores.
#[derive(Debug, Clone, PartialEq)]
struct HostResult {
    eol: Option<Value>,
    summary: Value,
    findings: Value,
    findings_truncated: bool,
}

/// `ScanHostPackages::rank()`: findings in kernels that the server does not run come last, then
/// problems with an exploit signal first, then the ones an update fixes, then the highest risk.
fn rank(finding: &Finding, running_kernel: &str) -> (u8, u8, u8, f64) {
    let other_kernel = u8::from(is_other_kernel_package(
        &finding.package,
        &finding.version,
        running_kernel,
    ));
    let signal = match rules::exploit_signal(finding) {
        Some(ExploitSignal::Exploited) => 0,
        Some(ExploitSignal::Likely) => 1,
        None => 2,
    };
    (
        other_kernel,
        signal,
        u8::from(finding.fixed_in.is_none()),
        -finding.risk.unwrap_or(0.0),
    )
}

fn compare_rank(a: &(u8, u8, u8, f64), b: &(u8, u8, u8, f64)) -> Ordering {
    (a.0, a.1, a.2)
        .cmp(&(b.0, b.1, b.2))
        .then_with(|| a.3.partial_cmp(&b.3).unwrap_or(Ordering::Equal))
}

/// The pure part of `ScanHostPackages::complete()`: the relevant findings (spec 15.2), problems
/// with an exploit signal first, then the ones an update fixes. Findings in installed kernels
/// that the server does not run are no risk now: they are not counted (only as `other_kernels`)
/// and come last, so they do not push out the others.
fn host_result(
    report: &GrypeReport,
    cut: bool,
    running_kernel: &str,
    now: NaiveDateTime,
) -> HostResult {
    let relevance = RelevanceRules::from_ignores(Vec::new(), false, now);
    let mut relevant: Vec<&Finding> = report
        .findings
        .iter()
        .filter(|finding| rules::is_relevant(finding, &relevance))
        .collect();
    relevant.sort_by(|a, b| compare_rank(&rank(a, running_kernel), &rank(b, running_kernel)));

    let mut counts: Vec<(String, i64)> = [
        "critical",
        "high",
        "medium",
        "low",
        "unknown",
        "fixable",
        "exploited",
        "likely",
        "other_kernels",
    ]
    .into_iter()
    .map(|key| (key.to_owned(), 0))
    .collect();
    let mut add = |key: &str, by: i64| match counts.iter_mut().find(|(k, _)| k == key) {
        Some(slot) => slot.1 += by,
        // `$summary[$finding->severity]++` of a severity that is not in the list.
        None => counts.push((key.to_owned(), by)),
    };
    for finding in &relevant {
        if is_other_kernel_package(&finding.package, &finding.version, running_kernel) {
            add("other_kernels", 1);
            continue;
        }
        add(&finding.severity, 1);
        add("fixable", i64::from(finding.fixed_in.is_some()));
        let signal = rules::exploit_signal(finding);
        add(
            "exploited",
            i64::from(signal == Some(ExploitSignal::Exploited)),
        );
        add("likely", i64::from(signal == Some(ExploitSignal::Likely)));
    }

    // PHP appends a new severity key after `running_kernel`; the known keys keep upstream's order.
    let known = 9;
    let mut summary = Map::new();
    for (key, count) in &counts[..known] {
        summary.insert(key.clone(), Value::from(*count));
    }
    summary.insert("total".to_owned(), Value::from(report.findings.len()));
    summary.insert(
        "running_kernel".to_owned(),
        if running_kernel.is_empty() {
            Value::Null
        } else {
            Value::String(running_kernel.to_owned())
        },
    );
    for (key, count) in &counts[known..] {
        summary.insert(key.clone(), Value::from(*count));
    }

    HostResult {
        eol: report.eol.as_ref().map(EolDistro::to_value),
        summary: Value::Object(summary),
        findings: Value::Array(
            relevant
                .iter()
                .take(MAX_STORED_FINDINGS)
                .map(|finding| finding.to_value())
                .collect(),
        ),
        findings_truncated: cut || report.truncated || relevant.len() > MAX_STORED_FINDINGS,
    }
}

/// The size in KiB that the `packages-size` step reported.
fn packages_size(packages: &StepResult) -> Result<i64, ScanFailed> {
    match packages.size {
        Some(size) if packages.code == 0 => Ok(size),
        _ => Err(ScanFailed::detail(
            ErrorCode::Unknown,
            format!("step packages-size exited with code {}", packages.code),
        )),
    }
}

/// The `copy` marker of the SBOM step: 1 when no package database was found, 2 (or none) when a
/// copy failed, so a partial database never reads as a complete scan.
fn assert_copied(sbom: &StepResult) -> Result<(), ScanFailed> {
    let copy = sbom.parsed.exit_code("copy").unwrap_or(2);
    if copy == 0 {
        return Ok(());
    }
    Err(ScanFailed::detail(
        if copy == 1 {
            ErrorCode::NoPackageDatabase
        } else {
            ErrorCode::Unknown
        },
        format!("step copy exited with code {copy}"),
    ))
}

/// A host scan that stopped: with an error code, or with an error that is not a scan problem.
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

/// `ScanHostPackages::handle($server)`. False when another scan holds the server lock: the caller
/// tries again later.
pub async fn scan_host_packages(jobs: &Jobs, server: &ScanServer) -> bool {
    if !scan_jobs::can_run_on(server) {
        return true;
    }

    let lock = scan_image::server_lock(server);
    if !scan_image::acquire_server_lock(&lock, server.id(), 0).await {
        return false;
    }

    let commands = match server.commands(&new_run_id()) {
        Ok(commands) => commands,
        Err(e) => {
            lock.release();
            tracing::error!(server_id = server.id(), error = %e, "ScanHostPackages could not start");
            return true;
        }
    };
    match scan_steps(jobs, server, &commands).await {
        Ok(()) => {}
        Err(Stop::Failed(e)) => {
            e.log(&server.name(), server.uuid(), "the host packages");
            record_failure(jobs, server, e.code, &e.error_context(&Map::new())).await;
        }
        Err(Stop::Other(message)) => {
            tracing::error!(server_id = server.id(), error = %message, "ScanHostPackages failed");
            let failure = ScanFailed::detail(ErrorCode::Unknown, message);
            record_failure(
                jobs,
                server,
                ErrorCode::Unknown,
                &failure.error_context(&Map::new()),
            )
            .await;
        }
    }
    server.run(&commands.cleanup(), TIMEOUT_DEFAULT).await;
    lock.release();
    true
}

/// The steps of `handle()` under the server lock.
async fn scan_steps(
    jobs: &Jobs,
    server: &ScanServer,
    commands: &ScannerCommands,
) -> Result<(), Stop> {
    let database_built_at = scan_image::database_built_at(server, commands, Jobs::now()).await?;

    let packages = scan_image::run_step(
        server,
        &commands.host_packages_size(),
        "packages-size",
        TIMEOUT_DEFAULT,
    )
    .await?;
    let size = packages_size(&packages)?;
    scan_image::check_disk_space(server, commands, size.saturating_mul(1024)).await?;

    let sbom = scan_image::run_step(
        server,
        &commands.sbom_from_host(),
        "sbom",
        scan_image::sbom_step_timeout(&commands.host),
    )
    .await?;
    assert_copied(&sbom)?;
    scan_image::assert_succeeded(&sbom, "sbom")?;

    let grype_commands = commands.grype(HOST_SBOM).map_err(|e| {
        ScanFailed::detail(ErrorCode::Unknown, format!("InvalidArgumentException: {e}"))
    })?;
    let grype = scan_image::run_step(
        server,
        &grype_commands,
        "grype",
        scan_image::grype_step_timeout(&commands.host),
    )
    .await?;
    scan_image::assert_succeeded(&grype, "grype")?;

    let running_kernel = server
        .run(&["uname -r".to_owned()], TIMEOUT_DEFAULT)
        .await
        .unwrap_or_default();
    let running_kernel = coolify_remote::text::php_trim(&running_kernel);
    complete(jobs, server, &grype, database_built_at, running_kernel)
        .await
        .map_err(Stop::Other)
}

/// `ScanHostPackages::complete()`.
async fn complete(
    jobs: &Jobs,
    server: &ScanServer,
    grype_step: &StepResult,
    database_built_at: NaiveDateTime,
    running_kernel: &str,
) -> Result<(), String> {
    let now = Jobs::now();
    let (payload, cut) = scan_image::read_payload(grype_step);
    let report = grype::parse_grype_report_with_limit(&payload, MAX_PARSED_FINDINGS);
    let result = host_result(&report, cut, running_kernel, now);
    host_scans::save_completed(
        &jobs.0.db,
        server.id(),
        &CompletedHostScan {
            eol: result.eol.as_ref(),
            summary: &result.summary,
            findings: &result.findings,
            findings_truncated: result.findings_truncated,
            grype_db_built_at: database_built_at,
            scanned_at: now,
        },
        now,
    )
    .await
    .map(|_| ())
    .map_err(|e| e.to_string())
}

/// `ScanHostPackages::recordFailure($server, $code, $context)`: keeps the findings of the last
/// completed scan. `context` holds the values of the error message.
pub async fn record_failure(
    jobs: &Jobs,
    server: &ScanServer,
    code: ErrorCode,
    context: &Map<String, Value>,
) {
    let context = (!context.is_empty()).then(|| Value::Object(context.clone()));
    if let Err(e) = host_scans::save_failure(
        &jobs.0.db,
        server.id(),
        code.value(),
        context.as_ref(),
        Jobs::now(),
    )
    .await
    {
        tracing::error!(server_id = server.id(), error = %e, "ScanHostPackages::recordFailure failed");
    }
}

/// `ScanHostPackagesJob::runningCacheKey($server)`: set while a scan that a user asked for is
/// queued or runs, so the patches page shows the progress after a reload.
pub fn running_cache_key(server_id: i64) -> String {
    format!("server-host-scan-running:{server_id}")
}

/// The key of the `ShouldBeUniqueUntilProcessing` lock of the job.
fn unique_key(server_id: i64) -> String {
    format!("server-host-scan-job:{server_id}")
}

/// `$timeout` and `$uniqueFor` of the job: a run never takes longer than the server scan lock
/// lives.
fn job_timeout() -> Duration {
    Duration::from_secs(scan_image::MAX_LOCK_TTL.unsigned_abs())
}

/// `ScanHostPackagesJob::dispatchManual($server)`: queues a scan and marks it as running. The
/// mark lasts for the longest wait in the queue and the longest run. Each request writes a new
/// token, so a scan that started before this request cannot clear the marker of this request.
pub fn dispatch_manual(jobs: &Jobs, server: &ScanServer) {
    cache::put(
        &running_cache_key(server.id()),
        Value::String(sbom_document::uuid_v4()),
        Duration::from_secs((2 * scan_image::lock_ttl(&server.host())).unsigned_abs()),
    );
    jobs.dispatch_scan_host_packages(server.id());
}

/// One attempt of the job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Attempt {
    Done,
    /// The scan lock was busy: run again after this many seconds.
    Release(u64),
}

/// What an attempt does after the scan: `released` is false when another scan held the lock.
fn after_scan(scanned: bool, attempt: u32) -> (Attempt, bool) {
    match (scanned, attempt < TRIES) {
        (true, _) => (Attempt::Done, false),
        (false, true) => (Attempt::Release(RETRY_DELAY_SECONDS), false),
        // The last try records `lock_busy`.
        (false, false) => (Attempt::Done, true),
    }
}

/// `ScanHostPackagesJob::handle()`.
async fn handle(jobs: &Jobs, server_id: i64, attempt: u32) -> Attempt {
    let Ok(server) = ScanServer::load(jobs, server_id).await else {
        return Attempt::Done;
    };
    let key = running_cache_key(server_id);
    let request = cache::get(&key);

    let scanned = scan_host_packages(jobs, &server).await;
    let (outcome, record_busy) = after_scan(scanned, attempt);
    if outcome != Attempt::Done {
        return outcome;
    }
    if record_busy {
        record_failure(jobs, &server, ErrorCode::LockBusy, &Map::new()).await;
    }

    // A request made during this scan queued a new scan, which clears the marker when it ends.
    if cache::get(&key) == request {
        cache::forget(&key);
    }
    super::events::dispatch_server_patch_check_finished(server.team_id(), server.uuid());
    Attempt::Done
}

impl Jobs {
    /// `ScanHostPackagesJob::dispatch($server)`. The unique lock ends when the job starts
    /// (`ShouldBeUniqueUntilProcessing`), so a scan requested during a running scan (for example,
    /// after a package update) runs after it, instead of being dropped. A job that finds the scan
    /// lock busy runs again 300 seconds later, up to 6 times.
    pub fn dispatch_scan_host_packages(&self, server_id: i64) {
        if !cache::add(&unique_key(server_id), Value::Bool(true), job_timeout()) {
            return;
        }
        let jobs = self.clone();
        tokio::spawn(async move {
            cache::forget(&unique_key(server_id));
            let mut attempt = 1;
            loop {
                match tokio::time::timeout(job_timeout(), handle(&jobs, server_id, attempt)).await {
                    Ok(Attempt::Done) => break,
                    Ok(Attempt::Release(delay)) => {
                        tokio::time::sleep(Duration::from_secs(delay)).await;
                        attempt += 1;
                    }
                    Err(_) => {
                        // `failed()`.
                        cache::forget(&running_cache_key(server_id));
                        break;
                    }
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::super::rules::tests::{finding, now};
    use super::*;

    const GRYPE_HOST: &str = include_str!("../../tests/fixtures/image-scan/grype-host.ndjson");

    fn step(output: &str, name: &str) -> Result<StepResult, ScanFailed> {
        scan_image::step_result(Some(output), name)
    }

    #[test]
    fn saves_the_relevant_host_findings_exploited_and_fixable_ones_first() {
        let report = grype::parse_grype_report_with_limit(GRYPE_HOST, MAX_PARSED_FINDINGS);
        let result = host_result(&report, false, "", now());
        assert!(!result.findings_truncated);
        assert_eq!(result.eol, None);
        assert_eq!(
            result.summary.to_string(),
            r#"{"critical":2,"high":1,"medium":0,"low":1,"unknown":0,"fixable":2,"exploited":2,"likely":1,"other_kernels":0,"total":5,"running_kernel":null}"#
        );
        let stored: Vec<(String, String, Option<String>)> =
            Finding::list_from_value(Some(&result.findings))
                .into_iter()
                .map(|f| (f.display_id, f.package, f.fixed_in))
                .collect();
        let row = |id: &str, package: &str, fixed: Option<&str>| {
            (id.to_owned(), package.to_owned(), fixed.map(str::to_owned))
        };
        assert_eq!(
            stored,
            vec![
                row("CVE-2026-111497", "libbrotli-dev", Some("1.1.0-2+deb13u1")),
                row("CVE-2023-5217", "firefox-esr", None),
                row(
                    "CVE-2024-1283",
                    "gir1.2-javascriptcoregtk-4.1",
                    Some("2.54.0-1~deb13u1")
                ),
                row("CVE-2026-34980", "cups", None),
            ]
        );
    }

    #[test]
    fn a_cut_payload_marks_the_findings_as_truncated() {
        let report = grype::parse_grype_report_with_limit(GRYPE_HOST, MAX_PARSED_FINDINGS);
        assert!(host_result(&report, true, "", now()).findings_truncated);
        let mut cut_report = report.clone();
        cut_report.truncated = true;
        assert!(host_result(&cut_report, false, "", now()).findings_truncated);
    }

    #[test]
    fn stores_at_most_500_findings_and_counts_all_of_them() {
        let findings: Vec<Finding> = (0..MAX_STORED_FINDINGS + 3)
            .map(|n| finding(&format!("CVE-2026-{n}"), "high"))
            .collect();
        let report = GrypeReport {
            findings,
            truncated: false,
            eol: None,
        };
        let result = host_result(&report, false, "6.8.0-139-generic", now());
        assert!(result.findings_truncated);
        assert_eq!(
            result.findings.as_array().map(Vec::len),
            Some(MAX_STORED_FINDINGS)
        );
        assert_eq!(result.summary["high"], json!(MAX_STORED_FINDINGS + 3));
        assert_eq!(result.summary["total"], json!(MAX_STORED_FINDINGS + 3));
        assert_eq!(result.summary["running_kernel"], json!("6.8.0-139-generic"));
    }

    #[test]
    fn findings_in_kernels_that_do_not_run_are_not_counted_and_come_last() {
        let mut old_kernel = finding("CVE-2026-1", "critical");
        old_kernel.package = "linux-modules-6.8.0-130-generic".to_owned();
        old_kernel.version = "6.8.0-130.140".to_owned();
        old_kernel.kev = true;
        let mut running = finding("CVE-2026-2", "high");
        running.package = "linux-modules-6.8.0-139-generic".to_owned();
        running.version = "6.8.0-139.149".to_owned();
        running.fixed_in = Some("6.8.0-140.150".to_owned());
        let mut other = finding("CVE-2026-3", "high");
        other.risk = Some(9.0);
        let low = finding("CVE-2026-4", "low");
        let report = GrypeReport {
            findings: vec![old_kernel, other, running, low],
            truncated: false,
            eol: Some(EolDistro {
                name: "debian".to_owned(),
                version: "10".to_owned(),
            }),
        };
        let result = host_result(&report, false, "6.8.0-139-generic", now());
        assert_eq!(
            result.summary.to_string(),
            r#"{"critical":0,"high":2,"medium":0,"low":0,"unknown":0,"fixable":1,"exploited":0,"likely":0,"other_kernels":1,"total":4,"running_kernel":"6.8.0-139-generic"}"#
        );
        let ids: Vec<String> = Finding::list_from_value(Some(&result.findings))
            .into_iter()
            .map(|f| f.id)
            .collect();
        // The fixable one first, then by risk; the kernel that does not run last.
        assert_eq!(ids, vec!["CVE-2026-2", "CVE-2026-3", "CVE-2026-1"]);
        assert_eq!(
            result.eol,
            Some(
                EolDistro {
                    name: "debian".to_owned(),
                    version: "10".to_owned()
                }
                .to_value()
            )
        );
    }

    #[test]
    fn finds_the_packages_of_kernels_that_the_server_does_not_run() {
        // Upstream `ServerPatchCommandsTest`.
        for (package, version, running, other) in [
            (
                "linux-modules-6.8.0-139-generic",
                "6.8.0-139.149",
                "6.8.0-139-generic",
                false,
            ),
            (
                "linux-modules-6.8.0-130-generic",
                "6.8.0-130.140",
                "6.8.0-139-generic",
                true,
            ),
            (
                "kernel-core",
                "5.14.0-427.13.1.el9_4",
                "5.14.0-427.13.1.el9_4.x86_64",
                false,
            ),
            (
                "kernel-core",
                "5.14.0-362.8.1.el9_3",
                "5.14.0-427.13.1.el9_4.x86_64",
                true,
            ),
            (
                "kernel-tools",
                "5.14.0-362.8.1.el9_3",
                "5.14.0-427.13.1.el9_4.x86_64",
                false,
            ),
            (
                "kernel-headers",
                "5.14.0-362.8.1.el9_3",
                "5.14.0-427.13.1.el9_4.x86_64",
                false,
            ),
            (
                "kernel-default",
                "6.4.0-150600.23.17.1",
                "6.4.0-150600.23.17-default",
                false,
            ),
            (
                "kernel-default",
                "6.4.0-150600.23.14.1",
                "6.4.0-150600.23.17-default",
                true,
            ),
            ("kernel-default", "6.11.8-1.1", "6.11.8-1-default", false),
        ] {
            assert_eq!(
                is_other_kernel_package(package, version, running),
                other,
                "{package} {version} on {running}"
            );
        }
        // Without a running release there is no claim.
        assert!(!is_other_kernel_package(
            "linux-modules-6.8.0-130-generic",
            "6.8.0-130.140",
            ""
        ));
        // An epoch is not part of the kernel version.
        assert!(!is_other_kernel_package(
            "kernel-core",
            "1:5.14.0-427.13.1.el9_4",
            "5.14.0-427.13.1.el9_4.x86_64"
        ));
        assert!(!is_other_kernel_package(
            "openssl",
            "3.0.1",
            "6.8.0-139-generic"
        ));
    }

    #[test]
    fn the_package_databases_are_not_copied_when_their_size_cannot_be_measured() {
        let measured = step("coolify-scan-exit packages-size 0 204800", "packages-size");
        assert_eq!(
            measured.ok().and_then(|s| packages_size(&s).ok()),
            Some(204_800)
        );
        // `du` failed.
        let failed = step("coolify-scan-exit packages-size 1 0", "packages-size");
        let error = failed.as_ref().ok().and_then(|s| packages_size(s).err());
        assert_eq!(error.as_ref().map(|e| e.code), Some(ErrorCode::Unknown));
        assert_eq!(
            error.and_then(|e| e.detail),
            Some("step packages-size exited with code 1".to_owned())
        );
        // No output: the step did not run.
        assert_eq!(
            scan_image::step_result(Some(""), "packages-size")
                .err()
                .map(|e| e.code),
            Some(ErrorCode::Unknown)
        );
        assert_eq!(
            scan_image::step_result(None, "packages-size")
                .err()
                .map(|e| e.code),
            Some(ErrorCode::Unknown)
        );
    }

    #[test]
    fn a_missing_or_partial_package_database_is_an_error() {
        let copied = |output: &str| {
            step(output, "sbom")
                .ok()
                .and_then(|s| assert_copied(&s).err())
                .map(|e| (e.code, e.detail.unwrap_or_default()))
        };
        assert_eq!(
            copied("coolify-scan-exit copy 0\ncoolify-scan-exit sbom 0"),
            None
        );
        assert_eq!(
            copied("coolify-scan-exit copy 1\ncoolify-scan-exit sbom 0"),
            Some((
                ErrorCode::NoPackageDatabase,
                "step copy exited with code 1".to_owned()
            ))
        );
        assert_eq!(
            copied("coolify-scan-exit copy 2\ncoolify-scan-exit sbom 0"),
            Some((
                ErrorCode::Unknown,
                "step copy exited with code 2".to_owned()
            ))
        );
        // Without the marker the copy counts as failed.
        assert_eq!(
            copied("coolify-scan-exit sbom 0"),
            Some((
                ErrorCode::Unknown,
                "step copy exited with code 2".to_owned()
            ))
        );
    }

    #[test]
    fn a_busy_host_scan_is_retried_and_records_lock_busy_after_the_last_try() {
        assert_eq!(after_scan(true, 1), (Attempt::Done, false));
        assert_eq!(after_scan(true, TRIES), (Attempt::Done, false));
        assert_eq!(after_scan(false, 1), (Attempt::Release(300), false));
        assert_eq!(after_scan(false, TRIES - 1), (Attempt::Release(300), false));
        assert_eq!(after_scan(false, TRIES), (Attempt::Done, true));
        assert_eq!(TRIES, 6);
    }

    #[test]
    fn cache_keys() {
        assert_eq!(running_cache_key(7), "server-host-scan-running:7");
        assert_eq!(
            job_timeout().as_secs(),
            scan_image::MAX_LOCK_TTL.unsigned_abs()
        );
    }
}
