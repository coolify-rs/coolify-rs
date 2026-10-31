//! `Project\Shared\Maintenance`: the proxy shows a maintenance page for an application or service.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use coolify_db::resource_ops as ops;
use coolify_db::servers;
use coolify_jobs::server::ServerInfo;
use coolify_jobs::server_ops::proxy::sync_maintenance_pages;
use serde_json::{Value, json};

use super::{Class, Res, find};
use crate::auth::AuthCtx;
use crate::response::{ApiError, ApiResult, ok};
use crate::state::AppState;

/// `Server::PROXY_ERROR_PAGE_MAX_BYTES`.
const MAX_BYTES: usize = 1_048_576;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/ui/resources/{kind}/{uuid}/maintenance",
            get(show).post(submit),
        )
        .route(
            "/ui/resources/{kind}/{uuid}/maintenance/toggle",
            post(toggle),
        )
        .route("/ui/resources/{kind}/{uuid}/maintenance/reset", post(reset))
}

/// `maintenanceServers()`: the ids.
async fn maintenance_servers(state: &AppState, res: &Res) -> Result<Vec<i64>, ApiError> {
    let mut ids: Vec<i64> = res.server_id.into_iter().collect();
    if res.class == Class::Application {
        for id in ops::additional_server_ids(&state.db, res.id).await? {
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
    }
    Ok(ids)
}

/// `getIsSupportedProperty()`.
async fn is_supported(state: &AppState, res: &Res) -> Result<bool, ApiError> {
    for id in maintenance_servers(state, res).await? {
        let Some(row) = servers::find_by_id(&state.db, id).await? else {
            continue;
        };
        let proxy = row.record.get("proxy").and_then(|p| match p {
            Value::String(s) => serde_json::from_str::<Value>(s).ok(),
            other => Some(other.clone()),
        });
        let kind = proxy
            .as_ref()
            .and_then(|p| p.get("type"))
            .and_then(Value::as_str);
        if !ServerInfo::from_row(&row).is_swarm() && matches!(kind, Some("TRAEFIK" | "CADDY")) {
            return Ok(true);
        }
    }
    Ok(false)
}

async fn found(state: &AppState, ctx: &AuthCtx, kind: &str, uuid: &str) -> Result<Res, ApiError> {
    let res = find(state, ctx, kind, uuid).await?;
    if !matches!(res.class, Class::Application | Class::Service) {
        return Err(super::NOT_FOUND);
    }
    Ok(res)
}

async fn sync(state: &AppState, res: &Res) -> Result<(), ApiError> {
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    let ids = maintenance_servers(state, res).await?;
    sync_maintenance_pages(jobs, &ids).await.map_err(|e| {
        // `handleError()` shows the server's message as a toast.
        tracing::warn!(resource = %res.uuid, error = %e, "maintenance page could not be applied");
        ApiError::Message(
            StatusCode::INTERNAL_SERVER_ERROR,
            "The maintenance page could not be applied on the server.",
        )
    })
}

async fn show(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid)): Path<(String, String)>,
) -> ApiResult {
    let res = found(&state, &ctx, &kind, &uuid).await?;
    Ok(ok(json!({
        "can_update": ctx.is_team_admin(),
        "is_supported": is_supported(&state, &res).await?,
        "is_maintenance_enabled": res.bool("is_maintenance_enabled"),
        "maintenance_page": res.str("maintenance_page"),
    }))
    .into_response())
}

/// `instantSaveMaintenance()`.
async fn toggle(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> ApiResult {
    let res = found(&state, &ctx, &kind, &uuid).await?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let enabled = body
        .get("enabled")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    ops::set_maintenance(
        &state.db,
        res.table,
        res.id,
        Some(enabled),
        None,
        state.now(),
    )
    .await?;
    sync(&state, &res).await?;
    Ok(ok(json!({"message": if enabled { "Maintenance mode enabled." } else { "Maintenance mode disabled." }})).into_response())
}

/// `submit()`.
async fn submit(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> ApiResult {
    let res = found(&state, &ctx, &kind, &uuid).await?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let page = body
        .get("maintenancePage")
        .and_then(Value::as_str)
        .filter(|p| !p.trim().is_empty());
    if page.is_some_and(|p| p.len() > MAX_BYTES) {
        return Err(ApiError::Message(
            StatusCode::UNPROCESSABLE_ENTITY,
            "The maintenance page must not be larger than 1024 KB.",
        ));
    }
    ops::set_maintenance(&state.db, res.table, res.id, None, Some(page), state.now()).await?;
    if res.bool("is_maintenance_enabled") {
        sync(&state, &res).await?;
    }
    Ok(ok(json!({"message": "Maintenance page saved."})).into_response())
}

/// `resetMaintenancePage()`.
async fn reset(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid)): Path<(String, String)>,
) -> ApiResult {
    let res = found(&state, &ctx, &kind, &uuid).await?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    ops::set_maintenance(&state.db, res.table, res.id, None, Some(None), state.now()).await?;
    if res.bool("is_maintenance_enabled") {
        sync(&state, &res).await?;
    }
    Ok(ok(json!({"message": "Default maintenance page restored."})).into_response())
}
