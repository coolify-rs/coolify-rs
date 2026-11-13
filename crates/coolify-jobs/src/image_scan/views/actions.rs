//! `App\Services\ImageScan\SecurityActions`: maps each relevant finding of a scanned image to the
//! one action that fixes it, or to none (spec 15.5).
//!
//! Reads only stored data: the findings, the version and base checks in `candidates`, and, for a
//! Dockerfile app with OS findings that have a fix, two queries for the latest rebuild without
//! cache of the app and the scan before it. A finding that a rebuild without cache did not
//! remove gets `upgrade_in_dockerfile` instead of another `rebuild`, also in the scans of later
//! deploys.

use std::collections::{HashMap, HashSet};

use coolify_db::image_scans::{
    self, ImageScanRow, SERVICE_APPLICATION_MORPH, SERVICE_DATABASE_MORPH, ScanOwner,
};
use regex::Regex;
use serde_json::Value;
use std::sync::LazyLock;

use super::super::finding::Finding;
use super::super::image_ref::ImageRef;
use super::super::rules::{self, RelevanceRules};
use super::super::{ScanServer, candidates, resource_scans, update};
use super::action::{SecurityAction, value_string};
use super::{SecurityActionType, filled, version_greater};
use crate::Jobs;

/// `SecurityActions::BUILDER_TEXT`.
pub const BUILDER_TEXT: &str = "A fix exists, but it reaches this app only with a builder update in a Coolify release. To choose the base image yourself, use the Dockerfile build pack.";

/// `SecurityActions::COOLIFY_TEXT`.
pub const COOLIFY_TEXT: &str = "Update Coolify to get a newer version.";

const OS_PACKAGE_TYPES: [&str; 3] = ["apk", "deb", "rpm"];

const LANGUAGE_PACKAGE_TYPES: [&str; 8] = [
    "npm",
    "python",
    "go-module",
    "java-archive",
    "gem",
    "php-composer",
    "rust-crate",
    "dotnet",
];

const BUTTON_TYPES: [SecurityActionType; 3] = [
    SecurityActionType::UpdateImage,
    SecurityActionType::PullLatest,
    SecurityActionType::Rebuild,
];

static LEADING_V: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^v").unwrap_or_else(|_| unreachable!()));
static MAJOR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^v?(\d+)").unwrap_or_else(|_| unreachable!()));

/// `SecurityActions::forScan()`: the actions of a scan, in the order of the action types.
/// `by_finding` maps a finding ID to the index of the action that fixes it; `eol` is the index of
/// the action that moves the image off an end-of-life OS.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ScanActions {
    pub actions: Vec<SecurityAction>,
    pub by_finding: HashMap<String, usize>,
    pub eol: Option<usize>,
}

impl ScanActions {
    /// `$result['byFinding'][$finding->id] ?? null`.
    pub fn for_finding(&self, finding_id: &str) -> Option<&SecurityAction> {
        self.by_finding
            .get(finding_id)
            .and_then(|index| self.actions.get(*index))
    }

    /// `$result['eol']`.
    pub fn eol_action(&self) -> Option<&SecurityAction> {
        self.eol.and_then(|index| self.actions.get(index))
    }
}

/// `ImageScanFixState`: the fix state of a finding in the findings lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FixState {
    Fixed,
    NoFix,
}

impl FixState {
    pub fn value(self) -> &'static str {
        match self {
            FixState::Fixed => "fixed",
            FixState::NoFix => "no_fix",
        }
    }
}

/// `SecurityActions::fixStates($scan, $rules)`: the finding IDs an action of the scan fixes.
#[derive(Debug, Clone, Default)]
pub struct FixStates {
    by_finding: HashSet<String>,
}

impl FixStates {
    /// Fixed when an action of the scan fixes it or a fixed package version exists, else no fix.
    pub fn state(&self, finding: &Finding) -> FixState {
        if self.by_finding.contains(&finding.id) || has_fix(finding) {
            FixState::Fixed
        } else {
            FixState::NoFix
        }
    }
}

/// `SecurityActions::fixStates($scan, $rules)`.
pub async fn fix_states(
    jobs: &Jobs,
    scan: &ImageScanRow,
    owner: &ScanOwner,
    rules: &RelevanceRules,
) -> Result<FixStates, String> {
    let result = for_scan(jobs, scan, owner, rules).await?;
    Ok(FixStates {
        by_finding: result.by_finding.into_keys().collect(),
    })
}

fn has_fix(finding: &Finding) -> bool {
    finding.fix_state == "fixed" && filled(finding.fixed_in.as_deref())
}

/// What one action gathers before its sentence is written.
#[derive(Debug, Clone)]
struct Plan {
    action_type: SecurityActionType,
    ids: Vec<String>,
    package: Option<String>,
    fixed_in: Option<String>,
    versions: Vec<String>,
}

impl Plan {
    fn new(action_type: SecurityActionType, package: Option<String>) -> Self {
        Self {
            action_type,
            ids: Vec::new(),
            package,
            fixed_in: None,
            versions: Vec::new(),
        }
    }
}

/// What every action of one scan shares.
#[derive(Debug, Clone)]
struct Context {
    image: Option<String>,
    coolify: bool,
    /// A Compose app: Coolify keeps the new image as an override of its compose file.
    compose: bool,
    buttons: bool,
    switch_blocked: bool,
    line: Option<Value>,
    same_tag: Option<Value>,
    base_to: Option<String>,
}

/// `SecurityActions::forScan($scan, $rules)`. An unsaved or failed scan works too; it simply has
/// no findings.
pub async fn for_scan(
    jobs: &Jobs,
    scan: &ImageScanRow,
    owner: &ScanOwner,
    rules: &RelevanceRules,
) -> Result<ScanActions, String> {
    let kind = image_kind(scan, owner);
    let (line, same_tag) = if matches!(kind, "third_party" | "docker_image") {
        image_candidates(scan)
    } else {
        (None, None)
    };
    let base = if kind == "dockerfile" {
        candidates::better_base(scan)
    } else {
        None
    };
    // `None` while Coolify does not know which findings come from the base image.
    let from_base = if kind == "dockerfile" {
        base_ids(scan)
    } else {
        Some(HashSet::new())
    };
    let origin_known = from_base.is_some();
    let from_base = from_base.unwrap_or_default();
    let line_fixed = id_set(line.as_ref().and_then(|l| l.get("fixed_ids")));
    let same_tag_fixed = id_set(same_tag.as_ref().and_then(|l| l.get("fixed_ids")));
    let base_fixed = id_set(base.as_ref().and_then(|b| b.entry.get("fixed_ids")));
    let mut before_rebuild: Option<HashSet<String>> = None;

    let mut plans: Vec<(String, Plan)> = Vec::new();
    let mut by_finding: Vec<(String, String)> = Vec::new();
    for finding in resource_scans::findings(scan) {
        if !rules::is_relevant(&finding, rules) {
            continue;
        }

        let ids = ids(&finding);
        let package_type = finding.r#type.to_lowercase();
        let fix = has_fix(&finding);
        let is_os = OS_PACKAGE_TYPES.contains(&package_type.as_str());
        let is_from_base = hits(&ids, &from_base);
        let is_package_fix = fix
            && origin_known
            && !is_from_base
            && LANGUAGE_PACKAGE_TYPES.contains(&package_type.as_str());

        let action = if hits(&ids, &line_fixed) {
            Some(SecurityActionType::UpdateImage)
        } else if hits(&ids, &same_tag_fixed) {
            Some(SecurityActionType::PullLatest)
        } else if is_from_base && hits(&ids, &base_fixed) {
            Some(SecurityActionType::ChangeBase)
        } else if kind == "dockerfile" && is_os && fix && origin_known && !is_from_base {
            if before_rebuild.is_none() {
                before_rebuild = Some(ids_before_forced_rebuild(jobs, scan, owner).await?);
            }
            if hits(&ids, before_rebuild.as_ref().unwrap_or(&HashSet::new())) {
                Some(SecurityActionType::UpgradeInDockerfile)
            } else {
                Some(SecurityActionType::Rebuild)
            }
        } else if kind != "third_party" && is_package_fix {
            Some(SecurityActionType::UpgradePackage)
        } else if kind == "docker_image" && is_os && fix {
            Some(SecurityActionType::RebuildInCi)
        } else {
            None
        };
        let Some(action) = action else {
            continue;
        };

        let per_package = matches!(
            action,
            SecurityActionType::UpgradePackage | SecurityActionType::UpgradeInDockerfile
        );
        let key = if per_package {
            format!("{}:{}|{}", action.value(), package_type, finding.package)
        } else {
            action.value().to_owned()
        };
        let index = match plans.iter().position(|(k, _)| *k == key) {
            Some(index) => index,
            None => {
                plans.push((
                    key.clone(),
                    Plan::new(action, per_package.then(|| finding.package.clone())),
                ));
                plans.len() - 1
            }
        };
        let plan = &mut plans[index].1;
        if !plan.ids.contains(&finding.id) {
            plan.ids.push(finding.id.clone());
        }
        if per_package {
            plan.fixed_in = Some(higher(
                plan.fixed_in.as_deref(),
                finding.fixed_in.as_deref().unwrap_or_default(),
            ));
            plan.versions.push(finding.version.clone());
        }
        if !by_finding.iter().any(|(id, _)| *id == finding.id) {
            by_finding.push((finding.id.clone(), key));
        }
    }

    let mut eol_key = None;
    if scan
        .summary
        .as_ref()
        .and_then(|s| s.get("eol_distro"))
        .is_some_and(|e| e.is_object() || e.is_array())
    {
        let empty_eol = |entry: Option<&Value>| entry.is_none_or(|e| php_empty(e.get("eol")));
        let eol_type = if line.is_some() && empty_eol(line.as_ref()) {
            Some(SecurityActionType::UpdateImage)
        } else if same_tag.is_some() && empty_eol(same_tag.as_ref()) {
            Some(SecurityActionType::PullLatest)
        } else if base.is_some() && empty_eol(base.as_ref().map(|b| &b.entry)) {
            Some(SecurityActionType::ChangeBase)
        } else {
            None
        };
        if let Some(eol_type) = eol_type {
            let key = eol_type.value().to_owned();
            if !plans.iter().any(|(k, _)| *k == key) {
                plans.push((key.clone(), Plan::new(eol_type, None)));
            }
            eol_key = Some(key);
        }
    }

    // `uasort()` by the order of the action types (stable).
    plans.sort_by_key(|(_, plan)| plan.action_type.order());

    let coolify = is_coolify_resource(owner);
    let transferred = ScanServer::load(jobs, scan.server_id)
        .await
        .ok()
        .is_some_and(|server| server.is_transferred_away());
    let switch_blocked = if line.is_some() || same_tag.is_some() {
        update::blocked_reason(jobs, scan, owner).await?.is_some()
    } else {
        false
    };
    let context = Context {
        image: Some(scan.image_ref.clone()),
        coolify,
        compose: owner.is_application() && owner.build_pack() == Some("dockercompose"),
        buttons: !coolify && !transferred,
        switch_blocked,
        line,
        same_tag,
        base_to: base.map(|b| b.to),
    };
    let actions: Vec<SecurityAction> = plans
        .iter()
        .map(|(_, plan)| action(plan, &context))
        .collect();
    let index_of = |key: &str| plans.iter().position(|(k, _)| k == key);
    Ok(ScanActions {
        by_finding: by_finding
            .iter()
            .filter_map(|(id, key)| index_of(key.as_str()).map(|index| (id.clone(), index)))
            .collect(),
        eol: eol_key.as_deref().and_then(index_of),
        actions,
    })
}

/// `empty($value)` of a decoded JSON value.
fn php_empty(value: Option<&Value>) -> bool {
    !super::action::php_bool(value)
}

/// `SecurityActions::action($plan, $context)`.
fn action(plan: &Plan, context: &Context) -> SecurityAction {
    let action_type = plan.action_type;
    let package = plan.package.clone().unwrap_or_default();
    let fixed_in = plan.fixed_in.clone().unwrap_or_default();
    let candidate = match action_type {
        SecurityActionType::UpdateImage => context.line.as_ref(),
        SecurityActionType::PullLatest => context.same_tag.as_ref(),
        _ => None,
    };
    let reference = candidate.map(|c| c.get("ref").map(value_string).unwrap_or_default());
    let tag = reference
        .as_deref()
        .and_then(|r| ImageRef::parse(r).ok())
        .and_then(|r| r.tag);
    let tag_text = tag.clone().unwrap_or_default();
    let new_base = (action_type == SecurityActionType::ChangeBase)
        .then(|| context.base_to.clone().unwrap_or_default());
    let new_base_text = new_base.clone().unwrap_or_default();
    let pulled = reference
        .clone()
        .or_else(|| context.image.clone())
        .unwrap_or_else(|| "this image".to_owned());

    let sentence = match action_type {
        SecurityActionType::UpdateImage => {
            if context.coolify {
                COOLIFY_TEXT.to_owned()
            } else if context.compose && context.switch_blocked {
                format!(
                    "Change the version to {tag_text} in the compose file of this app, then redeploy."
                )
            } else {
                format!("Update to {tag_text}.")
            }
        }
        SecurityActionType::PullLatest => {
            if context.coolify {
                COOLIFY_TEXT.to_owned()
            } else if context.compose && context.switch_blocked {
                "A newer build of this image fixes it. Redeploy this app to pull it.".to_owned()
            } else if context.compose {
                format!("Pull the newer build of {pulled} and redeploy.")
            } else {
                format!("Pull the newer build of {pulled} and restart.")
            }
        }
        SecurityActionType::ChangeBase => {
            format!("Change the base image to {new_base_text} in your Dockerfile, then redeploy.")
        }
        SecurityActionType::Rebuild => {
            "Rebuild without cache to get the fixed system packages.".to_owned()
        }
        SecurityActionType::UpgradeInDockerfile => {
            format!("Upgrade {package} to {fixed_in} or later in your Dockerfile.")
        }
        SecurityActionType::UpgradePackage => format!(
            "Upgrade {package} to {fixed_in} or later, then redeploy.{}",
            if changes_major(&plan.versions, &fixed_in) {
                " This is a major version change. Check for breaking changes."
            } else {
                ""
            }
        ),
        SecurityActionType::RebuildInCi => {
            "Rebuild the image in your CI, then redeploy.".to_owned()
        }
    };

    SecurityAction {
        action_type,
        sentence,
        target: match action_type {
            SecurityActionType::UpdateImage | SecurityActionType::PullLatest => tag,
            SecurityActionType::ChangeBase => new_base,
            SecurityActionType::UpgradePackage | SecurityActionType::UpgradeInDockerfile => {
                Some(package.clone())
            }
            _ => None,
        },
        copy: match action_type {
            SecurityActionType::ChangeBase => Some(format!("FROM {new_base_text}")),
            SecurityActionType::UpgradePackage => Some(format!("{package} {fixed_in}")),
            _ => None,
        },
        fixed_ids: plan.ids.clone(),
        button: BUTTON_TYPES.contains(&action_type)
            && context.buttons
            && (action_type == SecurityActionType::Rebuild || !context.switch_blocked),
        image: context.image.clone(),
        reference,
        parts: Vec::new(),
    }
}

/// `SecurityActions::imageKind($scan, $owner)`: `third_party` (Coolify pulls it),
/// `docker_image` (a Docker image app the team builds itself), `dockerfile`, `compose`, or
/// `builder` (Nixpacks, Railpack, static): Coolify builds it.
pub fn image_kind(scan: &ImageScanRow, owner: &ScanOwner) -> &'static str {
    if !scan.builds_image || !owner.is_application() {
        return "third_party";
    }
    match owner.build_pack() {
        Some("dockerimage") => "docker_image",
        Some("dockerfile") => "dockerfile",
        Some("dockercompose") => "compose",
        _ => "builder",
    }
}

/// `SecurityActions::imageCandidates($scan)`: the better newer tag of the same line and the
/// better newer build of the same tag. Never a major version.
fn image_candidates(scan: &ImageScanRow) -> (Option<Value>, Option<Value>) {
    let candidates = update::update_candidates(scan);
    let of_kind = |kind: &str| {
        candidates
            .iter()
            .find(|entry| entry.get("kind").and_then(Value::as_str) == Some(kind))
            .cloned()
    };
    (of_kind("line"), of_kind("same_tag"))
}

/// `SecurityActions::baseIds($scan)`: the relevant findings that the current base image has
/// too. Empty without a base image. `None` while unknown: the base check did not run yet, is for
/// another base image, or did not scan the current base image.
fn base_ids(scan: &ImageScanRow) -> Option<HashSet<String>> {
    let base_ref = scan.base_image_ref.as_deref().unwrap_or_default();
    if base_ref.trim().is_empty() {
        return Some(HashSet::new());
    }
    let base = scan.candidates.as_ref()?.get("base")?.as_object()?;
    if base.get("image").and_then(Value::as_str) != Some(base_ref)
        || !base.get("attributable").is_some_and(Value::is_i64)
    {
        return None;
    }
    Some(id_set(base.get("attributable_ids")))
}

/// `SecurityActions::idsBeforeForcedRebuild($scan, $owner)`: the IDs that survived the latest
/// rebuild without cache of the app (up to this scan). Empty before the first such rebuild.
async fn ids_before_forced_rebuild(
    jobs: &Jobs,
    scan: &ImageScanRow,
    owner: &ScanOwner,
) -> Result<HashSet<String>, String> {
    if !owner.is_application() || !scan.exists() {
        return Ok(HashSet::new());
    }
    let Some((rebuilt, previous)) =
        image_scans::views::forced_rebuild_findings(&jobs.0.db, owner.morph, owner.id, scan.id)
            .await
            .map_err(|e| e.to_string())?
    else {
        return Ok(HashSet::new());
    };
    Ok(survived(
        &Finding::list_from_value(previous.as_ref()),
        &Finding::list_from_value(rebuilt.as_ref()),
    ))
}

/// The IDs of the findings of the rebuilt scan that the scan before it had too.
fn survived(previous: &[Finding], rebuilt: &[Finding]) -> HashSet<String> {
    let before: HashSet<String> = previous.iter().flat_map(ids).collect();
    rebuilt
        .iter()
        .map(ids)
        .filter(|finding_ids| hits(finding_ids, &before))
        .flatten()
        .collect()
}

/// `SecurityActions::isCoolifyResource($owner)`: the instance database, or a part of a service
/// with id 0.
pub(crate) fn is_coolify_resource(owner: &ScanOwner) -> bool {
    if owner.id == 0 {
        return true;
    }
    if owner.morph != SERVICE_APPLICATION_MORPH && owner.morph != SERVICE_DATABASE_MORPH {
        return false;
    }
    owner.service_id == Some(0)
}

/// `SecurityActions::higher($current, $candidate)`: the higher of two fixed versions (a leading
/// `v` is ignored). A version is preferred to a value that is not one; of two values that are
/// not versions, the first is kept.
fn higher(current: Option<&str>, candidate: &str) -> String {
    let Some(current) = current else {
        return candidate.to_owned();
    };
    let a = LEADING_V.replace(current, "").into_owned();
    let b = LEADING_V.replace(candidate, "").into_owned();
    let a_is_version = a.chars().next().is_some_and(|c| c.is_ascii_digit());
    let b_is_version = b.chars().next().is_some_and(|c| c.is_ascii_digit());
    if !a_is_version || !b_is_version {
        return if b_is_version { candidate } else { current }.to_owned();
    }
    if version_greater(&b, &a) {
        candidate.to_owned()
    } else {
        current.to_owned()
    }
}

fn changes_major(versions: &[String], fixed_in: &str) -> bool {
    let Some(target) = major(fixed_in) else {
        return false;
    };
    versions
        .iter()
        .any(|version| major(version).is_some_and(|m| m != target))
}

fn major(version: &str) -> Option<u64> {
    MAJOR
        .captures(version)
        .and_then(|c| c.get(1))
        .and_then(|m| m.as_str().parse().ok())
}

/// The ID and the aliases of a finding, lower case, without empty ones.
fn ids(finding: &Finding) -> Vec<String> {
    finding
        .ids()
        .map(str::to_lowercase)
        .filter(|id| !id.is_empty())
        .collect()
}

/// `SecurityActions::idSet((array) $ids)`.
pub(crate) fn id_set(value: Option<&Value>) -> HashSet<String> {
    let items: Vec<&Value> = match value {
        Some(Value::Array(items)) => items.iter().collect(),
        Some(Value::Object(items)) => items.values().collect(),
        None | Some(Value::Null) => Vec::new(),
        Some(scalar) => vec![scalar],
    };
    items
        .into_iter()
        .filter_map(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_lowercase)
        .collect()
}

fn hits(ids: &[String], set: &HashSet<String>) -> bool {
    ids.iter().any(|id| set.contains(id))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::super::super::resource_scans::tests::{owner, scan};
    use super::*;
    use coolify_db::image_scans::{APPLICATION_MORPH, STATUS_COMPLETED};

    fn finding(id: &str, package_type: &str, fixed_in: Option<&str>) -> Finding {
        Finding::from_value(&json!({
            "id": id,
            "aliases": [],
            "severity": "high",
            "package": "openssl",
            "version": "3.0.1",
            "type": package_type,
            "fixedIn": fixed_in,
            "fixState": if fixed_in.is_some() { "fixed" } else { "not-fixed" },
        }))
    }

    #[test]
    fn image_kinds() {
        let mut row = scan("nginx:1.27", STATUS_COMPLETED, None);
        let app = |pack: &str| {
            owner(
                APPLICATION_MORPH,
                json!({"id": 3, "uuid": "a", "build_pack": pack}),
            )
        };
        assert_eq!(image_kind(&row, &app("dockerfile")), "third_party");
        row.builds_image = true;
        assert_eq!(image_kind(&row, &app("dockerfile")), "dockerfile");
        assert_eq!(image_kind(&row, &app("dockerimage")), "docker_image");
        assert_eq!(image_kind(&row, &app("dockercompose")), "compose");
        assert_eq!(image_kind(&row, &app("nixpacks")), "builder");
        let database = owner(
            r"App\Models\StandalonePostgresql",
            json!({"id": 3, "uuid": "d"}),
        );
        assert_eq!(image_kind(&row, &database), "third_party");
    }

    #[test]
    fn the_higher_fixed_version_wins() {
        assert_eq!(higher(None, "1.2.3"), "1.2.3");
        assert_eq!(higher(Some("1.2.3"), "v1.10.0"), "v1.10.0");
        assert_eq!(higher(Some("1.10.0"), "1.2.3"), "1.10.0");
        assert_eq!(higher(Some("abc"), "1.0"), "1.0");
        assert_eq!(higher(Some("1.0"), "abc"), "1.0");
        assert_eq!(higher(Some("abc"), "def"), "abc");
        assert!(changes_major(&["2.9.1".to_owned()], "3.0.0"));
        assert!(!changes_major(&["3.0.1".to_owned()], "v3.2.0"));
        assert!(!changes_major(&["x".to_owned()], "3.0.0"));
    }

    #[test]
    fn findings_that_survived_a_rebuild() {
        let previous = vec![
            finding("CVE-1", "deb", Some("1")),
            finding("CVE-2", "deb", Some("1")),
        ];
        let rebuilt = vec![
            finding("CVE-2", "deb", Some("1")),
            finding("CVE-3", "deb", Some("1")),
        ];
        assert_eq!(
            survived(&previous, &rebuilt),
            HashSet::from(["cve-2".to_owned()])
        );
    }

    #[test]
    fn base_ids_are_unknown_until_the_base_check_ran() {
        let mut row = scan("app:abc", STATUS_COMPLETED, None);
        assert_eq!(base_ids(&row), Some(HashSet::new()));
        row.base_image_ref = Some("node:20".to_owned());
        assert_eq!(base_ids(&row), None);
        row.candidates = Some(
            json!({"base": {"image": "node:20", "attributable": 2, "attributable_ids": ["CVE-1", "GHSA-x"]}}),
        );
        assert_eq!(
            base_ids(&row),
            Some(HashSet::from(["cve-1".to_owned(), "ghsa-x".to_owned()]))
        );
        row.candidates = Some(json!({"base": {"image": "node:18", "attributable": 2}}));
        assert_eq!(base_ids(&row), None);
    }

    #[test]
    fn sentences_of_each_action() {
        let context = Context {
            image: Some("nginx:1.27".to_owned()),
            coolify: false,
            compose: false,
            buttons: true,
            switch_blocked: false,
            line: Some(json!({"ref": "nginx:1.28", "kind": "line"})),
            same_tag: Some(json!({"ref": "nginx:1.27", "kind": "same_tag"})),
            base_to: None,
        };
        let plan = |t: SecurityActionType| Plan {
            action_type: t,
            ids: vec!["CVE-1".to_owned()],
            package: Some("lodash".to_owned()),
            fixed_in: Some("5.0.0".to_owned()),
            versions: vec!["4.17.0".to_owned()],
        };
        let update = action(&plan(SecurityActionType::UpdateImage), &context);
        assert_eq!(update.sentence, "Update to 1.28.");
        assert_eq!(update.target.as_deref(), Some("1.28"));
        assert!(update.button);
        let pull = action(&plan(SecurityActionType::PullLatest), &context);
        assert_eq!(
            pull.sentence,
            "Pull the newer build of nginx:1.27 and restart."
        );
        let upgrade = action(&plan(SecurityActionType::UpgradePackage), &context);
        assert_eq!(
            upgrade.sentence,
            "Upgrade lodash to 5.0.0 or later, then redeploy. This is a major version change. Check for breaking changes."
        );
        assert_eq!(upgrade.copy.as_deref(), Some("lodash 5.0.0"));
        assert!(!upgrade.button);
        let mut compose = context.clone();
        compose.compose = true;
        compose.switch_blocked = true;
        assert_eq!(
            action(&plan(SecurityActionType::UpdateImage), &compose).sentence,
            "Change the version to 1.28 in the compose file of this app, then redeploy."
        );
        assert!(!action(&plan(SecurityActionType::UpdateImage), &compose).button);
        assert!(action(&plan(SecurityActionType::Rebuild), &compose).button);
        let mut coolify = context.clone();
        coolify.coolify = true;
        coolify.buttons = false;
        assert_eq!(
            action(&plan(SecurityActionType::PullLatest), &coolify).sentence,
            COOLIFY_TEXT
        );
    }

    #[test]
    fn coolify_resources() {
        let mut instance = owner(
            r"App\Models\StandalonePostgresql",
            json!({"id": 0, "uuid": "x"}),
        );
        assert!(!is_coolify_resource(&instance));
        instance.id = 0;
        assert!(is_coolify_resource(&instance));
        let mut part = owner(SERVICE_APPLICATION_MORPH, json!({"id": 5, "uuid": "x"}));
        part.service_id = Some(0);
        assert!(is_coolify_resource(&part));
        part.service_id = Some(2);
        assert!(!is_coolify_resource(&part));
    }
}
