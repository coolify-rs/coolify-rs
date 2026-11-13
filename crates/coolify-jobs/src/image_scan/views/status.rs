//! `App\Services\ImageScan\SecurityStatus`: decides one security status per image and per
//! resource (spec 15.6) from stored data only: the findings, the version and base checks, the
//! scan times, and the resource's configuration. No remote calls.
//!
//! Each completed scan that is not a preview stores its status in `summary.security`
//! ([`store`]), written by the scan and by every summary refresh. [`stored`] combines the stored
//! statuses of the current scans of a resource without reading the findings: it adds the counts
//! and the per-action data, merges the actions (and, for a service, the updates of images that
//! share the current tag to one new tag), keeps the check reasons that do not depend on the time
//! (database age, running image, version check), adds the age of the oldest current result and
//! the images that are not checked, and then applies the rules again. [`for_resource`] does the
//! same with statuses computed now.
//!
//! Images that are not checked: an application or database of a service that Coolify scans
//! without a current scan, a current scan without a stored status ([`stored`] only), and the
//! images of a Compose application's current deployment without a completed result.

use std::cmp::Ordering;
use std::collections::HashMap;

use chrono::{Duration, NaiveDateTime};
use coolify_db::image_scans::{
    self, ImageScanRow, SERVICE_APPLICATION_MORPH, SERVICE_DATABASE_MORPH, ScanOwner,
};
use serde_json::{Map, Value, json};

use super::super::finding::Finding;
use super::super::image_ref::ImageRef;
use super::super::rules::{self, ExploitSignal, RelevanceRules};
use super::super::{policy, resource_scans};
use super::action::{ActionPart, SecurityAction, php_bool, php_int, value_string};
use super::actions::{self, COOLIFY_TEXT};
use super::resource_security::{Eol, ResourceSecurity, Why, list};
use super::{OwnedScan, SecurityActionType, SecurityStatusType, iso8601};
use crate::Jobs;

pub const STALE_AFTER_DAYS: i64 = 2;
pub const DATABASE_MAX_AGE_DAYS: i64 = 5;
pub const WHY_LIMIT: usize = 3;

pub const REASON_IMAGES_UNCHECKED: &str = "images_unchecked";
pub const REASON_STALE: &str = "stale";
pub const REASON_DB_OUTDATED: &str = "db_outdated";
pub const REASON_IMAGE_CHANGED: &str = "image_changed";
pub const REASON_VERSIONS_UNCHECKED: &str = "versions_unchecked";
pub const REASON_TRUNCATED: &str = "truncated";
pub const NOT_SCANNED: &str = "not_scanned";

/// The check_needed reasons in the order they are shown.
pub const REASON_ORDER: [&str; 6] = [
    REASON_IMAGES_UNCHECKED,
    REASON_TRUNCATED,
    REASON_STALE,
    REASON_DB_OUTDATED,
    REASON_IMAGE_CHANGED,
    REASON_VERSIONS_UNCHECKED,
];

/// Results of a version or base check that count as checked.
const CHECKED_STATUSES: [&str; 3] = ["ok", "no_better", "cannot_compare"];

/// The keys of `facts.counts`, in their stored order.
pub const COUNT_KEYS: [&str; 9] = [
    "signal_fixed",
    "signal_unfixed",
    "kev_fixed",
    "kev_unfixed",
    "critical_fixed",
    "high_fixed",
    "medium_fixed",
    "relevant_fixed",
    "builder_unfixed",
];

fn err(e: coolify_db::DbError) -> String {
    e.to_string()
}

// ---------------------------------------------------------------------------------------------
// Counts and per-action data
// ---------------------------------------------------------------------------------------------

/// `facts.counts`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Counts([i64; 9]);

impl Counts {
    fn index(key: &str) -> usize {
        COUNT_KEYS.iter().position(|k| *k == key).unwrap_or(0)
    }

    fn get(&self, key: &str) -> i64 {
        self.0[Self::index(key)]
    }

    fn add(&mut self, key: &str, n: i64) {
        self.0[Self::index(key)] += n;
    }

    fn to_value(self) -> Value {
        Value::Object(
            COUNT_KEYS
                .iter()
                .zip(self.0)
                .map(|(k, v)| ((*k).to_owned(), json!(v)))
                .collect(),
        )
    }
}

/// What one action fixes: owner (name, morph type, key), relevant fixes with an exploit signal,
/// `exploited` ones, critical, high, and medium ones, all relevant ones, an end-of-life OS it
/// leaves (counted in `critical` and `relevant` too), and its why-list.
#[derive(Debug, Clone, Default, PartialEq)]
struct Stats {
    owner: Option<String>,
    owner_type: Option<String>,
    owner_id: Value,
    signal: i64,
    kev: i64,
    critical: i64,
    high: i64,
    medium: i64,
    relevant: i64,
    eol: i64,
    why: Vec<Why>,
}

impl Stats {
    fn to_value(&self) -> Value {
        json!({
            "owner": self.owner,
            "owner_type": self.owner_type,
            "owner_id": self.owner_id,
            "signal": self.signal,
            "kev": self.kev,
            "critical": self.critical,
            "high": self.high,
            "medium": self.medium,
            "relevant": self.relevant,
            "eol": self.eol,
            "why": self.why.iter().map(Why::to_value).collect::<Vec<_>>(),
        })
    }

    /// `SecurityStatus::stats($data)`.
    fn from_value(data: Option<&Value>) -> Self {
        let empty = Map::new();
        let data = data.and_then(Value::as_object).unwrap_or(&empty);
        let isset = |key: &str| data.get(key).filter(|v| !v.is_null()).map(value_string);
        let owner_id = match data.get("owner_id") {
            Some(v @ Value::String(_)) => v.clone(),
            Some(Value::Number(n)) if n.is_i64() || n.is_u64() => Value::Number(n.clone()),
            _ => Value::Null,
        };
        let int = |key: &str| php_int(data.get(key));
        Self {
            owner: isset("owner"),
            owner_type: isset("owner_type"),
            owner_id,
            signal: int("signal"),
            kev: int("kev"),
            critical: int("critical"),
            high: int("high"),
            medium: int("medium"),
            relevant: int("relevant"),
            eol: int("eol"),
            why: list(data.get("why")).iter().map(Why::from_value).collect(),
        }
    }
}

/// One action of a status with what it fixes.
#[derive(Debug, Clone)]
struct Entry {
    action: SecurityAction,
    stats: Stats,
}

// ---------------------------------------------------------------------------------------------
// Entry points
// ---------------------------------------------------------------------------------------------

/// `SecurityStatus::forScan($scan, $rules)`: the status of one scanned image, with the age of
/// the result. `rules` are the relevance rules of the scan's owner, when the caller has them.
pub async fn for_scan(
    jobs: &Jobs,
    scan: &ImageScanRow,
    owner: &ScanOwner,
    rules: Option<&RelevanceRules>,
) -> Result<ResourceSecurity, String> {
    if !scan.is_completed() {
        return Ok(ResourceSecurity::not_checked(NOT_SCANNED));
    }
    evaluate(jobs, scan, owner, rules).await
}

/// `SecurityStatus::forResource($resource, $currentScans, $imageStatuses)`: the status of a
/// resource from its current scans, computed now. A service combines the images of its
/// applications and databases. A caller that loaded the current scans of the owners passes
/// them, and the statuses of [`for_scan`] it computed already, keyed by scan UUID.
pub async fn for_resource(
    jobs: &Jobs,
    resource: &ScanOwner,
    current_scans: Option<&[OwnedScan]>,
    image_statuses: &HashMap<String, ResourceSecurity>,
) -> Result<ResourceSecurity, String> {
    let db = &jobs.0.db;
    if let Some(off) = policy::off_state(db, resource, None).await? {
        return Ok(ResourceSecurity::not_checked(off));
    }

    let owners = image_scans::owners(db, resource).await.map_err(err)?;
    let mut images = Vec::new();
    for owner in status_owners(resource, owners) {
        let scans: Vec<ImageScanRow> = match current_scans {
            None => resource_scans::current_for(db, &owner).await?,
            Some(all) => all
                .iter()
                .filter(|s| s.scan.resource_type == owner.morph && s.scan.resource_id == owner.id)
                .map(|s| s.scan.clone())
                .collect(),
        };
        let mut parts = Vec::new();
        for scan in &scans {
            parts.push(Some(match image_statuses.get(&scan.uuid) {
                Some(status) => status.clone(),
                None => evaluate(jobs, scan, &owner, None).await?,
            }));
        }
        let unchecked = resource_scans::unchecked_compose_images(db, &owner, &scans, None).await?;
        images.push((parts, unchecked));
    }
    Ok(combine_images(images, resource.is_service(), Jobs::now()))
}

/// `SecurityStatus::stored($resource)`: the status of a resource from the stored statuses of
/// its current scans. A scan without a stored status counts as an image that is not checked.
pub async fn stored(jobs: &Jobs, resource: &ScanOwner) -> Result<ResourceSecurity, String> {
    let mut all = stored_many(jobs, std::slice::from_ref(resource), None).await?;
    Ok(all
        .remove(&resource.key())
        .unwrap_or_else(|| ResourceSecurity::not_checked(NOT_SCANNED)))
}

/// `SecurityStatus::storedMany($resources, $scans)`: [`stored`] for many resources, keyed by
/// `ResourceScans::key()`. `scans` are the current scans of the owners of the scanned resources
/// (`resource_scans::current_for_many`, light is enough), when the caller has them.
pub async fn stored_many(
    jobs: &Jobs,
    resources: &[ScanOwner],
    scans: Option<&HashMap<String, Vec<ImageScanRow>>>,
) -> Result<HashMap<String, ResourceSecurity>, String> {
    let db = &jobs.0.db;
    if resources.is_empty() {
        return Ok(HashMap::new());
    }
    let mut off_states = HashMap::new();
    for resource in resources {
        off_states.insert(resource.key(), policy::off_state(db, resource, None).await?);
    }
    let scanned: Vec<ScanOwner> = resources
        .iter()
        .filter(|r| off_states.get(&r.key()).is_some_and(Option::is_none))
        .cloned()
        .collect();
    let owners = resource_scans::owners_of_many(db, &scanned).await?;
    let flat: Vec<ScanOwner> = owners.values().flatten().cloned().collect();
    let loaded;
    let scans = match scans {
        Some(scans) => scans,
        None => {
            loaded = resource_scans::current_for_many(db, &flat, true).await?;
            &loaded
        }
    };
    let compose_rows = resource_scans::compose_deployment_rows(db, &flat, scans).await?;
    let now = Jobs::now();

    let mut result = HashMap::new();
    for resource in resources {
        let key = resource.key();
        if let Some(Some(off)) = off_states.get(&key) {
            result.insert(key, ResourceSecurity::not_checked(off));
            continue;
        }
        let mut images = Vec::new();
        let resource_owners = owners.get(&key).cloned().unwrap_or_default();
        for owner in status_owners(resource, resource_owners) {
            let owner_scans = scans.get(&owner.key()).cloned().unwrap_or_default();
            let parts = owner_scans
                .iter()
                .map(|scan| {
                    scan.summary
                        .as_ref()
                        .and_then(|s| s.get("security"))
                        .filter(|s| s.is_object() || s.is_array())
                        .map(ResourceSecurity::from_value)
                })
                .collect();
            let unchecked = resource_scans::unchecked_compose_images(
                db,
                &owner,
                &owner_scans,
                compose_rows.get(&owner.key()).map(Vec::as_slice),
            )
            .await?;
            images.push((parts, unchecked));
        }
        result.insert(key, combine_images(images, resource.is_service(), now));
    }
    Ok(result)
}

/// `SecurityStatus::store($scan)`: adds the status of a completed scan to its summary (not
/// saved). Preview scans get none: their actions would compare with the scans of the
/// application. A failure to evaluate is reported and leaves the summary without a status.
pub async fn store(jobs: &Jobs, scan: &mut ImageScanRow, owner: &ScanOwner) -> Result<(), String> {
    if !scan.is_completed() {
        return Ok(());
    }
    let mut summary = match scan.summary.take() {
        Some(Value::Object(map)) => map,
        _ => Map::new(),
    };
    summary.shift_remove("security");
    let not_preview = match scan.deployment_queue_id {
        None => true,
        Some(id) => image_scans::deployment_facts(&jobs.0.db, id)
            .await
            .map_err(err)?
            .is_none_or(|d| d.pull_request_id == 0),
    };
    if not_preview {
        match evaluate(jobs, scan, owner, None).await {
            Ok(security) => {
                summary.insert("security".to_owned(), security.to_value());
            }
            Err(e) => {
                tracing::error!(scan_id = scan.id, error = %e, "SecurityStatus::store failed");
            }
        }
    }
    scan.summary = Some(super::resource_security::php_map(&summary));
    Ok(())
}

/// `SecurityStatus::whyFor($finding, $signal)`.
pub fn why_for(finding: &Finding, signal: Option<ExploitSignal>) -> Why {
    Why {
        title: finding.title.clone(),
        display_id: finding.display_id.clone(),
        link: resource_scans::advisory_url(&finding.display_id),
        score: finding.cvss_score,
        severity: finding.severity.to_lowercase(),
        signal: signal.map(|s| s.value().to_owned()),
        package: finding.package.clone(),
        version: finding.version.clone(),
    }
}

/// `SecurityStatus::topWhy($entries, $limit)`: `exploited`, then `likely`, then the highest
/// score (no score last), then severity, at most `limit`.
pub fn top_why(mut entries: Vec<Why>, limit: usize) -> Vec<Why> {
    let signal_rank = |why: &Why| match why.signal.as_deref() {
        Some("exploited") => 0,
        Some("likely") => 1,
        _ => 2,
    };
    let severity_rank = |why: &Why| match why.severity.as_str() {
        "critical" => 0,
        "high" => 1,
        "medium" => 2,
        "low" => 3,
        _ => 4,
    };
    entries.sort_by(|a, b| {
        signal_rank(a)
            .cmp(&signal_rank(b))
            .then_with(|| {
                let a_score = -a.score.unwrap_or(-1.0);
                let b_score = -b.score.unwrap_or(-1.0);
                a_score.partial_cmp(&b_score).unwrap_or(Ordering::Equal)
            })
            .then_with(|| severity_rank(a).cmp(&severity_rank(b)))
            .then_with(|| a.display_id.cmp(&b.display_id))
    });
    entries.truncate(limit);
    entries
}

/// `SecurityStatus::versionsChecked($scan, $owner, $blockedCounts)`: the version check (images
/// the user does not build) and the base check (Dockerfile apps with a base image) have a
/// result. `cannot_compare` and `no_better` count as checked, and so do the checks that only the
/// user can unblock ([`is_blocked_check`]). A Dockerfile app also needs to know which findings
/// come from its current base image. Notifications pass `blocked_counts` false.
pub fn versions_checked(scan: &ImageScanRow, owner: &ScanOwner, blocked_counts: bool) -> bool {
    let settled = |status: Option<&str>, error: Option<&Value>| {
        status.is_some_and(|s| CHECKED_STATUSES.contains(&s))
            || (blocked_counts && is_blocked_check(status, error))
    };
    let build_pack = owner.build_pack();
    let needs_image_check = !owner.is_application()
        || build_pack == Some("dockerimage")
        || (build_pack == Some("dockercompose") && !scan.builds_image);
    let candidates_error = scan.candidates.as_ref().and_then(|c| c.get("error"));
    if needs_image_check && !settled(scan.candidate_status(), candidates_error) {
        return false;
    }

    let base_ref = scan.base_image_ref.as_deref().unwrap_or_default();
    if owner.is_application() && build_pack == Some("dockerfile") && !base_ref.trim().is_empty() {
        let Some(base) = scan
            .candidates
            .as_ref()
            .and_then(|c| c.get("base"))
            .and_then(Value::as_object)
        else {
            return false;
        };
        return base.get("image").and_then(Value::as_str) == Some(base_ref)
            && settled(
                base.get("status").and_then(Value::as_str),
                base.get("error"),
            )
            && base.get("attributable").is_some_and(Value::is_i64);
    }
    true
}

/// `SecurityStatus::isBlockedCheck($status, $error)`: a version or base check that Coolify
/// cannot finish by itself: the registry needs a login (`auth_required`), or the server reached
/// the daily limit of checks (`failed` with `daily_limit`).
pub fn is_blocked_check(status: Option<&str>, error: Option<&Value>) -> bool {
    status == Some("auth_required")
        || (status == Some("failed") && error.and_then(Value::as_str) == Some("daily_limit"))
}

// ---------------------------------------------------------------------------------------------
// Rules
// ---------------------------------------------------------------------------------------------

/// The owners whose images count for the status: the applications and databases of a service
/// that Coolify scans, else the resource itself.
fn status_owners(resource: &ScanOwner, owners: Vec<ScanOwner>) -> Vec<ScanOwner> {
    if resource.is_service() {
        owners
            .into_iter()
            .filter(resource_scans::is_scan_target)
            .collect()
    } else {
        owners
    }
}

/// `SecurityStatus::combineImages()`: per scan owner, the status of each current scan (`None`:
/// no stored status) and the number of its images without a result. An owner without a current
/// scan is one image that is not checked.
fn combine_images(
    images: Vec<(Vec<Option<ResourceSecurity>>, usize)>,
    is_service: bool,
    now: NaiveDateTime,
) -> ResourceSecurity {
    let mut total = 0;
    let mut parts = Vec::new();
    for (owner_parts, unchecked) in images {
        total += owner_parts.len().max(1) + unchecked;
        parts.extend(owner_parts.into_iter().flatten());
    }
    if parts.is_empty() {
        return ResourceSecurity::not_checked(NOT_SCANNED);
    }
    let checked = parts.len();
    resolve(parts, Some((checked, total)), is_service, now)
}

/// The name of an owner in the per-action data: the human name of a part of a service, else its
/// name; `None` when blank.
fn owner_name(owner: &ScanOwner) -> Option<String> {
    let falsy = |v: &str| v.is_empty() || v == "0";
    let name = if owner.morph == SERVICE_APPLICATION_MORPH || owner.morph == SERVICE_DATABASE_MORPH
    {
        let human = owner.string("human_name");
        if falsy(&human) { owner.name() } else { human }
    } else {
        owner.name()
    };
    (!name.trim().is_empty()).then_some(name)
}

/// A finding of a builder image that a builder update would fix: an OS package with a fixed
/// version.
fn builder_can_fix(finding: &Finding) -> bool {
    matches!(
        finding.r#type.to_lowercase().as_str(),
        "apk" | "deb" | "rpm"
    ) && finding.fix_state == "fixed"
        && finding.fixed_in.as_deref().is_some_and(|f| !f.is_empty())
}

/// `SecurityStatus::evaluate($scan, $rules)`: the status of one scan before it is combined with
/// other images.
async fn evaluate(
    jobs: &Jobs,
    scan: &ImageScanRow,
    owner: &ScanOwner,
    rules: Option<&RelevanceRules>,
) -> Result<ResourceSecurity, String> {
    let now = Jobs::now();
    let loaded;
    let rules = match rules {
        Some(rules) => rules,
        None => {
            loaded = RelevanceRules::for_resource(&jobs.0.db, owner, now).await?;
            &loaded
        }
    };
    let result = actions::for_scan(jobs, scan, owner, rules).await?;
    let findings = resource_scans::findings(scan);
    let mut security = evaluate_findings(scan, owner, rules, &result, &findings);

    let mut reasons = Vec::new();
    if php_bool(scan.summary.as_ref().and_then(|s| s.get("truncated"))) {
        reasons.push(REASON_TRUNCATED);
    }
    if database_was_old(scan) {
        reasons.push(REASON_DB_OUTDATED);
    }
    if resource_scans::runs_other_image(jobs, scan, owner).await? {
        reasons.push(REASON_IMAGE_CHANGED);
    }
    let has_relevant = findings.iter().any(|f| rules::is_relevant(f, rules));
    if (has_relevant || security.eol.is_some()) && !versions_checked(scan, owner, true) {
        reasons.push(REASON_VERSIONS_UNCHECKED);
    }
    security.facts.insert("reasons".to_owned(), json!(reasons));
    // `reasons` first, as upstream writes the facts of one image.
    let mut facts = Map::new();
    for key in ["reasons", "counts", "action_stats", "unfixed_why"] {
        if let Some(value) = security.facts.shift_remove(key) {
            facts.insert(key.to_owned(), value);
        }
    }
    security.facts = facts;
    Ok(resolve(vec![security], None, false, now))
}

/// The part of `evaluate()` that reads only the findings and the actions: the raw status of one
/// image (status ok, with its actions, end-of-life OS, scan time, and facts without `reasons`).
fn evaluate_findings(
    scan: &ImageScanRow,
    owner: &ScanOwner,
    rules: &RelevanceRules,
    result: &actions::ScanActions,
    findings: &[Finding],
) -> ResourceSecurity {
    let owner_stats = Stats {
        owner: owner_name(owner),
        owner_type: Some(owner.morph.to_owned()),
        owner_id: json!(owner.id),
        ..Stats::default()
    };
    let mut stats: Vec<Stats> = result.actions.iter().map(|_| owner_stats.clone()).collect();
    let mut counts = Counts::default();
    let mut unfixed = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let is_builder = actions::image_kind(scan, owner) == "builder";
    for finding in findings {
        let key = finding.id.to_lowercase();
        if seen.contains(&key) || !rules::is_relevant(finding, rules) {
            continue;
        }
        seen.insert(key);

        let signal = rules::exploit_signal(finding);
        let severity = finding.severity.to_lowercase();
        let why = why_for(finding, signal);
        let Some(index) = result.by_finding.get(&finding.id).copied() else {
            if signal.is_some() {
                counts.add("signal_unfixed", 1);
                if signal == Some(ExploitSignal::Exploited) {
                    counts.add("kev_unfixed", 1);
                }
                if is_builder && builder_can_fix(finding) {
                    counts.add("builder_unfixed", 1);
                }
                unfixed.push(why);
            }
            continue;
        };

        let entry = &mut stats[index];
        entry.relevant += 1;
        entry.why.push(why);
        counts.add("relevant_fixed", 1);
        if signal.is_some() {
            entry.signal += 1;
            counts.add("signal_fixed", 1);
        }
        if signal == Some(ExploitSignal::Exploited) {
            entry.kev += 1;
            counts.add("kev_fixed", 1);
        }
        match severity.as_str() {
            "critical" => entry.critical += 1,
            "high" => entry.high += 1,
            "medium" => entry.medium += 1,
            _ => {}
        }
        if matches!(severity.as_str(), "critical" | "high" | "medium") {
            counts.add(&format!("{severity}_fixed"), 1);
        }
    }

    // Leaving an end-of-life OS counts as one critical relevant fix (spec 15.3).
    if let Some(index) = result.eol {
        let entry = &mut stats[index];
        entry.eol = 1;
        entry.critical += 1;
        entry.relevant += 1;
        counts.add("critical_fixed", 1);
        counts.add("relevant_fixed", 1);
    }
    for entry in &mut stats {
        entry.why = top_why(std::mem::take(&mut entry.why), WHY_LIMIT);
    }

    let eol = match scan.summary.as_ref().and_then(|s| s.get("eol_distro")) {
        Some(Value::Object(distro)) => Some(Eol {
            name: distro.get("name").map(value_string).unwrap_or_default(),
            version: distro.get("version").map(value_string).unwrap_or_default(),
            fixed: result.eol.is_some(),
        }),
        Some(Value::Array(_)) => Some(Eol {
            name: String::new(),
            version: String::new(),
            fixed: result.eol.is_some(),
        }),
        _ => None,
    };

    let mut security = ResourceSecurity::new(SecurityStatusType::Ok);
    security.actions = result.actions.clone();
    security.eol = eol;
    security.checked_at = scan.scanned_at;
    security
        .facts
        .insert("counts".to_owned(), counts.to_value());
    security.facts.insert(
        "action_stats".to_owned(),
        Value::Array(stats.iter().map(Stats::to_value).collect()),
    );
    security.facts.insert(
        "unfixed_why".to_owned(),
        Value::Array(
            top_why(unfixed, WHY_LIMIT)
                .iter()
                .map(Why::to_value)
                .collect(),
        ),
    );
    security
}

/// `SecurityStatus::resolve($parts, $images, $isService)`: combines the statuses of images (or
/// the raw data of one image) and applies the rules of spec 15.6.
fn resolve(
    parts: Vec<ResourceSecurity>,
    images: Option<(usize, usize)>,
    is_service: bool,
    now: NaiveDateTime,
) -> ResourceSecurity {
    let mut entries = Vec::new();
    let mut counts = Counts::default();
    let mut reasons: Vec<String> = Vec::new();
    let mut unfixed = Vec::new();
    let mut eol: Option<Eol> = None;
    let mut checked_at: Option<NaiveDateTime> = None;
    let mut unknown_time = false;
    for part in parts {
        for (index, action) in part.actions.iter().enumerate() {
            entries.push(Entry {
                action: action.clone(),
                stats: Stats::from_value(part.fact(&format!("action_stats.{index}"))),
            });
        }
        for key in COUNT_KEYS {
            counts.add(key, part.fact_int(&format!("counts.{key}")));
        }
        reasons.extend(
            list(part.facts.get("reasons"))
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned),
        );
        unfixed.extend(
            list(part.facts.get("unfixed_why"))
                .iter()
                .map(Why::from_value),
        );
        if let Some(part_eol) = &part.eol
            && (eol.as_ref().is_none_or(|e| e.fixed && !part_eol.fixed))
        {
            eol = Some(part_eol.clone());
        }
        match part.checked_at {
            None => unknown_time = true,
            Some(at) => {
                if checked_at.is_none_or(|current| at < current) {
                    checked_at = Some(at);
                }
            }
        }
    }
    if unknown_time {
        checked_at = None;
    }

    if is_service {
        entries = merge_updates(entries);
    }
    let entries = sort_entries(entries);

    let action_status = if counts.get("signal_fixed") > 0 {
        SecurityStatusType::UpdateNow
    } else if counts.get("relevant_fixed") > 0 {
        SecurityStatusType::UpdateAvailable
    } else if counts.get("signal_unfixed") > 0 {
        SecurityStatusType::NoFix
    } else {
        SecurityStatusType::Ok
    };

    let mut stored_reasons: Vec<String> = Vec::new();
    for reason in reasons {
        if !stored_reasons.contains(&reason) {
            stored_reasons.push(reason);
        }
    }
    let mut all_reasons: Vec<&str> = stored_reasons.iter().map(String::as_str).collect();
    if checked_at.is_none_or(|at| at < now - Duration::days(STALE_AFTER_DAYS)) {
        all_reasons.push(REASON_STALE);
    }
    if let Some((checked, total)) = images
        && checked < total
    {
        all_reasons.push(REASON_IMAGES_UNCHECKED);
    }
    let reason = REASON_ORDER
        .iter()
        .find(|code| all_reasons.contains(code))
        .copied();
    let status = if reason.is_none() {
        action_status
    } else {
        SecurityStatusType::CheckNeeded
    };

    let primary = entries.first().map(|e| &e.stats);
    let mut why = Vec::new();
    let mut more_fixed = 0;
    match primary {
        Some(primary) if action_status.needs_update() => {
            why = primary.why.clone();
            more_fixed = (primary.relevant - primary.eol - why.len() as i64).max(0);
        }
        _ if action_status == SecurityStatusType::NoFix => {
            why = top_why(unfixed.clone(), WHY_LIMIT);
        }
        _ => {}
    }

    let mut check_context = Map::new();
    match reason {
        Some(REASON_IMAGES_UNCHECKED) => {
            if let Some((checked, total)) = images {
                check_context.insert("checked".to_owned(), json!(checked));
                check_context.insert("total".to_owned(), json!(total));
            }
        }
        Some(REASON_STALE) => {
            check_context.insert("time".to_owned(), json!(checked_at.map(iso8601)));
        }
        _ => {}
    }

    let primary_int = |f: fn(&Stats) -> i64| primary.map_or(0, f);
    let mut facts = Map::new();
    facts.insert("action_status".to_owned(), json!(action_status.value()));
    facts.insert("reasons".to_owned(), json!(stored_reasons));
    facts.insert("counts".to_owned(), counts.to_value());
    facts.insert(
        "action_stats".to_owned(),
        Value::Array(entries.iter().map(|e| e.stats.to_value()).collect()),
    );
    facts.insert(
        "unfixed_why".to_owned(),
        Value::Array(
            top_why(unfixed, WHY_LIMIT)
                .iter()
                .map(Why::to_value)
                .collect(),
        ),
    );

    ResourceSecurity {
        status,
        check_reason: reason.map(str::to_owned),
        check_context,
        actions: entries.iter().map(|e| e.action.clone()).collect(),
        why,
        more_fixed,
        exploited_without_fix: counts.get("signal_unfixed") > 0 && status.needs_update(),
        problem_count: match action_status {
            SecurityStatusType::UpdateNow => counts.get("signal_fixed"),
            SecurityStatusType::UpdateAvailable => {
                counts.get("critical_fixed") + counts.get("high_fixed") + counts.get("medium_fixed")
            }
            SecurityStatusType::NoFix => counts.get("signal_unfixed"),
            _ => 0,
        },
        exploited_count: counts.get("signal_fixed") + counts.get("signal_unfixed"),
        any_exploited: match action_status {
            SecurityStatusType::UpdateNow => counts.get("kev_fixed") > 0,
            SecurityStatusType::NoFix => counts.get("kev_unfixed") > 0,
            _ => false,
        },
        critical_count: counts.get("critical_fixed"),
        high_count: counts.get("high_fixed"),
        fixed_by_primary: match action_status {
            SecurityStatusType::UpdateNow => primary_int(|p| p.signal),
            SecurityStatusType::UpdateAvailable => primary_int(|p| p.critical + p.high + p.medium),
            _ => 0,
        },
        primary_critical: primary_int(|p| p.critical),
        primary_high: primary_int(|p| p.high),
        eol,
        checked_at,
        facts,
        builder_image: counts.get("signal_unfixed") > 0
            && counts.get("builder_unfixed") == counts.get("signal_unfixed"),
        medium_count: counts.get("medium_fixed"),
        primary_medium: primary_int(|p| p.medium),
    }
}

/// `SecurityStatus::mergeUpdates()`: images of one service that run the same tag and update to
/// the same tag get one action, also from different repositories: "Update n8n and n8n runners
/// to 2.10.4." The action lists every image in `parts`.
fn merge_updates(entries: Vec<Entry>) -> Vec<Entry> {
    let mut groups: Vec<(String, Vec<usize>)> = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        let key = update_key(entry).unwrap_or_else(|| format!("single:{index}"));
        match groups.iter_mut().find(|(k, _)| *k == key) {
            Some((_, members)) => members.push(index),
            None => groups.push((key, vec![index])),
        }
    }

    let mut merged = Vec::new();
    for (_, indexes) in groups {
        let group: Vec<&Entry> = indexes.iter().map(|i| &entries[*i]).collect();
        let mut names: Vec<String> = Vec::new();
        for entry in &group {
            let name = entry.stats.owner.clone().unwrap_or_default();
            if !names.contains(&name) {
                names.push(name);
            }
        }
        if group.len() == 1 || names.len() < 2 {
            merged.extend(group.into_iter().cloned());
            continue;
        }

        let first = &group[0].action;
        let last = names.pop().unwrap_or_default();
        let mut stats = Stats::default();
        let mut why = Vec::new();
        for entry in &group {
            stats.signal += entry.stats.signal;
            stats.kev += entry.stats.kev;
            stats.critical += entry.stats.critical;
            stats.high += entry.stats.high;
            stats.medium += entry.stats.medium;
            stats.relevant += entry.stats.relevant;
            stats.eol += entry.stats.eol;
            why.extend(entry.stats.why.iter().cloned());
        }
        stats.why = top_why(why, WHY_LIMIT);

        let mut fixed_ids: Vec<String> = Vec::new();
        for entry in &group {
            for id in &entry.action.fixed_ids {
                if !fixed_ids.contains(id) {
                    fixed_ids.push(id.clone());
                }
            }
        }
        let target = first.target.clone().unwrap_or_default();
        merged.push(Entry {
            action: SecurityAction {
                action_type: SecurityActionType::UpdateImage,
                sentence: format!("Update {} and {last} to {target}.", names.join(", ")),
                target: first.target.clone(),
                copy: None,
                fixed_ids,
                button: group.iter().all(|e| e.action.button),
                image: first.image.clone(),
                reference: first.reference.clone(),
                parts: group
                    .iter()
                    .map(|e| ActionPart {
                        owner_type: e.stats.owner_type.clone().unwrap_or_default(),
                        owner_id: e.stats.owner_id.clone(),
                        image: e.action.image.clone(),
                        reference: e.action.reference.clone(),
                    })
                    .collect(),
            },
            stats,
        });
    }
    merged
}

/// Current tag and target tag of an update of a service image that can be merged, else `None`.
fn update_key(entry: &Entry) -> Option<String> {
    let action = &entry.action;
    let blank = |v: Option<&str>| v.is_none_or(|v| v.trim().is_empty());
    if action.action_type != SecurityActionType::UpdateImage
        || action.sentence == COOLIFY_TEXT
        || blank(action.target.as_deref())
        || blank(action.image.as_deref())
        || blank(entry.stats.owner.as_deref())
        || blank(entry.stats.owner_type.as_deref())
    {
        return None;
    }
    let current = ImageRef::parse(action.image.as_deref()?).ok()?;
    current.digest.is_none().then(|| {
        format!(
            "{}|{}",
            current.tag.as_deref().unwrap_or("latest"),
            action.target.as_deref().unwrap_or_default()
        )
    })
}

/// `SecurityStatus::sortEntries()`: most fixes with an exploit signal first, then most critical
/// and high fixes, then most medium fixes, then the order of the action types.
fn sort_entries(entries: Vec<Entry>) -> Vec<Entry> {
    let mut keyed: Vec<(usize, Entry)> = entries.into_iter().enumerate().collect();
    keyed.sort_by_key(|(index, entry)| {
        (
            -entry.stats.signal,
            -(entry.stats.critical + entry.stats.high),
            -entry.stats.medium,
            entry.action.action_type.order(),
            *index,
        )
    });
    keyed.into_iter().map(|(_, entry)| entry).collect()
}

/// The vulnerability database was more than [`DATABASE_MAX_AGE_DAYS`] old when the image was
/// scanned.
fn database_was_old(scan: &ImageScanRow) -> bool {
    match (scan.grype_db_built_at, scan.scanned_at) {
        (Some(built), Some(scanned)) => built < scanned - Duration::days(DATABASE_MAX_AGE_DAYS),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use chrono::NaiveDate;
    use coolify_db::image_scans::{APPLICATION_MORPH, SERVICE_MORPH, STATUS_COMPLETED};

    use super::super::super::resource_scans::tests::{owner, scan};
    use super::*;

    fn now() -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 11, 10)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap()
    }

    fn finding(id: &str, severity: &str, kev: bool, fixed_in: Option<&str>) -> Finding {
        Finding::from_value(&json!({
            "id": id,
            "aliases": [],
            "severity": severity,
            "package": "openssl",
            "version": "3.0.1",
            "type": "deb",
            "fixedIn": fixed_in,
            "fixState": if fixed_in.is_some() { "fixed" } else { "not-fixed" },
            "kev": kev,
        }))
    }

    fn update(owner_name: &str, owner_id: i64, image: &str, tag: &str) -> Entry {
        Entry {
            action: SecurityAction {
                action_type: SecurityActionType::UpdateImage,
                sentence: format!("Update to {tag}."),
                target: Some(tag.to_owned()),
                copy: None,
                fixed_ids: vec![format!("CVE-{owner_id}")],
                button: true,
                image: Some(image.to_owned()),
                reference: Some(format!("{}:{tag}", image.split(':').next().unwrap())),
                parts: Vec::new(),
            },
            stats: Stats {
                owner: Some(owner_name.to_owned()),
                owner_type: Some(SERVICE_APPLICATION_MORPH.to_owned()),
                owner_id: json!(owner_id),
                signal: 1,
                relevant: 1,
                ..Stats::default()
            },
        }
    }

    #[test]
    fn service_updates_to_the_same_tag_merge() {
        let merged = merge_updates(vec![
            update("n8n", 1, "n8nio/n8n:2.10.1", "2.10.4"),
            update("n8n runners", 2, "n8nio/runners:2.10.1", "2.10.4"),
            update("postgres", 3, "postgres:16.1", "16.4"),
        ]);
        assert_eq!(merged.len(), 2);
        assert_eq!(
            merged[0].action.sentence,
            "Update n8n and n8n runners to 2.10.4."
        );
        assert_eq!(merged[0].action.parts.len(), 2);
        assert_eq!(merged[0].action.fixed_ids, vec!["CVE-1", "CVE-2"]);
        assert_eq!(merged[0].stats.signal, 2);
        assert_eq!(merged[1].action.sentence, "Update to 16.4.");
    }

    #[test]
    fn signal_fixes_sort_first() {
        let mut low = update("a", 1, "a:1", "2");
        low.stats.signal = 0;
        low.stats.critical = 3;
        let high = update("b", 2, "b:1", "2");
        let sorted = sort_entries(vec![low, high]);
        assert_eq!(sorted[0].stats.owner.as_deref(), Some("b"));
    }

    #[test]
    fn why_rows_sort_by_signal_score_and_severity() {
        let why = |id: &str, signal: Option<&str>, score: Option<f64>, severity: &str| Why {
            title: id.to_owned(),
            display_id: id.to_owned(),
            link: None,
            score,
            severity: severity.to_owned(),
            signal: signal.map(str::to_owned),
            package: String::new(),
            version: String::new(),
        };
        let sorted = top_why(
            vec![
                why("CVE-4", None, None, "critical"),
                why("CVE-3", None, Some(9.8), "critical"),
                why("CVE-2", Some("likely"), Some(7.0), "high"),
                why("CVE-1", Some("exploited"), None, "high"),
            ],
            3,
        );
        let ids: Vec<&str> = sorted.iter().map(|w| w.display_id.as_str()).collect();
        assert_eq!(ids, vec!["CVE-1", "CVE-2", "CVE-3"]);
    }

    fn evaluated(findings: Vec<Finding>, result: actions::ScanActions) -> ResourceSecurity {
        let mut row = scan("nginx:1.27", STATUS_COMPLETED, None);
        row.scanned_at = Some(now());
        let app = owner(
            APPLICATION_MORPH,
            json!({"id": 7, "uuid": "a", "name": "api", "build_pack": "dockerimage"}),
        );
        let mut security =
            evaluate_findings(&row, &app, &RelevanceRules::default(), &result, &findings);
        security.facts.insert("reasons".to_owned(), json!([]));
        resolve(vec![security], None, false, now())
    }

    #[test]
    fn a_fixed_exploited_problem_is_update_now() {
        let action = SecurityAction {
            action_type: SecurityActionType::UpdateImage,
            sentence: "Update to 1.28.".to_owned(),
            target: Some("1.28".to_owned()),
            copy: None,
            fixed_ids: vec!["CVE-1".to_owned()],
            button: true,
            image: Some("nginx:1.27".to_owned()),
            reference: Some("nginx:1.28".to_owned()),
            parts: Vec::new(),
        };
        let result = actions::ScanActions {
            actions: vec![action],
            by_finding: HashMap::from([("CVE-1".to_owned(), 0)]),
            eol: None,
        };
        let security = evaluated(
            vec![
                finding("CVE-1", "high", true, Some("1.2")),
                finding("CVE-2", "critical", true, None),
                finding("CVE-3", "low", false, None),
            ],
            result,
        );
        assert_eq!(security.status, SecurityStatusType::UpdateNow);
        assert_eq!(security.problem_count, 1);
        assert_eq!(security.exploited_count, 2);
        assert!(security.any_exploited);
        assert!(security.exploited_without_fix);
        assert_eq!(security.fixed_by_primary, 1);
        assert_eq!(security.why.len(), 1);
        assert_eq!(security.fact_int("counts.signal_fixed"), 1);
        assert_eq!(security.fact_int("counts.kev_unfixed"), 1);
        assert_eq!(security.fact("action_status"), Some(&json!("update_now")));
        // The stored form reads back to the same status.
        assert_eq!(ResourceSecurity::from_value(&security.to_value()), security);
    }

    #[test]
    fn an_unfixed_exploited_problem_is_no_fix_and_old_results_need_a_check() {
        let security = evaluated(
            vec![finding("CVE-2", "critical", true, None)],
            actions::ScanActions::default(),
        );
        assert_eq!(security.status, SecurityStatusType::NoFix);
        assert_eq!(security.problem_count, 1);
        assert_eq!(security.why.len(), 1);

        let mut stale = ResourceSecurity::new(SecurityStatusType::Ok);
        stale.checked_at = Some(now() - Duration::days(3));
        let resolved = resolve(vec![stale], Some((1, 2)), false, now());
        assert_eq!(resolved.status, SecurityStatusType::CheckNeeded);
        assert_eq!(
            resolved.check_reason.as_deref(),
            Some(REASON_IMAGES_UNCHECKED)
        );
        assert_eq!(resolved.check_context.get("total"), Some(&json!(2)));
        assert_eq!(
            combine_images(vec![(vec![None], 0)], true, now())
                .check_reason
                .as_deref(),
            Some(NOT_SCANNED)
        );
    }

    #[test]
    fn versions_checked_needs_the_checks_of_the_image() {
        let mut row = scan("nginx:1.27", STATUS_COMPLETED, None);
        let database = owner(
            r"App\Models\StandalonePostgresql",
            json!({"id": 7, "uuid": "d"}),
        );
        assert!(!versions_checked(&row, &database, true));
        row.candidates = Some(json!({"status": "no_better"}));
        assert!(versions_checked(&row, &database, true));
        row.candidates = Some(json!({"status": "auth_required"}));
        assert!(versions_checked(&row, &database, true));
        assert!(!versions_checked(&row, &database, false));
        row.candidates = Some(json!({"status": "failed", "error": "daily_limit"}));
        assert!(versions_checked(&row, &database, true));
        row.candidates = Some(json!({"status": "failed", "error": "lock_busy"}));
        assert!(!versions_checked(&row, &database, true));

        let dockerfile = owner(
            APPLICATION_MORPH,
            json!({"id": 7, "uuid": "a", "build_pack": "dockerfile"}),
        );
        let mut built = scan("app:abc", STATUS_COMPLETED, None);
        built.builds_image = true;
        assert!(versions_checked(&built, &dockerfile, true));
        built.base_image_ref = Some("node:20".to_owned());
        assert!(!versions_checked(&built, &dockerfile, true));
        built.candidates =
            Some(json!({"base": {"image": "node:20", "status": "ok", "attributable": 0}}));
        assert!(versions_checked(&built, &dockerfile, true));
        let service = owner(SERVICE_MORPH, json!({"id": 1, "uuid": "s"}));
        assert!(!versions_checked(
            &scan("x:1", STATUS_COMPLETED, None),
            &service,
            true
        ));
    }

    #[test]
    fn an_old_database_is_outdated() {
        let mut row = scan("nginx:1.27", STATUS_COMPLETED, None);
        row.scanned_at = Some(now());
        row.grype_db_built_at = Some(now() - Duration::days(6));
        assert!(database_was_old(&row));
        row.grype_db_built_at = Some(now() - Duration::days(4));
        assert!(!database_was_old(&row));
    }
}
