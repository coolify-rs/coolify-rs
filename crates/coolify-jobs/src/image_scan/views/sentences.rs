//! `App\Services\ImageScan\SecuritySentences`: the copy of the security status card (spec 15.5,
//! 15.7, 15.11). Pure functions of the stored [`ResourceSecurity`] and the stored scan data.
//!
//! A scan is passed with its owner (`Option<(&ImageScanRow, &ScanOwner)>`); functions that say
//! how long ago something happened take `now` ([`super::now`]).

use std::collections::HashSet;
use std::sync::LazyLock;

use chrono::NaiveDateTime;
use coolify_db::image_scans::{ImageScanRow, ScanOwner};
use regex::Regex;
use serde_json::Value;

use super::super::finding::Finding;
use super::super::image_ref::ImageRef;
use super::super::{resource_scans, update};
use super::action::{SecurityAction, value_string};
use super::actions::{self, BUILDER_TEXT, COOLIFY_TEXT};
use super::resource_security::ResourceSecurity;
use super::status::{self, NOT_SCANNED, REASON_ORDER};
use super::{
    SecurityActionType, SecurityStatusType, diff_for_humans, filled, iso8601, parse_time, ucfirst,
};

/// The badge while the queued version check of a scanned image runs.
pub const CHECKING_BADGE: &str = "Checking";

const MAJOR_NOTE: &str = " This is a major version change. Check for breaking changes.";

static RELEASE_LINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[^0-9]*(\d+(?:\.\d+)*)").unwrap_or_else(|_| unreachable!()));
static LEADING_MAJOR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^v?(\d+)").unwrap_or_else(|_| unreachable!()));

/// A scan with its owner.
pub type ScanRef<'a> = (&'a ImageScanRow, &'a ScanOwner);

fn is_check_reason(reason: Option<&str>) -> bool {
    reason.is_some_and(|r| REASON_ORDER.contains(&r))
}

/// `SecuritySentences::headline($security)`: the sentence under the status line.
pub fn headline(security: &ResourceSecurity, now: NaiveDateTime) -> String {
    match security.status {
        SecurityStatusType::UpdateNow => {
            update_now(security, "This image", "A newer build of this image")
        }
        SecurityStatusType::UpdateAvailable => update_available(security),
        SecurityStatusType::NoFix => no_fix(security, "This image"),
        SecurityStatusType::Ok => "No action needed.".to_owned(),
        SecurityStatusType::CheckNeeded => format!(
            "Coolify needs to check this image again: {}.",
            if is_check_reason(security.check_reason.as_deref()) {
                check_reason(
                    security.check_reason.as_deref().unwrap_or_default(),
                    &check_context(security),
                    now,
                )
            } else {
                "the last check is incomplete".to_owned()
            }
        ),
        SecurityStatusType::NotChecked => check_reason(
            security.check_reason.as_deref().unwrap_or(NOT_SCANNED),
            &security.check_context,
            now,
        ),
    }
}

/// `SecuritySentences::lines($security)`: extra lines under the headline.
pub fn lines(security: &ResourceSecurity) -> Vec<String> {
    if !security.exploited_without_fix {
        return Vec::new();
    }
    let unfixed = match security.status {
        SecurityStatusType::UpdateNow => security.exploited_count - security.problem_count,
        SecurityStatusType::UpdateAvailable => security.exploited_count,
        _ => 0,
    };
    if unfixed < 1 {
        return Vec::new();
    }
    let used = if security.fact_int("counts.kev_unfixed") > 0 {
        "are using"
    } else {
        "are likely to use"
    };
    vec![if unfixed > 1 {
        format!("{unfixed} more problems that attackers {used} have no fix yet.")
    } else {
        format!("One more problem that attackers {used} has no fix yet.")
    }]
}

/// `SecuritySentences::checkReason($code, $context)`: the reason of a check_needed status, or
/// the text of a not_checked status.
pub fn check_reason(
    code: &str,
    context: &serde_json::Map<String, Value>,
    now: NaiveDateTime,
) -> String {
    let int = |key: &str| super::action::php_int(context.get(key));
    match code {
        status::REASON_IMAGES_UNCHECKED => {
            format!("{} of {} images were checked", int("checked"), int("total"))
        }
        status::REASON_TRUNCATED => "the scan result was too large to read completely".to_owned(),
        status::REASON_STALE => format!("the last check was {}", ago(context.get("time"), now)),
        status::REASON_DB_OUTDATED => "the vulnerability database is out of date".to_owned(),
        status::REASON_IMAGE_CHANGED => "the running image is not the checked image".to_owned(),
        status::REASON_VERSIONS_UNCHECKED => {
            "Coolify has not checked for newer versions yet".to_owned()
        }
        "server" => "Image scanning is off for this server.".to_owned(),
        "application" => "Image scanning is off for this app.".to_owned(),
        _ => "This image has not been scanned yet.".to_owned(),
    }
}

/// `SecuritySentences::whyMore($count)`: the line after the why-list. Empty for none.
pub fn why_more(count: i64) -> String {
    match count {
        c if c <= 0 => String::new(),
        1 => "and 1 more problem that this update fixes".to_owned(),
        c => format!("and {c} more problems that this update fixes"),
    }
}

/// `SecuritySentences::badge($status)`.
pub fn badge(status: SecurityStatusType) -> Option<&'static str> {
    match status {
        SecurityStatusType::UpdateNow => Some("Update now"),
        SecurityStatusType::UpdateAvailable => Some("Security update"),
        SecurityStatusType::NoFix => Some("No fix yet"),
        SecurityStatusType::CheckNeeded => Some("Check needed"),
        SecurityStatusType::Ok | SecurityStatusType::NotChecked => None,
    }
}

/// `SecuritySentences::databaseUpdateConfirmation()`: the confirmation before a database moves
/// to a newer tag of the same major version.
pub fn database_update_confirmation(db_type: &str, from_tag: &str, to_tag: &str) -> String {
    let major = |tag: &str| {
        LEADING_MAJOR
            .captures(tag)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_owned())
    };
    let name = match major(to_tag).or_else(|| major(from_tag)) {
        Some(major) => format!("{db_type} {major}"),
        None => db_type.to_owned(),
    };
    format!(
        "This is a patch release of {name}. Your data stays as it is. The database restarts and is unavailable for a few seconds."
    )
}

/// `SecuritySentences::imageSentence()`: the sentence of one image row (spec 15.11). It starts
/// with the image ref, or with "This app" for an image that Coolify builds.
pub fn image_sentence(
    security: &ResourceSecurity,
    image_ref: &str,
    scan: Option<ScanRef<'_>>,
    now: NaiveDateTime,
) -> String {
    let subject = if built_by_coolify(scan) {
        "This app"
    } else {
        image_ref
    };
    match security.status {
        SecurityStatusType::UpdateNow => update_now(security, subject, "A newer build"),
        SecurityStatusType::UpdateAvailable => image_update_available(security, subject),
        SecurityStatusType::NoFix => {
            if security.builder_image {
                no_fix(security, subject)
            } else {
                format!(
                    "{subject} has {} that attackers {}. {}",
                    problems(security.problem_count),
                    if security.any_exploited {
                        "are using"
                    } else {
                        "are likely to use"
                    },
                    no_fix_state(scan, security)
                )
            }
        }
        SecurityStatusType::Ok => format!("{subject} needs no action."),
        SecurityStatusType::CheckNeeded => format!(
            "{subject} needs another check: {}.",
            if is_check_reason(security.check_reason.as_deref()) {
                check_reason(
                    security.check_reason.as_deref().unwrap_or_default(),
                    &check_context(security),
                    now,
                )
            } else {
                "the last check is incomplete".to_owned()
            }
        ),
        SecurityStatusType::NotChecked => match security.check_reason.as_deref() {
            None | Some(NOT_SCANNED) => format!("{subject} has not been checked yet."),
            Some(reason) => check_reason(reason, &security.check_context, now),
        },
    }
}

/// `SecuritySentences::versionCheckRunning()`: the sentence of an image row while its queued
/// version check runs.
pub fn version_check_running(image_ref: &str, scan: Option<ScanRef<'_>>) -> String {
    let subject = if built_by_coolify(scan) {
        "this app"
    } else {
        image_ref
    };
    format!("Coolify is checking for newer versions of {subject}.")
}

/// `SecuritySentences::noFixAction()`: the short action text of a no_fix status for the team
/// page. `None` for other statuses.
pub fn no_fix_action(security: &ResourceSecurity) -> Option<&'static str> {
    if security.status != SecurityStatusType::NoFix {
        return None;
    }
    Some(if security.builder_image {
        "Needs a builder update in a Coolify release."
    } else {
        "No fix released yet. Coolify checks daily."
    })
}

/// Coolify builds the image (Dockerfile, Nixpacks, Railpack, static, Compose build), so its ref
/// is a commit tag.
fn built_by_coolify(scan: Option<ScanRef<'_>>) -> bool {
    let Some((scan, owner)) = scan else {
        return false;
    };
    scan.builds_image
        && matches!(
            actions::image_kind(scan, owner),
            "dockerfile" | "compose" | "builder"
        )
}

/// `SecuritySentences::noFixState()`: what a no_fix image row says about a fix.
fn no_fix_state(scan: Option<ScanRef<'_>>, security: &ResourceSecurity) -> String {
    let none = "No fixed version exists yet, and Coolify checks every day.".to_owned();
    let Some((scan, _)) = scan else {
        return none;
    };
    let findings = why_findings(scan, security);
    let with_fix = findings.iter().filter(|f| has_package_fix(f)).count();
    if findings.is_empty() || with_fix == 0 {
        return none;
    }
    let many = security.problem_count > 1;
    if with_fix < findings.len() {
        return "Some of them have a fix that the image does not include yet. Coolify checks every day.".to_owned();
    }
    match newest_image_without(scan, &findings) {
        Some(newest) => format!(
            "{} not in the newest {newest} image yet. Coolify checks every day.",
            if many { "The fixes are" } else { "The fix is" }
        ),
        None if many => {
            "A fix exists for each, but the image does not include them yet. Coolify checks every day.".to_owned()
        }
        None => {
            "A fix exists, but the image does not include it yet. Coolify checks every day."
                .to_owned()
        }
    }
}

/// "mysql 8.x" when the stored version check (ok or no_better) found no newer build or tag of
/// the line that has a fix of the findings.
fn newest_image_without(scan: &ImageScanRow, findings: &[Finding]) -> Option<String> {
    if !matches!(scan.candidate_status(), Some("ok" | "no_better")) {
        return None;
    }
    let image = ImageRef::parse(&scan.image_ref).ok()?;
    let name = ImageRef::parse(&image.qualified_name()).ok()?.to_string();
    let line = release_line(image.tag.as_deref(), image.tag.as_deref())?;

    let ids: HashSet<String> = findings
        .iter()
        .flat_map(|f| f.ids().map(str::to_lowercase).collect::<Vec<_>>())
        .collect();
    for entry in scan.candidate_entries() {
        if !matches!(
            entry.get("kind").and_then(Value::as_str),
            Some("same_tag" | "line")
        ) {
            continue;
        }
        if super::resource_security::list(entry.get("fixed_ids"))
            .iter()
            .any(|id| ids.contains(&value_string(id).to_lowercase()))
        {
            return None;
        }
    }
    Some(format!("{name} {line}"))
}

/// The stored findings of the why-list (by ID, display ID, or alias).
fn why_findings(scan: &ImageScanRow, security: &ResourceSecurity) -> Vec<Finding> {
    let why_ids: HashSet<String> = security
        .why
        .iter()
        .map(|w| w.display_id.to_lowercase())
        .collect();
    resource_scans::findings(scan)
        .into_iter()
        .filter(|finding| {
            std::iter::once(finding.id.as_str())
                .chain(std::iter::once(finding.display_id.as_str()))
                .chain(finding.aliases.iter().map(String::as_str))
                .any(|id| why_ids.contains(&id.to_lowercase()))
        })
        .collect()
}

fn has_package_fix(finding: &Finding) -> bool {
    finding.fix_state == "fixed" && filled(finding.fixed_in.as_deref())
}

/// `SecuritySentences::attackNote()`: the short note after the sentence of an image row that
/// does not mention attackers itself (update_available).
pub fn attack_note(security: &ResourceSecurity) -> Option<String> {
    if security.status != SecurityStatusType::UpdateAvailable || security.exploited_without_fix {
        return None;
    }
    Some(format!(
        "No known attacks on {}.",
        if security.fixed_by_primary.max(1) > 1 {
            "these problems"
        } else {
            "this problem"
        }
    ))
}

/// `SecuritySentences::noFixAdvice()`: the advice under a no_fix image row ("this app", "this
/// service", "this database"). `None` for other statuses and for builder images.
pub fn no_fix_advice(security: &ResourceSecurity, resource_noun: &str) -> Option<String> {
    if security.status != SecurityStatusType::NoFix || security.builder_image {
        return None;
    }
    Some(if security.problem_count > 1 {
        format!(
            "Until then, limit who can reach {resource_noun}, or ignore the problems if the affected parts are not used."
        )
    } else {
        format!(
            "Until then, limit who can reach {resource_noun}, or ignore the problem if the affected part is not used."
        )
    })
}

/// `SecuritySentences::provenance()`: how Coolify knows that the action of an image row fixes
/// its problems, or that no fix exists (spec 15.11), from the stored version and base checks
/// only. `fixed_in` is the first fixed version of a package upgrade. `None` when there is
/// nothing to say.
pub fn provenance(
    scan: &ImageScanRow,
    action: Option<&SecurityAction>,
    security: &ResourceSecurity,
    fixed_in: Option<&str>,
    now: NaiveDateTime,
) -> Option<String> {
    let Some(action) = action else {
        return (security.status == SecurityStatusType::NoFix && !security.builder_image)
            .then(|| no_fix_provenance(scan, security, now))
            .flatten();
    };
    let registry = registry_name(&scan.image_ref);
    match action.action_type {
        SecurityActionType::PullLatest | SecurityActionType::UpdateImage => {
            let entry = candidate_entry(scan, action)?;
            let tag = action
                .target
                .clone()
                .or_else(|| tag_of(action.reference.as_deref().unwrap_or_default()))
                .unwrap_or_default();
            let line = release_line(tag_of(&scan.image_ref).as_deref(), Some(&tag));
            let intro = if action.action_type == SecurityActionType::PullLatest {
                format!("{registry} has a newer build of {tag}.")
            } else {
                match line {
                    None => format!("{tag} is the newest release of this line on {registry}."),
                    Some(line) => format!("{tag} is the newest {line} release on {registry}."),
                }
            };
            Some(format!(
                "{intro} Coolify scanned it{}{}",
                scanned_at(entry.get("scanned_at"), now),
                candidate_findings(&entry)
            ))
        }
        SecurityActionType::ChangeBase => {
            let target = action.target.clone().unwrap_or_default();
            let base = scan.candidates.as_ref().and_then(|c| c.get("base"));
            let entries = base
                .and_then(|b| b.get("entries"))
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let entry = entries.into_iter().find(|entry| {
                entry.is_object()
                    && entry.get("kind").and_then(Value::as_str) == Some("line")
                    && entry.get("better") == Some(&Value::Bool(true))
                    && !entry
                        .get("ref")
                        .map(value_string)
                        .unwrap_or_default()
                        .is_empty()
                    && tag_of(&entry.get("ref").map(value_string).unwrap_or_default())
                        == tag_of(&target)
            })?;
            Some(format!(
                "{target} is the newest release of your base image. Coolify scanned it{}{}",
                scanned_at(entry.get("scanned_at"), now),
                candidate_findings(&entry)
            ))
        }
        SecurityActionType::UpgradePackage | SecurityActionType::UpgradeInDockerfile => {
            (filled(fixed_in) && filled(action.target.as_deref())).then(|| {
                format!(
                    "The advisory lists {} {} as the first fixed version.",
                    action.target.as_deref().unwrap_or_default(),
                    fixed_in.unwrap_or_default()
                )
            })
        }
        SecurityActionType::Rebuild => Some(match scan.base_image_ref.as_deref() {
            Some(base) if filled(Some(base)) => {
                format!("The current {base} image already has the fixed system packages.")
            }
            _ => "The current base image already has the fixed system packages.".to_owned(),
        }),
        SecurityActionType::RebuildInCi => None,
    }
}

/// `SecuritySentences::registryName($imageRef)`: "Docker Hub" for docker.io, "GitHub Container
/// Registry" for ghcr.io, "Quay" for quay.io, else the host.
pub fn registry_name(image_ref: &str) -> String {
    match ImageRef::parse(image_ref) {
        Err(_) => "the registry".to_owned(),
        Ok(parsed) => match parsed.registry.as_str() {
            "docker.io" => "Docker Hub".to_owned(),
            "ghcr.io" => "GitHub Container Registry".to_owned(),
            "quay.io" => "Quay".to_owned(),
            other => other.to_owned(),
        },
    }
}

/// `SecuritySentences::imagesNeedingUpdate()`: "2 of 3 images need an update". `None` when none
/// needs an update.
pub fn images_needing_update(count: usize, total: usize) -> Option<String> {
    if count < 1 {
        return None;
    }
    Some(format!(
        "{count} of {total} images {} an update",
        if count == 1 { "needs" } else { "need" }
    ))
}

/// `SecuritySentences::knownIssues()`: the count in the footer of an image row.
pub fn known_issues(known: i64) -> String {
    match known {
        k if k < 1 => "No known issues".to_owned(),
        1 => "1 known issue".to_owned(),
        k => format!("{k} known issues"),
    }
}

/// `SecuritySentences::shortDigest()`: the first 12 characters of the digest, without the
/// `sha256:` prefix.
pub fn short_digest(digest: Option<&str>) -> Option<String> {
    let digest = digest.unwrap_or_default().trim();
    if digest.is_empty() {
        return None;
    }
    let hex = match digest.find("sha256:") {
        Some(at) => &digest[at + "sha256:".len()..],
        None => digest,
    };
    let short: String = hex.chars().take(12).collect();
    (!short.is_empty() && short != "0").then_some(short)
}

/// "Coolify checked Docker Hub 2 hours ago. No newer 16.x release fixes this yet." Only for
/// images Coolify checks for newer versions.
fn no_fix_provenance(
    scan: &ImageScanRow,
    security: &ResourceSecurity,
    now: NaiveDateTime,
) -> Option<String> {
    let checked_at = match &scan.candidates {
        Some(Value::Object(map)) => map.get("checked_at").and_then(Value::as_str),
        _ => None,
    }?;
    if !matches!(scan.candidate_status(), Some("ok" | "no_better")) {
        return None;
    }

    let one = security.problem_count <= 1;
    let entries = scan.candidate_entries();
    let tag = tag_of(&scan.image_ref);
    let tag_text = tag.clone().unwrap_or_default();
    let checked = format!(
        "Coolify checked {}{}",
        registry_name(&scan.image_ref),
        scanned_at(Some(&Value::String(checked_at.to_owned())), now)
    );

    let mut newer: Vec<String> = Vec::new();
    for entry in &entries {
        let kind = entry.get("kind").and_then(Value::as_str);
        if !matches!(kind, Some("same_tag" | "line")) {
            continue;
        }
        let text = if kind == Some("same_tag") {
            format!("a newer build of {tag_text}")
        } else {
            tag_of(&entry.get("ref").map(value_string).unwrap_or_default()).unwrap_or_default()
        };
        if !newer.contains(&text) {
            newer.push(text);
        }
    }
    let line = release_line(tag.as_deref(), tag.as_deref());
    let mut sentences: Vec<String> = vec![if newer.is_empty() {
        format!(
            "{checked}: you already run the newest {}.",
            match &line {
                None => format!("build of {tag_text}"),
                Some(line) => format!("{line} version"),
            }
        )
    } else {
        format!(
            "{checked}: {}{} newer, but not better.",
            and_list(&newer),
            if newer.len() > 1 { " are" } else { " is" }
        )
    }];

    if let Some(packages) = no_fix_packages(scan, security, one) {
        sentences.push(packages);
    }

    let major = entries.iter().find(|entry| {
        entry.get("kind").and_then(Value::as_str) == Some("major")
            && entry
                .as_object()
                .is_some_and(|e| e.contains_key("fixes_problems"))
    });
    if let Some(major) = major {
        let fixed: HashSet<String> = super::resource_security::list(major.get("fixed_ids"))
            .iter()
            .map(|id| value_string(id).to_lowercase())
            .collect();
        let why_ids: Vec<String> = security
            .why
            .iter()
            .map(|w| w.display_id.to_lowercase())
            .collect();
        let fixed_why = why_ids.iter().filter(|id| fixed.contains(*id)).count();
        let reference = major.get("ref").map(value_string).unwrap_or_default();
        sentences.push(
            if major.get("fixes_problems") == Some(&Value::Bool(true)) && fixed_why > 0 {
                format!(
                    "{reference}, a new major version, fixes {}. Coolify does not change major versions for you, because they can need a data migration or code changes: read its upgrade notes first.",
                    if fixed_why < why_ids.len() {
                        "some of them"
                    } else if one {
                        "it"
                    } else {
                        "them"
                    }
                )
            } else {
                format!("The newest major version, {reference}, is not better either.")
            },
        );
    }

    Some(
        sentences
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" "),
    )
}

/// Where the problems of a no_fix image are, and whether a fixed version of those packages
/// exists upstream that the image does not include yet. `None` without known packages.
fn no_fix_packages(scan: &ImageScanRow, security: &ResourceSecurity, one: bool) -> Option<String> {
    let mut packages: Vec<String> = Vec::new();
    for why in &security.why {
        if !why.package.is_empty() && !packages.contains(&why.package) {
            packages.push(why.package.clone());
        }
    }
    if packages.is_empty() {
        return None;
    }

    let findings = why_findings(scan, security);
    let with_fix = findings.iter().filter(|f| has_package_fix(f)).count();
    let names = and_list(&packages);
    let lead = if security.problem_count > security.why.len() as i64 {
        "The listed problems are"
    } else if one {
        "The problem is"
    } else {
        "The problems are"
    };
    let place = format!("{lead} in {names}");
    Some(if findings.is_empty() {
        format!("{place}.")
    } else if with_fix == 0 {
        format!(
            "{place}, and no fixed version of {} exists yet.",
            if packages.len() > 1 { "them" } else { "it" }
        )
    } else if with_fix == findings.len() {
        format!("{place}. Fixed versions exist, but the image does not include them yet.")
    } else {
        format!("{place}. Some of them have a fixed version that the image does not include yet.")
    })
}

/// `SecuritySentences::releaseLine()`: "8.x" for the line of a tag, or "8.0.x" when both tags
/// have three or more version parts with the same minor. `None` for a tag that is not a version.
fn release_line(current_tag: Option<&str>, tag: Option<&str>) -> Option<String> {
    let parts = |value: Option<&str>| -> Vec<String> {
        value
            .and_then(|v| RELEASE_LINE.captures(v))
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().split('.').map(str::to_owned).collect())
            .unwrap_or_default()
    };
    let current = parts(current_tag);
    let candidate = parts(tag);
    if candidate.is_empty() {
        return None;
    }
    if candidate.len() >= 3
        && current.len() >= 3
        && current[0] == candidate[0]
        && current[1] == candidate[1]
        && current_tag != tag
    {
        return Some(format!("{}.{}.x", candidate[0], candidate[1]));
    }
    Some(format!("{}.x", candidate[0]))
}

/// "a", "a and b", "a, b, and c".
fn and_list(items: &[String]) -> String {
    if items.len() < 3 {
        items.join(" and ")
    } else {
        format!(
            "{}, and {}",
            items[..items.len() - 1].join(", "),
            items[items.len() - 1]
        )
    }
}

/// What the scan of an offered candidate found, without a count of its own: ": it fixes these
/// problems and adds none that attackers use." The other relevant findings it adds follow as
/// their own sentence.
fn candidate_findings(entry: &Value) -> String {
    let mut text = ": it fixes these problems and adds none that attackers use.".to_owned();
    if let Some(added) = entry.get("added").and_then(Value::as_i64)
        && added > 0
    {
        text.push_str(&if added == 1 {
            " It has 1 other problem that attackers are not known to use.".to_owned()
        } else {
            format!(" It has {added} other problems that attackers are not known to use.")
        });
    }
    text
}

/// The stored candidate of an update_image or pull_latest action.
fn candidate_entry(scan: &ImageScanRow, action: &SecurityAction) -> Option<Value> {
    let kind = if action.action_type == SecurityActionType::PullLatest {
        "same_tag"
    } else {
        "line"
    };
    update::update_candidates(scan).into_iter().find(|entry| {
        entry.get("kind").and_then(Value::as_str) == Some(kind)
            && action
                .reference
                .as_deref()
                .is_none_or(|r| entry.get("ref").and_then(Value::as_str) == Some(r))
    })
}

/// " 2 hours ago", or nothing when the time is not known.
fn scanned_at(time: Option<&Value>, now: NaiveDateTime) -> String {
    match time.and_then(Value::as_str) {
        Some(t) if !t.is_empty() => format!(" {}", ago(time, now)),
        _ => String::new(),
    }
}

fn tag_of(reference: &str) -> Option<String> {
    ImageRef::parse(reference).ok().and_then(|r| r.tag)
}

/// update_available in the words of an image row.
fn image_update_available(security: &ResourceSecurity, image_ref: &str) -> String {
    let action = security.primary();
    let fixed = security.fixed_by_primary.max(1);
    let only = if security.primary_critical > 0
        && security.primary_high == 0
        && security.primary_medium == 0
    {
        Some("critical")
    } else if security.primary_medium > 0
        && security.primary_critical == 0
        && security.primary_high == 0
    {
        Some("medium")
    } else {
        None
    };
    let parts = severity_parts(security);
    let problems = match (fixed, only) {
        (1, None) => "a serious problem".to_owned(),
        (1, Some(only)) => format!("a {only} problem"),
        (_, Some(only)) => format!("{fixed} {only} problems"),
        (_, None) => format!(
            "{fixed} {}{}",
            problems_noun(security, "problems"),
            if parts.is_empty() {
                String::new()
            } else {
                format!(" ({})", parts.join(", "))
            }
        ),
    };
    let it = if fixed == 1 { "it" } else { "them" };

    let mut sentence = action.map(|a| a.sentence.clone()).unwrap_or_default();
    let mut note = "";
    if let Some(stripped) = sentence.strip_suffix(MAJOR_NOTE) {
        sentence = stripped.to_owned();
        note = MAJOR_NOTE;
    }

    let text = match action {
        None => format!("{image_ref} has {problems}."),
        Some(a) if a.action_type == SecurityActionType::PullLatest && sentence != COOLIFY_TEXT => {
            format!(
                "{image_ref} has a newer build that fixes {problems}.{}",
                if sentence.contains("Redeploy this app to pull it.") {
                    " Redeploy this app to pull it."
                } else {
                    ""
                }
            )
        }
        Some(a)
            if a.action_type == SecurityActionType::UpdateImage
                && sentence.starts_with("Update to ")
                && sentence.ends_with('.') =>
        {
            format!(
                "{image_ref} has {problems}. {} to fix {it}.",
                &sentence[..sentence.len() - 1]
            )
        }
        Some(_) => format!("{image_ref} has {problems}. {sentence}"),
    };
    let eol = eol_sentence(security);
    php_trim(&format!(
        "{text}{note}{}",
        if eol.is_empty() {
            String::new()
        } else {
            format!(" {eol}")
        }
    ))
}

fn update_now(security: &ResourceSecurity, subject: &str, newer_build: &str) -> String {
    let problems = problems(security.problem_count);
    let used = if security.any_exploited {
        "are using"
    } else {
        "are likely to use"
    };
    let headline = format!("{subject} has {problems} that attackers {used}.");

    let Some(action) = security.primary() else {
        return headline;
    };
    let fixed = security.fixed_by_primary;
    let what = if fixed > 0 && fixed < security.problem_count {
        format!("{fixed} of them")
    } else if security.problem_count == 1 {
        "it".to_owned()
    } else {
        "them".to_owned()
    };
    let mut sentence = action.sentence.clone();
    if action.action_type == SecurityActionType::UpdateImage
        && sentence.starts_with("Update ")
        && sentence != COOLIFY_TEXT
        && sentence.ends_with('.')
    {
        sentence = format!("{} to fix {what}.", &sentence[..sentence.len() - 1]);
    } else if action.action_type == SecurityActionType::PullLatest && sentence != COOLIFY_TEXT {
        sentence = format!(
            "{newer_build} fixes {what}. {}",
            pull_instruction(&sentence)
        );
    }
    php_trim(&format!("{headline} {sentence} {}", eol_sentence(security)))
}

fn update_available(security: &ResourceSecurity) -> String {
    let action = security.primary();
    let fixed = security.fixed_by_primary.max(1);
    let parts = severity_parts(security);
    let breakdown = if parts.is_empty() {
        String::new()
    } else {
        format!(" ({})", parts.join(", "))
    };
    let noun = problems_noun(security, if fixed == 1 { "problem" } else { "problems" });
    let fixes = format!("This fixes {fixed} {noun}{breakdown}.");

    let mut sentence = action.map(|a| a.sentence.clone()).unwrap_or_default();
    if action.is_some_and(|a| a.action_type == SecurityActionType::PullLatest)
        && sentence != COOLIFY_TEXT
    {
        sentence = format!(
            "A newer build of this image is available. {}",
            pull_instruction(&sentence)
        );
    }
    let mut note = "";
    if let Some(stripped) = sentence.strip_suffix(MAJOR_NOTE) {
        sentence = stripped.to_owned();
        note = MAJOR_NOTE;
    }

    let mut text = format!("{}{note}", php_trim(&format!("{sentence} {fixes}")));
    let eol = eol_sentence(security);
    if !eol.is_empty() {
        text.push(' ');
        text.push_str(&eol);
    }
    if !security.exploited_without_fix {
        text.push_str(&format!(
            " Attackers are not known to use {}.",
            if fixed == 1 { "it" } else { "them" }
        ));
    }
    text
}

/// "1 critical", "3 high", "2 medium" for the fixes of the primary action, without the zero
/// ones.
fn severity_parts(security: &ResourceSecurity) -> Vec<String> {
    let mut parts = Vec::new();
    if security.primary_critical > 0 {
        parts.push(format!("{} critical", security.primary_critical));
    }
    if security.primary_high > 0 {
        parts.push(format!("{} high", security.primary_high));
    }
    if security.primary_medium > 0 {
        parts.push(format!("{} medium", security.primary_medium));
    }
    parts
}

/// "serious problems", or only "problems" when the fixes include medium ones.
fn problems_noun(security: &ResourceSecurity, noun: &str) -> String {
    if security.primary_medium > 0 {
        noun.to_owned()
    } else {
        format!("serious {noun}")
    }
}

fn no_fix(security: &ResourceSecurity, subject: &str) -> String {
    let problems = problems(security.problem_count);
    let used = if security.any_exploited {
        "are using"
    } else {
        "are likely to use"
    };
    if security.builder_image {
        return format!("{subject} has {problems} that attackers {used}. {BUILDER_TEXT}");
    }
    let ignore = if security.problem_count > 1 {
        "ignore the problems if the affected parts are not used"
    } else {
        "ignore the problem if the affected part is not used"
    };
    format!(
        "{subject} has {problems} that attackers {used}, and no fixed version exists yet. Coolify checks every day and tells you when a fix is out. Until then, limit who can reach this app, or {ignore}."
    )
}

/// The "pull it" half of a pull_latest sentence.
fn pull_instruction(sentence: &str) -> &'static str {
    if sentence.contains("Redeploy this app to pull it.") {
        "Redeploy this app to pull it."
    } else if sentence.ends_with(" and redeploy.") {
        "Pull it and redeploy."
    } else {
        "Pull it and restart."
    }
}

/// Said when the primary action moves the image off an end-of-life OS.
fn eol_sentence(security: &ResourceSecurity) -> String {
    let Some(eol) = &security.eol else {
        return String::new();
    };
    if eol.name.is_empty() || security.fact_int("action_stats.0.eol") < 1 {
        return String::new();
    }
    format!(
        "{}, which no longer gets security fixes.",
        php_trim(&format!(
            "It moves off {} {}",
            ucfirst(&eol.name),
            eol.version
        ))
    )
}

fn problems(count: i64) -> String {
    if count > 1 {
        format!("{count} security problems")
    } else {
        "a security problem".to_owned()
    }
}

/// The check context, with the time of the oldest result when it has none.
fn check_context(security: &ResourceSecurity) -> serde_json::Map<String, Value> {
    let mut context = security.check_context.clone();
    if context.get("time").is_none_or(Value::is_null)
        && let Some(at) = security.checked_at
    {
        context.insert("time".to_owned(), Value::String(iso8601(at)));
    }
    context
}

/// `CarbonImmutable::parse($time)->diffForHumans()`, "a while ago" when it cannot be read.
fn ago(time: Option<&Value>, now: NaiveDateTime) -> String {
    match time.and_then(Value::as_str).and_then(parse_time) {
        Some(at) => diff_for_humans(at, now),
        None => "a while ago".to_owned(),
    }
}

fn php_trim(value: &str) -> String {
    value
        .trim_matches([' ', '\t', '\n', '\r', '\0', '\x0B'])
        .to_owned()
}

#[cfg(test)]
mod tests {
    use chrono::NaiveDate;
    use coolify_db::image_scans::STATUS_COMPLETED;
    use serde_json::json;

    use super::super::super::resource_scans::tests::scan;
    use super::super::resource_security::{Eol, Why};
    use super::*;

    fn now() -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 11, 10)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap()
    }

    fn action(action_type: SecurityActionType, sentence: &str) -> SecurityAction {
        SecurityAction {
            action_type,
            sentence: sentence.to_owned(),
            target: Some("1.28".to_owned()),
            copy: None,
            fixed_ids: Vec::new(),
            button: true,
            image: Some("nginx:1.27".to_owned()),
            reference: Some("nginx:1.28".to_owned()),
            parts: Vec::new(),
        }
    }

    #[test]
    fn update_now_headlines() {
        let mut security = ResourceSecurity::new(SecurityStatusType::UpdateNow);
        security.problem_count = 2;
        security.fixed_by_primary = 1;
        security.any_exploited = true;
        security.actions = vec![action(SecurityActionType::UpdateImage, "Update to 1.28.")];
        assert_eq!(
            headline(&security, now()),
            "This image has 2 security problems that attackers are using. Update to 1.28 to fix 1 of them."
        );
        security.actions = vec![action(
            SecurityActionType::PullLatest,
            "Pull the newer build of nginx:1.27 and restart.",
        )];
        security.fixed_by_primary = 2;
        assert_eq!(
            image_sentence(&security, "nginx:1.27", None, now()),
            "nginx:1.27 has 2 security problems that attackers are using. A newer build fixes them. Pull it and restart."
        );
    }

    #[test]
    fn update_available_sentences() {
        let mut security = ResourceSecurity::new(SecurityStatusType::UpdateAvailable);
        security.fixed_by_primary = 3;
        security.primary_critical = 1;
        security.primary_high = 2;
        security.actions = vec![action(SecurityActionType::UpdateImage, "Update to 1.28.")];
        assert_eq!(
            headline(&security, now()),
            "Update to 1.28. This fixes 3 serious problems (1 critical, 2 high). Attackers are not known to use them."
        );
        assert_eq!(
            image_sentence(&security, "nginx:1.27", None, now()),
            "nginx:1.27 has 3 serious problems (1 critical, 2 high). Update to 1.28 to fix them."
        );
        assert_eq!(
            attack_note(&security).as_deref(),
            Some("No known attacks on these problems.")
        );
        security.fixed_by_primary = 1;
        security.primary_high = 0;
        assert_eq!(
            image_sentence(&security, "nginx:1.27", None, now()),
            "nginx:1.27 has a critical problem. Update to 1.28 to fix it."
        );
        security.eol = Some(Eol {
            name: "debian".to_owned(),
            version: "10".to_owned(),
            fixed: true,
        });
        security
            .facts
            .insert("action_stats".to_owned(), json!([{"eol": 1}]));
        assert_eq!(
            image_sentence(&security, "nginx:1.27", None, now()),
            "nginx:1.27 has a critical problem. Update to 1.28 to fix it. It moves off Debian 10, which no longer gets security fixes."
        );
    }

    #[test]
    fn no_fix_and_check_sentences() {
        let mut security = ResourceSecurity::new(SecurityStatusType::NoFix);
        security.problem_count = 1;
        assert_eq!(
            headline(&security, now()),
            "This image has a security problem that attackers are likely to use, and no fixed version exists yet. Coolify checks every day and tells you when a fix is out. Until then, limit who can reach this app, or ignore the problem if the affected part is not used."
        );
        assert_eq!(
            image_sentence(&security, "mysql:8.0", None, now()),
            "mysql:8.0 has a security problem that attackers are likely to use. No fixed version exists yet, and Coolify checks every day."
        );
        assert_eq!(
            no_fix_action(&security),
            Some("No fix released yet. Coolify checks daily.")
        );
        assert_eq!(
            no_fix_advice(&security, "this database").as_deref(),
            Some(
                "Until then, limit who can reach this database, or ignore the problem if the affected part is not used."
            )
        );

        let mut check = ResourceSecurity::new(SecurityStatusType::CheckNeeded);
        check.check_reason = Some(status::REASON_STALE.to_owned());
        check.checked_at = Some(now() - chrono::Duration::days(3));
        assert_eq!(
            headline(&check, now()),
            "Coolify needs to check this image again: the last check was 3 days ago."
        );
        check.check_reason = Some("bogus".to_owned());
        assert_eq!(
            image_sentence(&check, "a:1", None, now()),
            "a:1 needs another check: the last check is incomplete."
        );
        let off = ResourceSecurity::not_checked("server");
        assert_eq!(
            headline(&off, now()),
            "Image scanning is off for this server."
        );
        assert_eq!(
            image_sentence(
                &ResourceSecurity::not_checked(NOT_SCANNED),
                "a:1",
                None,
                now()
            ),
            "a:1 has not been checked yet."
        );
    }

    /// Cases of upstream's `SecuritySentencesTest`.
    #[test]
    fn upstream_sentence_cases() {
        let mut security = ResourceSecurity::new(SecurityStatusType::UpdateNow);
        security.problem_count = 3;
        security.fixed_by_primary = 2;
        security.any_exploited = true;
        security.actions = vec![action(
            SecurityActionType::UpdateImage,
            "Update to 16.4-alpine.",
        )];
        assert_eq!(
            headline(&security, now()),
            "This image has 3 security problems that attackers are using. Update to 16.4-alpine to fix 2 of them."
        );

        security.problem_count = 1;
        security.fixed_by_primary = 1;
        security.actions = vec![action(
            SecurityActionType::PullLatest,
            "Pull the newer build of postgres:16-alpine and redeploy.",
        )];
        assert_eq!(
            headline(&security, now()),
            "This image has a security problem that attackers are using. A newer build of this image fixes it. Pull it and redeploy."
        );
        security.actions = vec![action(
            SecurityActionType::Rebuild,
            "Rebuild without cache to get the fixed system packages.",
        )];
        assert_eq!(
            headline(&security, now()),
            "This image has a security problem that attackers are using. Rebuild without cache to get the fixed system packages."
        );

        let mut upgrade = ResourceSecurity::new(SecurityStatusType::UpdateAvailable);
        upgrade.fixed_by_primary = 1;
        upgrade.primary_critical = 1;
        upgrade.actions = vec![action(
            SecurityActionType::UpgradePackage,
            "Upgrade lodash to 5.0.0 or later, then redeploy. This is a major version change. Check for breaking changes.",
        )];
        assert_eq!(
            headline(&upgrade, now()),
            "Upgrade lodash to 5.0.0 or later, then redeploy. This fixes 1 serious problem (1 critical). This is a major version change. Check for breaking changes. Attackers are not known to use it."
        );

        let mut mixed = ResourceSecurity::new(SecurityStatusType::UpdateAvailable);
        mixed.fixed_by_primary = 6;
        mixed.primary_critical = 1;
        mixed.primary_high = 3;
        mixed.primary_medium = 2;
        mixed.actions = vec![action(
            SecurityActionType::UpdateImage,
            "Update to 16.4-alpine.",
        )];
        assert_eq!(
            headline(&mixed, now()),
            "Update to 16.4-alpine. This fixes 6 problems (1 critical, 3 high, 2 medium). Attackers are not known to use them."
        );

        assert_eq!(
            headline(
                &ResourceSecurity::new(SecurityStatusType::NotChecked),
                now()
            ),
            "This image has not been scanned yet."
        );
    }

    #[test]
    fn small_texts() {
        assert_eq!(
            badge(SecurityStatusType::UpdateAvailable),
            Some("Security update")
        );
        assert_eq!(badge(SecurityStatusType::Ok), None);
        assert_eq!(why_more(0), "");
        assert_eq!(why_more(1), "and 1 more problem that this update fixes");
        assert_eq!(
            images_needing_update(1, 3).as_deref(),
            Some("1 of 3 images needs an update")
        );
        assert_eq!(images_needing_update(0, 3), None);
        assert_eq!(known_issues(12), "12 known issues");
        assert_eq!(
            short_digest(Some("sha256:3f1a2b4c5d6e7f80")).as_deref(),
            Some("3f1a2b4c5d6e")
        );
        assert_eq!(short_digest(Some(" ")), None);
        assert_eq!(registry_name("ghcr.io/a/b:1"), "GitHub Container Registry");
        assert_eq!(registry_name("nginx"), "Docker Hub");
        assert_eq!(
            registry_name("registry.example.com/a:1"),
            "registry.example.com"
        );
        assert_eq!(
            database_update_confirmation("PostgreSQL", "15.0", "15.13"),
            "This is a patch release of PostgreSQL 15. Your data stays as it is. The database restarts and is unavailable for a few seconds."
        );
        assert_eq!(
            release_line(Some("8.0.36"), Some("8.0.40")).as_deref(),
            Some("8.0.x")
        );
        assert_eq!(
            release_line(Some("8.0"), Some("8.4")).as_deref(),
            Some("8.x")
        );
        assert_eq!(release_line(Some("latest"), Some("latest")), None);
        assert_eq!(
            and_list(&["a".into(), "b".into(), "c".into()]),
            "a, b, and c"
        );
        assert_eq!(
            version_check_running("nginx:1.27", None),
            "Coolify is checking for newer versions of nginx:1.27."
        );
    }

    #[test]
    fn provenance_of_an_update() {
        let mut row = scan("nginx:1.27.0", STATUS_COMPLETED, None);
        row.candidates = Some(
            json!({"status": "ok", "checked_at": "2026-11-10T10:00:00+00:00", "entries": [
                {"ref": "nginx:1.27.3", "kind": "line", "better": true, "scanned_at": "2026-11-10T10:00:00+00:00", "added": 2},
            ]}),
        );
        let mut update = action(SecurityActionType::UpdateImage, "Update to 1.27.3.");
        update.target = Some("1.27.3".to_owned());
        update.reference = Some("nginx:1.27.3".to_owned());
        let security = ResourceSecurity::new(SecurityStatusType::UpdateNow);
        assert_eq!(
            provenance(&row, Some(&update), &security, None, now()).as_deref(),
            Some(
                "1.27.3 is the newest 1.27.x release on Docker Hub. Coolify scanned it 2 hours ago: it fixes these problems and adds none that attackers use. It has 2 other problems that attackers are not known to use."
            )
        );

        let mut no_fix = ResourceSecurity::new(SecurityStatusType::NoFix);
        no_fix.problem_count = 1;
        no_fix.why = vec![Why {
            title: "t".to_owned(),
            display_id: "CVE-9".to_owned(),
            link: None,
            score: None,
            severity: "high".to_owned(),
            signal: Some("likely".to_owned()),
            package: "openssl".to_owned(),
            version: "3.0.1".to_owned(),
        }];
        row.candidates = Some(
            json!({"status": "no_better", "checked_at": "2026-11-10T10:00:00+00:00", "entries": []}),
        );
        assert_eq!(
            provenance(&row, None, &no_fix, None, now()).as_deref(),
            Some(
                "Coolify checked Docker Hub 2 hours ago: you already run the newest 1.x version. The problem is in openssl."
            )
        );
    }
}
