//! `Project\Shared\Destination`: the servers section. Databases and services only show their primary
//! server. Deploying to one additional server (`only_this_server`) needs the deploy job's multi-server
//! path, which is not ported: that action answers 501.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use coolify_db::applications as apps;
use coolify_db::resource_ops as ops;
use coolify_db::servers;
use coolify_jobs::applications::queue::{QueueOptions, Queued};
use serde_json::{Value, json};

use super::{Class, Res, find};
use crate::auth::AuthCtx;
use crate::response::{ApiError, ApiResult, ok};
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/ui/resources/{kind}/{uuid}/destination", get(show))
        .route("/ui/resources/{kind}/{uuid}/destination/add", post(add))
        .route(
            "/ui/resources/{kind}/{uuid}/destination/remove",
            post(remove),
        )
        .route(
            "/ui/resources/{kind}/{uuid}/destination/promote",
            post(promote),
        )
        .route("/ui/resources/{kind}/{uuid}/destination/stop", post(stop))
        .route(
            "/ui/resources/{kind}/{uuid}/destination/redeploy",
            post(redeploy),
        )
}

const fn bad(message: &'static str) -> ApiError {
    ApiError::Message(StatusCode::UNPROCESSABLE_ENTITY, message)
}

fn int(body: &Value, key: &str) -> i64 {
    body.get(key)
        .and_then(|v| v.as_i64().or_else(|| v.as_str()?.parse().ok()))
        .unwrap_or_default()
}

/// `$server->proxyType()`.
fn proxy_type(row: &servers::ServerRow) -> Option<String> {
    let proxy = row.record.get("proxy").and_then(|p| match p {
        Value::String(s) => serde_json::from_str::<Value>(s).ok(),
        other => Some(other.clone()),
    });
    proxy
        .as_ref()
        .and_then(|p| p.get("type"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// `loadData()` and the view.
async fn show(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid)): Path<(String, String)>,
) -> ApiResult {
    let res = find(&state, &ctx, &kind, &uuid).await?;
    let Some((dest_id, _, _, network, server_id, server_uuid, server_name)) =
        ops::destination_info(&state.db, res.table, res.id).await?
    else {
        return Err(ApiError::NotPorted);
    };
    let is_app = res.class == Class::Application;
    let additional = if is_app {
        ops::additional_networks(&state.db, res.id).await?
    } else {
        Vec::new()
    };
    let mut networks = Vec::new();
    if is_app {
        for (id, name, net_server_id, net_server_name) in
            ops::team_standalone_networks(&state.db, res.team_id).await?
        {
            let taken = id == dest_id
                || net_server_id == server_id
                || additional
                    .iter()
                    .any(|a| a.network_id == id || a.server_id == net_server_id);
            if !taken {
                networks.push(json!({"id": id, "name": name, "server_id": net_server_id, "server_name": net_server_name}));
            }
        }
    }
    Ok(ok(json!({
        "can_update": ctx.is_team_admin(),
        "can_deploy": ctx.is_team_admin(),
        "is_application": is_app,
        "build_pack": res.str("build_pack"),
        "status": res.str("status"),
        "primary": {
            "network_id": dest_id,
            "network": network,
            "server_id": server_id,
            "server_uuid": server_uuid,
            "server_name": server_name,
        },
        "additional": additional.iter().map(|a| json!({
            "network_id": a.network_id,
            "network": a.network,
            "name": a.network_name,
            "server_id": a.server_id,
            "server_uuid": a.server_uuid,
            "server_name": a.server_name,
            "status": a.status,
        })).collect::<Vec<_>>(),
        "has_persistent_storage": is_app && ops::has_persistent_storage(&state.db, res.id).await?,
        "networks": networks,
        "restart_limit_reached": res.bool("restart_limit_reached"),
        "container_present": res.bool("container_present"),
        "restart_count": res.int("restart_count"),
        "max_restart_count": res.int("max_restart_count"),
    }))
    .into_response())
}

async fn application(
    state: &AppState,
    ctx: &AuthCtx,
    kind: &str,
    uuid: &str,
) -> Result<Res, ApiError> {
    let res = find(state, ctx, kind, uuid).await?;
    if res.class != Class::Application {
        return Err(super::NOT_FOUND);
    }
    Ok(res)
}

/// `resourceServers()->findOrFail($id)`.
async fn team_server(state: &AppState, res: &Res, id: i64) -> Result<servers::ServerRow, ApiError> {
    servers::find_by_id(&state.db, id)
        .await?
        .filter(|s| s.team_id == res.team_id)
        .ok_or(super::NOT_FOUND)
}

/// `addServer()`.
async fn add(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> ApiResult {
    let res = application(&state, &ctx, &kind, &uuid).await?;
    let server = team_server(&state, &res, int(&body, "server_id")).await?;
    let network_id = int(&body, "network_id");
    if !ops::standalone_on_server(&state.db, network_id, server.id).await? {
        return Err(super::NOT_FOUND);
    }
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    // `additionalServersUnavailableReason($server)`.
    let reason = if res.str("build_pack") == Some("dockercompose") {
        Some("Docker Compose applications cannot use multiple servers.".to_owned())
    } else if ops::has_persistent_storage(&state.db, res.id).await? {
        Some("Applications with persistent storage cannot use multiple servers because volumes are not shared between servers.".to_owned())
    } else {
        let primary = match res.server_id {
            Some(id) => servers::find_by_id(&state.db, id).await?,
            None => None,
        };
        let a = primary.as_ref().and_then(proxy_type);
        let b = proxy_type(&server);
        let routing = |p: &Option<String>| matches!(p.as_deref(), Some("TRAEFIK" | "CADDY"));
        if routing(&a) && routing(&b) && a != b {
            let name = server
                .record
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            Some(format!(
                "The primary server uses {} and {name} uses {}. All servers of an application must use the same proxy, because they share the same proxy labels.",
                a.unwrap_or_default(),
                b.unwrap_or_default()
            ))
        } else {
            None
        }
    };
    if let Some(reason) = reason {
        return Ok(
            ok(json!({"error": "Failed to add server.", "details": reason})).into_response(),
        );
    }
    ops::attach_network(&state.db, res.id, network_id, server.id, state.now()).await?;
    Ok(ok(json!({})).into_response())
}

/// `removeServer()`.
async fn remove(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> ApiResult {
    let res = application(&state, &ctx, &kind, &uuid).await?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    if !crate::ui::password_confirmed(
        &state,
        ctx.user_id,
        body.get("password").and_then(Value::as_str),
    )
    .await?
    {
        return Err(bad("The provided password is incorrect."));
    }
    let network_id = int(&body, "network_id");
    let server_id = int(&body, "server_id");
    let primary = ops::destination_info(&state.db, res.table, res.id).await?;
    if primary
        .as_ref()
        .is_some_and(|p| p.0 == network_id && p.4 == server_id)
    {
        return Err(bad("You are trying to remove the main server."));
    }
    team_server(&state, &res, server_id).await?;
    let a = apps::find_in_team(&state.db, res.team_id, &res.uuid)
        .await?
        .ok_or(super::NOT_FOUND)?;
    if let Some(jobs) = state.jobs.as_ref() {
        let _ = jobs.stop_application_on(&a, server_id).await;
    }
    ops::detach_network(&state.db, res.id, network_id, server_id).await?;
    Ok(ok(json!({})).into_response())
}

/// `promote()`.
async fn promote(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> ApiResult {
    let res = application(&state, &ctx, &kind, &uuid).await?;
    let server = team_server(&state, &res, int(&body, "server_id")).await?;
    let network_id = int(&body, "network_id");
    if !ops::standalone_on_server(&state.db, network_id, server.id).await? {
        return Err(super::NOT_FOUND);
    }
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let Some(primary) = ops::destination_info(&state.db, res.table, res.id).await? else {
        return Err(ApiError::NotPorted);
    };
    ops::promote_network(
        &state.db,
        res.id,
        network_id,
        server.id,
        primary.0,
        primary.4,
        state.now(),
    )
    .await?;
    // refreshServers()
    if let Some(jobs) = state.jobs.as_ref() {
        jobs.check_server_applications(server.id).await;
    }
    Ok(ok(json!({})).into_response())
}

/// `stop($serverId)`.
async fn stop(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> ApiResult {
    let res = application(&state, &ctx, &kind, &uuid).await?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let server = team_server(&state, &res, int(&body, "server_id")).await?;
    let a = apps::find_in_team(&state.db, res.team_id, &res.uuid)
        .await?
        .ok_or(super::NOT_FOUND)?;
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    jobs.stop_application_on(&a, server.id).await;
    // refreshServers()
    if let Some(main) = res.server_id {
        jobs.check_server_applications(main).await;
    }
    Ok(ok(json!({})).into_response())
}

/// `redeploy($network_id, $server_id)`.
async fn redeploy(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> ApiResult {
    let res = application(&state, &ctx, &kind, &uuid).await?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    if !ops::additional_server_ids(&state.db, res.id)
        .await?
        .is_empty()
        && res
            .str("docker_registry_image_name")
            .is_none_or(str::is_empty)
    {
        return Ok(ok(json!({"error": "Failed to deploy.", "details": "Before deploying to multiple servers, you must first set a Docker image in the General tab.<br>More information here: <a target=\"_blank\" class=\"underline\" href=\"https://coolify.io/docs/knowledge-base/server/multiple-servers\">documentation</a>"})).into_response());
    }
    let server = team_server(&state, &res, int(&body, "server_id")).await?;
    let network_id = int(&body, "network_id");
    if !ops::standalone_on_server(&state.db, network_id, server.id).await? {
        return Err(super::NOT_FOUND);
    }
    let a = apps::find_in_team(&state.db, res.team_id, &res.uuid)
        .await?
        .ok_or(super::NOT_FOUND)?;
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    let deployment = coolify_domain::new_public_id();
    let result = jobs
        .queue_application_deployment(
            &a,
            &deployment,
            QueueOptions {
                server_id: Some(server.id),
                destination_id: Some(network_id),
                only_this_server: true,
                no_questions_asked: true,
                ..Default::default()
            },
        )
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "deployment could not be queued");
            ApiError::Internal
        })?;
    Ok(match result {
        Queued::QueueFull => {
            ok(json!({"error": "Deployment queue full", "details": result.message()}))
                .into_response()
        }
        Queued::Skipped { .. } => {
            ok(json!({"skipped": "Deployment skipped", "details": result.message()}))
                .into_response()
        }
        Queued::Queued { deployment_uuid } => {
            ok(json!({"deployment_uuid": deployment_uuid})).into_response()
        }
    })
}
