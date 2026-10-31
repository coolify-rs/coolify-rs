//! Session endpoints behind `Server\Sentinel`, `Server\DockerCleanup`, `Server\DockerImages` and
//! the settings part of `Server\TrafficAnalyticsSettings`.

use std::collections::HashMap;
use std::net::SocketAddr;

use axum::Extension;
use axum::Json;
use axum::Router;
use axum::extract::{ConnectInfo, Path, Query, State};
use axum::http::HeaderMap;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use coolify_db::servers::ServerRow;
use coolify_jobs::server_ops::{checks, sentinel, shell_arg};
use regex::Regex;
use serde_json::{Map, Value, json};
use std::sync::LazyLock;

use crate::auth::AuthCtx;
use crate::input::Input;
use crate::response::{ApiError, ApiResult, ok};
use crate::routes::server_settings::{Setting, find_server, save_settings, sentinel_settings_hook};
use crate::serialize::php_truthy;
use crate::state::AppState;
use crate::ui_server_destinations::toast_error;
use crate::ui_server_general::SERVER_PAGE;
use crate::validation::{Field, Rule, validate};

const UNAUTHORIZED: &str = "This action is unauthorized.";

/// `Sentinel::$sentinelToken`.
static SENTINEL_TOKEN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\A[a-zA-Z0-9._\-+=/]+\z").unwrap_or_else(|_| unreachable!()));

pub fn routes() -> Router<AppState> {
    let p = |s: &str| format!("{SERVER_PAGE}/{s}");
    Router::new()
        .route(&p("sentinel"), get(sentinel_show).post(sentinel_submit))
        .route(&p("log-drains"), get(log_drains))
        .route(&p("sentinel/restart"), post(sentinel_restart))
        .route(&p("sentinel/regenerate"), post(sentinel_regenerate))
        .route(&p("sentinel/restore"), post(sentinel_restore))
        .route(&p("docker-cleanup"), get(cleanup_show).post(cleanup_submit))
        .route(&p("docker-cleanup/run"), post(cleanup_run))
        .route(&p("images"), get(images))
        .route(&p("images/delete"), post(delete_image))
        .route(&p("analytics"), get(analytics).post(analytics_save))
        .route(&p("analytics/toggle"), post(analytics_toggle))
}

fn setting(s: &ServerRow, k: &str) -> Value {
    s.settings
        .as_ref()
        .and_then(|r| r.get(k))
        .cloned()
        .unwrap_or(Value::Null)
}

fn decrypted(state: &AppState, s: &ServerRow, k: &str) -> Value {
    setting(s, k)
        .as_str()
        .and_then(|c| state.encrypter.as_ref()?.decrypt_string(c).ok())
        .map_or(Value::Null, Value::String)
}

fn int_of(input: &Input, k: &str) -> i64 {
    match input.get(k) {
        Some(Value::Number(n)) => n.as_i64().unwrap_or_default(),
        Some(Value::String(s)) => s.trim().parse().unwrap_or_default(),
        _ => 0,
    }
}

// ── Sentinel ────────────────────────────────────────────────────────────

/// `$server->isSentinelEnabled()`.
fn is_sentinel_enabled(s: &ServerRow) -> bool {
    let info = coolify_jobs::server::ServerInfo::from_row(s);
    !info.is_build_server()
        && !info.is_swarm()
        && !setting(s, "force_disabled").as_bool().unwrap_or(false)
        && !crate::routes::servers::is_transferred(s)
}

/// `Sentinel::syncData` and `refreshSentinelStatus`.
/// `LogDrains::mount` for the page. The fields of a secret stay empty for who cannot edit them,
/// where upstream's Livewire snapshot carries them to every team member.
async fn log_drains(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let mut c = ctx.clone();
    c.can_read_sensitive = ctx.is_team_admin();
    let s = find_server(&state, &c, &uuid).await?;
    let mut drains = crate::routes::server_settings::drain_transform(&state, &c, &s);
    // The form binds every field.
    for k in [
        "logdrain_newrelic_license_key",
        "logdrain_axiom_api_key",
        "logdrain_custom_config",
        "logdrain_custom_config_parser",
    ] {
        if drains.get(k).is_none() {
            drains[k] = json!("");
        }
    }
    Ok(ok(drains).into_response())
}

async fn sentinel_show(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    Ok(ok(json!({
        "enabled": is_sentinel_enabled(&s),
        "status": checks::sentinel_status(&s),
        "push_problem": sentinel::push_problem(s.id),
        "url": setting(&s, "sentinel_custom_url"),
        "token": if ctx.is_team_admin() { decrypted(&state, &s, "sentinel_token") } else { Value::Null },
        "is_localhost": crate::routes::server_settings::is_localhost(&s),
    }))
    .into_response())
}

/// `Sentinel::submit`.
async fn sentinel_submit(
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
                name: "sentinel_token",
                rules: vec![
                    Rule::Required,
                    Rule::String,
                    Rule::Max(500),
                    Rule::Regex(&SENTINEL_TOKEN),
                ],
            },
            Field {
                name: "sentinel_custom_url",
                rules: vec![Rule::Nullable, Rule::Url],
            },
        ],
        &[],
        &["sentinel_token", "sentinel_custom_url"],
    )?;
    let changed = save_settings(
        &state,
        &s,
        &[
            (
                "sentinel_token",
                Setting::Encrypted(input.str("sentinel_token").map(str::to_string)),
            ),
            (
                "sentinel_custom_url",
                Setting::Text(input.str("sentinel_custom_url").map(str::to_string)),
            ),
        ],
    )
    .await?;
    sentinel_settings_hook(&state, &s, &changed);
    tracing::info!(target: "audit", event = "ui.server.sentinel.updated", team_id = ctx.team_id, server_uuid = %uuid, "sentinel updated");
    Ok(ok(json!({"message": "Sentinel settings updated. Restarting Sentinel."})).into_response())
}

/// `Sentinel::restartSentinel`.
async fn sentinel_restart(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    if !ctx.is_team_admin() {
        return toast_error(UNAUTHORIZED);
    }
    state
        .jobs
        .as_ref()
        .ok_or(ApiError::NotPorted)?
        .spawn_restart_sentinel(s.id);
    tracing::info!(target: "audit", event = "ui.server.sentinel.restarted", team_id = ctx.team_id, server_uuid = %uuid, "sentinel restarted");
    Ok(ok(json!({})).into_response())
}

/// `Sentinel::regenerateSentinelToken`.
async fn sentinel_regenerate(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    if !ctx.is_team_admin() {
        return toast_error(UNAUTHORIZED);
    }
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let token = enc.encrypt_serialized(&json!({"server_uuid": s.uuid}).to_string());
    let changed = save_settings(
        &state,
        &s,
        &[("sentinel_token", Setting::Encrypted(Some(token)))],
    )
    .await?;
    sentinel_settings_hook(&state, &s, &changed);
    tracing::info!(target: "audit", event = "ui.server.sentinel.token_regenerated", team_id = ctx.team_id, server_uuid = %uuid, "sentinel token regenerated");
    Ok(ok(json!({})).into_response())
}

/// `Sentinel::restoreDefaultConfiguration`.
async fn sentinel_restore(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    if !ctx.is_team_admin() {
        return toast_error(UNAUTHORIZED);
    }
    let ip = s
        .record
        .get("ip")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let url = crate::routes::servers::sentinel_url(&state, &ip, peer, &headers).await?;
    // `saveQuietly`: the settings observer does not run.
    save_settings(
        &state,
        &s,
        &[
            ("sentinel_custom_url", Setting::Text(url)),
            ("sentinel_metrics_refresh_rate_seconds", Setting::Int(10)),
            ("sentinel_metrics_history_days", Setting::Int(7)),
            ("sentinel_push_interval_seconds", Setting::Int(60)),
            ("is_sentinel_debug_enabled", Setting::Bool(false)),
        ],
    )
    .await?;
    state
        .jobs
        .as_ref()
        .ok_or(ApiError::NotPorted)?
        .spawn_restart_sentinel(s.id);
    tracing::info!(target: "audit", event = "ui.server.sentinel.defaults_restored", team_id = ctx.team_id, server_uuid = %uuid, "sentinel defaults restored");
    Ok(ok(json!({})).into_response())
}

// ── Docker cleanup ──────────────────────────────────────────────────────

/// `DockerCleanup::syncData` and `DockerCleanupExecutions::refreshExecutions`.
async fn cleanup_show(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    let executions = coolify_db::server_ops::cleanup_executions(&state.db, s.id).await?;
    // The executions poll needs only the list.
    let stale = !query.contains_key("poll") && {
        let last = executions
            .first()
            .and_then(|e| e.get("created_at"))
            .and_then(Value::as_str);
        let frequency = setting(&s, "docker_cleanup_frequency");
        cleanup_is_stale(last, frequency.as_str().unwrap_or("0 0 * * *"))
    };
    Ok(ok(json!({
        "is_cleanup_stale": stale,
        "is_scheduler_healthy": coolify_jobs::scheduler::heartbeat_alive(chrono::Utc::now()),
        "docker_cleanup_frequency": setting(&s, "docker_cleanup_frequency"),
        "docker_cleanup_threshold": setting(&s, "docker_cleanup_threshold"),
        "force_docker_cleanup": setting(&s, "force_docker_cleanup"),
        "delete_unused_volumes": setting(&s, "delete_unused_volumes"),
        "delete_unused_networks": setting(&s, "delete_unused_networks"),
        "disable_application_image_retention": setting(&s, "disable_application_image_retention"),
        "server_timezone": setting(&s, "server_timezone"),
        "executions": executions,
    }))
    .into_response())
}

/// `DockerCleanup::isCleanupStale`: the last execution is older than two intervals of the schedule (at least 10 minutes).
fn cleanup_is_stale(last: Option<&str>, frequency: &str) -> bool {
    let Some(last) = last.and_then(|l| {
        chrono::DateTime::parse_from_rfc3339(l)
            .map(|d| d.with_timezone(&chrono::Utc))
            .ok()
            .or_else(|| {
                chrono::NaiveDateTime::parse_from_str(&l[..l.len().min(19)], "%Y-%m-%dT%H:%M:%S")
                    .or_else(|_| {
                        chrono::NaiveDateTime::parse_from_str(
                            &l[..l.len().min(19)],
                            "%Y-%m-%d %H:%M:%S",
                        )
                    })
                    .ok()
                    .map(|d| d.and_utc())
            })
    }) else {
        return false;
    };
    let now = chrono::Utc::now();
    let age = (now - last).num_minutes();
    if age <= 10 {
        return false;
    }
    // `getNextRunDate` twice.
    let Ok(cron) = coolify_jobs::scheduler::parse_frequency(frequency) else {
        return false;
    };
    let Some(zone) = coolify_compat::cron_expression::time_zone("UTC") else {
        return false;
    };
    let local = coolify_compat::cron_expression::in_zone(now, zone);
    let Ok(first) = cron.next_run_date(&local, false) else {
        return false;
    };
    let Ok(second) = cron.next_run_date(&first, false) else {
        return false;
    };
    age > ((second - first).num_minutes() * 2).max(10)
}

/// `DockerCleanup::submit` and `instantSave`.
async fn cleanup_submit(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    Json(body): Json<Map<String, Value>>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    let input = Input(body);
    let frequency = input
        .str("docker_cleanup_frequency")
        .unwrap_or_default()
        .to_string();
    if !input.has("instant") && !coolify_compat::cron::validate_cron_expression(&frequency) {
        return toast_error("Invalid Cron / Human expression for Docker Cleanup Frequency.");
    }
    if !ctx.is_team_admin() {
        return toast_error(UNAUTHORIZED);
    }
    validate(
        &input,
        &[
            Field {
                name: "docker_cleanup_frequency",
                rules: vec![Rule::String, Rule::Required],
            },
            Field {
                name: "docker_cleanup_threshold",
                rules: vec![Rule::Integer, Rule::MinNum(1), Rule::MaxNum(99)],
            },
        ],
        &[],
        &[
            "instant",
            "docker_cleanup_frequency",
            "docker_cleanup_threshold",
            "force_docker_cleanup",
            "delete_unused_volumes",
            "delete_unused_networks",
            "disable_application_image_retention",
        ],
    )?;
    let flag = |k: &str| input.get(k).is_some_and(php_truthy);
    let changed = save_settings(
        &state,
        &s,
        &[
            (
                "force_docker_cleanup",
                Setting::Bool(flag("force_docker_cleanup")),
            ),
            ("docker_cleanup_frequency", Setting::Text(Some(frequency))),
            (
                "docker_cleanup_threshold",
                Setting::Int(int_of(&input, "docker_cleanup_threshold")),
            ),
            (
                "delete_unused_volumes",
                Setting::Bool(flag("delete_unused_volumes")),
            ),
            (
                "delete_unused_networks",
                Setting::Bool(flag("delete_unused_networks")),
            ),
            (
                "disable_application_image_retention",
                Setting::Bool(flag("disable_application_image_retention")),
            ),
        ],
    )
    .await?;
    if !changed.is_empty() {
        tracing::info!(target: "audit", event = "ui.server.docker_cleanup.updated", team_id = ctx.team_id, server_uuid = %uuid, "docker cleanup updated");
    }
    Ok(ok(json!({"message": "Server updated."})).into_response())
}

/// `DockerCleanup::manualCleanup`.
async fn cleanup_run(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    if !ctx.is_team_admin() {
        return toast_error(UNAUTHORIZED);
    }
    let flag = |k: &str| setting(&s, k).as_bool().unwrap_or(false);
    state
        .jobs
        .as_ref()
        .ok_or(ApiError::NotPorted)?
        .spawn_docker_cleanup(
            s.id,
            true,
            flag("delete_unused_volumes"),
            flag("delete_unused_networks"),
        );
    tracing::info!(target: "audit", event = "ui.server.docker_cleanup_started", team_id = ctx.team_id, server_uuid = %uuid, "docker cleanup started");
    Ok(ok(json!({"message": "Manual cleanup job started. Depending on the amount of data, this might take a while."})).into_response())
}

// ── Images ──────────────────────────────────────────────────────────────

/// `DockerImages::load`.
async fn images(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    if !coolify_jobs::server::ServerInfo::from_row(&s).is_functional() {
        return Ok(ok(json!({"images": [], "usage": null})).into_response());
    }
    let run = async |command: &str| {
        jobs.instant_remote(&s, &[command.to_string()], false, "server.images")
            .await
            .ok()
            .flatten()
            .unwrap_or_default()
    };
    let inspect = run("docker ps -a --no-trunc --format '{{.ID}}' | xargs -r docker container inspect --format '{{.Image}}#{{.Name}}' 2>/dev/null || true").await;
    let mut used: std::collections::HashMap<String, Vec<String>> = Default::default();
    for line in inspect.lines() {
        if let Some((image, name)) = line.trim().split_once('#')
            && !image.is_empty()
        {
            used.entry(image.to_string())
                .or_default()
                .push(name.trim_start_matches('/').to_string());
        }
    }
    used.values_mut().for_each(|v| v.sort());
    let listed = run("docker image ls --no-trunc --format '{{json .}}'").await;
    let mut items = Vec::new();
    for line in listed.lines() {
        let Ok(image) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        let text = |k: &str| {
            image
                .get(k)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        let (repository, tag, id) = (text("Repository"), text("Tag"), text("ID"));
        let dangling = repository == "<none>" || tag == "<none>";
        let short: String = id
            .split_once("sha256:")
            .map_or(id.as_str(), |(_, a)| a)
            .chars()
            .take(12)
            .collect();
        items.push(json!({
            "id": id,
            "short_id": short,
            "repository": repository,
            "tag": tag,
            "dangling": dangling,
            "reference": if dangling { id.clone() } else { format!("{repository}:{tag}") },
            "size": image.get("Size"),
            "created": image.get("CreatedSince"),
            "containers": used.get(&id).cloned().unwrap_or_default(),
        }));
    }
    let df = run("docker system df --format '{{json .}}'").await;
    let usage = df
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l.trim()).ok())
        .find(|v| v.get("Type").and_then(Value::as_str) == Some("Images"))
        .map(|row| json!({"size": row.get("Size"), "reclaimable": row.get("Reclaimable")}));
    Ok(ok(json!({"images": items, "usage": usage})).into_response())
}

/// `DockerImages::delete`.
async fn delete_image(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    Json(body): Json<Map<String, Value>>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    if !ctx.is_team_admin() {
        return toast_error(UNAUTHORIZED);
    }
    let reference = body
        .get("reference")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if reference.is_empty() {
        return toast_error("Unknown image, refresh the page and try again.");
    }
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    if let Err(e) = jobs
        .instant_remote(
            &s,
            &[format!("docker image rm {}", shell_arg(reference))],
            true,
            "server.images",
        )
        .await
    {
        return toast_error(e);
    }
    tracing::info!(target: "audit", event = "ui.server.image_deleted", team_id = ctx.team_id, server_uuid = %uuid, "image deleted");
    Ok(ok(json!({"message": format!("Deleted {reference}.")})).into_response())
}

// ── Traffic analytics settings ──────────────────────────────────────────

/// `TrafficAnalyticsSettings::syncData` with `trafficAnalyticsUnsupportedReason`.
async fn analytics(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    let info = coolify_jobs::server::ServerInfo::from_row(&s);
    let proxy = s
        .record
        .get("proxy")
        .and_then(|p| p.get("type"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let reason = if info.is_swarm() || info.is_build_server() {
        Some("Traffic analytics is not supported on Swarm/Build servers.")
    } else if !["TRAEFIK", "CADDY"].contains(&proxy) {
        Some("Traffic analytics needs the Traefik or Caddy proxy.")
    } else {
        None
    };
    Ok(ok(json!({
        "enabled": setting(&s, "is_traffic_analytics_enabled").as_bool().unwrap_or(false),
        "unsupported_reason": reason,
        "caddy_note": (proxy == "CADDY").then_some("Caddy logs a resource only after you redeploy it."),
        "traffic_topn": setting(&s, "traffic_topn"),
        "traffic_sample_threshold": setting(&s, "traffic_sample_threshold"),
        "traffic_retention_1h_days": setting(&s, "traffic_retention_1h_days"),
        "traffic_retention_1d_days": setting(&s, "traffic_retention_1d_days"),
        "is_geoip_enabled": setting(&s, "is_geoip_enabled").as_bool().unwrap_or(false),
        "geoip_refresh_days": setting(&s, "geoip_refresh_days"),
        "geoip_maxmind_license_key": if ctx.is_team_admin() { decrypted(&state, &s, "geoip_maxmind_license_key") } else { Value::Null },
        "traffic_ip_mode": setting(&s, "traffic_ip_mode").as_str().unwrap_or("full"),
    }))
    .into_response())
}

/// `TrafficAnalyticsSettings::toggleTrafficAnalytics`.
async fn analytics_toggle(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    if !ctx.is_team_admin() {
        return toast_error(UNAUTHORIZED);
    }
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    let enable = !setting(&s, "is_traffic_analytics_enabled")
        .as_bool()
        .unwrap_or(false);
    let proxy = s
        .record
        .get("proxy")
        .and_then(|p| p.get("type"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let restarted = match coolify_jobs::server_ops::traffic::configure(jobs, s.id, enable).await {
        Ok(r) => r,
        Err(e) => return toast_error(e),
    };
    tracing::info!(target: "audit", event = if enable { "ui.server.traffic_analytics.enabled" } else { "ui.server.traffic_analytics.disabled" }, team_id = ctx.team_id, server_uuid = %uuid, "traffic analytics toggled");
    let state_word = if enable { "enabled" } else { "disabled" };
    // `proxyToggleMessage`.
    let mut message = if restarted {
        format!("Traffic analytics {state_word}. Restarting proxy and Sentinel.")
    } else if ["TRAEFIK", "CADDY"].contains(&proxy.as_str()) {
        format!(
            "Traffic analytics {state_word}. The proxy is stopped, so the new configuration applies the next time you start it."
        )
    } else {
        format!("Traffic analytics {state_word}.")
    };
    if enable && proxy == "CADDY" {
        message.push_str(" Caddy logs a resource only after you redeploy it.");
    }
    Ok(ok(json!({"message": message})).into_response())
}

/// `TrafficAnalyticsSettings::saveTrafficAnalyticsSettings`.
async fn analytics_save(
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
    let required_int = |name, min, max: Option<i64>| {
        let mut rules = vec![Rule::Required, Rule::Integer, Rule::MinNum(min)];
        if let Some(m) = max {
            rules.push(Rule::MaxNum(m));
        }
        Field { name, rules }
    };
    validate(
        &input,
        &[
            required_int("traffic_topn", 1, Some(1000)),
            required_int("traffic_sample_threshold", 0, None),
            required_int("traffic_retention_1h_days", 1, Some(365)),
            required_int("traffic_retention_1d_days", 1, Some(3650)),
            required_int("geoip_refresh_days", 1, None),
            Field {
                name: "geoip_maxmind_license_key",
                rules: vec![Rule::Nullable, Rule::String, Rule::Max(255)],
            },
            Field {
                name: "traffic_ip_mode",
                rules: vec![
                    Rule::Required,
                    Rule::String,
                    Rule::In(&["full", "anonymized", "off"]),
                ],
            },
        ],
        &[],
        &[
            "traffic_topn",
            "traffic_sample_threshold",
            "traffic_retention_1h_days",
            "traffic_retention_1d_days",
            "is_geoip_enabled",
            "geoip_refresh_days",
            "geoip_maxmind_license_key",
            "traffic_ip_mode",
        ],
    )?;
    let changed = save_settings(
        &state,
        &s,
        &[
            ("traffic_topn", Setting::Int(int_of(&input, "traffic_topn"))),
            (
                "traffic_sample_threshold",
                Setting::Int(int_of(&input, "traffic_sample_threshold")),
            ),
            (
                "traffic_retention_1h_days",
                Setting::Int(int_of(&input, "traffic_retention_1h_days")),
            ),
            (
                "traffic_retention_1d_days",
                Setting::Int(int_of(&input, "traffic_retention_1d_days")),
            ),
            (
                "is_geoip_enabled",
                Setting::Bool(input.get("is_geoip_enabled").is_some_and(php_truthy)),
            ),
            (
                "geoip_refresh_days",
                Setting::Int(int_of(&input, "geoip_refresh_days")),
            ),
            (
                "geoip_maxmind_license_key",
                Setting::Encrypted(
                    input
                        .str("geoip_maxmind_license_key")
                        .filter(|k| !k.is_empty())
                        .map(str::to_string),
                ),
            ),
            (
                "traffic_ip_mode",
                Setting::Text(input.str("traffic_ip_mode").map(str::to_string)),
            ),
        ],
    )
    .await?;
    sentinel_settings_hook(&state, &s, &changed);
    tracing::info!(target: "audit", event = "ui.server.traffic_analytics.updated", team_id = ctx.team_id, server_uuid = %uuid, "traffic analytics updated");
    Ok(
        ok(json!({"message": "Traffic analytics settings updated. Restarting Sentinel."}))
            .into_response(),
    )
}
