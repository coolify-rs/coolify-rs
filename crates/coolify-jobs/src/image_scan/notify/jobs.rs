//! `App\Jobs\ImageScanAlertCheckJob` and `App\Jobs\ImageScanDigestJob`.
//!
//! The digest reads the security status of each resource page from the view models
//! (`TeamVulnerabilities::load()`, `SecuritySentences::badge()`, in [`super::super::views`])
//! through the two private functions [`team_entries`] and [`status_badge`]. Everything else of
//! the digest is here: the plans of each scan owner, which resources are reported, holding while
//! a check is needed, ignores that end soon, failing servers, the marks recorded after sending.

use std::time::Duration;

use chrono::{Duration as Days, NaiveDateTime};
use coolify_db::image_scans::notifications as states;
use coolify_db::image_scans::{self, ImageScanIgnoreRow, ImageScanRow, ScanOwner};
use serde_json::Value;

use super::super::cache;
use super::super::scan_jobs;
use super::digest::{DigestIgnore, DigestResource, DigestServer, VulnerabilityDigest};
use super::state::{DigestMark, DigestPlan};
use super::text::WhyEntry;
use super::{
    ImageScanNotification, after_scan, apply_digest, base_url, digest_plan, link, page_resource,
    send,
};
use crate::Jobs;

/// `ImageScanDigestJob::IGNORE_WINDOW_DAYS`.
pub const IGNORE_WINDOW_DAYS: i64 = 7;

/// `ImageScanDigestJob::MAX_HELD_DIGESTS`: while a resource needs a check, its new problems
/// without an exploit signal wait at most this many digests.
pub const MAX_HELD_DIGESTS: i64 = 3;

/// `TeamVulnerabilities::STATUS_ORDER`.
pub const STATUS_ORDER: [&str; 6] = [
    "update_now",
    "update_available",
    "no_fix",
    "check_needed",
    "ok",
    "not_checked",
];

/// `SecurityStatus::REASON_VERSIONS_UNCHECKED`.
pub const REASON_VERSIONS_UNCHECKED: &str = "versions_unchecked";

/// `ImageScanAlertCheckJob::$uniqueFor`.
const ALERT_CHECK_UNIQUE: Duration = Duration::from_secs(7200);
/// `ImageScanAlertCheckJob::backoff()`: the waits after a failed first and second run.
const ALERT_CHECK_BACKOFF: [u64; 2] = [60, 300];
/// `ImageScanDigestJob::$uniqueFor` and `$timeout`.
const DIGEST_SECONDS: u64 = 3600;

// ---------------------------------------------------------------------------------------------
// ImageScanAlertCheckJob
// ---------------------------------------------------------------------------------------------

/// `ImageScanAlertCheckJob::uniqueId()`.
fn alert_check_key(scan_id: i64, scanned_at: Option<&str>) -> String {
    format!(
        "image-scan-alert-check:{scan_id}:{}",
        scanned_at.unwrap_or_default()
    )
}

/// `ImageScanAlertCheckJob::handle()`: looks again at the alert of a scan whose exploited
/// problems waited for the version check (spec 15.8). Errors fail the run, so it is tried again.
pub async fn image_scan_alert_check(jobs: &Jobs, scan_id: i64) -> Result<(), String> {
    let scan = image_scans::find_scan(&jobs.0.db, scan_id)
        .await
        .map_err(|e| e.to_string())?;
    match scan {
        Some(scan) => after_scan(jobs, &scan, None, true, false).await,
        None => Ok(()),
    }
}

impl Jobs {
    /// `ImageScanAlertCheckJob::dispatch($scanId, $scannedAt)->delay(...)` (`HOLD_MINUTES + 1`
    /// minutes): runs when the wait for the version check ends, so the alert is sent even when the
    /// check never stores a result. Unique per scan and scan time, so a rescan of the same row
    /// during a wait gets its own run. Three tries.
    pub fn dispatch_image_scan_alert_check(&self, scan_id: i64, scanned_at: Option<String>) {
        let key = alert_check_key(scan_id, scanned_at.as_deref());
        if !cache::add(&key, Value::Bool(true), ALERT_CHECK_UNIQUE) {
            return;
        }
        let jobs = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(
                ((super::HOLD_MINUTES + 1) * 60).unsigned_abs(),
            ))
            .await;
            for attempt in 0..=ALERT_CHECK_BACKOFF.len() {
                match image_scan_alert_check(&jobs, scan_id).await {
                    Ok(()) => break,
                    Err(e) => {
                        tracing::warn!(scan_id, attempt = attempt + 1, error = %e, "ImageScanAlertCheckJob failed");
                        match ALERT_CHECK_BACKOFF.get(attempt) {
                            Some(delay) => tokio::time::sleep(Duration::from_secs(*delay)).await,
                            None => break,
                        }
                    }
                }
            }
            cache::forget(&key);
        });
    }
}

// ---------------------------------------------------------------------------------------------
// ImageScanDigestJob
// ---------------------------------------------------------------------------------------------

/// What the digest reads of `ResourceSecurity`, the security status of one resource page.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DigestSecurity {
    /// `SecurityStatusType` value.
    pub status: String,
    /// `$security->checkReason`.
    pub check_reason: Option<String>,
    /// `$security->primary()?->sentence`.
    pub primary_sentence: Option<String>,
    /// `TeamVulnerabilities::action($security)['text']`.
    pub action_text: Option<String>,
    /// `$security->why`.
    pub why: Vec<WhyEntry>,
}

/// One entry of `TeamVulnerabilities::load($teamId)->entries`.
#[derive(Debug, Clone)]
pub struct TeamEntry {
    /// The resource page (application, service, standalone database).
    pub resource: ScanOwner,
    /// `$entry['scanned']`.
    pub scanned: bool,
    /// The current scans of its owners.
    pub scans: Vec<ImageScanRow>,
    pub security: DigestSecurity,
}

/// `TeamVulnerabilities::load($teamId)->entries`.
async fn team_entries(jobs: &Jobs, team_id: i64) -> Result<Vec<TeamEntry>, String> {
    use super::super::views::team;

    let now = Jobs::now();
    Ok(team::TeamVulnerabilities::load(jobs, team_id)
        .await?
        .entries
        .into_iter()
        .map(|entry| {
            let security = &entry.security;
            let digest_security = DigestSecurity {
                status: security.status.value().to_owned(),
                check_reason: security.check_reason.clone(),
                primary_sentence: security.primary().map(|a| a.sentence.clone()),
                action_text: team::action(security, now).map(|a| a.text),
                why: security.why.iter().map(|w| w.to_notify()).collect(),
            };
            TeamEntry {
                resource: entry.resource,
                scanned: entry.scanned,
                scans: entry.scans,
                security: digest_security,
            }
        })
        .collect())
}

/// `SecuritySentences::badge($status)`.
fn status_badge(status: &str) -> String {
    use super::super::views::{SecurityStatusType, sentences};

    SecurityStatusType::from_value(status)
        .and_then(sentences::badge)
        .unwrap_or_default()
        .to_owned()
}

/// The decision of `buildResourceDigest()` over the plans of the owners of one resource page:
/// the marks to record, and whether the resource is reported (`report`), with the action and
/// the why-list it is reported with.
#[derive(Debug, Clone, PartialEq)]
pub struct ResourceDecision {
    pub marks: Vec<DigestMark>,
    pub report: bool,
    pub action: Option<String>,
    /// `false`: the why-list is left out (a held check_needed).
    pub with_why: bool,
}

/// `ImageScanDigestJob::buildResourceDigest()` without the reads: `None` when no owner has a
/// plan, or when the status is check_needed only because a version check did not finish yet.
pub fn decide_resource(
    plans: &[DigestPlan],
    security: &DigestSecurity,
) -> Option<ResourceDecision> {
    if plans.is_empty() {
        return None;
    }
    let needs_check = security.status == "check_needed";
    if needs_check
        && security.check_reason.as_deref() == Some(REASON_VERSIONS_UNCHECKED)
        && plans.iter().any(|plan| plan.pending)
    {
        return None;
    }
    let active: Vec<&DigestPlan> = plans.iter().filter(|plan| !plan.baseline).collect();
    let holds = |plan: &DigestPlan| {
        plan.new_fixable.len() > plan.new_signal_fixable.len()
            || plan.new_candidates.len() > plan.new_signal_candidates.len()
    };
    // Fixes of problems with an exploit signal are never held; other new problems are held at
    // most MAX_HELD_DIGESTS digests.
    let bypass = needs_check
        && active.iter().any(|plan| {
            !plan.new_signal_fixable.is_empty() || !plan.new_signal_candidates.is_empty()
        });
    let release = needs_check
        && active
            .iter()
            .any(|plan| plan.held_digests >= MAX_HELD_DIGESTS)
        && active.iter().any(|plan| holds(plan));
    let report = match security.status.as_str() {
        "update_now" | "update_available" => active
            .iter()
            .any(|plan| !plan.new_fixable.is_empty() || !plan.new_candidates.is_empty()),
        "check_needed" => {
            !active.is_empty()
                && (bypass || release || !active.iter().any(|plan| plan.check_reported))
        }
        _ => false,
    };

    let mut marks = Vec::with_capacity(plans.len());
    for plan in plans {
        let mut mark = plan.mark.clone();
        mark.check_reported = Some(needs_check);
        mark.held_digests = Some(0);
        if needs_check && !plan.baseline && !release {
            let mut ids: Vec<String> = Vec::new();
            for finding in &plan.new_signal_fixable {
                if !ids.contains(&finding.id) {
                    ids.push(finding.id.clone());
                }
            }
            mark.add_digest_ids = ids;
            mark.add_candidates = plan.new_signal_candidates.clone();
            mark.held_digests = Some(if holds(plan) {
                plan.held_digests + 1
            } else {
                0
            });
        }
        marks.push(mark);
    }

    let mut action = security.action_text.clone();
    if needs_check
        && (bypass || release)
        && let Some(sentence) = security
            .primary_sentence
            .as_ref()
            .filter(|s| !s.trim().is_empty())
    {
        action = Some(sentence.clone());
    }
    Some(ResourceDecision {
        marks,
        report,
        action,
        with_why: !(needs_check && !bypass && !release),
    })
}

/// The order of the digest: by `STATUS_ORDER` (unknown statuses last), then by name.
pub fn sort_resources(resources: &mut [DigestResource]) {
    let rank = |status: &str| STATUS_ORDER.iter().position(|s| *s == status).unwrap_or(99);
    resources.sort_by(|a, b| {
        rank(&a.status)
            .cmp(&rank(&b.status))
            .then_with(|| a.name.cmp(&b.name))
    });
}

/// `ImageScanDigestJob::ignoreCacheKey($ignore)`.
pub fn ignore_cache_key(ignore_id: i64) -> String {
    format!("image-scan-digest:ignore:{ignore_id}")
}

/// `$ignore->expires_at->toIso8601String()` of a stored UTC time.
fn iso8601(at: NaiveDateTime) -> String {
    at.format("%Y-%m-%dT%H:%M:%S+00:00").to_string()
}

/// `endingIgnores()`: the ignores of the team that end within [`IGNORE_WINDOW_DAYS`] and that no
/// digest named with this end date.
async fn ending_ignores(
    jobs: &Jobs,
    team_id: i64,
    now: NaiveDateTime,
) -> Result<Vec<ImageScanIgnoreRow>, String> {
    let ignores = states::ending_ignores(
        &jobs.0.db,
        team_id,
        now,
        now + Days::days(IGNORE_WINDOW_DAYS),
    )
    .await
    .map_err(|e| e.to_string())?;
    Ok(ignores
        .into_iter()
        .filter(|ignore| {
            let named = cache::get(&ignore_cache_key(ignore.id))
                .and_then(|v| v.as_str().map(str::to_owned));
            named != ignore.expires_at.map(iso8601)
        })
        .collect())
}

/// `failingServers()`: servers of the team that still scan images and whose daily rescan failed
/// 2 days in a row, not named yet for this run of failures.
async fn failing_servers(jobs: &Jobs, team_id: i64) -> Result<Vec<states::TeamScanServer>, String> {
    let mut failing = Vec::new();
    for server in states::scanning_servers_of_team(&jobs.0.db, team_id)
        .await
        .map_err(|e| e.to_string())?
    {
        let transferred = super::super::ScanServer::load(jobs, server.id)
            .await
            .is_ok_and(|s| s.is_transferred_away());
        if !transferred
            && scan_jobs::failed_days(server.id) >= 2
            && !scan_jobs::is_failure_notified(server.id)
        {
            failing.push(server);
        }
    }
    Ok(failing)
}

/// `resourceDigest()`: the digest line of one resource page (a service covers its applications
/// and databases). The marks are added only when its digest succeeded.
async fn resource_digest(
    jobs: &Jobs,
    entry: &TeamEntry,
    marks: &mut Vec<DigestMark>,
) -> Result<Option<DigestResource>, String> {
    let mut owners: Vec<ScanOwner> = Vec::new();
    for scan in &entry.scans {
        if owners
            .iter()
            .any(|o| o.morph == scan.resource_type && o.id == scan.resource_id)
        {
            continue;
        }
        if let Some(owner) =
            image_scans::find_owner(&jobs.0.db, &scan.resource_type, scan.resource_id)
                .await
                .map_err(|e| e.to_string())?
        {
            owners.push(owner);
        }
    }
    let mut plans = Vec::new();
    for owner in &owners {
        if let Some(plan) = digest_plan(jobs, owner).await? {
            plans.push(plan);
        }
    }
    let Some(decision) = decide_resource(&plans, &entry.security) else {
        return Ok(None);
    };
    let url = link(jobs, &entry.resource).await?;
    marks.extend(decision.marks);
    if !decision.report {
        return Ok(None);
    }
    Ok(Some(DigestResource {
        name: entry.resource.name(),
        uuid: entry.resource.uuid.clone(),
        url,
        status: entry.security.status.clone(),
        badge: status_badge(&entry.security.status),
        action: decision.action,
        why: if decision.with_why {
            entry.security.why.clone()
        } else {
            Vec::new()
        },
    }))
}

/// `ImageScanDigestJob::handle()`: the daily security digest of one team (spec 15.8). Sends one
/// `VulnerabilityDigest` only when something changed, then records what it covered in the
/// notification state of each scan owner. An ignore is named once per end date (cache); a
/// failing server once per run of failures.
pub async fn image_scan_digest(jobs: &Jobs, team_id: i64) -> Result<(), String> {
    let db = &jobs.0.db;
    let mut marks: Vec<DigestMark> = Vec::new();
    let mut resources: Vec<DigestResource> = Vec::new();
    for entry in team_entries(jobs, team_id).await? {
        if !entry.scanned {
            continue;
        }
        let mut resource_marks = Vec::new();
        match resource_digest(jobs, &entry, &mut resource_marks).await {
            Ok(resource) => {
                marks.extend(resource_marks);
                resources.extend(resource);
            }
            Err(e) => {
                tracing::error!(team_id, resource = %entry.resource.uuid, error = %e, "image scan digest of a resource failed");
            }
        }
    }

    let now = Jobs::now();
    let ignores = ending_ignores(jobs, team_id, now).await?;
    let servers = failing_servers(jobs, team_id).await?;
    if !resources.is_empty() || !ignores.is_empty() || !servers.is_empty() {
        sort_resources(&mut resources);
        let base = base_url(jobs).await;
        let mut digest_ignores = Vec::with_capacity(ignores.len());
        for ignore in &ignores {
            let resource = match (&ignore.resource_type, ignore.resource_id) {
                (Some(morph), Some(id)) => {
                    match image_scans::find_owner(db, morph, id)
                        .await
                        .map_err(|e| e.to_string())?
                    {
                        Some(owner) => Some(page_resource(jobs, &owner).await?.name()),
                        None => None,
                    }
                }
                _ => None,
            };
            digest_ignores.push(DigestIgnore {
                resource,
                id: ignore.vulnerability_id.clone(),
                package: ignore.package.clone(),
                expires_at: ignore
                    .expires_at
                    .map(|at| at.format("%Y-%m-%d").to_string())
                    .unwrap_or_default(),
            });
        }
        let digest_servers = servers
            .iter()
            .map(|server| DigestServer {
                name: server.name.clone(),
                url: format!("{}/server/{}", base.trim_end_matches('/'), server.uuid),
            })
            .collect();
        let digest = VulnerabilityDigest::new(resources, digest_ignores, digest_servers, &base);
        send(jobs, team_id, ImageScanNotification::Digest(digest)).await?;
        for ignore in &ignores {
            if let Some(expires_at) = ignore.expires_at {
                let ttl = (expires_at + Days::days(1) - now).num_seconds().max(1);
                cache::put(
                    &ignore_cache_key(ignore.id),
                    Value::String(iso8601(expires_at)),
                    Duration::from_secs(ttl.unsigned_abs()),
                );
            }
        }
        for server in &servers {
            scan_jobs::mark_failure_notified(server.id);
        }
    }

    for mark in &marks {
        if let Err(e) = apply_digest(jobs, mark).await {
            tracing::error!(team_id, owner = %mark.owner_id, error = %e, "ScanNotifications::applyDigest failed");
        }
    }
    Ok(())
}

impl Jobs {
    /// `ImageScanDigestJob::dispatch($team)`: one digest per team at a time, one try.
    pub fn dispatch_image_scan_digest(&self, team_id: i64) {
        let key = format!("image-scan-digest-job:{team_id}");
        if !cache::add(&key, Value::Bool(true), Duration::from_secs(DIGEST_SECONDS)) {
            return;
        }
        let jobs = self.clone();
        tokio::spawn(async move {
            let run = tokio::time::timeout(
                Duration::from_secs(DIGEST_SECONDS),
                image_scan_digest(&jobs, team_id),
            )
            .await;
            match run {
                Ok(Ok(())) => {}
                Ok(Err(e)) => tracing::warn!(team_id, error = %e, "ImageScanDigestJob failed"),
                Err(_) => tracing::warn!(team_id, "ImageScanDigestJob timed out"),
            }
            cache::forget(&key);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image_scan::rules::tests::finding;

    fn mark(owner_id: i64) -> DigestMark {
        DigestMark {
            baseline: false,
            owner_type: image_scans::APPLICATION_MORPH.to_owned(),
            owner_id,
            team_id: 1,
            read_ids: Vec::new(),
            read_digest_ids: Vec::new(),
            read_candidates: Vec::new(),
            add_ids: Vec::new(),
            add_digest_ids: vec!["cve-from-plan".to_owned()],
            add_candidates: vec!["from-plan".to_owned()],
            present: Vec::new(),
            offered: Vec::new(),
            prune_ids: false,
            prune_candidates: false,
            check_reported: None,
            held_digests: None,
        }
    }

    fn plan() -> DigestPlan {
        DigestPlan {
            baseline: false,
            new_fixable: Vec::new(),
            new_signal_fixable: Vec::new(),
            new_candidates: Vec::new(),
            new_signal_candidates: Vec::new(),
            pending: false,
            check_reported: false,
            held_digests: 0,
            mark: mark(7),
        }
    }

    fn security(status: &str) -> DigestSecurity {
        DigestSecurity {
            status: status.to_owned(),
            action_text: Some("Update to nginx:1.28.".to_owned()),
            primary_sentence: Some("Update to nginx:1.28.".to_owned()),
            ..DigestSecurity::default()
        }
    }

    #[test]
    fn nothing_without_plans() {
        assert_eq!(decide_resource(&[], &security("update_available")), None);
    }

    #[test]
    fn update_available_is_reported_for_new_fixes_only() {
        let quiet = decide_resource(&[plan()], &security("update_available")).unwrap();
        assert!(!quiet.report);
        // The plan's own mark is recorded, with the check flag off.
        assert_eq!(quiet.marks[0].check_reported, Some(false));
        assert_eq!(quiet.marks[0].held_digests, Some(0));
        assert_eq!(quiet.marks[0].add_digest_ids, vec!["cve-from-plan"]);

        let mut new = plan();
        new.new_candidates = vec!["nginx:1.28@sha256:b".to_owned()];
        let reported = decide_resource(&[new], &security("update_now")).unwrap();
        assert!(reported.report);
        assert!(reported.with_why);
        assert_eq!(reported.action.as_deref(), Some("Update to nginx:1.28."));

        // A baseline owner never reports.
        let mut baseline = plan();
        baseline.baseline = true;
        baseline.new_fixable = vec![finding("CVE-1", "high")];
        assert!(
            !decide_resource(&[baseline], &security("update_available"))
                .unwrap()
                .report
        );
        assert!(!decide_resource(&[plan()], &security("ok")).unwrap().report);
    }

    #[test]
    fn a_pending_version_check_skips_the_resource() {
        let mut pending = plan();
        pending.pending = true;
        let mut check = security("check_needed");
        check.check_reason = Some(REASON_VERSIONS_UNCHECKED.to_owned());
        assert_eq!(decide_resource(&[pending.clone()], &check), None);
        check.check_reason = Some("scan_failed".to_owned());
        assert!(decide_resource(&[pending], &check).is_some());
    }

    #[test]
    fn check_needed_is_reported_once_and_holds_new_problems() {
        let mut check = security("check_needed");
        check.action_text = Some("Check again: the scan failed.".to_owned());
        let mut new = plan();
        new.new_fixable = vec![finding("CVE-1", "high")];
        let first = decide_resource(&[new.clone()], &check).unwrap();
        assert!(first.report);
        // Held: no why-list, the check action, nothing recorded as digested, one held digest.
        assert!(!first.with_why);
        assert_eq!(
            first.action.as_deref(),
            Some("Check again: the scan failed.")
        );
        assert_eq!(first.marks[0].check_reported, Some(true));
        assert!(first.marks[0].add_digest_ids.is_empty());
        assert!(first.marks[0].add_candidates.is_empty());
        assert_eq!(first.marks[0].held_digests, Some(1));

        // Reported already: quiet until the hold ends.
        new.check_reported = true;
        new.held_digests = 2;
        let quiet = decide_resource(&[new.clone()], &check).unwrap();
        assert!(!quiet.report);
        assert_eq!(quiet.marks[0].held_digests, Some(3));

        // After MAX_HELD_DIGESTS the problems are released with the primary action.
        new.held_digests = MAX_HELD_DIGESTS;
        let released = decide_resource(&[new], &check).unwrap();
        assert!(released.report);
        assert!(released.with_why);
        assert_eq!(released.action.as_deref(), Some("Update to nginx:1.28."));
        // A release keeps the plan's mark.
        assert_eq!(released.marks[0].add_digest_ids, vec!["cve-from-plan"]);
        assert_eq!(released.marks[0].held_digests, Some(0));
    }

    #[test]
    fn fixes_of_exploited_problems_are_never_held() {
        let check = security("check_needed");
        let mut signal = plan();
        signal.check_reported = true;
        signal.new_fixable = vec![finding("CVE-9", "critical"), finding("CVE-9", "critical")];
        signal.new_signal_fixable = signal.new_fixable.clone();
        let decision = decide_resource(&[signal], &check).unwrap();
        assert!(decision.report);
        assert!(decision.with_why);
        assert_eq!(decision.action.as_deref(), Some("Update to nginx:1.28."));
        assert_eq!(decision.marks[0].add_digest_ids, vec!["CVE-9"]);
        // Nothing else is held, so the hold count resets.
        assert_eq!(decision.marks[0].held_digests, Some(0));
    }

    #[test]
    fn resources_sort_by_status_then_name() {
        let resource = |name: &str, status: &str| DigestResource {
            name: name.to_owned(),
            uuid: name.to_owned(),
            url: None,
            status: status.to_owned(),
            badge: String::new(),
            action: None,
            why: Vec::new(),
        };
        let mut list = vec![
            resource("b", "check_needed"),
            resource("z", "update_now"),
            resource("a", "check_needed"),
            resource("c", "weird"),
            resource("y", "update_available"),
        ];
        sort_resources(&mut list);
        let names: Vec<&str> = list.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["z", "y", "a", "b", "c"]);
    }

    #[test]
    fn keys() {
        assert_eq!(ignore_cache_key(5), "image-scan-digest:ignore:5");
        assert_eq!(
            alert_check_key(5, Some("2026-11-10T11:00:00+00:00")),
            "image-scan-alert-check:5:2026-11-10T11:00:00+00:00"
        );
        assert_eq!(alert_check_key(5, None), "image-scan-alert-check:5:");
    }
}
