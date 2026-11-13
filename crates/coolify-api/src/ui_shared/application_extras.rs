//! Actions of the application sections that are not plain settings: `Rollback::rollbackImage`,
//! `Advanced::refreshGpus` and `Project\Application\Swarm`.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use base64::Engine as _;
use coolify_db::applications as apps;
use coolify_db::resource_ops as ops;
use coolify_db::servers::{self, ColumnValue};
use coolify_jobs::applications::queue::{QueueOptions, Queued, validate_commit};
use coolify_jobs::server::ServerInfo;
use coolify_jobs::server_ops::install::gather_server_metadata;
use serde_json::{Value, json};

use super::{Class, Res, find};
use crate::auth::AuthCtx;
use crate::response::{ApiError, ApiResult, ok};
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/ui/resources/{kind}/{uuid}/rollback/start",
            post(rollback_image),
        )
        .route(
            "/ui/resources/{kind}/{uuid}/gpus/refresh",
            post(refresh_gpus),
        )
        .route(
            "/ui/resources/{kind}/{uuid}/swarm",
            get(swarm_show).post(swarm_save),
        )
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

/// `rollbackImage($commit)`.
async fn rollback_image(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> ApiResult {
    let res = application(&state, &ctx, &kind, &uuid).await?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let commit = body
        .get("commit")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let commit = match validate_commit(commit) {
        Ok(c) => c,
        Err(e) => {
            return Ok((
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({"message": e.replace("deployment commit", "rollback commit")})),
            )
                .into_response());
        }
    };
    let a = apps::find_in_team(&state.db, res.team_id, &res.uuid)
        .await?
        .ok_or(super::NOT_FOUND)?;
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    // `GetRollbackImages::allows($this->application, $commit)`.
    if !jobs.rollback_allows(&a, &commit).await {
        return Ok(ok(json!({
            "title": "Rollback did not start.",
            "message": "The commit is not a rollback image or a successful deployment of this application.",
        }))
        .into_response());
    }
    let deployment = coolify_domain::new_public_id();
    let result = jobs
        .queue_application_deployment(
            &a,
            &deployment,
            QueueOptions {
                commit: Some(commit),
                rollback: true,
                force_rebuild: false,
                ..Default::default()
            },
        )
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "rollback could not be queued");
            ApiError::Internal
        })?;
    Ok(match result {
        Queued::QueueFull => {
            ok(json!({"title": "Deployment queue full", "message": result.message()}))
        }
        Queued::Skipped { .. } => {
            ok(json!({"title": "Deployment skipped", "message": result.message()}))
        }
        Queued::Queued { deployment_uuid } => ok(json!({"deployment_uuid": deployment_uuid})),
    }
    .into_response())
}

/// `refreshGpus()`.
async fn refresh_gpus(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid)): Path<(String, String)>,
) -> ApiResult {
    let res = application(&state, &ctx, &kind, &uuid).await?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    let mut ids: Vec<i64> = res.server_id.into_iter().collect();
    ids.extend(ops::additional_server_ids(&state.db, res.id).await?);
    ids.dedup();
    let mut functional = Vec::new();
    for id in ids {
        if let Some(row) = servers::find_by_id(&state.db, id).await?
            && ServerInfo::from_row(&row).is_functional()
        {
            functional.push(row);
        }
    }
    let error = |m: String| {
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({"message": m})),
        )
            .into_response()
    };
    if functional.is_empty() {
        return Ok(error(
            "Validate the server connection before you refresh GPUs.".to_owned(),
        ));
    }
    let mut failed: Vec<String> = Vec::new();
    for row in functional {
        if !matches!(gather_server_metadata(jobs, row.id).await, Ok(true)) {
            failed.push(
                row.record
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            );
        }
    }
    if !failed.is_empty() {
        return Ok(error(format!(
            "Could not collect GPUs from {}. Check the application logs for the remote command output.",
            failed.join(", ")
        )));
    }
    Ok(ok(json!({"message": "GPUs refreshed."})).into_response())
}

/// `mount()` of `Swarm`.
async fn swarm_show(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid)): Path<(String, String)>,
) -> ApiResult {
    let res = application(&state, &ctx, &kind, &uuid).await?;
    let constraints = res
        .str("swarm_placement_constraints")
        .filter(|c| !c.is_empty())
        .and_then(|c| base64::engine::general_purpose::STANDARD.decode(c).ok())
        .map(|b| String::from_utf8_lossy(&b).into_owned());
    Ok(ok(json!({
        "can_update": ctx.is_team_admin(),
        "swarm_replicas": res.int("swarm_replicas"),
        "swarm_placement_constraints": constraints,
        "is_swarm_only_worker_nodes": res.setting_bool("is_swarm_only_worker_nodes"),
    }))
    .into_response())
}

/// `instantSave()` and `submit()` of `Swarm`.
async fn swarm_save(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> ApiResult {
    let res = application(&state, &ctx, &kind, &uuid).await?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    // `#[Validate('required')] public int $swarmReplicas`: Livewire casts the input to an int.
    let replicas = body.get("swarmReplicas").and_then(|v| match v {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    });
    let Some(replicas) = replicas else {
        return Ok((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({
                "message": "The swarm replicas field is required.",
                "errors": {"swarmReplicas": ["The swarm replicas field is required."]},
            })),
        )
            .into_response());
    };
    let constraints = body
        .get("swarmPlacementConstraints")
        .and_then(Value::as_str)
        .filter(|c| !c.is_empty())
        .map(|c| base64::engine::general_purpose::STANDARD.encode(c));
    let now = state.now();
    apps::update_if_changed(
        &state.db,
        res.id,
        &[
            ("swarm_replicas", ColumnValue::Int(replicas)),
            (
                "swarm_placement_constraints",
                ColumnValue::Text(constraints),
            ),
        ],
        now,
    )
    .await?;
    let only_workers = body
        .get("isSwarmOnlyWorkerNodes")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    apps::update_settings(
        &state.db,
        res.id,
        &[(
            "is_swarm_only_worker_nodes",
            ColumnValue::Bool(only_workers),
        )],
        now,
    )
    .await?;
    Ok(ok(json!({"message": "Swarm settings updated."})).into_response())
}
