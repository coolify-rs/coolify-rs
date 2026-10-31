//! Session endpoints behind `Server\CloudflareTunnel`.
//!
//! `CloudflareTunnelService` (the setup through the Cloudflare API, and its tear down) is not ported: those two
//! paths answer with an error toast and never contact Cloudflare.

use axum::Extension;
use axum::Json;
use axum::Router;
use axum::extract::{Path, State};
use axum::response::IntoResponse;
use axum::routing::post;
use coolify_db::servers::{ColumnValue, ServerRow};
use coolify_jobs::Jobs;
use coolify_jobs::server::ServerInfo;
use coolify_jobs::server_ops::{Srv, tunnel};
use serde_json::{Map, Value, json};

use crate::auth::AuthCtx;
use crate::response::{ApiError, ApiResult, ok};
use crate::routes::cloud_providers::hostname_rule;
use crate::routes::server_settings::{find_server, is_localhost, restore_previous_ip};
use crate::serialize::php_truthy;
use crate::state::AppState;
use crate::ui_server_destinations::toast_error;
use crate::ui_server_general::SERVER_PAGE;

const UNAUTHORIZED: &str = "This action is unauthorized.";
const MANAGED_ELSEWHERE: &str = "This server is managed by another Coolify instance.";
const API_NOT_PORTED: &str = "Managing a tunnel through the Cloudflare API is not available in coolify-rs yet. Use an existing tunnel with its tunnel token.";

pub fn routes() -> Router<AppState> {
    let p = |s: &str| format!("{SERVER_PAGE}/cloudflare-tunnel{s}");
    Router::new()
        .route(&p(""), axum::routing::get(show))
        .route(&p("/manual"), post(manual))
        .route(&p("/disable"), post(disable))
        .route(&p("/update"), post(update))
        .route(&p("/automated"), post(automated))
        .route(&p("/api"), post(api_setup))
}

fn setting(s: &ServerRow, k: &str) -> Value {
    s.settings
        .as_ref()
        .and_then(|r| r.get(k))
        .cloned()
        .unwrap_or(Value::Null)
}

fn tunnel_id(s: &ServerRow) -> Option<String> {
    setting(s, "cloudflare_tunnel_id")
        .as_str()
        .filter(|t| !t.trim().is_empty())
        .map(str::to_string)
}

fn audit(s: &ServerRow, event: &'static str) {
    tracing::info!(
        target: "audit",
        event,
        team_id = ServerInfo::from_row(s).team_id,
        server_uuid = %s.uuid,
        "cloudflare tunnel"
    );
}

fn jobs(state: &AppState) -> Result<&Jobs, ApiError> {
    state.jobs.as_ref().ok_or(ApiError::NotPorted)
}

/// `CloudflareTunnel::mount` and `render`.
async fn show(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    let info = ServerInfo::from_row(&s);
    let tokens = coolify_db::server_ops::cloudflare_tokens(&state.db, info.team_id).await?;
    let id = tunnel_id(&s);
    let (text, password) = crate::ui::confirmation_steps(&state, ctx.user_id).await?;
    Ok(ok(json!({
        "confirm": {"text": text, "password": password},
        "is_localhost": is_localhost(&s),
        "enabled": php_truthy(&setting(&s, "is_cloudflare_tunnel")),
        "managed_tunnel": id.as_ref().map(|_| format!("coolify-{}", s.uuid)),
        "has_previous_ip": s.record.get("ip_previous").and_then(Value::as_str).is_some_and(|p| !p.is_empty() && p != "0"),
        "functional": info.is_functional(),
        "port": s.record.get("port").cloned().unwrap_or(Value::Null),
        "has_cloudflare_tokens": !tokens.is_empty(),
        "tunnel_tokens": tokens
            .iter()
            .filter(|(_, _, tunnel)| *tunnel)
            .map(|(id, name, _)| json!({"id": id, "name": name}))
            .collect::<Vec<_>>(),
    }))
    .into_response())
}

/// `CloudflareTunnel::manualCloudflareConfig`.
async fn manual(
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
    if !php_truthy(&setting(&s, "is_cloudflare_tunnel")) {
        coolify_db::servers::update_settings(
            &state.db,
            s.id,
            &[("is_cloudflare_tunnel", ColumnValue::Bool(true))],
            state.now(),
        )
        .await?;
    }
    audit(&s, "ui.server.cloudflare_tunnel.enabled");
    Ok(ok(json!({"message": "Cloudflare Tunnel enabled."})).into_response())
}

/// `CloudflareTunnel::toggleCloudflareTunnels`.
async fn disable(
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
    if tunnel_id(&s).is_some() {
        return toast_error(API_NOT_PORTED);
    }
    let srv = Srv::from_row(jobs(&state)?, s.clone());
    if let Err(e) = tunnel::remove_cloudflared(&srv).await {
        return toast_error(e);
    }
    coolify_db::servers::update_settings(
        &state.db,
        s.id,
        &[("is_cloudflare_tunnel", ColumnValue::Bool(false))],
        state.now(),
    )
    .await?;
    audit(&s, "ui.server.cloudflare_tunnel.disabled");
    if restore_previous_ip(&state, &s).await? {
        return Ok(ok(json!({"message": "Cloudflare Tunnel disabled.<br><br>Manually updated the server IP address to its previous IP address."})).into_response());
    }
    Ok(ok(json!({"warning": "Cloudflare Tunnel disabled. Action required: Update the server IP address to its real IP address in the Advanced settings."})).into_response())
}

/// `CloudflareTunnel::updateCloudflareTunnel`.
async fn update(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    if !ctx.is_team_admin() {
        return toast_error(UNAUTHORIZED);
    }
    let srv = Srv::from_row(jobs(&state)?, s.clone());
    match tunnel::update_cloudflared(&srv).await {
        Ok(activity) => {
            audit(&s, "ui.server.cloudflare_tunnel.update_started");
            Ok(ok(json!({"activity_id": activity})).into_response())
        }
        Err(e) => toast_error(e),
    }
}

/// `normalizeSshDomain`.
fn normalize_domain(domain: &str) -> String {
    domain
        .replace("https://", "")
        .replace("http://", "")
        .replace('/', "")
        .trim()
        .to_lowercase()
}

/// `CloudflareTunnel::automatedCloudflareConfig`.
async fn automated(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    Json(body): Json<Map<String, Value>>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    if !ctx.is_team_admin() {
        return toast_error(UNAUTHORIZED);
    }
    if ServerInfo::from_row(&s).transferred {
        return toast_error(MANAGED_ELSEWHERE);
    }
    let text = |k: &str| {
        body.get(k)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let domain = normalize_domain(&text("ssh_domain"));
    let srv = Srv::from_row(jobs(&state)?, s.clone());
    match tunnel::configure_cloudflared(&srv, &text("cloudflare_token"), &domain).await {
        Ok(activity) => {
            audit(&s, "ui.server.cloudflare_tunnel.configuration_started");
            Ok(ok(json!({"activity_id": activity})).into_response())
        }
        Err(e) => toast_error(e),
    }
}

/// `CloudflareTunnel::apiCloudflareConfig`: the validation; `CloudflareTunnelService::setUp` is not ported.
async fn api_setup(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    Json(body): Json<Map<String, Value>>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    authorize(&ctx)?;
    let domain = normalize_domain(
        body.get("api_ssh_domain")
            .and_then(Value::as_str)
            .unwrap_or_default(),
    );
    let id = body
        .get("cloudflare_integration_token_id")
        .and_then(|v| v.as_i64().or_else(|| v.as_str()?.parse().ok()));
    let mut errors = Map::new();
    if id.is_none() {
        errors.insert(
            "cloudflare_integration_token_id".into(),
            json!(["The Cloudflare token field is required."]),
        );
    }
    if domain.is_empty() {
        errors.insert(
            "api_ssh_domain".into(),
            json!(["The SSH hostname field is required."]),
        );
    } else if let Some(message) = hostname_rule(&json!(domain)) {
        errors.insert(
            "api_ssh_domain".into(),
            json!([message.replace(":attribute", "SSH hostname")]),
        );
    }
    if !errors.is_empty() {
        return Err(ApiError::Validation(errors));
    }
    let team = ServerInfo::from_row(&s).team_id;
    let tokens = coolify_db::server_ops::cloudflare_tokens(&state.db, team).await?;
    if !tokens
        .iter()
        .any(|(t, _, tunnel)| Some(*t) == id && *tunnel)
    {
        errors.insert(
            "cloudflare_integration_token_id".into(),
            json!(["Select a Cloudflare token of this server team with the Cloudflare Tunnel capability."]),
        );
        return Err(ApiError::Validation(errors));
    }
    toast_error(API_NOT_PORTED)
}

fn authorize(ctx: &AuthCtx) -> Result<(), ApiError> {
    crate::routes::server_settings::authorize_update(ctx)
}
