//! `VulnerabilitiesController` (app/Http/Controllers/Api): image scan results, with the same
//! data and permissions as the Security pages of the UI.
//!
//! - `GET /vulnerabilities`: the resources of the team with their stored security status.
//! - `GET /applications|services|databases/{uuid}/vulnerabilities`: the status, the scanned
//!   images, and the findings of one resource.
//!
//! The view models are `coolify_jobs::image_scan::views`; they need the background jobs (the
//! scan engine reads servers through them), so these routes answer 501 without them.

use std::collections::HashMap;

use axum::Extension;
use axum::extract::{Path, State};
use axum::http::{StatusCode, Uri};
use axum::response::IntoResponse;
use coolify_db::image_scans::{self, ImageScanIgnoreRow, ScanOwner};
use coolify_domain::grants::AccessLevel;
use coolify_jobs::Jobs;
use coolify_jobs::image_scan::finding::Finding;
use coolify_jobs::image_scan::rules::RelevanceRules;
use coolify_jobs::image_scan::views::team::{self, TeamVulnerabilities};
use coolify_jobs::image_scan::views::{
    self as views, OwnedScan, ResourceSecurity, actions, iso8601, status,
};
use coolify_jobs::image_scan::{policy, resource_scans};
use serde_json::{Map, Value, json};

use crate::auth::AuthCtx;
use crate::input::{Input, query_param};
use crate::response::{ApiError, ApiResult, PhpJson, ok};
use crate::state::AppState;
use crate::validation::{CUSTOM_API_MESSAGES, Field, Rule, validate};

const FINDING_STATES: [&str; 3] = ["fixed", "no_fix", "ignored"];
const SEVERITIES: [&str; 5] = ["critical", "high", "medium", "low", "unknown"];

fn severity_rank(severity: &str) -> i64 {
    match severity {
        "critical" => 4,
        "high" => 3,
        "medium" => 2,
        "low" => 1,
        _ => 0,
    }
}

fn jobs(state: &AppState) -> Result<&Jobs, ApiError> {
    state.jobs.as_ref().ok_or(ApiError::NotPorted)
}

fn internal(e: String) -> ApiError {
    tracing::error!(error = %e, "vulnerabilities request failed");
    ApiError::Internal
}

/// `$this->authorize('view', $resource)` (the policies of `Application`, `Service`, and the
/// standalone databases): read access on the resource. The instance database (id 0 of the root
/// team) needs an owner or admin of the root team.
pub(crate) fn can_access(ctx: &AuthCtx, resource: &ScanOwner, level: AccessLevel) -> bool {
    let environment_id = resource
        .record
        .get("environment_id")
        .and_then(Value::as_i64);
    if !resource.is_application() && !resource.is_service() && resource.id == 0 {
        return ctx.can_instance_database(level);
    }
    ctx.can_resource(level, environment_id, &resource.uuid)
}

/// `listParameter($request, $name, $allowed)`: a comma-separated query parameter, checked
/// against the allowed values. Empty when not sent.
fn list_parameter(uri: &Uri, name: &str, allowed: &[&str]) -> Result<Vec<String>, ApiError> {
    let Some(raw) = query_param(uri, name).filter(|r| !r.is_empty()) else {
        return Ok(Vec::new());
    };
    let mut values: Vec<String> = Vec::new();
    for value in raw.to_lowercase().split(',') {
        let value = value.trim();
        if !value.is_empty() && !values.iter().any(|v| v == value) {
            values.push(value.to_owned());
        }
    }
    if values.iter().any(|v| !allowed.contains(&v.as_str())) {
        let mut errors = Map::new();
        errors.insert(
            name.to_owned(),
            json!([format!("Allowed values: {}.", allowed.join(", "))]),
        );
        return Err(ApiError::Validation(errors));
    }
    Ok(values)
}

/// `security($security)`: the security status without the internal facts and record IDs.
fn security_value(security: &ResourceSecurity) -> Value {
    json!({
        "status": security.status.value(),
        "label": team::status_label(security.status),
        "action": team::action(security, views::now()).map(|a| a.text),
        "check_reason": security.check_reason,
        "problem_count": security.problem_count,
        "exploited_count": security.exploited_count,
        "any_exploited": security.any_exploited,
        "critical_count": security.critical_count,
        "high_count": security.high_count,
        "medium_count": security.medium_count,
        "eol": security.eol.as_ref().map(|e| e.to_value()),
        "checked_at": security.checked_at.map(iso8601),
        "actions": security.actions.iter().map(|action| json!({
            "type": action.action_type.value(),
            "sentence": action.sentence,
            "target": action.target,
            "copy": action.copy,
            "image": action.image,
            "ref": action.reference,
        })).collect::<Vec<_>>(),
    })
}

/// `GET /api/v1/vulnerabilities`.
pub async fn index(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    uri: Uri,
) -> ApiResult {
    let statuses = list_parameter(&uri, "status", &team::STATUS_ORDER)?;
    let jobs = jobs(&state)?;
    let mut loaded = TeamVulnerabilities::load(jobs, ctx.team_id)
        .await
        .map_err(internal)?;
    // The resource models of upstream hide what a member or token limited to some projects
    // cannot see.
    loaded.retain(|entry| can_access(&ctx, &entry.resource, AccessLevel::Read));
    let rank = |s: &str| {
        team::STATUS_ORDER
            .iter()
            .position(|o| *o == s)
            .unwrap_or(usize::MAX)
    };
    let mut entries: Vec<&team::TeamEntry> = loaded
        .entries
        .iter()
        .filter(|e| statuses.is_empty() || statuses.iter().any(|s| s == e.security.status.value()))
        .collect();
    entries.sort_by(|a, b| {
        rank(a.security.status.value())
            .cmp(&rank(b.security.status.value()))
            .then_with(|| a.resource.name().cmp(&b.resource.name()))
    });
    let resources: Vec<Value> = entries
        .iter()
        .map(|entry| {
            json!({
                "uuid": entry.resource.uuid,
                "name": entry.resource.name(),
                "type": entry.resource_type(),
                "project_uuid": entry.place.as_ref().map(|p| p.project_uuid.clone()),
                "environment_uuid": entry.place.as_ref().map(|p| p.environment_uuid.clone()),
                "server_uuid": entry.server.as_ref().map(|s| s.uuid.clone()),
                "images": {"update": entry.images.update, "total": entry.images.total},
                "security": security_value(&entry.security),
            })
        })
        .collect();
    Ok(ok(json!({"resources": resources})).into_response())
}

/// `GET /api/v1/applications/{uuid}/vulnerabilities`.
pub async fn application(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    uri: Uri,
) -> ApiResult {
    show(&state, &ctx, "application", &uuid, &uri).await
}

/// `GET /api/v1/services/{uuid}/vulnerabilities`.
pub async fn service(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    uri: Uri,
) -> ApiResult {
    show(&state, &ctx, "service", &uuid, &uri).await
}

/// `GET /api/v1/databases/{uuid}/vulnerabilities`.
pub async fn database(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    uri: Uri,
) -> ApiResult {
    show(&state, &ctx, "database", &uuid, &uri).await
}

fn query_fields() -> Vec<Field> {
    vec![
        Field {
            name: "state",
            rules: vec![Rule::Nullable, Rule::String],
        },
        Field {
            name: "severity",
            rules: vec![Rule::Nullable, Rule::String],
        },
        Field {
            name: "image",
            rules: vec![Rule::Nullable, Rule::String, Rule::Max(1000)],
        },
        Field {
            name: "search",
            rules: vec![Rule::Nullable, Rule::String, Rule::Max(200)],
        },
        Field {
            name: "page",
            rules: vec![Rule::Nullable, Rule::Integer, Rule::MinNum(1)],
        },
        Field {
            name: "per_page",
            rules: vec![
                Rule::Nullable,
                Rule::Integer,
                Rule::MinNum(1),
                Rule::MaxNum(200),
            ],
        },
    ]
}

const QUERY_KEYS: [&str; 6] = ["state", "severity", "image", "search", "page", "per_page"];

/// `VulnerabilitiesController::show($request, $type)`.
async fn show(state: &AppState, ctx: &AuthCtx, kind: &str, uuid: &str, uri: &Uri) -> ApiResult {
    let mut query = Map::new();
    for key in QUERY_KEYS {
        if let Some(value) = query_param(uri, key) {
            query.insert(key.to_owned(), Value::String(value));
        }
    }
    validate(
        &Input(query),
        &query_fields(),
        CUSTOM_API_MESSAGES,
        &QUERY_KEYS,
    )?;
    let states = list_parameter(uri, "state", &FINDING_STATES)?;
    let severities = list_parameter(uri, "severity", &SEVERITIES)?;

    let jobs = jobs(state)?;
    let Some(resource) = image_scans::find_resource(&state.db, kind, uuid, &[ctx.team_id]).await?
    else {
        return Ok(PhpJson(
            StatusCode::NOT_FOUND,
            json!({"message": format!("{} not found.", ucfirst(kind))}),
        )
        .into_response());
    };
    if !can_access(ctx, &resource, AccessLevel::Read) {
        return Err(ApiError::Unauthorized);
    }

    let off_state = policy::off_state(&state.db, &resource, None)
        .await
        .map_err(internal)?;
    let scans = if off_state.is_none() {
        current_owned(jobs, &resource).await.map_err(internal)?
    } else {
        Vec::new()
    };
    let now = views::now();
    let mut rules: HashMap<String, RelevanceRules> = HashMap::new();
    for scan in &scans {
        if let std::collections::hash_map::Entry::Vacant(e) = rules.entry(scan.owner.key()) {
            e.insert(
                RelevanceRules::for_resource(&state.db, &scan.owner, now)
                    .await
                    .map_err(internal)?,
            );
        }
    }
    let mut image_statuses: HashMap<String, ResourceSecurity> = HashMap::new();
    for scan in &scans {
        let security =
            status::for_scan(jobs, &scan.scan, &scan.owner, rules.get(&scan.owner.key()))
                .await
                .map_err(internal)?;
        image_statuses.insert(scan.scan.uuid.clone(), security);
    }
    let all_findings = findings(jobs, &scans, &rules).await.map_err(internal)?;

    let image = query_param(uri, "image")
        .unwrap_or_default()
        .trim()
        .to_owned();
    let search = query_param(uri, "search")
        .unwrap_or_default()
        .trim()
        .to_lowercase();
    let filtered: Vec<Value> = all_findings
        .into_iter()
        .filter(|row| {
            let text = |key: &str| {
                row.get(key)
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned()
            };
            let haystack = std::iter::once(text("id"))
                .chain(
                    row.get("aliases")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .map(str::to_owned),
                )
                .chain([text("package"), text("version")])
                .collect::<Vec<_>>()
                .join(" ")
                .to_lowercase();
            (states.is_empty() || states.contains(&text("state")))
                && (severities.is_empty() || severities.contains(&text("severity")))
                && (image.is_empty() || text("image_ref") == image)
                && (search.is_empty() || haystack.contains(&search))
        })
        .collect();

    let per_page = query_param(uri, "per_page")
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(50);
    let page = query_param(uri, "page")
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(1);
    let total = filtered.len();
    let page_rows: Vec<Value> = filtered
        .into_iter()
        .skip((page.saturating_sub(1)).saturating_mul(per_page))
        .take(per_page)
        .collect();

    let security = if scans.is_empty() {
        Value::Null
    } else {
        security_value(
            &status::for_resource(jobs, &resource, Some(&scans), &image_statuses)
                .await
                .map_err(internal)?,
        )
    };
    let images: Vec<Value> = scans
        .iter()
        .map(|s| {
            json!({
                "scan_uuid": s.scan.uuid,
                "owner_uuid": s.owner.uuid,
                "image_ref": s.scan.image_ref,
                "scan_status": s.scan.status,
                "error_code": s.scan.error_code,
                "scanned_at": s.scan.scanned_at.map(iso8601),
                "truncated": s.scan.summary.as_ref().and_then(|v| v.get("truncated")).is_some_and(php_bool),
                "security_status": image_statuses.get(&s.scan.uuid).map(|x| x.status.value()),
            })
        })
        .collect();

    Ok(ok(json!({
        "uuid": resource.uuid,
        "type": kind,
        "off_reason": off_state,
        "security": security,
        "images": images,
        "findings": page_rows,
        "pagination": {"page": page, "per_page": per_page, "total": total},
    }))
    .into_response())
}

fn php_bool(value: &Value) -> bool {
    match value {
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty() && s != "0",
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
        Value::Null => false,
    }
}

fn ucfirst(value: &str) -> String {
    let mut chars = value.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// `ResourceScans::current($resource)` with the owner of each scan (`$scan->resource`).
pub(crate) async fn current_owned(
    jobs: &Jobs,
    resource: &ScanOwner,
) -> Result<Vec<OwnedScan>, String> {
    views::current_owned(jobs, resource).await
}

/// `findings($scans, $rulesFor)`: the findings of the current scans, worst first, in the same
/// fix states as the UI.
async fn findings(
    jobs: &Jobs,
    scans: &[OwnedScan],
    rules: &HashMap<String, RelevanceRules>,
) -> Result<Vec<Value>, String> {
    let mut rows: Vec<(bool, i64, f64, Value)> = Vec::new();
    let default_rules = RelevanceRules::default();
    for scan in scans {
        let owner_rules = rules.get(&scan.owner.key()).unwrap_or(&default_rules);
        let classify = actions::fix_states(jobs, &scan.scan, &scan.owner, owner_rules).await?;
        for finding in resource_scans::findings(&scan.scan) {
            let ignore = owner_rules.ignores.matches(&finding);
            let state = match ignore {
                Some(_) => "ignored",
                None => classify.state(&finding).value(),
            };
            let mut row = finding_value(&finding, state, ignore);
            if let Value::Object(map) = &mut row {
                map.insert("image_ref".to_owned(), json!(scan.scan.image_ref));
                map.insert("owner_uuid".to_owned(), json!(scan.owner.uuid));
            }
            rows.push((
                finding.kev,
                severity_rank(&team::severity_of(&finding)),
                finding.risk.unwrap_or(0.0),
                row,
            ));
        }
    }
    rows.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| b.1.cmp(&a.1))
            .then_with(|| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal))
    });
    Ok(rows.into_iter().map(|(_, _, _, row)| row).collect())
}

/// `finding($finding, $state, $ignore)`.
fn finding_value(finding: &Finding, state: &str, ignore: Option<&ImageScanIgnoreRow>) -> Value {
    json!({
        "id": finding.id,
        "display_id": finding.display_id,
        "aliases": finding.aliases,
        "title": finding.title,
        "severity": team::severity_of(finding),
        "package": finding.package,
        "version": finding.version,
        "package_type": finding.r#type,
        "fixed_in": finding.fixed_in,
        "state": state,
        "ignore": ignore.map(|ignore| json!({
            "reason": ignore.reason,
            "expires_at": ignore.expires_at.map(iso8601),
            "team_wide": ignore.resource_type.is_none(),
        })),
        "kev": finding.kev,
        "epss": finding.epss,
        "risk": finding.risk,
        "cvss_score": finding.cvss_score,
        "advisory_url": resource_scans::advisory_url(&finding.display_id),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_parameters_are_checked() {
        let uri: Uri = "/api/v1/vulnerabilities?status=Update_Now,%20ok,ok"
            .parse()
            .unwrap();
        assert_eq!(
            list_parameter(&uri, "status", &team::STATUS_ORDER).unwrap(),
            vec!["update_now", "ok"]
        );
        let bad: Uri = "/api/v1/vulnerabilities?status=bad".parse().unwrap();
        assert!(list_parameter(&bad, "status", &team::STATUS_ORDER).is_err());
        let none: Uri = "/api/v1/vulnerabilities".parse().unwrap();
        assert!(
            list_parameter(&none, "status", &team::STATUS_ORDER)
                .unwrap()
                .is_empty()
        );
    }
}
