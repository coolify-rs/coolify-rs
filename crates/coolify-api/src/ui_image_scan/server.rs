//! The Vulnerabilities page of a server (`/server/{uuid}/security/vulnerabilities`):
//!
//! - `App\Livewire\ImageScan\ServerImageScanning`: turn image scanning on or off, get the scanner,
//!   delete the scanner data, and the scanner limits.
//! - `App\Livewire\Server\Security\HostVulnerabilities`: the known vulnerabilities in the
//!   operating system packages of the server (`ScanHostPackages`), with the package updates of
//!   the last patch check.

use std::time::Duration;

use axum::extract::{Path, State};
use axum::response::IntoResponse;
use axum::routing::{get, patch, post};
use axum::{Extension, Json, Router};
use coolify_db::image_scans::{self, ServerScanSettings};
use coolify_db::servers::{self, ServerRow};
use coolify_jobs::image_scan::commands::{IMAGE_SCAN_TIMEOUT_OPTIONS_SECONDS, SCANNER_IMAGES};
use coolify_jobs::image_scan::error::ErrorCode;
use coolify_jobs::image_scan::finding::Finding;
use coolify_jobs::image_scan::image_ref::ImageRef;
use coolify_jobs::image_scan::prepare::{self, SetScanningError};
use coolify_jobs::image_scan::rules::{self, ExploitSignal};
use coolify_jobs::image_scan::views::export::{self, ExportRow};
use coolify_jobs::image_scan::views::{self as views, parse_time};
use coolify_jobs::image_scan::{
    ScanServer, cache, host, new_run_id, resource_scans, scan_image, scan_jobs,
};
use coolify_jobs::server_ops::{Srv, patches};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::{
    count_text, download, forbidden, format_bytes, internal, jobs, slug_or, strnatcasecmp,
    toast_error, toast_success,
};
use crate::audit::{self, Meta};
use crate::auth::AuthCtx;
use crate::response::{ApiError, ApiResult, ok};
use crate::state::AppState;
use crate::ui_server_general::SERVER_PAGE;

const MANAGED_ELSEWHERE: &str = "This server is managed by another Coolify instance.";

pub fn routes() -> Router<AppState> {
    let p = |s: &str| format!("{SERVER_PAGE}/image-scanning{s}");
    let h = |s: &str| format!("{SERVER_PAGE}/host-vulnerabilities{s}");
    Router::new()
        .route(&p(""), get(show))
        .route(&p("/enable"), post(enable))
        .route(&p("/disable"), post(disable))
        .route(&p("/prepare"), post(prepare_scanner))
        .route(&p("/delete-data"), post(delete_scanner_data))
        .route(&p("/limits"), patch(save_limit))
        .route(&h(""), post(host_render))
        .route(&h("/scan"), post(host_scan))
        .route(&h("/update-package"), post(host_update_package))
        .route(&h("/export"), post(host_export))
}

/// `Server::ownedByCurrentTeam()->whereUuid($uuid)->firstOrFail()` and `viewSecurity` (a team
/// admin or owner).
async fn find_server(state: &AppState, ctx: &AuthCtx, uuid: &str) -> Result<ServerRow, ApiError> {
    let server = servers::find(&state.db, ctx.team_id, uuid)
        .await?
        .ok_or(ApiError::Abort(
            axum::http::StatusCode::NOT_FOUND,
            "Not Found",
        ))?;
    if !ctx.is_team_admin() {
        return Err(forbidden());
    }
    Ok(server)
}

fn server_name(server: &ServerRow) -> String {
    server
        .record
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

async fn audit_server(
    state: &AppState,
    ctx: &AuthCtx,
    meta: &Meta,
    server: &ServerRow,
    action: &str,
) {
    audit::log(
        state,
        ctx,
        meta,
        &format!("ui.server.{action}"),
        json!({
            "team_id": server.team_id,
            "server_uuid": server.uuid,
            "server_name": server_name(server),
        }),
        "info",
    )
    .await;
}

/// `(string) $float` of PHP: `1.0` is "1".
fn float_text(value: f64) -> String {
    if value.fract() == 0.0 && value.abs() < 1e15 {
        format!("{value:.0}")
    } else {
        value.to_string()
    }
}

/// `listboxOptions($allowed, $current, $label)`: the values that fit the server; a saved value
/// that no longer fits stays in the list, marked.
fn listbox_options(allowed: &[f64], current: f64, label: impl Fn(f64) -> String) -> Vec<Value> {
    let mut values: Vec<f64> = allowed.to_vec();
    if !allowed.contains(&current) {
        values.push(current);
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    values
        .into_iter()
        .map(|v| {
            let suffix = if allowed.contains(&v) {
                ""
            } else {
                " (more than this server has)"
            };
            json!({"value": float_text(v), "label": format!("{}{suffix}", label(v))})
        })
        .collect()
}

fn cpu_label(cpus: f64) -> String {
    format!(
        "{} {}",
        float_text(cpus),
        if cpus > 1.0 { "CPUs" } else { "CPU" }
    )
}

/// `ServerImageScanning::render()`.
async fn show(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let jobs = jobs(&state)?;
    let server = find_server(&state, &ctx, &uuid).await?;
    let scan_server = ScanServer::load(jobs, server.id).await.map_err(internal)?;
    let host = scan_server.host();
    let settings = image_scans::server_scan_settings(&state.db, server.id)
        .await?
        .unwrap_or_else(ServerScanSettings::default);
    let now = views::now();
    let prepare_state = prepare::state(server.id);
    let built_at = prepare::database_built_at(server.id).and_then(|t| parse_time(&t));
    let disk_used = prepare::disk_used_bytes(server.id);
    let memory_allowed: Vec<f64> = host.memory_options_mb().iter().map(|m| *m as f64).collect();
    let mut registries: Vec<String> = Vec::new();
    for image in SCANNER_IMAGES {
        if let Ok(reference) = ImageRef::parse(image.repo)
            && !registries.contains(&reference.registry)
        {
            registries.push(reference.registry);
        }
    }
    let tool_versions = [("syft", "Syft"), ("grype", "Grype"), ("regctl", "regctl")]
        .iter()
        .map(|(tool, name)| {
            let version = SCANNER_IMAGES
                .iter()
                .find(|i| i.tool == *tool)
                .map(|i| i.version)
                .unwrap_or_default();
            format!("{name} {version}")
        })
        .collect::<Vec<_>>()
        .join(", ");
    Ok(ok(json!({
        "name": server_name(&server),
        "can_update": ctx.is_team_admin(),
        "enabled": settings.is_image_scan_enabled,
        "memory_limit": format_bytes(settings.image_scan_memory_mb * 1024 * 1024, 0),
        "memory_options": listbox_options(&memory_allowed, settings.image_scan_memory_mb as f64, |mb| {
            format_bytes((mb as i64) * 1024 * 1024, 0)
        }),
        "timeout_options": IMAGE_SCAN_TIMEOUT_OPTIONS_SECONDS.iter().map(|s| json!({
            "value": s.to_string(),
            "label": format!("{} minutes", float_text(*s as f64 / 60.0)),
        })).collect::<Vec<_>>(),
        "cpu_options": listbox_options(&host.cpu_options(), settings.image_scan_cpus, cpu_label),
        "values": {
            "scanner_memory_mb": settings.image_scan_memory_mb.to_string(),
            "scanner_syft_timeout": settings.image_scan_syft_timeout.to_string(),
            "scanner_grype_timeout": settings.image_scan_grype_timeout.to_string(),
            "scanner_cpus": float_text(settings.image_scan_cpus),
        },
        "prepare_state": prepare_state,
        "failed_step": prepare::failed_step(server.id),
        "registries": registries.join(", "),
        "disk_used": disk_used.filter(|d| *d != 0).map(|d| format_bytes(d, 1)),
        // Hide the delete action only when the server is known to be empty.
        "has_scanner_data": prepare_state.is_some() || disk_used != Some(0),
        "tool_versions": tool_versions,
        "built_at": built_at.map(|t| views::diff_for_humans(t, now)),
        "cpu_limit": host.cpus(),
    }))
    .into_response())
}

async fn set_scanning(
    state: &AppState,
    ctx: &AuthCtx,
    meta: &Meta,
    uuid: &str,
    enabled: bool,
) -> ApiResult {
    let jobs = jobs(state)?;
    let server = find_server(state, ctx, uuid).await?;
    let scan_server = ScanServer::load(jobs, server.id).await.map_err(internal)?;
    match prepare::set_server_image_scanning(jobs, &scan_server, enabled).await {
        Ok(()) => {}
        Err(SetScanningError::Refused(refused)) => return toast_error(refused.0),
        Err(SetScanningError::Other(e)) => return toast_error(e),
    }
    if enabled {
        audit_server(state, ctx, meta, &server, "image_scanning_enabled").await;
        toast_success("Image scanning turned on. Coolify is getting the scanner.")
    } else {
        audit_server(state, ctx, meta, &server, "image_scanning_disabled").await;
        toast_success("Image scanning turned off.")
    }
}

/// `enable()`.
async fn enable(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    meta: Meta,
) -> ApiResult {
    set_scanning(&state, &ctx, &meta, &uuid, true).await
}

/// `disable()`.
async fn disable(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    meta: Meta,
) -> ApiResult {
    set_scanning(&state, &ctx, &meta, &uuid, false).await
}

/// `prepareScanner()`.
async fn prepare_scanner(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let jobs = jobs(&state)?;
    let server = find_server(&state, &ctx, &uuid).await?;
    let scan_server = ScanServer::load(jobs, server.id).await.map_err(internal)?;
    if let Err(refused) = prepare::ensure_managed_here(&scan_server) {
        return toast_error(refused.0);
    }
    if !scan_server.is_scan_enabled() {
        return toast_error("Turn on image scanning first.");
    }
    prepare::prepare_scanner(jobs, server.id);
    toast_success("Coolify is getting the scanner.")
}

/// `deleteScannerData()`.
async fn delete_scanner_data(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    meta: Meta,
) -> ApiResult {
    let jobs = jobs(&state)?;
    let server = find_server(&state, &ctx, &uuid).await?;
    let scan_server = ScanServer::load(jobs, server.id).await.map_err(internal)?;
    let lock = scan_image::server_lock(&scan_server);
    if let Err(refused) = prepare::ensure_managed_here(&scan_server) {
        return toast_error(refused.0);
    }
    if !scan_image::acquire_server_lock(&lock, server.id, 0).await {
        return toast_error(
            "A scan or a scanner update is running on this server. Try again when it is complete.",
        );
    }
    let result = match scan_server.commands(&new_run_id()) {
        Ok(commands) => scan_server
            .srv
            .run_once(
                &commands.delete_scanner_data(),
                "image-scan",
                Duration::from_secs(120),
            )
            .await
            .map(|_| ()),
        Err(e) => Err(e.to_string()),
    };
    lock.release();
    if let Err(e) = result {
        tracing::warn!(error = %e, server = %server.uuid, "scanner data could not be deleted");
        return toast_error("Could not delete the scanner data.");
    }
    prepare::forget(server.id);
    // Records the server as empty, so the page can hide the delete action until the data comes
    // back.
    cache::put(
        &prepare::disk_used_cache_key(server.id),
        json!(0),
        Duration::from_secs(7 * 86_400),
    );
    audit_server(&state, &ctx, &meta, &server, "image_scanner_data_deleted").await;
    toast_success("Scanner data deleted.")
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct LimitInput {
    field: String,
    value: String,
}

/// `saveScannerLimit($property)`.
async fn save_limit(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    meta: Meta,
    Json(input): Json<LimitInput>,
) -> ApiResult {
    let jobs = jobs(&state)?;
    let server = find_server(&state, &ctx, &uuid).await?;
    let scan_server = ScanServer::load(jobs, server.id).await.map_err(internal)?;
    let host = scan_server.host();
    let (options, action, message): (Vec<String>, &str, &str) = match input.field.as_str() {
        "scanner_memory_mb" => (
            host.memory_options_mb()
                .iter()
                .map(i64::to_string)
                .collect(),
            "image_scanner_memory_changed",
            "Scanner memory limit saved. It applies to the next scan.",
        ),
        "scanner_syft_timeout" => (
            IMAGE_SCAN_TIMEOUT_OPTIONS_SECONDS
                .iter()
                .map(i64::to_string)
                .collect(),
            "image_scanner_syft_timeout_changed",
            "Syft time limit saved. It applies to the next scan.",
        ),
        "scanner_grype_timeout" => (
            IMAGE_SCAN_TIMEOUT_OPTIONS_SECONDS
                .iter()
                .map(i64::to_string)
                .collect(),
            "image_scanner_grype_timeout_changed",
            "Grype time limit saved. It applies to the next scan.",
        ),
        "scanner_cpus" => (
            host.cpu_options().iter().map(|c| float_text(*c)).collect(),
            "image_scanner_cpus_changed",
            "Scanner CPU limit saved. It applies to the next scan.",
        ),
        _ => return Err(super::not_found()),
    };
    if input.value.is_empty() {
        return toast_error(format!(
            "The {} field is required.",
            input.field.replace('_', " ")
        ));
    }
    if !options.contains(&input.value) {
        return toast_error(
            "Choose one of the listed values. Values that are more than this server has are not allowed.",
        );
    }
    if let Err(refused) = prepare::ensure_managed_here(&scan_server) {
        return toast_error(refused.0);
    }
    let int = |f: &str| {
        (input.field == f)
            .then(|| input.value.parse::<i64>().ok())
            .flatten()
    };
    image_scans::views::update_scanner_limits(
        &state.db,
        server.id,
        int("scanner_memory_mb"),
        int("scanner_syft_timeout"),
        int("scanner_grype_timeout"),
        (input.field == "scanner_cpus")
            .then(|| input.value.parse::<f64>().ok())
            .flatten(),
        state.now(),
    )
    .await?;
    audit_server(&state, &ctx, &meta, &server, action).await;
    toast_success(message)
}

// ---------------------------------------------------------------------------------------------
// HostVulnerabilities
// ---------------------------------------------------------------------------------------------

const SORT_COLUMNS: [&str; 4] = ["vulnerability", "package", "fixed", "severity"];

/// `CheckUpdates::RISK_LABELS`.
fn risk_label(risk: &str) -> Option<&'static str> {
    match risk {
        "docker" => Some("Restarts Docker and the running containers"),
        "kernel" => Some("Needs a reboot to apply"),
        "system" => Some("Restarts core system services"),
        _ => None,
    }
}

fn severity_order(severity: &str) -> i64 {
    match severity {
        "low" => 1,
        "medium" => 2,
        "high" => 3,
        "critical" => 4,
        _ => 0,
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct HostState {
    sort_by: String,
    search: String,
    show_other_kernels: bool,
    page: usize,
    per_page: usize,
    format: String,
}

/// `patch_check_result` with the model's `array` cast.
fn patch_result(server: &ServerRow) -> Value {
    match server
        .settings
        .as_ref()
        .and_then(|s| s.get("patch_check_result"))
    {
        Some(Value::String(text)) => serde_json::from_str(text).unwrap_or(Value::Null),
        Some(other) => other.clone(),
        None => Value::Null,
    }
}

/// `sortedFindings()`: findings without a fix stay last in both directions; the sort is stable.
fn sorted_findings(mut findings: Vec<Finding>, sort_by: &str) -> Vec<Finding> {
    let Some((column, direction)) = sort_by.split_once('_') else {
        return findings;
    };
    if !SORT_COLUMNS.contains(&column) || !["asc", "desc"].contains(&direction) {
        return findings;
    }
    let compare = |a: &Finding, b: &Finding| match column {
        "vulnerability" => strnatcasecmp(&a.display_id, &b.display_id),
        "package" => strnatcasecmp(&a.package, &b.package),
        "fixed" => coolify_jobs::compose_parser::php::version_compare(
            a.fixed_in.as_deref().unwrap_or_default(),
            b.fixed_in.as_deref().unwrap_or_default(),
        )
        .cmp(&0),
        _ => severity_order(&a.severity).cmp(&severity_order(&b.severity)),
    };
    findings.sort_by(|a, b| {
        if column == "fixed" && a.fixed_in.is_none() != b.fixed_in.is_none() {
            return if a.fixed_in.is_none() {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Less
            };
        }
        if direction == "asc" {
            compare(a, b)
        } else {
            compare(b, a)
        }
    });
    findings
}

/// What `findingSets()` returns.
struct FindingSets {
    host_scan: Option<image_scans::host::ServerHostScanRow>,
    patch_result: Value,
    running_kernel: Option<String>,
    active: Vec<Finding>,
    other_kernels: Vec<Finding>,
    shown: Vec<Finding>,
}

/// `findingSets()`: the host scan, the patch check with the kernel that ran during the scan, and
/// the findings: in the running kernel, in other installed kernels, and the shown ones.
async fn finding_sets(
    state: &AppState,
    server: &ServerRow,
    list: &HostState,
) -> Result<FindingSets, ApiError> {
    let host_scan = image_scans::host::find(&state.db, server.id).await?;
    let mut patch_result = patch_result(server);
    let running_kernel = host_scan
        .as_ref()
        .and_then(|h| h.summary.as_ref())
        .and_then(|s| s.get("running_kernel"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| {
            patch_result
                .get("kernels")
                .and_then(|k| k.get("running"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        });
    if let (Value::Object(result), Some(running)) = (&mut patch_result, &running_kernel) {
        let kernels = result
            .entry("kernels")
            .or_insert_with(|| Value::Object(Map::new()));
        if !kernels.is_object() {
            *kernels = Value::Object(Map::new());
        }
        if let Value::Object(k) = kernels {
            k.insert("running".to_owned(), json!(running));
        }
    }
    let findings = Finding::list_from_value(host_scan.as_ref().and_then(|h| h.findings.as_ref()));
    let (other_kernels, active): (Vec<Finding>, Vec<Finding>) =
        findings.into_iter().partition(|f| {
            patches::is_other_kernel_package(&f.package, &f.version, running_kernel.as_deref())
        });
    let mut all = active.clone();
    if list.show_other_kernels {
        all.extend(other_kernels.iter().cloned());
    }
    let search = list.search.trim().to_lowercase();
    let sorted = sorted_findings(all, &list.sort_by);
    let shown = if search.is_empty() {
        sorted
    } else {
        sorted
            .into_iter()
            .filter(|f| {
                std::iter::once(f.display_id.as_str())
                    .chain(std::iter::once(f.id.as_str()))
                    .chain(f.aliases.iter().map(String::as_str))
                    .chain([f.package.as_str(), f.title.as_str()])
                    .collect::<Vec<_>>()
                    .join(" ")
                    .to_lowercase()
                    .contains(&search)
            })
            .collect()
    };
    Ok(FindingSets {
        host_scan,
        patch_result,
        running_kernel,
        active,
        other_kernels,
        shown,
    })
}

/// One row of the host findings table.
fn host_finding_value(f: &Finding, patch_result: &Value, running_kernel: Option<&str>) -> Value {
    let signal = rules::exploit_signal(f);
    let mut fixing = patches::fixing_updates(&f.package, patch_result);
    let is_own_update = fixing.len() == 1
        && fixing[0].get("package").and_then(Value::as_str) == Some(f.package.as_str());
    let mut risks: Vec<String> = Vec::new();
    for update in &fixing {
        if let Some(risk) = update.get("risk").and_then(Value::as_str)
            && !risk.is_empty()
            && !risks.iter().any(|r| r == risk)
        {
            risks.push(risk.to_owned());
        }
    }
    let mut risk_labels: Vec<&str> = risks.iter().filter_map(|r| risk_label(r)).collect();
    let kernel_status =
        patches::kernel_status(&f.package, &f.version, f.fixed_in.as_deref(), patch_result);
    let is_other_kernel = patches::is_other_kernel_package(&f.package, &f.version, running_kernel);
    let is_old_kernel = is_other_kernel && kernel_status != Some("runs_after_reboot");
    if is_other_kernel {
        fixing.clear();
        risk_labels.clear();
    }
    let aliases: Vec<&String> = f.aliases.iter().filter(|a| **a != f.display_id).collect();
    json!({
        "key": format!("{}-{}-{}", f.id, f.package, f.artifact_id),
        "id": f.id,
        "display_id": f.display_id,
        "title": f.title,
        "summary": if f.summary.is_empty() { f.title.clone() } else { f.summary.clone() },
        "package": f.package,
        "version": f.version,
        "fixed_in": f.fixed_in,
        "severity": f.severity,
        "signal": signal.map(ExploitSignal::value),
        "kev": f.kev,
        "cvss_score": f.cvss_score.map(|s| format!("{:.1}", (s * 10.0).round() / 10.0)),
        "likelihood": resource_scans::exploit_likelihood(f.epss),
        "aliases": aliases,
        "advisory_url": resource_scans::advisory_url(&f.display_id),
        "fixing_updates": fixing.iter().map(|u| json!({
            "package": u.get("package").cloned().unwrap_or(Value::Null),
            "new_version": u.get("new_version").cloned().unwrap_or(Value::Null),
        })).collect::<Vec<_>>(),
        "is_own_update": is_own_update,
        "risk_labels": risk_labels,
        "kernel_status": kernel_status,
        "is_other_kernel": is_other_kernel,
        "is_old_kernel": is_old_kernel,
    })
}

/// `HostVulnerabilities::render()`.
async fn host_render(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    Json(mut list): Json<HostState>,
) -> ApiResult {
    let jobs = jobs(&state)?;
    let server = find_server(&state, &ctx, &uuid).await?;
    let scan_server = ScanServer::load(jobs, server.id).await.map_err(internal)?;
    list.per_page = if list.per_page == 0 {
        10
    } else {
        list.per_page.clamp(1, 100)
    };
    let sets = finding_sets(&state, &server, &list).await?;
    let now = views::now();
    let host_findings = sets.active.len()
        + if list.show_other_kernels {
            sets.other_kernels.len()
        } else {
            0
        };
    let installed: Vec<Value> = sets
        .patch_result
        .get("kernels")
        .and_then(|k| k.get("installed"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let running_version = installed
        .iter()
        .find(|k| k.get("release").and_then(Value::as_str) == sets.running_kernel.as_deref())
        .and_then(|k| k.get("version"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let has_old_kernels = running_version.as_ref().is_some_and(|running| {
        installed.iter().any(|k| {
            k.get("version")
                .and_then(Value::as_str)
                .is_some_and(|v| coolify_jobs::compose_parser::php::version_compare(v, running) < 0)
        })
    });
    let mut fixed_after_reboot = 0usize;
    for f in &sets.active {
        if patches::kernel_status(
            &f.package,
            &f.version,
            f.fixed_in.as_deref(),
            &sets.patch_result,
        ) == Some("fixed_after_reboot")
        {
            fixed_after_reboot += 1;
        }
    }
    let summary_other = sets
        .host_scan
        .as_ref()
        .and_then(|h| h.summary.as_ref())
        .and_then(|s| s.get("other_kernels"))
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;
    let total = sets.shown.len();
    let last_page = total.div_ceil(list.per_page).max(1);
    let page = list.page.clamp(1, last_page);
    let shown: Vec<Value> = sets
        .shown
        .iter()
        .skip((page - 1) * list.per_page)
        .take(list.per_page)
        .map(|f| host_finding_value(f, &sets.patch_result, sets.running_kernel.as_deref()))
        .collect();
    let host_scan_error = sets.host_scan.as_ref().and_then(|h| {
        let code = ErrorCode::from_value(h.error_code.as_deref()?)?;
        let mut context: Map<String, Value> = h
            .error_context
            .as_ref()
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        context.insert("server".to_owned(), json!(server_name(&server)));
        Some(code.message(&context))
    });
    let host_scan = sets.host_scan.as_ref().map(|h| {
        json!({
            "summary": h.summary,
            "scanned_at": h.scanned_at.map(|t| json!({
                "human": views::diff_for_humans(t, now),
                "title": t.format("%Y-%m-%d %H:%M:%S").to_string(),
            })),
            "findings_truncated": h.findings_truncated,
            "eol": h.eol,
            "error_detail": h.error_context.as_ref().and_then(|c| c.get("detail")).and_then(Value::as_str),
        })
    });
    Ok(ok(json!({
        "name": server_name(&server),
        "can_update": ctx.is_team_admin(),
        "host_scan_running": cache::has(&host::running_cache_key(server.id)),
        "host_scan_available": scan_jobs::can_run_on(&scan_server),
        "host_scan": host_scan,
        "host_scan_error": host_scan_error,
        "has_result": !sets.patch_result.is_null() && sets.patch_result != json!([]),
        "reboot_required": sets.patch_result.get("reboot_required") == Some(&Value::Bool(true)),
        "package_manager": sets.patch_result.get("package_manager"),
        "fixed_after_reboot": fixed_after_reboot,
        "other_kernel_count": sets.other_kernels.len().max(summary_other),
        "has_old_kernels": has_old_kernels,
        "running_kernel": sets.running_kernel,
        "boot_kernel": patches::boot_kernel(&sets.patch_result)
            .and_then(|k| k.get("release").cloned()),
        "host_finding_count": host_findings,
        "shown_count": total,
        "findings": shown,
        "pagination": {"total": total, "page": page, "per_page": list.per_page, "last_page": last_page},
        "sort_by": list.sort_by,
    }))
    .into_response())
}

/// `scanHostPackages()`.
async fn host_scan(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let jobs = jobs(&state)?;
    let server = find_server(&state, &ctx, &uuid).await?;
    let scan_server = ScanServer::load(jobs, server.id).await.map_err(internal)?;
    if scan_server.is_transferred_away() {
        return toast_error(MANAGED_ELSEWHERE);
    }
    if !scan_jobs::can_run_on(&scan_server) {
        return toast_error("Turn on image scanning for this server first.");
    }
    host::dispatch_manual(jobs, &scan_server);
    Ok(ok(json!({"host_scan_running": true})).into_response())
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct PackageInput {
    package: String,
}

/// `updateVulnerablePackage($package)`: updates the package of a host finding with the updates
/// that the last patch check found for it (`ManagesServerPackageUpdates::runUpdate`).
async fn host_update_package(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    Json(input): Json<PackageInput>,
) -> ApiResult {
    let jobs = jobs(&state)?;
    let server = find_server(&state, &ctx, &uuid).await?;
    let host_scan = image_scans::host::find(&state.db, server.id).await?;
    let is_finding = Finding::list_from_value(host_scan.as_ref().and_then(|h| h.findings.as_ref()))
        .iter()
        .any(|f| f.package == input.package);
    let result = patch_result(&server);
    let packages: Vec<String> = patches::fixing_updates(&input.package, &result)
        .iter()
        .filter_map(|u| u.get("package").and_then(Value::as_str).map(str::to_owned))
        .collect();
    if !is_finding || packages.is_empty() {
        return toast_error(
            "This package has no update in the last check. Run \"Check for updates\" on the Patching page.",
        );
    }
    let srv = Srv::from_row(jobs, server.clone());
    if srv.info.transferred {
        return toast_error(MANAGED_ELSEWHERE);
    }
    let manager = result
        .get("package_manager")
        .and_then(Value::as_str)
        .filter(|m| !m.is_empty())
        .map(str::to_owned);
    match patches::update_packages(&srv, &packages, manager.as_deref(), false).await {
        Ok(activity) => {
            tracing::info!(
                target: "audit",
                event = "ui.server.packages_update_requested",
                team_id = server.team_id,
                server_uuid = %server.uuid,
                detail = %packages.join(" "),
                "server patch action"
            );
            Ok(ok(json!({"activity_id": activity})).into_response())
        }
        Err(e) => toast_error(e),
    }
}

/// `copyList($format)` and `downloadList($format)` of the host findings.
async fn host_export(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    Json(list): Json<HostState>,
) -> ApiResult {
    if !["markdown", "ids", "csv", "json"].contains(&list.format.as_str()) {
        return Err(super::not_found());
    }
    let server = find_server(&state, &ctx, &uuid).await?;
    let sets = finding_sets(&state, &server, &list).await?;
    // The ID that the page shows.
    let rows: Vec<ExportRow> = sets
        .shown
        .into_iter()
        .map(|f| ExportRow {
            state: if f.fixed_in.as_deref().is_some_and(|v| !v.trim().is_empty()) {
                "fixed".to_owned()
            } else {
                "no_fix".to_owned()
            },
            id: Some(if f.display_id.is_empty() {
                f.id.clone()
            } else {
                f.display_id.clone()
            }),
            finding: f,
            ignore: None,
            image: String::new(),
        })
        .collect();
    let name = server_name(&server);
    match list.format.as_str() {
        "ids" => Ok(ok(json!({"content": export::findings_ids(&rows)})).into_response()),
        "markdown" => {
            let title = format!(
                "Vulnerabilities of the server {name} ({})",
                count_text(rows.len(), "finding", "findings")
            );
            Ok(
                ok(json!({"content": export::findings_markdown(&title, &rows, false, false)}))
                    .into_response(),
            )
        }
        format => {
            let now = views::now();
            let content = if format == "csv" {
                export::findings_csv(&rows, &export::FIX_STATES)
            } else {
                export::findings_json(
                    json!({"type": "server", "name": name, "uuid": server.uuid}),
                    &rows,
                    &export::FIX_STATES,
                    now,
                )
            };
            let file = format!(
                "{}-host-vulnerabilities-{}.{format}",
                slug_or(&name, "server"),
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

    #[test]
    fn listbox_options_keep_a_saved_value() {
        let options = listbox_options(&[0.5, 1.0], 2.0, cpu_label);
        assert_eq!(options.len(), 3);
        assert_eq!(options[1]["label"], "1 CPU");
        assert_eq!(options[2]["value"], "2");
        assert_eq!(options[2]["label"], "2 CPUs (more than this server has)");
        assert_eq!(float_text(0.5), "0.5");
        assert_eq!(float_text(5.0), "5");
    }

    fn finding(id: &str, package: &str, fixed: Option<&str>, severity: &str) -> Finding {
        Finding {
            id: id.to_owned(),
            aliases: Vec::new(),
            severity: severity.to_owned(),
            package: package.to_owned(),
            version: "1".to_owned(),
            r#type: "deb".to_owned(),
            artifact_id: String::new(),
            fixed_in: fixed.map(str::to_owned),
            fix_state: String::new(),
            kev: false,
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

    #[test]
    fn findings_without_a_fix_sort_last() {
        let list = vec![
            finding("CVE-1", "a", None, "high"),
            finding("CVE-2", "b", Some("2.0"), "low"),
            finding("CVE-3", "c", Some("1.10"), "critical"),
        ];
        let ids = |v: Vec<Finding>| v.into_iter().map(|f| f.id).collect::<Vec<_>>();
        assert_eq!(
            ids(sorted_findings(list.clone(), "fixed_asc")),
            vec!["CVE-3", "CVE-2", "CVE-1"]
        );
        assert_eq!(
            ids(sorted_findings(list.clone(), "fixed_desc")),
            vec!["CVE-2", "CVE-3", "CVE-1"]
        );
        assert_eq!(
            ids(sorted_findings(list.clone(), "severity_desc")),
            vec!["CVE-3", "CVE-1", "CVE-2"]
        );
        assert_eq!(
            ids(sorted_findings(list.clone(), "bad_asc")),
            vec!["CVE-1", "CVE-2", "CVE-3"]
        );
        assert_eq!(risk_label("kernel"), Some("Needs a reboot to apply"));
    }
}
