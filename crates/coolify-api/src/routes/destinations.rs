//! `DestinationsController`, `GET /resources`, `GET /servers/{uuid}/resources`
//! and `GET /servers/{uuid}/domains`.

use std::sync::LazyLock;

use axum::Extension;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::IntoResponse;
use coolify_db::destinations::{self as dest, Destination, Kind};
use coolify_db::servers;
use coolify_domain::naming::strip_tags;
use coolify_domain::new_public_id;
use coolify_jobs::server_ops::Srv;
use coolify_jobs::server_ops::proxy as proxy_ops;
use regex::Regex;
use serde_json::{Map, Value, json};

use crate::auth::AuthCtx;
use crate::input::{query_param, validated_json};
use crate::response::{ApiError, ApiResult, ok};
use crate::routes::server_settings::{
    authorize_update, find_server, json_status, validation_errors,
};
use crate::routes::servers::as_php_string;
use crate::serialize::{php_truthy, serialize_record};
use crate::state::AppState;
use crate::validation::{CUSTOM_API_MESSAGES, Field, Rule, validate};

/// The JSON body of Laravel's handler for `firstOrFail()`.
fn model_not_found(model: &'static str) -> ApiError {
    ApiError::Message(StatusCode::NOT_FOUND, model)
}

const NO_SWARM_DOCKER: &str = "No query results for model [App\\Models\\SwarmDocker].";
const NO_SERVER: &str = "No query results for model [App\\Models\\Server].";

pub async fn index(State(state): State<AppState>, Extension(ctx): Extension<AuthCtx>) -> ApiResult {
    let rows = dest::list_for_team(&state.db, ctx.team_id).await?;
    Ok(ok(Value::Array(
        rows.iter().map(Destination::transform).collect(),
    ))
    .into_response())
}

pub async fn index_by_server(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let s = servers::find(&state.db, ctx.team_id, &uuid)
        .await?
        .ok_or_else(|| model_not_found(NO_SERVER))?;
    let rows = dest::list_for_server(&state.db, s.id).await?;
    Ok(ok(Value::Array(
        rows.iter().map(Destination::transform).collect(),
    ))
    .into_response())
}

async fn find(state: &AppState, ctx: &AuthCtx, uuid: &str) -> Result<Destination, ApiError> {
    dest::find_in_team(&state.db, ctx.team_id, uuid)
        .await?
        .ok_or_else(|| model_not_found(NO_SWARM_DOCKER))
}

pub async fn show(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let d = find(&state, &ctx, &uuid).await?;
    Ok(ok(d.transform()).into_response())
}

static NETWORK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[a-zA-Z0-9][a-zA-Z0-9._-]*\n?\z").unwrap_or_else(|_| unreachable!())
});

pub async fn create(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(server_uuid): Path<String>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let input = validated_json(&headers, &uri, &body)?;
    let s = servers::find(&state.db, ctx.team_id, &server_uuid)
        .await?
        .ok_or_else(|| model_not_found(NO_SERVER))?;
    validate(
        &input,
        &[
            Field {
                name: "name",
                rules: vec![Rule::Nullable, Rule::String, Rule::Max(255)],
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
            Field {
                name: "type",
                rules: vec![Rule::Nullable, Rule::In(&["standalone", "swarm"])],
            },
        ],
        CUSTOM_API_MESSAGES,
        &["name", "network", "type"],
    )?;
    let network = input.str("network").unwrap_or_default().to_string();
    if ["bridge", "host", "none"].contains(&network.to_lowercase().as_str()) {
        return Err(validation_errors(&[(
            "network",
            "The network name is reserved by Docker. Use a different name.",
        )]));
    }
    let info = coolify_jobs::server::ServerInfo::from_row(&s);
    let expected = if info.is_swarm() {
        "swarm"
    } else {
        "standalone"
    };
    let requested = if input.has("type") {
        input
            .get("type")
            .filter(|v| !v.is_null())
            .map(as_php_string)
    } else {
        Some(expected.to_string())
    };
    if requested.as_deref() != Some(expected) {
        return Ok(json_status(
            StatusCode::UNPROCESSABLE_ENTITY,
            json!({"message": format!("Destination type must be {expected} for this server.")}),
        ));
    }
    let server_name = s
        .record
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let name = match input.get("name").filter(|v| php_truthy(v)) {
        Some(v) => as_php_string(v),
        None => format!("{server_name}-{network}"),
    };
    // `authorize('create', $class)`.
    authorize_update(&ctx)?;
    let kind = if expected == "swarm" {
        Kind::Swarm
    } else {
        Kind::Standalone
    };
    const EXISTS: ApiError = ApiError::Message(
        StatusCode::CONFLICT,
        "A destination with this network already exists on the server.",
    );
    if dest::network_exists(&state.db, kind, s.id, &network).await? {
        return Err(EXISTS);
    }
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    if kind == Kind::Standalone {
        // `existingServerNetworkResponse`.
        if !info.is_functional() {
            return Err(ApiError::Message(
                StatusCode::BAD_REQUEST,
                "Server is not functional.",
            ));
        }
        let output = jobs
            .instant_remote(
                &s,
                &["docker network ls --format '{{.Name}}'".to_string()],
                true,
                "destination.networks",
            )
            .await
            .map_err(|_| {
                ApiError::Message(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "Could not read the Docker networks on the server. Check that the server is reachable and try again.",
                )
            })?;
        if output
            .unwrap_or_default()
            .split('\n')
            .any(|l| coolify_remote::text::php_trim(l) == network)
        {
            return Err(validation_errors(&[(
                "network",
                "A Docker network with this name already exists on the server. Use a new network name.",
            )]));
        }
    }
    let uuid = new_public_id();
    let stored_name = strip_tags(Some(&name));
    let created = dest::create(
        &state.db,
        kind,
        &uuid,
        &stored_name,
        &network,
        s.id,
        state.now(),
    )
    .await?;
    if created.is_none() {
        return Err(EXISTS);
    }
    if kind == Kind::Standalone {
        // `StandaloneDocker::created`.
        let _ = jobs
            .instant_remote(
                &s,
                &proxy_ops::destination_created_commands(&network),
                false,
                "destination.created",
            )
            .await;
    }
    tracing::info!(target: "audit", event = "api.destination.created", team_id = ctx.team_id, destination_uuid = %uuid, "destination created");
    let d = dest::find_in_team(&state.db, ctx.team_id, &uuid)
        .await?
        .ok_or(ApiError::Internal)?;
    Ok(json_status(StatusCode::CREATED, d.transform()))
}

pub async fn update(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let input = validated_json(&headers, &uri, &body)?;
    validate(
        &input,
        &[Field {
            name: "name",
            rules: vec![Rule::Required, Rule::String, Rule::Max(255)],
        }],
        CUSTOM_API_MESSAGES,
        &["name"],
    )?;
    let d = find(&state, &ctx, &uuid).await?;
    authorize_update(&ctx)?;
    let name = strip_tags(input.str("name"));
    dest::update_name(&state.db, d.kind, d.id, &name, state.now()).await?;
    tracing::info!(target: "audit", event = "api.destination.updated", team_id = ctx.team_id, destination_uuid = %uuid, "destination updated");
    let fresh = find(&state, &ctx, &uuid).await?;
    Ok(ok(fresh.transform()).into_response())
}

pub async fn delete(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let d = find(&state, &ctx, &uuid).await?;
    authorize_update(&ctx)?;
    if dest::attached_to(&state.db, d.kind, d.id).await? {
        return Err(ApiError::Message(
            StatusCode::CONFLICT,
            "Destination has attached resources, detach first.",
        ));
    }
    if d.kind == Kind::Standalone {
        let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
        let row = servers::find_by_id(&state.db, d.server_id)
            .await?
            .ok_or(ApiError::Internal)?;
        let srv = Srv::from_row(jobs, row);
        proxy_ops::remove_standalone_network(&srv, &d)
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "removing the destination network failed");
                ApiError::Internal
            })?;
    }
    dest::delete(&state.db, d.kind, d.id).await?;
    tracing::info!(target: "audit", event = "api.destination.deleted", team_id = ctx.team_id, destination_uuid = %uuid, "destination deleted");
    Ok(ok(json!({"message": "Deleted."})).into_response())
}

// ── Resources ───────────────────────────────────────────────────────────

/// `ResourcesController::resources`.
pub async fn resources(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
) -> ApiResult {
    // `authorize('viewAny', Project::class)` allows every team member.
    if coolify_db::server_ops::team_has_services(&state.db, ctx.team_id).await? {
        // Presenting a service needs the Service model (ported with services).
        return Err(ApiError::NotPorted);
    }
    let mut items: Vec<Value> = Vec::new();
    let apps = coolify_db::applications::list_for_team(&state.db, ctx.team_id).await?;
    for a in &apps {
        let v = crate::routes::applications::present(&state, &ctx, a, false).await?;
        let mut m = match v {
            Value::Object(m) => m,
            _ => Map::new(),
        };
        m.insert("type".into(), json!("application"));
        items.push(Value::Object(serialize_record(m)));
    }
    for d in coolify_db::databases::list_for_team(&state.db, ctx.team_id).await? {
        let v = crate::routes::databases::present(&state, &ctx, &d, false, Some(json!([]))).await?;
        let mut m = match v {
            Value::Object(m) => m,
            _ => Map::new(),
        };
        m.insert("type".into(), json!(d.kind.type_name()));
        items.push(Value::Object(serialize_record(m)));
    }
    Ok(ok(Value::Array(items)).into_response())
}

/// `ServersController::resources_by_server`.
pub async fn resources_by_server(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    if !coolify_db::server_ops::services_on_server(&state.db, s.id)
        .await?
        .is_empty()
    {
        return Err(ApiError::NotPorted);
    }
    let items = defined_resources(&state, s.id)
        .await?
        .into_iter()
        .map(|v| match v {
            Value::Object(m) => Value::Object(serialize_record(m)),
            o => o,
        })
        .collect();
    Ok(ok(Value::Array(items)).into_response())
}

/// `$server->definedResources()` as the controller maps them (services are not ported).
pub(crate) async fn defined_resources(
    state: &AppState,
    server_id: i64,
) -> Result<Vec<Value>, ApiError> {
    let mut items = Vec::new();
    let time = |r: &coolify_db::records::Record, k: &str| r.get(k).cloned().unwrap_or(Value::Null);
    for a in coolify_db::applications::list_for_server(&state.db, server_id).await? {
        let r = coolify_db::records::APPLICATION.present(a.raw.clone());
        let mut m = Map::new();
        m.insert("id".into(), time(&r, "id"));
        m.insert("uuid".into(), time(&r, "uuid"));
        m.insert("name".into(), time(&r, "name"));
        m.insert("type".into(), json!("application"));
        m.insert("created_at".into(), time(&r, "created_at"));
        m.insert("updated_at".into(), time(&r, "updated_at"));
        m.insert(
            "status".into(),
            json!(coolify_jobs::applications::status::normalize_status(
                a.str("status").unwrap_or_default()
            )),
        );
        items.push(Value::Object(m));
    }
    for d in coolify_db::databases::list_for_server(&state.db, server_id).await? {
        let r = coolify_db::databases::present(d.kind, d.raw.clone());
        let mut m = Map::new();
        m.insert("id".into(), time(&r, "id"));
        m.insert("uuid".into(), time(&r, "uuid"));
        m.insert("name".into(), time(&r, "name"));
        m.insert("type".into(), json!(d.kind.type_name()));
        m.insert("created_at".into(), time(&r, "created_at"));
        m.insert("updated_at".into(), time(&r, "updated_at"));
        m.insert(
            "status".into(),
            json!(coolify_jobs::databases::normalize_status(
                d.str("status").unwrap_or_default()
            )),
        );
        items.push(Value::Object(m));
    }
    Ok(items)
}

/// `domainHostWithoutPort`.
fn domain_host_without_port(fqdn: &str) -> String {
    let stripped = fqdn.replace("http://", "").replace("https://", "");
    let authority = stripped.split('/').next().unwrap_or_default();
    if authority.starts_with('[')
        && let Some(end) = authority.find(']')
    {
        return authority[..=end].to_string();
    }
    authority.split(':').next().unwrap_or_default().to_string()
}

/// `ServersController::domains_by_server`.
pub async fn domains_by_server(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    uri: Uri,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    if let Some(app_uuid) = query_param(&uri, "uuid").filter(|u| !u.is_empty() && u != "0") {
        let a = coolify_db::applications::find_in_team(&state.db, ctx.team_id, &app_uuid)
            .await?
            .ok_or(ApiError::Message(
                StatusCode::NOT_FOUND,
                "Application not found.",
            ))?;
        // `$application->fqdns`: the comma-separated list, split and filtered.
        let fqdn = a.str("fqdn").unwrap_or_default();
        let list: Vec<Value> = if fqdn.is_empty() {
            Vec::new()
        } else {
            fqdn.split(',').map(|f| json!(f)).collect()
        };
        return Ok(ok(Value::Array(list)).into_response());
    }
    let settings = coolify_db::server_ops::instance_server_settings(&state.db).await?;
    let ip = s
        .record
        .get("ip")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let mut grouped: Vec<(String, Vec<Value>)> = Vec::new();
    let mut push = |ip: &str, domains: &[Value]| {
        if let Some((_, list)) = grouped.iter_mut().find(|(i, _)| i == ip) {
            list.extend_from_slice(domains);
        } else {
            grouped.push((ip.to_string(), domains.to_vec()));
        }
    };
    fn filled(v: &Option<String>) -> Option<&str> {
        v.as_deref().filter(|x| !x.is_empty() && *x != "0")
    }
    let targets: Vec<String> = if ip == "host.docker.internal" {
        let mut t = Vec::new();
        if let Some(v) = filled(&settings.public_ipv4) {
            t.push(v.to_string());
        }
        if let Some(v) = filled(&settings.public_ipv6) {
            t.push(v.to_string());
        }
        if t.is_empty() {
            t.push(ip.clone());
        }
        t
    } else {
        vec![ip.clone()]
    };
    let apps = coolify_db::applications::list_for_team(&state.db, ctx.team_id).await?;
    for a in apps.iter().filter(|a| a.server_id() == Some(s.id)) {
        let domains: Vec<Value> = a
            .str("fqdn")
            .unwrap_or_default()
            .split(',')
            .map(domain_host_without_port)
            .filter(|d| !d.is_empty())
            .map(|d| json!(d))
            .collect();
        for t in &targets {
            push(t, &domains);
        }
    }
    // Service applications are read with the services port.
    if !coolify_db::server_ops::services_on_server(&state.db, s.id)
        .await?
        .is_empty()
    {
        return Err(ApiError::NotPorted);
    }
    let out: Vec<Value> = grouped
        .into_iter()
        .map(|(ip, domains)| {
            let mut m = Map::new();
            m.insert("ip".into(), json!(ip));
            m.insert("domains".into(), Value::Array(domains));
            Value::Object(m)
        })
        .collect();
    Ok(ok(Value::Array(
        out.into_iter()
            .map(|v| match v {
                Value::Object(m) => Value::Object(serialize_record(m)),
                o => o,
            })
            .collect(),
    ))
    .into_response())
}
