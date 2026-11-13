//! `App\Livewire\ImageScan\Vulnerabilities`: the Security section of a resource (spec 15.7):
//! the main card with the status, the sentence, and the action that fixes the most, then the
//! findings of the images the resource runs (spec 6.2). The resource is reloaded and authorized
//! on every request.
//!
//! The browser sends the list state (fix state, filters, view, page) with every render, like the
//! public properties of the Livewire component, and polls while a scan runs.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::time::Duration;

use axum::extract::{Path, State};
use axum::response::IntoResponse;
use axum::routing::post;
use axum::{Extension, Json, Router};
use chrono::NaiveDateTime;
use coolify_db::applications as apps;
use coolify_db::destinations::{self, Kind as DestinationKind};
use coolify_db::image_scans::{self, ImageScanIgnoreRow, ImageScanRow, NewIgnore, ScanOwner};
use coolify_domain::grants::AccessLevel;
use coolify_jobs::Jobs;
use coolify_jobs::applications::queue::{QueueOptions, Queued};
use coolify_jobs::image_scan::error::ErrorCode;
use coolify_jobs::image_scan::finding::Finding;
use coolify_jobs::image_scan::rules::{self, RelevanceRules};
use coolify_jobs::image_scan::scan_jobs::{self, QUEUED_STAGE, ScanResourceImages};
use coolify_jobs::image_scan::update::UpdatePart;
use coolify_jobs::image_scan::views::export::{self, ExportRow};
use coolify_jobs::image_scan::views::team::severity_of;
use coolify_jobs::image_scan::views::{
    self as views, OwnedScan, ResourceSecurity, SecurityAction, actions, card, status,
};
use coolify_jobs::image_scan::{ScanServer, cache, policy, resource_scans, scan_image};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::{
    count_text, day_date_time, download, formatted_date, internal, jobs, load_resource, not_found,
    number_format, slug_or, strnatcmp, toast_error, toast_success,
};
use crate::audit::{self, Meta};
use crate::auth::AuthCtx;
use crate::response::{ApiError, ApiResult, ok};
use crate::routes::image_scan::{
    ImageUpdateOutcome, update_resource_image, update_resource_images,
};
use crate::routes::vulnerabilities::can_access;
use crate::state::AppState;

const SEVERITIES: [&str; 5] = ["critical", "high", "medium", "low", "unknown"];
const VIEWS: [&str; 2] = ["package", "cve"];
const EXPIRES: [&str; 3] = ["30", "90", "never"];
/// "Start scan" covers the wait in the queue and for the scan lock of the server.
const CHECKPOINTS: [&str; 4] = [
    "Start scan",
    "Update database",
    "Read image",
    "Find vulnerabilities",
];

fn severity_rank(severity: &str) -> i64 {
    match severity {
        "critical" => 4,
        "high" => 3,
        "medium" => 2,
        "low" => 1,
        _ => 0,
    }
}

/// `STAGE_CHECKPOINT`: progress stage of `ScanImage` => index in [`CHECKPOINTS`].
fn stage_checkpoint(stage: &str) -> usize {
    match stage {
        s if s == QUEUED_STAGE => 0,
        "database" => 1,
        "pull" | "inspect" | "sbom" => 2,
        "vulnerabilities" => 3,
        _ => 0,
    }
}

pub fn routes() -> Router<AppState> {
    let p = |s: &str| format!("/ui/image-scan/{{kind}}/{{uuid}}/vulnerabilities{s}");
    Router::new()
        .route(&p(""), post(render))
        .route(&p("/scan-now"), post(scan_now))
        .route(&p("/update-image"), post(update_image))
        .route(&p("/update-images"), post(update_images))
        .route(&p("/rebuild"), post(rebuild))
        .route(&p("/ignore"), post(ignore))
        .route(&p("/remove-image-override"), post(remove_image_override))
        .route(&p("/export"), post(export_list))
}

/// The public properties of the component that the browser keeps.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct ListState {
    pub fix_state: String,
    pub severities: Vec<String>,
    pub images: Vec<String>,
    pub view: String,
    pub search: String,
    pub show_ignored: bool,
    pub page: usize,
    pub per_page: usize,
    /// `copyList` / `downloadList`: markdown, ids, csv, json.
    pub format: String,
}

impl ListState {
    /// `array_key_exists($this->fixState, self::FIX_STATES) ? … : 'fixed'`.
    fn normalize(&mut self) {
        if !export::FIX_STATES.iter().any(|(k, _)| *k == self.fix_state) {
            self.fix_state = "fixed".to_owned();
        }
        if !VIEWS.contains(&self.view.as_str()) {
            self.view = "package".to_owned();
        }
        self.per_page = if self.per_page == 0 {
            10
        } else {
            self.per_page.clamp(1, 100)
        };
        self.page = self.page.max(1);
    }

    fn has_filters(&self) -> bool {
        !self.severities.is_empty() || !self.images.is_empty() || !self.search.trim().is_empty()
    }
}

/// One finding of the current scans (`findingRows()`).
#[derive(Debug, Clone)]
struct Row {
    key: String,
    finding: Finding,
    state: String,
    ignore: Option<ImageScanIgnoreRow>,
    image: String,
    owner: String,
    base: Option<String>,
    never_allowed: bool,
    package_never_allowed: bool,
}

/// What one request loads once.
struct Page {
    resource: ScanOwner,
    owners: Vec<ScanOwner>,
    scans: Vec<OwnedScan>,
    rules: HashMap<String, RelevanceRules>,
}

async fn load_page(
    state: &AppState,
    jobs: &Jobs,
    ctx: &AuthCtx,
    kind: &str,
    uuid: &str,
) -> Result<Page, ApiError> {
    let resource = load_resource(state, ctx, kind, uuid).await?;
    let owners = image_scans::owners(&state.db, &resource).await?;
    let scans = views::current_owned(jobs, &resource)
        .await
        .map_err(internal)?;
    let now = views::now();
    let mut rules = HashMap::new();
    for scan in &scans {
        if let std::collections::hash_map::Entry::Vacant(e) = rules.entry(scan.owner.key()) {
            let r = RelevanceRules::for_resource(&state.db, &scan.owner, now)
                .await
                .map_err(internal)?;
            e.insert(r);
        }
    }
    Ok(Page {
        resource,
        owners,
        scans,
        rules,
    })
}

/// A finding whose ignore must expire (spec 15.2): an exploit signal, and not a CPE-only OS
/// match.
fn has_exploit_signal(finding: &Finding) -> bool {
    rules::exploit_signal(finding).is_some() && !rules::is_cpe_only_os_match(finding)
}

/// `baseIds($scan)`: the IDs of the findings that the current base image of a Dockerfile build
/// has too, from the last base check.
fn base_ids(scan: &ImageScanRow) -> HashSet<String> {
    let Some(base_ref) = scan
        .base_image_ref
        .as_deref()
        .filter(|b| !b.trim().is_empty())
    else {
        return HashSet::new();
    };
    let Some(base) = scan
        .candidates
        .as_ref()
        .and_then(|c| c.get("base"))
        .filter(|b| b.is_object())
    else {
        return HashSet::new();
    };
    if base.get("image").and_then(Value::as_str) != Some(base_ref) {
        return HashSet::new();
    }
    base.get("attributable_ids")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// `findingRows()`: one row per finding of the current scans, with its fix state and the ignore
/// that matches it.
async fn finding_rows(jobs: &Jobs, page: &Page) -> Result<Vec<Row>, ApiError> {
    let default_rules = RelevanceRules::default();
    let mut rows = Vec::new();
    for scan in &page.scans {
        let rules = page.rules.get(&scan.owner.key()).unwrap_or(&default_rules);
        let classify = actions::fix_states(jobs, &scan.scan, &scan.owner, rules)
            .await
            .map_err(internal)?;
        let base = base_ids(&scan.scan);
        for (index, finding) in resource_scans::findings(&scan.scan).into_iter().enumerate() {
            let from_base =
                !base.is_empty() && finding.ids().any(|id| base.contains(&id.to_lowercase()));
            rows.push(Row {
                key: format!("{}-{index}", scan.scan.uuid),
                state: classify.state(&finding).value().to_owned(),
                ignore: rules.ignores.matches(&finding).cloned(),
                image: scan.scan.image_ref.clone(),
                owner: scan.owner.uuid.clone(),
                base: if from_base {
                    scan.scan.base_image_ref.clone()
                } else {
                    None
                },
                finding,
                never_allowed: true,
                package_never_allowed: true,
            });
        }
    }
    // The ignore modal offers "Never" only when no finding the ignore would match has an
    // exploit signal.
    let mut signal_ids: HashSet<(String, String)> = HashSet::new();
    let mut signal_packages: HashSet<(String, String)> = HashSet::new();
    for row in &rows {
        if has_exploit_signal(&row.finding) {
            for id in row.finding.ids() {
                signal_ids.insert((row.owner.clone(), id.to_lowercase()));
            }
            signal_packages.insert((row.owner.clone(), row.finding.package.clone()));
        }
    }
    for row in &mut rows {
        row.never_allowed =
            !signal_ids.contains(&(row.owner.clone(), row.finding.id.to_lowercase()));
        row.package_never_allowed =
            !signal_packages.contains(&(row.owner.clone(), row.finding.package.clone()));
    }
    Ok(rows)
}

/// `filterRows()`: severity, image, and search.
fn filter_rows(rows: Vec<Row>, list: &ListState) -> Vec<Row> {
    let search = list.search.trim().to_lowercase();
    let severities: Vec<&String> = list
        .severities
        .iter()
        .filter(|s| SEVERITIES.contains(&s.as_str()))
        .collect();
    rows.into_iter()
        .filter(|row| {
            let f = &row.finding;
            if !severities.is_empty() && !severities.iter().any(|s| **s == severity_of(f)) {
                return false;
            }
            if !list.images.is_empty() && !list.images.contains(&row.image) {
                return false;
            }
            if search.is_empty() {
                return true;
            }
            let haystack = std::iter::once(f.id.as_str())
                .chain(f.aliases.iter().map(String::as_str))
                .chain([f.package.as_str(), f.version.as_str()])
                .collect::<Vec<_>>()
                .join(" ")
                .to_lowercase();
            haystack.contains(&search)
        })
        .collect()
}

/// `sortRows()`: known exploited first, then severity, then risk.
fn sort_rows(mut rows: Vec<Row>) -> Vec<Row> {
    rows.sort_by(|a, b| {
        b.finding
            .kev
            .cmp(&a.finding.kev)
            .then_with(|| {
                severity_rank(&severity_of(&b.finding))
                    .cmp(&severity_rank(&severity_of(&a.finding)))
            })
            .then_with(|| {
                b.finding
                    .risk
                    .unwrap_or(0.0)
                    .partial_cmp(&a.finding.risk.unwrap_or(0.0))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    });
    rows
}

/// `shownRows()`: the ignored ones, or the current fix state, with the filters, worst first.
fn shown_rows(all: &[Row], list: &ListState) -> Vec<Row> {
    let rows: Vec<Row> = all
        .iter()
        .filter(|row| {
            if list.show_ignored {
                row.ignore.is_some()
            } else {
                row.ignore.is_none() && row.state == list.fix_state
            }
        })
        .cloned()
        .collect();
    sort_rows(filter_rows(rows, list))
}

/// One row of the findings table (`partials/finding-row.blade.php`).
fn row_value(row: &Row) -> Value {
    let f = &row.finding;
    let aliases: Vec<Value> = f
        .aliases
        .iter()
        .filter(|a| **a != f.id && resource_scans::is_advisory_id(a))
        .map(|a| json!({"id": a, "url": resource_scans::advisory_url(a)}))
        .collect();
    json!({
        "key": row.key,
        "id": f.id,
        "url": resource_scans::advisory_url(&f.id),
        "aliases": aliases,
        "package": f.package,
        "version": f.version,
        "fixed_in": f.fixed_in.as_deref().filter(|v| !v.is_empty()),
        "severity": f.severity.to_lowercase(),
        "kev": f.kev,
        "summary": f.summary,
        "likelihood": resource_scans::exploit_likelihood(f.epss),
        "base": row.base.as_deref().filter(|b| !b.trim().is_empty()),
        "image": row.image,
        "owner": row.owner,
        "never_allowed": row.never_allowed,
        "ignore": row.ignore.as_ref().map(|i| json!({
            "reason": i.reason,
            "until": i.expires_at.map(formatted_date),
        })),
    })
}

/// `packageGroups()`: one row per package version of an image, worst first.
fn package_groups(rows: &[Row]) -> Vec<Value> {
    struct Group<'a> {
        key: String,
        package: String,
        version: String,
        image: String,
        owner: String,
        severity: String,
        kev: bool,
        never_allowed: bool,
        fixed: Vec<String>,
        rows: Vec<&'a Row>,
    }
    let mut groups: Vec<Group> = Vec::new();
    for row in rows {
        let f = &row.finding;
        let key = format!("{}\0{}\0{}\0{}", row.owner, row.image, f.package, f.version);
        let index = match groups.iter().position(|g| g.key == key) {
            Some(i) => i,
            None => {
                groups.push(Group {
                    key,
                    package: f.package.clone(),
                    version: f.version.clone(),
                    image: row.image.clone(),
                    owner: row.owner.clone(),
                    severity: "unknown".to_owned(),
                    kev: false,
                    never_allowed: row.package_never_allowed,
                    fixed: Vec::new(),
                    rows: Vec::new(),
                });
                groups.len() - 1
            }
        };
        let group = &mut groups[index];
        group.rows.push(row);
        group.kev = group.kev || f.kev;
        let severity = severity_of(f);
        if severity_rank(&severity) > severity_rank(&group.severity) {
            group.severity = severity;
        }
        if let Some(fixed) = f.fixed_in.as_deref().filter(|v| !v.is_empty())
            && !group.fixed.iter().any(|x| x == fixed)
        {
            group.fixed.push(fixed.to_owned());
        }
    }
    for group in &mut groups {
        group.fixed.sort_by(|a, b| {
            coolify_jobs::compose_parser::php::version_compare(b, a)
                .cmp(&0)
                .then_with(|| strnatcmp(b, a))
        });
    }
    groups.sort_by(|a, b| {
        b.kev
            .cmp(&a.kev)
            .then_with(|| severity_rank(&b.severity).cmp(&severity_rank(&a.severity)))
            .then_with(|| b.rows.len().cmp(&a.rows.len()))
            .then_with(|| a.package.cmp(&b.package))
    });
    groups
        .iter()
        .enumerate()
        .map(|(i, g)| {
            json!({
                "key": format!("{i}-{}-{}-{}", g.owner, g.package, g.version),
                "package": g.package,
                "version": g.version,
                "image": g.image,
                "owner": g.owner,
                "severity": g.severity,
                "kev": g.kev,
                "never_allowed": g.never_allowed,
                "fixed": g.fixed.first(),
                "count": g.rows.len(),
                "rows": g.rows.iter().map(|r| row_value(r)).collect::<Vec<_>>(),
            })
        })
        .collect()
}

/// `progress($owners)`: the scan progress of the first owner that is being scanned.
fn progress(owners: &[ScanOwner]) -> Option<Value> {
    owners
        .iter()
        .find_map(|o| cache::get(&scan_image::progress_cache_key(o)).filter(Value::is_object))
}

/// `checkpoints($progress)`.
fn checkpoints(progress: Option<&Value>) -> Vec<Value> {
    let Some(progress) = progress else {
        return Vec::new();
    };
    let stage = progress
        .get("stage")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let current = stage_checkpoint(stage);
    CHECKPOINTS
        .iter()
        .enumerate()
        .map(|(index, title)| {
            let status = match index.cmp(&current) {
                std::cmp::Ordering::Less => "success",
                std::cmp::Ordering::Equal => "running",
                std::cmp::Ordering::Greater => "pending",
            };
            json!({"title": title, "status": status})
        })
        .collect()
}

/// `errorMessages($owners, $showDetail)`: the last scan errors of the owners, with the error log
/// for users who can update the resource.
fn error_messages(owners: &[ScanOwner], show_detail: bool) -> Vec<Value> {
    let mut seen = Vec::new();
    let mut out = Vec::new();
    for owner in owners {
        let Some(error) = cache::get(&scan_image::last_error_cache_key(owner)) else {
            continue;
        };
        let code = error
            .get("code")
            .and_then(Value::as_str)
            .and_then(ErrorCode::from_value);
        let Some(code) = code else {
            continue;
        };
        let context: Map<String, Value> = error
            .get("context")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let detail = if show_detail {
            context
                .get("detail")
                .and_then(Value::as_str)
                .map(str::to_owned)
        } else {
            None
        };
        let message = code.message(&context);
        let key = format!("{message}\n{}", detail.clone().unwrap_or_default());
        if seen.contains(&key) {
            continue;
        }
        seen.push(key);
        out.push(json!({"message": message, "detail": detail}));
    }
    out
}

/// Pages of a list (`paginate()`): the shown slice, and the page clamped to the last page.
fn paginate(items: Vec<Value>, list: &ListState) -> Value {
    let total = items.len();
    let last_page = total.div_ceil(list.per_page).max(1);
    let page = list.page.clamp(1, last_page);
    let shown: Vec<Value> = items
        .into_iter()
        .skip((page - 1) * list.per_page)
        .take(list.per_page)
        .collect();
    json!({
        "items": shown,
        "total": total,
        "page": page,
        "per_page": list.per_page,
        "last_page": last_page,
    })
}

/// `$this->authorize('update', $resource)`.
fn authorize_update(ctx: &AuthCtx, resource: &ScanOwner) -> Result<(), ApiError> {
    if can_access(ctx, resource, AccessLevel::Write) {
        Ok(())
    } else {
        Err(super::forbidden())
    }
}

/// `render()`.
async fn render(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid)): Path<(String, String)>,
    Json(mut list): Json<ListState>,
) -> ApiResult {
    let jobs = jobs(&state)?;
    list.normalize();
    let page = load_page(&state, jobs, &ctx, &kind, &uuid).await?;
    let resource = &page.resource;
    let resolved = policy::resolve(&state.db, resource, None)
        .await
        .map_err(internal)?;
    let off_state = policy::off_state(&state.db, resource, resolved.scan_server_id)
        .await
        .map_err(internal)?;
    let server = match resolved.scan_server_id {
        Some(id) => coolify_db::servers::find_by_id(&state.db, id).await?,
        None => None,
    };
    let can_update = can_access(&ctx, resource, AccessLevel::Write);
    let mut out = json!({
        "off_state": off_state,
        "server_name": server
            .as_ref()
            .and_then(|s| s.record.get("name").and_then(Value::as_str).map(str::to_owned))
            .unwrap_or_else(|| "this server".to_owned()),
        // `route('server.security.vulnerabilities')` for users who can update the server.
        "server_url": server
            .as_ref()
            .filter(|_| ctx.is_team_admin())
            .map(|s| format!("/server/{}/security/vulnerabilities", s.uuid)),
        "can_update": can_update,
        "is_application": resource.is_application(),
        "is_service": resource.is_service(),
    });
    if off_state.is_some() {
        return Ok(ok(out).into_response());
    }

    let now = views::now();
    let all_rows = finding_rows(jobs, &page).await?;
    let active_rows: Vec<Row> = all_rows
        .iter()
        .filter(|r| r.ignore.is_none())
        .cloned()
        .collect();
    let ignored_count = all_rows.len() - active_rows.len();
    let finding_count = active_rows.len();
    let mut state_counts: Map<String, Value> = Map::new();
    let mut fixed = 0usize;
    let mut no_fix = 0usize;
    for row in filter_rows(active_rows, &list) {
        if row.state == "fixed" {
            fixed += 1;
        } else if row.state == "no_fix" {
            no_fix += 1;
        }
    }
    state_counts.insert("fixed".into(), json!(fixed));
    state_counts.insert("no_fix".into(), json!(no_fix));

    let list_rows = shown_rows(&all_rows, &list);
    let grouped = !list.show_ignored && list.view == "package";
    let items = if grouped {
        package_groups(&list_rows)
    } else {
        list_rows.iter().map(row_value).collect()
    };
    let pagination = paginate(items, &list);

    let progress = progress(&page.owners);
    let default_rules = RelevanceRules::default();
    let mut image_statuses: HashMap<String, ResourceSecurity> = HashMap::new();
    for scan in &page.scans {
        let rules = page.rules.get(&scan.owner.key()).unwrap_or(&default_rules);
        let security = status::for_scan(jobs, &scan.scan, &scan.owner, Some(rules))
            .await
            .map_err(internal)?;
        image_statuses.insert(scan.scan.uuid.clone(), security);
    }
    let security = if page.scans.is_empty() {
        None
    } else {
        Some(
            status::for_resource(jobs, resource, Some(page.scans.as_slice()), &image_statuses)
                .await
                .map_err(internal)?,
        )
    };
    let checked_at: Option<NaiveDateTime> = security
        .as_ref()
        .and_then(|s| s.checked_at)
        .or_else(|| page.scans.iter().filter_map(|s| s.scan.scanned_at).max());
    let mut image_options: Vec<String> = Vec::new();
    for scan in &page.scans {
        if !image_options.contains(&scan.scan.image_ref) {
            image_options.push(scan.scan.image_ref.clone());
        }
    }
    let card_value = match &security {
        None => Value::Null,
        Some(security) => {
            let admin = ctx.is_team_admin();
            let can_update_server = move |_server_id: i64| admin;
            let user = card::CardUser {
                can_update,
                can_deploy: resource.is_application()
                    && can_access(&ctx, resource, AccessLevel::Deploy),
                can_update_server: &can_update_server,
            };
            let c = card::card(
                jobs,
                resource,
                security,
                &page.owners,
                &page.scans,
                &user,
                &image_statuses,
                &page.rules,
                now,
            )
            .await
            .map_err(internal)?;
            serde_json::to_value(c).map_err(internal)?
        }
    };
    let truncated = page.scans.iter().any(|s| {
        s.scan
            .summary
            .as_ref()
            .and_then(|v| v.get("truncated"))
            .is_some_and(crate::serialize::php_truthy)
    });
    let shows_image = image_options.len() > 1;
    let fields = json!({
        "has_scans": !page.scans.is_empty(),
        "scanning": progress.is_some(),
        "checkpoints": checkpoints(progress.as_ref()),
        "error_messages": error_messages(&page.owners, can_update),
        "card": card_value,
        "checked_at": checked_at.map(|t| json!({
            "human": views::diff_for_humans(t, now),
            "title": day_date_time(t),
        })),
        "truncated": truncated,
        "finding_count": finding_count,
        "finding_count_text": number_format(finding_count),
        "state_counts": state_counts,
        "fix_states": export::FIX_STATES.iter().map(|(v, l)| json!({"value": v, "label": l})).collect::<Vec<_>>(),
        "ignored_count": ignored_count,
        "shown_count": list_rows.len(),
        "pagination": pagination,
        "grouped_finding_count": if grouped { Some(list_rows.len()) } else { None },
        "image_options": if shows_image { image_options.clone() } else { Vec::new() },
        "shows_image": shows_image,
        "can_ignore_for_team": can_update && ctx.is_team_admin(),
        "has_filters": list.has_filters(),
        "severity_options": SEVERITIES,
        // The normalized state, so the browser shows what the server used.
        "fix_state": list.fix_state,
        "view": list.view,
        "per_page": list.per_page,
    });
    if let (Value::Object(o), Value::Object(f)) = (&mut out, fields) {
        o.extend(f);
    }
    Ok(ok(out).into_response())
}

fn audit_context(team_id: Option<i64>, pairs: &[(&str, Value)]) -> Value {
    let mut map = Map::new();
    map.insert("team_id".to_owned(), json!(team_id));
    for (k, v) in pairs {
        map.insert((*k).to_owned(), v.clone());
    }
    Value::Object(map)
}

/// `applicationImageRefs($application)`.
async fn application_image_refs(
    state: &AppState,
    application: &ScanOwner,
) -> Result<Vec<String>, ApiError> {
    if let Some(reference) = resource_scans::current_image_ref(&state.db, application)
        .await
        .map_err(internal)?
    {
        return Ok(vec![reference]);
    }
    let mut refs: Vec<String> = Vec::new();
    for scan in resource_scans::current_for(&state.db, application)
        .await
        .map_err(internal)?
    {
        if !scan.image_ref.is_empty() && !refs.contains(&scan.image_ref) {
            refs.push(scan.image_ref);
        }
    }
    Ok(refs)
}

/// `scanNow()`.
async fn scan_now(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid)): Path<(String, String)>,
    meta: Meta,
) -> ApiResult {
    let jobs = jobs(&state)?;
    let resource = load_resource(&state, &ctx, &kind, &uuid).await?;
    authorize_update(&ctx, &resource)?;
    let owners = image_scans::owners(&state.db, &resource).await?;
    if progress(&owners).is_some() {
        return toast_error("A scan is already running.");
    }
    let resolved = policy::resolve(&state.db, &resource, None)
        .await
        .map_err(internal)?;
    if let Some(server_id) = resolved.scan_server_id
        && let Ok(server) = ScanServer::load(jobs, server_id).await
        && server.is_transferred_away()
    {
        return toast_error("Another Coolify instance manages this server now.");
    }
    if !resolved.scan_enabled {
        return toast_error("Image scanning is off for this resource.");
    }
    let mut image_refs = Vec::new();
    if resource.is_application() {
        image_refs = application_image_refs(&state, &resource).await?;
        if image_refs.is_empty() {
            return toast_error("Deploy the app first.");
        }
    }
    // `RateLimiter::attempt("image-scan-now:{morph}:{id}", 1, …, 60)`.
    let limiter = format!("image-scan-now:{}:{}", resource.morph, resource.id);
    if !cache::add(&limiter, json!(1), Duration::from_secs(60)) {
        return toast_error("A scan started less than a minute ago. Try again later.");
    }
    // Marked first: the job may run at once.
    scan_jobs::mark_queued(jobs, &resource, &image_refs)
        .await
        .map_err(internal)?;
    jobs.dispatch_scan_resource_images(
        ScanResourceImages {
            image_refs,
            pull_when_missing: true,
            ..ScanResourceImages::new(&resource)
        },
        0,
    );
    audit::log(
        &state,
        &ctx,
        &meta,
        "ui.image_scan.scan_now",
        audit_context(
            resource.team_id,
            &[
                ("resource_type", json!(resource.morph)),
                ("resource_uuid", json!(uuid)),
            ],
        ),
        "info",
    )
    .await;
    toast_success("Scan started")
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct OverrideInput {
    service: String,
}

/// `removeImageOverride($service)`: the next deploy uses the image of the compose file again.
async fn remove_image_override(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid)): Path<(String, String)>,
    meta: Meta,
    Json(input): Json<OverrideInput>,
) -> ApiResult {
    let jobs = jobs(&state)?;
    let resource = load_resource(&state, &ctx, &kind, &uuid).await?;
    authorize_update(&ctx, &resource)?;
    if !resource.is_application() || resource.build_pack() != Some("dockercompose") {
        return Err(not_found());
    }
    if let Some(server_id) = resource.main_server_id
        && let Ok(server) = ScanServer::load(jobs, server_id).await
        && server.is_transferred_away()
    {
        return toast_error("Another Coolify instance manages this server now.");
    }
    // The `array` cast: stored as JSON text or as a JSON value.
    let mut overrides: Map<String, Value> =
        match resource.record.get("docker_compose_image_overrides") {
            Some(Value::String(text)) => serde_json::from_str::<Value>(text)
                .ok()
                .and_then(|v| v.as_object().cloned())
                .unwrap_or_default(),
            Some(Value::Object(map)) => map.clone(),
            _ => Map::new(),
        };
    if overrides.remove(&input.service).is_none() {
        return Ok(ok(json!({})).into_response());
    }
    let encoded = (!overrides.is_empty())
        .then(|| coolify_compat::php_json::encode(&Value::Object(overrides)));
    coolify_db::image_scans::update::set_compose_image_overrides(
        &state.db,
        resource.id,
        encoded.as_deref(),
        state.now(),
    )
    .await?;
    audit::log(
        &state,
        &ctx,
        &meta,
        "ui.image_scan.image_override_removed",
        audit_context(
            resource.team_id,
            &[
                ("resource_type", json!(resource.morph)),
                ("resource_uuid", json!(resource.uuid)),
                ("service", json!(input.service)),
                ("user_id", json!(ctx.user_id)),
            ],
        ),
        "info",
    )
    .await;
    toast_success("The next deploy uses the image of the compose file.")
}

/// `updateRefusal($action)`.
fn update_refusal(action: Option<&SecurityAction>) -> Option<&'static str> {
    match action {
        None => Some("This update is no longer offered. Reload the page."),
        Some(a) if !a.button => {
            Some("Coolify cannot run this update. Follow the steps on this page instead.")
        }
        Some(_) => None,
    }
}

fn outcome_response(outcome: ImageUpdateOutcome) -> ApiResult {
    match outcome {
        ImageUpdateOutcome::Started => toast_success("Update started."),
        ImageUpdateOutcome::Refused(message) => toast_error(message),
        ImageUpdateOutcome::Failed => toast_error("The update could not start."),
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct UpdateImageInput {
    candidate_ref: String,
    owner_uuid: Option<String>,
}

/// `updateImage($candidateRef, $ownerUuid)`.
async fn update_image(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid)): Path<(String, String)>,
    meta: Meta,
    Json(input): Json<UpdateImageInput>,
) -> ApiResult {
    let jobs = jobs(&state)?;
    let resource = load_resource(&state, &ctx, &kind, &uuid).await?;
    authorize_update(&ctx, &resource)?;
    let target = if resource.is_service() {
        let owner_uuid = input
            .owner_uuid
            .as_deref()
            .filter(|u| !u.trim().is_empty())
            .ok_or_else(not_found)?;
        image_scans::owners(&state.db, &resource)
            .await?
            .into_iter()
            .find(|o| o.uuid == owner_uuid)
            .ok_or_else(not_found)?
    } else {
        resource.clone()
    };
    let security = status::for_resource(jobs, &resource, None, &HashMap::new())
        .await
        .map_err(internal)?;
    let candidate = input.candidate_ref.as_str();
    let action = security.actions.iter().find(|a| {
        a.reference.as_deref() == Some(candidate)
            || a.parts
                .iter()
                .any(|p| p.reference.as_deref() == Some(candidate))
    });
    if let Some(refusal) = update_refusal(action) {
        return toast_error(refusal);
    }
    let outcome = update_resource_image(&state, &ctx, &meta, &resource, &target, candidate).await?;
    outcome_response(outcome)
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct UpdateImagesInput {
    action_key: String,
}

/// `updateImages($actionKey)`: a merged service update, every image in one change.
async fn update_images(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid)): Path<(String, String)>,
    meta: Meta,
    Json(input): Json<UpdateImagesInput>,
) -> ApiResult {
    let jobs = jobs(&state)?;
    let resource = load_resource(&state, &ctx, &kind, &uuid).await?;
    authorize_update(&ctx, &resource)?;
    if !resource.is_service() {
        return Err(not_found());
    }
    let security = status::for_resource(jobs, &resource, None, &HashMap::new())
        .await
        .map_err(internal)?;
    let action = security
        .actions
        .iter()
        .find(|a| !a.parts.is_empty() && card::key(a) == input.action_key);
    if let Some(refusal) = update_refusal(action) {
        return toast_error(refusal);
    }
    let Some(action) = action else {
        return Err(not_found());
    };
    let owners = image_scans::owners(&state.db, &resource).await?;
    let mut parts = Vec::new();
    for part in &action.parts {
        let owner = owners
            .iter()
            .find(|o| o.morph == part.owner_type && o.id.to_string() == part.owner_id_string())
            .ok_or_else(not_found)?;
        let reference = part.reference.clone().ok_or_else(not_found)?;
        parts.push(UpdatePart {
            owner_morph: owner.morph.to_owned(),
            owner_id: owner.id,
            reference,
        });
    }
    let outcome = update_resource_images(&state, &ctx, &meta, &resource, &parts).await?;
    outcome_response(outcome)
}

/// `Application::deploymentServer($deployment)`: the server of a deployment when it is still the
/// main server or an additional server of the app.
async fn deployment_server(
    state: &AppState,
    application: &apps::ApplicationRow,
    server_id: Option<i64>,
) -> Result<Option<i64>, ApiError> {
    let Some(server_id) = server_id else {
        return Ok(None);
    };
    if application.server_id() == Some(server_id) {
        return Ok(Some(server_id));
    }
    let additional = apps::additional_pivots(&state.db, application.id).await?;
    Ok(additional
        .iter()
        .any(|(_, s, _)| *s == server_id)
        .then_some(server_id))
}

/// `StandaloneDocker::query()->whereKey($id)->where('server_id', $serverId)->first()`.
pub(crate) async fn standalone_destination(
    state: &AppState,
    destination_id: Option<i64>,
    server_id: i64,
) -> Result<Option<i64>, ApiError> {
    let Some(destination_id) = destination_id else {
        return Ok(None);
    };
    Ok(destinations::list_for_server(&state.db, server_id)
        .await?
        .into_iter()
        .find(|d| matches!(d.kind, DestinationKind::Standalone) && d.id == destination_id)
        .map(|d| d.id))
}

/// `rebuild()`: rebuilds the running commit of the app without cache and redeploys it, on the
/// server and destination of the deployment that runs now.
async fn rebuild(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid)): Path<(String, String)>,
    meta: Meta,
) -> ApiResult {
    let jobs = jobs(&state)?;
    let resource = load_resource(&state, &ctx, &kind, &uuid).await?;
    if !resource.is_application() {
        return Err(not_found());
    }
    if !can_access(&ctx, &resource, AccessLevel::Deploy) {
        return Err(super::forbidden());
    }
    if resource.id == 0 {
        return Err(ApiError::Abort(
            axum::http::StatusCode::FORBIDDEN,
            "Coolify does not rebuild its own resources.",
        ));
    }
    let Some((deployment_id, commit)) = card::running_deployment(jobs, &resource)
        .await
        .map_err(internal)?
    else {
        return toast_error("Deploy the app first.");
    };
    if !card::knows_running_commit(&resource, commit.as_deref()) {
        return toast_error("Coolify does not know which commit runs now. Redeploy the app first.");
    }
    let application = apps::find_by_id(&state.db, resource.id)
        .await?
        .ok_or_else(not_found)?;
    let running = apps::deployment_by_id(&state.db, deployment_id)
        .await?
        .ok_or_else(not_found)?;
    let Some(server_id) = deployment_server(&state, &application, running.int("server_id")).await?
    else {
        return toast_error("The server of the running deployment is no longer one of this app.");
    };
    if ScanServer::load(jobs, server_id)
        .await
        .is_ok_and(|s| s.is_transferred_away())
    {
        return toast_error("Another Coolify instance manages this server now.");
    }
    let destination_id =
        standalone_destination(&state, running.int("destination_id"), server_id).await?;
    let result = jobs
        .queue_application_deployment(
            &application,
            &coolify_domain::new_public_id(),
            QueueOptions {
                commit: running.str("commit").map(str::to_owned),
                force_rebuild: true,
                git_type: running.str("git_type").map(str::to_owned),
                server_id: Some(server_id),
                destination_id,
                only_this_server: running.bool("only_this_server"),
                docker_registry_image_tag: running
                    .str("docker_registry_image_tag")
                    .map(str::to_owned),
                ..Default::default()
            },
        )
        .await;
    let queued = match result {
        Ok(q) => q,
        Err(e) => {
            tracing::error!(error = %e, "the rebuild could not start");
            return toast_error("The rebuild could not start.");
        }
    };
    if !matches!(queued, Queued::Queued { .. }) {
        return toast_error("The rebuild did not start.");
    }
    audit::log(
        &state,
        &ctx,
        &meta,
        "ui.image_scan.rebuild",
        audit_context(
            resource.team_id,
            &[
                ("resource_type", json!(resource.morph)),
                ("resource_uuid", json!(resource.uuid)),
                ("commit", json!(running.str("commit"))),
            ],
        ),
        "info",
    )
    .await;
    toast_success("Rebuild started.")
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct IgnoreInput {
    target: String,
    kind: String,
    scope: String,
    expires: String,
    reason: String,
    owner: Option<String>,
}

/// The first validation error of `ignore()` (`ImageScanIgnore::validationRules()` and the rules
/// of the component), in rule order.
fn ignore_validation_error(
    vulnerability_id: Option<&str>,
    package: Option<&str>,
    reason: &str,
    kind: &str,
    scope: &str,
    expires: &str,
) -> Option<&'static str> {
    // `regex:/^[A-Za-z0-9][A-Za-z0-9._:-]{0,63}$/`.
    let valid_id = |id: &str| {
        let mut chars = id.chars();
        chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
            && id.len() <= 64
            && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '-'))
    };
    match vulnerability_id {
        Some(id) if !valid_id(id) => {
            return Some("The vulnerability id field format is invalid.");
        }
        None if package.is_none() => {
            return Some("The vulnerability id field is required when package is not present.");
        }
        _ => {}
    }
    if let Some(package) = package
        && package.chars().count() > 214
    {
        return Some("The package field must not be greater than 214 characters.");
    }
    if reason.is_empty() {
        return Some("The reason field is required.");
    }
    if reason.chars().count() > 500 {
        return Some("The reason field must not be greater than 500 characters.");
    }
    if !["vulnerability", "package"].contains(&kind) {
        return Some("The selected kind is invalid.");
    }
    if !["resource", "team"].contains(&scope) {
        return Some("The selected scope is invalid.");
    }
    if !EXPIRES.contains(&expires) {
        return Some("The selected expires is invalid.");
    }
    None
}

/// `ignore($target, $kind, $scope, $expires, $reason, $ownerUuid)`: ignores a vulnerability ID or
/// all vulnerabilities of a package. A validation error is shown as an error toast.
async fn ignore(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid)): Path<(String, String)>,
    meta: Meta,
    Json(input): Json<IgnoreInput>,
) -> ApiResult {
    let jobs = jobs(&state)?;
    let page = load_page(&state, jobs, &ctx, &kind, &uuid).await?;
    let resource = &page.resource;
    authorize_update(&ctx, resource)?;
    let team_id = resource.team_id.ok_or_else(not_found)?;
    if input.scope == "team" && !ctx.is_team_admin() {
        return Err(super::forbidden());
    }

    // `preg_replace('/\p{Cc}/u', '', $target)`.
    let package: Option<String> = (input.kind == "package")
        .then(|| input.target.chars().filter(|c| !c.is_control()).collect());
    let vulnerability_id: Option<String> =
        (input.kind == "vulnerability").then(|| input.target.clone());
    let reason = input.reason.trim().to_owned();
    let package = package.filter(|p| !p.is_empty());
    let vulnerability_id = vulnerability_id.filter(|v| !v.is_empty());
    if let Some(message) = ignore_validation_error(
        vulnerability_id.as_deref(),
        package.as_deref(),
        &reason,
        &input.kind,
        &input.scope,
        &input.expires,
    ) {
        return toast_error(message);
    }

    let owner = if resource.is_service() {
        page.owners
            .iter()
            .find(|o| input.owner.as_deref() == Some(o.uuid.as_str()))
            .cloned()
    } else {
        Some(resource.clone())
    };
    let owner = owner.ok_or_else(not_found)?;

    // `ownerFindings()`.
    let target = match &package {
        Some(p) => p.clone(),
        None => input.target.to_lowercase(),
    };
    let matching: Vec<Finding> = page
        .scans
        .iter()
        .filter(|s| s.owner.morph == owner.morph && s.owner.id == owner.id)
        .flat_map(|s| resource_scans::findings(&s.scan))
        .filter(|f| {
            if package.is_some() {
                f.package == target
            } else {
                f.ids().any(|id| id.to_lowercase() == target)
            }
        })
        .collect();
    if matching.is_empty() {
        return toast_error("This is not in the scan results.");
    }
    // Spec 15.2: an ignore of a problem with an exploit signal expires after at most 90 days.
    if input.expires == "never" && matching.iter().any(has_exploit_signal) {
        return toast_error(
            "Attackers are using or are likely to use this problem, so its ignore must expire. Choose 30 or 90 days.",
        );
    }

    let now = state.now();
    let expires_at = input
        .expires
        .parse::<i64>()
        .ok()
        .map(|days| now + chrono::Duration::days(days));
    let resource_scope = (input.scope == "resource").then_some((owner.morph, owner.id));
    let created = image_scans::create_ignore(
        &state.db,
        &NewIgnore {
            team_id,
            resource: resource_scope,
            vulnerability_id: vulnerability_id.as_deref(),
            package: package.as_deref(),
            reason: &reason,
            expires_at,
            created_by: Some(ctx.user_id),
        },
        now,
    )
    .await?;

    let current: Vec<ImageScanRow> = page.scans.iter().map(|s| s.scan.clone()).collect();
    resource_scans::refresh_summaries(jobs, current)
        .await
        .map_err(internal)?;
    if input.scope == "team" {
        jobs.dispatch_refresh_image_scan_summaries(team_id);
    }
    audit::log(
        &state,
        &ctx,
        &meta,
        "ui.image_scan.ignore_added",
        audit_context(
            Some(team_id),
            &[
                ("resource_type", json!(created.resource_type)),
                (
                    "resource_uuid",
                    json!((input.scope == "resource").then_some(owner.uuid.clone())),
                ),
                ("vulnerability_id", json!(created.vulnerability_id)),
                ("package", json!(created.package)),
                ("expires_at", json!(created.expires_at.map(views::iso8601))),
            ],
        ),
        "info",
    )
    .await;
    Ok(ok(json!({"message": "Ignored", "saved": true})).into_response())
}

/// `copyList($format)` and `downloadList($format)`: the shown findings as Markdown or IDs for
/// the clipboard, or as a CSV or JSON download.
async fn export_list(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid)): Path<(String, String)>,
    Json(mut list): Json<ListState>,
) -> ApiResult {
    let jobs = jobs(&state)?;
    if !["markdown", "ids", "csv", "json"].contains(&list.format.as_str()) {
        return Err(not_found());
    }
    list.normalize();
    let page = load_page(&state, jobs, &ctx, &kind, &uuid).await?;
    if policy::off_state(&state.db, &page.resource, None)
        .await
        .map_err(internal)?
        .is_some()
    {
        return Err(not_found());
    }
    let rows: Vec<ExportRow> = shown_rows(&finding_rows(jobs, &page).await?, &list)
        .into_iter()
        .map(|r| ExportRow {
            finding: r.finding,
            state: r.state,
            ignore: r.ignore,
            image: r.image,
            id: None,
        })
        .collect();
    let name = page.resource.name();
    match list.format.as_str() {
        "ids" => Ok(ok(json!({"content": export::findings_ids(&rows)})).into_response()),
        "markdown" => {
            let scope = if list.show_ignored {
                "ignored".to_owned()
            } else {
                export::FIX_STATES
                    .iter()
                    .find(|(k, _)| *k == list.fix_state)
                    .map(|(_, l)| (*l).to_owned())
                    .unwrap_or_default()
            };
            let title = format!(
                "Vulnerabilities of {name} ({}, {scope})",
                count_text(rows.len(), "finding", "findings")
            );
            let images: BTreeSet<&str> = page
                .scans
                .iter()
                .map(|s| s.scan.image_ref.as_str())
                .collect();
            Ok(ok(json!({"content": export::findings_markdown(&title, &rows, images.len() > 1, list.show_ignored)}))
                .into_response())
        }
        format => {
            let now = views::now();
            let content = if format == "csv" {
                export::findings_csv(&rows, &export::FIX_STATES)
            } else {
                export::findings_json(
                    json!({"type": kind, "name": name, "uuid": uuid}),
                    &rows,
                    &export::FIX_STATES,
                    now,
                )
            };
            let file = format!(
                "{}-vulnerabilities-{}.{format}",
                slug_or(&name, "resource"),
                now.format("%Y-%m-%d")
            );
            let mime = if format == "csv" {
                "text/csv; charset=UTF-8"
            } else {
                "application/json"
            };
            download(file, mime, content)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finding(id: &str, severity: &str, package: &str, kev: bool) -> Finding {
        Finding {
            id: id.to_owned(),
            aliases: Vec::new(),
            severity: severity.to_owned(),
            package: package.to_owned(),
            version: "1.0".to_owned(),
            r#type: "deb".to_owned(),
            artifact_id: String::new(),
            fixed_in: Some("1.1".to_owned()),
            fix_state: "fixed".to_owned(),
            kev,
            epss: None,
            risk: None,
            summary: String::new(),
            layer: "app".to_owned(),
            cvss_score: None,
            title: String::new(),
            display_id: id.to_owned(),
            namespace: String::new(),
            match_type: String::new(),
        }
    }

    fn row(f: Finding, state: &str) -> Row {
        Row {
            key: f.id.clone(),
            finding: f,
            state: state.to_owned(),
            ignore: None,
            image: "nginx:1".to_owned(),
            owner: "o".to_owned(),
            base: None,
            never_allowed: true,
            package_never_allowed: true,
        }
    }

    #[test]
    fn rows_sort_worst_first_and_filter() {
        let rows = vec![
            row(finding("CVE-1", "low", "a", false), "fixed"),
            row(finding("CVE-2", "high", "b", false), "fixed"),
            row(finding("CVE-3", "low", "c", true), "fixed"),
            row(finding("CVE-4", "critical", "d", false), "no_fix"),
        ];
        let mut list = ListState::default();
        list.normalize();
        let shown = shown_rows(&rows, &list);
        let ids: Vec<&str> = shown.iter().map(|r| r.finding.id.as_str()).collect();
        assert_eq!(ids, vec!["CVE-3", "CVE-2", "CVE-1"]);
        list.severities = vec!["high".to_owned()];
        assert_eq!(shown_rows(&rows, &list).len(), 1);
        list.severities.clear();
        list.search = "cve-4".to_owned();
        list.fix_state = "no_fix".to_owned();
        assert_eq!(shown_rows(&rows, &list).len(), 1);
    }

    #[test]
    fn package_groups_take_the_worst_severity_and_newest_fix() {
        let mut a = finding("CVE-1", "low", "openssl", false);
        a.fixed_in = Some("1.9".to_owned());
        let mut b = finding("CVE-2", "high", "openssl", false);
        b.fixed_in = Some("1.10".to_owned());
        let groups = package_groups(&[row(a, "fixed"), row(b, "fixed")]);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0]["severity"], "high");
        assert_eq!(groups[0]["fixed"], "1.10");
        assert_eq!(groups[0]["count"], 2);
    }

    #[test]
    fn checkpoints_follow_the_stage() {
        let c = checkpoints(Some(&json!({"stage": "sbom"})));
        let statuses: Vec<&str> = c.iter().map(|x| x["status"].as_str().unwrap()).collect();
        assert_eq!(statuses, vec!["success", "success", "running", "pending"]);
        assert!(checkpoints(None).is_empty());
    }

    #[test]
    fn ignore_validation_reads_like_laravel() {
        assert_eq!(
            ignore_validation_error(None, None, "x", "vulnerability", "resource", "30"),
            Some("The vulnerability id field is required when package is not present.")
        );
        assert_eq!(
            ignore_validation_error(Some("CVE 1"), None, "x", "vulnerability", "resource", "30"),
            Some("The vulnerability id field format is invalid.")
        );
        assert_eq!(
            ignore_validation_error(Some("CVE-1"), None, "", "vulnerability", "resource", "30"),
            Some("The reason field is required.")
        );
        assert_eq!(
            ignore_validation_error(None, Some("openssl"), "r", "package", "team", "forever"),
            Some("The selected expires is invalid.")
        );
        assert_eq!(
            ignore_validation_error(
                Some("CVE-1"),
                None,
                "r",
                "vulnerability",
                "resource",
                "never"
            ),
            None
        );
    }

    #[test]
    fn pages_are_clamped() {
        let mut list = ListState {
            page: 9,
            per_page: 2,
            ..Default::default()
        };
        list.normalize();
        let p = paginate(vec![json!(1), json!(2), json!(3)], &list);
        assert_eq!(p["page"], 2);
        assert_eq!(p["last_page"], 2);
        assert_eq!(p["items"], json!([3]));
    }
}
