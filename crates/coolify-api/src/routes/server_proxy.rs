//! `ServerProxyController`, `ServerDockerRegistriesController`, the server scope of
//! `SharedEnvironmentVariablesController`, `SentinelController` and the small parts
//! of `ServerTransferController` that need no transfer bundle.

use std::sync::LazyLock;

use axum::Extension;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use base64::Engine as _;
use coolify_compat::php_json;
use coolify_db::server_ops as ops;
use coolify_db::servers::{self, ColumnValue as C, ServerRow};
use coolify_jobs::server_ops::{Srv, proxy as proxy_ops, proxy_config, registries, sentinel};
use regex::Regex;
use serde_json::{Map, Value, json};

use crate::auth::AuthCtx;
use crate::input::{lenient_json, validated_json};
use crate::response::{ApiError, ApiResult, ok};
use crate::routes::server_settings::{
    Setting, authorize_update, find_server, json_status, save_settings, validation_errors,
};
use crate::routes::servers::{as_php_string, deploy_forbidden, input_bool, proxy_type_valid};
use crate::serialize::{php_truthy, serialize_record};
use crate::state::AppState;
use crate::validation::{CUSTOM_API_MESSAGES, Field, Messages, Rule, validate};

fn is_functional(s: &ServerRow) -> bool {
    coolify_jobs::server::ServerInfo::from_row(s).is_functional()
}

const NOT_FUNCTIONAL: ApiError =
    ApiError::Message(StatusCode::BAD_REQUEST, "Server is not functional.");

// ── Proxy ───────────────────────────────────────────────────────────────

/// `ServerProxyController::payload`.
fn proxy_payload(ctx: &AuthCtx, s: &ServerRow) -> Value {
    let proxy = s.record.get("proxy").and_then(Value::as_object);
    let get = |k: &str| proxy.and_then(|p| p.get(k)).cloned().unwrap_or(Value::Null);
    let settings = s.settings.clone().unwrap_or_default();
    let mut m = Map::new();
    m.insert("proxy_type".into(), get("type"));
    m.insert("status".into(), get("status"));
    m.insert(
        "redirect_enabled".into(),
        Value::Bool(match get("redirect_enabled") {
            Value::Null => proxy.is_none_or(|p| !p.contains_key("redirect_enabled")),
            v => php_truthy(&v),
        }),
    );
    m.insert("redirect_url".into(), get("redirect_url"));
    m.insert(
        "proxy_error_page".into(),
        s.record
            .get("proxy_error_page")
            .cloned()
            .unwrap_or(Value::Null),
    );
    m.insert(
        "generate_exact_labels".into(),
        Value::Bool(
            settings
                .get("generate_exact_labels")
                .is_some_and(php_truthy),
        ),
    );
    let resolver = settings
        .get("traefik_certificate_resolver")
        .and_then(Value::as_str)
        .filter(|r| !r.trim().is_empty() && *r != "none")
        .map_or(Value::Null, |r| json!(r));
    m.insert("traefik_certificate_resolver".into(), resolver);
    if ctx.can_read_sensitive {
        let c = get("last_saved_proxy_configuration");
        let filled = c.as_str().is_some_and(|t| !t.trim().is_empty());
        m.insert("configuration".into(), if filled { c } else { Value::Null });
    }
    Value::Object(m)
}

pub async fn proxy_show(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    Ok(ok(proxy_payload(&ctx, &s)).into_response())
}

pub(crate) static RESOLVER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z0-9_-]+\n?\z").unwrap_or_else(|_| unreachable!()));

fn page_size_rule(v: &Value) -> Option<String> {
    let t = v.as_str()?;
    (t.len() > proxy_ops::PROXY_ERROR_PAGE_MAX_BYTES).then(|| {
        format!(
            "The custom error page must not be larger than {} KB.",
            proxy_ops::PROXY_ERROR_PAGE_MAX_BYTES / 1024
        )
    })
}

pub async fn proxy_update(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let input = validated_json(&headers, &uri, &body)?;
    const ALLOWED: &[&str] = &[
        "redirect_enabled",
        "redirect_url",
        "proxy_error_page",
        "generate_exact_labels",
        "traefik_certificate_resolver",
        "proxy_type",
    ];
    let mut url_error: Option<String> = None;
    if let Some(Value::String(u)) = input.get("redirect_url") {
        url_error = crate::safe_url::check(&state, "redirect_url", u, &headers).await?;
    }
    let fields = vec![
        Field {
            name: "redirect_enabled",
            rules: vec![Rule::Boolean],
        },
        Field {
            name: "redirect_url",
            rules: vec![Rule::Nullable, Rule::String],
        },
        Field {
            name: "proxy_error_page",
            rules: vec![
                Rule::Nullable,
                Rule::String,
                Rule::CustomOwned(page_size_rule),
            ],
        },
        Field {
            name: "generate_exact_labels",
            rules: vec![Rule::Boolean],
        },
        Field {
            name: "traefik_certificate_resolver",
            rules: vec![
                Rule::Nullable,
                Rule::String,
                Rule::Max(255),
                Rule::Regex(&RESOLVER),
            ],
        },
        Field {
            name: "proxy_type",
            rules: vec![Rule::Nullable, Rule::String],
        },
    ];
    match validate(&input, &fields, CUSTOM_API_MESSAGES, ALLOWED) {
        Ok(()) => {
            if let Some(e) = url_error {
                return Err(validation_errors(&[("redirect_url", e.as_str())]));
            }
        }
        Err(ApiError::Validation(errors)) => {
            // The rule's message goes after those of the earlier fields.
            let Some(e) = url_error else {
                return Err(ApiError::Validation(errors));
            };
            let mut merged = Map::new();
            if let Some(v) = errors.get("redirect_enabled") {
                merged.insert("redirect_enabled".into(), v.clone());
            }
            let mut list = errors
                .get("redirect_url")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            list.push(json!(e));
            merged.insert("redirect_url".into(), Value::Array(list));
            for (k, v) in errors {
                merged.entry(k).or_insert(v);
            }
            return Err(ApiError::Validation(merged));
        }
        Err(e) => return Err(e),
    }
    let s = find_server(&state, &ctx, &uuid).await?;
    authorize_update(&ctx)?;
    let requested_type = input
        .get("proxy_type")
        .filter(|v| php_truthy(v))
        .map(as_php_string);
    if let Some(t) = &requested_type
        && !proxy_type_valid(t)
    {
        return Err(validation_errors(&[("proxy_type", "Invalid proxy type.")]));
    }
    let current_type = s
        .record
        .get("proxy")
        .and_then(|p| p.get("type"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_lowercase();
    let type_changed = requested_type
        .as_deref()
        .is_some_and(|t| t.to_lowercase() != current_type);
    if type_changed {
        deploy_forbidden(
            &ctx,
            "Missing required permissions: deploy. Changing the proxy type starts the new proxy on the server, so it needs a token with the deploy permission.",
        )?;
    }
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    let mut srv = Srv::from_row(jobs, s.clone());
    let mut redirect_changed = false;
    if input.has("redirect_enabled") {
        srv.proxy_set(
            "redirect_enabled",
            Value::Bool(input_bool(&input, "redirect_enabled")),
        );
        redirect_changed = true;
    }
    if input.has("redirect_url") {
        let v = input
            .get("redirect_url")
            .filter(|v| php_truthy(v))
            .cloned()
            .unwrap_or(Value::Null);
        srv.proxy_set("redirect_url", v);
        redirect_changed = true;
    }
    if redirect_changed {
        srv.save_proxy().await.map_err(job_error)?;
    }
    if input.has("proxy_error_page") {
        let v = input
            .get("proxy_error_page")
            .filter(|v| php_truthy(v))
            .map(as_php_string);
        servers::update_if_changed(
            &state.db,
            servers::Row::Server(s.id),
            &[("proxy_error_page", C::Text(v))],
            state.now(),
        )
        .await?;
        redirect_changed = true;
    }
    if input.has("generate_exact_labels") {
        save_settings(
            &state,
            &s,
            &[(
                "generate_exact_labels",
                Setting::Bool(input_bool(&input, "generate_exact_labels")),
            )],
        )
        .await?;
    }
    let mut resolver_changed = false;
    if input.has("traefik_certificate_resolver") {
        let v = input
            .get("traefik_certificate_resolver")
            .filter(|v| php_truthy(v))
            .map(as_php_string);
        let changed = save_settings(
            &state,
            &s,
            &[("traefik_certificate_resolver", Setting::Text(v))],
        )
        .await?;
        resolver_changed = !changed.is_empty();
    }
    if let (true, Some(t)) = (type_changed, &requested_type) {
        coolify_jobs::server_ops::proxy::change_proxy(jobs, s.id, t)
            .await
            .map_err(job_error)?;
    }
    let fresh = servers::find_by_id(&state.db, s.id)
        .await?
        .ok_or(ApiError::Internal)?;
    if is_functional(&fresh) && (redirect_changed || resolver_changed) {
        let srv = Srv::from_row(jobs, fresh.clone());
        proxy_ops::setup_default_redirect(&srv)
            .await
            .map_err(job_error)?;
        if resolver_changed {
            proxy_ops::setup_dynamic_proxy_configuration(&srv)
                .await
                .map_err(job_error)?;
        }
    }
    tracing::info!(target: "audit", event = "api.server.proxy.updated", team_id = ctx.team_id, server_uuid = %s.uuid, "proxy updated");
    let fresh = servers::find_by_id(&state.db, s.id)
        .await?
        .ok_or(ApiError::Internal)?;
    Ok(ok(proxy_payload(&ctx, &fresh)).into_response())
}

fn job_error(e: String) -> ApiError {
    tracing::error!(error = %e, "server operation failed");
    ApiError::Internal
}

/// A `ValidationException` from `SaveProxyConfiguration`.
fn configuration_exception(message: &str) -> Response {
    json_status(
        StatusCode::UNPROCESSABLE_ENTITY,
        json!({"message": message, "errors": {"configuration": [message]}}),
    )
}

/// PHP `isBase64Encoded`.
fn is_base64_encoded(v: &str) -> bool {
    use base64::engine::general_purpose::STANDARD;
    STANDARD.decode(v).is_ok_and(|d| STANDARD.encode(d) == v)
}

pub async fn proxy_save_configuration(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    deploy_forbidden(
        &ctx,
        "Missing required permissions: deploy. Coolify applies the proxy configuration on the server, so it needs a token with the deploy permission.",
    )?;
    let input = validated_json(&headers, &uri, &body)?;
    validate(
        &input,
        &[Field {
            name: "configuration",
            rules: vec![Rule::Required, Rule::String],
        }],
        CUSTOM_API_MESSAGES,
        &["configuration"],
    )?;
    let s = find_server(&state, &ctx, &uuid).await?;
    authorize_update(&ctx)?;
    let mut configuration = input.str("configuration").unwrap_or_default().to_string();
    if is_base64_encoded(&configuration) {
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(&configuration)
            .ok()
            .and_then(|d| String::from_utf8(d).ok());
        let Some(decoded) = decoded else {
            return Err(validation_errors(&[(
                "configuration",
                "The configuration should be valid base64-encoded UTF-8 text.",
            )]));
        };
        configuration = decoded;
    }
    if coolify_remote::text::php_trim(&configuration).is_empty() {
        return Err(validation_errors(&[(
            "configuration",
            "The configuration field is required.",
        )]));
    }
    if !is_functional(&s) {
        return Err(NOT_FUNCTIONAL);
    }
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    let mut srv = Srv::from_row(jobs, s.clone());
    if let Err(e) = proxy_config::save_proxy_configuration(&mut srv, &configuration).await {
        return match e {
            proxy_config::SaveError::Invalid(i) => Ok(configuration_exception(&i.0)),
            proxy_config::SaveError::Other(m) => Err(job_error(m)),
        };
    }
    tracing::info!(target: "audit", event = "api.server.proxy.configuration_saved", team_id = ctx.team_id, server_uuid = %s.uuid, "proxy configuration saved");
    let fresh = servers::find_by_id(&state.db, s.id)
        .await?
        .ok_or(ApiError::Internal)?;
    let mut payload = match proxy_payload(&ctx, &fresh) {
        Value::Object(m) => m,
        _ => Map::new(),
    };
    payload.insert("message".into(), json!("Proxy configuration saved."));
    Ok(ok(Value::Object(payload)).into_response())
}

pub async fn proxy_restart(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    authorize_update(&ctx)?;
    if !is_functional(&s) {
        return Err(NOT_FUNCTIONAL);
    }
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    jobs.spawn_restart_proxy(s.id);
    tracing::info!(target: "audit", event = "api.server.proxy.restarted", team_id = ctx.team_id, server_uuid = %s.uuid, "proxy restart queued");
    Ok(ok(json!({"message": "Proxy restart queued."})).into_response())
}

// ── Registries ──────────────────────────────────────────────────────────

const NOT_REACHABLE: ApiError = ApiError::Message(
    StatusCode::BAD_REQUEST,
    "The server is not reachable. Validate the server first.",
);

/// The scheme and host `route()` uses for the current request.
fn request_root(headers: &HeaderMap) -> String {
    let host = headers
        .get(axum::http::header::HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("localhost");
    format!("http://{host}")
}

/// `$resource->link()`.
fn resource_link(
    root: &str,
    project: &coolify_db::records::Record,
    environment: &coolify_db::records::Record,
    kind: &str,
    uuid: &str,
) -> Option<String> {
    let p = project.get("uuid").and_then(Value::as_str)?;
    let e = environment.get("uuid").and_then(Value::as_str)?;
    Some(format!("{root}/project/{p}/environment/{e}/{kind}/{uuid}"))
}

/// `usersOf`: registries of the images on this server.
async fn registry_users(
    state: &AppState,
    s: &ServerRow,
    headers: &HeaderMap,
) -> Result<Vec<registries::RegistryUser>, ApiError> {
    let root = request_root(headers);
    let mut users = Vec::new();
    for a in coolify_db::applications::list_for_server(&state.db, s.id).await? {
        if let Some(image) = a
            .str("docker_registry_image_name")
            .filter(|i| !i.trim().is_empty())
        {
            users.push((
                registries::registry_from_image(image),
                "Application",
                a.str("name").unwrap_or_default().to_string(),
                resource_link(
                    &root,
                    &a.project,
                    &a.environment,
                    "application",
                    a.str("uuid").unwrap_or_default(),
                ),
            ));
        }
    }
    for d in coolify_db::databases::list_for_server(&state.db, s.id).await? {
        if let Some(image) = d.str("image").filter(|i| !i.trim().is_empty()) {
            users.push((
                registries::registry_from_image(image),
                "Database",
                d.str("name").unwrap_or_default().to_string(),
                resource_link(
                    &root,
                    &d.project,
                    &d.environment,
                    "database",
                    d.str("uuid").unwrap_or_default(),
                ),
            ));
        }
    }
    let mut seen = std::collections::HashSet::new();
    for service in coolify_db::services::list_for_server(&state.db, s.id).await? {
        let link = service_link(&root, &service);
        let name = service.str("name").unwrap_or_default().to_string();
        let mut subs = coolify_db::services::subs(
            &state.db,
            coolify_db::services::SubKind::Application,
            service.id,
        )
        .await?;
        subs.extend(
            coolify_db::services::subs(
                &state.db,
                coolify_db::services::SubKind::Database,
                service.id,
            )
            .await?,
        );
        for sub in subs {
            if let Some(image) = sub.str("image").filter(|i| !i.trim().is_empty()) {
                let registry = registries::registry_from_image(image);
                if seen.insert(format!("{registry}|{link:?}|{name}")) {
                    users.push((registry, "Service", name.clone(), link.clone()));
                }
            }
        }
    }
    if crate::routes::servers::effective_role_is_build(&s.settings.clone().unwrap_or_default()) {
        users.extend(build_users(state, s, &root).await?);
    }
    Ok(users)
}

/// `$service->link()`.
fn service_link(root: &str, service: &coolify_db::services::ServiceRow) -> Option<String> {
    let p = service.project.get("uuid").and_then(Value::as_str)?;
    let e = service.environment.get("uuid").and_then(Value::as_str)?;
    Some(format!(
        "{root}/project/{p}/environment/{e}/service/{}",
        service.uuid
    ))
}

/// `applicationsBuiltOnBuildServers`: applications of the team a deployment builds on a build
/// server and pushes to a registry.
async fn build_users(
    state: &AppState,
    s: &ServerRow,
    root: &str,
) -> Result<Vec<registries::RegistryUser>, ApiError> {
    let mut apps = coolify_db::applications::list_for_team(
        &state.db,
        s.record
            .get("team_id")
            .and_then(Value::as_i64)
            .unwrap_or_default(),
    )
    .await?;
    apps.sort_by(|a, b| a.str("name").cmp(&b.str("name")));
    let mut users = Vec::new();
    for a in apps {
        let Some(image) = a
            .str("docker_registry_image_name")
            .filter(|i| !i.is_empty())
        else {
            continue;
        };
        let Some(deployment_server) = a.server_id() else {
            continue;
        };
        if deployment_server == s.id {
            continue;
        }
        let Some(target) = coolify_db::servers::find_by_id(&state.db, deployment_server).await?
        else {
            continue;
        };
        // `canBuildApplications`: every role except deployments only.
        let settings = target.settings.clone().unwrap_or_default();
        let deployment_only =
            settings.get("server_role").and_then(Value::as_str) == Some("deployment");
        let build_pack = a.str("build_pack").unwrap_or_default();
        let must_build_elsewhere =
            deployment_only && !matches!(build_pack, "dockerimage" | "dockercompose");
        let enabled = a
            .settings
            .get("is_build_server_enabled")
            .is_some_and(crate::serialize::php_truthy);
        if must_build_elsewhere || enabled {
            users.push((
                registries::registry_from_image(image),
                "Build",
                a.str("name").unwrap_or_default().to_string(),
                resource_link(
                    root,
                    &a.project,
                    &a.environment,
                    "application",
                    a.str("uuid").unwrap_or_default(),
                ),
            ));
        }
    }
    Ok(users)
}

pub async fn registries_index(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    headers: HeaderMap,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    authorize_update(&ctx)?;
    let mut error = Value::Null;
    let mut logins = registries::Logins::new();
    if !is_functional(&s) {
        error =
            json!("The server is not reachable. Validate the server to read its registry logins.");
    } else {
        let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
        match registries::for_server(&Srv::from_row(jobs, s.clone())).await {
            Ok(l) => logins = l,
            Err(_) => error = json!("Could not read the Docker configuration on this server."),
        }
    }
    let users = registry_users(&state, &s, &headers).await?;
    let mut m = Map::new();
    m.insert(
        "registries".into(),
        Value::Array(registries::rows(&logins, &users)),
    );
    m.insert("error".into(), error);
    Ok(ok(Value::Object(m)).into_response())
}

static USERNAME_NO_SPACE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[^\s]+\n?\z").unwrap_or_else(|_| unreachable!()));

const REGISTRY_MESSAGES: Messages = &[
    ("required", "This field is required."),
    (
        "registry.regex",
        "Enter a registry host, for example ghcr.io or registry.example.com:5000.",
    ),
    ("username.regex", "The username must not contain spaces."),
];

pub async fn registries_login(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let mut input = validated_json(&headers, &uri, &body)?;
    if let Some(Value::String(r)) = input.get("registry") {
        let n = registries::normalize_registry(r);
        input.0.insert("registry".into(), Value::String(n));
    }
    validate(
        &input,
        &[
            Field {
                name: "registry",
                rules: vec![
                    Rule::Required,
                    Rule::String,
                    Rule::Max(255),
                    Rule::Regex(&registries::REGISTRY_PATTERN),
                ],
            },
            Field {
                name: "username",
                rules: vec![
                    Rule::Required,
                    Rule::String,
                    Rule::Max(255),
                    Rule::Regex(&USERNAME_NO_SPACE),
                ],
            },
            Field {
                name: "password",
                rules: vec![Rule::Required, Rule::String, Rule::Max(20000)],
            },
        ],
        REGISTRY_MESSAGES,
        &["registry", "username", "password"],
    )?;
    let s = find_server(&state, &ctx, &uuid).await?;
    authorize_update(&ctx)?;
    if !is_functional(&s) {
        return Err(NOT_REACHABLE);
    }
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    let registry = input.str("registry").unwrap_or_default().to_string();
    let srv = Srv::from_row(jobs, s.clone());
    if let Err(e) = registries::login(
        &srv,
        &registry,
        input.str("username").unwrap_or_default(),
        input.str("password").unwrap_or_default(),
    )
    .await
    {
        return Ok(json_status(
            StatusCode::BAD_REQUEST,
            json!({"message": format!("Login failed: {e}")}),
        ));
    }
    Ok(ok(json!({"message": format!("Logged in to {registry}.")})).into_response())
}

async fn run_on_registry(
    state: &AppState,
    ctx: &AuthCtx,
    uuid: &str,
    raw_registry: &str,
) -> Result<(Srv, String), ApiError> {
    let registry = registries::normalize_registry(raw_registry);
    if registry.len() > 255 || !registries::REGISTRY_PATTERN.is_match(&registry) {
        return Err(validation_errors(&[(
            "registry",
            "Enter a registry host, for example ghcr.io or registry.example.com:5000.",
        )]));
    }
    let s = find_server(state, ctx, uuid).await?;
    authorize_update(ctx)?;
    if !is_functional(&s) {
        return Err(NOT_REACHABLE);
    }
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    Ok((Srv::from_row(jobs, s), registry))
}

pub async fn registries_check(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((uuid, registry)): Path<(String, String)>,
) -> ApiResult {
    let (srv, registry) = run_on_registry(&state, &ctx, &uuid, &registry).await?;
    match registries::check_login(&srv, &registry).await {
        Ok(()) => {
            Ok(ok(json!({"message": format!("The login for {registry} works.")})).into_response())
        }
        Err(e) => Ok(json_status(
            StatusCode::BAD_REQUEST,
            json!({"message": format!("Login check failed: {e}")}),
        )),
    }
}

pub async fn registries_logout(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((uuid, registry)): Path<(String, String)>,
) -> ApiResult {
    let (srv, registry) = run_on_registry(&state, &ctx, &uuid, &registry).await?;
    match registries::logout(&srv, &registry).await {
        Ok(()) => {
            Ok(ok(json!({"message": format!("Logged out from {registry}.")})).into_response())
        }
        Err(e) => Ok(json_status(
            StatusCode::BAD_REQUEST,
            json!({"message": format!("Logout failed: {e}")}),
        )),
    }
}

// ── Server shared variables ─────────────────────────────────────────────

const ENV_ALLOWED: &[&str] = &[
    "key",
    "value",
    "is_literal",
    "is_multiline",
    "is_shown_once",
    "comment",
];
const ENV_NOT_FOUND: ApiError =
    ApiError::Message(StatusCode::NOT_FOUND, "Environment variable not found.");

static ENV_KEY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\A[A-Za-z_][A-Za-z0-9_.]*\z").unwrap_or_else(|_| unreachable!()));

/// `removeSensitiveData` + `serializeApiResponse`.
fn present_env(
    state: &AppState,
    ctx: &AuthCtx,
    mut r: coolify_db::records::Record,
) -> Result<Value, ApiError> {
    for k in [
        "team_id",
        "project_id",
        "environment_id",
        "server_id",
        "version",
    ] {
        r.shift_remove(k);
    }
    let shown_once = r.get("is_shown_once").is_some_and(php_truthy);
    let cipher = r.shift_remove("value");
    if ctx.can_read_sensitive && !shown_once {
        let plain = cipher
            .as_ref()
            .and_then(Value::as_str)
            .map(|c| {
                state
                    .encrypter
                    .as_ref()
                    .ok_or(ApiError::Internal)
                    .and_then(|e| e.decrypt_string(c).map_err(|_| ApiError::Internal))
            })
            .transpose()?;
        r.insert("value".into(), plain.map_or(Value::Null, Value::String));
    }
    for k in ["created_at", "updated_at"] {
        if let Some(Value::String(t)) = r.get(k) {
            let v = coolify_db::records::laravel_datetime(t);
            r.insert(k.into(), Value::String(v));
        }
    }
    Ok(Value::Object(serialize_record(r)))
}

fn env_fields(require_key: bool) -> Vec<Field> {
    let mut key = vec![if require_key {
        Rule::Required
    } else {
        Rule::Nullable
    }];
    key.extend([Rule::String, Rule::Max(255), Rule::Regex(&ENV_KEY)]);
    vec![
        Field {
            name: "key",
            rules: key,
        },
        Field {
            name: "value",
            rules: vec![Rule::String, Rule::Nullable],
        },
        Field {
            name: "is_literal",
            rules: vec![Rule::Boolean],
        },
        Field {
            name: "is_multiline",
            rules: vec![Rule::Boolean],
        },
        Field {
            name: "is_shown_once",
            rules: vec![Rule::Boolean],
        },
        Field {
            name: "comment",
            rules: vec![Rule::String, Rule::Nullable, Rule::Max(256)],
        },
    ]
}

pub async fn envs_list(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    let rows = ops::server_envs(&state.db, ctx.team_id, s.id).await?;
    let items = rows
        .into_iter()
        .map(|r| present_env(&state, &ctx, r))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ok(Value::Array(items)).into_response())
}

pub async fn envs_create(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    let input = validated_json(&headers, &uri, &body)?;
    validate(&input, &env_fields(true), CUSTOM_API_MESSAGES, ENV_ALLOWED)?;
    // `authorize('create', SharedEnvironmentVariable::class)`.
    authorize_update(&ctx)?;
    let key = input.str("key").unwrap_or_default().to_string();
    if ops::server_env_key_exists(&state.db, ctx.team_id, s.id, &key, None).await? {
        return Err(ApiError::Message(
            StatusCode::CONFLICT,
            "Environment variable already exists. Use PATCH request to update it.",
        ));
    }
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    // `value` is NOT NULL: a request without one fails at the database, as upstream.
    let Some(value) = input.str("value") else {
        return Err(ApiError::Internal);
    };
    let id = ops::create_server_env(
        &state.db,
        &ops::NewServerEnv {
            team_id: ctx.team_id,
            server_id: s.id,
            key: &key,
            value: &enc.encrypt_string(value),
            is_literal: input_bool(&input, "is_literal"),
            is_multiline: input_bool(&input, "is_multiline"),
            is_shown_once: input_bool(&input, "is_shown_once"),
            comment: input.str("comment"),
        },
        state.now(),
    )
    .await?;
    tracing::info!(target: "audit", event = "api.shared_env.created", team_id = ctx.team_id, env_id = id, "shared variable created");
    Ok(json_status(StatusCode::CREATED, json!({"id": id})))
}

fn parse_env_id(raw: &str) -> Option<i64> {
    raw.parse().ok()
}

pub async fn envs_update(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((uuid, env_id)): Path<(String, String)>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    let id = parse_env_id(&env_id).ok_or(ENV_NOT_FOUND)?;
    let current = ops::find_server_env(&state.db, ctx.team_id, s.id, id)
        .await?
        .ok_or(ENV_NOT_FOUND)?;
    authorize_update(&ctx)?;
    let input = validated_json(&headers, &uri, &body)?;
    validate(&input, &env_fields(false), CUSTOM_API_MESSAGES, ENV_ALLOWED)?;
    let mut changes: Vec<(&'static str, C)> = Vec::new();
    if input.has("key") {
        let key = input.str("key").unwrap_or_default().to_string();
        if Some(key.as_str()) != current.get("key").and_then(Value::as_str) {
            if ops::server_env_key_exists(&state.db, ctx.team_id, s.id, &key, Some(id)).await? {
                return Err(ApiError::Message(
                    StatusCode::CONFLICT,
                    "Environment variable already exists with this key.",
                ));
            }
            changes.push(("key", C::Text(Some(key))));
        }
    }
    if input.has("value") {
        let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
        let new = input.str("value");
        let old = current
            .get("value")
            .and_then(Value::as_str)
            .and_then(|c| enc.decrypt_string(c).ok());
        if new != old.as_deref() {
            let Some(new) = new else {
                return Err(ApiError::Internal);
            };
            changes.push(("value", C::Text(Some(enc.encrypt_string(new)))));
        }
    }
    for k in ["is_literal", "is_multiline", "is_shown_once"] {
        if input.has(k) {
            let v = input_bool(&input, k);
            if current.get(k).and_then(Value::as_bool) != Some(v) {
                changes.push((k, C::Bool(v)));
            }
        }
    }
    if input.has("comment") {
        let v = input.str("comment").map(str::to_string);
        if current.get("comment").and_then(Value::as_str) != v.as_deref() {
            changes.push(("comment", C::Text(v)));
        }
    }
    ops::update_shared_env(&state.db, id, &changes, state.now()).await?;
    tracing::info!(target: "audit", event = "api.shared_env.updated", team_id = ctx.team_id, env_id = id, "shared variable updated");
    let fresh = ops::find_server_env(&state.db, ctx.team_id, s.id, id)
        .await?
        .ok_or(ENV_NOT_FOUND)?;
    Ok(ok(present_env(&state, &ctx, fresh)?).into_response())
}

pub async fn envs_delete(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((uuid, env_id)): Path<(String, String)>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    let id = parse_env_id(&env_id).ok_or(ENV_NOT_FOUND)?;
    ops::find_server_env(&state.db, ctx.team_id, s.id, id)
        .await?
        .ok_or(ENV_NOT_FOUND)?;
    authorize_update(&ctx)?;
    ops::delete_shared_env(&state.db, id).await?;
    tracing::info!(target: "audit", event = "api.shared_env.deleted", team_id = ctx.team_id, env_id = id, "shared variable deleted");
    Ok(ok(json!({"message": "Environment variable deleted."})).into_response())
}

// ── Sentinel push ───────────────────────────────────────────────────────

fn unauthorized(message: &'static str) -> Response {
    json_status(StatusCode::UNAUTHORIZED, json!({"message": message}))
}

fn php_json_hash(v: &Value) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    php_json::encode(v).hash(&mut h);
    h.finish()
}

/// `SentinelController::containerStateHash`.
fn container_state_hash(data: &Map<String, Value>) -> u64 {
    let mut containers: Vec<Value> = data
        .get("containers")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|c| {
            json!({
                "name": c.get("name"),
                "state": c.get("state"),
                "health_status": c.get("health_status"),
                "restart_count": c.get("restart_count"),
            })
        })
        .collect();
    containers.sort_by(|a, b| {
        a.get("name")
            .and_then(Value::as_str)
            .cmp(&b.get("name").and_then(Value::as_str))
    });
    let complete =
        data.get("snapshot").and_then(|s| s.get("complete")) != Some(&Value::Bool(false));
    php_json_hash(&json!({"snapshot_complete": complete, "containers": containers}))
}

/// `SentinelController::push`.
pub async fn sentinel_push(
    State(state): State<AppState>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    use axum::http::header::AUTHORIZATION;
    let Some(token) = headers.get(AUTHORIZATION).and_then(|v| v.to_str().ok()) else {
        return Ok(unauthorized("Unauthorized"));
    };
    let naked = token.replace("Bearer ", "");
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let Ok(decrypted) = enc.decrypt_maybe_serialized(&naked) else {
        return Ok(unauthorized("Invalid token"));
    };
    let server_uuid = serde_json::from_str::<Value>(&decrypted)
        .ok()
        .and_then(|d| d.get("server_uuid").cloned())
        .filter(php_truthy)
        .map(|v| as_php_string(&v));
    let Some(server_uuid) = server_uuid else {
        return Ok(unauthorized("Invalid token"));
    };
    let Some(row) = servers::find_any_team(&state.db, &server_uuid).await? else {
        return Ok(json_status(
            StatusCode::NOT_FOUND,
            json!({"message": "Server not found"}),
        ));
    };
    let info = coolify_jobs::server::ServerInfo::from_row(&row);
    if !info.is_functional() {
        sentinel::remember_push_problem(
            row.id,
            Some(
                "Coolify rejected the push: the server is marked unreachable, not usable, or disabled. Validate the server connection.",
            ),
        );
        return Ok(unauthorized("Server is not functional"));
    }
    let stored = row
        .settings
        .as_ref()
        .and_then(|s| s.get("sentinel_token"))
        .and_then(Value::as_str)
        .and_then(|c| enc.decrypt_string(c).ok());
    if stored.as_deref() != Some(naked.as_str()) {
        sentinel::remember_push_problem(
            row.id,
            Some(
                "Coolify rejected the push: the Sentinel token does not match. Sync Sentinel to apply the current token.",
            ),
        );
        return Ok(unauthorized("Unauthorized"));
    }
    let input = lenient_json(&headers, &uri, &body);
    let containers = input.get("containers");
    let error = match containers {
        None => Some("The containers field must be present."),
        Some(Value::Array(_)) | Some(Value::Object(_)) => None,
        Some(_) => Some("The containers field must be an array."),
    };
    if let Some(e) = error {
        sentinel::remember_push_problem(
            row.id,
            Some("Coolify rejected the push: the payload is not valid."),
        );
        return Ok(json_status(
            StatusCode::UNPROCESSABLE_ENTITY,
            json!({"message": "Validation failed.", "errors": {"containers": [e]}}),
        ));
    }
    let jobs = state.jobs.as_ref();
    let srv_live = jobs.map(|j| sentinel::sentinel_live(&Srv::from_row(j, row.clone())));
    ops::sentinel_heartbeat(&state.db, row.id, state.now()).await?;
    if srv_live != Some(true) {
        sentinel::remember_push_problem(row.id, None);
    }
    // `updateSentinelIfOutdated`.
    if let Some(version) = headers
        .get("X-Sentinel-Version")
        .and_then(|v| v.to_str().ok())
        .filter(|v| {
            let p: Vec<&str> = v.split('.').collect();
            p.len() == 3
                && p.iter()
                    .all(|x| !x.is_empty() && x.chars().all(|c| c.is_ascii_digit()))
        })
    {
        let marks = &coolify_jobs::server_ops::MARKS;
        marks.put(
            format!("sentinel:reported-version:{}", row.id),
            version.to_string(),
            std::time::Duration::from_secs(7200),
        );
        let latest = coolify_jobs::server_ops::latest_sentinel_version();
        if coolify_jobs::server_ops::version_compare(version, &latest) == std::cmp::Ordering::Less
            && marks.add(
                format!("sentinel:update-dispatched:{}", row.id),
                std::time::Duration::from_secs(3600),
            )
            && let Some(j) = jobs
        {
            j.spawn_check_sentinel(row.id);
        }
    }
    // `shouldDispatchUpdate`, then `PushServerUpdateJob::dispatch($server, $data)`.
    let hash = container_state_hash(&input.0);
    let marks = &coolify_jobs::server_ops::MARKS;
    let hash_key = format!("sentinel:push-hash:{}", row.id);
    let force_key = format!("sentinel:push-force:{}", row.id);
    let cached = marks.get(&hash_key);
    if cached.as_deref() != Some(hash.to_string().as_str()) || !marks.has(&force_key) {
        marks.put(
            hash_key,
            hash.to_string(),
            std::time::Duration::from_secs(86_400),
        );
        marks.put(force_key, "1".into(), std::time::Duration::from_secs(300));
        if let Some(j) = jobs {
            j.spawn_push_server_update(row.id, Value::Object(input.0.clone()));
        }
    }
    Ok(ok(json!({"message": "ok"})).into_response())
}

// ── Transfer status ─────────────────────────────────────────────────────

/// `ServerTransferController::status`.
pub async fn transfer_status(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    let meta = s.record.get("server_metadata");
    let mut migration = meta
        .and_then(|m| m.get("migration"))
        .cloned()
        .unwrap_or(Value::Null);
    if let Value::Object(m) = &mut migration
        && !ctx.is_team_admin()
    {
        m.shift_remove("log");
        m.shift_remove("error");
    }
    let mut transfer = Map::new();
    if let Some(t) = meta
        .and_then(|m| m.get("transfer"))
        .and_then(Value::as_object)
    {
        for k in [
            "status",
            "export_id",
            "imported_at",
            "claimed_at",
            "transferred_at",
            "target_instance_url",
        ] {
            if let Some(v) = t.get(k) {
                transfer.insert(k.into(), v.clone());
            }
        }
    }
    let mut out = Map::new();
    out.insert("server_uuid".into(), json!(s.uuid));
    out.insert(
        "transfer".into(),
        if transfer.is_empty() {
            json!([])
        } else {
            Value::Object(transfer)
        },
    );
    out.insert("migration".into(), migration);
    Ok(ok(Value::Object(out)).into_response())
}
