//! `App\Services\ImageScan\TeamVulnerabilities`: the resources of a team and their stored
//! security status, for the team Security page and the dashboard callout (spec 15.8). Lists read
//! only stored statuses; only the search decodes findings.
//!
//! Upstream loads only the resources the user can see (the resource models hide the others for
//! members whose access is limited to some projects). Here [`TeamVulnerabilities::load`] loads
//! every resource of the team: the caller keeps the entries the user can read
//! (`AuthCtx::can_resource(AccessLevel::Read, environment_id, uuid)` in coolify-api), with
//! [`TeamVulnerabilities::retain`], before it shows, searches, or exports them.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};

use chrono::NaiveDateTime;
use coolify_db::databases::Kind;
use coolify_db::image_scans::views::{self as queries, Place, ServerBrief};
use coolify_db::image_scans::{
    self, APPLICATION_MORPH, ImageScanIgnoreRow, ImageScanRow, SERVICE_APPLICATION_MORPH,
    SERVICE_DATABASE_MORPH, SERVICE_MORPH, ScanOwner,
};
use serde::Serialize;
use serde_json::Value;

use super::super::finding::Finding;
use super::super::rules::{IgnoreMatcher, RelevanceRules};
use super::super::{policy, resource_scans};
use super::actions;
use super::resource_security::ResourceSecurity;
use super::{SecurityStatusType, iso8601, sentences, status};
use crate::Jobs;

/// The sort order of the team page.
pub const STATUS_ORDER: [&str; 6] = [
    "update_now",
    "update_available",
    "no_fix",
    "check_needed",
    "ok",
    "not_checked",
];

/// The fix state of a search match, in the words of the resource page.
pub const STATE_LABELS: [(&str, &str); 3] = [
    ("fixed", "Fix available upstream"),
    ("no_fix", "No fix yet"),
    ("ignored", "Ignored"),
];

/// At most this many scans have their findings decoded in one request.
pub const MAX_DECODED_SCANS: usize = 200;

/// The Action column of a resource that has a problem with no fix yet (no_fix).
pub const NO_FIX_ACTION: &str = "No fix released yet. Coolify checks daily.";

fn err(e: coolify_db::DbError) -> String {
    e.to_string()
}

/// `STATE_LABELS[$state] ?? $state`.
pub fn state_label(state: &str) -> String {
    STATE_LABELS
        .iter()
        .find(|(k, _)| *k == state)
        .map_or_else(|| state.to_owned(), |(_, label)| (*label).to_owned())
}

fn state_rank(state: &str) -> usize {
    match state {
        "fixed" => 0,
        "no_fix" => 1,
        _ => 2,
    }
}

fn severity_rank(severity: &str) -> i64 {
    match severity {
        "critical" => 4,
        "high" => 3,
        "medium" => 2,
        "low" => 1,
        _ => 0,
    }
}

/// The images of a resource that the status counts, and how many of them need an update.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ImageCounts {
    pub update: i64,
    pub total: i64,
}

/// One resource of the team page.
#[derive(Debug, Clone)]
pub struct TeamEntry {
    /// `ResourceScans::key($resource)`.
    pub key: String,
    /// The application, service, or standalone database.
    pub resource: ScanOwner,
    /// Its scan owners (the applications and databases of a service, else the resource).
    pub owners: Vec<ScanOwner>,
    /// `ImageScanPolicyResolver::mainServerOf($resource)`.
    pub server: Option<ServerBrief>,
    /// Its project and environment.
    pub place: Option<Place>,
    /// The path of its Security page ([`vulnerabilities_path`]).
    pub link: Option<String>,
    pub scanned: bool,
    /// The current scans of its owners, without `findings` and `candidates`.
    pub scans: Vec<ImageScanRow>,
    pub security: ResourceSecurity,
    pub images: ImageCounts,
}

impl TeamEntry {
    /// `$resource->environment_id`, for the access check of the caller.
    pub fn environment_id(&self) -> Option<i64> {
        self.resource
            .record
            .get("environment_id")
            .and_then(Value::as_i64)
    }

    /// `application`, `service`, or `database`.
    pub fn resource_type(&self) -> &'static str {
        resource_type(&self.resource)
    }
}

/// `application`, `service`, or `database` of a resource page.
pub fn resource_type(resource: &ScanOwner) -> &'static str {
    if resource.is_application() {
        "application"
    } else if resource.is_service() {
        "service"
    } else {
        "database"
    }
}

/// The best search match of an entry: the most actionable finding (fixed by an update, no fix
/// yet, then ignored), and how many more the entry has.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchMatch {
    pub id: String,
    pub package: String,
    pub version: String,
    pub fixed_in: Option<String>,
    pub severity: String,
    /// `fixed`, `no_fix`, or `ignored`.
    pub state: String,
    pub more: usize,
}

/// `TeamVulnerabilities`.
#[derive(Debug)]
pub struct TeamVulnerabilities {
    pub team_id: i64,
    pub entries: Vec<TeamEntry>,
    search_capped: AtomicBool,
}

impl TeamVulnerabilities {
    /// `TeamVulnerabilities::load($teamId)`.
    pub async fn load(jobs: &Jobs, team_id: i64) -> Result<Self, String> {
        let db = &jobs.0.db;
        let resources = queries::team_resources(db, team_id).await.map_err(err)?;
        let mut scan_on = Vec::new();
        for resource in &resources {
            if policy::is_scan_on(db, resource).await? {
                scan_on.push(resource.clone());
            }
        }
        let owners = resource_scans::owners_of_many(db, &scan_on).await?;
        let flat: Vec<ScanOwner> = owners.values().flatten().cloned().collect();
        let scans = resource_scans::current_for_many(db, &flat, true).await?;
        let statuses = status::stored_many(jobs, &resources, Some(&scans)).await?;

        let mut environment_ids: Vec<i64> = resources
            .iter()
            .filter_map(|r| r.record.get("environment_id").and_then(Value::as_i64))
            .collect();
        environment_ids.sort_unstable();
        environment_ids.dedup();
        let places = queries::places(db, &environment_ids).await.map_err(err)?;
        let mut server_ids: Vec<i64> = resources.iter().filter_map(|r| r.main_server_id).collect();
        server_ids.sort_unstable();
        server_ids.dedup();
        let servers = queries::servers_brief(db, &server_ids).await.map_err(err)?;

        let entries = resources
            .into_iter()
            .map(|resource| {
                let key = resource.key();
                let resource_owners = owners.get(&key).cloned().unwrap_or_default();
                let resource_scans: Vec<ImageScanRow> = resource_owners
                    .iter()
                    .flat_map(|owner| scans.get(&owner.key()).cloned().unwrap_or_default())
                    .collect();
                let security = statuses
                    .get(&key)
                    .cloned()
                    .unwrap_or_else(|| ResourceSecurity::not_checked(status::NOT_SCANNED));
                let place = resource
                    .record
                    .get("environment_id")
                    .and_then(Value::as_i64)
                    .and_then(|id| places.iter().find(|p| p.environment_id == id))
                    .cloned();
                let server = resource
                    .main_server_id
                    .and_then(|id| servers.iter().find(|s| s.id == id))
                    .cloned();
                let images = image_counts(&resource, &resource_owners, &scans, &security);
                TeamEntry {
                    link: place
                        .as_ref()
                        .map(|place| vulnerabilities_path(&resource, place)),
                    scanned: !resource_scans.is_empty(),
                    scans: resource_scans,
                    key,
                    resource,
                    owners: resource_owners,
                    server,
                    place,
                    security,
                    images,
                }
            })
            .collect();
        Ok(Self {
            team_id,
            entries,
            search_capped: AtomicBool::new(false),
        })
    }

    /// Keeps the entries the user can see (see the module documentation).
    pub fn retain(&mut self, visible: impl FnMut(&TeamEntry) -> bool) {
        self.entries.retain(visible);
    }

    /// `search($entries, $term, $maxScans)`: the entries that have a finding with this ID,
    /// alias, or package, each with the state of its most actionable match: fixed by an
    /// update, no fix yet, then ignored. At most `max_scans` matching scans are decoded; see
    /// [`Self::search_was_capped`].
    pub async fn search(
        &self,
        jobs: &Jobs,
        entries: &[TeamEntry],
        term: &str,
        max_scans: usize,
    ) -> Result<Vec<(TeamEntry, SearchMatch)>, String> {
        let db = &jobs.0.db;
        self.search_capped.store(false, AtomicOrdering::Relaxed);
        let term = search_term(term);
        let scan_ids: Vec<i64> = entries
            .iter()
            .flat_map(|e| e.scans.iter().map(|s| s.id))
            .collect();
        if term.is_empty() || scan_ids.is_empty() {
            return Ok(Vec::new());
        }

        // The database narrows the scans first, with "!" as the LIKE escape. jsonb text keeps
        // "/" as is, while a JSON string written by PHP has "\/", so "/" matches any run of
        // characters.
        let pattern = format!("%{}%", like_escape(&term));
        let mut found = queries::search_findings(db, &scan_ids, &pattern, max_scans as i64 + 1)
            .await
            .map_err(err)?;
        if found.len() > max_scans {
            self.search_capped.store(true, AtomicOrdering::Relaxed);
            found.truncate(max_scans);
        }
        let found_ids: Vec<i64> = found.iter().map(|(id, _)| *id).collect();
        let full: HashMap<i64, ImageScanRow> = image_scans::scans_by_ids(db, &found_ids, false)
            .await
            .map_err(err)?
            .into_iter()
            .map(|s| (s.id, s))
            .collect();
        let now = Jobs::now();
        let ignores = image_scans::active_ignores_of_team(db, self.team_id, now)
            .await
            .map_err(err)?;
        let resources: Vec<ScanOwner> = entries.iter().map(|e| e.resource.clone()).collect();
        let count_medium = policy::count_medium_for_many(db, self.team_id, &resources).await?;

        let mut results = Vec::new();
        for entry in entries {
            let mut matches: Vec<SearchMatch> = Vec::new();
            for scan in &entry.scans {
                let Some(loaded) = full.get(&scan.id) else {
                    continue;
                };
                let Some(owner) = entry.owners.iter().find(|o| o.key() == scan.owner_key()) else {
                    continue;
                };
                let rules = RelevanceRules::new(
                    matcher_for(Some(owner), &ignores, now),
                    count_medium.get(&entry.key).copied().unwrap_or(false),
                );
                let classify = actions::fix_states(jobs, loaded, owner, &rules).await?;
                for finding in resource_scans::findings(loaded) {
                    if !matches_term(&finding, &term) {
                        continue;
                    }
                    let state = if rules.ignores.matches(&finding).is_some() {
                        "ignored".to_owned()
                    } else {
                        classify.state(&finding).value().to_owned()
                    };
                    matches.push(SearchMatch {
                        id: finding.id.clone(),
                        package: finding.package.clone(),
                        version: finding.version.clone(),
                        fixed_in: finding.fixed_in.clone(),
                        severity: severity_of(&finding),
                        state,
                        more: 0,
                    });
                }
            }
            if matches.is_empty() {
                continue;
            }
            matches.sort_by(|a, b| {
                state_rank(&a.state)
                    .cmp(&state_rank(&b.state))
                    .then_with(|| severity_rank(&b.severity).cmp(&severity_rank(&a.severity)))
                    .then_with(|| a.id.cmp(&b.id))
            });
            let more = matches.len() - 1;
            let mut best = matches.swap_remove(0);
            best.more = more;
            results.push((entry.clone(), best));
        }
        results.sort_by(|(a, am), (b, bm)| {
            state_rank(&am.state)
                .cmp(&state_rank(&bm.state))
                .then_with(|| severity_rank(&bm.severity).cmp(&severity_rank(&am.severity)))
                .then_with(|| strnatcasecmp(&a.resource.name(), &b.resource.name()))
        });
        Ok(results)
    }

    /// Whether the last search found more matching scans than it reads, so some resources may
    /// be missing.
    pub fn search_was_capped(&self) -> bool {
        self.search_capped.load(AtomicOrdering::Relaxed)
    }
}

/// `strtr($term, ['!' => '!!', '%' => '!%', '_' => '!_', '/' => '%'])`.
fn like_escape(term: &str) -> String {
    let mut out = String::with_capacity(term.len());
    for c in term.chars() {
        match c {
            '!' => out.push_str("!!"),
            '%' => out.push_str("!%"),
            '_' => out.push_str("!_"),
            '/' => out.push('%'),
            c => out.push(c),
        }
    }
    out
}

/// `strnatcasecmp($a, $b)`: digits compare as numbers, letters without case.
fn strnatcasecmp(a: &str, b: &str) -> Ordering {
    let a = a.to_lowercase();
    let b = b.to_lowercase();
    let mut ai = a.chars().peekable();
    let mut bi = b.chars().peekable();
    loop {
        match (ai.peek().copied(), bi.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let mut xs = String::new();
                while let Some(c) = ai.peek().copied().filter(char::is_ascii_digit) {
                    xs.push(c);
                    ai.next();
                }
                let mut ys = String::new();
                while let Some(c) = bi.peek().copied().filter(char::is_ascii_digit) {
                    ys.push(c);
                    bi.next();
                }
                let xt = xs.trim_start_matches('0');
                let yt = ys.trim_start_matches('0');
                let ord = xt.len().cmp(&yt.len()).then_with(|| xt.cmp(yt));
                if ord != Ordering::Equal {
                    return ord;
                }
            }
            (Some(x), Some(y)) => {
                if x != y {
                    return x.cmp(&y);
                }
                ai.next();
                bi.next();
            }
        }
    }
}

/// `TeamVulnerabilities::imageCounts()`.
fn image_counts(
    resource: &ScanOwner,
    owners: &[ScanOwner],
    scans: &HashMap<String, Vec<ImageScanRow>>,
    security: &ResourceSecurity,
) -> ImageCounts {
    let mut update = 0;
    let mut total = 0;
    for owner in owners {
        if resource.is_service() && !resource_scans::is_scan_target(owner) {
            continue;
        }
        let owner_scans = scans
            .get(&owner.key())
            .map(Vec::as_slice)
            .unwrap_or_default();
        total += owner_scans.len().max(1) as i64;
        update += owner_scans
            .iter()
            .filter(|scan| {
                matches!(
                    scan.summary
                        .as_ref()
                        .and_then(|s| s.pointer("/security/status"))
                        .and_then(Value::as_str),
                    Some("update_now" | "update_available")
                )
            })
            .count() as i64;
    }
    let context_total = super::action::php_int(security.check_context.get("total"));
    ImageCounts {
        update,
        total: total.max(context_total),
    }
}

/// `TeamVulnerabilities::imagesLine()`: "2 of 3 images" under the status of a resource with
/// several images that needs an update. `None` otherwise.
pub fn images_line(security: &ResourceSecurity, images: ImageCounts) -> Option<String> {
    if !security.status.needs_update() || images.total < 2 || images.update < 1 {
        return None;
    }
    Some(format!("{} of {} images", images.update, images.total))
}

/// `TeamVulnerabilities::statusLabel()`: the badge, "No action needed" for ok, "Not checked"
/// for not_checked.
pub fn status_label(status: SecurityStatusType) -> String {
    match status {
        SecurityStatusType::Ok => "No action needed".to_owned(),
        SecurityStatusType::NotChecked => "Not checked".to_owned(),
        other => sentences::badge(other).unwrap_or_default().to_owned(),
    }
}

/// The Action column of the team page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ActionText {
    pub text: String,
    pub link: bool,
    pub muted: bool,
}

/// `TeamVulnerabilities::action()`: the sentence of the primary action (update_now,
/// update_available), "Check again: {reason}." (check_needed, muted), that no fix exists yet
/// (no_fix, muted, no link), or why the resource is not checked (not_checked, muted, no link).
/// `None` for ok.
pub fn action(security: &ResourceSecurity, now: NaiveDateTime) -> Option<ActionText> {
    match security.status {
        SecurityStatusType::NoFix => Some(ActionText {
            text: sentences::no_fix_action(security)
                .unwrap_or(NO_FIX_ACTION)
                .to_owned(),
            link: false,
            muted: true,
        }),
        SecurityStatusType::UpdateNow | SecurityStatusType::UpdateAvailable => security
            .primary()
            .map(|a| a.sentence.clone())
            .filter(|s| !s.trim().is_empty())
            .map(|text| ActionText {
                text,
                link: true,
                muted: false,
            }),
        SecurityStatusType::CheckNeeded => Some(ActionText {
            text: format!("Check again: {}.", check_reason(security, now)),
            link: true,
            muted: true,
        }),
        SecurityStatusType::NotChecked => Some(ActionText {
            text: sentences::headline(security, now),
            link: false,
            muted: true,
        }),
        SecurityStatusType::Ok => None,
    }
}

/// The check_needed reason, with the time of the oldest result for a stale one.
fn check_reason(security: &ResourceSecurity, now: NaiveDateTime) -> String {
    let mut context = security.check_context.clone();
    if context.get("time").is_none_or(Value::is_null)
        && let Some(at) = security.checked_at
    {
        context.insert("time".to_owned(), Value::String(iso8601(at)));
    }
    match &security.check_reason {
        None => "the last check is incomplete".to_owned(),
        Some(reason) => sentences::check_reason(reason, &context, now),
    }
}

/// `TeamVulnerabilities::updateNowResources($teamId)`: the UUIDs of the resources of the team
/// that need a security update now (update_now), sorted. Starts from the current scans whose
/// stored status has a fixed finding with an exploit signal. The caller keeps the ones the user
/// can see.
pub async fn update_now_resources(jobs: &Jobs, team_id: i64) -> Result<Vec<ScanOwner>, String> {
    let db = &jobs.0.db;
    let owners = queries::completed_owner_keys_of_team(db, team_id)
        .await
        .map_err(err)?;
    let current_ids = image_scans::current_scan_ids(db, &owners)
        .await
        .map_err(err)?;
    if current_ids.is_empty() {
        return Ok(Vec::new());
    }

    // A resource is update_now only when one of its images has a fixed finding with an exploit
    // signal.
    let mut candidates: Vec<(String, Vec<i64>)> = Vec::new();
    for scan in image_scans::scans_by_ids(db, &current_ids, true)
        .await
        .map_err(err)?
    {
        let signal_fixed = super::action::php_int(
            scan.summary
                .as_ref()
                .and_then(|s| s.pointer("/security/facts/counts/signal_fixed")),
        );
        if signal_fixed <= 0 {
            continue;
        }
        match candidates
            .iter_mut()
            .find(|(t, _)| *t == scan.resource_type)
        {
            Some((_, ids)) => {
                if !ids.contains(&scan.resource_id) {
                    ids.push(scan.resource_id);
                }
            }
            None => candidates.push((scan.resource_type.clone(), vec![scan.resource_id])),
        }
    }

    let resource_morphs: Vec<&'static str> = std::iter::once(APPLICATION_MORPH)
        .chain(std::iter::once(SERVICE_MORPH))
        .chain(Kind::ALL.into_iter().map(Kind::morph))
        .collect();
    let mut resource_ids: Vec<(&'static str, Vec<i64>)> = Vec::new();
    let mut add =
        |morph: &'static str, id: i64| match resource_ids.iter_mut().find(|(m, _)| *m == morph) {
            Some((_, ids)) => {
                if !ids.contains(&id) {
                    ids.push(id);
                }
            }
            None => resource_ids.push((morph, vec![id])),
        };
    for (resource_type, ids) in candidates {
        if resource_type == SERVICE_APPLICATION_MORPH || resource_type == SERVICE_DATABASE_MORPH {
            for id in ids {
                if let Some(service_id) = image_scans::find_owner(db, &resource_type, id)
                    .await
                    .map_err(err)?
                    .and_then(|o| o.service_id)
                {
                    add(SERVICE_MORPH, service_id);
                }
            }
        } else if let Some(morph) = resource_morphs.iter().find(|m| **m == resource_type) {
            for id in ids {
                add(morph, id);
            }
        }
    }

    let mut resources = Vec::new();
    for (morph, ids) in resource_ids {
        resources.extend(
            queries::team_resources_by_ids(db, morph, &ids, team_id)
                .await
                .map_err(err)?,
        );
    }
    let statuses = status::stored_many(jobs, &resources, None).await?;
    let mut out: Vec<ScanOwner> = resources
        .into_iter()
        .filter(|r| {
            statuses
                .get(&r.key())
                .is_some_and(|s| s.status == SecurityStatusType::UpdateNow)
        })
        .collect();
    out.sort_by(|a, b| a.uuid.cmp(&b.uuid));
    Ok(out)
}

/// `TeamVulnerabilities::serversNotScanning($teamId)`: servers of the team that do not scan
/// images. A transferred server is an archive and is not counted.
pub async fn servers_not_scanning(jobs: &Jobs, team_id: i64) -> Result<usize, String> {
    let db = &jobs.0.db;
    let flags = queries::team_servers(db, team_id).await.map_err(err)?;
    let rows = coolify_db::servers::list(db, team_id).await.map_err(err)?;
    Ok(rows
        .iter()
        .filter(|row| !crate::server::ServerInfo::from_row(row).transferred)
        .filter(|row| {
            flags
                .iter()
                .find(|f| f.id == row.id)
                .and_then(|f| f.is_image_scan_enabled)
                != Some(true)
        })
        .count())
}

/// `TeamVulnerabilities::searchTerm()`: lower case, at most 100 characters, without quotes and
/// backslashes.
pub fn search_term(term: &str) -> String {
    let cleaned: String = term.chars().filter(|c| *c != '"' && *c != '\\').collect();
    cleaned.trim().to_lowercase().chars().take(100).collect()
}

/// `TeamVulnerabilities::severityOf()`.
pub fn severity_of(finding: &Finding) -> String {
    let severity = finding.severity.to_lowercase();
    match severity.as_str() {
        "critical" | "high" | "medium" | "low" | "unknown" => severity,
        _ => "unknown".to_owned(),
    }
}

fn matches_term(finding: &Finding, term: &str) -> bool {
    finding
        .ids()
        .chain(std::iter::once(finding.package.as_str()))
        .any(|value| value.to_lowercase().contains(term))
}

/// `matcherFor($owner, $ignores)`: the team-wide ignores and the ones of the owner.
fn matcher_for(
    owner: Option<&ScanOwner>,
    ignores: &[ImageScanIgnoreRow],
    now: NaiveDateTime,
) -> IgnoreMatcher {
    IgnoreMatcher::from_ignores(
        ignores
            .iter()
            .filter(|ignore| match &ignore.resource_type {
                None => true,
                Some(resource_type) => owner
                    .is_some_and(|o| resource_type == o.morph && ignore.resource_id == Some(o.id)),
            })
            .cloned()
            .collect(),
        now,
    )
}

/// The path of the Security page of a resource (`route('project.{type}.vulnerabilities')`).
pub fn vulnerabilities_path(resource: &ScanOwner, place: &Place) -> String {
    format!(
        "/project/{}/environment/{}/{}/{}/vulnerabilities",
        place.project_uuid,
        place.environment_uuid,
        resource_type(resource),
        resource.uuid
    )
}

/// `TeamVulnerabilities::vulnerabilitiesUrl($resource)`: the path of the Security page of a
/// resource with a page (application, service, standalone database); `None` without an
/// environment or project.
pub async fn vulnerabilities_url(
    jobs: &Jobs,
    resource: &ScanOwner,
) -> Result<Option<String>, String> {
    let Some(environment_id) = resource
        .record
        .get("environment_id")
        .and_then(Value::as_i64)
    else {
        return Ok(None);
    };
    let places = queries::places(&jobs.0.db, &[environment_id])
        .await
        .map_err(err)?;
    Ok(places
        .first()
        .map(|place| vulnerabilities_path(resource, place)))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::super::super::resource_scans::tests::owner;
    use super::*;

    #[test]
    fn labels_and_terms() {
        assert_eq!(status_label(SecurityStatusType::Ok), "No action needed");
        assert_eq!(status_label(SecurityStatusType::NotChecked), "Not checked");
        assert_eq!(status_label(SecurityStatusType::UpdateNow), "Update now");
        assert_eq!(search_term("  \"CVE-2024\\-1\"  "), "cve-2024-1");
        assert_eq!(search_term(&"a".repeat(150)).len(), 100);
        assert_eq!(like_escape("a_b%c!d/e"), "a!_b!%c!!d%e");
        assert_eq!(state_label("fixed"), "Fix available upstream");
        assert_eq!(state_label("other"), "other");
    }

    #[test]
    fn natural_order() {
        assert_eq!(strnatcasecmp("app2", "App10"), Ordering::Less);
        assert_eq!(strnatcasecmp("b", "A"), Ordering::Greater);
        assert_eq!(strnatcasecmp("x", "x"), Ordering::Equal);
    }

    #[test]
    fn the_action_column() {
        let mut security = ResourceSecurity::new(SecurityStatusType::NoFix);
        let now = super::super::parse_time("2026-11-10T12:00:00+00:00").unwrap();
        assert_eq!(action(&security, now).unwrap().text, NO_FIX_ACTION);
        security.status = SecurityStatusType::CheckNeeded;
        security.check_reason = Some(status::REASON_DB_OUTDATED.to_owned());
        assert_eq!(
            action(&security, now).unwrap().text,
            "Check again: the vulnerability database is out of date."
        );
        security.status = SecurityStatusType::Ok;
        assert_eq!(action(&security, now), None);
        security.status = SecurityStatusType::UpdateNow;
        assert_eq!(action(&security, now), None);
        assert_eq!(
            images_line(
                &security,
                ImageCounts {
                    update: 1,
                    total: 3
                }
            )
            .as_deref(),
            Some("1 of 3 images")
        );
    }

    #[test]
    fn paths_of_the_security_pages() {
        let place = Place {
            environment_id: 1,
            environment_uuid: "e".to_owned(),
            environment_name: "production".to_owned(),
            project_id: 1,
            project_uuid: "p".to_owned(),
            project_name: "Acme".to_owned(),
        };
        let app = owner(APPLICATION_MORPH, json!({}));
        assert_eq!(
            vulnerabilities_path(&app, &place),
            "/project/p/environment/e/application/appuuid/vulnerabilities"
        );
        let database = owner(r"App\Models\StandaloneRedis", json!({}));
        assert_eq!(
            vulnerabilities_path(&database, &place),
            "/project/p/environment/e/database/appuuid/vulnerabilities"
        );
    }
}
