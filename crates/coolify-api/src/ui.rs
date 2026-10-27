//! Read models for the web UI that the REST API has no endpoint for.
//! Session-guarded like the `/ui/api/v1` bridge.

use axum::Extension;
use axum::Router;
use axum::extract::{Path, State};
use axum::middleware::from_fn_with_state;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use coolify_db::dashboard::{self, DashServer};
use serde_json::{Value, json};

use crate::auth::AuthCtx;
use crate::response::{ApiError, ApiResult, ok};
use crate::state::AppState;

pub fn routes(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/ui/dashboard", get(dashboard_data))
        .route("/ui/projects/{uuid}", get(project_data))
        .route("/ui/keys", get(keys_data))
        .route("/ui/keys/generate", post(generate_key))
        .route("/ui/keys/cleanup", post(cleanup_keys))
        .route("/ui/keys/inspect", post(inspect_key))
        .route("/ui/team", get(team_data).patch(update_team))
        .route(
            "/ui/team/members/{id}",
            axum::routing::delete(remove_member),
        )
        .route("/ui/team/members/{id}/role", post(set_member_role))
        .route_layer(from_fn_with_state(state, crate::session::ui_guard))
}

/// `$serverStatuses` in upstream livewire/dashboard.blade.php, without the
/// proxy-version and Sentinel checks (coolify-rs does not track either).
fn server_status(s: &DashServer) -> (&'static str, &'static str) {
    let functional = s.is_reachable && s.is_usable && !s.force_disabled;
    let proxy_set = functional
        && s.proxy_type
            .as_deref()
            .is_some_and(|t| !t.is_empty() && t != "NONE");
    match () {
        _ if s.force_disabled => ("Disabled", "error"),
        _ if !s.is_reachable && !s.is_usable => ("Unavailable", "error"),
        _ if !s.is_reachable => ("Unreachable", "error"),
        _ if !s.is_usable => ("Not ready", "warning"),
        _ if proxy_set && s.proxy_status.as_deref() != Some("running") => {
            ("Attention required", "warning")
        }
        _ => ("Ready", "success"),
    }
}

async fn dashboard_data(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
) -> ApiResult {
    let projects = dashboard::projects(&state.db, ctx.team_id).await?;
    let servers = dashboard::servers(&state.db, ctx.team_id).await?;
    let private_keys = dashboard::private_key_count(&state.db, ctx.team_id).await?;
    let projects: Vec<Value> = projects
        .iter()
        .map(|p| {
            json!({
                "uuid": p.uuid,
                "name": p.name,
                "description": p.description,
                "created_at": p.created_at,
                "environments": p.environments.iter().map(|(uuid, name)| json!({"uuid": uuid, "name": name})).collect::<Vec<_>>(),
                "resources": p.resources,
            })
        })
        .collect();
    let servers: Vec<Value> = servers
        .iter()
        .map(|s| {
            let (status, status_type) = server_status(s);
            json!({
                "uuid": s.uuid,
                "name": s.name,
                "description": s.description,
                "status": status,
                "status_type": status_type,
            })
        })
        .collect();
    Ok(ok(json!({
        "projects": projects,
        "servers": servers,
        "private_keys": private_keys,
        "can_create": ctx.role.is_admin(),
    }))
    .into_response())
}

/// Project page and breadcrumb data: the project, its environments with
/// resource counts, and the team's projects for the switcher.
async fn project_data(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let project = coolify_db::projects::find(&state.db, ctx.team_id, &uuid)
        .await?
        .ok_or(ApiError::Message(
            axum::http::StatusCode::NOT_FOUND,
            "Project not found.",
        ))?;
    let environments = dashboard::environments(&state.db, project.id).await?;
    let projects = dashboard::project_names(&state.db, ctx.team_id).await?;
    Ok(ok(json!({
        "project": {"uuid": project.uuid, "name": project.name, "description": project.description},
        "environments": environments.iter().map(|e| json!({
            "uuid": e.uuid,
            "name": e.name,
            "description": e.description,
            "created_at": e.created_at,
            "resources": e.resources,
        })).collect::<Vec<_>>(),
        "projects": projects.iter().map(|(uuid, name)| json!({"uuid": uuid, "name": name})).collect::<Vec<_>>(),
        "can_update": ctx.role.is_admin(),
        "can_create": ctx.role.is_admin(),
    }))
    .into_response())
}

async fn keys_data(State(state): State<AppState>, Extension(ctx): Extension<AuthCtx>) -> ApiResult {
    Ok(ok(crate::routes::private_keys::ui_list(&state, &ctx).await?).into_response())
}

async fn generate_key(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    body: axum::body::Bytes,
) -> ApiResult {
    let input: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let kind = match input.get("type").and_then(Value::as_str) {
        Some("ed25519") => coolify_compat::sshkey::KeyKind::Ed25519,
        Some("rsa") => coolify_compat::sshkey::KeyKind::Rsa,
        _ => return Err(ApiError::InvalidRequest("Invalid private key type.")),
    };
    let uuid = crate::routes::private_keys::ui_generate(&state, &ctx, kind).await?;
    Ok(crate::response::created(json!({"uuid": uuid})).into_response())
}

async fn cleanup_keys(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
) -> ApiResult {
    let deleted = crate::routes::private_keys::ui_cleanup(&state, &ctx).await?;
    Ok(ok(json!({"deleted": deleted})).into_response())
}

/// Public key of a pasted private key, for the create form's read-only
/// "Public key" field (upstream derives it with Livewire realtime validation).
async fn inspect_key(body: axum::body::Bytes) -> ApiResult {
    let input: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let key = input
        .get("private_key")
        .and_then(Value::as_str)
        .unwrap_or("");
    let public = coolify_compat::sshkey::inspect(key).map(|i| i.public_key);
    Ok(ok(json!({"public_key": public})).into_response())
}

/// `Livewire\Team\Index::submit`: name, description and the three team
/// switches, with upstream's validation and the 2FA self-check.
async fn update_team(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    body: axum::body::Bytes,
) -> ApiResult {
    use crate::validation::{Field, Rule, patterns, validate};
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let input: Value =
        serde_json::from_slice(&body).map_err(|_| ApiError::InvalidRequest("Invalid JSON."))?;
    let Value::Object(map) = input else {
        return Err(ApiError::InvalidRequest("Invalid JSON."));
    };
    // Laravel skips every rule but the implicit ones for an empty string,
    // which `required` then rejects; null gives the same result here.
    let map = map
        .into_iter()
        .map(|(k, v)| match v {
            Value::String(s) if s.trim().is_empty() => (k, Value::Null),
            v => (k, v),
        })
        .collect();
    let input = crate::input::Input(map);
    let flags = [
        "is_mcp_server_enabled",
        "is_build_server_fallback_enabled",
        "is_two_factor_required",
    ];
    let mut fields = vec![patterns::name(true), patterns::description()];
    fields.extend(flags.iter().map(|f| Field {
        name: f,
        rules: vec![Rule::Required, Rule::Boolean],
    }));
    let allowed: Vec<&str> = input.keys().map(String::as_str).collect();
    validate(&input, &fields, patterns::TEAM_MESSAGES, &allowed)?;
    let flag = |f: &str| matches!(input.get(f), Some(Value::Bool(true)));
    let team = coolify_db::teams::get(&state.db, ctx.team_id)
        .await?
        .ok_or(ApiError::Internal)?;
    let currently_required = team
        .get("is_two_factor_required")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if flag("is_two_factor_required") && !currently_required {
        let me = coolify_db::users::get(&state.db, ctx.user_id).await?;
        if me.and_then(|u| u.two_factor_confirmed_at).is_none() {
            return Err(ApiError::Message(
                axum::http::StatusCode::UNPROCESSABLE_ENTITY,
                "Enable two-factor authentication on your profile before you require it for the team.",
            ));
        }
    }
    coolify_db::teams::update_settings(
        &state.db,
        ctx.team_id,
        &coolify_db::teams::TeamSettings {
            name: input.str("name").unwrap_or_default().to_string(),
            description: input.str("description").map(str::to_string),
            is_mcp_server_enabled: flag("is_mcp_server_enabled"),
            is_build_server_fallback_enabled: flag("is_build_server_fallback_enabled"),
            is_two_factor_required: flag("is_two_factor_required"),
        },
        state.now(),
    )
    .await?;
    Ok(ok(json!({"message": "Team updated."})).into_response())
}

/// Team pages: the team's settings and members (with 2FA state for admins).
async fn team_data(State(state): State<AppState>, Extension(ctx): Extension<AuthCtx>) -> ApiResult {
    let team = coolify_db::teams::get(&state.db, ctx.team_id)
        .await?
        .ok_or(ApiError::Internal)?;
    let flag = |k: &str, default: bool| team.get(k).and_then(Value::as_bool).unwrap_or(default);
    let members = coolify_db::teams::member_rows(&state.db, ctx.team_id).await?;
    let manage = ctx.is_team_admin();
    Ok(ok(json!({
        "team": {
            "id": ctx.team_id,
            "name": team.get("name"),
            "description": team.get("description"),
            "is_mcp_server_enabled": flag("is_mcp_server_enabled", true),
            "is_build_server_fallback_enabled": flag("is_build_server_fallback_enabled", true),
            "is_two_factor_required": flag("is_two_factor_required", false),
        },
        "members": members.iter().map(|m| json!({
            "id": m.id,
            "name": m.name,
            "email": m.email,
            "role": m.role,
            "two_factor": if manage { Value::Bool(m.two_factor) } else { Value::Null },
        })).collect::<Vec<_>>(),
        "me": ctx.user_id,
        "my_role": ctx.role.as_str(),
        "can_update": manage,
        "can_manage_members": manage,
    }))
    .into_response())
}

/// `Team\Member` guard: the actor needs `min` and may not act on a member
/// whose role is above their own.
async fn member_guard(
    state: &AppState,
    ctx: &AuthCtx,
    member_id: i64,
    min: coolify_domain::Role,
) -> Result<(), ApiError> {
    const DENIED: ApiError = ApiError::Message(
        axum::http::StatusCode::FORBIDDEN,
        "You are not authorized to perform this action.",
    );
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let member_role = coolify_db::auth::membership_role(&state.db, ctx.team_id, member_id)
        .await?
        .ok_or(ApiError::Message(
            axum::http::StatusCode::NOT_FOUND,
            "Member not found.",
        ))?;
    if ctx.role < min || member_role > ctx.role {
        return Err(DENIED);
    }
    Ok(())
}

async fn set_member_role(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(id): Path<i64>,
    body: axum::body::Bytes,
) -> ApiResult {
    use coolify_domain::Role;
    let input: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let role = input
        .get("role")
        .and_then(Value::as_str)
        .and_then(Role::parse)
        .ok_or(ApiError::InvalidRequest("Invalid role."))?;
    // makeOwner needs an owner; makeAdmin and makeReadonly need an admin.
    let min = if role == Role::Owner {
        Role::Owner
    } else {
        Role::Admin
    };
    member_guard(&state, &ctx, id, min).await?;
    coolify_db::teams::set_member_role(&state.db, ctx.team_id, id, role.as_str()).await?;
    Ok(ok(json!({"message": "Member updated."})).into_response())
}

async fn remove_member(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(id): Path<i64>,
) -> ApiResult {
    member_guard(&state, &ctx, id, coolify_domain::Role::Admin).await?;
    coolify_db::teams::remove_member(&state.db, ctx.team_id, id).await?;
    Ok(ok(json!({"message": "Member removed."})).into_response())
}
