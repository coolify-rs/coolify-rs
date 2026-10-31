//! Session endpoints behind `Server\Advanced`, `Server\Swarm`, `Server\PrivateKey\Show`,
//! `Server\CaCertificate\Show`, `Server\Destinations`, `Server\Resources` and
//! `Server\Security\TerminalAccess`.

use std::sync::LazyLock;

use axum::Extension;
use axum::Json;
use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use coolify_db::destinations::{self as dest, Kind};
use coolify_db::private_keys;
use coolify_db::servers::ServerRow;
use coolify_domain::new_public_id;
use coolify_jobs::server::ServerInfo;
use coolify_jobs::server_ops::proxy as proxy_ops;
use coolify_jobs::server_ops::{Srv, certificate, install};
use regex::Regex;
use serde_json::{Map, Value, json};

use crate::auth::AuthCtx;
use crate::input::Input;
use crate::response::{ApiError, ApiResult, ok};
use crate::routes::server_settings::{Setting, authorize_update, find_server, save_settings};
use crate::state::AppState;
use crate::ui_server_destinations::toast_error;
use crate::validation::{Field, Rule, validate};

pub const SERVER_PAGE: &str = "/ui/server-page/{uuid}";

/// `ValidationPatterns::CONTAINER_NAME_PATTERN`.
static CONTAINER_NAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\A[a-zA-Z0-9][a-zA-Z0-9._-]*\z").unwrap_or_else(|_| unreachable!())
});

pub fn routes() -> Router<AppState> {
    let p = |s: &str| format!("{SERVER_PAGE}/{s}");
    Router::new()
        .route(&p("advanced"), get(advanced).post(save_advanced))
        .route(&p("swarm"), get(swarm).post(save_swarm))
        .route("/ui/deployments-indicator", get(deployments_indicator))
        .route(&p("details"), get(details).post(refresh_details))
        .route(&p("check-localhost"), post(check_localhost))
        .route(&p("private-key"), get(keys))
        .route(&p("private-key/use"), post(use_key))
        .route(&p("private-key/generate"), post(generate_key))
        .route(&p("private-key/check"), post(check_connection))
        .route(&p("ca-certificate"), get(ca))
        .route(&p("ca-certificate/save"), post(save_ca))
        .route(&p("ca-certificate/regenerate"), post(regenerate_ca))
        .route(&p("destinations"), get(destinations))
        .route(&p("destinations/scan"), post(scan_networks))
        .route(&p("destinations/add"), post(add_network))
        .route(&p("resources"), get(resources))
        .route(&p("resources/unmanaged"), get(unmanaged))
        .route(&p("resources/unmanaged/{action}"), post(unmanaged_action))
        .route(&p("terminal-access"), get(terminal).post(toggle_terminal))
}

fn setting(s: &ServerRow, k: &str) -> Value {
    s.settings
        .as_ref()
        .and_then(|r| r.get(k))
        .cloned()
        .unwrap_or(Value::Null)
}

fn int_of(input: &Input, k: &str) -> i64 {
    match input.get(k) {
        Some(Value::Number(n)) => n.as_i64().unwrap_or_default(),
        Some(Value::String(s)) => s.trim().parse().unwrap_or_default(),
        _ => 0,
    }
}

// ── Advanced ────────────────────────────────────────────────────────────

/// `Advanced::syncData`.
async fn advanced(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    Ok(ok(json!({
        "server_disk_usage_check_frequency": setting(&s, "server_disk_usage_check_frequency"),
        "server_disk_usage_notification_threshold": setting(&s, "server_disk_usage_notification_threshold"),
        "server_disk_usage_notification_interval_hours": setting(&s, "server_disk_usage_notification_interval_hours"),
        "concurrent_builds": setting(&s, "concurrent_builds"),
        "dynamic_timeout": setting(&s, "dynamic_timeout"),
        "deployment_queue_limit": setting(&s, "deployment_queue_limit"),
        "backup_compression_cpu_percentage": setting(&s, "backup_compression_cpu_percentage"),
    }))
    .into_response())
}

/// `Advanced::submit` and `instantSave`.
async fn save_advanced(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    Json(body): Json<Map<String, Value>>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    let input = Input(body);
    let frequency = input
        .str("server_disk_usage_check_frequency")
        .unwrap_or_default()
        .to_string();
    // `instantSave` does not check the expression.
    if input.get("instant").is_none() && !coolify_compat::cron::validate_cron_expression(&frequency)
    {
        return toast_error("Invalid Cron / Human expression for Disk Usage Check Frequency.");
    }
    if !ctx.is_team_admin() {
        return toast_error("This action is unauthorized.");
    }
    validate(
        &input,
        &[
            Field {
                name: "server_disk_usage_check_frequency",
                rules: vec![Rule::String],
            },
            Field {
                name: "server_disk_usage_notification_threshold",
                rules: vec![
                    Rule::Required,
                    Rule::Integer,
                    Rule::MinNum(1),
                    Rule::MaxNum(99),
                ],
            },
            Field {
                name: "server_disk_usage_notification_interval_hours",
                rules: vec![
                    Rule::Required,
                    Rule::Integer,
                    Rule::MinNum(1),
                    Rule::MaxNum(720),
                ],
            },
            Field {
                name: "concurrent_builds",
                rules: vec![Rule::Required, Rule::Integer, Rule::MinNum(1)],
            },
            Field {
                name: "dynamic_timeout",
                rules: vec![Rule::Required, Rule::Integer, Rule::MinNum(1)],
            },
            Field {
                name: "deployment_queue_limit",
                rules: vec![Rule::Required, Rule::Integer, Rule::MinNum(1)],
            },
            Field {
                name: "backup_compression_cpu_percentage",
                rules: vec![
                    Rule::Required,
                    Rule::Integer,
                    Rule::In(&["25", "50", "75", "100"]),
                ],
            },
        ],
        &[],
        &[
            "instant",
            "server_disk_usage_check_frequency",
            "server_disk_usage_notification_threshold",
            "server_disk_usage_notification_interval_hours",
            "concurrent_builds",
            "dynamic_timeout",
            "deployment_queue_limit",
            "backup_compression_cpu_percentage",
        ],
    )?;
    let changed = save_settings(
        &state,
        &s,
        &[
            (
                "concurrent_builds",
                Setting::Int(int_of(&input, "concurrent_builds")),
            ),
            (
                "dynamic_timeout",
                Setting::Int(int_of(&input, "dynamic_timeout")),
            ),
            (
                "deployment_queue_limit",
                Setting::Int(int_of(&input, "deployment_queue_limit")),
            ),
            (
                "backup_compression_cpu_percentage",
                Setting::Int(int_of(&input, "backup_compression_cpu_percentage")),
            ),
            (
                "server_disk_usage_notification_threshold",
                Setting::Int(int_of(&input, "server_disk_usage_notification_threshold")),
            ),
            (
                "server_disk_usage_check_frequency",
                Setting::Text(Some(frequency)),
            ),
            (
                "server_disk_usage_notification_interval_hours",
                Setting::Int(int_of(
                    &input,
                    "server_disk_usage_notification_interval_hours",
                )),
            ),
        ],
    )
    .await?;
    if !changed.is_empty() {
        tracing::info!(target: "audit", event = "ui.server.settings_updated", team_id = ctx.team_id, server_uuid = %uuid, "server settings updated");
    }
    Ok(ok(json!({"message": "Server updated."})).into_response())
}

// ── Swarm ───────────────────────────────────────────────────────────────

/// `$team->usesSwarm()`.
async fn team_uses_swarm(state: &AppState, team_id: i64) -> Result<bool, ApiError> {
    for s in coolify_db::servers::list(&state.db, team_id).await? {
        let flag = |k: &str| setting(&s, k).as_bool().unwrap_or(false);
        if flag("is_swarm_manager")
            || flag("is_swarm_worker")
            || coolify_db::databases::has_swarm_destinations(&state.db, s.id).await?
        {
            return Ok(true);
        }
    }
    Ok(false)
}

async fn swarm(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    Ok(ok(json!({
        "is_swarm_manager": setting(&s, "is_swarm_manager"),
        "is_swarm_worker": setting(&s, "is_swarm_worker"),
        "can_use_swarm": team_uses_swarm(&state, ctx.team_id).await?,
    }))
    .into_response())
}

/// `Swarm::instantSave`.
async fn save_swarm(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    Json(body): Json<Map<String, Value>>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    if !ctx.is_team_admin() {
        return toast_error("This action is unauthorized.");
    }
    if !team_uses_swarm(&state, ctx.team_id).await? {
        return toast_error("Docker Swarm is deprecated and cannot be enabled for new teams.");
    }
    let flag = |k: &str| body.get(k).and_then(Value::as_bool).unwrap_or(false);
    let changed = save_settings(
        &state,
        &s,
        &[
            ("is_swarm_manager", Setting::Bool(flag("is_swarm_manager"))),
            ("is_swarm_worker", Setting::Bool(flag("is_swarm_worker"))),
        ],
    )
    .await?;
    if !changed.is_empty() {
        tracing::info!(target: "audit", event = "ui.server.settings_updated", team_id = ctx.team_id, server_uuid = %uuid, "server settings updated");
    }
    Ok(ok(json!({"message": "Swarm settings updated."})).into_response())
}

// ── Private key ─────────────────────────────────────────────────────────

/// `PrivateKey\Show::loadPrivateKeys`: the server's own key first.
async fn keys(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    let current = s.record.get("private_key_id").and_then(Value::as_i64);
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let mut rows = private_keys::list(&state.db, s.team_id).await?;
    rows.retain(|k| {
        !k.record
            .get("is_git_related")
            .is_some_and(crate::serialize::php_truthy)
    });
    rows.sort_by_key(|k| Some(k.id) != current);
    let items: Vec<Value> = rows
        .iter()
        .map(|k| {
            let public = enc
                .decrypt_string(&k.private_key_encrypted)
                .ok()
                .and_then(|p| coolify_compat::sshkey::inspect(&p))
                .map(|i| i.public_key)
                .unwrap_or_else(|| "Error loading private key".into());
            json!({
                "id": k.id,
                "uuid": k.uuid,
                "name": k.name,
                "description": k.description,
                "public_key": public,
                "active": Some(k.id) == current,
            })
        })
        .collect();
    Ok(ok(json!({
        "keys": items,
        "transferred": crate::routes::servers::is_transferred(&s),
    }))
    .into_response())
}

/// `PrivateKey\Show::setPrivateKey`.
async fn use_key(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    Json(body): Json<Map<String, Value>>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    let id = body.get("id").and_then(Value::as_i64).unwrap_or_default();
    if private_keys::find_by_id(&state.db, s.team_id, id)
        .await?
        .is_none()
    {
        return toast_error("You are not allowed to use this private key.");
    }
    authorize_update(&ctx)?;
    if crate::routes::servers::is_transferred(&s) {
        return toast_error("The private key of a transferred server cannot be changed.");
    }
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    let previous = s.record.get("private_key_id").and_then(Value::as_i64);
    set_private_key(&state, s.id, id).await?;
    let fresh = coolify_db::servers::find_by_id(&state.db, s.id)
        .await?
        .ok_or(ApiError::Internal)?;
    let srv = Srv::from_row(jobs, fresh);
    match install::validate_connection(&srv).await {
        Ok((true, _)) => {
            Ok(ok(json!({"message": "Private key updated successfully."})).into_response())
        }
        Ok((false, error)) | Err(error) => {
            // The transaction rolls the key back; validating again stores the state.
            if let Some(previous) = previous {
                set_private_key(&state, s.id, previous).await?;
            }
            if let Some(row) = coolify_db::servers::find_by_id(&state.db, s.id).await? {
                let _ = install::validate_connection(&Srv::from_row(jobs, row)).await;
            }
            toast_error(error)
        }
    }
}

async fn set_private_key(state: &AppState, server_id: i64, key_id: i64) -> Result<(), ApiError> {
    Ok(coolify_db::servers::update_columns(
        &state.db,
        server_id,
        &[(
            "private_key_id",
            coolify_db::servers::ColumnValue::Int(key_id),
        )],
        state.now(),
    )
    .await?)
}

/// `PrivateKey\Show::generatePrivateKey`.
async fn generate_key(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    Json(body): Json<Map<String, Value>>,
) -> ApiResult {
    find_server(&state, &ctx, &uuid).await?;
    if !ctx.is_team_admin() {
        return toast_error("This action is unauthorized.");
    }
    let kind = match body.get("type").and_then(Value::as_str) {
        Some("ed25519") => coolify_compat::sshkey::KeyKind::Ed25519,
        Some("rsa") => coolify_compat::sshkey::KeyKind::Rsa,
        _ => return toast_error("Invalid private key type."),
    };
    let key_uuid = crate::routes::private_keys::ui_generate(&state, &ctx, kind).await?;
    let row = private_keys::find(&state.db, ctx.team_id, &key_uuid)
        .await?
        .ok_or(ApiError::Internal)?;
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let public = enc
        .decrypt_string(&row.private_key_encrypted)
        .ok()
        .and_then(|p| coolify_compat::sshkey::inspect(&p))
        .map(|i| i.public_key);
    Ok(
        ok(json!({"message": "Private key created successfully.", "public_key": public}))
            .into_response(),
    )
}

/// `PrivateKey\Show::checkConnection`.
async fn check_connection(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    if !ctx.is_team_admin() {
        return toast_error("This action is unauthorized.");
    }
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    match install::validate_connection(&Srv::from_row(jobs, s)).await {
        Ok((true, _)) => Ok(ok(json!({"message": "Server is reachable."})).into_response()),
        Ok((false, error)) | Err(error) => {
            let sanitized = error
                .replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;")
                .replace('"', "&quot;")
                .replace('\'', "&#039;");
            toast_error(format!(
                "Server is not reachable.<br><br>Check this <a target=\"_blank\" class=\"underline\" href=\"https://coolify.io/docs/knowledge-base/server/openssh\">documentation</a> for further help.<br><br>Error: {sanitized}"
            ))
        }
    }
}

/// `Livewire\\DeploymentsIndicator`.
async fn deployments_indicator(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
) -> ApiResult {
    let deployments = coolify_db::ui_servers::active_deployments(&state.db, ctx.team_id).await?;
    Ok(ok(json!({"deployments": deployments})).into_response())
}

// ── General ─────────────────────────────────────────────────────────────

/// What `livewire.server.partials.server-details` reads.
async fn details(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    let raw = s.record.get("server_metadata");
    let metadata = match raw {
        Some(Value::String(t)) => serde_json::from_str(t).unwrap_or(Value::Null),
        Some(v) => v.clone(),
        None => Value::Null,
    };
    Ok(ok(json!({
        "metadata": metadata,
        "docker_version": s.settings.as_ref().and_then(|t| t.get("docker_version")),
        "compose_version": s.settings.as_ref().and_then(|t| t.get("compose_version")),
    }))
    .into_response())
}

/// `Server\Show::refreshServerMetadata`.
async fn refresh_details(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    if !ctx.is_team_admin() {
        return toast_error("This action is unauthorized.");
    }
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    if !ServerInfo::from_row(&s).is_functional() {
        return toast_error("Validate the server connection before fetching details.");
    }
    match install::gather_server_metadata(jobs, s.id).await {
        Ok(true) => Ok(ok(json!({"message": "Server details refreshed."})).into_response()),
        _ => toast_error(
            "Could not collect server details. Check the application logs for the remote command error.",
        ),
    }
}

/// `Server\Show::checkLocalhostConnection`.
async fn check_localhost(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    if !ctx.is_team_admin() {
        return toast_error("This action is unauthorized.");
    }
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    let srv = Srv::from_row(jobs, s.clone());
    match install::validate_connection(&srv).await {
        Ok((true, _)) => {
            srv.settings_update(&[
                ("is_reachable", coolify_db::servers::ColumnValue::Bool(true)),
                ("is_usable", coolify_db::servers::ColumnValue::Bool(true)),
            ])
            .await
            .map_err(|_| ApiError::Internal)?;
            let _ = install::gather_server_metadata(jobs, s.id).await;
            Ok(ok(json!({"message": "Server is reachable."})).into_response())
        }
        _ => toast_error(
            "Server is not reachable.<br><br>Please validate your configuration and connection.",
        ),
    }
}

// ── CA certificate ──────────────────────────────────────────────────────

/// `CaCertificate\Show::loadCaCertificate`.
async fn ca(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    let row = coolify_db::ui_servers::ca_certificate_row(&state.db, s.id).await?;
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let content = row
        .as_ref()
        .and_then(|r| r.get("ssl_certificate"))
        .and_then(Value::as_str)
        .and_then(|c| enc.decrypt_string(c).ok());
    Ok(ok(json!({
        "certificate": content,
        "valid_until": row.as_ref().and_then(|r| r.get("valid_until")),
    }))
    .into_response())
}

/// `CaCertificate\Show::saveCaCertificate`.
async fn save_ca(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    Json(body): Json<Map<String, Value>>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    if !ctx.is_team_admin() {
        return toast_error("This action is unauthorized.");
    }
    let content = body
        .get("certificate")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if content.is_empty() {
        return toast_error("Certificate content cannot be empty.");
    }
    let Some(cleaned) = certificate::export_certificate(content) else {
        // PHP's warning, turned into an exception by Laravel, comes before upstream's own "Invalid certificate format.".
        return toast_error("openssl_x509_read(): X.509 Certificate cannot be retrieved");
    };
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    if let Some(row) = coolify_db::ui_servers::ca_certificate_row(&state.db, s.id).await? {
        let key = row
            .get("ssl_private_key")
            .and_then(Value::as_str)
            .and_then(|c| enc.decrypt_string(c).ok())
            .unwrap_or_default();
        if !certificate::certificate_matches_key(&cleaned, &key) {
            return toast_error(
                "This certificate does not match the CA private key of this server. Coolify signs database certificates with that key, so clients would fail with a certificate signature error.",
            );
        }
        coolify_db::ui_servers::update_ca_certificate(
            &state.db,
            row.get("id").and_then(Value::as_i64).unwrap_or_default(),
            &enc.encrypt_string(&cleaned),
            state.now(),
        )
        .await?;
        tracing::info!(target: "audit", event = "ui.server.ca_certificate.updated", team_id = ctx.team_id, server_uuid = %uuid, "CA certificate updated");
        let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
        // `remote_process` runs in the background; `RegenerateSslCertJob` is not ported.
        let _ = Srv::from_row(jobs, s)
            .run_quiet(
                &certificate::ca_certificate_file_commands(&cleaned),
                "server.ca_certificate",
            )
            .await;
    }
    Ok(
        ok(json!({"message": "CA Certificate saved successfully.", "certificate": cleaned}))
            .into_response(),
    )
}

/// `CaCertificate\Show::regenerateCaCertificate`.
async fn regenerate_ca(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    if !ctx.is_team_admin() {
        return toast_error("This action is unauthorized.");
    }
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    match certificate::regenerate_ca_certificate(&Srv::from_row(jobs, s)).await {
        Ok(_) => {
            tracing::info!(target: "audit", event = "ui.server.ca_certificate.regenerated", team_id = ctx.team_id, server_uuid = %uuid, "CA certificate regenerated");
            Ok(ok(json!({"message": "CA Certificate regenerated successfully."})).into_response())
        }
        Err(e) => toast_error(e),
    }
}

// ── Destinations ────────────────────────────────────────────────────────

/// `$server->standaloneDockers->concat($server->swarmDockers)`.
async fn destinations(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    let list: Vec<Value> = dest::list_for_server(&state.db, s.id)
        .await?
        .into_iter()
        .map(|d| json!({"uuid": d.uuid, "network": d.network, "is_swarm": d.kind == Kind::Swarm}))
        .collect();
    Ok(ok(json!({
        "destinations": list,
        "network": new_public_id(),
        "server": {"id": s.id, "uuid": s.uuid, "name": s.record.get("name")},
    }))
    .into_response())
}

fn kind_of(s: &ServerRow) -> Kind {
    if ServerInfo::from_row(s).is_swarm() {
        Kind::Swarm
    } else {
        Kind::Standalone
    }
}

/// `Destinations::scan`.
async fn scan_networks(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    if !ctx.is_team_admin() {
        return toast_error("This action is unauthorized.");
    }
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    let added = dest::networks(&state.db, kind_of(&s), s.id).await?;
    let out = jobs
        .instant_remote(
            &s,
            &["docker network ls --format \"{{json .}}\"".to_string()],
            false,
            "server.destinations.scan",
        )
        .await
        .ok()
        .flatten()
        .unwrap_or_default();
    let networks: Vec<String> = out
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l.trim()).ok())
        .filter_map(|v| v.get("Name").and_then(Value::as_str).map(str::to_string))
        .filter(|n| !["bridge", "host", "none"].contains(&n.as_str()) && !added.contains(n))
        .collect();
    let message = if networks.is_empty() {
        "No new destinations found on this server."
    } else {
        "Scan done."
    };
    Ok(ok(json!({"message": message, "networks": networks})).into_response())
}

/// `Destinations::add`.
async fn add_network(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    Json(body): Json<Map<String, Value>>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    authorize_update(&ctx)?;
    let network = body
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let kind = kind_of(&s);
    if dest::network_exists(&state.db, kind, s.id, &network).await? {
        return toast_error("Network already added to this server.");
    }
    let name = format!(
        "{}-{network}",
        s.record
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
    );
    let dest_uuid = new_public_id();
    dest::create(
        &state.db,
        kind,
        &dest_uuid,
        &name,
        &network,
        s.id,
        state.now(),
    )
    .await?;
    if kind == Kind::Standalone
        && let Some(jobs) = &state.jobs
    {
        let _ = jobs
            .instant_remote(
                &s,
                &proxy_ops::destination_created_commands(&network),
                false,
                "destination.created",
            )
            .await;
    }
    tracing::info!(target: "audit", event = "ui.destination.created", team_id = ctx.team_id, destination_uuid = %dest_uuid, "destination created");
    Ok(ok(json!({})).into_response())
}

// ── Resources ───────────────────────────────────────────────────────────

fn str_of(r: &coolify_db::records::Record, k: &str) -> Value {
    r.get(k).cloned().unwrap_or(Value::Null)
}

/// `definedResources()` as the `Server\Resources` table shows them.
async fn resources(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    let mut items = Vec::new();
    for a in coolify_db::applications::list_for_server(&state.db, s.id).await? {
        let p = str_of(&a.project, "uuid");
        let e = str_of(&a.environment, "uuid");
        let status = a.str("status").unwrap_or_default().to_string();
        items.push(json!({
            "uuid": a.str("uuid"),
            "name": a.str("name"),
            "type": "application",
            "link": link(&p, &e, "application", a.str("uuid").unwrap_or_default()),
            "project": a.project.get("name"),
            "environment": a.environment.get("name"),
            "status": coolify_jobs::applications::status::normalize_status(&status),
            "application": {
                "buildPack": a.str("build_pack"),
                "sourceType": a.str("source_type"),
                "fqdn": a.str("fqdn"),
                "basicAuth": a.settings.get("is_http_basic_auth_enabled").is_some_and(crate::serialize::php_truthy) || a.bool("is_http_basic_auth_enabled"),
            },
            "restart_limit": status.starts_with("exited")
                && a.bool("container_present") && a.bool("restart_limit_reached"),
            "restart_count": a.int("restart_count").unwrap_or_default().max(a.int("max_restart_count").unwrap_or_default()),
        }));
    }
    for d in coolify_db::databases::list_for_server(&state.db, s.id).await? {
        let p = str_of(&d.project, "uuid");
        let e = str_of(&d.environment, "uuid");
        items.push(json!({
            "uuid": d.str("uuid"),
            "name": d.str("name"),
            "type": d.kind.type_name(),
            "link": link(&p, &e, "database", d.str("uuid").unwrap_or_default()),
            "project": d.project.get("name"),
            "environment": d.environment.get("name"),
            "status": coolify_jobs::databases::normalize_status(d.str("status").unwrap_or_default()),
        }));
    }
    for service in coolify_db::services::list_for_server(&state.db, s.id).await? {
        let p = str_of(&service.project, "uuid");
        let e = str_of(&service.environment, "uuid");
        items.push(json!({
            "uuid": service.uuid,
            "name": service.str("name"),
            "type": "service",
            "service_type": service.str("service_type"),
            "link": link(&p, &e, "service", &service.uuid),
            "project": service.project.get("name"),
            "environment": service.environment.get("name"),
            "status": crate::routes::services::status_of(&state, &service).await?,
        }));
    }
    Ok(ok(json!({"resources": items})).into_response())
}

fn link(project: &Value, environment: &Value, kind: &str, uuid: &str) -> Value {
    match (project.as_str(), environment.as_str()) {
        (Some(p), Some(e)) => json!(format!("/project/{p}/environment/{e}/{kind}/{uuid}")),
        _ => Value::Null,
    }
}

/// `$server->loadUnmanagedContainers()`.
async fn unmanaged(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    let mut containers = Vec::new();
    if ServerInfo::from_row(&s).is_functional() {
        let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
        let out = match jobs
            .instant_remote(
                &s,
                &["docker ps -a --format '{{json .}}'".to_string()],
                true,
                "server.resources.unmanaged",
            )
            .await
        {
            Ok(o) => o.unwrap_or_default(),
            Err(e) => return toast_error(e),
        };
        for line in out.lines() {
            let Ok(v) = serde_json::from_str::<Value>(line.trim()) else {
                continue;
            };
            if v.get("Labels")
                .and_then(Value::as_str)
                .is_some_and(|l| l.contains("coolify.managed"))
            {
                continue;
            }
            containers.push(json!({
                "ID": v.get("ID"),
                "Names": v.get("Names"),
                "Image": v.get("Image"),
                "State": v.get("State"),
            }));
        }
    }
    Ok(ok(json!({"containers": containers})).into_response())
}

/// `Resources::startUnmanaged`, `restartUnmanaged` and `stopUnmanaged`.
async fn unmanaged_action(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((uuid, action)): Path<(String, String)>,
    Json(body): Json<Map<String, Value>>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    if !ctx.is_team_admin() {
        return toast_error("This action is unauthorized.");
    }
    let id = body.get("id").and_then(Value::as_str).unwrap_or_default();
    if !CONTAINER_NAME.is_match(id) {
        return toast_error("Invalid container identifier.");
    }
    let arg = coolify_jobs::server_ops::shell_arg(id);
    let (command, message) = match action.as_str() {
        "start" => (format!("docker start {arg}"), "Container started."),
        "restart" => (format!("docker restart {arg}"), "Container restarted."),
        "stop" => (
            ServerInfo::from_row(&s).docker_stop_command(0, &arg),
            "Container stopped.",
        ),
        _ => {
            return Err(ApiError::Message(
                StatusCode::NOT_FOUND,
                "Unknown container action.",
            ));
        }
    };
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    match jobs
        .instant_remote(&s, &[command], true, "server.resources.unmanaged")
        .await
    {
        Ok(_) => Ok(ok(json!({"message": message})).into_response()),
        Err(e) => toast_error(e),
    }
}

// ── Terminal access ─────────────────────────────────────────────────────

async fn terminal(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    authorize_update(&ctx)?;
    let (text, password) = crate::ui::confirmation_steps(&state, ctx.user_id).await?;
    Ok(ok(json!({
        "is_terminal_enabled": setting(&s, "is_terminal_enabled"),
        "confirm": {"text": text, "password": password},
    }))
    .into_response())
}

/// `TerminalAccess::toggleTerminal`.
async fn toggle_terminal(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    Json(body): Json<Map<String, Value>>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    if !ctx.is_team_admin() {
        return toast_error("This action is unauthorized.");
    }
    if !crate::ui::password_confirmed(
        &state,
        ctx.user_id,
        body.get("password").and_then(Value::as_str),
    )
    .await?
    {
        return Err(ApiError::Message(
            StatusCode::UNPROCESSABLE_ENTITY,
            "The provided password is incorrect.",
        ));
    }
    let next = !setting(&s, "is_terminal_enabled")
        .as_bool()
        .unwrap_or(false);
    save_settings(&state, &s, &[("is_terminal_enabled", Setting::Bool(next))]).await?;
    let status = if next { "enabled" } else { "disabled" };
    tracing::info!(target: "audit", event = "ui.server.terminal_access", status, team_id = ctx.team_id, server_uuid = %uuid, "terminal access changed");
    Ok(ok(json!({
        "message": format!("Terminal access has been {status}."),
        "is_terminal_enabled": next,
    }))
    .into_response())
}
