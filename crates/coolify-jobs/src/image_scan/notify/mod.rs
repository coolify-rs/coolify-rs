//! `App\Services\ImageScan\ScanNotifications`: what the image security notifications told a team
//! (spec 15.8), kept in one `image_scan_notification_states` row per scan owner (application,
//! service application or database, standalone database).
//!
//! - The current scans of an owner are `resource_scans::current_for()`: completed, never previews.
//! - `notified_ids`: relevant findings with an exploit signal that an immediate alert named. A
//!   finding is new when none of its ID and aliases is in it, so an EPSS that moves around the
//!   threshold does not alert again.
//! - `digest_ids`: relevant findings with an action that a digest named, or an alert named with
//!   its action.
//! - `notified_candidates`: better images (and base images) that a digest covered.
//! - `check_reported`: a digest said that the resource needs a check.
//! - Without a state row the owner is a baseline: the row is created from what the current scans
//!   have, and nothing is sent. The baseline also records, without notifying, the other images of
//!   the same deployment (Compose) and the version checks that follow. It ends with the next scan
//!   that is not part of that deployment, or with the next digest.
//! - A problem with an exploit signal whose fix depends on the version or base check (no action
//!   yet, or an image update) waits for an outstanding check at most [`HOLD_MINUTES`] after the
//!   scan; `ImageScanAlertCheckJob` looks again when the wait ends. Without a check result the
//!   alert says that Coolify could not check for a fixed version.
//! - A rescan (not a deploy) of the deployment that started the baseline extends it only
//!   [`BASELINE_RESCAN_HOURS`] long.
//! - `held_digests`: digests that held new problems back while the resource needed a check (see
//!   `ImageScanDigestJob`).
//! - The digest removes IDs and candidates that no current scan has any more, so a regression
//!   notifies again. It skips that while a deployment of the application runs (its current scans
//!   can be incomplete), and it keeps the candidates while a version check failed.
//! - Every write locks the state row, so the alert and the digest never overwrite each other.
//! - Preview scans never notify and never change the state.
//!
//! | Module | Ports |
//! |---|---|
//! | this file | `ScanNotifications` (what reads the database), the calls into the view models |
//! | [`state`] | the decisions of `ScanNotifications` over plain data |
//! | [`jobs`] | `ImageScanAlertCheckJob`, `ImageScanDigestJob` |
//! | [`exploited`] | `Notifications\ImageScan\ExploitedProblemFound` |
//! | [`digest`] | `Notifications\ImageScan\VulnerabilityDigest` |
//! | [`text`] | `Notifications\ImageScan\NotificationText` |
//! | [`mail`] | `resources/views/emails/image-vulnerabilities.blade.php` |
//!
//! The view models (`SecurityStatus`, `SecurityActions`, `SecuritySentences`,
//! `TeamVulnerabilities`, in [`super::views`]) come in through one private function each, here
//! or in [`jobs`]: `actions_by_finding`, `versions_checked`, `vulnerabilities_url`, `top_why`,
//! `jobs::team_entries`, `jobs::status_badge`.
//!
//! Both notifications are built as plain data ([`ExploitedProblemFound`],
//! [`VulnerabilityDigest`]) with the payload of every channel; the private `send` hands them to
//! the notification channels as `Notification::ImageExploitedProblem` and
//! `Notification::ImageVulnerabilityDigest`.

pub mod digest;
pub mod exploited;
pub mod jobs;
pub mod mail;
pub mod state;
pub mod text;

use std::collections::HashMap;

use chrono::{DateTime, Duration, NaiveDateTime, SubsecRound};
use coolify_db::image_scans::notifications as states;
use coolify_db::image_scans::{self, ImageScanRow, ScanOwner};
use serde_json::Value;

use super::rules::{self, RelevanceRules};
use super::views::{actions, status, team};
use super::{candidates, resource_scans, update};
use crate::Jobs;
use crate::notifications::{Ctx, Notification};

pub use digest::{DigestIgnore, DigestResource, DigestServer, VulnerabilityDigest};
pub use exploited::ExploitedProblemFound;
pub use jobs::{image_scan_alert_check, image_scan_digest};
pub use state::{
    BASELINE_RESCAN_HOURS, CheckState, Current, DigestMark, DigestPlan, FixAction, HOLD_MINUTES,
    IdSet, MAX_NOTIFIED_CANDIDATES, PENDING_HOURS, SignalEntry,
};
pub use text::WhyEntry;

/// The notification event of both notifications
/// (`$notifiable->getEnabledChannels('image_vulnerabilities')`).
pub use states::{EVENT, TELEGRAM_THREAD_COLUMN};

fn err(e: coolify_db::DbError) -> String {
    e.to_string()
}

// ---------------------------------------------------------------------------------------------
// What the view models give
// ---------------------------------------------------------------------------------------------

/// `SecurityActions::forScan($scan, $rules)['byFinding']`: the action that fixes each relevant
/// finding of the scan, by finding ID (findings without an action are left out).
async fn actions_by_finding(
    jobs: &Jobs,
    scan: &ImageScanRow,
    owner: &ScanOwner,
    rules: &RelevanceRules,
) -> Result<HashMap<String, FixAction>, String> {
    let result = actions::for_scan(jobs, scan, owner, rules).await?;
    Ok(result
        .by_finding
        .iter()
        .filter_map(|(id, index)| {
            result.actions.get(*index).map(|action| {
                (
                    id.clone(),
                    FixAction::new(action.action_type.value(), &action.sentence),
                )
            })
        })
        .collect())
}

/// `SecurityStatus::versionsChecked($scan, $owner, blockedCounts: false)`: the version check
/// (images the user does not build) and the base check (Dockerfile apps with a base image) have
/// a result; checks that only the user can unblock do not count.
async fn versions_checked(
    _jobs: &Jobs,
    scan: &ImageScanRow,
    owner: &ScanOwner,
) -> Result<bool, String> {
    Ok(status::versions_checked(scan, owner, false))
}

/// `TeamVulnerabilities::vulnerabilitiesUrl($resource)`: the Security page of a resource with a
/// page (application, service, standalone database); `None` without an environment or project.
async fn vulnerabilities_url(jobs: &Jobs, resource: &ScanOwner) -> Result<Option<String>, String> {
    team::vulnerabilities_url(jobs, resource).await
}

/// `SecurityStatus::topWhy(array_map(SecurityStatus::whyFor(...), $problems), $limit)`: the
/// why-list of the alert email: exploited first, then likely, then the highest score.
fn top_why(problems: &[SignalEntry], limit: usize) -> Vec<WhyEntry> {
    let why = problems
        .iter()
        .map(|p| status::why_for(&p.finding, rules::exploit_signal(&p.finding)))
        .collect();
    status::top_why(why, limit)
        .iter()
        .map(|w| w.to_notify())
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Sending
// ---------------------------------------------------------------------------------------------

/// One of the two image security notifications, with what it sends on each channel.
#[derive(Debug, Clone, PartialEq)]
pub enum ImageScanNotification {
    Exploited(ExploitedProblemFound),
    Digest(VulnerabilityDigest),
}

/// `$team->notify($notification)` for a team that exists. Both notifications go to the channels
/// of `getEnabledChannels('image_vulnerabilities')` ([`EVENT`]); Telegram uses the thread of
/// [`TELEGRAM_THREAD_COLUMN`].
async fn send(
    jobs: &Jobs,
    team_id: i64,
    notification: ImageScanNotification,
) -> Result<(), String> {
    if !states::team_exists(&jobs.0.db, team_id)
        .await
        .map_err(err)?
    {
        return Ok(());
    }
    let notification = match notification {
        ImageScanNotification::Exploited(n) => Notification::ImageExploitedProblem(n),
        ImageScanNotification::Digest(n) => Notification::ImageVulnerabilityDigest(n),
    };
    crate::notifications::notify(&jobs.0.db, jobs.encrypter(), team_id, notification).await;
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// ScanNotifications
// ---------------------------------------------------------------------------------------------

/// `ScanNotifications::afterScan()`: after a scan of the owner completed, outside the scan lock:
/// creates or extends the baseline, or alerts the team right away for new relevant findings with
/// an exploit signal.
///
/// - `deployment_id`: the deployment that ran this scan (deploy path); `None` for rescans, Scan
///   now, and scans after a start, which use the deployment the row is linked to.
/// - `candidate_check`: called after a version check of the scan (or when its wait ended)
///   instead of the scan itself.
/// - `check_gave_up`: the version check of the scan will not store a result, so its findings do
///   not wait.
pub async fn after_scan(
    jobs: &Jobs,
    scan: &ImageScanRow,
    deployment_id: Option<i64>,
    candidate_check: bool,
    check_gave_up: bool,
) -> Result<(), String> {
    if !scan.exists() || !scan.is_completed() {
        return Ok(());
    }
    let db = &jobs.0.db;
    let owner = image_scans::find_owner(db, &scan.resource_type, scan.resource_id)
        .await
        .map_err(err)?;
    let explicit = deployment_id.is_some();
    let deployment_id = deployment_id.or(scan.deployment_queue_id);
    let Some(owner) = owner else {
        return Ok(());
    };
    let Some(team_id) = owner.team_id else {
        return Ok(());
    };
    if is_preview(jobs, deployment_id).await? {
        return Ok(());
    }
    let released = check_gave_up.then_some(scan.id);

    // The current scans are read under the lock, so a scan that lands meanwhile is not recorded
    // without notice.
    let mut locked = states::lock_state(db, owner.morph, owner.id)
        .await
        .map_err(err)?;
    let current = current(jobs, &owner, None, released).await?;
    if current.scans.is_empty() {
        return Ok(());
    }
    let now = Jobs::now();
    let (alert, held) = match locked.row().cloned() {
        None => {
            let new =
                state::baseline_state(owner.morph, owner.id, team_id, &current, deployment_id);
            locked.create_or_ignore(&new, now).await.map_err(err)?;
            (Vec::new(), Vec::new())
        }
        Some(row)
            if row.is_baseline
                && (candidate_check
                    || state::extends_baseline(&row, deployment_id, explicit, now)) =>
        {
            // A version check adds only what the checked scan has; a redeploy meanwhile is not
            // part of the baseline.
            let recorded = if candidate_check {
                self::current(jobs, &owner, Some(vec![scan.clone()]), released).await?
            } else {
                current
            };
            locked
                .update(&state::extend_baseline(&row, &recorded), now)
                .await
                .map_err(err)?;
            (Vec::new(), Vec::new())
        }
        Some(row) => {
            let change = state::alert_change(&row, &current);
            locked.update(&change.update, now).await.map_err(err)?;
            (change.alert, change.held)
        }
    };
    locked.commit().await.map_err(err)?;

    if !alert.is_empty() {
        let page = page_resource(jobs, &owner).await?;
        let notification = ExploitedProblemFound::new(
            &page.name(),
            &page.uuid,
            link(jobs, &page).await?,
            &alert,
            top_why(&alert, digest::MAIL_ID_LIMIT),
            &base_url(jobs).await,
        );
        send(
            jobs,
            team_id,
            ImageScanNotification::Exploited(notification),
        )
        .await?;
    }

    // After the alert, so a failed dispatch cannot lose an alert whose IDs are recorded already.
    for (scan_id, scanned_at) in held {
        // The wait ends at the latest HOLD_MINUTES after the scan, also when the check never
        // stores a result.
        jobs.dispatch_image_scan_alert_check(scan_id, scanned_at);
    }
    Ok(())
}

/// `ScanNotifications::reevaluate()`: looks at the alert of a scan again: when its version check
/// stored a result, gave up, or its wait ended. A failure is reported, never passed on.
///
/// `check_gave_up`: the version check will not store a result (the job failed, or cannot run).
pub async fn reevaluate(
    jobs: &Jobs,
    scan: &ImageScanRow,
    check_gave_up: bool,
) -> Result<(), String> {
    if let Err(e) = after_scan(jobs, scan, None, true, check_gave_up).await {
        tracing::error!(scan_id = scan.id, error = %e, "ScanNotifications::reevaluate failed");
    }
    Ok(())
}

/// `ScanNotifications::digestPlan($owner)`: what a digest sees for one owner, and what it records
/// after sending (see [`apply_digest`]). `None` for an owner without a team or without current
/// scans.
pub async fn digest_plan(jobs: &Jobs, owner: &ScanOwner) -> Result<Option<DigestPlan>, String> {
    let db = &jobs.0.db;
    let current = current(jobs, owner, None, None).await?;
    let Some(team_id) = owner.team_id else {
        return Ok(None);
    };
    if current.scans.is_empty() {
        return Ok(None);
    }
    let row = states::find_state(db, owner.morph, owner.id)
        .await
        .map_err(err)?;
    // While an image of a Compose app has no result (a failed scan), its findings are not "gone":
    // keep them, so they are not reported again once its scan works.
    let prunable = !is_deploying(jobs, owner).await?
        && resource_scans::unchecked_compose_images(db, owner, &current.scans, None).await? == 0;
    Ok(Some(state::plan_digest(
        owner.morph,
        owner.id,
        team_id,
        row.as_ref(),
        &current,
        prunable,
    )))
}

/// `ScanNotifications::applyDigest($mark)`: records what a digest covered under the row lock, and
/// ends a baseline. IDs and candidates that the state had when the digest read it and that no
/// current scan has any more are removed; ones added meanwhile stay.
pub async fn apply_digest(jobs: &Jobs, mark: &DigestMark) -> Result<(), String> {
    let now = Jobs::now();
    let mut locked = states::lock_state(&jobs.0.db, &mark.owner_type, mark.owner_id)
        .await
        .map_err(err)?;
    match locked.row().cloned() {
        None => {
            locked
                .create_or_ignore(&state::first_digest_state(mark), now)
                .await
                .map_err(err)?;
        }
        Some(row) => {
            locked
                .update(&state::applied_digest(&row, mark), now)
                .await
                .map_err(err)?;
        }
    }
    locked.commit().await.map_err(err)
}

/// `(string) $value` of a JSON scalar.
fn value_string(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(true) => "1".to_owned(),
        _ => String::new(),
    }
}

/// `self::idSet((array) ($entry['fixed_ids'] ?? []))`.
fn fixed_ids(entry: Option<&Value>) -> IdSet {
    match entry.and_then(|e| e.get("fixed_ids")) {
        Some(Value::Array(ids)) => ids
            .iter()
            .map(|id| value_string(id).to_lowercase())
            .collect(),
        Some(Value::Object(ids)) => ids
            .values()
            .map(|id| value_string(id).to_lowercase())
            .collect(),
        None | Some(Value::Null) => IdSet::new(),
        Some(scalar) => IdSet::from([value_string(scalar).to_lowercase()]),
    }
}

/// `$candidates[$key] = $ids`: a key that is set again keeps its place.
fn set_candidate(candidates: &mut Vec<(String, IdSet)>, key: String, ids: IdSet) {
    match candidates.iter_mut().find(|(k, _)| *k == key) {
        Some(slot) => slot.1 = ids,
        None => candidates.push((key, ids)),
    }
}

/// The better images among the update candidates of a scan (`ref@digest`), each with the IDs it
/// fixes.
fn image_candidates(entries: &[Value]) -> Vec<(String, IdSet)> {
    let mut candidates = Vec::new();
    for entry in entries {
        let kind = entry.get("kind").and_then(Value::as_str);
        if !matches!(kind, Some("line" | "same_tag")) {
            continue;
        }
        let reference = entry.get("ref").map(value_string).unwrap_or_default();
        let digest = entry
            .get("digest")
            .filter(|d| !d.is_null())
            .map(value_string)
            .unwrap_or_default();
        set_candidate(
            &mut candidates,
            format!("{reference}@{digest}"),
            fixed_ids(Some(entry)),
        );
    }
    candidates
}

/// `ScanNotifications::betterCandidates($scan, $owner)`: the better images the resource can
/// switch to (`ref@digest`) and a better base image (`base:{ref}`), each with the IDs it fixes
/// (lower case), in the order they are offered.
pub async fn better_candidates(
    jobs: &Jobs,
    scan: &ImageScanRow,
    owner: &ScanOwner,
) -> Result<Vec<(String, IdSet)>, String> {
    if owner.id == 0 {
        return Ok(Vec::new());
    }
    let mut found = Vec::new();
    if update::blocked_reason(jobs, scan, owner).await?.is_none() {
        found = image_candidates(&update::update_candidates(scan));
    }
    if let Some(base) = candidates::base_image(jobs, scan).await? {
        let better = candidates::better_base(scan);
        set_candidate(
            &mut found,
            format!("base:{}", base.to),
            fixed_ids(better.as_ref().map(|b| &b.entry)),
        );
    }
    Ok(found)
}

/// `data_get($scan->candidates, 'checked_at')` when it is a string.
fn checked_at(scan: &ImageScanRow) -> Option<&str> {
    scan.candidates
        .as_ref()
        .and_then(|c| c.get("checked_at"))
        .and_then(Value::as_str)
}

/// The part of `versionCheckPending()` that reads only the row: it was created less than
/// [`PENDING_HOURS`] ago and its version check stored no result.
fn awaits_first_check(scan: &ImageScanRow, now: NaiveDateTime) -> bool {
    scan.created_at
        .or(scan.scanned_at)
        .is_some_and(|created| created > now - Duration::hours(PENDING_HOURS))
        && checked_at(scan).is_none()
}

/// `ScanNotifications::versionCheckPending($scan)`: for the digest: the version check is needed
/// and stored no result since the row was created, less than [`PENDING_HOURS`] ago, so a
/// check_needed (versions_unchecked) is not settled yet.
pub async fn version_check_pending(jobs: &Jobs, scan: &ImageScanRow) -> Result<bool, String> {
    Ok(awaits_first_check(scan, Jobs::now()) && candidates::needs_candidates(jobs, scan).await?)
}

/// `CarbonImmutable::parse($checkedAt)` of a stored time; `None` when it cannot be read.
fn parse_time(value: &str) -> Option<NaiveDateTime> {
    let value = value.trim();
    if let Ok(at) = DateTime::parse_from_rfc3339(value) {
        return Some(at.naive_utc());
    }
    ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%d %H:%M:%S%.f"]
        .iter()
        .find_map(|format| NaiveDateTime::parse_from_str(value, format).ok())
}

/// The version check of a scan stored a result at or after the scan.
fn checked_after_scan(scan: &ImageScanRow) -> bool {
    checked_at(scan).and_then(parse_time).is_some_and(|at| {
        scan.scanned_at
            .is_none_or(|scanned| at >= scanned.trunc_subsecs(0))
    })
}

/// `ScanNotifications::checkState($scan, $owner)`: for alerts: whether the version check of a
/// scan is outstanding (needed, no result since the scan, and queued again because the last
/// result is not fresh), and whether it is incomplete (outstanding, older than the scan, failed,
/// or without a checked result), so Coolify cannot say that no fixed version exists.
pub async fn check_state(
    jobs: &Jobs,
    scan: &ImageScanRow,
    owner: &ScanOwner,
) -> Result<CheckState, String> {
    if !candidates::needs_candidates(jobs, scan).await? {
        return Ok(CheckState::default());
    }
    let after_scan = checked_after_scan(scan);
    Ok(CheckState {
        outstanding: !after_scan && !candidates::is_fresh(jobs, scan).await?,
        incomplete: !after_scan || !versions_checked(jobs, scan, owner).await?,
    })
}

/// `rtrim(base_url(), '/')` would cut it; this is `base_url()` itself.
async fn base_url(jobs: &Jobs) -> String {
    Ctx::load(&jobs.0.db).await.base_url
}

/// `parse_url($url, PHP_URL_PATH)`; `None` when the URL has no path.
fn url_path(url: &str) -> Option<&str> {
    let rest = match url.find("://") {
        Some(scheme) => {
            let after = &url[scheme + 3..];
            &after[after.find(['/', '?', '#']).unwrap_or(after.len())..]
        }
        None => url,
    };
    let path = &rest[..rest.find(['?', '#']).unwrap_or(rest.len())];
    (!path.is_empty()).then_some(path)
}

/// The link of `link()` from the instance URL and the URL of a page.
fn instance_link(base_url: &str, page_url: &str) -> String {
    format!(
        "{}{}",
        base_url.trim_end_matches('/'),
        url_path(page_url).unwrap_or_default()
    )
}

/// `ScanNotifications::link($resource)`: a link built from the instance URL, because queued
/// notifications have no request and `route()` would use `APP_URL`.
pub async fn link(jobs: &Jobs, resource: &ScanOwner) -> Result<Option<String>, String> {
    let page = page_resource(jobs, resource).await?;
    Ok(match vulnerabilities_url(jobs, &page).await? {
        Some(url) => Some(instance_link(&base_url(jobs).await, &url)),
        None => None,
    })
}

/// `ScanNotifications::pageResource($owner)`: the resource with a page: the service of a service
/// application or database.
pub async fn page_resource(jobs: &Jobs, owner: &ScanOwner) -> Result<ScanOwner, String> {
    Ok(image_scans::owner_service(&jobs.0.db, owner)
        .await
        .map_err(err)?
        .unwrap_or_else(|| owner.clone()))
}

/// `ScanNotifications::current($owner, $only, $released)`: the current scans of the owner (or
/// `only`), decoded once.
///
/// `released`: a scan whose findings do not wait for its check any more.
async fn current(
    jobs: &Jobs,
    owner: &ScanOwner,
    only: Option<Vec<ImageScanRow>>,
    released: Option<i64>,
) -> Result<Current, String> {
    let db = &jobs.0.db;
    let now = Jobs::now();
    let scans = match only {
        Some(scans) => scans,
        None => resource_scans::current_for(db, owner).await?,
    };
    let rules = RelevanceRules::for_resource(db, owner, now).await?;
    let mut current = Current::new();
    for scan in &scans {
        let findings = resource_scans::findings(scan);
        let by_finding = actions_by_finding(jobs, scan, owner, &rules).await?;
        let pending = version_check_pending(jobs, scan).await?;
        let check = if state::has_signal(&findings, &rules) {
            check_state(jobs, scan, owner).await?
        } else {
            CheckState::default()
        };
        let offered = better_candidates(jobs, scan, owner).await?;
        // Only read before the first check, like the condition of `current()`.
        let needs_candidates =
            scan.candidate_status().is_none() && candidates::needs_candidates(jobs, scan).await?;
        let facts = state::ScanFacts {
            by_finding,
            pending,
            check,
            candidates: offered,
            needs_candidates,
        };
        state::add_scan(&mut current, scan, &findings, &facts, &rules, released, now);
    }
    current.scans = scans;
    Ok(current)
}

/// `ScanNotifications::isPreview($deploymentId)`: a scan of a pull request preview deployment.
async fn is_preview(jobs: &Jobs, deployment_id: Option<i64>) -> Result<bool, String> {
    let Some(id) = deployment_id else {
        return Ok(false);
    };
    Ok(image_scans::deployment_facts(&jobs.0.db, id)
        .await
        .map_err(err)?
        .is_some_and(|deployment| deployment.pull_request_id != 0))
}

/// `ScanNotifications::isDeploying($owner)`: a deployment of the application is queued or
/// running, so its current scans can miss the images it did not scan yet.
async fn is_deploying(jobs: &Jobs, owner: &ScanOwner) -> Result<bool, String> {
    if !owner.is_application() {
        return Ok(false);
    }
    states::application_is_deploying(&jobs.0.db, owner.id)
        .await
        .map_err(err)
}

#[cfg(test)]
mod tests {
    use chrono::NaiveDate;
    use coolify_db::image_scans::STATUS_COMPLETED;
    use serde_json::json;

    use super::super::resource_scans::tests::scan as scan_row;
    use super::*;

    fn at(hour: u32, minute: u32, second: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 11, 10)
            .unwrap()
            .and_hms_opt(hour, minute, second)
            .unwrap()
    }

    fn ids(values: &[&str]) -> IdSet {
        values.iter().map(|v| (*v).to_owned()).collect()
    }

    #[test]
    fn candidates_are_keyed_by_ref_and_digest() {
        // `notifyCandidates()`: one better tag of the same line.
        let entries = vec![
            json!({"ref": "nginx:1.28", "digest": "sha256:bbbb", "kind": "line", "better": true, "fixed_ids": ["CVE-2026-0001", "GHSA-AAAA"]}),
            json!({"ref": "nginx:1.27", "kind": "same_tag", "better": true}),
            json!({"ref": "nginx:2.0", "digest": "sha256:cccc", "kind": "major", "fixed_ids": ["CVE-2026-0002"]}),
            json!({"ref": "nginx:1.28", "digest": "sha256:bbbb", "kind": "line", "fixed_ids": ["CVE-2026-0003"]}),
        ];
        assert_eq!(
            image_candidates(&entries),
            vec![
                ("nginx:1.28@sha256:bbbb".to_owned(), ids(&["cve-2026-0003"])),
                ("nginx:1.27@".to_owned(), IdSet::new()),
            ]
        );
        assert_eq!(fixed_ids(None), IdSet::new());
        assert_eq!(
            fixed_ids(Some(&json!({"fixed_ids": ["CVE-1", "cve-1", 7]}))),
            ids(&["cve-1", "7"])
        );
    }

    #[test]
    fn a_new_row_waits_a_day_for_its_first_version_check() {
        let now = at(12, 0, 0);
        let mut scan = scan_row("nginx:1.27", STATUS_COMPLETED, None);
        // Neither time is known.
        assert!(!awaits_first_check(&scan, now));
        scan.scanned_at = Some(now - Duration::hours(23));
        assert!(awaits_first_check(&scan, now));
        // The creation time of the row counts before the scan time.
        scan.created_at = Some(now - Duration::hours(24));
        assert!(!awaits_first_check(&scan, now));
        scan.created_at = Some(now - Duration::minutes(5));
        assert!(awaits_first_check(&scan, now));
        // A stored result ends the wait, a failed one too.
        scan.candidates =
            Some(json!({"status": "failed", "checked_at": "2026-11-10T11:59:00+00:00"}));
        assert!(!awaits_first_check(&scan, now));
        scan.candidates = Some(json!({"status": "failed", "checked_at": null}));
        assert!(awaits_first_check(&scan, now));
        // Older rows hold a bare list of entries.
        scan.candidates = Some(json!([{"ref": "nginx:1.28"}]));
        assert!(awaits_first_check(&scan, now));
    }

    #[test]
    fn a_check_counts_when_it_ran_after_the_scan() {
        let mut scan = scan_row("nginx:1.27", STATUS_COMPLETED, None);
        scan.scanned_at = Some(at(11, 0, 0));
        assert!(!checked_after_scan(&scan));
        let mut with = |checked_at: Value| {
            scan.candidates = Some(json!({"status": "ok", "checked_at": checked_at}));
            checked_after_scan(&scan)
        };
        assert!(with(json!("2026-11-10T11:00:00+00:00")));
        assert!(with(json!("2026-11-10T13:00:00+02:00")));
        assert!(with(json!("2026-11-10T11:00:01Z")));
        assert!(with(json!("2026-11-10 11:00:00")));
        assert!(!with(json!("2026-11-10T10:59:59+00:00")));
        assert!(!with(json!("not a time")));
        assert!(!with(json!(1_762_772_400)));
        // Without a scan time any result counts.
        scan.scanned_at = None;
        scan.candidates = Some(json!({"checked_at": "2020-01-01T00:00:00+00:00"}));
        assert!(checked_after_scan(&scan));
    }

    #[test]
    fn both_notifications_use_the_image_vulnerabilities_event() {
        let exploited = Notification::ImageExploitedProblem(ExploitedProblemFound {
            resource_name: "api".to_owned(),
            resource_uuid: "a".to_owned(),
            url: None,
            count: 1,
            exploited: true,
            action: Some("Update to 1.28.".to_owned()),
            check_incomplete: false,
            why: Vec::new(),
            team_url: "https://coolify.example.com/vulnerabilities".to_owned(),
        });
        let digest = Notification::ImageVulnerabilityDigest(VulnerabilityDigest::new(
            Vec::new(),
            Vec::new(),
            Vec::new(),
            "https://coolify.example.com/",
        ));
        for n in [&exploited, &digest] {
            assert_eq!(n.event(), "image_vulnerabilities");
            assert_eq!(
                n.telegram_thread_column(),
                Some("telegram_notifications_image_vulnerabilities_thread_id")
            );
            assert_eq!(n.throttle(), None);
        }
        // The notification survives the queue (serde) unchanged.
        let value = serde_json::to_value(&exploited).unwrap();
        assert_eq!(value["type"], json!("ImageExploitedProblem"));
        assert_eq!(
            serde_json::from_value::<Notification>(value).unwrap(),
            exploited
        );
    }

    #[test]
    fn links_use_the_instance_url_and_the_path_of_the_page() {
        assert_eq!(
            url_path(
                "http://localhost/project/p/environment/e/application/a/vulnerabilities?x=1#top"
            ),
            Some("/project/p/environment/e/application/a/vulnerabilities")
        );
        assert_eq!(url_path("/project/p"), Some("/project/p"));
        assert_eq!(url_path("https://coolify.example.com"), None);
        assert_eq!(url_path("https://coolify.example.com?x=1"), None);
        assert_eq!(
            instance_link(
                "https://coolify.example.com/",
                "http://localhost:8000/project/p/environment/e/service/s/vulnerabilities"
            ),
            "https://coolify.example.com/project/p/environment/e/service/s/vulnerabilities"
        );
        assert_eq!(
            instance_link("https://coolify.example.com", "http://localhost"),
            "https://coolify.example.com"
        );
    }
}
