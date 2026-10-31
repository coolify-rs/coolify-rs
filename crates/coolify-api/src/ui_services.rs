//! Session endpoints behind the service pages of the web UI: `Project\Service\Configuration`,
//! `StackForm`, `EditCompose`, `Heading`, `ResourceCard`, the one-click part of
//! `Project\Resource\Create` and of `Project\New\Select`.

pub mod extra_fields;
pub mod resources;
pub mod storage;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::middleware::from_fn_with_state;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Router};
use coolify_db::compose::NewEnv;
use coolify_db::records::Record;
use coolify_db::servers::ColumnValue;
use coolify_db::services::{self as svc, NewService, ServiceRow, SubKind, SubRow};
use coolify_domain::naming::strip_tags;
use coolify_jobs::compose_parser::domains::get_fqdn_without_port;
use coolify_jobs::services::templates;
use serde_json::{Map, Value, json};

use crate::auth::AuthCtx;
use crate::input::Input;
use crate::response::{ApiError, ApiResult, created, ok};
use crate::routes::databases::server_functional;
use crate::state::AppState;
use crate::ui_notifications::toast_error;
use crate::validation::{Messages, patterns, validate};

use extra_fields::{StackField, stack_fields};

pub fn routes(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/ui/service-templates", get(service_templates))
        .route("/ui/services", post(create))
        .route("/ui/services/{uuid}", get(page).patch(submit))
        .route("/ui/services/{uuid}/network", post(save_network))
        .route(
            "/ui/services/{uuid}/compose/validate",
            post(validate_compose),
        )
        .route(
            "/ui/services/{uuid}/compose/label-escape",
            post(save_label_escape),
        )
        .route("/ui/services/{uuid}/action/{action}", post(action))
        .route(
            "/ui/services/{uuid}/resources/{resource}",
            get(resources::show)
                .patch(resources::submit)
                .delete(resources::delete),
        )
        .route(
            "/ui/services/{uuid}/resources/{resource}/restart",
            post(resources::restart),
        )
        .route(
            "/ui/services/{uuid}/resources/{resource}/remove-container",
            post(resources::remove_container),
        )
        .route(
            "/ui/services/{uuid}/resources/{resource}/convert",
            post(resources::convert),
        )
        .route(
            "/ui/services/{uuid}/resources/{resource}/settings",
            post(resources::instant_settings),
        )
        .route(
            "/ui/services/{uuid}/resources/{resource}/public",
            post(resources::public_access),
        )
        .route("/ui/services/{uuid}/storages", get(storage::index))
        .route(
            "/ui/services/{uuid}/file-storages/{id}",
            axum::routing::patch(storage::file_save).delete(storage::file_delete),
        )
        .route(
            "/ui/services/{uuid}/file-storages/{id}/load",
            post(storage::file_load),
        )
        .route(
            "/ui/services/{uuid}/file-storages/{id}/convert",
            post(storage::file_convert),
        )
        .route_layer(from_fn_with_state(state, crate::session::ui_guard))
}

pub(crate) const NOT_FOUND: ApiError =
    ApiError::Message(StatusCode::NOT_FOUND, "Service not found.");

pub(crate) async fn find_service(
    state: &AppState,
    ctx: &AuthCtx,
    uuid: &str,
) -> Result<ServiceRow, ApiError> {
    svc::find_in_team(&state.db, ctx.team_id, uuid)
        .await?
        .ok_or(NOT_FOUND)
}

pub(crate) fn jobs_of(state: &AppState) -> Result<&coolify_jobs::Jobs, ApiError> {
    state.jobs.as_ref().ok_or(ApiError::NotPorted)
}

/// `$this->authorize('update', $service)` and the other admin abilities of `ServicePolicy`.
pub(crate) fn require_admin(ctx: &AuthCtx) -> Result<(), ApiError> {
    if ctx.is_team_admin() {
        Ok(())
    } else {
        Err(ApiError::Unauthorized)
    }
}

/// A `handleError($e, $this)` toast, as the other session endpoints answer it.
pub(crate) fn toast(message: impl Into<String>) -> Response {
    toast_error(StatusCode::UNPROCESSABLE_ENTITY, message)
}

/// The service's variables with their decrypted values.
pub(crate) async fn env_values(
    state: &AppState,
    id: i64,
) -> Result<Vec<(Record, Option<String>)>, ApiError> {
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let rows = svc::env_rows(&state.db, id).await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            let value = r
                .get("value")
                .and_then(Value::as_str)
                .filter(|v| !v.is_empty())
                .and_then(|v| enc.decrypt_maybe_serialized(v).ok());
            (r, value)
        })
        .collect())
}

/// `Service::documentation()`.
fn documentation(s: &ServiceRow) -> String {
    let name = s.name();
    let key = name.rsplit_once('-').map_or(name, |x| x.0);
    let key = templates::resolve_service_template_key(key);
    templates::get_service_templates()
        .get(&key)
        .and_then(|t| t.get("documentation"))
        .and_then(Value::as_str)
        .unwrap_or("https://coolify.io/docs")
        .to_owned()
}

/// `service_logo_urls()`, without the host.
fn logo_urls(logo: Option<&str>) -> (String, String, String) {
    const DEFAULT: &str = "svgs/default.webp";
    let basename = |v: &str| v.rsplit('/').next().unwrap_or(v).to_owned();
    let Some(logo) = logo.filter(|l| !l.is_empty() && basename(l) != basename(DEFAULT)) else {
        let d = format!("/{DEFAULT}");
        return (d.clone(), d.clone(), d);
    };
    let logo = match logo.strip_prefix("svg/") {
        Some(rest) => format!("svgs/{rest}"),
        None => logo.to_owned(),
    };
    let logo = logo.trim_start_matches('/');
    (
        format!("/{logo}"),
        format!(
            "https://raw.githubusercontent.com/coollabsio/coolify/refs/heads/main/public/{logo}"
        ),
        format!("/{DEFAULT}"),
    )
}

/// `resource_logo($service)`: `null` without a template logo.
pub(crate) fn resource_logo(s: &ServiceRow) -> Value {
    let name = s.name();
    let kind = s
        .str("service_type")
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| name.rsplit_once('-').map_or(name, |x| x.0));
    let key = templates::resolve_service_template_key(kind);
    let logo = templates::get_service_templates()
        .get(&key)
        .and_then(|t| t.get("logo"))
        .and_then(Value::as_str);
    match logo.filter(|l| !l.is_empty() && !l.ends_with("default.webp")) {
        None => Value::Null,
        Some(l) => {
            let (src, cdn, _) = logo_urls(Some(l));
            json!({"src": src, "fallback": cdn})
        }
    }
}

/// `base_url(withPort: false)`.
async fn base_url(state: &AppState) -> Result<String, ApiError> {
    let (fqdn, ipv4, ipv6) = coolify_db::servers::instance_urls(&state.db).await?;
    if let Some(f) = fqdn.filter(|f| !f.is_empty()) {
        return Ok(f);
    }
    if let Some(ip) = ipv4.filter(|f| !f.is_empty()) {
        return Ok(format!("http://{ip}"));
    }
    if let Some(ip) = ipv6.filter(|f| !f.is_empty()) {
        return Ok(format!("http://[{ip}]"));
    }
    Ok("http://localhost".to_owned())
}

/// `View\Components\Services\Links`.
async fn links(
    state: &AppState,
    apps: &[SubRow],
    rows: &[(Record, Option<String>)],
) -> Result<Vec<String>, ApiError> {
    let mut out = Vec::new();
    for app in apps {
        if let Some(kind) =
            coolify_jobs::compose_parser::service_parser::service_type(app.str("image"))
        {
            // `generateServiceSpecificFqdns($application)`: the parser stored these values.
            let keys: &[&str] = if kind.contains("minio") || kind.contains("pgsty/silo") {
                &["MINIO_BROWSER_REDIRECT_URL", "MINIO_SERVER_URL"]
            } else if kind.contains("logto") {
                &["LOGTO_ENDPOINT", "LOGTO_ADMIN_ENDPOINT"]
            } else if kind.contains("garage") {
                &["GARAGE_S3_API_URL", "GARAGE_WEB_URL", "GARAGE_ADMIN_URL"]
            } else {
                &[]
            };
            let values: Vec<&str> = keys
                .iter()
                .filter_map(|k| {
                    rows.iter()
                        .find(|(r, _)| r.get("key").and_then(Value::as_str) == Some(*k))
                        .and_then(|(_, v)| v.as_deref())
                })
                .collect();
            if values.len() == keys.len() {
                out.extend(values.iter().map(|l| get_fqdn_without_port(l)));
            }
            continue;
        }
        if let Some(fqdn) = app.str("fqdn").filter(|f| !f.is_empty()) {
            out.extend(fqdn.split(',').map(get_fqdn_without_port));
        }
        if let Some(ports) = app.str("ports").filter(|p| !p.is_empty()) {
            let base = base_url(state).await?;
            for port in ports.split(',') {
                let host = port.split_once(':').map_or(port, |x| x.0);
                out.push(format!("{base}:{host}"));
            }
        }
    }
    Ok(out)
}

/// `redactLockedValuesInGeneratedCompose()` for the variables of the service.
fn redact_locked(compose: Option<&str>, rows: &[(Record, Option<String>)]) -> Option<String> {
    let mut compose = compose?.to_owned();
    if compose.trim().is_empty() {
        return Some(compose);
    }
    let mut variants: Vec<String> = Vec::new();
    for (r, value) in rows {
        if !r
            .get("is_shown_once")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            continue;
        }
        let Some(value) = value else { continue };
        let encoded = serde_json::to_string(value).unwrap_or_default();
        let inner = encoded
            .strip_prefix('"')
            .and_then(|e| e.strip_suffix('"'))
            .unwrap_or_default()
            .to_owned();
        variants.extend([
            value.clone(),
            value.replace('$', "$$"),
            value.replace('\'', "''"),
            inner,
        ]);
    }
    variants.retain(|v| v.chars().count() >= 4);
    variants.sort_by_key(|v| std::cmp::Reverse(v.len()));
    variants.dedup();
    for v in variants {
        compose = compose.replace(&v, "<locked>");
    }
    Some(compose)
}

/// `ServiceDatabase::databaseType()`.
pub(crate) fn database_type(sub: &SubRow) -> String {
    if let Some(custom) = sub.str("custom_type").filter(|c| !c.trim().is_empty()) {
        return format!("standalone-{custom}");
    }
    let image = sub.str("image").unwrap_or_default();
    let image = image.split(':').next().unwrap_or_default();
    let name = if image.contains("supabase/postgres") {
        "supabase/postgres"
    } else if image.contains("timescale")
        || image.contains("pgvector")
        || image.contains("postgres")
        || image.contains("postgis")
    {
        "postgresql"
    } else {
        image
    };
    format!("standalone-{name}")
}

/// `ServiceDatabase::isBackupSolutionAvailable()`.
pub(crate) fn backup_solution_available(sub: &SubRow) -> bool {
    let t = database_type(sub);
    ["mysql", "postgres", "postgis", "mariadb", "mongo"]
        .iter()
        .any(|n| t.contains(n))
        || sub.str("custom_type").is_some_and(|c| !c.trim().is_empty())
}

/// What the resource cards and pages read of a compose application or database.
pub(crate) fn sub_json(sub: &SubRow) -> Value {
    let flag = |k: &str| sub.raw.get(k).cloned().unwrap_or(Value::Null);
    json!({
        "id": sub.id,
        "uuid": sub.uuid,
        "name": sub.name,
        "human_name": flag("human_name"),
        "description": flag("description"),
        "image": flag("image"),
        "status": flag("status"),
        "fqdn": flag("fqdn"),
        "configuration_required": sub.bool("configuration_required"),
        "container_present": flag("container_present"),
        "restart_limit_reached": sub.bool("restart_limit_reached"),
        "restart_count": flag("restart_count"),
        "max_restart_count": flag("max_restart_count"),
        "is_migrated": sub.bool("is_migrated"),
        "backup_available": !sub.is_application() && backup_solution_available(sub),
    })
}

/// `$this->service->applications->sort()`: `id` order in practice, as the collection holds them.
async fn subs_by_id(state: &AppState, kind: SubKind, id: i64) -> Result<Vec<SubRow>, ApiError> {
    let mut rows = svc::subs(&state.db, kind, id).await?;
    rows.sort_by_key(|r| r.id);
    Ok(rows)
}

/// `Service::isDeployable`: no required variable without a value.
fn is_deployable(rows: &[(Record, Option<String>)]) -> bool {
    !rows.iter().any(|(r, v)| {
        r.get("is_required")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            && v.as_deref().unwrap_or_default().is_empty()
    })
}

async fn fields_for(
    state: &AppState,
    ctx: &AuthCtx,
    s: &ServiceRow,
    apps: &[SubRow],
    databases: &[SubRow],
    rows: &[(Record, Option<String>)],
) -> Result<Vec<StackField>, ApiError> {
    let _ = state;
    let _ = s;
    let image = |r: &SubRow| r.str("image").unwrap_or_default().to_owned();
    Ok(stack_fields(
        &apps.iter().map(image).collect::<Vec<_>>(),
        &databases.iter().map(image).collect::<Vec<_>>(),
        rows,
        !ctx.is_team_admin(),
    ))
}

/// `Project\Service\Configuration`, `Heading`, `StackForm` and `EditCompose`: what they load.
pub async fn page(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let s = find_service(&state, &ctx, &uuid).await?;
    let apps = subs_by_id(&state, SubKind::Application, s.id).await?;
    let databases = subs_by_id(&state, SubKind::Database, s.id).await?;
    let rows = env_values(&state, s.id).await?;
    let fields = fields_for(&state, &ctx, &s, &apps, &databases, &rows).await?;
    let admin = ctx.is_team_admin();
    let status = crate::routes::services::status_of(&state, &s).await?;
    let functional = match s.server_id() {
        Some(id) => coolify_db::servers::find_by_id(&state.db, id)
            .await?
            .is_some_and(|srv| server_functional(&srv)),
        None => false,
    };
    let jobs = jobs_of(&state)?;
    // `isConfigurationChanged()`, with `!is_null($resource->config_hash)`.
    let saved = s.str("config_hash");
    let changed = saved.is_some()
        && jobs
            .service_configuration_hash(&s)
            .await
            .is_ok_and(|h| Some(h.as_str()) != saved);
    let activity = svc::latest_activity_with_id(&state.db, &s.uuid)
        .await?
        .map(|(id, props, description)| {
            let entries: Value =
                serde_json::from_str(&description).unwrap_or(Value::Array(vec![]));
            json!({"id": id, "status": props.get("status").cloned().unwrap_or(Value::Null), "entries": entries})
        });
    let raw = |k: &str| s.raw.get(k).cloned().unwrap_or(Value::Null);
    Ok(ok(json!({
        "service": {
            "id": s.id,
            "uuid": s.uuid,
            "name": s.name(),
            "description": raw("description"),
            "status": status,
            "service_type": raw("service_type"),
            "compose_parsing_version": raw("compose_parsing_version"),
            "connect_to_docker_network": s.bool("connect_to_docker_network"),
            "is_container_label_escape_enabled": s.bool("is_container_label_escape_enabled"),
            "docker_compose_raw": if admin { raw("docker_compose_raw") } else { Value::Null },
            "docker_compose": if admin {
                json!(redact_locked(s.str("docker_compose"), &rows))
            } else {
                Value::Null
            },
            "created_at": raw("created_at"),
            "updated_at": raw("updated_at"),
        },
        "documentation": documentation(&s),
        "logo": resource_logo(&s),
        "links": links(&state, &apps, &rows).await?,
        "is_deployable": is_deployable(&rows),
        "server_functional": functional,
        "configuration_changed": changed,
        "activity": activity,
        "can_update": admin,
        "can_access_terminal": admin,
        "is_password_hidden": !admin,
        "fields": fields.iter().map(StackField::to_json).collect::<Vec<_>>(),
        "applications": apps.iter().map(sub_json).collect::<Vec<_>>(),
        "databases": databases.iter().map(sub_json).collect::<Vec<_>>(),
    }))
    .into_response())
}

const STACK_MESSAGES: Messages = &[
    ("name.required", "The Name field is required."),
    (
        "name.regex",
        "The name may only contain letters (including Unicode), numbers, spaces, and these characters: - _ . : / () ' \" , ! ? @ # % & + = [] {} | ~ ` *",
    ),
    ("name.min", "The name must be at least :min characters."),
    (
        "name.max",
        "The name may not be greater than :max characters.",
    ),
    ("name.string", "The name must be a string."),
    (
        "description.regex",
        "The description may only contain letters (including Unicode), numbers, spaces, and common punctuation: - _ . , ! ? ( ) ' \" + = * / @ &",
    ),
    (
        "description.max",
        "The description may not be greater than :max characters.",
    ),
    ("description.string", "The description must be a string."),
];

/// The rules of a template field (`'required|email'` ...), with `validationAttributes` as the name.
fn field_error(field: &StackField, value: Option<&str>) -> Option<String> {
    let value = value.unwrap_or_default();
    let name = &field.name;
    for rule in field.rules.split('|') {
        match rule {
            "required" if value.trim().is_empty() => {
                return Some(format!("The {name} field is required."));
            }
            "email" if !value.is_empty() && !is_email(value) => {
                return Some(format!("The {name} field must be a valid email address."));
            }
            "url" if !value.is_empty() && !is_url(value) => {
                return Some(format!("The {name} field must be a valid URL."));
            }
            r if r.starts_with("in:")
                && !value.is_empty()
                && !r[3..].split(',').any(|v| v == value) =>
            {
                return Some(format!("The selected {name} is invalid."));
            }
            _ => {}
        }
    }
    None
}

fn is_email(v: &str) -> bool {
    let Some((local, domain)) = v.rsplit_once('@') else {
        return false;
    };
    !local.is_empty() && !domain.is_empty() && !v.contains(char::is_whitespace)
}

fn is_url(v: &str) -> bool {
    let Some((scheme, rest)) = v.split_once("://") else {
        return false;
    };
    !scheme.is_empty()
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c))
        && !rest.is_empty()
        && !v.contains(char::is_whitespace)
}

/// `Service::saveExtraFields`.
async fn save_extra_fields(
    state: &AppState,
    s: &ServiceRow,
    fields: &[(String, Option<String>)],
) -> Result<(), ApiError> {
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let now = state.now();
    for (key, value) in fields {
        if key.is_empty() {
            continue;
        }
        let encrypted = value.as_deref().map(|v| enc.encrypt_serialized(v.trim()));
        if let Some(row) = svc::env_row_by_key(&state.db, s.id, key).await? {
            let current = row
                .get("value")
                .and_then(Value::as_str)
                .and_then(|v| enc.decrypt_maybe_serialized(v).ok());
            if current.as_deref().map(str::trim) == value.as_deref().map(str::trim) {
                continue;
            }
            let id = row.get("id").and_then(Value::as_i64).unwrap_or_default();
            svc::env_update_columns(
                &state.db,
                id,
                &[("value", ColumnValue::Text(encrypted))],
                now,
            )
            .await?;
        } else {
            coolify_db::compose::env_insert(
                &state.db,
                svc::MORPH,
                s.id,
                &NewEnv {
                    key: key.clone(),
                    value_encrypted: encrypted,
                    is_preview: false,
                    is_required: false,
                    is_multiline: false,
                    is_literal: false,
                    is_shown_once: false,
                    is_shared: false,
                    comment: None,
                },
                &state.version,
                now,
            )
            .await?;
        }
    }
    Ok(())
}

/// `StackForm::submit` (`saveCompose` sends the edited Compose file through it).
#[allow(clippy::too_many_lines)]
pub async fn submit(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    body: axum::body::Bytes,
) -> ApiResult {
    let s = find_service(&state, &ctx, &uuid).await?;
    require_admin(&ctx)?;
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let input = Input(body.as_object().cloned().unwrap_or_default());
    let compose = body
        .get("docker_compose_raw")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| s.str("docker_compose_raw").map(str::to_owned));
    let mut errors = Map::new();
    if compose.as_deref().unwrap_or_default().trim().is_empty() {
        errors.insert(
            "docker_compose_raw".into(),
            json!(["The Docker Compose Raw field is required."]),
        );
    }
    if let Err(ApiError::Validation(m)) = validate(
        &input,
        &[patterns::name(true), patterns::description()],
        STACK_MESSAGES,
        &[
            "name",
            "description",
            "docker_compose_raw",
            "connect_to_docker_network",
            "fields",
        ],
    ) {
        errors.extend(m);
    }
    let apps = subs_by_id(&state, SubKind::Application, s.id).await?;
    let databases = subs_by_id(&state, SubKind::Database, s.id).await?;
    let rows = env_values(&state, s.id).await?;
    let fields = fields_for(&state, &ctx, &s, &apps, &databases, &rows).await?;
    let sent = body.get("fields").and_then(Value::as_object);
    let mut to_save: Vec<(String, Option<String>)> = Vec::new();
    for field in fields.iter().filter(|f| !f.is_locked) {
        let value = sent
            .and_then(|m| m.get(&field.key))
            .map_or(field.value.clone(), |v| v.as_str().map(str::to_owned));
        if let Some(message) = field_error(field, value.as_deref()) {
            errors.insert(format!("fields.{}.value", field.key), json!([message]));
        }
        to_save.push((field.key.clone(), value));
    }
    if !errors.is_empty() {
        return Err(ApiError::Validation(errors));
    }
    let compose = compose.unwrap_or_default();
    // `validateDockerComposeForInjection` before any database operation.
    if let Err(e) = coolify_jobs::services::validate_compose(&compose) {
        return Ok(toast(e));
    }
    let name = input.str("name").map(|n| strip_tags(Some(n)));
    let description = input.str("description").map(str::to_owned);
    let before: Vec<(&'static str, ColumnValue)> = vec![
        ("name", ColumnValue::Text(Some(s.name().to_owned()))),
        (
            "description",
            ColumnValue::Text(s.str("description").map(str::to_owned)),
        ),
        (
            "docker_compose_raw",
            ColumnValue::Text(s.str("docker_compose_raw").map(str::to_owned)),
        ),
        (
            "connect_to_docker_network",
            ColumnValue::Bool(s.bool("connect_to_docker_network")),
        ),
    ];
    let connect = body
        .get("connect_to_docker_network")
        .and_then(Value::as_bool)
        .unwrap_or_else(|| s.bool("connect_to_docker_network"));
    let values: Vec<(&'static str, ColumnValue)> = vec![
        ("name", ColumnValue::Text(name)),
        ("description", ColumnValue::Text(description)),
        ("docker_compose_raw", ColumnValue::Text(Some(compose))),
        ("connect_to_docker_network", ColumnValue::Bool(connect)),
    ];
    let now = state.now();
    let jobs = jobs_of(&state)?;
    // The upstream transaction: a parse failure leaves the service as it was.
    svc::update(&state.db, s.id, &values, now).await?;
    let previous: Vec<(String, Option<String>)> = rows
        .iter()
        .filter_map(|(r, v)| {
            let key = r.get("key").and_then(Value::as_str)?;
            to_save
                .iter()
                .any(|(k, _)| k == key)
                .then(|| (key.to_owned(), v.clone()))
        })
        .collect();
    save_extra_fields(&state, &s, &to_save).await?;
    if let Err(e) = jobs.parse_service(s.id).await {
        svc::update(&state.db, s.id, &before, state.now()).await?;
        save_extra_fields(&state, &s, &previous).await?;
        return Ok(toast(e));
    }
    let fresh = svc::find_by_id(&state.db, s.id)
        .await?
        .ok_or(ApiError::Internal)?;
    // `saveComposeConfigs()` after the commit.
    let saved = match jobs.service_target(&fresh).await {
        Ok((_, target)) => jobs.save_compose_configs(&fresh, &target).await,
        Err(e) => Err(e),
    };
    // `finally`: a service without a hash gets one.
    if fresh.str("config_hash").is_none()
        && let Ok(hash) = jobs.service_configuration_hash(&fresh).await
    {
        svc::update(
            &state.db,
            s.id,
            &[("config_hash", ColumnValue::Text(Some(hash)))],
            state.now(),
        )
        .await?;
    }
    if let Err(e) = saved {
        return Ok(toast(e));
    }
    Ok(ok(json!({"message": "Service saved."})).into_response())
}

/// `StackForm::instantSave`.
pub async fn save_network(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    body: axum::body::Bytes,
) -> ApiResult {
    let s = find_service(&state, &ctx, &uuid).await?;
    require_admin(&ctx)?;
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let connect = body
        .get("connect_to_docker_network")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    svc::update(
        &state.db,
        s.id,
        &[("connect_to_docker_network", ColumnValue::Bool(connect))],
        state.now(),
    )
    .await?;
    Ok(ok(json!({"message": "Service settings saved."})).into_response())
}

/// `EditCompose::validateCompose` (`validateComposeFile`: `docker compose config` on the server).
pub async fn validate_compose(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    body: axum::body::Bytes,
) -> ApiResult {
    let s = find_service(&state, &ctx, &uuid).await?;
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let compose = body
        .get("docker_compose_raw")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let jobs = jobs_of(&state)?;
    match jobs.validate_compose_file(&s, compose).await {
        Ok(()) => Ok(ok(json!({"message": "Docker compose is valid."})).into_response()),
        Err(e) => Ok(toast(format!("Invalid docker-compose file.\n{e}"))),
    }
}

/// `EditCompose::instantSave`.
pub async fn save_label_escape(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    body: axum::body::Bytes,
) -> ApiResult {
    let s = find_service(&state, &ctx, &uuid).await?;
    require_admin(&ctx)?;
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let enabled = body
        .get("enabled")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    svc::update(
        &state.db,
        s.id,
        &[(
            "is_container_label_escape_enabled",
            ColumnValue::Bool(enabled),
        )],
        state.now(),
    )
    .await?;
    Ok(ok(json!({"message": "Service updated successfully"})).into_response())
}

/// `Heading`: `start`, `forceDeploy`, `restart`, `pullAndRestartEvent` and `stop`.
pub async fn action(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((uuid, action)): Path<(String, String)>,
    body: axum::body::Bytes,
) -> ApiResult {
    let s = find_service(&state, &ctx, &uuid).await?;
    require_admin(&ctx)?;
    let jobs = jobs_of(&state)?;
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let running = svc::has_running_start(&state.db, &s.uuid).await?;
    let audit = |event: &'static str| {
        tracing::info!(target: "audit", event = event, team_id = ctx.team_id, service_uuid = %s.uuid, service_name = %s.name(), "service action");
    };
    match action.as_str() {
        "start" => {
            jobs.spawn_service_start(s.id, true, false);
            audit("ui.service.started");
        }
        "force-deploy" => {
            svc::cancel_running_activities(&state.db, &s.uuid, state.now()).await?;
            jobs.spawn_service_start(s.id, true, true);
        }
        "restart" | "pull-restart" => {
            if running {
                return Ok(toast("There is a deployment in progress."));
            }
            jobs.spawn_service_start(s.id, action == "pull-restart", true);
            audit("ui.service.restarted");
        }
        "stop" => {
            let cleanup = body
                .get("docker_cleanup")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            jobs.spawn_service_stop_with(s.clone(), cleanup);
            audit("ui.service.stopped");
        }
        _ => return Err(ApiError::NotPorted),
    }
    Ok(ok(json!({"message": "queued"})).into_response())
}

/// `Project\Resource\Create` for `?type=one-click-service-{name}`.
pub async fn create(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    body: axum::body::Bytes,
) -> ApiResult {
    require_admin(&ctx)?;
    let input: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let text = |k: &str| {
        input
            .get(k)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    let project = coolify_db::projects::find(&state.db, ctx.team_id, &text("project_uuid"))
        .await?
        .ok_or(ApiError::Message(
            StatusCode::NOT_FOUND,
            "Project not found.",
        ))?;
    let env =
        coolify_db::projects::find_environment(&state.db, project.id, &text("environment_uuid"))
            .await?
            .ok_or(ApiError::Message(
                StatusCode::NOT_FOUND,
                "Environment not found.",
            ))?;
    let (destination_id, destination_type, server_id) =
        svc::team_destination_by_uuid(&state.db, ctx.team_id, &text("destination_uuid"))
            .await?
            .ok_or(ApiError::Message(
                StatusCode::NOT_FOUND,
                "Destination not found.",
            ))?;
    let kind = text("type");
    let kind = kind.strip_prefix("one-click-service-").unwrap_or(&kind);
    let kind = templates::resolve_service_template_key(kind);
    let Some(compose) = templates::template_compose(&kind) else {
        return Err(ApiError::Message(
            StatusCode::NOT_FOUND,
            "Service not found.",
        ));
    };
    let now = state.now();
    let connect = templates::NEEDS_TO_CONNECT_TO_PREDEFINED_NETWORK.contains(&kind.as_str());
    let (id, uuid) = svc::create(
        &state.db,
        &NewService {
            uuid: None,
            name: format!("{kind}-{}", crate::routes::services::random_str(10)),
            description: None,
            docker_compose_raw: compose,
            environment_id: env.id,
            service_type: Some(kind.clone()),
            server_id,
            destination_id,
            destination_type: destination_type.to_owned(),
            connect_to_docker_network: connect.then_some(true),
            is_container_label_escape_enabled: None,
            redact_all_env_values_in_logs: None,
        },
        "5",
        now,
    )
    .await?;
    svc::update(
        &state.db,
        id,
        &[("name", ColumnValue::Text(Some(format!("{kind}-{uuid}"))))],
        now,
    )
    .await?;
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    for line in templates::template_envs(&kind) {
        let (key, value) = line
            .split_once('=')
            .unwrap_or((line.as_str(), line.as_str()));
        coolify_db::compose::env_insert(
            &state.db,
            svc::MORPH,
            id,
            &NewEnv {
                key: key.to_owned(),
                value_encrypted: Some(enc.encrypt_serialized(value.trim())),
                is_preview: false,
                is_required: false,
                is_multiline: false,
                is_literal: false,
                is_shown_once: false,
                is_shared: false,
                comment: None,
            },
            &state.version,
            now,
        )
        .await?;
    }
    let jobs = jobs_of(&state)?;
    // `$service->parse(isNew: true)`: the new model has no column defaults loaded.
    if let Err(e) = jobs.parse_new_service(id, false).await {
        tracing::error!(error = %e, "service parse failed");
        return Ok(toast(e));
    }
    crate::routes::services::apply_prerequisites(&state, id, &kind).await?;
    tracing::info!(target: "audit", event = "service.created", team_id = ctx.team_id, service_uuid = %uuid, service_type = %kind, "service created");
    Ok(created(json!({"uuid": uuid})).into_response())
}

/// `serviceWebsiteUrl()`.
fn website_url(documentation: Option<&str>) -> Value {
    let Some(doc) = documentation else {
        return Value::Null;
    };
    let Some((scheme, rest)) = doc.split_once("://") else {
        return Value::Null;
    };
    if !["http", "https"].contains(&scheme) {
        return Value::Null;
    }
    let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
    let host = host.split(['?', '#']).next().unwrap_or_default();
    if host.is_empty() {
        return Value::Null;
    }
    let lower = host.to_lowercase();
    let segments = match lower.strip_prefix("www.").unwrap_or(&lower) {
        "github.com" | "gitlab.com" | "codeberg.org" => 2,
        "hub.docker.com" => 3,
        _ => 0,
    };
    let path = path.split(['?', '#']).next().unwrap_or_default();
    let project: Vec<&str> = path
        .split('/')
        .filter(|p| !p.is_empty() && *p != "0")
        .take(segments)
        .collect();
    let url = format!("{scheme}://{host}/{}", project.join("/"));
    json!(url.trim_end_matches('/'))
}

/// `Str::headline`.
fn headline(v: &str) -> String {
    let mut words: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut prev_lower = false;
    for c in v.chars() {
        if c == '-' || c == '_' || c == ' ' {
            if !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
            prev_lower = false;
            continue;
        }
        if c.is_uppercase() && prev_lower {
            words.push(std::mem::take(&mut current));
        }
        prev_lower = c.is_lowercase() || c.is_ascii_digit();
        current.push(c);
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
        .iter()
        .map(|w| {
            let mut chars = w.chars();
            chars.next().map_or_else(String::new, |f| {
                f.to_uppercase().collect::<String>() + chars.as_str()
            })
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// `Select::loadServices`: the templates and their categories.
pub async fn service_templates(State(_state): State<AppState>) -> ApiResult {
    let mut services = Vec::new();
    let mut categories: Vec<String> = Vec::new();
    for (key, t) in templates::get_service_templates() {
        let text = |k: &str| t.get(k).and_then(Value::as_str);
        let (logo, cdn, default) = logo_urls(text("logo"));
        for category in text("category")
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|c| !c.is_empty())
        {
            const ACRONYMS: &[&str] = &[
                "ai", "api", "ci", "cd", "cms", "crm", "erp", "iot", "vpn", "vps", "dns", "ssl",
                "tls", "ssh", "ftp", "http", "https",
            ];
            let c = if ACRONYMS.contains(&category.to_lowercase().as_str()) {
                category.to_uppercase()
            } else {
                category.to_owned()
            };
            if !categories.contains(&c) {
                categories.push(c);
            }
        }
        services.push(json!({
            "id": key,
            "name": headline(key),
            "docsSlug": key.to_lowercase(),
            "templateLastUpdated": text("template_last_updated_at").and_then(format_timestamp),
            "website": website_url(text("documentation")),
            "logo": logo,
            "logo_cdn_url": cdn,
            "logo_default_url": default,
            "slogan": text("slogan"),
            "description": text("description"),
            "documentation": text("documentation"),
            "category": text("category"),
            "tags": t.get("tags").cloned().unwrap_or(Value::Null),
            "amd_only": t.get("amd_only").cloned().unwrap_or(Value::Null),
            "arm_only": t.get("arm_only").cloned().unwrap_or(Value::Null),
        }));
    }
    categories.sort_by_key(|c| c.to_lowercase());
    Ok(ok(json!({"services": services, "categories": categories})).into_response())
}

/// `CarbonImmutable::parse($timestamp)->format('M j, Y H:i')`.
fn format_timestamp(v: &str) -> Option<String> {
    let parsed = chrono::DateTime::parse_from_rfc3339(v).ok()?;
    Some(
        parsed
            .with_timezone(&chrono::Utc)
            .format("%b %-d, %Y %H:%M")
            .to_string(),
    )
}

/// `Project\Resource\Index`: an environment's services, for its resource list.
pub async fn environment_services(
    state: &AppState,
    environment_id: i64,
) -> Result<Vec<Value>, ApiError> {
    let mut out = Vec::new();
    for s in svc::list_for_environment(&state.db, environment_id).await? {
        let server = match s.server_id() {
            Some(id) => coolify_db::servers::find_by_id(&state.db, id).await?,
            None => None,
        };
        let tags: Vec<Value> = svc::tags_of(&state.db, s.id)
            .await?
            .into_iter()
            .map(|t| json!({"id": t.get("id"), "name": t.get("name")}))
            .collect();
        out.push(json!({
            "uuid": s.uuid,
            "name": s.name(),
            "description": s.raw.get("description"),
            "status": crate::routes::services::status_of(state, &s).await?,
            "logo": resource_logo(&s),
            "server": server.as_ref().and_then(|x| x.record.get("name")).cloned(),
            "tags": tags,
        }));
    }
    Ok(out)
}
