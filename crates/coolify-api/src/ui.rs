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
        .route("/ui/servers", get(servers_data).post(create_server_form))
        .route("/ui/servers/form", get(server_form_data))
        .route("/ui/timezones", get(timezones))
        .route("/ui/servers/{uuid}/delete", post(delete_server_form))
        .route(
            "/ui/projects/{project}/environments/{env}/resources",
            get(crate::ui_databases::resources),
        )
        .route("/ui/destinations", get(crate::ui_databases::destinations))
        .route(
            "/ui/applications",
            post(crate::ui_applications::create_docker_image),
        )
        .route(
            "/ui/applications/{uuid}",
            axum::routing::patch(crate::ui_applications::update),
        )
        .route(
            "/ui/applications/{uuid}/state",
            get(crate::ui_applications::state),
        )
        .route(
            "/ui/applications/{uuid}/deploy",
            post(crate::ui_applications::deploy),
        )
        .route(
            "/ui/applications/{uuid}/restart",
            post(crate::ui_applications::restart),
        )
        .route(
            "/ui/applications/{uuid}/stop",
            post(crate::ui_applications::stop),
        )
        .route(
            "/ui/applications/{uuid}/settings",
            post(crate::ui_applications::settings),
        )
        .route(
            "/ui/applications/{uuid}/debug",
            post(crate::ui_applications::toggle_debug),
        )
        .route(
            "/ui/applications/{uuid}/delete",
            post(crate::ui_applications::delete),
        )
        .route(
            "/ui/applications/{uuid}/internal-hostname",
            get(crate::ui_applications::internal_hostname),
        )
        .route(
            "/ui/applications/{uuid}/deployments",
            get(crate::ui_applications::deployments),
        )
        .route(
            "/ui/applications/{uuid}/deployments/{dep}",
            get(crate::ui_applications::deployment),
        )
        .route(
            "/ui/applications/{uuid}/deployments/{dep}/cancel",
            post(crate::ui_applications::cancel),
        )
        .route("/ui/databases", post(crate::ui_databases::create))
        .route(
            "/ui/databases/{uuid}",
            axum::routing::patch(crate::ui_databases::update),
        )
        .route(
            "/ui/databases/{uuid}/state",
            get(crate::ui_databases::state),
        )
        .route(
            "/ui/databases/{uuid}/delete",
            post(crate::ui_databases::delete),
        )
        .route(
            "/ui/servers/{uuid}/validation",
            get(validation_state).post(start_validation),
        )
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

/// Server facts the server pages need (`Server::isFunctional()` and friends).
fn server_facts(s: &coolify_db::servers::ServerRow) -> Value {
    let settings = s.settings.clone().unwrap_or_default();
    let flag = |k: &str| settings.get(k).and_then(Value::as_bool).unwrap_or(false);
    let transferred = s
        .record
        .get("server_metadata")
        .and_then(|m| m.pointer("/transfer/status"))
        .and_then(Value::as_str)
        == Some("transferred");
    let functional =
        flag("is_reachable") && flag("is_usable") && !flag("force_disabled") && !transferred;
    let is_build = crate::routes::servers::effective_role_is_build(&settings);
    let proxy_type = s
        .record
        .get("proxy")
        .and_then(|p| p.get("type"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let proxy_status = s
        .record
        .get("proxy")
        .and_then(|p| p.get("status"))
        .and_then(Value::as_str);
    let proxy_set = !proxy_type.is_empty()
        && proxy_type != "NONE"
        && functional
        && !flag("is_swarm_worker")
        && !is_build;
    let (status, status_type) = if transferred {
        ("Transferred away", "error")
    } else if flag("force_disabled") {
        ("Disabled", "error")
    } else if functional && proxy_set && proxy_status != Some("running") {
        ("Attention required", "warning")
    } else if functional {
        ("Ready", "success")
    } else {
        ("Validation required", "error")
    };
    let ip = s.record.get("ip").and_then(Value::as_str).unwrap_or("");
    let cloud = [
        "hetzner_server_id",
        "vultr_instance_id",
        "digitalocean_droplet_id",
        "hostinger_virtual_machine_id",
    ]
    .iter()
    .any(|k| s.record.get(*k).is_some_and(crate::serialize::php_truthy));
    json!({
        "uuid": s.uuid,
        "name": s.record.get("name"),
        "description": s.record.get("description"),
        "status": status,
        "status_type": status_type,
        "functional": functional,
        "transferred": transferred,
        "is_coolify_host": s.id == 0,
        "is_localhost": ip == "host.docker.internal" || s.id == 0,
        "is_build": is_build,
        "is_swarm": flag("is_swarm_manager") || flag("is_swarm_worker"),
        "is_swarm_worker": flag("is_swarm_worker"),
        "proxy_set": proxy_set,
        "proxy_status": proxy_status,
        "cloud": cloud,
    })
}

/// Servers index (`Livewire\Server\Index`) and the server switcher.
async fn servers_data(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
) -> ApiResult {
    let rows = coolify_db::servers::list_by_name(&state.db, ctx.team_id).await?;
    let mut items = Vec::with_capacity(rows.len());
    for s in &rows {
        let mut v = server_facts(s);
        v["resources"] = json!(coolify_db::servers::resource_count(&state.db, s.id).await?);
        items.push(v);
    }
    let (two_step, password) = confirmation_steps(&state, ctx.user_id).await?;
    Ok(ok(json!({
        "servers": items,
        "can_create": ctx.is_team_admin(),
        "confirm": {"text": two_step, "password": password},
    }))
    .into_response())
}

/// `Livewire\Server\New\ByIp::mount`: SSH keys to pick from and a generated name.
async fn server_form_data(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
) -> ApiResult {
    let keys = coolify_db::servers::ssh_keys_for_form(&state.db, ctx.team_id).await?;
    Ok(ok(json!({
        "keys": keys.iter().map(|(id, name)| json!({"id": id, "name": name})).collect::<Vec<_>>(),
        "name": crate::routes::private_keys::random_name(),
    }))
    .into_response())
}

const BY_IP_MESSAGES: crate::validation::Messages = &[
    (
        "private_key_id.integer",
        "The Private Key field must be an integer.",
    ),
    ("ip.required", "The IP Address/Domain is required."),
    ("ip.string", "The IP Address/Domain must be a string."),
    ("user.required", "The User field is required."),
    ("user.string", "The User field must be a string."),
    (
        "user.regex",
        "The User may only contain letters, numbers, dots, hyphens, and underscores.",
    ),
    ("port.required", "The Port field is required."),
    ("port.integer", "The Port field must be an integer."),
    (
        "port.between",
        "The Port field must be between 1 and 65535.",
    ),
    ("server_role.required", "The Server Role field is required."),
    ("server_role.in", "The selected Server Role is invalid."),
    (
        "name.regex",
        "The name may only contain letters (including Unicode), numbers, spaces, and these characters: - _ . / @ & ( ) # , : +",
    ),
    ("name.min", "The name must be at least :min characters."),
    (
        "name.max",
        "The name may not be greater than :max characters.",
    ),
    (
        "description.regex",
        "The description may only contain letters (including Unicode), numbers, spaces, and common punctuation: - _ . , ! ? ( ) ' \" + = * / @ &",
    ),
    (
        "description.max",
        "The description may not be greater than :max characters.",
    ),
];

static SSH_USER: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"^[a-zA-Z0-9._-]+\n?\z").unwrap_or_else(|_| unreachable!())
});

/// `Livewire\Server\New\ByIp::submit`.
async fn create_server_form(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    peer: Option<Extension<axum::extract::ConnectInfo<std::net::SocketAddr>>>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> ApiResult {
    use crate::routes::servers::{
        as_php_string, normalize_ip, sentinel_url, user_accessor, valid_server_ip,
    };
    use crate::validation::{Field, Rule, patterns, validate};
    use axum::http::StatusCode;
    let input: Value =
        serde_json::from_slice(&body).map_err(|_| ApiError::InvalidRequest("Invalid JSON."))?;
    let Value::Object(map) = input else {
        return Err(ApiError::InvalidRequest("Invalid JSON."));
    };
    let map = map
        .into_iter()
        .map(|(k, v)| match v {
            Value::String(s) if s.trim().is_empty() => (k, Value::Null),
            v => (k, v),
        })
        .collect();
    let input = crate::input::Input(map);
    let fields = vec![
        Field {
            name: "private_key_id",
            rules: vec![Rule::Nullable, Rule::Integer],
        },
        patterns::name(true),
        patterns::description(),
        Field {
            name: "ip",
            rules: vec![Rule::Required, Rule::String, Rule::Custom(valid_server_ip)],
        },
        Field {
            name: "user",
            rules: vec![Rule::Required, Rule::String, Rule::Regex(&SSH_USER)],
        },
        Field {
            name: "port",
            rules: vec![Rule::Required, Rule::Integer, Rule::Between(1, 65535)],
        },
        Field {
            name: "server_role",
            rules: vec![Rule::Required, Rule::In(&["deployment", "build", "both"])],
        },
    ];
    let allowed: Vec<&str> = input.keys().map(String::as_str).collect();
    validate(&input, &fields, BY_IP_MESSAGES, &allowed)?;
    let key_id = input.get("private_key_id").and_then(Value::as_i64);
    if let Some(id) = key_id
        && !coolify_db::private_keys::exists_in_team(&state.db, ctx.team_id, id).await?
    {
        let mut errors = serde_json::Map::new();
        errors.insert(
            "private_key_id".into(),
            json!(["The selected Private Key is invalid."]),
        );
        return Err(ApiError::Validation(errors));
    }
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let ip = normalize_ip(&input.get("ip").map(as_php_string).unwrap_or_default());
    if coolify_db::servers::ip_in_use(&state.db, &ip, None).await? {
        return Err(ApiError::Message(
            StatusCode::UNPROCESSABLE_ENTITY,
            "A server with this IP/Domain already exists.",
        ));
    }
    let Some(key_id) = key_id else {
        return Err(ApiError::Message(
            StatusCode::UNPROCESSABLE_ENTITY,
            "You must select a private key",
        ));
    };
    let enc = state.encrypter.as_ref().ok_or(ApiError::Message(
        StatusCode::SERVICE_UNAVAILABLE,
        "APP_KEY is not configured; servers cannot be created.",
    ))?;
    let uuid = coolify_domain::new_public_id();
    let name = coolify_domain::naming::strip_tags(input.str("name"));
    let role: &'static str = match input.str("server_role") {
        Some("deployment") => "deployment",
        Some("build") => "build",
        _ => "both",
    };
    let peer = peer
        .map(|c| (c.0).0)
        .unwrap_or_else(|| std::net::SocketAddr::from(([127, 0, 0, 1], 0)));
    let token =
        enc.encrypt_string(&enc.encrypt_serialized(&json!({"server_uuid": uuid}).to_string()));
    let new = coolify_db::servers::NewServer {
        sentinel_custom_url: sentinel_url(&state, &ip, peer, &headers).await?,
        sentinel_token_encrypted: token,
        shared_vars: [
            ("COOLIFY_SERVER_UUID", enc.encrypt_string(&uuid)),
            ("COOLIFY_SERVER_NAME", enc.encrypt_string(&name)),
        ],
        uuid,
        name,
        description: coolify_domain::naming::strip_tags(input.str("description")),
        ip,
        port: input
            .get("port")
            .map(|v| as_php_string(v).trim().parse::<i64>().unwrap_or(22))
            .unwrap_or(22),
        user: user_accessor(
            input
                .get("user")
                .map(as_php_string)
                .unwrap_or_default()
                .trim(),
        ),
        private_key_id: key_id,
        team_id: ctx.team_id,
        // ByIp sets status before type.
        proxy: json!({"redirect_enabled": true, "status": "exited", "type": "TRAEFIK"}).to_string(),
        docker_uuid: coolify_domain::new_public_id(),
        server_role: role,
        always_write_role: false,
    };
    coolify_db::servers::create(&state.db, &new, state.now()).await?;
    Ok(crate::response::created(json!({"uuid": new.uuid})).into_response())
}

/// `modal-confirmation` steps: the type-the-name step unless
/// `disable_two_step_confirmation`; the password step unless that, or the user
/// has no password or signs in through SSO (`shouldSkipPasswordConfirmation`;
/// coolify-rs keeps no "recently confirmed" session flag).
pub(crate) async fn confirmation_steps(
    state: &AppState,
    user_id: i64,
) -> Result<(bool, bool), ApiError> {
    let disabled = coolify_db::users::two_step_confirmation_disabled(&state.db).await?;
    if disabled {
        return Ok((false, false));
    }
    let has_password = coolify_db::users::get(&state.db, user_id)
        .await?
        .and_then(|u| u.password)
        .is_some_and(|p| !p.is_empty());
    let sso = coolify_db::users::has_sso_identity(&state.db, user_id).await?;
    Ok((true, has_password && !sso))
}

/// `verifyPasswordConfirmation`.
pub(crate) async fn password_confirmed(
    state: &AppState,
    user_id: i64,
    password: Option<&str>,
) -> Result<bool, ApiError> {
    let (_, required) = confirmation_steps(state, user_id).await?;
    if !required {
        return Ok(true);
    }
    let hash = coolify_db::users::get(&state.db, user_id)
        .await?
        .and_then(|u| u.password);
    Ok(match (password, hash) {
        (Some(p), Some(h)) => coolify_compat::password::verify(p, &h),
        _ => false,
    })
}

/// `Livewire\Server\Delete::delete($password, $selectedActions)`.
/// `Livewire\Server\Show::validateServer` followed by `ValidateAndInstall`
/// without installing: runs the validation job with the component's wording.
async fn start_validation(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    use axum::http::StatusCode;
    let server = coolify_db::servers::find(&state.db, ctx.team_id, &uuid)
        .await?
        .ok_or(ApiError::Message(
            StatusCode::NOT_FOUND,
            "Server not found.",
        ))?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    if crate::routes::servers::is_transferred(&server) {
        return Err(ApiError::Message(
            StatusCode::UNPROCESSABLE_ENTITY,
            "This server was transferred to another Coolify instance. Manage it from the target instance instead.",
        ));
    }
    // Refreshing a cloud provider's state first is not ported.
    let linked = [
        "vultr_instance_id",
        "digitalocean_droplet_id",
        "hostinger_virtual_machine_id",
    ]
    .iter()
    .any(|k| {
        server
            .record
            .get(*k)
            .is_some_and(crate::serialize::php_truthy)
    });
    let Some(jobs) = state.jobs.as_ref().filter(|_| !linked) else {
        return Err(ApiError::NotPorted);
    };
    let started = jobs.spawn_validation(
        server.id,
        server.team_id,
        server.uuid.clone(),
        coolify_jobs::Wording::Ui,
    );
    tracing::info!(target: "audit", event = "server.validated", team_id = ctx.team_id, server_uuid = %server.uuid, "server validation started");
    Ok(ok(json!({"started": started})).into_response())
}

/// Checkpoints of the latest validation and the stored `validation_logs`.
async fn validation_state(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    use axum::http::StatusCode;
    let server = coolify_db::servers::find(&state.db, ctx.team_id, &uuid)
        .await?
        .ok_or(ApiError::Message(
            StatusCode::NOT_FOUND,
            "Server not found.",
        ))?;
    let progress = state.jobs.as_ref().and_then(|j| j.progress(server.id));
    Ok(ok(json!({
        "progress": progress,
        "validation_logs": server.record.get("validation_logs"),
        "available": state.jobs.is_some(),
    }))
    .into_response())
}

async fn delete_server_form(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    body: axum::body::Bytes,
) -> ApiResult {
    use axum::http::StatusCode;
    let input: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let server = coolify_db::servers::find(&state.db, ctx.team_id, &uuid)
        .await?
        .ok_or(ApiError::Message(
            StatusCode::NOT_FOUND,
            "Server not found.",
        ))?;
    if !password_confirmed(
        &state,
        ctx.user_id,
        input.get("password").and_then(Value::as_str),
    )
    .await?
    {
        return Err(ApiError::Message(
            StatusCode::UNPROCESSABLE_ENTITY,
            "The provided password is incorrect.",
        ));
    }
    let actions: Vec<&str> = input
        .get("actions")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    if server.id == 0 {
        return Err(ApiError::Message(
            StatusCode::UNPROCESSABLE_ENTITY,
            "The Coolify host server cannot be deleted.",
        ));
    }
    let transferred = server_facts(&server)["transferred"]
        .as_bool()
        .unwrap_or(false);
    let resources = coolify_db::servers::resource_count(&state.db, server.id).await?;
    let force = actions.contains(&"force_delete_resources");
    if resources > 0 && !force && !transferred {
        return Err(ApiError::Message(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Server has defined resources. Please delete them first or select \"Delete all resources\".",
        ));
    }
    let from_provider = actions.iter().any(|a| a.starts_with("delete_from_"));
    // Archiving a transferred server, deleting resources, cloud servers,
    // queued deployments' logs and GitHub runners are not ported.
    if transferred
        || resources > 0
        || from_provider
        || coolify_db::servers::has_unported_delete_side_effects(&state.db, server.id).await?
    {
        return Err(ApiError::NotPorted);
    }
    coolify_db::servers::delete(&state.db, server.id, state.now()).await?;
    Ok(ok(json!({"message": "Server deleted."})).into_response())
}

/// `timezone_identifiers_list()` sorted (server timezone pickers).
async fn timezones() -> ApiResult {
    let mut zones: Vec<&str> = coolify_compat::timezones::PHP_TIMEZONES.to_vec();
    zones.sort_unstable();
    Ok(ok(json!(zones)).into_response())
}
