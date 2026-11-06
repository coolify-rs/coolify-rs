//! Plugin registry endpoints (ADR 0010, section 7): the in-app catalogue of the signed index, installing from
//! it, and updates that need an admin's approval when they ask for more. Like `routes/plugins.rs` these are
//! coolify-rs's own; none is in the upstream route inventory. docs/PLUGINS.md documents the JSON.
//!
//! - `GET /api/v1/plugins/catalog`: the last verified index, with what this instance has installed.
//! - `POST /api/v1/plugins/catalog/refresh`: fetch and verify the index now (instance admins).
//! - `POST /api/v1/plugins/catalog/install` `{id, version?, grant}` (instance admins). Without the exact
//!   capability list as `grant` it answers 422 with the list, and downloads nothing.
//! - `POST /api/v1/plugins/{publisher}/{name}/update` `{version?, grant?}` (instance admins). A version
//!   asking for more than was granted answers 422 with what it adds, and is recorded as a pending update.

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::middleware::{from_fn, from_fn_with_state};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Extension, Router};
use coolify_domain::Ability;
use coolify_plugins::registry::{self, RegistryError};
use coolify_plugins::{Host, Installed, PluginError};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::auth::AuthCtx;
use crate::response::{ApiError, ApiResult, created, ok};
use crate::state::AppState;

const READ: &[&[Ability]] = &[&[Ability::Read]];
const WRITE: &[&[Ability]] = &[&[Ability::Write]];

pub fn routes(state: AppState) -> Router<AppState> {
    let guard = from_fn_with_state(state.clone(), crate::auth::main_guard);
    let read = || from_fn(|req, next| crate::auth::require_abilities(READ, req, next));
    let write = || from_fn(|req, next| crate::auth::require_abilities(WRITE, req, next));
    Router::new()
        .route("/api/v1/plugins/catalog", get(catalog).route_layer(read()))
        .route(
            "/api/v1/plugins/catalog/refresh",
            post(refresh).route_layer(write()),
        )
        .route(
            "/api/v1/plugins/catalog/install",
            post(install).route_layer(write()),
        )
        .route(
            "/api/v1/plugins/{publisher}/{name}/update",
            post(update).route_layer(write()),
        )
        .route_layer(guard)
}

fn host(state: &AppState) -> Result<&Host, ApiError> {
    state.plugins.as_ref().ok_or(ApiError::Status(
        StatusCode::NOT_FOUND,
        json!({"message": "Plugins are switched off on this instance."}),
    ))
}

/// The rule of `routes/plugins.rs`: owners and admins of the root team.
fn instance_admin(ctx: &AuthCtx) -> Result<(), ApiError> {
    if ctx.team_id == 0 && ctx.is_team_admin() {
        Ok(())
    } else {
        Err(ApiError::Status(
            StatusCode::FORBIDDEN,
            json!({"message": "Only instance admins can manage plugins."}),
        ))
    }
}

fn internal(e: impl std::fmt::Display) -> ApiError {
    tracing::error!(error = %e, "plugin registry request failed");
    ApiError::Internal
}

fn status(code: StatusCode, body: Value) -> ApiError {
    ApiError::Status(code, body)
}

fn registry_error(e: RegistryError) -> ApiError {
    let message = e.to_string();
    match e {
        RegistryError::GrantRequired {
            id,
            version,
            capabilities,
            added,
            pending,
        } => status(
            StatusCode::UNPROCESSABLE_ENTITY,
            json!({
                "message": "Approve the plugin's capabilities by sending them back as grant.",
                "id": id,
                "version": version,
                "capabilities": capabilities,
                "added": added,
                "pending_update": pending,
            }),
        ),
        RegistryError::Revoked { id, reason }
        | RegistryError::Plugin(PluginError::Revoked { id, reason }) => status(
            StatusCode::UNPROCESSABLE_ENTITY,
            json!({"message": message, "id": id, "reason": reason}),
        ),
        RegistryError::Plugin(PluginError::Db(e)) => internal(e),
        RegistryError::NoIndex
        | RegistryError::AlreadyInstalled(_)
        | RegistryError::UpToDate { .. } => {
            status(StatusCode::CONFLICT, json!({"message": message}))
        }
        RegistryError::Unknown(_)
        | RegistryError::NotInstalled(_)
        | RegistryError::NoSuchVersion { .. } => {
            status(StatusCode::NOT_FOUND, json!({"message": message}))
        }
        RegistryError::Fetch { .. }
        | RegistryError::Verify(_)
        | RegistryError::Integrity { .. } => {
            tracing::warn!(error = %message, "plugin registry");
            status(StatusCode::BAD_GATEWAY, json!({"message": message}))
        }
        RegistryError::Yanked { .. }
        | RegistryError::Incompatible { .. }
        | RegistryError::NoInstallableVersion(_)
        | RegistryError::Plugin(_) => status(
            StatusCode::UNPROCESSABLE_ENTITY,
            json!({"message": message}),
        ),
    }
}

/// Same shape as `GET /api/v1/plugins/{publisher}/{name}`.
async fn present(host: &Host, p: &Installed, team_id: i64) -> ApiResult<Value> {
    let on = coolify_plugins::store::enabled_for_team(host.db(), &p.id, team_id)
        .await
        .map_err(internal)?;
    let m = &p.manifest;
    Ok(json!({
        "id": p.id,
        "name": m.name,
        "version": p.version,
        "description": m.description,
        "license": m.license,
        "source": m.source,
        "sha256": p.sha256,
        "origin": p.origin,
        "verified": p.verified,
        "enabled": p.enabled,
        "disabled_reason": p.disabled_reason,
        "capabilities": m.capability_summary(),
        "enabled_for_team": on,
    }))
}

/// A JSON body, or the default when the body is empty.
fn body<T: serde::de::DeserializeOwned + Default>(bytes: &Bytes) -> Result<T, ApiError> {
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(T::default());
    }
    serde_json::from_slice(bytes).map_err(|e| {
        status(
            StatusCode::UNPROCESSABLE_ENTITY,
            json!({"message": format!("Invalid JSON body: {e}")}),
        )
    })
}

async fn catalog(State(state): State<AppState>) -> ApiResult {
    let host = host(&state)?;
    let catalog = registry::catalog(host).await.map_err(registry_error)?;
    Ok(ok(serde_json::to_value(catalog).map_err(internal)?).into_response())
}

async fn refresh(State(state): State<AppState>, Extension(ctx): Extension<AuthCtx>) -> ApiResult {
    instance_admin(&ctx)?;
    let host = host(&state)?;
    let refreshed = registry::refresh(host).await.map_err(registry_error)?;
    Ok(ok(serde_json::to_value(refreshed).map_err(internal)?).into_response())
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct InstallBody {
    #[serde(default)]
    id: String,
    #[serde(default)]
    version: Option<String>,
    /// The capability list the admin approved; must equal the version's.
    #[serde(default)]
    grant: Option<Vec<String>>,
}

async fn install(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    bytes: Bytes,
) -> ApiResult {
    instance_admin(&ctx)?;
    let host = host(&state)?;
    let b: InstallBody = body(&bytes)?;
    if b.id.is_empty() {
        return Err(status(
            StatusCode::UNPROCESSABLE_ENTITY,
            json!({"message": "id is required."}),
        ));
    }
    let installed = registry::install(
        host,
        &b.id,
        b.version.as_deref(),
        b.grant.as_deref(),
        Some(ctx.user_id),
    )
    .await
    .map_err(registry_error)?;
    Ok(created(present(host, &installed, ctx.team_id).await?).into_response())
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct UpdateBody {
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    grant: Option<Vec<String>>,
}

async fn update(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((publisher, name)): Path<(String, String)>,
    bytes: Bytes,
) -> ApiResult {
    instance_admin(&ctx)?;
    let host = host(&state)?;
    let b: UpdateBody = body(&bytes)?;
    let id = format!("{publisher}/{name}");
    let installed = registry::update(
        host,
        &id,
        b.version.as_deref(),
        b.grant.as_deref(),
        Some(ctx.user_id),
    )
    .await
    .map_err(registry_error)?;
    Ok(ok(present(host, &installed, ctx.team_id).await?).into_response())
}
