//! Session endpoints behind the destination pages (`Destination\Index`, `Destination\New\Docker`,
//! `Destination\Show`, `Destination\Resources`).

use std::sync::LazyLock;

use axum::Extension;
use axum::Json;
use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use coolify_db::destinations::{self as dest, Kind};
use coolify_db::servers;
use coolify_domain::new_public_id;
use coolify_jobs::server_ops::Srv;
use coolify_jobs::server_ops::proxy as proxy_ops;
use regex::Regex;
use serde_json::{Map, Value, json};

use crate::auth::AuthCtx;
use crate::input::Input;
use crate::response::{ApiError, ApiResult, ok};
use crate::routes::server_settings::json_status;
use crate::state::AppState;
use crate::validation::{Field, Rule, validate};

static NETWORK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[a-zA-Z0-9][a-zA-Z0-9._-]*$").unwrap_or_else(|_| unreachable!())
});

/// `config('deprecations.swarm')`.
const SWARM_NOTICE: &str = "Docker Swarm is deprecated and will be removed in Coolify v5. Coolify v5 will be replacing Swarm with native Docker Compose replicas and our own scaling solution. Existing Swarm deployments will continue to work on v4 as-is. We do not recommend setting up new Swarm deployments for the time being.";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/ui/destination-list", get(index).post(create))
        .route("/ui/destination-list/{uuid}", get(show).patch(update))
        .route("/ui/destination-list/{uuid}/delete", post(delete))
        .route("/ui/destination-list/{uuid}/resources", get(resources))
}

/// `$dispatch('error', $message)`.
pub(crate) fn toast_error(message: impl Into<String>) -> ApiResult {
    Ok(json_status(
        StatusCode::UNPROCESSABLE_ENTITY,
        json!({"message": message.into()}),
    ))
}

/// `Server::isUsable()`.
pub(crate) fn is_usable(s: &servers::ServerRow) -> bool {
    let settings = s.settings.clone().unwrap_or_default();
    let flag = |k: &str| settings.get(k).and_then(Value::as_bool).unwrap_or(false);
    flag("is_reachable")
        && flag("is_usable")
        && !flag("is_swarm_worker")
        && !flag("force_disabled")
        && !crate::routes::servers::effective_role_is_build(&settings)
}

/// `Destination\Index` + `Destination\New\Docker::mount`.
async fn index(State(state): State<AppState>, Extension(ctx): Extension<AuthCtx>) -> ApiResult {
    let usable: Vec<_> = servers::list(&state.db, ctx.team_id)
        .await?
        .into_iter()
        .filter(is_usable)
        .collect();
    let mut destinations = Vec::new();
    let mut list = Vec::new();
    for s in &usable {
        list.push(json!({
            "id": s.id,
            "uuid": s.uuid,
            "name": s.record.get("name"),
        }));
        for d in dest::list_for_server(&state.db, s.id).await? {
            destinations.push(json!({
                "uuid": d.uuid,
                "name": d.name,
                "server": s.record.get("name"),
                "is_swarm": d.kind == Kind::Swarm,
            }));
        }
    }
    Ok(ok(json!({
        "servers": list,
        "destinations": destinations,
        "network": new_public_id(),
        "can_create": ctx.is_team_admin(),
    }))
    .into_response())
}

/// `Destination\New\Docker::submit`.
async fn create(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Json(body): Json<Map<String, Value>>,
) -> ApiResult {
    let input = Input(body);
    let server_id: Option<i64> = input.str("serverId").and_then(|v| v.parse().ok());
    let s = match server_id {
        Some(id) => servers::find_by_id(&state.db, id)
            .await?
            .filter(|s| s.team_id == ctx.team_id),
        None => None,
    }
    .ok_or(ApiError::Message(
        StatusCode::NOT_FOUND,
        "Server not found.",
    ))?;
    if !ctx.is_team_admin() {
        return toast_error("This action is unauthorized.");
    }
    validate(
        &input,
        &[
            Field {
                name: "name",
                rules: vec![Rule::Required, Rule::String],
            },
            Field {
                name: "network",
                rules: vec![
                    Rule::Required,
                    Rule::String,
                    Rule::Max(255),
                    Rule::Regex(&NETWORK),
                ],
            },
        ],
        &[],
        &["name", "network", "serverId"],
    )?;
    let name = input.str("name").unwrap_or_default();
    let network = input.str("network").unwrap_or_default();
    let info = coolify_jobs::server::ServerInfo::from_row(&s);
    let kind = if info.is_swarm() {
        Kind::Swarm
    } else {
        Kind::Standalone
    };
    if dest::network_exists(&state.db, kind, s.id, network).await? {
        return toast_error("Network already added to this server.");
    }
    let uuid = new_public_id();
    if dest::create(&state.db, kind, &uuid, name, network, s.id, state.now())
        .await?
        .is_none()
    {
        return toast_error("Duplicate entry found. Please use a different name.");
    }
    if kind == Kind::Standalone
        && let Some(jobs) = &state.jobs
    {
        // `StandaloneDocker::created`.
        let _ = jobs
            .instant_remote(
                &s,
                &proxy_ops::destination_created_commands(network),
                false,
                "destination.created",
            )
            .await;
    }
    tracing::info!(target: "audit", event = "ui.destination.created", team_id = ctx.team_id, destination_uuid = %uuid, "destination created");
    Ok(ok(json!({"uuid": uuid})).into_response())
}

/// `find_destination_for_current_team`.
async fn find(state: &AppState, ctx: &AuthCtx, uuid: &str) -> Result<dest::Destination, ApiError> {
    dest::find_in_team(&state.db, ctx.team_id, uuid)
        .await?
        .ok_or(ApiError::Message(
            StatusCode::NOT_FOUND,
            "Destination not found.",
        ))
}

/// `Destination\Show::mount`.
async fn show(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let d = find(&state, &ctx, &uuid).await?;
    let s = servers::find_by_id(&state.db, d.server_id).await?;
    let record = s.as_ref().map(|s| &s.record);
    Ok(ok(json!({
        "uuid": d.uuid,
        "name": d.name,
        "network": d.network,
        "is_swarm": d.kind == Kind::Swarm,
        "server_ip": record.and_then(|r| r.get("ip")),
        "server_name": record.and_then(|r| r.get("name")),
        "can_update": ctx.is_team_admin(),
        "swarm_notice": SWARM_NOTICE,
    }))
    .into_response())
}

/// `Destination\Show::submit`.
async fn update(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    Json(body): Json<Map<String, Value>>,
) -> ApiResult {
    let d = find(&state, &ctx, &uuid).await?;
    if !ctx.is_team_admin() {
        return toast_error("This action is unauthorized.");
    }
    let input = Input(body);
    validate(
        &input,
        &[Field {
            name: "name",
            rules: vec![Rule::String, Rule::Required],
        }],
        &[],
        &["name"],
    )?;
    let name = input.str("name").unwrap_or_default();
    if name != d.name {
        dest::update_name(&state.db, d.kind, d.id, name, state.now()).await?;
        tracing::info!(target: "audit", event = "ui.destination.updated", team_id = ctx.team_id, destination_uuid = %uuid, "destination updated");
    }
    Ok(ok(json!({"message": "Destination saved."})).into_response())
}

/// `Destination\Show::delete`.
async fn delete(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let d = find(&state, &ctx, &uuid).await?;
    if !ctx.is_team_admin() {
        return toast_error("This action is unauthorized.");
    }
    if d.kind == Kind::Standalone {
        if dest::attached_to(&state.db, d.kind, d.id).await? {
            return toast_error("You must delete all resources before deleting this destination.");
        }
        let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
        let row = servers::find_by_id(&state.db, d.server_id)
            .await?
            .ok_or(ApiError::Internal)?;
        let srv = Srv::from_row(jobs, row);
        if let Err(e) = proxy_ops::remove_standalone_network(&srv, &d).await {
            return toast_error(e);
        }
    }
    dest::delete(&state.db, d.kind, d.id).await?;
    tracing::info!(target: "audit", event = "ui.destination.deleted", team_id = ctx.team_id, destination_uuid = %uuid, "destination deleted");
    Ok(ok(json!({"message": "Deleted."})).into_response())
}

/// `Destination\Resources::loadResources`.
async fn resources(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let d = find(&state, &ctx, &uuid).await?;
    let mut rows = Vec::new();
    if d.kind == Kind::Standalone {
        for (table, kind) in [
            ("applications", "application"),
            ("services", "service"),
            ("standalone_postgresqls", "database"),
            ("standalone_redis", "database"),
            ("standalone_mongodbs", "database"),
            ("standalone_mysqls", "database"),
            ("standalone_mariadbs", "database"),
            ("standalone_keydbs", "database"),
            ("standalone_dragonflies", "database"),
            ("standalone_clickhouses", "database"),
            ("standalone_sqlites", "database"),
        ] {
            for r in coolify_db::ui_servers::destination_resources(
                &state.db,
                table,
                d.id,
                d.kind.class(),
            )
            .await?
            {
                let url = match (&r.project_uuid, &r.environment_uuid) {
                    (Some(p), Some(e)) => {
                        Some(format!("/project/{p}/environment/{e}/{kind}/{}", r.uuid))
                    }
                    _ => None,
                };
                let search: Vec<&str> = [
                    Some(kind),
                    r.name.as_deref(),
                    r.project_name.as_deref(),
                    r.environment_name.as_deref(),
                ]
                .into_iter()
                .flatten()
                .filter(|v| !v.is_empty())
                .collect();
                rows.push(json!({
                    "uuid": r.uuid,
                    "type": kind,
                    "name": r.name,
                    "project": r.project_name,
                    "environment": r.environment_name,
                    "url": url,
                    "search": search.join(" ").to_lowercase(),
                }));
            }
        }
    }
    Ok(ok(json!({"resources": rows})).into_response())
}
