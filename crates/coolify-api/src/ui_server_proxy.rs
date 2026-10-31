//! Session endpoints behind `Server\Proxy`, `Server\Proxy\DynamicConfigurations`,
//! `NewDynamicConfiguration`, `DynamicConfigurationNavbar`, `Proxy\Certificates`, the logs pages
//! and the proxy actions of `Server\Navbar`.

use axum::Extension;
use axum::Json;
use axum::Router;
use axum::extract::{Path, Query, State};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use base64::Engine as _;
use coolify_db::servers::{self, ColumnValue as C, ServerRow};
use coolify_jobs::server::ServerInfo;
use coolify_jobs::server_ops::proxy as proxy_ops;
use coolify_jobs::server_ops::{Srv, acme, proxy_config, shell_arg};
use serde_json::{Map, Value, json};
use std::collections::HashMap;

use crate::auth::AuthCtx;
use crate::input::Input;
use crate::response::{ApiError, ApiResult, ok};
use crate::routes::server_settings::{Setting, find_server, save_settings};
use crate::serialize::php_truthy;
use crate::state::AppState;
use crate::ui_server_destinations::toast_error;
use crate::ui_server_general::SERVER_PAGE;
use crate::validation::{Field, Rule, validate};

const UNAUTHORIZED: &str = "This action is unauthorized.";

pub fn routes() -> Router<AppState> {
    let p = |s: &str| format!("{SERVER_PAGE}/{s}");
    Router::new()
        .route(&p("proxy"), get(show).post(submit))
        .route(&p("proxy/configuration"), get(configuration))
        .route(&p("proxy/select"), post(select))
        .route(&p("proxy/change"), post(change))
        .route(&p("proxy/instant"), post(instant))
        .route(&p("proxy/redirect"), post(redirect))
        .route(&p("proxy/reset-error-page"), post(reset_error_page))
        .route(&p("proxy/reset-configuration"), post(reset_configuration))
        .route(&p("proxy/action/{action}"), post(action))
        .route(&p("proxy/activity"), get(activity))
        .route(&p("proxy/dynamic"), get(dynamic).post(save_dynamic))
        .route(&p("proxy/dynamic/delete"), post(delete_dynamic))
        .route(&p("proxy/certificates"), get(certificates))
        .route(&p("proxy/certificates/delete"), post(delete_certificate))
        .route(
            &p("proxy/certificates/backups/restore"),
            post(restore_backup),
        )
        .route(&p("proxy/certificates/backups/delete"), post(delete_backup))
        .route(&p("logs/{container}"), get(logs))
}

fn proxy_str<'a>(s: &'a ServerRow, k: &str) -> Option<&'a str> {
    s.record
        .get("proxy")
        .and_then(|p| p.get(k))
        .and_then(Value::as_str)
}

/// `$server->hasPendingProxyConfiguration()`.
pub(crate) fn pending(s: &ServerRow) -> bool {
    if proxy_str(s, "status") != Some("running") {
        return false;
    }
    let proxy = s.record.get("proxy");
    if proxy
        .and_then(|p| p.get("certificates_restart_required"))
        .is_some_and(php_truthy)
    {
        return true;
    }
    let saved = proxy_str(s, "last_saved_settings");
    let applied = proxy_str(s, "last_applied_settings");
    saved.is_some() && applied.is_some() && saved != applied
}

/// `get_versions_data()['traefik']`.
fn traefik_versions() -> Map<String, Value> {
    coolify_jobs::server_ops::versions_data()
        .get("traefik")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default()
}

/// `traefikVersionForWarning`.
fn traefik_version_for_warning(s: &ServerRow) -> Option<String> {
    if let Some(v) = s
        .record
        .get("detected_traefik_version")
        .and_then(Value::as_str)
    {
        return Some(v.to_string());
    }
    if proxy_str(s, "status") != Some("running") || pending(s) {
        return None;
    }
    let configuration = proxy_str(s, "last_saved_proxy_configuration")?;
    configuration.lines().find_map(|l| {
        let rest = l.trim().strip_prefix("image:")?.trim();
        let rest = rest.trim_matches(|c| c == '\'' || c == '"');
        let v = rest.strip_prefix("traefik:")?;
        let plain = v.trim_start_matches('v');
        let numeric = plain.split('.').count() >= 2
            && plain.split('.').count() <= 3
            && plain
                .split('.')
                .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()));
        (numeric || v == "latest").then(|| v.to_string())
    })
}

fn version_lt(a: &str, b: &str) -> bool {
    coolify_jobs::server_ops::version_compare(a, b) == std::cmp::Ordering::Less
}

/// `latestTraefikVersion`, `isTraefikOutdated`, `newerTraefikBranchAvailable` and
/// `latestNewerTraefikVersion`.
fn traefik_warnings(s: &ServerRow) -> Value {
    let versions = traefik_versions();
    let current = traefik_version_for_warning(s);
    let branch_of = |v: &str| -> Option<String> {
        let mut parts = v.trim_start_matches('v').split('.');
        Some(format!("{}.{}", parts.next()?, parts.next()?))
    };
    let newest = versions
        .values()
        .filter_map(Value::as_str)
        .map(|v| v.trim_start_matches('v').to_string())
        .max_by(|a, b| coolify_jobs::server_ops::version_compare(a, b))
        .map(|v| format!("v{v}"));
    let latest = current
        .as_deref()
        .filter(|c| *c != "latest")
        .and_then(branch_of)
        .and_then(|b| versions.get(&format!("v{b}")).and_then(Value::as_str))
        .map(|v| format!("v{}", v.trim_start_matches('v')))
        .or(newest);
    let is_traefik = proxy_str(s, "type") == Some("TRAEFIK");
    let usable = current.as_deref().filter(|c| *c != "latest" && is_traefik);
    let outdated = match (usable, &latest) {
        (Some(c), Some(l)) => version_lt(c.trim_start_matches('v'), l.trim_start_matches('v')),
        _ => false,
    };
    let configured_branch = if proxy_str(s, "status") == Some("running") && !pending(s) {
        proxy_str(s, "last_saved_proxy_configuration").and_then(|c| {
            c.lines().find_map(|l| {
                let rest = l.trim().strip_prefix("image:")?.trim();
                let v = rest
                    .trim_matches(|c| c == '\'' || c == '"')
                    .strip_prefix("traefik:")?;
                branch_of(v)
            })
        })
    } else {
        None
    };
    let mut newer: Option<String> = None;
    if let Some(c) = usable {
        let info = s.record.get("traefik_outdated_info");
        let stored_current = info
            .and_then(|i| i.get("current"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim_start_matches('v');
        let is_minor =
            info.and_then(|i| i.get("type")).and_then(Value::as_str) == Some("minor_upgrade");
        let target = info
            .and_then(|i| i.get("upgrade_target"))
            .and_then(Value::as_str);
        if stored_current == c.trim_start_matches('v') && is_minor && target.is_some() {
            let t = target.unwrap_or_default();
            let t = if t.starts_with('v') {
                t.to_string()
            } else {
                format!("v{t}")
            };
            let done = configured_branch
                .as_deref()
                .is_some_and(|b| !version_lt(b, t.trim_start_matches('v')));
            newer = (!done).then_some(t);
        } else if let Some(current_branch) = branch_of(c) {
            let best = versions
                .keys()
                .map(|b| b.trim_start_matches('v').to_string())
                .filter(|b| version_lt(&current_branch, b))
                .max_by(|a, b| coolify_jobs::server_ops::version_compare(a, b));
            newer = best
                .filter(|b| {
                    configured_branch
                        .as_deref()
                        .is_none_or(|configured| version_lt(configured, b))
                })
                .map(|b| format!("v{b}"));
        }
    }
    let newer_version = newer
        .as_deref()
        .and_then(|b| versions.get(b))
        .and_then(Value::as_str)
        .map(|v| format!("v{}", v.trim_start_matches('v')));
    json!({
        "version": current,
        "detected": s.record.get("detected_traefik_version").is_some_and(|v| !v.is_null()),
        "latest": latest,
        "outdated": outdated,
        "newer_branch": newer,
        "newer_version": newer_version,
    })
}

/// What the `Server\Proxy` component holds after `mount`.
async fn show(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    let settings = s.settings.clone().unwrap_or_default();
    let proxy = s.record.get("proxy");
    let info = ServerInfo::from_row(&s);
    let path = match &state.jobs {
        Some(jobs) => Srv::from_row(jobs, s.clone()).proxy_path(),
        None => String::new(),
    };
    Ok(ok(json!({
        "proxy_type": proxy_str(&s, "type").filter(|t| !t.is_empty()),
        "status": proxy_str(&s, "status"),
        "redirect_enabled": proxy.and_then(|p| p.get("redirect_enabled")).is_none_or(php_truthy),
        "redirect_url": proxy.and_then(|p| p.get("redirect_url")),
        "custom_error_page": s.record.get("proxy_error_page"),
        "generate_exact_labels": settings.get("generate_exact_labels").is_some_and(php_truthy),
        "certificate_resolver": settings.get("traefik_certificate_resolver"),
        "pending": pending(&s),
        "changed": proxy_str(&s, "last_applied_settings").is_some()
            && proxy_str(&s, "last_saved_settings") != proxy_str(&s, "last_applied_settings"),
        "is_swarm": info.is_swarm(),
        "configuration_path": format!("{}/docker-compose.yml", path.trim_end_matches('/')),
        "error_page_kb": proxy_ops::PROXY_ERROR_PAGE_MAX_BYTES / 1024,
        "traefik": traefik_warnings(&s),
    }))
    .into_response())
}

fn srv(state: &AppState, s: ServerRow) -> Result<Srv, ApiError> {
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    Ok(Srv::from_row(jobs, s))
}

async fn reload(state: &AppState, id: i64) -> Result<ServerRow, ApiError> {
    servers::find_by_id(&state.db, id)
        .await?
        .ok_or(ApiError::Internal)
}

/// `Proxy::loadProxyConfiguration`.
async fn configuration(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    if !ctx.is_team_admin() {
        return Ok(ok(json!({"configuration": null})).into_response());
    }
    let mut srv = srv(&state, s)?;
    match proxy_config::get_proxy_configuration(&mut srv, false).await {
        Ok(c) => Ok(ok(json!({"configuration": c})).into_response()),
        Err(e) => toast_error(e),
    }
}

/// `Proxy::selectProxy`.
async fn select(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    Json(body): Json<Map<String, Value>>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    if !ctx.is_team_admin() {
        return toast_error(UNAUTHORIZED);
    }
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    let t = body
        .get("proxy_type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    match proxy_ops::change_proxy(jobs, s.id, t).await {
        Ok(_) => Ok(ok(json!({})).into_response()),
        Err(e) => toast_error(e),
    }
}

/// `Proxy::changeProxy`.
async fn change(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    if !ctx.is_team_admin() {
        return toast_error(UNAUTHORIZED);
    }
    coolify_db::server_ops::save_proxy(&state.db, s.id, None, state.now()).await?;
    servers::update_columns(
        &state.db,
        s.id,
        &[
            ("detected_traefik_version", C::Text(None)),
            ("traefik_outdated_info", C::Text(None)),
        ],
        state.now(),
    )
    .await?;
    Ok(ok(json!({})).into_response())
}

/// `saveSettings`.
async fn save_routing(
    state: &AppState,
    s: &ServerRow,
    input: &Input,
    ctx: &AuthCtx,
) -> Result<(), ApiError> {
    let flag = input.get("generate_exact_labels").is_some_and(php_truthy);
    let resolver = input
        .str("certificate_resolver")
        .filter(|r| !r.is_empty())
        .map(str::to_string);
    let changed = save_settings(
        state,
        s,
        &[
            ("generate_exact_labels", Setting::Bool(flag)),
            ("traefik_certificate_resolver", Setting::Text(resolver)),
        ],
    )
    .await?;
    if !changed.is_empty() {
        tracing::info!(target: "audit", event = "ui.server.proxy.updated", team_id = ctx.team_id, server_uuid = %s.uuid, "proxy updated");
    }
    if changed.contains(&"traefik_certificate_resolver") {
        let srv = srv(state, reload(state, s.id).await?)?;
        let _ = proxy_ops::setup_dynamic_proxy_configuration(&srv).await;
        let _ = proxy_ops::setup_default_redirect(&srv).await;
    }
    Ok(())
}

/// `Proxy::rules`.
fn rules(input: &Input) -> Result<(), ApiError> {
    fn page_size(v: &Value) -> Option<String> {
        let t = v.as_str()?;
        (t.len() > proxy_ops::PROXY_ERROR_PAGE_MAX_BYTES).then(|| {
            format!(
                "The custom error page must not be larger than {} KB.",
                proxy_ops::PROXY_ERROR_PAGE_MAX_BYTES / 1024
            )
        })
    }
    validate(
        input,
        &[
            Field {
                name: "generate_exact_labels",
                rules: vec![Rule::Required, Rule::Boolean],
            },
            Field {
                name: "certificate_resolver",
                rules: vec![
                    Rule::Nullable,
                    Rule::String,
                    Rule::Max(255),
                    Rule::Regex(&crate::routes::server_proxy::RESOLVER),
                ],
            },
            Field {
                name: "custom_error_page",
                rules: vec![Rule::Nullable, Rule::String, Rule::CustomOwned(page_size)],
            },
        ],
        &[],
        &[
            "generate_exact_labels",
            "redirect_url",
            "certificate_resolver",
            "custom_error_page",
            "proxy_settings",
        ],
    )
}

/// `Proxy::instantSave`.
async fn instant(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    Json(body): Json<Map<String, Value>>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    if !ctx.is_team_admin() {
        return toast_error(UNAUTHORIZED);
    }
    let input = Input(body);
    rules(&input)?;
    save_routing(&state, &s, &input, &ctx).await?;
    Ok(ok(json!({"message": "Settings saved."})).into_response())
}

/// `Proxy::instantSaveRedirect`.
async fn redirect(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    Json(body): Json<Map<String, Value>>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    if !ctx.is_team_admin() {
        return toast_error(UNAUTHORIZED);
    }
    let enabled = body.get("redirect_enabled").is_some_and(php_truthy);
    let mut srv = srv(&state, s)?;
    srv.proxy_set("redirect_enabled", Value::Bool(enabled));
    if let Err(e) = srv.save_proxy().await {
        return toast_error(e);
    }
    if let Err(e) = proxy_ops::setup_default_redirect(&srv).await {
        return toast_error(e);
    }
    Ok(ok(json!({"message": "Proxy configuration saved."})).into_response())
}

/// `Proxy::resetCustomErrorPage`.
async fn reset_error_page(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    if !ctx.is_team_admin() {
        return toast_error(UNAUTHORIZED);
    }
    servers::update_if_changed(
        &state.db,
        servers::Row::Server(s.id),
        &[("proxy_error_page", C::Text(None))],
        state.now(),
    )
    .await?;
    let srv = srv(&state, reload(&state, s.id).await?)?;
    if let Err(e) = proxy_ops::setup_default_redirect(&srv).await {
        return toast_error(e);
    }
    Ok(ok(json!({"message": "Default error page restored."})).into_response())
}

/// `Proxy::submit`.
async fn submit(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    headers: axum::http::HeaderMap,
    Json(body): Json<Map<String, Value>>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    if !ctx.is_team_admin() {
        return toast_error(UNAUTHORIZED);
    }
    let input = Input(body);
    rules(&input)?;
    if let Some(u) = input.str("redirect_url").filter(|u| !u.is_empty())
        && let Some(e) = crate::safe_url::check(&state, "redirect_url", u, &headers).await?
    {
        return Err(crate::routes::server_settings::validation_errors(&[(
            "redirect_url",
            e.as_str(),
        )]));
    }
    let mut srv = srv(&state, s.clone())?;
    if let Some(configuration) = input.str("proxy_settings")
        && let Err(e) = proxy_config::save_proxy_configuration(&mut srv, configuration).await
    {
        return toast_error(match e {
            proxy_config::SaveError::Invalid(i) => i.0,
            proxy_config::SaveError::Other(m) => m,
        });
    }
    // `$this->server->proxy->redirect_url = $this->redirectUrl`: a cleared field stays an empty string.
    srv.proxy_set(
        "redirect_url",
        input.get("redirect_url").cloned().unwrap_or(Value::Null),
    );
    if let Err(e) = srv.save_proxy().await {
        return toast_error(e);
    }
    servers::update_if_changed(
        &state.db,
        servers::Row::Server(s.id),
        &[(
            "proxy_error_page",
            C::Text(
                input
                    .str("custom_error_page")
                    .filter(|p| !p.trim().is_empty())
                    .map(str::to_string),
            ),
        )],
        state.now(),
    )
    .await?;
    let fresh = reload(&state, s.id).await?;
    save_routing(&state, &fresh, &input, &ctx).await?;
    tracing::info!(target: "audit", event = "ui.server.proxy.configuration_saved", team_id = ctx.team_id, server_uuid = %s.uuid, "proxy configuration saved");
    let srv = self::srv(&state, reload(&state, s.id).await?)?;
    if let Err(e) = proxy_ops::setup_default_redirect(&srv).await {
        return toast_error(e);
    }
    Ok(ok(json!({"message": "Proxy configuration saved."})).into_response())
}

/// `Proxy::resetProxyConfiguration`.
async fn reset_configuration(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    if !ctx.is_team_admin() {
        return toast_error(UNAUTHORIZED);
    }
    let mut srv = srv(&state, s.clone())?;
    let configuration = match proxy_config::get_proxy_configuration(&mut srv, true).await {
        Ok(c) => c,
        Err(e) => return toast_error(e),
    };
    if let Err(e) = proxy_config::save_proxy_configuration(&mut srv, &configuration).await {
        return toast_error(match e {
            proxy_config::SaveError::Invalid(i) => i.0,
            proxy_config::SaveError::Other(m) => m,
        });
    }
    tracing::info!(target: "audit", event = "ui.server.proxy.configuration_reset", team_id = ctx.team_id, server_uuid = %s.uuid, "proxy configuration reset");
    Ok(ok(
        json!({"message": "Proxy configuration reset to default.", "configuration": configuration}),
    )
    .into_response())
}

/// `Navbar::restart`, `checkProxy` + `startProxy`, `stop` and `checkProxyStatus`.
async fn action(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((uuid, action)): Path<(String, String)>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    if !ctx.is_team_admin() {
        return toast_error(UNAUTHORIZED);
    }
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    let mut srv = Srv::from_row(jobs, s.clone());
    let activity_after = coolify_db::server_ops::max_activity_id(&state.db).await?;
    match action.as_str() {
        "restart" => {
            jobs.spawn_restart_proxy(s.id);
            tracing::info!(target: "audit", event = "ui.proxy.restarted", team_id = ctx.team_id, server_uuid = %s.uuid, "proxy restart queued");
        }
        "start" => {
            if let Err(e) = proxy_ops::check_proxy(&mut srv, true).await {
                return toast_error(e);
            }
            if let Err(e) = proxy_ops::start_proxy(&mut srv, true, true, false).await {
                return toast_error(e);
            }
            tracing::info!(target: "audit", event = "ui.proxy.started", team_id = ctx.team_id, server_uuid = %s.uuid, "proxy started");
        }
        "stop" => {
            if let Err(e) = proxy_ops::stop_proxy(&mut srv, true, 30, false).await {
                return toast_error(e);
            }
            tracing::info!(target: "audit", event = "ui.proxy.stopped", team_id = ctx.team_id, server_uuid = %s.uuid, "proxy stopped");
        }
        "status" => {
            if let Err(e) = proxy_ops::check_proxy(&mut srv, true).await {
                return toast_error(e);
            }
            let status = proxy_ops::container_status(&srv, "coolify-proxy").await;
            srv.proxy_set("status", json!(status));
            if let Err(e) = srv.save_proxy().await {
                return toast_error(e);
            }
        }
        _ => return toast_error("Unknown proxy action."),
    }
    Ok(ok(json!({"activity_after": activity_after})).into_response())
}

/// What `livewire:activity-monitor` shows in the "Proxy Startup Logs" dialog.
async fn activity(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    let after = query.get("after").and_then(|a| a.parse().ok()).unwrap_or(0);
    let activity = coolify_db::server_ops::latest_proxy_activity(&state.db, &s.uuid, after)
        .await?
        .map(|(id, props, description)| {
            let entries: Value = serde_json::from_str(&description).unwrap_or(Value::Array(vec![]));
            json!({"id": id, "status": props.get("status").cloned().unwrap_or(Value::Null), "entries": entries})
        });
    Ok(ok(json!({"activity": activity, "proxy_status": proxy_str(&s, "status")})).into_response())
}

// ── Dynamic configurations ──────────────────────────────────────────────

const MAX_FILE: usize = 1024 * 1024;
const MAX_TOTAL: usize = 5 * 1024 * 1024;
const MAX_FILES: usize = 100;
const MANAGED: [&str; 5] = [
    "coolify.yaml",
    "Caddyfile",
    "coolify.caddy",
    "default_redirect_503.yaml",
    "default_redirect_503.caddy",
];

/// `DynamicConfigurations::loadDynamicConfigurations`.
async fn dynamic(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    let path = Srv::from_row(jobs, s.clone()).proxy_path();
    let limit = MAX_FILES + 1;
    let listed = match jobs
        .instant_remote(
            &s,
            &[format!(
                "mkdir -p {path}/dynamic && ls -1 {path}/dynamic | head -n {limit}"
            )],
            true,
            "server.proxy.dynamic",
        )
        .await
    {
        Ok(o) => o.unwrap_or_default(),
        Err(e) => return toast_error(e),
    };
    let mut files: Vec<String> = listed
        .lines()
        .map(|f| f.trim().to_string())
        .filter(|f| !f.is_empty())
        .collect();
    files.sort();
    let mut skipped = Vec::new();
    if files.len() > MAX_FILES {
        skipped.push("additional files".to_string());
    }
    files.truncate(MAX_FILES);
    let mut items = Vec::new();
    if !ctx.is_team_admin() {
        for f in &files {
            items.push(
                json!({"name": f, "content": null, "managed": MANAGED.contains(&f.as_str())}),
            );
        }
        return Ok(ok(json!({"files": items})).into_response());
    }
    let mut total = 0usize;
    for f in files {
        let file_path = shell_arg(&format!("{path}/dynamic/{f}"));
        let content = match jobs
            .instant_remote(
                &s,
                &[format!("head -c {} {file_path}", MAX_FILE + 1)],
                true,
                "server.proxy.dynamic",
            )
            .await
        {
            Ok(o) => o.unwrap_or_default(),
            Err(e) => return toast_error(e),
        };
        if content.len() > MAX_FILE || total + content.len() > MAX_TOTAL {
            skipped.push(f);
            continue;
        }
        total += content.len();
        items
            .push(json!({"name": f, "content": content, "managed": MANAGED.contains(&f.as_str())}));
    }
    let warning = (!skipped.is_empty()).then(|| {
        format!(
            "Some dynamic configurations were not loaded because they exceed the safe display limits: {}",
            skipped.join(", ")
        )
    });
    Ok(ok(json!({"files": items, "warning": warning})).into_response())
}

/// `ValidProxyConfigFilename`.
fn filename_rule(v: &Value) -> Option<String> {
    let t = v.as_str()?.trim();
    if t.is_empty() {
        return None;
    }
    if t.len() > 255 {
        return Some("The file name must not exceed 255 characters.".into());
    }
    if t.contains('/') || t.contains('\\') {
        return Some("The file name cannot contain path separators.".into());
    }
    if t.starts_with('.') {
        return Some("The file name cannot start with a dot (hidden files not allowed).".into());
    }
    if !t
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
    {
        return Some(
            "The file name may only contain letters, numbers, dashes, underscores, and dots."
                .into(),
        );
    }
    if ["coolify.yaml", "coolify.yml", "Caddyfile"].contains(&t) {
        return Some("The file name uses a reserved filename.".into());
    }
    None
}

async fn reload_caddy(state: &AppState, s: &ServerRow) {
    if proxy_str(s, "type") == Some("CADDY")
        && let Some(jobs) = &state.jobs
    {
        let _ = jobs
            .instant_remote(
                s,
                &["docker exec coolify-proxy caddy reload --config /config/caddy/Caddyfile.autosave"
                    .to_string()],
                false,
                "server.proxy.dynamic",
            )
            .await;
    }
}

/// `NewDynamicConfiguration::addDynamicConfiguration`.
async fn save_dynamic(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    Json(body): Json<Map<String, Value>>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    if !ctx.is_team_admin() {
        return toast_error(UNAUTHORIZED);
    }
    let input = Input(body);
    validate(
        &input,
        &[
            Field {
                name: "file_name",
                rules: vec![Rule::Required, Rule::CustomOwned(filename_rule)],
            },
            Field {
                name: "value",
                rules: vec![Rule::Required],
            },
        ],
        &[],
        &["file_name", "value", "new_file"],
    )?;
    let mut name = input.str("file_name").unwrap_or_default().to_string();
    let mut value = input.str("value").unwrap_or_default().to_string();
    let proxy_type = proxy_str(&s, "type").unwrap_or_default();
    if proxy_type == "TRAEFIK" {
        if !name.ends_with(".yaml") && !name.ends_with(".yml") {
            name.push_str(".yaml");
        }
        if name == "coolify.yaml" {
            return toast_error("File name is reserved.");
        }
    } else if proxy_type == "CADDY" && !name.ends_with(".caddy") {
        name.push_str(".caddy");
    }
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    let path = Srv::from_row(jobs, s.clone()).proxy_path();
    let file = shell_arg(&format!("{path}/dynamic/{name}"));
    if input.get("new_file").is_some_and(php_truthy) {
        let exists = jobs
            .instant_remote(
                &s,
                &[format!("test -f {file} && echo 1 || echo 0")],
                true,
                "server.proxy.dynamic",
            )
            .await
            .ok()
            .flatten()
            .unwrap_or_default();
        if exists.trim() == "1" {
            return toast_error("File already exists");
        }
    }
    if proxy_type == "TRAEFIK" {
        // `Yaml::dump(Yaml::parse($value), 10, 2)`.
        match coolify_compat::yaml_parse::parse(&value, 128) {
            Ok(parsed) => value = coolify_compat::yaml::dump_with_indent(&parsed, 10, 2),
            Err(e) => return toast_error(e.to_string()),
        }
    }
    let encoded = base64::engine::general_purpose::STANDARD.encode(&value);
    if let Err(e) = jobs
        .instant_remote(
            &s,
            &[format!(
                "echo '{encoded}' | base64 -d | tee {file} > /dev/null"
            )],
            true,
            "server.proxy.dynamic",
        )
        .await
    {
        return toast_error(e);
    }
    reload_caddy(&state, &s).await;
    Ok(ok(json!({"message": "Dynamic configuration saved."})).into_response())
}

/// `DynamicConfigurationNavbar::delete`.
async fn delete_dynamic(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    Json(body): Json<Map<String, Value>>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    if !ctx.is_team_admin() {
        return toast_error(UNAUTHORIZED);
    }
    let name = body
        .get("file_name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if name.is_empty() || name.contains('/') || name.contains('\\') || name.starts_with('.') {
        return toast_error("Invalid proxy configuration filename.");
    }
    if proxy_str(&s, "type") == Some("CADDY") && name == "Caddyfile" {
        return toast_error("Cannot delete Caddyfile.");
    }
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    let path = Srv::from_row(jobs, s.clone()).proxy_path();
    if let Err(e) = jobs
        .instant_remote(
            &s,
            &[format!(
                "rm -f {}",
                shell_arg(&format!("{path}/dynamic/{name}"))
            )],
            true,
            "server.proxy.dynamic",
        )
        .await
    {
        return toast_error(e);
    }
    reload_caddy(&state, &s).await;
    Ok(ok(json!({"message": "File deleted."})).into_response())
}

// ── TLS certificates ────────────────────────────────────────────────────

/// `GetTraefikCertificates` and `TraefikAcmeService::certificates`.
async fn certificates(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    let srv = Srv::from_row(jobs, s);
    let list = match acme::list_certificates(&srv).await {
        Ok(list) => list,
        Err(e) => return toast_error(e),
    };
    // `loadTraefikAcmeBackups`: only for who can manage the proxy.
    let backups = if ctx.is_team_admin() {
        match acme::list_backups(&srv).await {
            Ok(b) => b,
            Err(e) => return toast_error(e),
        }
    } else {
        Vec::new()
    };
    Ok(
        ok(json!({"certificates": list, "backups": backups, "keep": acme::KEEP_BACKUPS}))
            .into_response(),
    )
}

/// `Proxy\Certificates::deleteTraefikCertificate`.
async fn delete_certificate(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    Json(body): Json<Map<String, Value>>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    if !ctx.is_team_admin() {
        return toast_error("This action is unauthorized.");
    }
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    let mut srv = Srv::from_row(jobs, s);
    let id = body.get("id").and_then(Value::as_str).unwrap_or_default();
    match acme::delete_certificate(&mut srv, id).await {
        Ok(_) => Ok(ok(json!({"message": "TLS certificate deleted. Restart Traefik to remove it from the running proxy."})).into_response()),
        Err(e) => toast_error(e),
    }
}

/// `Proxy\Certificates::restoreTraefikAcmeBackup`.
async fn restore_backup(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    Json(body): Json<Map<String, Value>>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    if !ctx.is_team_admin() {
        return toast_error("This action is unauthorized.");
    }
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    let id = s.id;
    let mut srv = Srv::from_row(jobs, s);
    let name = body.get("name").and_then(Value::as_str).unwrap_or_default();
    if let Err(e) = acme::restore_backup(&mut srv, name).await {
        return toast_error(e);
    }
    if body.get("restart").is_some_and(php_truthy) {
        jobs.spawn_restart_proxy(id);
        return Ok(ok(json!({"message": "acme.json restored. The proxy is restarting to load the restored certificates."})).into_response());
    }
    Ok(ok(json!({"message": "acme.json restored. Restart the proxy to load the restored certificates."})).into_response())
}

/// `Proxy\Certificates::deleteTraefikAcmeBackup`.
async fn delete_backup(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    Json(body): Json<Map<String, Value>>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    if !ctx.is_team_admin() {
        return toast_error("This action is unauthorized.");
    }
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    let srv = Srv::from_row(jobs, s);
    let name = body.get("name").and_then(Value::as_str).unwrap_or_default();
    match acme::delete_backup(&srv, name).await {
        Ok(()) => Ok(ok(json!({"message": "acme.json backup deleted."})).into_response()),
        Err(e) => toast_error(e),
    }
}

// ── Logs ────────────────────────────────────────────────────────────────

/// `GetLogs::MAX_DISPLAY_SIZE_BYTES`.
pub(crate) const MAX_DISPLAY_SIZE_BYTES: usize = 5 * 1024 * 1024;
/// `GetLogs::MAX_DOWNLOAD_SIZE_BYTES`.
pub(crate) const MAX_DOWNLOAD_SIZE_BYTES: usize = 50 * 1024 * 1024;

/// `GetLogs::DOCKER_TIMESTAMP_PATTERN`.
pub(crate) fn is_docker_timestamp(since: &str) -> bool {
    let b = since.as_bytes();
    let digits =
        |r: std::ops::Range<usize>| b.get(r).is_some_and(|d| d.iter().all(u8::is_ascii_digit));
    if b.len() < 20
        || !digits(0..4)
        || b[4] != b'-'
        || !digits(5..7)
        || b[7] != b'-'
        || !digits(8..10)
        || b[10] != b'T'
        || !digits(11..13)
        || b[13] != b':'
        || !digits(14..16)
        || b[16] != b':'
        || !digits(17..19)
        || b[b.len() - 1] != b'Z'
    {
        return false;
    }
    let fraction = &since[19..since.len() - 1];
    fraction.is_empty()
        || (fraction.starts_with('.')
            && (2..=10).contains(&fraction.len())
            && fraction[1..].bytes().all(|d| d.is_ascii_digit()))
}

/// `GetLogs::resolveLogTail`.
pub(crate) fn log_tail(lines: Option<i64>) -> String {
    match lines {
        Some(-1) => "all".to_string(),
        None | Some(i64::MIN..=0) => "1000".to_string(),
        Some(n) => n.min(50000).to_string(),
    }
}

/// `removeAnsiColors`.
pub(crate) fn remove_ansi_colors(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            let mut body = String::new();
            let mut terminated = false;
            for n in chars.by_ref() {
                if n == 'm' {
                    terminated = true;
                    break;
                }
                if n.is_ascii_digit() || n == ';' {
                    body.push(n);
                } else {
                    out.push('\u{1b}');
                    out.push('[');
                    out.push_str(&body);
                    out.push(n);
                    terminated = true;
                    body.clear();
                    break;
                }
            }
            if !terminated {
                out.push('\u{1b}');
                out.push('[');
                out.push_str(&body);
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// `GetLogs::getLogs`, `copyLogs` and `downloadAllLogs` for the proxy and Sentinel containers.
async fn logs(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((uuid, container)): Path<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    if !["coolify-proxy", "coolify-sentinel"].contains(&container.as_str()) {
        return toast_error("Unknown container.");
    }
    let mode = query.get("mode").map_or("get", String::as_str);
    let lines = query.get("lines").and_then(|l| l.parse::<i64>().ok());
    let timestamps = query.get("timestamps").is_none_or(|t| t == "1");
    let tail = log_tail(lines);
    let (options, max, limit, timestamped) = match mode {
        "copy" => (
            format!("-n {tail}{}", if timestamps { " -t" } else { "" }),
            MAX_DISPLAY_SIZE_BYTES,
            "5MB",
            timestamps,
        ),
        "all" => (
            if timestamps {
                "-t".to_string()
            } else {
                String::new()
            },
            MAX_DOWNLOAD_SIZE_BYTES,
            "50MB",
            timestamps,
        ),
        _ => match query.get("since").filter(|v| is_docker_timestamp(v)) {
            Some(since) => (
                format!("--since {since} -t"),
                MAX_DISPLAY_SIZE_BYTES,
                "5MB",
                true,
            ),
            None => (format!("-n {tail} -t"), MAX_DISPLAY_SIZE_BYTES, "5MB", true),
        },
    };
    let options = if options.is_empty() {
        String::new()
    } else {
        format!("{options} ")
    };
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    // `boundedLogCommand`: one byte over the limit means the output was cut.
    let out = jobs
        .instant_remote(
            &s,
            &[format!(
                "(docker logs {options}{container}) 2>&1 | head -c {}",
                max + 1
            )],
            false,
            "server.logs",
        )
        .await
        .ok()
        .flatten()
        .unwrap_or_default();
    let truncated = out.len() > max;
    let cut = if truncated {
        let mut end = max;
        while !out.is_char_boundary(end) {
            end -= 1;
        }
        &out[..end]
    } else {
        out.as_str()
    };
    let cleaned = remove_ansi_colors(cut);
    let cleaned = cleaned.trim_end_matches('\n');
    // `runLogCommand`: Docker writes stdout and stderr separately, so sort timestamped lines back into order.
    let mut text = if timestamped {
        let mut rows: Vec<&str> = cleaned.split('\n').collect();
        rows.sort_by_key(|l| l.split(' ').next().unwrap_or_default());
        rows.join("\n")
    } else {
        cleaned.to_string()
    };
    if truncated {
        text.push_str(&format!("\n\n[... Output truncated at {limit} limit ...]"));
    }
    if mode != "get" {
        // `sanitizeLogsForExport`: a server-level container has no resource secrets to add.
        text = coolify_compat::redact::redact_text(&text, &[], &[]);
    }
    Ok(ok(json!({"logs": text})).into_response())
}
