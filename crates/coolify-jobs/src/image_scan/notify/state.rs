//! The decisions of `ScanNotifications` over plain data: what the current scans of an owner
//! hold, which findings an alert names, what a baseline records, and what a digest sees and
//! records. The parent module reads the database and calls these.

use std::collections::{HashMap, HashSet};

use chrono::{Duration, NaiveDateTime};
use coolify_db::image_scans::ImageScanRow;
use coolify_db::image_scans::notifications::{NewState, NotificationStateRow, StateUpdate};
use serde_json::Value;

use super::super::finding::Finding;
use super::super::rules::{self, RelevanceRules};

/// `ScanNotifications::MAX_NOTIFIED_CANDIDATES`: candidates kept per resource; the oldest go
/// first.
pub const MAX_NOTIFIED_CANDIDATES: usize = 200;

/// `ScanNotifications::PENDING_HOURS`: how long after its scan a check_needed
/// (versions_unchecked) waits for the version check before a digest names it.
pub const PENDING_HOURS: i64 = 24;

/// `ScanNotifications::HOLD_MINUTES`: how long after its scan an exploited problem without a fix
/// waits for the version check before it is alerted.
pub const HOLD_MINUTES: i64 = 60;

/// `ScanNotifications::BASELINE_RESCAN_HOURS`: a baseline that a rescan (not a deploy) continues
/// ends this long after it started.
pub const BASELINE_RESCAN_HOURS: i64 = 2;

/// The values of `SecurityActionType`, in the order of its cases (the order the actions are
/// listed in).
pub const ACTION_TYPES: [&str; 7] = [
    "update_image",
    "pull_latest",
    "change_base",
    "rebuild",
    "upgrade_in_dockerfile",
    "upgrade_package",
    "rebuild_in_ci",
];

/// `ScanNotifications::CHECK_ACTIONS`: actions that only the version or base check finds. A
/// problem with another action never waits for the check.
const CHECK_ACTIONS: [&str; 3] = ["update_image", "pull_latest", "change_base"];

/// What the notifications read of a `SecurityAction`: its type (`SecurityActionType` value) and
/// its sentence (spec 15.5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixAction {
    pub action_type: String,
    pub sentence: String,
}

impl FixAction {
    pub fn new(action_type: &str, sentence: &str) -> Self {
        Self {
            action_type: action_type.to_owned(),
            sentence: sentence.to_owned(),
        }
    }

    /// `in_array($action->type, self::CHECK_ACTIONS, true)`.
    pub fn waits_for_check(&self) -> bool {
        CHECK_ACTIONS.contains(&self.action_type.as_str())
    }

    /// The position of the type among the cases of `SecurityActionType`.
    pub fn order(&self) -> usize {
        ACTION_TYPES
            .iter()
            .position(|t| *t == self.action_type)
            .unwrap_or(ACTION_TYPES.len())
    }
}

/// `ScanNotifications::checkState()`: whether the version check of a scan is outstanding, and
/// whether it is incomplete.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CheckState {
    pub outstanding: bool,
    pub incomplete: bool,
}

/// Lower-case IDs (`idSet()`).
pub type IdSet = HashSet<String>;

/// A relevant finding with an exploit signal of a current scan.
#[derive(Debug, Clone, PartialEq)]
pub struct SignalEntry {
    pub finding: Finding,
    /// The action that fixes it; `None`: none.
    pub action: Option<FixAction>,
    /// It waits for the version check: no action that does not depend on the check, the check is
    /// outstanding, and the scan is less than [`HOLD_MINUTES`] old.
    pub held: bool,
    /// The version check of its scan has no result.
    pub incomplete: bool,
    pub scan_id: i64,
    /// `$scan->scanned_at?->toIso8601String()`.
    pub scanned_at: Option<String>,
}

/// `ScanNotifications::current()`: the current scans of an owner, decoded once.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Current {
    pub scans: Vec<ImageScanRow>,
    /// Relevant findings with an exploit signal.
    pub signal: Vec<SignalEntry>,
    /// Relevant findings that an action fixes.
    pub fixable: Vec<Finding>,
    /// Better images with the IDs each fixes, in the order they were found.
    pub candidates: Vec<(String, IdSet)>,
    /// Every ID and alias found (lower case).
    pub present: IdSet,
    /// Every version check succeeded.
    pub candidates_checked: bool,
    /// A current scan waits for its version check (`versionCheckPending()`).
    pub pending: bool,
}

impl Current {
    /// The state before the first scan is added.
    pub fn new() -> Self {
        Self {
            candidates_checked: true,
            ..Self::default()
        }
    }

    /// `array_keys($current['candidates'])`.
    pub fn candidate_keys(&self) -> Vec<String> {
        self.candidates.iter().map(|(key, _)| key.clone()).collect()
    }

    /// `array_column($current['signal'], 'finding')`.
    fn signal_findings(&self) -> Vec<&Finding> {
        self.signal.iter().map(|entry| &entry.finding).collect()
    }
}

/// What `current()` reads of one scan besides its findings.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ScanFacts {
    /// `SecurityActions::forScan($scan, $rules)['byFinding']`: the action that fixes each
    /// finding, by finding ID.
    pub by_finding: HashMap<String, FixAction>,
    /// `versionCheckPending($scan)`.
    pub pending: bool,
    /// `checkState($scan, $owner)`; only read when the scan has a relevant finding with an
    /// exploit signal.
    pub check: CheckState,
    /// `betterCandidates($scan, $owner)`.
    pub candidates: Vec<(String, IdSet)>,
    /// `RefreshCandidates::needsCandidates($scan)`; only read before the first check.
    pub needs_candidates: bool,
}

/// `$scan->scanned_at?->toIso8601String()` (the stored times are UTC).
pub fn iso8601(at: NaiveDateTime) -> String {
    at.format("%Y-%m-%dT%H:%M:%S+00:00").to_string()
}

/// Whether a scan has a relevant finding with an exploit signal, so `current()` reads its
/// `checkState()`.
pub fn has_signal(findings: &[Finding], rules: &RelevanceRules) -> bool {
    findings
        .iter()
        .any(|f| rules::is_relevant(f, rules) && rules::exploit_signal(f).is_some())
}

/// One round of the loop of `ScanNotifications::current()`: adds what one scan holds.
///
/// `released`: a scan whose findings do not wait for its check any more.
pub fn add_scan(
    current: &mut Current,
    scan: &ImageScanRow,
    findings: &[Finding],
    facts: &ScanFacts,
    rules: &RelevanceRules,
    released: Option<i64>,
    now: NaiveDateTime,
) {
    current.pending = current.pending || facts.pending;
    for finding in findings {
        for id in finding.ids() {
            current.present.insert(id.to_lowercase());
        }
        if !rules::is_relevant(finding, rules) {
            continue;
        }
        let action = facts.by_finding.get(&finding.id).cloned();
        if action.is_some() {
            current.fixable.push(finding.clone());
        }
        if rules::exploit_signal(finding).is_none() {
            continue;
        }
        let waits = action.as_ref().is_none_or(FixAction::waits_for_check);
        let fresh = scan
            .scanned_at
            .is_some_and(|at| at > now - Duration::minutes(HOLD_MINUTES));
        current.signal.push(SignalEntry {
            finding: finding.clone(),
            action,
            held: waits && facts.check.outstanding && Some(scan.id) != released && fresh,
            incomplete: facts.check.incomplete,
            scan_id: scan.id,
            scanned_at: scan.scanned_at.map(iso8601),
        });
    }
    // `$candidates += …`: the first scan that offers a candidate names what it fixes.
    for (key, fixed) in &facts.candidates {
        if !current.candidates.iter().any(|(k, _)| k == key) {
            current.candidates.push((key.clone(), fixed.clone()));
        }
    }
    // A failed check, or one that did not run yet, keeps the candidates the team was told about.
    let status = scan.candidate_status();
    let base_failed = scan
        .candidates
        .as_ref()
        .and_then(|c| c.pointer("/base/status"))
        .and_then(Value::as_str)
        == Some("failed");
    if status == Some("failed") || base_failed || (status.is_none() && facts.needs_candidates) {
        current.candidates_checked = false;
    }
}

/// `ScanNotifications::idSet($ids)`: lower-case IDs.
pub fn id_set(ids: &[String]) -> IdSet {
    ids.iter().map(|id| id.to_lowercase()).collect()
}

/// `ScanNotifications::isIn($finding, $set)`: one of the ID and aliases of the finding is in the
/// set.
pub fn is_in(finding: &Finding, set: &IdSet) -> bool {
    finding.ids().any(|id| set.contains(&id.to_lowercase()))
}

/// `ScanNotifications::ids($findings)`: the IDs of the findings, each once.
pub fn ids<'a>(findings: impl IntoIterator<Item = &'a Finding>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for finding in findings {
        if !out.contains(&finding.id) {
            out.push(finding.id.clone());
        }
    }
    out
}

/// `ScanNotifications::merge($ids, $add)`: both lists, each value once.
pub fn merge(ids: &[String], add: &[String]) -> Vec<String> {
    let mut seen: HashSet<&str> = HashSet::new();
    ids.iter()
        .chain(add)
        .filter(|id| seen.insert(id.as_str()))
        .cloned()
        .collect()
}

/// `ScanNotifications::capCandidates($keys)`: unique keys, the newest last, at most
/// [`MAX_NOTIFIED_CANDIDATES`] (the oldest are dropped).
pub fn cap_candidates(keys: &[String]) -> Vec<String> {
    let mut seen: HashSet<&str> = HashSet::new();
    let mut unique: Vec<String> = keys
        .iter()
        .rev()
        .filter(|key| seen.insert(key.as_str()))
        .cloned()
        .collect();
    unique.truncate(MAX_NOTIFIED_CANDIDATES);
    unique.reverse();
    unique
}

/// `ScanNotifications::extendsBaseline()`: a scan of the deployment that started the baseline
/// adds to it: always on the deploy path, and on a rescan or Scan now (the deployment of the row)
/// only within [`BASELINE_RESCAN_HOURS`] of the start, so the first rescan of a Compose
/// application is one baseline while a later rescan of the same row alerts.
pub fn extends_baseline(
    state: &NotificationStateRow,
    deployment_id: Option<i64>,
    explicit: bool,
    now: NaiveDateTime,
) -> bool {
    if deployment_id.is_none() || deployment_id != state.baseline_deployment_queue_id {
        return false;
    }
    explicit
        || state
            .created_at
            .is_some_and(|at| at > now - Duration::hours(BASELINE_RESCAN_HOURS))
}

/// `ScanNotifications::createState()`: the baseline row, from what the current scans have.
pub fn baseline_state(
    owner_type: &str,
    owner_id: i64,
    team_id: i64,
    current: &Current,
    deployment_id: Option<i64>,
) -> NewState {
    NewState {
        resource_type: owner_type.to_owned(),
        resource_id: owner_id,
        team_id,
        notified_ids: ids(current.signal_findings()),
        digest_ids: ids(&current.fixable),
        notified_candidates: cap_candidates(&current.candidate_keys()),
        check_reported: false,
        held_digests: 0,
        is_baseline: true,
        baseline_deployment_queue_id: deployment_id,
    }
}

/// What a scan (or a version check) that continues an open baseline records, without notifying.
pub fn extend_baseline(state: &NotificationStateRow, recorded: &Current) -> StateUpdate {
    let mut candidates = state.notified_candidates();
    candidates.extend(recorded.candidate_keys());
    StateUpdate {
        notified_ids: Some(merge(
            &state.notified_ids(),
            &ids(recorded.signal_findings()),
        )),
        digest_ids: Some(merge(&state.digest_ids(), &ids(&recorded.fixable))),
        notified_candidates: Some(cap_candidates(&candidates)),
        ..StateUpdate::default()
    }
}

/// What `afterScan()` does for an owner whose baseline ended.
#[derive(Debug, Clone, PartialEq)]
pub struct AlertChange {
    pub update: StateUpdate,
    /// The new relevant findings with an exploit signal that the alert names.
    pub alert: Vec<SignalEntry>,
    /// The scans whose new problems wait for the version check: `(scan id, scan time)`.
    pub held: Vec<(i64, Option<String>)>,
}

/// The alert of `afterScan()`: a finding is new when none of its ID and aliases was notified, so
/// an EPSS that moves around the threshold does not alert again. The alert ends a baseline.
pub fn alert_change(state: &NotificationStateRow, current: &Current) -> AlertChange {
    let stored = state.notified_ids();
    let mut notified = id_set(&stored);
    let mut alert: Vec<SignalEntry> = Vec::new();
    let mut held: Vec<(i64, Option<String>)> = Vec::new();
    for entry in &current.signal {
        if is_in(&entry.finding, &notified) {
            continue;
        }
        if entry.held {
            match held.iter_mut().find(|(id, _)| *id == entry.scan_id) {
                Some(slot) => slot.1 = entry.scanned_at.clone(),
                None => held.push((entry.scan_id, entry.scanned_at.clone())),
            }
            continue;
        }
        for id in entry.finding.ids() {
            notified.insert(id.to_lowercase());
        }
        alert.push(entry.clone());
    }
    let named = ids(alert.iter().map(|entry| &entry.finding));
    let with_action = ids(alert
        .iter()
        .filter(|entry| entry.action.is_some())
        .map(|entry| &entry.finding));
    AlertChange {
        update: StateUpdate {
            is_baseline: Some(false),
            notified_ids: Some(merge(&stored, &named)),
            digest_ids: Some(merge(&state.digest_ids(), &with_action)),
            ..StateUpdate::default()
        },
        alert,
        held,
    }
}

/// What `applyDigest()` records for one owner after a digest (`mark` of `digestPlan()`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DigestMark {
    /// The plan saw a baseline (or no state row).
    pub baseline: bool,
    pub owner_type: String,
    pub owner_id: i64,
    pub team_id: i64,
    /// What the state had when the digest read it.
    pub read_ids: Vec<String>,
    pub read_digest_ids: Vec<String>,
    pub read_candidates: Vec<String>,
    pub add_ids: Vec<String>,
    pub add_digest_ids: Vec<String>,
    pub add_candidates: Vec<String>,
    /// Every ID and alias the current scans have (lower case).
    pub present: Vec<String>,
    /// Every better image the current scans offer.
    pub offered: Vec<String>,
    pub prune_ids: bool,
    pub prune_candidates: bool,
    /// `None` keeps the stored value.
    pub check_reported: Option<bool>,
    /// `None` keeps the stored value.
    pub held_digests: Option<i64>,
}

/// `ScanNotifications::digestPlan()`: what a digest sees for one owner.
#[derive(Debug, Clone, PartialEq)]
pub struct DigestPlan {
    pub baseline: bool,
    /// Relevant findings with an action that no digest named yet.
    pub new_fixable: Vec<Finding>,
    /// The ones of them with an exploit signal.
    pub new_signal_fixable: Vec<Finding>,
    /// Better images no digest named yet.
    pub new_candidates: Vec<String>,
    /// The ones that fix a finding with an exploit signal.
    pub new_signal_candidates: Vec<String>,
    /// A current scan waits for its version check.
    pub pending: bool,
    pub check_reported: bool,
    pub held_digests: i64,
    pub mark: DigestMark,
}

/// The plan of `digestPlan()` from the state row and the current scans. Nothing is new for a
/// baseline or an owner without a state row; the digest records what it sees.
///
/// `prunable`: no deployment of the application runs and every image of a Compose application
/// has a result, so IDs that no current scan has are gone.
pub fn plan_digest(
    owner_type: &str,
    owner_id: i64,
    team_id: i64,
    state: Option<&NotificationStateRow>,
    current: &Current,
    prunable: bool,
) -> DigestPlan {
    let baseline = state.is_none_or(|s| s.is_baseline);
    let read_ids = state
        .map(NotificationStateRow::notified_ids)
        .unwrap_or_default();
    let read_digest_ids = state
        .map(NotificationStateRow::digest_ids)
        .unwrap_or_default();
    let read_candidates = state
        .map(NotificationStateRow::notified_candidates)
        .unwrap_or_default();
    let reported = id_set(&read_digest_ids);
    let candidate_keys = current.candidate_keys();

    let new_fixable: Vec<Finding> = if baseline {
        Vec::new()
    } else {
        current
            .fixable
            .iter()
            .filter(|f| !is_in(f, &reported))
            .cloned()
            .collect()
    };
    let new_candidates: Vec<String> = if baseline {
        Vec::new()
    } else {
        candidate_keys
            .iter()
            .filter(|key| !read_candidates.contains(key))
            .cloned()
            .collect()
    };

    let mut signal_ids = IdSet::new();
    for entry in &current.signal {
        for id in entry.finding.ids() {
            signal_ids.insert(id.to_lowercase());
        }
    }
    let new_signal_fixable: Vec<Finding> = new_fixable
        .iter()
        .filter(|f| is_in(f, &signal_ids))
        .cloned()
        .collect();
    let new_signal_candidates: Vec<String> = new_candidates
        .iter()
        .filter(|key| {
            current
                .candidates
                .iter()
                .find(|(k, _)| k == *key)
                .is_some_and(|(_, fixed)| !fixed.is_disjoint(&signal_ids))
        })
        .cloned()
        .collect();

    let mut present: Vec<String> = current.present.iter().cloned().collect();
    present.sort();
    let mark = DigestMark {
        baseline,
        owner_type: owner_type.to_owned(),
        owner_id,
        team_id,
        read_ids,
        read_digest_ids,
        read_candidates,
        // A fix of an exploited problem that the digest names is not alerted again when its wait ends.
        add_ids: if baseline {
            ids(current.signal_findings())
        } else {
            ids(&new_signal_fixable)
        },
        add_digest_ids: if baseline {
            ids(&current.fixable)
        } else {
            ids(&new_fixable)
        },
        add_candidates: if baseline {
            candidate_keys.clone()
        } else {
            new_candidates.clone()
        },
        present,
        offered: candidate_keys,
        prune_ids: prunable,
        prune_candidates: prunable && current.candidates_checked,
        check_reported: None,
        held_digests: None,
    };

    DigestPlan {
        baseline,
        new_fixable,
        new_signal_fixable,
        new_candidates,
        new_signal_candidates,
        pending: current.pending,
        check_reported: state.is_some_and(|s| s.check_reported),
        held_digests: state.map_or(0, |s| s.held_digests),
        mark,
    }
}

/// The row `applyDigest()` creates for an owner without a state row.
pub fn first_digest_state(mark: &DigestMark) -> NewState {
    NewState {
        resource_type: mark.owner_type.clone(),
        resource_id: mark.owner_id,
        team_id: mark.team_id,
        notified_ids: mark.add_ids.clone(),
        digest_ids: mark.add_digest_ids.clone(),
        notified_candidates: cap_candidates(&mark.add_candidates),
        check_reported: mark.check_reported.unwrap_or(false),
        held_digests: mark.held_digests.unwrap_or(0),
        is_baseline: false,
        baseline_deployment_queue_id: None,
    }
}

/// What `applyDigest()` writes to the locked state row: what the digest covered, and the end of
/// a baseline. IDs and candidates that the state had when the digest read it and that no current
/// scan has any more are removed; ones added meanwhile stay.
pub fn applied_digest(state: &NotificationStateRow, mark: &DigestMark) -> StateUpdate {
    // The plan recorded a baseline without reporting it; when the baseline ended meanwhile (an
    // alert or a scan of another deployment), those findings are not added, so they can still be
    // reported.
    let ended_meanwhile = mark.baseline && !state.is_baseline;
    let none: Vec<String> = Vec::new();
    let (add_ids, add_digest_ids, add_candidates) = if ended_meanwhile {
        (&none, &none, &none)
    } else {
        (&mark.add_ids, &mark.add_digest_ids, &mark.add_candidates)
    };

    let present = id_set(&mark.present);
    let prune = |stored: &[String], add: &[String], read: &[String]| -> Vec<String> {
        let read = id_set(read);
        merge(stored, add)
            .into_iter()
            .filter(|id| {
                let id = id.to_lowercase();
                !mark.prune_ids || !read.contains(&id) || present.contains(&id)
            })
            .collect()
    };

    let mut candidates = state.notified_candidates();
    candidates.extend(add_candidates.iter().cloned());
    candidates.retain(|key| {
        !mark.prune_candidates || !mark.read_candidates.contains(key) || mark.offered.contains(key)
    });

    StateUpdate {
        is_baseline: Some(false),
        notified_ids: Some(prune(&state.notified_ids(), add_ids, &mark.read_ids)),
        digest_ids: Some(prune(
            &state.digest_ids(),
            add_digest_ids,
            &mark.read_digest_ids,
        )),
        notified_candidates: Some(cap_candidates(&candidates)),
        check_reported: mark.check_reported,
        held_digests: mark.held_digests,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use chrono::NaiveDate;
    use coolify_db::image_scans::{APPLICATION_MORPH, STATUS_COMPLETED};
    use serde_json::json;

    use super::super::super::resource_scans::tests::scan as scan_row;
    use super::super::super::rules::tests::{finding as plain_finding, ignore};
    use super::*;

    pub(crate) fn now() -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 11, 10)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap()
    }

    /// A critical finding with a fix and no exploit signal (`notifyFinding()`).
    pub(crate) fn finding(id: &str) -> Finding {
        let mut f = plain_finding(id, "critical");
        f.r#type = "apk".to_owned();
        f.fixed_in = Some("3.0.2".to_owned());
        f.fix_state = "fixed".to_owned();
        f.epss = Some(0.01);
        f.title = format!("The bug {id} lets attackers read memory");
        f
    }

    /// A finding that attackers are using (`notifyExploited()`).
    pub(crate) fn exploited(id: &str) -> Finding {
        let mut f = finding(id);
        f.kev = true;
        f
    }

    pub(crate) fn rebuild() -> FixAction {
        FixAction::new(
            "rebuild_in_ci",
            "Rebuild the image in your CI, then redeploy.",
        )
    }

    fn rules() -> RelevanceRules {
        RelevanceRules::from_ignores(Vec::new(), false, now())
    }

    /// A completed scan that was scanned `minutes_ago`.
    fn scan(id: i64, minutes_ago: i64) -> ImageScanRow {
        let mut row = scan_row("nginx:1.27", STATUS_COMPLETED, None);
        row.id = id;
        row.scanned_at = Some(now() - Duration::minutes(minutes_ago));
        row.candidates = Some(
            json!({"status": "no_better", "checked_at": "2026-11-10T11:59:00+00:00", "error": null, "entries": []}),
        );
        row
    }

    fn actions(findings: &[Finding], action: &FixAction) -> HashMap<String, FixAction> {
        findings
            .iter()
            .map(|f| (f.id.clone(), action.clone()))
            .collect()
    }

    /// The current scans of an owner with one scan that holds `findings`, each fixed by `action`.
    pub(crate) fn current_of(findings: &[Finding], action: Option<&FixAction>) -> Current {
        let facts = ScanFacts {
            by_finding: action.map(|a| actions(findings, a)).unwrap_or_default(),
            ..ScanFacts::default()
        };
        let row = scan(1, 5);
        let mut current = Current::new();
        add_scan(&mut current, &row, findings, &facts, &rules(), None, now());
        current.scans = vec![row];
        current
    }

    pub(crate) fn state(
        notified: &[&str],
        digest: &[&str],
        candidates: &[&str],
        baseline: bool,
    ) -> NotificationStateRow {
        NotificationStateRow {
            id: 1,
            resource_type: APPLICATION_MORPH.to_owned(),
            resource_id: 7,
            team_id: 1,
            notified_ids: Some(json!(notified)),
            notified_candidates: Some(json!(candidates)),
            digest_ids: Some(json!(digest)),
            is_baseline: baseline,
            check_reported: false,
            held_digests: 0,
            baseline_deployment_queue_id: None,
            created_at: Some(now()),
            updated_at: Some(now()),
        }
    }

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|v| (*v).to_owned()).collect()
    }

    fn plan(state: Option<&NotificationStateRow>, current: &Current, prunable: bool) -> DigestPlan {
        plan_digest(APPLICATION_MORPH, 7, 1, state, current, prunable)
    }

    #[test]
    fn merges_and_caps_lists() {
        assert_eq!(
            merge(&strings(&["a", "b"]), &strings(&["b", "c", "a"])),
            strings(&["a", "b", "c"])
        );
        // The newest occurrence of a key counts.
        assert_eq!(
            cap_candidates(&strings(&["a", "b", "a", "c"])),
            strings(&["b", "a", "c"])
        );
        let many: Vec<String> = (0..250).map(|i| format!("nginx:{i}@sha256:x")).collect();
        let capped = cap_candidates(&many);
        assert_eq!(capped.len(), MAX_NOTIFIED_CANDIDATES);
        assert_eq!(capped.first(), Some(&"nginx:50@sha256:x".to_owned()));
        assert_eq!(capped.last(), Some(&"nginx:249@sha256:x".to_owned()));
    }

    #[test]
    fn matches_ids_by_alias_without_case() {
        let mut f = finding("GHSA-aaaa-bbbb-cccc");
        f.aliases = vec!["CVE-2026-0001".to_owned()];
        assert!(is_in(&f, &id_set(&strings(&["cve-2026-0001"]))));
        assert!(is_in(&f, &id_set(&strings(&["ghsa-AAAA-bbbb-cccc"]))));
        assert!(!is_in(&f, &id_set(&strings(&["CVE-2026-0002"]))));
        assert_eq!(
            ids([&f, &finding("CVE-2026-0002"), &f]),
            strings(&["GHSA-aaaa-bbbb-cccc", "CVE-2026-0002"])
        );
    }

    #[test]
    fn action_types_keep_the_order_of_the_cases() {
        assert!(FixAction::new("update_image", "Update to 1.28.").waits_for_check());
        assert!(FixAction::new("pull_latest", "Pull the latest image.").waits_for_check());
        assert!(FixAction::new("change_base", "Change the base image.").waits_for_check());
        assert!(!rebuild().waits_for_check());
        assert_eq!(FixAction::new("update_image", "").order(), 0);
        assert_eq!(rebuild().order(), 6);
        assert_eq!(FixAction::new("other", "").order(), 7);
    }

    #[test]
    fn current_collects_signal_fixable_and_present_ids() {
        let mut aliased = exploited("GHSA-aaaa-bbbb-cccc");
        aliased.aliases = vec!["CVE-2026-0001".to_owned()];
        let fixable = finding("CVE-2026-0002");
        let mut low = finding("CVE-2026-0003");
        low.severity = "low".to_owned();
        let findings = vec![aliased.clone(), fixable.clone(), low];
        let facts = ScanFacts {
            by_finding: actions(&findings[1..2], &rebuild()),
            pending: true,
            ..ScanFacts::default()
        };
        let mut current = Current::new();
        add_scan(
            &mut current,
            &scan(4, 5),
            &findings,
            &facts,
            &rules(),
            None,
            now(),
        );

        assert_eq!(current.signal.len(), 1);
        assert_eq!(current.signal[0].finding, aliased);
        assert_eq!(current.signal[0].action, None);
        assert!(!current.signal[0].held);
        assert_eq!(current.signal[0].scan_id, 4);
        assert_eq!(
            current.signal[0].scanned_at.as_deref(),
            Some("2026-11-10T11:55:00+00:00")
        );
        assert_eq!(current.fixable, vec![fixable]);
        // Findings that are not relevant are present too.
        assert_eq!(
            current.present,
            id_set(&strings(&[
                "ghsa-aaaa-bbbb-cccc",
                "cve-2026-0001",
                "cve-2026-0002",
                "cve-2026-0003"
            ]))
        );
        assert!(current.pending);
        assert!(current.candidates_checked);
        assert!(has_signal(&findings, &rules()));
        assert!(!has_signal(&findings[1..], &rules()));
    }

    #[test]
    fn current_skips_ignored_findings() {
        let findings = vec![exploited("CVE-2026-0001"), finding("CVE-2026-0002")];
        let ignored =
            RelevanceRules::from_ignores(vec![ignore(Some("CVE-2026-0001"), None)], false, now());
        let facts = ScanFacts {
            by_finding: actions(&findings, &rebuild()),
            ..ScanFacts::default()
        };
        let mut current = Current::new();
        add_scan(
            &mut current,
            &scan(1, 5),
            &findings,
            &facts,
            &ignored,
            None,
            now(),
        );
        assert!(current.signal.is_empty());
        assert_eq!(ids(&current.fixable), strings(&["CVE-2026-0002"]));
        assert!(current.present.contains("cve-2026-0001"));
    }

    #[test]
    fn a_problem_waits_for_an_outstanding_version_check() {
        let findings = vec![exploited("CVE-2026-0001")];
        let outstanding = CheckState {
            outstanding: true,
            incomplete: true,
        };
        let held = |facts: &ScanFacts, minutes_ago: i64, released: Option<i64>| {
            let mut current = Current::new();
            add_scan(
                &mut current,
                &scan(9, minutes_ago),
                &findings,
                facts,
                &rules(),
                released,
                now(),
            );
            (current.signal[0].held, current.signal[0].incomplete)
        };

        // Without an action the problem waits while the check is outstanding.
        let waiting = ScanFacts {
            check: outstanding,
            ..ScanFacts::default()
        };
        assert_eq!(held(&waiting, 5, None), (true, true));
        // The wait ends HOLD_MINUTES after the scan.
        assert_eq!(held(&waiting, 59, None), (true, true));
        assert_eq!(held(&waiting, 60, None), (false, true));
        // The check gave up: the findings of this scan do not wait any more.
        assert_eq!(held(&waiting, 5, Some(9)), (false, true));
        assert_eq!(held(&waiting, 5, Some(8)), (true, true));
        // No check is outstanding.
        assert_eq!(held(&ScanFacts::default(), 5, None), (false, false));

        // A fix that does not depend on the check never waits.
        let rebuilt = ScanFacts {
            by_finding: actions(&findings, &rebuild()),
            check: outstanding,
            ..ScanFacts::default()
        };
        assert_eq!(held(&rebuilt, 5, None), (false, true));
        // An image update can still change with the check.
        let update = ScanFacts {
            by_finding: actions(
                &findings,
                &FixAction::new("update_image", "Update to 1.28."),
            ),
            check: outstanding,
            ..ScanFacts::default()
        };
        assert_eq!(held(&update, 5, None), (true, true));

        // A scan without a scan time never waits.
        let mut unscanned = scan(9, 5);
        unscanned.scanned_at = None;
        let mut current = Current::new();
        add_scan(
            &mut current,
            &unscanned,
            &findings,
            &waiting,
            &rules(),
            None,
            now(),
        );
        assert!(!current.signal[0].held);
        assert_eq!(current.signal[0].scanned_at, None);
    }

    #[test]
    fn a_failed_or_missing_version_check_keeps_the_candidates() {
        let checked = |candidates: Option<Value>, needs_candidates: bool| {
            let mut row = scan(1, 5);
            row.candidates = candidates;
            let facts = ScanFacts {
                needs_candidates,
                ..ScanFacts::default()
            };
            let mut current = Current::new();
            add_scan(&mut current, &row, &[], &facts, &rules(), None, now());
            current.candidates_checked
        };
        assert!(checked(Some(json!({"status": "ok", "entries": []})), true));
        assert!(!checked(
            Some(json!({"status": "failed", "entries": []})),
            false
        ));
        assert!(!checked(
            Some(json!({"status": "ok", "base": {"status": "failed"}})),
            false
        ));
        // Before the first check: only when the scan needs one.
        assert!(!checked(None, true));
        assert!(checked(None, false));
    }

    #[test]
    fn the_first_scan_that_offers_a_candidate_names_what_it_fixes() {
        let key = "nginx:1.28@sha256:b".to_owned();
        let mut current = Current::new();
        for fixed in ["cve-2026-0001", "cve-2026-0002"] {
            let facts = ScanFacts {
                candidates: vec![(key.clone(), id_set(&strings(&[fixed])))],
                ..ScanFacts::default()
            };
            add_scan(
                &mut current,
                &scan(1, 5),
                &[],
                &facts,
                &rules(),
                None,
                now(),
            );
        }
        assert_eq!(
            current.candidates,
            vec![(key, id_set(&strings(&["cve-2026-0001"])))]
        );
    }

    #[test]
    fn creates_a_baseline_from_what_the_current_scans_have() {
        // "creates a baseline for the first deploy of an image and sends nothing"
        let mut current = current_of(
            &[exploited("CVE-2026-0001"), finding("CVE-2026-0002")],
            Some(&rebuild()),
        );
        current.candidates = vec![("nginx:1.28@sha256:b".to_owned(), IdSet::new())];
        let new = baseline_state(APPLICATION_MORPH, 7, 1, &current, Some(33));
        assert!(new.is_baseline);
        assert_eq!(new.notified_ids, strings(&["CVE-2026-0001"]));
        assert_eq!(new.digest_ids, strings(&["CVE-2026-0001", "CVE-2026-0002"]));
        assert_eq!(new.notified_candidates, strings(&["nginx:1.28@sha256:b"]));
        assert_eq!(new.baseline_deployment_queue_id, Some(33));
        assert_eq!(
            (new.resource_type.as_str(), new.resource_id, new.team_id),
            (APPLICATION_MORPH, 7, 1)
        );
        assert!(!new.check_reported);
        assert_eq!(new.held_digests, 0);
    }

    #[test]
    fn a_baseline_is_extended_by_its_deployment_only() {
        let mut baseline = state(&[], &[], &[], true);
        baseline.baseline_deployment_queue_id = Some(33);
        // The deploy path always adds to the baseline of its deployment.
        assert!(extends_baseline(
            &baseline,
            Some(33),
            true,
            now() + Duration::hours(30)
        ));
        // A rescan of the row only within BASELINE_RESCAN_HOURS of the start.
        assert!(extends_baseline(
            &baseline,
            Some(33),
            false,
            now() + Duration::minutes(119)
        ));
        assert!(!extends_baseline(
            &baseline,
            Some(33),
            false,
            now() + Duration::hours(2)
        ));
        // Another deployment, or a scan without one, ends it.
        assert!(!extends_baseline(&baseline, Some(34), true, now()));
        assert!(!extends_baseline(&baseline, None, false, now()));
        // A baseline of a scan without a deployment is never extended.
        baseline.baseline_deployment_queue_id = None;
        assert!(!extends_baseline(&baseline, None, true, now()));
        assert!(!extends_baseline(&baseline, Some(33), true, now()));
    }

    #[test]
    fn a_scan_of_the_baseline_deployment_is_recorded_without_notice() {
        // "treats both images of a first Compose deployment as a baseline"
        let baseline = state(&["CVE-2026-0001"], &["CVE-2026-0001"], &["a"], true);
        let mut recorded = current_of(
            &[
                exploited("CVE-2026-0001"),
                exploited("CVE-2026-0005"),
                finding("CVE-2026-0006"),
            ],
            Some(&rebuild()),
        );
        recorded.candidates = vec![
            ("b".to_owned(), IdSet::new()),
            ("a".to_owned(), IdSet::new()),
        ];
        let update = extend_baseline(&baseline, &recorded);
        assert_eq!(update.is_baseline, None);
        assert_eq!(
            update.notified_ids,
            Some(strings(&["CVE-2026-0001", "CVE-2026-0005"]))
        );
        assert_eq!(
            update.digest_ids,
            Some(strings(&[
                "CVE-2026-0001",
                "CVE-2026-0005",
                "CVE-2026-0006"
            ]))
        );
        assert_eq!(update.notified_candidates, Some(strings(&["b", "a"])));
        assert_eq!((update.check_reported, update.held_digests), (None, None));
    }

    #[test]
    fn alerts_once_for_new_exploited_problems() {
        // "alerts once for new problems that attackers are using with the action that fixes them"
        let current = current_of(
            &[
                exploited("CVE-2026-0001"),
                exploited("CVE-2026-0002"),
                finding("CVE-2026-0003"),
            ],
            Some(&rebuild()),
        );
        let change = alert_change(&state(&[], &[], &[], false), &current);
        assert_eq!(
            ids(change.alert.iter().map(|e| &e.finding)),
            strings(&["CVE-2026-0001", "CVE-2026-0002"])
        );
        assert!(change.held.is_empty());
        assert_eq!(change.update.is_baseline, Some(false));
        assert_eq!(
            change.update.notified_ids,
            Some(strings(&["CVE-2026-0001", "CVE-2026-0002"]))
        );
        // The alert named the action, so the digest does not name these problems again.
        assert_eq!(
            change.update.digest_ids,
            Some(strings(&["CVE-2026-0001", "CVE-2026-0002"]))
        );
        assert_eq!(change.update.notified_candidates, None);

        // The next scan of the same image alerts nothing.
        let mut after = state(&["CVE-2026-0001", "CVE-2026-0002"], &[], &[], false);
        after.digest_ids = Some(json!(["CVE-2026-0001", "CVE-2026-0002"]));
        let again = alert_change(&after, &current);
        assert!(again.alert.is_empty());
        assert_eq!(
            again.update.notified_ids,
            Some(strings(&["CVE-2026-0001", "CVE-2026-0002"]))
        );
    }

    #[test]
    fn an_alert_without_a_fix_is_not_recorded_for_the_digest() {
        // "alerts for a new problem that attackers are using without a fix"
        let current = current_of(&[exploited("CVE-2026-0001")], None);
        let change = alert_change(&state(&[], &["CVE-2026-0009"], &[], false), &current);
        assert_eq!(change.alert.len(), 1);
        assert_eq!(change.alert[0].action, None);
        assert_eq!(
            change.update.notified_ids,
            Some(strings(&["CVE-2026-0001"]))
        );
        assert_eq!(change.update.digest_ids, Some(strings(&["CVE-2026-0009"])));
    }

    #[test]
    fn an_alias_of_a_notified_finding_does_not_alert_again() {
        // "matches reported IDs by alias and skips ignored findings", and an EPSS that drops
        // below and rises above the threshold: the ID stays notified.
        let mut aliased = exploited("GHSA-aaaa-bbbb-cccc");
        aliased.aliases = vec!["CVE-2026-0001".to_owned()];
        let current = current_of(&[aliased.clone(), aliased], Some(&rebuild()));
        let notified = alert_change(&state(&["cve-2026-0001"], &[], &[], false), &current);
        assert!(notified.alert.is_empty());
        // Two findings with the same alias in one scan alert once.
        let fresh = alert_change(&state(&[], &[], &[], false), &current);
        assert_eq!(fresh.alert.len(), 1);
        assert_eq!(
            fresh.update.notified_ids,
            Some(strings(&["GHSA-aaaa-bbbb-cccc"]))
        );
    }

    #[test]
    fn an_alert_ends_a_baseline() {
        // "reports a new critical of a redeploy that comes before the first digest": a scan of
        // another deployment ends the baseline, also when it alerts nothing.
        let current = current_of(&[finding("CVE-2026-0002")], Some(&rebuild()));
        let change = alert_change(&state(&["CVE-2026-0001"], &[], &[], true), &current);
        assert!(change.alert.is_empty());
        assert_eq!(change.update.is_baseline, Some(false));
        assert_eq!(
            change.update.notified_ids,
            Some(strings(&["CVE-2026-0001"]))
        );
        assert_eq!(change.update.digest_ids, Some(Vec::new()));
    }

    #[test]
    fn waiting_problems_are_looked_at_again_later() {
        // "waits for the version check before it alerts a problem without a fix"
        let findings = [exploited("CVE-2026-0001"), exploited("CVE-2026-0002")];
        let mut current = current_of(&findings, None);
        for entry in &mut current.signal {
            entry.held = true;
            entry.scan_id = 5;
        }
        current.signal[1].scanned_at = Some("2026-11-10T11:58:00+00:00".to_owned());
        let change = alert_change(&state(&[], &[], &[], false), &current);
        assert!(change.alert.is_empty());
        // One follow-up per scan, with the scan time of its last waiting problem.
        assert_eq!(
            change.held,
            vec![(5, Some("2026-11-10T11:58:00+00:00".to_owned()))]
        );
        // Nothing is recorded, so the alert can still be sent when the wait ends.
        assert_eq!(change.update.notified_ids, Some(Vec::new()));

        // "alerts with neutral wording when the wait … ends": the same problems, not held.
        for entry in &mut current.signal {
            entry.held = false;
        }
        let released = alert_change(&state(&[], &[], &[], false), &current);
        assert_eq!(released.alert.len(), 2);
        assert!(released.held.is_empty());
    }

    #[test]
    fn nothing_is_new_for_a_baseline_and_the_digest_records_what_it_sees() {
        let mut current = current_of(
            &[exploited("CVE-2026-0001"), finding("CVE-2026-0002")],
            Some(&rebuild()),
        );
        current.candidates = vec![(
            "nginx:1.28@sha256:b".to_owned(),
            id_set(&strings(&["cve-2026-0001"])),
        )];
        for stored in [None, Some(state(&["CVE-2026-0009"], &[], &[], true))] {
            let plan = plan(stored.as_ref(), &current, true);
            assert!(plan.baseline);
            assert!(plan.new_fixable.is_empty() && plan.new_candidates.is_empty());
            assert!(plan.new_signal_fixable.is_empty() && plan.new_signal_candidates.is_empty());
            assert!(plan.mark.baseline);
            assert_eq!(plan.mark.add_ids, strings(&["CVE-2026-0001"]));
            assert_eq!(
                plan.mark.add_digest_ids,
                strings(&["CVE-2026-0001", "CVE-2026-0002"])
            );
            assert_eq!(plan.mark.add_candidates, strings(&["nginx:1.28@sha256:b"]));
            assert_eq!(plan.mark.offered, strings(&["nginx:1.28@sha256:b"]));
            assert_eq!(
                plan.mark.present,
                strings(&["cve-2026-0001", "cve-2026-0002"])
            );
            assert_eq!(
                (plan.mark.check_reported, plan.mark.held_digests),
                (None, None)
            );
            assert_eq!(
                (
                    plan.mark.owner_type.as_str(),
                    plan.mark.owner_id,
                    plan.mark.team_id
                ),
                (APPLICATION_MORPH, 7, 1)
            );
        }
    }

    #[test]
    fn the_digest_sees_new_problems_with_an_action_and_new_candidates() {
        // "sends problems without an exploit signal to the digest, not right away" and
        // "reports a newly released fixing version once"
        let mut current = current_of(
            &[
                exploited("CVE-2026-0001"),
                finding("CVE-2026-0002"),
                finding("CVE-2026-0003"),
            ],
            Some(&rebuild()),
        );
        current.candidates = vec![
            (
                "nginx:1.28@sha256:b".to_owned(),
                id_set(&strings(&["cve-2026-0001"])),
            ),
            (
                "nginx:1.29@sha256:c".to_owned(),
                id_set(&strings(&["cve-2026-0002"])),
            ),
            (
                "base:debian:13".to_owned(),
                id_set(&strings(&["cve-2026-0003"])),
            ),
        ];
        current.pending = true;
        let mut stored = state(&[], &["cve-2026-0003"], &["base:debian:13"], false);
        stored.check_reported = true;
        stored.held_digests = 2;
        let plan = plan(Some(&stored), &current, true);

        assert!(!plan.baseline);
        assert_eq!(
            ids(&plan.new_fixable),
            strings(&["CVE-2026-0001", "CVE-2026-0002"])
        );
        assert_eq!(ids(&plan.new_signal_fixable), strings(&["CVE-2026-0001"]));
        assert_eq!(
            plan.new_candidates,
            strings(&["nginx:1.28@sha256:b", "nginx:1.29@sha256:c"])
        );
        assert_eq!(
            plan.new_signal_candidates,
            strings(&["nginx:1.28@sha256:b"])
        );
        assert!(plan.pending && plan.check_reported);
        assert_eq!(plan.held_digests, 2);
        // A fix of an exploited problem that the digest names is not alerted again.
        assert_eq!(plan.mark.add_ids, strings(&["CVE-2026-0001"]));
        assert_eq!(
            plan.mark.add_digest_ids,
            strings(&["CVE-2026-0001", "CVE-2026-0002"])
        );
        assert_eq!(plan.mark.add_candidates, plan.new_candidates);
        assert_eq!(plan.mark.read_digest_ids, strings(&["cve-2026-0003"]));
        assert_eq!(plan.mark.read_candidates, strings(&["base:debian:13"]));
        assert_eq!(plan.mark.offered.len(), 3);
        assert!(plan.mark.prune_ids && plan.mark.prune_candidates);

        // "sends no digest when nothing changed"
        let mut covered = state(
            &["CVE-2026-0001"],
            &["CVE-2026-0001", "CVE-2026-0002", "CVE-2026-0003"],
            &[
                "nginx:1.28@sha256:b",
                "nginx:1.29@sha256:c",
                "base:debian:13",
            ],
            false,
        );
        covered.held_digests = 0;
        let unchanged = plan_digest(APPLICATION_MORPH, 7, 1, Some(&covered), &current, true);
        assert!(unchanged.new_fixable.is_empty() && unchanged.new_candidates.is_empty());
        assert!(unchanged.mark.add_ids.is_empty() && unchanged.mark.add_digest_ids.is_empty());
    }

    #[test]
    fn candidates_are_kept_while_a_version_check_failed() {
        // "keeps the notified candidates while a version check has not run" and "keeps notified
        // IDs while a deployment of the application runs"
        let mut current = current_of(&[finding("CVE-2026-0002")], Some(&rebuild()));
        current.candidates_checked = false;
        let stored = state(&[], &[], &["nginx:1.28@sha256:b"], false);
        let unchecked = plan(Some(&stored), &current, true);
        assert!(unchecked.mark.prune_ids);
        assert!(!unchecked.mark.prune_candidates);
        current.candidates_checked = true;
        let deploying = plan(Some(&stored), &current, false);
        assert!(!deploying.mark.prune_ids && !deploying.mark.prune_candidates);
    }

    #[test]
    fn a_digest_removes_what_no_current_scan_has_any_more() {
        // "notifies again when a fixed problem comes back"
        let current = current_of(&[finding("CVE-2026-0002")], Some(&rebuild()));
        let stored = state(
            &["CVE-2026-0001"],
            &["CVE-2026-0001", "CVE-2026-0002"],
            &["nginx:1.28@sha256:b"],
            false,
        );
        let mut mark = plan(Some(&stored), &current, true).mark;
        mark.check_reported = Some(false);
        mark.held_digests = Some(0);
        let update = applied_digest(&stored, &mark);
        assert_eq!(update.is_baseline, Some(false));
        assert_eq!(update.notified_ids, Some(Vec::new()));
        assert_eq!(update.digest_ids, Some(strings(&["CVE-2026-0002"])));
        assert_eq!(update.notified_candidates, Some(Vec::new()));
        assert_eq!(
            (update.check_reported, update.held_digests),
            (Some(false), Some(0))
        );

        // While a deployment runs, or an image of a Compose app has no result, nothing is removed.
        let kept = applied_digest(&stored, &plan(Some(&stored), &current, false).mark);
        assert_eq!(kept.notified_ids, Some(strings(&["CVE-2026-0001"])));
        assert_eq!(
            kept.digest_ids,
            Some(strings(&["CVE-2026-0001", "CVE-2026-0002"]))
        );
        assert_eq!(
            kept.notified_candidates,
            Some(strings(&["nginx:1.28@sha256:b"]))
        );
        // Null keeps the stored value.
        assert_eq!((kept.check_reported, kept.held_digests), (None, None));
    }

    #[test]
    fn a_digest_keeps_what_an_alert_added_while_it_ran() {
        // "keeps the IDs of an alert that comes while the digest runs"
        let current = current_of(&[finding("CVE-2026-0002")], Some(&rebuild()));
        let read = state(&["CVE-2026-0001"], &[], &["old@sha256:a"], false);
        let mark = plan(Some(&read), &current, true).mark;
        // Meanwhile an alert named CVE-2026-0007 (not in the scans the digest read), and a
        // candidate was recorded.
        let meanwhile = state(
            &["CVE-2026-0001", "CVE-2026-0007"],
            &["CVE-2026-0007"],
            &["old@sha256:a", "new@sha256:b"],
            false,
        );
        let update = applied_digest(&meanwhile, &mark);
        assert_eq!(update.notified_ids, Some(strings(&["CVE-2026-0007"])));
        assert_eq!(
            update.digest_ids,
            Some(strings(&["CVE-2026-0007", "CVE-2026-0002"]))
        );
        assert_eq!(update.notified_candidates, Some(strings(&["new@sha256:b"])));
    }

    #[test]
    fn a_baseline_plan_is_not_recorded_when_the_baseline_ended_meanwhile() {
        // "does not record a baseline plan when the baseline ended while the digest ran"
        let current = current_of(
            &[exploited("CVE-2026-0001"), finding("CVE-2026-0002")],
            Some(&rebuild()),
        );
        let baseline = state(&[], &[], &[], true);
        let mark = plan(Some(&baseline), &current, true).mark;
        assert_eq!(mark.add_digest_ids.len(), 2);

        let ended = state(&[], &[], &[], false);
        let update = applied_digest(&ended, &mark);
        assert_eq!(update.notified_ids, Some(Vec::new()));
        assert_eq!(update.digest_ids, Some(Vec::new()));

        // The baseline that is still open ends with the digest and records what it saw.
        let update = applied_digest(&baseline, &mark);
        assert_eq!(update.is_baseline, Some(false));
        assert_eq!(update.notified_ids, Some(strings(&["CVE-2026-0001"])));
        assert_eq!(
            update.digest_ids,
            Some(strings(&["CVE-2026-0001", "CVE-2026-0002"]))
        );
    }

    #[test]
    fn a_digest_creates_the_state_of_an_owner_without_one() {
        let mut current = current_of(&[exploited("CVE-2026-0001")], Some(&rebuild()));
        current.candidates = vec![("nginx:1.28@sha256:b".to_owned(), IdSet::new())];
        let mut mark = plan(None, &current, true).mark;
        let new = first_digest_state(&mark);
        assert!(!new.is_baseline);
        assert_eq!(new.notified_ids, strings(&["CVE-2026-0001"]));
        assert_eq!(new.digest_ids, strings(&["CVE-2026-0001"]));
        assert_eq!(new.notified_candidates, strings(&["nginx:1.28@sha256:b"]));
        assert_eq!((new.check_reported, new.held_digests), (false, 0));
        assert_eq!(new.baseline_deployment_queue_id, None);
        mark.check_reported = Some(true);
        mark.held_digests = Some(2);
        let new = first_digest_state(&mark);
        assert_eq!((new.check_reported, new.held_digests), (true, 2));
    }
}
