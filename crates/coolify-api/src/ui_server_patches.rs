//! Session endpoints behind `Server\Security\Patches`.

use axum::Extension;
use axum::Json;
use axum::Router;
use axum::extract::{Path, State};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use coolify_compat::cron::runs_at_most_hourly;
use coolify_db::servers::{ColumnValue, ServerRow};
use coolify_jobs::Jobs;
use coolify_jobs::server::ServerInfo;
use coolify_jobs::server_ops::{MARKS, Srv, patches};
use serde_json::{Map, Value, json};

use crate::auth::AuthCtx;
use crate::response::{ApiError, ApiResult, ok};
use crate::routes::server_settings::{authorize_update, find_server};
use crate::serialize::php_truthy;
use crate::state::AppState;
use crate::ui_server_destinations::toast_error;
use crate::ui_server_general::SERVER_PAGE;

const UNAUTHORIZED: &str = "This action is unauthorized.";
const MANAGED_ELSEWHERE: &str = "This server is managed by another Coolify instance.";
const INVALID_TIME: &str = "Invalid time. Use the 24-hour format HH:MM, for example 03:00.";

pub fn routes() -> Router<AppState> {
    let p = |s: &str| format!("{SERVER_PAGE}/security/patches{s}");
    Router::new()
        .route(&p(""), get(show).post(submit))
        .route(&p("/check"), post(check))
        .route(&p("/update"), post(update))
        .route(&p("/update-all"), post(update_all))
        .route(&p("/update-selected"), post(update_selected))
        .route(&p("/remove-unused"), post(remove_unused))
        .route(&p("/reboot"), post(reboot))
        .route(&p("/auto-updates"), post(toggle_auto))
        .route(&format!("{SERVER_PAGE}/activity/{{id}}"), get(activity))
}

fn setting(s: &ServerRow, k: &str) -> Value {
    s.settings
        .as_ref()
        .and_then(|r| r.get(k))
        .cloned()
        .unwrap_or(Value::Null)
}

/// `patch_check_result` with the model's `array` cast.
fn saved_result(s: &ServerRow) -> Value {
    match setting(s, "patch_check_result") {
        Value::String(text) => serde_json::from_str(&text).unwrap_or(Value::Null),
        other => other,
    }
}

fn result_str(s: &ServerRow, k: &str) -> Option<String> {
    saved_result(s)
        .get(k)
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}

fn body_str(body: &Map<String, Value>, k: &str) -> String {
    body.get(k)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string()
}

fn audit(s: &ServerRow, event: &'static str, detail: &str) {
    tracing::info!(
        target: "audit",
        event,
        team_id = ServerInfo::from_row(s).team_id,
        server_uuid = %s.uuid,
        detail,
        "server patch action"
    );
}

/// `Patches::mount` with `loadResult` and `syncSettings`.
async fn show(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    authorize_update(&ctx)?;
    let (text, _) = crate::ui::confirmation_steps(&state, ctx.user_id).await?;
    Ok(ok(json!({
        "confirm_with_text": text,
        "name": s.record.get("name").cloned().unwrap_or(Value::Null),
        "patch_check_frequency": setting(&s, "patch_check_frequency").as_str().unwrap_or_default(),
        "auto_security_updates": php_truthy(&setting(&s, "is_auto_security_updates_enabled")),
        "auto_security_updates_time": setting(&s, "auto_security_updates_time").as_str().unwrap_or("03:00"),
        "result": saved_result(&s),
        "error": setting(&s, "patch_check_error"),
        "checked_at": setting(&s, "patch_checked_at"),
        "checking": MARKS.has(&patches::running_key(&s.uuid)),
    }))
    .into_response())
}

fn jobs(state: &AppState) -> Result<&Jobs, ApiError> {
    state.jobs.as_ref().ok_or(ApiError::NotPorted)
}

/// `applyAutomaticSecurityUpdates`.
async fn apply_auto(
    state: &AppState,
    s: &ServerRow,
    enabled: bool,
    time: &str,
) -> Result<Result<(), String>, ApiError> {
    if ServerInfo::from_row(s).transferred {
        return Ok(Err(MANAGED_ELSEWHERE.to_string()));
    }
    let manager = result_str(s, "package_manager");
    if !patches::auto_updates_supported(manager.as_deref()) {
        return Ok(Err("Automatic security updates are supported only on Debian, Ubuntu, and RHEL-family servers. Run \"Check for updates\" first, so Coolify knows the package manager.".to_string()));
    }
    let srv = Srv::from_row(jobs(state)?, s.clone());
    let result = patches::configure_auto_updates(
        &srv,
        manager.as_deref().unwrap_or_default(),
        enabled,
        time,
    )
    .await;
    audit(
        s,
        "ui.server.auto_security_updates_updated",
        if result.is_ok() { "success" } else { "failed" },
    );
    Ok(result)
}

/// `Patches::submit`.
async fn submit(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    Json(body): Json<Map<String, Value>>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    if !ctx.is_team_admin() {
        return toast_error(UNAUTHORIZED);
    }
    let frequency = body_str(&body, "patchCheckFrequency");
    if !frequency.is_empty() && !runs_at_most_hourly(&frequency) {
        return toast_error(
            "Invalid check frequency. Use a cron expression or a preset (daily, weekly, monthly) that runs at most once per hour.",
        );
    }
    let time = body_str(&body, "autoSecurityUpdatesTime");
    if !patches::valid_update_time(&time) {
        return toast_error(INVALID_TIME);
    }
    let saved_time = setting(&s, "auto_security_updates_time");
    let enabled = php_truthy(&setting(&s, "is_auto_security_updates_enabled"));
    if enabled
        && saved_time.as_str() != Some(time.as_str())
        && let Err(e) = apply_auto(&state, &s, true, &time).await?
    {
        return toast_error(e);
    }
    let new_frequency = (!frequency.is_empty()).then_some(frequency);
    let mut changes = Vec::new();
    if setting(&s, "patch_check_frequency").as_str() != new_frequency.as_deref() {
        changes.push((
            "patch_check_frequency",
            ColumnValue::Text(new_frequency.clone()),
        ));
    }
    if saved_time.as_str() != Some(time.as_str()) {
        changes.push((
            "auto_security_updates_time",
            ColumnValue::Text(Some(time.clone())),
        ));
    }
    if !changes.is_empty() {
        coolify_db::servers::update_settings(&state.db, s.id, &changes, state.now()).await?;
        let names = changes
            .iter()
            .map(|(c, _)| *c)
            .collect::<Vec<_>>()
            .join(",");
        audit(&s, "ui.server.patch_settings_updated", &names);
    }
    Ok(ok(json!({"message": "Patch settings saved."})).into_response())
}

/// `Patches::toggleAutomaticSecurityUpdates`.
async fn toggle_auto(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    Json(body): Json<Map<String, Value>>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    if !ctx.is_team_admin() {
        return toast_error(UNAUTHORIZED);
    }
    let enabled = !php_truthy(&setting(&s, "is_auto_security_updates_enabled"));
    let time = body_str(&body, "autoSecurityUpdatesTime");
    if !patches::valid_update_time(&time) {
        return toast_error(INVALID_TIME);
    }
    if let Err(e) = apply_auto(&state, &s, enabled, &time).await? {
        return toast_error(e);
    }
    coolify_db::servers::update_settings(
        &state.db,
        s.id,
        &[
            (
                "is_auto_security_updates_enabled",
                ColumnValue::Bool(enabled),
            ),
            ("auto_security_updates_time", ColumnValue::Text(Some(time))),
        ],
        state.now(),
    )
    .await?;
    Ok(ok(json!({"message": if enabled {
        "Automatic security updates turned on."
    } else {
        "Automatic security updates turned off."
    }}))
    .into_response())
}

/// `Patches::checkForUpdates`.
async fn check(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    if !ctx.is_team_admin() {
        return toast_error(UNAUTHORIZED);
    }
    if ServerInfo::from_row(&s).transferred {
        return toast_error(MANAGED_ELSEWHERE);
    }
    patches::dispatch_manual(jobs(&state)?, s.id, &s.uuid);
    Ok(ok(json!({"checking": true})).into_response())
}

/// `Patches::runUpdate`.
async fn run_update(state: &AppState, s: ServerRow, all: bool, package: Option<&str>) -> ApiResult {
    if ServerInfo::from_row(&s).transferred {
        return toast_error(MANAGED_ELSEWHERE);
    }
    let manager = result_str(&s, "package_manager");
    if manager.is_none() || result_str(&s, "osId").is_none() {
        return toast_error("Run \"Check for updates\" first.");
    }
    let srv = Srv::from_row(jobs(state)?, s.clone());
    match patches::update_package(&srv, package, manager.as_deref(), all).await {
        Ok(activity) => {
            audit(
                &s,
                "ui.server.packages_update_requested",
                package.unwrap_or("all"),
            );
            Ok(ok(json!({"activity_id": activity})).into_response())
        }
        Err(e) => toast_error(e),
    }
}

/// `Patches::updateAllPackages`.
async fn update_all(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    if !ctx.is_team_admin() {
        return toast_error(UNAUTHORIZED);
    }
    run_update(&state, s, true, None).await
}

/// `Patches::updatePackage`.
async fn update(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    Json(body): Json<Map<String, Value>>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    if !ctx.is_team_admin() {
        return toast_error(UNAUTHORIZED);
    }
    let package = body
        .get("package")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let known = saved_result(&s)
        .get("updates")
        .and_then(Value::as_array)
        .is_some_and(|u| {
            u.iter()
                .any(|e| e.get("package").and_then(Value::as_str) == Some(package))
        });
    if !known {
        return toast_error("This package has no update in the last check.");
    }
    run_update(&state, s, false, Some(package)).await
}

/// `savedPackages()`: the packages of the last check.
fn saved_packages(s: &ServerRow) -> Vec<String> {
    saved_result(s)
        .get("updates")
        .and_then(Value::as_array)
        .map(|updates| {
            updates
                .iter()
                .filter_map(|u| u.get("package").and_then(Value::as_str))
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// `array_values(array_intersect($this->savedPackages(), $this->selectedPackages))`.
fn selected_saved_packages(saved: &[String], selected: &[String]) -> Vec<String> {
    saved
        .iter()
        .filter(|p| selected.contains(p))
        .cloned()
        .collect()
}

/// `Patches::updateSelectedPackages` (v4.7.5).
async fn update_selected(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    Json(body): Json<Map<String, Value>>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    if !ctx.is_team_admin() {
        return toast_error(UNAUTHORIZED);
    }
    let selected: Vec<String> = body
        .get("packages")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let packages = selected_saved_packages(&saved_packages(&s), &selected);
    if packages.is_empty() {
        return toast_error("Select the packages to update.");
    }
    // `runUpdate(packages: $packages)`.
    if ServerInfo::from_row(&s).transferred {
        return toast_error(MANAGED_ELSEWHERE);
    }
    let manager = result_str(&s, "package_manager");
    if manager.is_none() || result_str(&s, "osId").is_none() {
        return toast_error("Run \"Check for updates\" first.");
    }
    let srv = Srv::from_row(jobs(&state)?, s.clone());
    match patches::update_packages(&srv, &packages, manager.as_deref(), false).await {
        Ok(activity) => {
            audit(
                &s,
                "ui.server.packages_update_requested",
                &packages.join(","),
            );
            Ok(ok(json!({"activity_id": activity})).into_response())
        }
        Err(e) => toast_error(e),
    }
}

/// `ManagesServerPackageUpdates::removeUnusedPackages` (v4.7.5): removes packages that nothing
/// needs anymore and old kernels (apt autoremove or the equivalent).
async fn remove_unused(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    if !ctx.is_team_admin() {
        return toast_error(UNAUTHORIZED);
    }
    if ServerInfo::from_row(&s).transferred {
        return toast_error(MANAGED_ELSEWHERE);
    }
    let manager = result_str(&s, "package_manager");
    let srv = Srv::from_row(jobs(&state)?, s.clone());
    match patches::remove_unused_packages(&srv, manager.as_deref()).await {
        Ok(activity) => {
            audit(&s, "ui.server.packages_update_requested", "unused");
            Ok(ok(json!({"activity_id": activity})).into_response())
        }
        Err(e) => toast_error(e),
    }
}

/// `Patches::rebootServer`.
async fn reboot(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    if !ctx.is_team_admin() {
        return toast_error(UNAUTHORIZED);
    }
    let srv = Srv::from_row(jobs(&state)?, s.clone());
    if let Err(e) = patches::reboot_server(&srv).await {
        audit(&s, "ui.server.reboot_requested", "failed");
        return toast_error(e);
    }
    audit(&s, "ui.server.reboot_requested", "started");
    Ok(ok(json!({"message": "Reboot started. The server is back in a few minutes, and Coolify then checks it again."})).into_response())
}

/// What `livewire:activity-monitor` shows for `activityMonitor($id)`, for an activity of this server.
async fn activity(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((uuid, id)): Path<(String, i64)>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    let found = coolify_db::databases::activity_by_id(&state.db, id).await?;
    let activity = found
        .filter(|(props, _)| props.get("server_uuid").and_then(Value::as_str) == Some(&s.uuid))
        .map(|(props, description)| {
            let entries: Value = serde_json::from_str(&description).unwrap_or(json!([]));
            json!({"id": id, "status": props.get("status").cloned().unwrap_or(Value::Null), "entries": entries})
        });
    Ok(ok(json!({"activity": activity})).into_response())
}

#[cfg(test)]
mod v475_tests {
    use super::selected_saved_packages;

    // `updateSelectedPackages()`: only packages of the last check, in the order of the check.
    #[test]
    fn only_saved_packages_can_be_selected() {
        let saved = vec!["openssl".to_owned(), "libc6".to_owned(), "curl".to_owned()];
        let selected = vec![
            "curl".to_owned(),
            "evil; rm -rf /".to_owned(),
            "openssl".to_owned(),
        ];
        assert_eq!(
            selected_saved_packages(&saved, &selected),
            ["openssl", "curl"]
        );
        assert!(selected_saved_packages(&saved, &[]).is_empty());
    }
}
