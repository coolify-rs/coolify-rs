//! `App\Services\ImageScan\SecurityCard`: what the main card of the Security page shows (spec
//! 15.7, 15.11): the status of the resource, and one row per image with its status badge,
//! sentence, primary action with its button or copy text, why-list, provenance, the other
//! actions, and the running digest. Buttons are only offered when the user may run them
//! ([`CardUser`]); the page actions authorize again.
//!
//! The rows serialize (`serde_json::to_value`) with upstream's array keys, so a page can hand
//! them to the browser as they are.

use std::collections::HashMap;
use std::time::Duration;

use chrono::NaiveDateTime;
use coolify_db::image_scans::{self, ImageScanRow, ScanOwner};
use md5::Digest as _;
use serde::Serialize;
use serde_json::{Value, json};

use super::super::image_ref::ImageRef;
use super::super::rules::{ExploitSignal, RelevanceRules};
use super::super::{cache, candidates, resource_scans, scan_image, text, update};
use super::action::{SecurityAction, value_string};
use super::resource_security::ResourceSecurity;
use super::status::{self, NOT_SCANNED, REASON_VERSIONS_UNCHECKED};
use super::{
    OwnedScan, SecurityActionType, SecurityStatusType, number_format_1, sentences, ucfirst,
    version_greater,
};
use crate::Jobs;

/// `SecurityCard::OTHER_ACTIONS_LIMIT`.
pub const OTHER_ACTIONS_LIMIT: usize = 5;

/// Image rows, worst first (the order of the team page).
const ROW_ORDER: [SecurityStatusType; 6] = [
    SecurityStatusType::UpdateNow,
    SecurityStatusType::UpdateAvailable,
    SecurityStatusType::NoFix,
    SecurityStatusType::CheckNeeded,
    SecurityStatusType::Ok,
    SecurityStatusType::NotChecked,
];

const PROBLEM_STATUSES: [SecurityStatusType; 4] = [
    SecurityStatusType::UpdateNow,
    SecurityStatusType::UpdateAvailable,
    SecurityStatusType::NoFix,
    SecurityStatusType::CheckNeeded,
];

fn err(e: coolify_db::DbError) -> String {
    e.to_string()
}

/// What the card may offer the user: `$user->can('update', $resource)`,
/// `$user->can('deploy', $resource)` and `$user->can('update', $server)` (for the registry login
/// link), decided by the caller with the policies of coolify-api.
pub struct CardUser<'a> {
    pub can_update: bool,
    pub can_deploy: bool,
    pub can_update_server: &'a (dyn Fn(i64) -> bool + Sync),
}

impl std::fmt::Debug for CardUser<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CardUser")
            .field("can_update", &self.can_update)
            .field("can_deploy", &self.can_deploy)
            .finish_non_exhaustive()
    }
}

/// The confirmation of a button (`ButtonData`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ButtonData {
    pub label: String,
    pub title: String,
    /// The page action the confirmation runs: `rebuild`, `updateImage('ref', 'uuid')`, or
    /// `updateImages('key')`.
    pub submit: String,
    pub actions: Vec<String>,
    pub warning: String,
}

/// One action of a row (`ActionData`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ActionData {
    pub key: String,
    #[serde(rename = "type")]
    pub action_type: String,
    pub sentence: String,
    pub copy: Option<String>,
    pub button: Option<ButtonData>,
}

/// One why row (`WhyData`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WhyData {
    pub title: String,
    pub display_id: String,
    pub link: Option<String>,
    pub score: String,
    pub cvss: Option<String>,
    #[serde(rename = "cvssNote")]
    pub cvss_note: Option<String>,
    pub severity: String,
    pub signal: Option<String>,
    pub exploited: bool,
    pub package: String,
    pub version: String,
}

/// A line for a version or base check that only the user can unblock.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VersionNote {
    pub text: String,
    pub link: Option<String>,
}

/// The image a security update set for a Compose service, while the compose file still names
/// the image it replaced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RowOverride {
    pub service: String,
    pub from: String,
    pub to: String,
}

/// One image row (`ImageRow`).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageRow {
    pub key: String,
    pub name: Option<String>,
    #[serde(rename = "ref")]
    pub reference: String,
    pub status: String,
    pub checking: bool,
    pub badge: Option<String>,
    pub badge_type: String,
    pub open: bool,
    pub sentence: String,
    pub attack_note: Option<String>,
    pub advice: Option<String>,
    pub lines: Vec<String>,
    pub primary: Option<ActionData>,
    pub merged_with: Option<String>,
    pub why: Vec<WhyData>,
    pub why_more: i64,
    pub provenance: Option<String>,
    pub others: Vec<ActionData>,
    pub more_actions: usize,
    pub version_notes: Vec<VersionNote>,
    pub filter: Option<String>,
    #[serde(rename = "override")]
    pub override_image: Option<RowOverride>,
}

/// The card (`SecurityCard::for()`).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Card {
    pub status: String,
    pub checking: bool,
    pub badge: Option<String>,
    pub badge_type: String,
    pub header: Option<String>,
    pub total: usize,
    pub single: bool,
    pub rows: Vec<ImageRow>,
}

/// The action of the deploy gate popover (`array{sentence: string, button: bool}`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GateAction {
    pub sentence: String,
    pub button: bool,
}

impl GateAction {
    pub fn to_value(&self) -> Value {
        json!({"sentence": self.sentence, "button": self.button})
    }

    fn from_value(value: &Value) -> Option<Self> {
        let sentence = value.get("sentence").map(value_string)?;
        (!sentence.trim().is_empty()).then(|| Self {
            sentence,
            button: super::action::php_bool(value.get("button")),
        })
    }
}

/// `SecurityCard::for($resource, $security, $owners, $scans, $user, $imageStatuses, $rulesFor)`.
///
/// - `owners`: the applications and databases of a service, else the resource
///   (`image_scans::owners`).
/// - `scans`: the current scans of the owners, with their owner (`resource_scans::current`).
/// - `image_statuses`: [`status::for_scan`] of the scans the caller computed, by scan UUID.
/// - `rules`: the relevance rules of the owners the caller keeps, by `ScanOwner::key()`; missing
///   ones are loaded.
#[allow(clippy::too_many_arguments)]
pub async fn card(
    jobs: &Jobs,
    resource: &ScanOwner,
    security: &ResourceSecurity,
    owners: &[ScanOwner],
    scans: &[OwnedScan],
    user: &CardUser<'_>,
    image_statuses: &HashMap<String, ResourceSecurity>,
    rules: &HashMap<String, RelevanceRules>,
    now: NaiveDateTime,
) -> Result<Card, String> {
    let status = security.status;
    let rows = images(
        jobs,
        resource,
        security,
        owners,
        scans,
        user,
        image_statuses,
        rules,
        now,
    )
    .await?;
    let total = rows.len();
    let needs_update = rows
        .iter()
        .filter(|r| r.status == "update_now" || r.status == "update_available")
        .count();
    let waiting: Vec<&ImageRow> = rows
        .iter()
        .filter(|r| r.status == SecurityStatusType::CheckNeeded.value())
        .collect();
    let checking = status == SecurityStatusType::CheckNeeded
        && !waiting.is_empty()
        && waiting.iter().all(|r| r.checking);
    Ok(Card {
        status: status.value().to_owned(),
        checking,
        badge: if checking {
            Some(sentences::CHECKING_BADGE.to_owned())
        } else {
            sentences::badge(status).map(str::to_owned)
        },
        badge_type: badge_type(status).to_owned(),
        header: if total > 1 {
            sentences::images_needing_update(needs_update, total)
        } else {
            None
        },
        total,
        single: total == 1,
        rows,
    })
}

/// One entry before it becomes a row.
struct RowEntry<'a> {
    key: String,
    scan: Option<&'a OwnedScan>,
    owner: ScanOwner,
    reference: String,
    name: Option<String>,
    security: ResourceSecurity,
}

/// `SecurityCard::images()`: one row per image the status counts. Worst first; the first row
/// with a problem is open, and a single image is always open. A service update that merges
/// several images has its button on the first row of the group; the other rows say which row
/// updates them.
#[allow(clippy::too_many_arguments)]
pub async fn images(
    jobs: &Jobs,
    resource: &ScanOwner,
    security: &ResourceSecurity,
    owners: &[ScanOwner],
    scans: &[OwnedScan],
    user: &CardUser<'_>,
    image_statuses: &HashMap<String, ResourceSecurity>,
    rules: &HashMap<String, RelevanceRules>,
    now: NaiveDateTime,
) -> Result<Vec<ImageRow>, String> {
    let db = &jobs.0.db;
    let is_service = resource.is_service();
    let status_owners: Vec<&ScanOwner> = if is_service {
        owners
            .iter()
            .filter(|o| resource_scans::is_scan_target(o))
            .collect()
    } else {
        owners.iter().collect()
    };
    let is_status_owner = |owner: &ScanOwner| status_owners.iter().any(|o| o.key() == owner.key());

    let mut digests: HashMap<String, Option<String>> = HashMap::new();
    for scan in scans {
        digests.entry(scan.owner.key()).or_insert_with(|| {
            let digest = scan
                .scan
                .repo_digest
                .as_deref()
                .filter(|d| !d.is_empty())
                .unwrap_or(&scan.scan.image_id);
            sentences::short_digest(Some(digest))
        });
    }

    let compose_services = resource_scans::compose_service_names(resource);
    let mut entries: Vec<RowEntry<'_>> = Vec::new();
    for owned in scans {
        if is_service && !is_status_owner(&owned.owner) {
            continue;
        }
        let security = match image_statuses.get(&owned.scan.uuid) {
            Some(security) => security.clone(),
            None => {
                let loaded;
                let rules = match rules.get(&owned.owner.key()) {
                    Some(rules) => rules,
                    None => {
                        loaded =
                            RelevanceRules::for_resource(db, &owned.owner, Jobs::now()).await?;
                        &loaded
                    }
                };
                status::for_scan(jobs, &owned.scan, &owned.owner, Some(rules)).await?
            }
        };
        let image_name = scan_image::image_name(&owned.scan.image_ref);
        entries.push(RowEntry {
            key: format!("scan-{}", owned.scan.uuid),
            scan: Some(owned),
            owner: owned.owner.clone(),
            reference: owned.scan.image_ref.clone(),
            name: if is_service {
                Some(owner_name(&owned.owner))
            } else {
                compose_services
                    .iter()
                    .find(|(image, _)| *image == image_name)
                    .map(|(_, service)| service.clone())
            },
            security,
        });
    }
    let not_checked = ResourceSecurity::not_checked(NOT_SCANNED);
    for owner in &status_owners {
        if is_service && !scans.iter().any(|s| s.owner.key() == owner.key()) {
            entries.push(RowEntry {
                key: format!("owner-{}", owner.uuid),
                scan: None,
                owner: (*owner).clone(),
                reference: owner.string("image").trim().to_owned(),
                name: Some(owner_name(owner)),
                security: not_checked.clone(),
            });
        }
    }
    let scan_rows: Vec<ImageScanRow> = scans.iter().map(|s| s.scan.clone()).collect();
    for image in resource_scans::unchecked_compose_refs(db, resource, &scan_rows, None).await? {
        let digest = md5::Md5::digest(image.name.as_deref().unwrap_or(&image.image_ref).as_bytes());
        entries.push(RowEntry {
            key: format!("compose-{}", hex(&digest)),
            scan: None,
            owner: resource.clone(),
            reference: image.image_ref.clone(),
            name: image.name.clone(),
            security: not_checked.clone(),
        });
    }

    let rank = |s: SecurityStatusType| ROW_ORDER.iter().position(|r| *r == s).unwrap_or(99);
    entries.sort_by(|a, b| {
        (
            rank(a.security.status),
            a.name.clone().unwrap_or_default().to_lowercase(),
            &a.reference,
        )
            .cmp(&(
                rank(b.security.status),
                b.name.clone().unwrap_or_default().to_lowercase(),
                &b.reference,
            ))
    });

    let merged: Vec<&SecurityAction> = security
        .actions
        .iter()
        .filter(|a| !a.parts.is_empty())
        .collect();
    let mut merged_stats: HashMap<String, serde_json::Map<String, Value>> = HashMap::new();
    for (index, action) in security.actions.iter().enumerate() {
        if !action.parts.is_empty() {
            merged_stats.insert(key(action), security.action_stats(index));
        }
    }
    let mut group_first: HashMap<String, String> = HashMap::new();
    let single = entries.len() == 1;
    let noun = if resource.is_application() {
        "this app"
    } else if is_service {
        "this service"
    } else {
        "this database"
    };
    let compose_overrides =
        if resource.is_application() && resource.build_pack() == Some("dockercompose") {
            update::active_compose_image_overrides(resource)
        } else {
            Vec::new()
        };
    let mut opened = false;
    let mut rows = Vec::new();
    for entry in entries {
        let image_security = &entry.security;
        let scan = entry.scan;
        let owner = &entry.owner;
        let row_status = image_security.status;
        let reference = entry.reference.clone();
        let name = entry.name.clone();
        let shows_actions = row_status.needs_update();

        let mut actions = Vec::new();
        if shows_actions {
            for (index, action) in image_security.actions.iter().enumerate() {
                actions.push(
                    action_data(
                        jobs,
                        resource,
                        action,
                        &image_security.action_stats(index),
                        owners,
                        user,
                        &digests,
                    )
                    .await?,
                );
            }
        }

        let primary_action = if shows_actions {
            image_security.primary()
        } else {
            None
        };
        let mut primary = actions.first().cloned();
        let mut merged_with = None;
        if let Some(group) = primary_action.and_then(|a| merged_group(&merged, a, owner)) {
            let group_key = key(group);
            if let Some(first) = group_first.get(&group_key) {
                merged_with = Some(first.clone());
                primary = None;
            } else {
                group_first.insert(
                    group_key.clone(),
                    name.clone().unwrap_or_else(|| reference.clone()),
                );
                primary = Some(
                    action_data(
                        jobs,
                        resource,
                        group,
                        &merged_stats.get(&group_key).cloned().unwrap_or_default(),
                        owners,
                        user,
                        &digests,
                    )
                    .await?,
                );
            }
        }

        // A single image is always open; in a list, the first row with a problem (spec 15.11).
        let open = single || (!opened && PROBLEM_STATUSES.contains(&row_status));
        opened = opened || open;
        let others: Vec<ActionData> = actions.iter().skip(1).cloned().collect();
        // The scan is done and only the queued version check is missing: say that it runs.
        let checking = row_status == SecurityStatusType::CheckNeeded
            && scan.is_some_and(|s| {
                image_security.check_reason.as_deref() == Some(REASON_VERSIONS_UNCHECKED)
                    && candidates::is_queued(&s.scan)
            });
        let scan_ref = scan.map(|s| (&s.scan, &s.owner));
        let shows_why = shows_actions || row_status == SecurityStatusType::NoFix;

        let mut lines = sentences::lines(image_security);
        lines.extend(eol_line(image_security));
        let provenance = match scan {
            Some(s) if shows_why => sentences::provenance(
                &s.scan,
                primary_action,
                image_security,
                provenance_fixed_in(&s.scan, primary_action).as_deref(),
                now,
            ),
            _ => None,
        };
        let version_notes = match scan {
            Some(s) => version_check_notes(jobs, std::slice::from_ref(&s.scan), user).await?,
            None => Vec::new(),
        };
        rows.push(ImageRow {
            key: entry.key.clone(),
            name: name.clone(),
            reference: reference.clone(),
            status: row_status.value().to_owned(),
            checking,
            badge: if checking {
                Some(sentences::CHECKING_BADGE.to_owned())
            } else {
                sentences::badge(row_status).map(str::to_owned)
            },
            badge_type: badge_type(row_status).to_owned(),
            open,
            sentence: if checking {
                sentences::version_check_running(&reference, scan_ref)
            } else {
                sentences::image_sentence(image_security, &reference, scan_ref, now)
            },
            attack_note: sentences::attack_note(image_security),
            advice: sentences::no_fix_advice(image_security, noun),
            lines,
            primary,
            merged_with,
            why: if shows_why {
                image_security
                    .why
                    .iter()
                    .map(|w| why_row(&w.to_value()))
                    .collect()
            } else {
                Vec::new()
            },
            why_more: if shows_actions {
                image_security.more_fixed.max(0)
            } else {
                0
            },
            provenance,
            more_actions: others.len().saturating_sub(OTHER_ACTIONS_LIMIT),
            others: others.into_iter().take(OTHER_ACTIONS_LIMIT).collect(),
            version_notes,
            filter: if single {
                None
            } else {
                Some(reference.clone())
            },
            override_image: name.as_ref().and_then(|n| {
                compose_overrides
                    .iter()
                    .find(|(service, _)| service == n)
                    .map(|(service, o)| RowOverride {
                        service: service.clone(),
                        from: o.from.clone(),
                        to: o.to.clone(),
                    })
            }),
        });
    }
    Ok(rows)
}

fn badge_type(status: SecurityStatusType) -> &'static str {
    match status {
        SecurityStatusType::UpdateNow => "error",
        SecurityStatusType::UpdateAvailable => "warning",
        _ => "neutral",
    }
}

/// `SecurityCard::cvssSeverity($score)`: 9.0 critical, 7.0 high, 4.0 medium, 0.1 low.
pub fn cvss_severity(score: f64) -> &'static str {
    if score >= 9.0 {
        "critical"
    } else if score >= 7.0 {
        "high"
    } else if score >= 4.0 {
        "medium"
    } else if score > 0.0 {
        "low"
    } else {
        "unknown"
    }
}

/// `SecurityCard::whyRow($why)`: the score shows next to the severity badge only when its CVSS
/// range has the same severity; else it is only in the tooltip ("CVSS 7.5").
pub fn why_row(why: &Value) -> WhyData {
    let severity = why
        .get("severity")
        .filter(|v| !v.is_null())
        .map_or_else(|| "unknown".to_owned(), value_string)
        .to_lowercase();
    let score = why.get("score").and_then(Value::as_f64);
    let cvss = score.map(number_format_1);
    let matches = score.is_some_and(|s| cvss_severity(s) == severity);
    let signal = why.get("signal").and_then(Value::as_str);
    WhyData {
        title: text::tidy_title(&why.get("title").map(value_string).unwrap_or_default()),
        display_id: why.get("display_id").map(value_string).unwrap_or_default(),
        link: why.get("link").and_then(Value::as_str).map(str::to_owned),
        score: match score {
            None => "No score".to_owned(),
            Some(s) => format!(
                "{} {}",
                number_format_1(s),
                ucfirst(&why.get("severity").map(value_string).unwrap_or_default())
            ),
        },
        cvss: if matches { cvss.clone() } else { None },
        cvss_note: cvss.map(|c| format!("CVSS {c}")),
        severity,
        signal: match signal {
            Some(s) if s == ExploitSignal::Exploited.value() => {
                Some("Used by attackers".to_owned())
            }
            Some(s) if s == ExploitSignal::Likely.value() => {
                Some("Likely to be exploited".to_owned())
            }
            _ => None,
        },
        exploited: signal == Some(ExploitSignal::Exploited.value()),
        package: why.get("package").map(value_string).unwrap_or_default(),
        version: why.get("version").map(value_string).unwrap_or_default(),
    }
}

/// The merged service update (from the status of the resource) that contains this image's
/// update.
fn merged_group<'a>(
    merged: &[&'a SecurityAction],
    action: &SecurityAction,
    owner: &ScanOwner,
) -> Option<&'a SecurityAction> {
    if action.action_type != SecurityActionType::UpdateImage {
        return None;
    }
    merged.iter().copied().find(|group| {
        group.parts.iter().any(|part| {
            part.owner_type == owner.morph
                && part.owner_id_string() == owner.id.to_string()
                && part.reference == action.reference
        })
    })
}

/// `SecurityCard::provenanceCounts()`: the first fixed version of a package upgrade, for the
/// provenance line: the highest fixed version of the findings of that package the action fixes.
fn provenance_fixed_in(scan: &ImageScanRow, action: Option<&SecurityAction>) -> Option<String> {
    let action = action?;
    if !matches!(
        action.action_type,
        SecurityActionType::UpgradePackage | SecurityActionType::UpgradeInDockerfile
    ) {
        return None;
    }
    let ids: Vec<String> = action.fixed_ids.iter().map(|i| i.to_lowercase()).collect();
    let mut fixed_in: Option<String> = None;
    for finding in resource_scans::findings(scan) {
        let Some(candidate) = finding.fixed_in.as_deref().filter(|f| !f.trim().is_empty()) else {
            continue;
        };
        if Some(finding.package.as_str()) == action.target.as_deref()
            && ids.contains(&finding.id.to_lowercase())
            && fixed_in.as_deref().is_none_or(|current| {
                version_greater(
                    candidate.trim_start_matches(['v', 'V']),
                    current.trim_start_matches(['v', 'V']),
                )
            })
        {
            fixed_in = Some(candidate.to_owned());
        }
    }
    fixed_in
}

/// `SecurityCard::versionCheckNotes($scans, $user)`: lines for version and base checks that
/// only the user can unblock. A registry login links to the server's registry logins when the
/// user may update the server.
pub async fn version_check_notes(
    jobs: &Jobs,
    scans: &[ImageScanRow],
    user: &CardUser<'_>,
) -> Result<Vec<VersionNote>, String> {
    let mut notes: Vec<VersionNote> = Vec::new();
    for scan in scans {
        let base = scan
            .candidates
            .as_ref()
            .and_then(|c| c.get("base"))
            .and_then(Value::as_object);
        let candidates_error = scan.candidates.as_ref().and_then(|c| c.get("error"));
        let mut checks: Vec<(bool, Option<String>, Option<Value>)> = vec![(
            false,
            scan.candidate_status().map(str::to_owned),
            candidates_error.cloned(),
        )];
        let base_ref = scan.base_image_ref.as_deref().unwrap_or_default();
        if let Some(base) = base
            && !base_ref.trim().is_empty()
            && base.get("image").and_then(Value::as_str) == Some(base_ref)
        {
            checks.push((
                true,
                base.get("status")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                base.get("error").cloned(),
            ));
        }

        let mut server: Option<Option<image_scans::views::ServerBrief>> = None;
        for (is_base, check_status, error) in checks {
            if !status::is_blocked_check(check_status.as_deref(), error.as_ref()) {
                continue;
            }
            if server.is_none() {
                server = Some(
                    image_scans::views::servers_brief(&jobs.0.db, &[scan.server_id])
                        .await
                        .map_err(err)?
                        .into_iter()
                        .next(),
                );
            }
            let server = server.clone().flatten();
            let Some(text) =
                candidates::message_on(scan, is_base, server.as_ref().map(|s| s.name.as_str()))
            else {
                continue;
            };
            if notes.iter().any(|n| n.text == text) {
                continue;
            }
            notes.push(VersionNote {
                text,
                link: match &server {
                    Some(server)
                        if check_status.as_deref() == Some("auth_required")
                            && (user.can_update_server)(server.id) =>
                    {
                        Some(format!("/server/{}/registries", server.uuid))
                    }
                    _ => None,
                },
            });
        }
    }
    Ok(notes)
}

/// `SecurityCard::key($action)`: a stable key of an action: its type, the image and candidate,
/// and every part of a merged update.
pub fn key(action: &SecurityAction) -> String {
    let parts: Vec<Value> = action
        .parts
        .iter()
        .map(|part| {
            json!([
                part.owner_type,
                part.owner_id_string(),
                part.reference.clone().unwrap_or_default()
            ])
        })
        .collect();
    let encoded = coolify_compat::php_json::encode(&json!([
        action.action_type.value(),
        action.image,
        action.reference,
        action.target,
        parts,
    ]));
    let digest = sha1::Sha1::digest(encoded.as_bytes());
    hex(&digest)[..16].to_owned()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// `SecurityCard::runningDeployment($application)`: the deployment whose image runs now: the
/// latest finished deployment that is not a preview, as `(id, commit)`.
pub async fn running_deployment(
    jobs: &Jobs,
    application: &ScanOwner,
) -> Result<Option<(i64, Option<String>)>, String> {
    image_scans::latest_finished_deployment(&jobs.0.db, application.id)
        .await
        .map_err(err)
}

/// `SecurityCard::gateAction($application, $deployment)`: the action the deploy gate popover
/// shows for a blocked deployment: the action stored with the gate result, else the primary
/// action of the blocked image (of a Compose app, the image whose primary action fixes the most
/// findings with an exploit signal), computed once per version of its scans. Without a scan of
/// the deployment, the primary action of the main card.
pub async fn gate_action(
    jobs: &Jobs,
    application: &ScanOwner,
    deployment_id: i64,
    scan_gate_result: Option<&Value>,
) -> Result<Option<GateAction>, String> {
    if let Some(stored) = scan_gate_result
        .and_then(|r| r.get("action"))
        .filter(|a| a.is_object())
        .and_then(GateAction::from_value)
    {
        return Ok(Some(stored));
    }

    let scans =
        image_scans::views::deployment_completed_scans(&jobs.0.db, application.id, deployment_id)
            .await
            .map_err(err)?;
    if scans.is_empty() {
        let security = status::stored(jobs, application).await?;
        return Ok(action_summary(security.primary()));
    }

    // The version changes with the scan, its version and base checks, and its stored status.
    let versions: Vec<Value> = scans
        .iter()
        .map(|s| {
            json!([
                s.id,
                s.updated_at
                    .map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string())
                    .unwrap_or_default(),
                s.candidates,
                s.summary
            ])
        })
        .collect();
    let digest =
        md5::Md5::digest(coolify_compat::php_json::encode(&Value::Array(versions)).as_bytes());
    let cache_key = format!("image-scan:gate-action:{deployment_id}:{}", hex(&digest));
    if let Some(cached) = cache::get(&cache_key) {
        return Ok(cached.get("action").and_then(GateAction::from_value));
    }

    let mut best: Option<(SecurityAction, (i64, i64, i64))> = None;
    for scan in &scans {
        if let Some((action, rank)) = ranked_action(jobs, scan, application).await?
            && best.as_ref().is_none_or(|(_, best_rank)| rank > *best_rank)
        {
            best = Some((action, rank));
        }
    }
    let action = action_summary(best.as_ref().map(|(a, _)| a));
    cache::put(
        &cache_key,
        json!({"action": action.as_ref().map(GateAction::to_value)}),
        Duration::from_secs(86_400),
    );
    Ok(action)
}

/// `SecurityCard::blockedImageAction($scan)`: the primary action of a blocked image, stored with
/// the gate result so the popover does not compute it again.
pub async fn blocked_image_action(
    jobs: &Jobs,
    scan: &ImageScanRow,
    owner: &ScanOwner,
) -> Result<Option<GateAction>, String> {
    Ok(action_summary(
        ranked_action(jobs, scan, owner)
            .await?
            .as_ref()
            .map(|(a, _)| a),
    ))
}

async fn ranked_action(
    jobs: &Jobs,
    scan: &ImageScanRow,
    owner: &ScanOwner,
) -> Result<Option<(SecurityAction, (i64, i64, i64))>, String> {
    let security = status::for_scan(jobs, scan, owner, None).await?;
    let Some(primary) = security.primary().cloned() else {
        return Ok(None);
    };
    let stats = security.action_stats(0);
    let int = |k: &str| super::action::php_int(stats.get(k));
    Ok(Some((
        primary,
        (int("signal"), int("critical") + int("high"), int("medium")),
    )))
}

fn action_summary(action: Option<&SecurityAction>) -> Option<GateAction> {
    action.map(|a| GateAction {
        sentence: a.sentence.clone(),
        button: a.button,
    })
}

/// `Application::git_based()`.
fn git_based(application: &ScanOwner) -> bool {
    let dockerfile = application.string("dockerfile");
    if !dockerfile.is_empty() && dockerfile != "0" {
        return false;
    }
    application.build_pack() != Some("dockerimage")
}

/// `SecurityCard::knowsRunningCommit()`: a Git app can be rebuilt only from a known commit:
/// "HEAD" or no commit would build whatever the branch has now.
pub fn knows_running_commit(application: &ScanOwner, commit: Option<&str>) -> bool {
    let commit = commit.unwrap_or_default().trim();
    !git_based(application) || (!commit.is_empty() && commit.to_uppercase() != "HEAD")
}

/// `SecurityCard::shortCommit()`.
pub fn short_commit(commit: Option<&str>) -> String {
    let commit = commit.unwrap_or_default().trim();
    if commit.is_empty() {
        "HEAD".to_owned()
    } else {
        commit.chars().take(7).collect()
    }
}

/// The end-of-life OS, when the sentence does not say it already.
fn eol_line(security: &ResourceSecurity) -> Vec<String> {
    let Some(eol) = &security.eol else {
        return Vec::new();
    };
    if eol.name.is_empty() {
        return Vec::new();
    }
    let said = security.status.needs_update() && security.fact_int("action_stats.0.eol") > 0;
    if said {
        return Vec::new();
    }
    vec![format!(
        "{}, which no longer gets security fixes.",
        format!("This image uses {} {}", ucfirst(&eol.name), eol.version).trim()
    )]
}

/// `SecurityCard::action()`.
async fn action_data(
    jobs: &Jobs,
    resource: &ScanOwner,
    action: &SecurityAction,
    stats: &serde_json::Map<String, Value>,
    owners: &[ScanOwner],
    user: &CardUser<'_>,
    digests: &HashMap<String, Option<String>>,
) -> Result<ActionData, String> {
    let button = if action.button && resource.id != 0 {
        button(jobs, resource, action, stats, owners, user, digests).await?
    } else {
        None
    };
    Ok(ActionData {
        key: key(action),
        action_type: action.action_type.value().to_owned(),
        sentence: action.sentence.clone(),
        copy: action.copy.clone(),
        button,
    })
}

/// `SecurityCard::button()`.
async fn button(
    jobs: &Jobs,
    resource: &ScanOwner,
    action: &SecurityAction,
    stats: &serde_json::Map<String, Value>,
    owners: &[ScanOwner],
    user: &CardUser<'_>,
    digests: &HashMap<String, Option<String>>,
) -> Result<Option<ButtonData>, String> {
    if action.action_type == SecurityActionType::Rebuild {
        if !resource.is_application() || !user.can_deploy {
            return Ok(None);
        }
        let Some((_, commit)) = running_deployment(jobs, resource).await? else {
            return Ok(None);
        };
        if !knows_running_commit(resource, commit.as_deref()) {
            return Ok(None);
        }
        let commit = short_commit(commit.as_deref());
        return Ok(Some(ButtonData {
            label: "Rebuild".to_owned(),
            title: "Rebuild without cache?".to_owned(),
            submit: "rebuild".to_owned(),
            actions: vec![
                format!("Rebuild commit {commit} without cache."),
                "Redeploy the app.".to_owned(),
            ],
            warning: format!("Coolify rebuilds commit {commit} without cache and redeploys it."),
        }));
    }

    if !matches!(
        action.action_type,
        SecurityActionType::UpdateImage | SecurityActionType::PullLatest
    ) || !user.can_update
    {
        return Ok(None);
    }

    if !action.parts.is_empty() {
        return merged_button(jobs, resource, action, owners, digests).await;
    }

    let image = action.image.clone().unwrap_or_default();
    let reference = action.reference.clone().unwrap_or_default();
    if image.is_empty() || reference.is_empty() {
        return Ok(None);
    }
    let owner = if resource.is_service() {
        let owner_type = stats.get("owner_type").map(value_string);
        let owner_id = stats.get("owner_id").map(value_string).unwrap_or_default();
        owners
            .iter()
            .find(|o| Some(o.morph.to_owned()) == owner_type && o.id.to_string() == owner_id)
    } else {
        Some(resource)
    };
    let Some(owner) = owner else {
        return Ok(None);
    };
    let owner_uuid = if resource.is_service() {
        owner.uuid.clone()
    } else {
        String::new()
    };
    let submit = format!("updateImage({}, {})", quote(&reference), quote(&owner_uuid));
    let restart = restart_line(resource, owners);

    if action.action_type == SecurityActionType::PullLatest {
        return Ok(Some(ButtonData {
            label: "Pull latest and restart".to_owned(),
            title: format!("Pull the newest build of {image}?"),
            submit,
            actions: vec![
                format!("Pull the build of {image} that Coolify scanned."),
                restart,
            ],
            warning: format!(
                "Coolify pulls exactly the build that it scanned, not a build pushed after the check. The settings keep {image}, so later restarts pull its newest build as usual."
            ),
        }));
    }

    let tag = action.target.clone().unwrap_or_default();
    let mut changes = if resource.is_service() {
        update::sub_resources_using(jobs, resource, &image)
            .await?
            .iter()
            .map(|sub| {
                format!(
                    "{}: {}",
                    owner_name(sub),
                    change(
                        &image,
                        digests.get(&sub.key()).cloned().flatten().as_deref(),
                        &reference
                    )
                )
            })
            .collect::<Vec<_>>()
    } else {
        vec![change(
            &image,
            digests.get(&resource.key()).cloned().flatten().as_deref(),
            &reference,
        )]
    };
    changes.push(restart);
    Ok(Some(ButtonData {
        label: format!("Update to {tag}"),
        title: format!("Update to {tag}?"),
        submit,
        actions: changes,
        warning: update_warning(resource, &image, &reference),
    }))
}

/// `SecurityCard::mergedButton()`: one confirmation for every image of a merged service update.
async fn merged_button(
    jobs: &Jobs,
    resource: &ScanOwner,
    action: &SecurityAction,
    owners: &[ScanOwner],
    digests: &HashMap<String, Option<String>>,
) -> Result<Option<ButtonData>, String> {
    if !resource.is_service() {
        return Ok(None);
    }
    let mut changes: Vec<(String, String)> = Vec::new();
    for part in &action.parts {
        let image = part.image.clone().unwrap_or_default();
        let reference = part.reference.clone().unwrap_or_default();
        if image.is_empty() || reference.is_empty() {
            return Ok(None);
        }
        for sub in update::sub_resources_using(jobs, resource, &image).await? {
            let text = format!(
                "{}: {}",
                owner_name(&sub),
                change(
                    &image,
                    digests.get(&sub.key()).cloned().flatten().as_deref(),
                    &reference
                )
            );
            match changes.iter_mut().find(|(k, _)| *k == sub.key()) {
                Some(slot) => slot.1 = text,
                None => changes.push((sub.key(), text)),
            }
        }
    }
    if changes.is_empty() {
        return Ok(None);
    }
    let tag = action.target.clone().unwrap_or_default();
    let mut actions: Vec<String> = changes.into_iter().map(|(_, text)| text).collect();
    actions.push(restart_line(resource, owners));
    Ok(Some(ButtonData {
        label: format!("Update to {tag}"),
        title: format!("Update to {tag}?"),
        submit: format!("updateImages({})", quote(&key(action))),
        actions,
        warning: update_warning(resource, "", ""),
    }))
}

/// The name a database type has in the update confirmation.
fn database_name(morph: &str) -> Option<&'static str> {
    Some(match morph {
        r"App\Models\StandalonePostgresql" => "PostgreSQL",
        r"App\Models\StandaloneRedis" => "Redis",
        r"App\Models\StandaloneMongodb" => "MongoDB",
        r"App\Models\StandaloneMysql" => "MySQL",
        r"App\Models\StandaloneMariadb" => "MariaDB",
        r"App\Models\StandaloneKeydb" => "KeyDB",
        r"App\Models\StandaloneDragonfly" => "Dragonfly",
        r"App\Models\StandaloneClickhouse" => "ClickHouse",
        _ => return None,
    })
}

fn update_warning(resource: &ScanOwner, image: &str, reference: &str) -> String {
    if resource.is_service() {
        return "One-click services pin versions that work together. Check the service template before you update.".to_owned();
    }
    if resource.is_application() && resource.build_pack() == Some("dockercompose") {
        return "Coolify redeploys the app with the new image. Your compose file in Git does not change: Coolify keeps using the new image until the file names another one.".to_owned();
    }
    if resource.is_application() {
        return "Coolify redeploys the app with the new image.".to_owned();
    }
    let from_tag = tag(image);
    let to_tag = tag(reference);
    if let (Some(name), Some(from), Some(to)) = (database_name(resource.morph), from_tag, to_tag)
        && major(&from).is_some()
        && major(&from) == major(&to)
    {
        return sentences::database_update_confirmation(name, &from, &to);
    }
    "Check the release notes. Some updates need a data migration.".to_owned()
}

/// What restarts. A service restarts every container, so the line names its images.
fn restart_line(resource: &ScanOwner, owners: &[ScanOwner]) -> String {
    if resource.is_service() {
        let names: Vec<String> = owners
            .iter()
            .map(owner_name)
            .filter(|n| !n.is_empty() && n != "0")
            .collect();
        return if names.is_empty() {
            "Pull the images and restart the service.".to_owned()
        } else {
            format!(
                "Pull the images and restart the service: {}.",
                names.join(", ")
            )
        };
    }
    if resource.is_application() {
        "Redeploy the app.".to_owned()
    } else {
        "Restart the database.".to_owned()
    }
}

/// "postgres:15.0 3f1a2b4c5d6e → postgres:15.13".
fn change(image: &str, digest: Option<&str>, reference: &str) -> String {
    format!(
        "{} → {reference}",
        format!("{image} {}", digest.unwrap_or_default()).trim()
    )
}

/// `(string) ($owner->human_name ?: $owner->name)`.
fn owner_name(owner: &ScanOwner) -> String {
    let human = owner.string("human_name");
    if human.is_empty() || human == "0" {
        owner.name()
    } else {
        human
    }
}

fn tag(reference: &str) -> Option<String> {
    ImageRef::parse(reference).ok().and_then(|r| r.tag)
}

fn major(tag: &str) -> Option<String> {
    let digits: String = tag
        .strip_prefix(['v', 'V'])
        .unwrap_or(tag)
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    (!digits.is_empty()).then_some(digits)
}

/// A single-quoted argument for the confirmation modal's submit action.
fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\\', "\\\\").replace('\'', "\\'"))
}

#[cfg(test)]
mod tests {
    use coolify_db::image_scans::{APPLICATION_MORPH, SERVICE_MORPH};

    use super::super::super::resource_scans::tests::owner;
    use super::super::action::ActionPart;
    use super::*;

    #[test]
    fn why_rows_show_a_matching_score() {
        let row = why_row(
            &json!({"title": "Bad thing", "display_id": "CVE-1", "link": null, "score": 7.5, "severity": "medium", "signal": "exploited", "package": "p", "version": "1"}),
        );
        assert_eq!(row.score, "7.5 Medium");
        assert_eq!(row.cvss, None);
        assert_eq!(row.cvss_note.as_deref(), Some("CVSS 7.5"));
        assert_eq!(row.signal.as_deref(), Some("Used by attackers"));
        assert!(row.exploited);
        let high = why_row(
            &json!({"title": "x", "display_id": "CVE-2", "score": 7.5, "severity": "high"}),
        );
        assert_eq!(high.cvss.as_deref(), Some("7.5"));
        let none = why_row(
            &json!({"title": "x", "display_id": "CVE-3", "score": null, "severity": "low"}),
        );
        assert_eq!(none.score, "No score");
        assert_eq!(cvss_severity(9.0), "critical");
        assert_eq!(cvss_severity(0.0), "unknown");
    }

    #[test]
    fn keys_are_stable_and_depend_on_the_parts() {
        let mut action = SecurityAction {
            action_type: SecurityActionType::UpdateImage,
            sentence: "Update to 2.".to_owned(),
            target: Some("2".to_owned()),
            copy: None,
            fixed_ids: Vec::new(),
            button: true,
            image: Some("n8nio/n8n:1".to_owned()),
            reference: Some("n8nio/n8n:2".to_owned()),
            parts: Vec::new(),
        };
        let plain = key(&action);
        assert_eq!(plain.len(), 16);
        assert_eq!(plain, key(&action.clone()));
        action.parts.push(ActionPart {
            owner_type: "x".to_owned(),
            owner_id: json!(1),
            image: None,
            reference: Some("n8nio/n8n:2".to_owned()),
        });
        assert_ne!(plain, key(&action));
    }

    #[test]
    fn small_parts_of_the_buttons() {
        assert_eq!(quote("it's"), r"'it\'s'");
        assert_eq!(
            change("postgres:15.0", Some("3f1a2b4c5d6e"), "postgres:15.13"),
            "postgres:15.0 3f1a2b4c5d6e → postgres:15.13"
        );
        assert_eq!(
            change("postgres:15.0", None, "postgres:15.13"),
            "postgres:15.0 → postgres:15.13"
        );
        assert_eq!(short_commit(Some("abcdef0123")), "abcdef0");
        assert_eq!(short_commit(None), "HEAD");
        let git = owner(APPLICATION_MORPH, json!({"build_pack": "nixpacks"}));
        assert!(!knows_running_commit(&git, Some("HEAD")));
        assert!(knows_running_commit(&git, Some("abc")));
        let image = owner(APPLICATION_MORPH, json!({"build_pack": "dockerimage"}));
        assert!(knows_running_commit(&image, None));
        let database = owner(r"App\Models\StandalonePostgresql", json!({}));
        assert_eq!(
            update_warning(&database, "postgres:15.0", "postgres:15.13"),
            "This is a patch release of PostgreSQL 15. Your data stays as it is. The database restarts and is unavailable for a few seconds."
        );
        assert_eq!(
            update_warning(&database, "postgres:15.0", "postgres:16.1"),
            "Check the release notes. Some updates need a data migration."
        );
        let service = owner(SERVICE_MORPH, json!({}));
        let parts = vec![
            owner(
                r"App\Models\ServiceApplication",
                json!({"name": "n8n", "human_name": "N8n"}),
            ),
            owner(r"App\Models\ServiceDatabase", json!({"name": "postgres"})),
        ];
        assert_eq!(
            restart_line(&service, &parts),
            "Pull the images and restart the service: N8n, postgres."
        );
    }
}
