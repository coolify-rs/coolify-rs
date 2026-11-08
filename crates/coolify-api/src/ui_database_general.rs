//! Session endpoints behind the General pages of a database: the actions of
//! `Project\Database\<Type>\General` (Postgresql, Mysql, Mariadb, Mongodb, Redis, Keydb, Dragonfly,
//! Clickhouse, Sqlite), `<Type>\StatusInfo` (`HasDatabaseStatusInfo`), `InitScript`,
//! `Project\Database\Health` and `Sqlite\ConnectApplication`.
//!
//! A Livewire `dispatch('success'|'error'|'info', ...)` is a 200 answer with `toasts`.

use std::sync::LazyLock;

use axum::Extension;
use axum::Router;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::middleware::from_fn_with_state;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use coolify_db::database_pages as pages;
use coolify_db::database_parts as parts;
use coolify_db::databases::{self as dbs, DatabaseRow, Kind};
use coolify_db::servers::{self, ColumnValue};
use coolify_domain::new_public_id;
use coolify_jobs::compose_parser::{php, validate, volumes};
use coolify_jobs::server::ServerInfo;
use regex::Regex;
use serde_json::{Map, Value, json};

use crate::audit::Meta;
use crate::auth::AuthCtx;
use crate::response::{ApiError, ApiResult, ok};
use crate::state::AppState;

const NOT_FOUND: ApiError = ApiError::Message(StatusCode::NOT_FOUND, "Database not found.");

pub fn routes(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/ui/databases/{uuid}/public", post(instant_save))
        .route(
            "/ui/databases/{uuid}/log-drain",
            post(instant_save_advanced),
        )
        .route("/ui/databases/{uuid}/ssl", post(instant_save_ssl))
        .route("/ui/databases/{uuid}/ssl/regenerate", post(regenerate_ssl))
        .route("/ui/databases/{uuid}/proxy-logs", get(proxy_logs))
        .route("/ui/databases/{uuid}/init-scripts", post(init_scripts))
        .route("/ui/databases/{uuid}/healthcheck", post(health))
        .route(
            "/ui/databases/{uuid}/anonymous-volume",
            get(anonymous_volume).post(keep_anonymous_volume),
        )
        .route(
            "/ui/databases/{uuid}/sqlite/connect",
            get(sqlite_state).post(sqlite_connect),
        )
        .route(
            "/ui/databases/{uuid}/sqlite/unlink/{volume}",
            post(sqlite_unlink),
        )
        .route_layer(from_fn_with_state(state, crate::session::ui_guard))
}

pub(crate) fn toasts(items: &[(&str, &str)]) -> Value {
    json!({"toasts": items.iter().map(|(t, m)| json!({"type": t, "message": m})).collect::<Vec<_>>()})
}

fn toast(kind: &str, message: &str) -> Response {
    ok(toasts(&[(kind, message)])).into_response()
}

async fn find(state: &AppState, ctx: &AuthCtx, uuid: &str) -> Result<DatabaseRow, ApiError> {
    dbs::find_in_team(&state.db, ctx.team_id, uuid)
        .await?
        .ok_or(NOT_FOUND)
}

/// `$this->authorize('update', $this->database)`.
async fn find_admin(state: &AppState, ctx: &AuthCtx, uuid: &str) -> Result<DatabaseRow, ApiError> {
    let d = find(state, ctx, uuid).await?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    Ok(d)
}

pub(crate) async fn server_of(
    state: &AppState,
    d: &DatabaseRow,
) -> Result<Option<ServerInfo>, ApiError> {
    let Some(id) = d.server_id() else {
        return Ok(None);
    };
    Ok(servers::find_by_id(&state.db, id)
        .await?
        .map(|r| ServerInfo::from_row(&r)))
}

async fn refresh_config_hash(state: &AppState, ctx: &AuthCtx, uuid: &str) -> Result<(), ApiError> {
    let Some(fresh) = dbs::find_in_team(&state.db, ctx.team_id, uuid).await? else {
        return Ok(());
    };
    if fresh.str("config_hash").is_none() {
        let hash = crate::routes::databases::config_hash(state, &fresh).await?;
        dbs::update_if_changed(
            &state.db,
            fresh.kind,
            fresh.id,
            &[("config_hash", ColumnValue::Text(Some(hash)))],
            state.now(),
        )
        .await?;
    }
    Ok(())
}

/// `General::instantSave`: makes the database public or private. The form is saved first
/// (`syncData(true)`), as upstream's single round trip does.
async fn instant_save(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    body: Bytes,
) -> ApiResult {
    let d = find_admin(&state, &ctx, &uuid).await?;
    let input: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let is_public = input
        .get("is_public")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let form = input.get("form").cloned().unwrap_or(json!({}));
    let port = match form.get("public_port") {
        Some(Value::Number(n)) => n.as_i64(),
        Some(Value::String(s)) => s.trim().parse::<i64>().ok(),
        Some(_) => None,
        None => d.int("public_port"),
    };
    if is_public && port.is_none_or(|p| p == 0) {
        return Ok(toast("error", "Public port is required."));
    }
    if is_public && !d.str("status").unwrap_or_default().starts_with("running") {
        return Ok(toast(
            "error",
            "Database must be started to be publicly accessible.",
        ));
    }
    if form.as_object().is_some_and(|m| !m.is_empty()) {
        crate::ui_databases::update(
            State(state.clone()),
            Extension(ctx.clone()),
            Path(uuid.clone()),
            Bytes::from(form.to_string()),
        )
        .await?;
    }
    let now = state.now();
    let save = |value: bool| {
        let state = state.clone();
        let (kind, id) = (d.kind, d.id);
        async move {
            dbs::update_if_changed(
                &state.db,
                kind,
                id,
                &[("is_public", ColumnValue::Bool(value))],
                now,
            )
            .await
        }
    };
    save(is_public).await?;
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    let fresh = find(&state, &ctx, &uuid).await?;
    let result = if is_public {
        jobs.start_database_proxy(&fresh).await
    } else {
        jobs.stop_database_proxy(&fresh).await
    };
    if let Err(e) = result {
        // `catch`: the switch goes back, and handleError shows the message.
        save(!is_public).await?;
        return Ok(toast("error", &e));
    }
    Ok(toast(
        "success",
        if is_public {
            "Database is now publicly accessible."
        } else {
            "Database is no longer publicly accessible."
        },
    ))
}

/// `General::instantSaveAdvanced`: the log drain switch.
async fn instant_save_advanced(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    body: Bytes,
) -> ApiResult {
    let d = find_admin(&state, &ctx, &uuid).await?;
    let input: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let server = server_of(&state, &d).await?;
    let drain = server.as_ref().is_some_and(|s| {
        coolify_jobs::compose_parser::service::is_log_drain_enabled(&Value::Object(
            s.settings.clone(),
        ))
    });
    if !drain {
        return Ok(toast(
            "error",
            "Log drain is not enabled on the server. Please enable it first.",
        ));
    }
    if let Some(form) = input.get("form").filter(|f| f.is_object()) {
        crate::ui_databases::update(
            State(state.clone()),
            Extension(ctx.clone()),
            Path(uuid.clone()),
            Bytes::from(form.to_string()),
        )
        .await?;
    }
    let value = input
        .get("is_log_drain_enabled")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    dbs::update_if_changed(
        &state.db,
        d.kind,
        d.id,
        &[("is_log_drain_enabled", ColumnValue::Bool(value))],
        state.now(),
    )
    .await?;
    Ok(ok(toasts(&[
        ("success", "Database updated."),
        (
            "success",
            "You need to restart the service for the changes to take effect.",
        ),
    ]))
    .into_response())
}

/// `HasDatabaseStatusInfo::instantSaveSSL` with `applyExtraSslAttributes` (`ssl_mode`).
async fn instant_save_ssl(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    body: Bytes,
) -> ApiResult {
    let d = find_admin(&state, &ctx, &uuid).await?;
    if matches!(d.kind, Kind::Clickhouse | Kind::Sqlite) {
        return Err(ApiError::NotPorted);
    }
    let input: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let enabled = input
        .get("enable_ssl")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mut values = vec![("enable_ssl", ColumnValue::Bool(enabled))];
    if matches!(d.kind, Kind::Postgresql | Kind::Mysql | Kind::Mongodb)
        && let Some(mode) = input.get("ssl_mode").and_then(Value::as_str)
    {
        values.push(("ssl_mode", ColumnValue::Text(Some(mode.to_owned()))));
    }
    dbs::update_if_changed(&state.db, d.kind, d.id, &values, state.now()).await?;
    Ok(toast("success", "SSL configuration updated."))
}

/// `HasDatabaseStatusInfo::regenerateSslCertificate`.
async fn regenerate_ssl(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path(uuid): Path<String>,
) -> ApiResult {
    let d = find_admin(&state, &ctx, &uuid).await?;
    if parts::resource_certificate(&state.db, d.kind.morph(), d.id)
        .await?
        .is_none()
    {
        return Ok(toast(
            "error",
            "No existing SSL certificate found for this database.",
        ));
    }
    let Some(server) = server_of(&state, &d).await? else {
        return Err(ApiError::Internal);
    };
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    let ca = match jobs.ensure_ca_certificate(&server).await {
        Ok(ca) => ca,
        Err(e) => return Ok(toast("error", &e)),
    };
    if let Err(e) = jobs.generate_database_certificate(&d, &server, &ca).await {
        return Ok(toast("error", &e));
    }
    crate::audit::log(
        &state,
        &ctx,
        &meta,
        "ui.database.ssl_certificate_regenerated",
        json!({
            "team_id": server.team_id,
            "database_uuid": d.uuid,
            "database_name": d.str("name"),
            "database_type": d.kind.type_name(),
            "server_uuid": server.uuid,
        }),
        "info",
    )
    .await;
    Ok(toast(
        "success",
        "SSL certificates regenerated. Restart database to apply changes.",
    ))
}

/// `<livewire:project.shared.get-logs container="{uuid}-proxy" />`: the logs of the TCP proxy.
async fn proxy_logs(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> ApiResult {
    let d = find_admin(&state, &ctx, &uuid).await?;
    let Some(server) = server_of(&state, &d).await? else {
        return Ok(ok(json!({"logs": ""})).into_response());
    };
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    let lines = q
        .get("lines")
        .and_then(|l| l.parse::<i64>().ok())
        .unwrap_or(100)
        .clamp(1, 50_000);
    let timestamps = q.get("timestamps").is_some_and(|t| t == "true" || t == "1");
    let logs = jobs
        .container_logs(&server, &format!("{}-proxy", d.uuid), lines, timestamps)
        .await
        .unwrap_or_default();
    Ok(ok(json!({"logs": logs})).into_response())
}

/// `validateFilenameSafe($input, 'init script filename')`.
fn validate_filename(input: &str) -> Result<(), String> {
    const CONTEXT: &str = "init script filename";
    validate::validate_shell_safe_path(input, CONTEXT).map_err(|e| e.to_string())?;
    if input.contains('\0') {
        return Err(format!(
            "Invalid {CONTEXT}: contains null byte. Null bytes are not allowed in filenames for security reasons."
        ));
    }
    if input.contains('/') || input.contains('\\') {
        return Err(format!(
            "Invalid {CONTEXT}: directory separators ('/' or '\\') are not allowed. Provide a plain filename without path components."
        ));
    }
    if input.contains("..") {
        return Err(format!(
            "Invalid {CONTEXT}: parent directory sequence ('..') is not allowed."
        ));
    }
    for (c, what) in [
        (' ', "whitespace"),
        ('*', "glob wildcard"),
        ('?', "glob wildcard"),
        ('[', "glob character class"),
        (']', "glob character class"),
        ('~', "tilde expansion"),
        ('"', "double quote"),
        ('\'', "single quote"),
    ] {
        if input.contains(c) {
            return Err(format!(
                "Invalid {CONTEXT}: contains forbidden character '{c}' ({what})."
            ));
        }
    }
    Ok(())
}

fn reindex(scripts: Vec<Value>) -> Value {
    Value::Array(
        scripts
            .into_iter()
            .enumerate()
            .map(|(i, mut s)| {
                if let Some(m) = s.as_object_mut() {
                    m.insert("index".into(), json!(i));
                }
                s
            })
            .collect(),
    )
}

/// `Postgresql\General::save_new_init_script`, `save_init_script` and `delete_init_script`, with
/// `InitScript::submit` and `delete` (livewire/project/database/init-script.blade.php).
async fn init_scripts(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    body: Bytes,
) -> ApiResult {
    let d = find_admin(&state, &ctx, &uuid).await?;
    if d.kind != Kind::Postgresql {
        return Err(ApiError::NotPorted);
    }
    let input: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let text = |k: &str| input.get(k).and_then(Value::as_str).unwrap_or_default();
    let mut scripts: Vec<Value> = d
        .raw
        .get("init_scripts")
        .map(|v| match v {
            Value::String(s) => serde_json::from_str(s).unwrap_or(json!([])),
            other => other.clone(),
        })
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default();
    let name_of = |v: &Value| {
        v.get("filename")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    let dir = format!(
        "{}/{}/docker-entrypoint-initdb.d",
        coolify_jobs::databases::configuration_dir(),
        d.uuid
    );
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    let remove = async |name: &str| -> Result<(), String> {
        let base = name.rsplit('/').next().unwrap_or(name);
        let server = server_of(&state, &d)
            .await
            .ok()
            .flatten()
            .ok_or("Server not found.")?;
        jobs.remove_server_file(&server, &format!("{dir}/{base}"))
            .await
    };
    let message = match text("action") {
        "add" => {
            let (name, content) = (text("filename"), text("content"));
            let mut errors = Map::new();
            if name.is_empty() {
                errors.insert(
                    "new_filename".into(),
                    json!(["The new filename field is required."]),
                );
            }
            if content.is_empty() {
                errors.insert(
                    "new_content".into(),
                    json!(["The new content field is required."]),
                );
            }
            if !errors.is_empty() {
                return Err(ApiError::Validation(errors));
            }
            if let Err(e) = validate_filename(name) {
                return Ok(toast("error", &e));
            }
            if scripts.iter().any(|s| name_of(s) == name) {
                return Ok(toast("error", "Filename already exists."));
            }
            scripts.push(json!({"index": scripts.len(), "filename": name, "content": content}));
            "Init script added."
        }
        "save" => {
            let (name, content, original) =
                (text("filename"), text("content"), text("original_filename"));
            if name != original && scripts.iter().any(|s| name_of(s) == name) {
                return Ok(toast(
                    "error",
                    "A script with this filename already exists.",
                ));
            }
            let index = scripts.iter().position(|s| name_of(s) == original);
            // Only an unchanged filename of an existing script skips validation, so legacy rows
            // stay editable; a new script or a rename must be a safe filename (no path traversal).
            if (index.is_none() || name != original)
                && let Err(e) = validate_filename(name)
            {
                return Ok(toast("error", &e));
            }
            if index.is_some()
                && name != original
                && let Err(e) = remove(original).await
            {
                return Ok(toast("error", &e));
            }
            let entry = json!({"index": index.unwrap_or(scripts.len()), "filename": name, "content": content});
            match index {
                Some(i) => scripts[i] = entry,
                None => scripts.push(entry),
            }
            "Init script saved and updated."
        }
        "delete" => {
            let name = text("filename");
            if !scripts.iter().any(|s| name_of(s) == name) {
                return Ok(ok(json!({"toasts": []})).into_response());
            }
            if let Err(e) = remove(name).await {
                return Ok(toast("error", &e));
            }
            scripts.retain(|s| name_of(s) != name);
            "Init script deleted from the database and the server."
        }
        _ => {
            return Err(ApiError::Message(
                StatusCode::BAD_REQUEST,
                "Unknown action.",
            ));
        }
    };
    pages::save_init_scripts(&state.db, d.id, &reindex(scripts), state.now()).await?;
    Ok(toast("success", message))
}

fn health_int(v: Option<&Value>) -> Option<i64> {
    match v {
        Some(Value::Number(n)) => n.as_i64(),
        Some(Value::String(s)) => s.trim().parse::<i64>().ok(),
        _ => None,
    }
}

/// `Project\Database\Health::submit` and `toggleHealthcheck`.
async fn health(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    body: Bytes,
) -> ApiResult {
    let d = find_admin(&state, &ctx, &uuid).await?;
    let input: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let mut errors = Map::new();
    let mut values: Vec<(&'static str, ColumnValue)> = Vec::new();
    for (column, attribute, min) in [
        ("health_check_interval", "health check interval", 1),
        ("health_check_timeout", "health check timeout", 1),
        ("health_check_retries", "health check retries", 1),
        ("health_check_start_period", "health check start period", 0),
    ] {
        match health_int(input.get(column)) {
            None => {
                errors.insert(
                    column.into(),
                    json!([format!("The {attribute} field must be an integer.")]),
                );
            }
            Some(v) if v < min => {
                errors.insert(
                    column.into(),
                    json!([format!("The {attribute} field must be at least {min}.")]),
                );
            }
            Some(v) => values.push((column, ColumnValue::Int(v))),
        }
    }
    if !errors.is_empty() {
        return Err(ApiError::Validation(errors));
    }
    let toggle = input
        .get("toggle")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let enabled = input
        .get("health_check_enabled")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let enabled = if toggle { !enabled } else { enabled };
    values.push(("health_check_enabled", ColumnValue::Bool(enabled)));
    dbs::update_if_changed(&state.db, d.kind, d.id, &values, state.now()).await?;
    refresh_config_hash(&state, &ctx, &uuid).await?;
    let message = if !toggle {
        "Healthcheck updated. Restart the database to apply the changes."
    } else if enabled {
        "Healthcheck enabled. Restart the database to apply the changes."
    } else {
        "Healthcheck disabled. Restart the database to apply the changes."
    };
    let mut res = toasts(&[("success", message)]);
    res["health_check_enabled"] = json!(enabled);
    Ok(ok(res).into_response())
}

/// `StandaloneClickhouse::anonymousDataVolume`.
async fn clickhouse_volume(state: &AppState, d: &DatabaseRow) -> Result<Option<String>, ApiError> {
    if d.kind != Kind::Clickhouse {
        return Ok(None);
    }
    let Some(server) = server_of(state, d).await? else {
        return Ok(None);
    };
    if !server.is_functional() {
        return Ok(None);
    }
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    let Some((kind, name)) = jobs.clickhouse_data_mount(&server, &d.uuid).await else {
        return Ok(None);
    };
    if kind != "volume" || name.is_empty() {
        return Ok(None);
    }
    let known = dbs::persistent_volumes(&state.db, d.kind, d.id)
        .await?
        .iter()
        .any(|(n, _, _)| *n == name);
    Ok((!known).then_some(name))
}

async fn anonymous_volume(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let d = find(&state, &ctx, &uuid).await?;
    Ok(ok(json!({"volume": clickhouse_volume(&state, &d).await?})).into_response())
}

/// `Clickhouse\General::keepCurrentDataVolume`.
async fn keep_anonymous_volume(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let d = find_admin(&state, &ctx, &uuid).await?;
    let Some(name) = clickhouse_volume(&state, &d).await? else {
        return Ok(toast("info", "This database already uses its data volume."));
    };
    pages::keep_anonymous_volume(
        &state.db,
        d.kind,
        d.id,
        &new_public_id(),
        &name,
        "/var/lib/clickhouse",
        state.now(),
    )
    .await?;
    Ok(toast(
        "success",
        "The current data volume is now the data volume of this database. You can restart it safely.",
    ))
}

// ---- Sqlite\ConnectApplication ----

/// The data volume name: `persistentStorages()->whereNull('host_path')->orderBy('id')->value('name')`.
async fn sqlite_volume(state: &AppState, d: &DatabaseRow) -> Result<String, ApiError> {
    Ok(dbs::persistent_volumes(&state.db, d.kind, d.id)
        .await?
        .into_iter()
        .find(|(_, _, host)| host.is_none())
        .map_or_else(|| format!("sqlite-data-{}", d.uuid), |(name, _, _)| name))
}

/// `composeExternalVolumeDockerNames`.
fn compose_external_names(compose: Option<&str>) -> Vec<String> {
    let Some(compose) = compose.filter(|c| !c.trim().is_empty()) else {
        return vec![];
    };
    let Ok(parsed) = validate::parse_docker_compose_yaml(compose) else {
        return vec![];
    };
    let Some(coolify_compat::yaml::PhpValue::Array(list)) = php::data_get(&parsed, "volumes")
    else {
        return vec![];
    };
    list.iter()
        .filter(|(_, v)| validate::is_compose_external_volume(Some(v)))
        .map(|(k, v)| volumes::compose_external_volume_docker_name(&k.as_string(), v))
        .collect()
}

/// `composeVolumeSnippet`: `Yaml::dump($lines, 4, 2)`.
fn compose_snippet(compose: Option<&str>, volume: &str, mount: &str) -> String {
    use coolify_compat::yaml::PhpValue;
    let service = compose
        .and_then(|c| validate::parse_docker_compose_yaml(c).ok())
        .and_then(|p| match php::data_get(&p, "services") {
            Some(PhpValue::Array(list)) => list.first().map(|(k, _)| k.as_string()),
            _ => None,
        })
        .unwrap_or_else(|| "app".to_owned());
    let value = PhpValue::map([
        (
            "services",
            PhpValue::map([(
                service,
                PhpValue::map([("volumes", PhpValue::strings([format!("{volume}:{mount}")]))]),
            )]),
        ),
        (
            "volumes",
            PhpValue::map([(
                volume.to_owned(),
                PhpValue::map([("external", PhpValue::Bool(true))]),
            )]),
        ),
    ]);
    coolify_compat::yaml::dump_with_indent(&value, 4, 2)
}

/// `applicationLabel`.
fn application_label(a: &pages::ConnectableApplication) -> String {
    let mut label = a.name.clone();
    if let (Some(p), Some(e)) = (&a.project_name, &a.environment_name) {
        label.push_str(&format!(" ({p} / {e})"));
    }
    if a.build_pack == "dockercompose" {
        label.push_str(" · Docker Compose");
    }
    label
}

/// What `ConnectApplication` renders: `connections`, `composeConnections` and the options.
async fn sqlite_state(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let d = find(&state, &ctx, &uuid).await?;
    if d.kind != Kind::Sqlite {
        return Err(ApiError::NotPorted);
    }
    let volume = sqlite_volume(&state, &d).await?;
    let connections: Vec<Value> = pages::sqlite_connections(&state.db, d.id)
        .await?
        .into_iter()
        .map(|c| {
            json!({
                "id": c.id,
                "mount_path": c.mount_path,
                "application_name": c.application_name,
                "href": match (&c.project_uuid, &c.environment_uuid, &c.application_uuid) {
                    (Some(p), Some(e), Some(a)) => Some(format!("/project/{p}/environment/{e}/application/{a}")),
                    _ => None,
                },
            })
        })
        .collect();
    let apps = match d.server_id() {
        Some(id) => pages::connectable_applications(&state.db, d.team_id, id).await?,
        None => vec![],
    };
    let compose: Vec<Value> = apps
        .iter()
        .filter(|a| {
            a.build_pack == "dockercompose"
                && compose_external_names(a.docker_compose_raw.as_deref()).contains(&volume)
        })
        .map(|a| {
            json!({
                "id": a.id,
                "name": a.name,
                "href": format!(
                    "/project/{}/environment/{}/application/{}",
                    a.project_uuid.as_deref().unwrap_or_default(),
                    a.environment_uuid.as_deref().unwrap_or_default(),
                    a.uuid
                ),
            })
        })
        .collect();
    let options: Vec<Value> = apps
        .iter()
        .map(|a| json!({"value": a.uuid, "label": application_label(a)}))
        .collect();
    Ok(ok(json!({
        "connections": connections,
        "compose_connections": compose,
        "application_options": options,
        "volume_name": volume,
        "sqlite_databases": d.str("sqlite_databases"),
    }))
    .into_response())
}

static DIRECTORY_PATH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^/([a-zA-Z0-9._\-/~@+]*)?\n?\z").unwrap_or_else(|_| unreachable!())
});
static VOLUME_NAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[a-zA-Z0-9][a-zA-Z0-9._-]*\n?\z").unwrap_or_else(|_| unreachable!())
});

/// `auditStorageChange` for an application with `sqliteAuditContext`.
#[allow(clippy::too_many_arguments)]
async fn audit_connection(
    state: &AppState,
    ctx: &AuthCtx,
    meta: &Meta,
    action: &str,
    application_uuid: Option<&str>,
    application_name: Option<&str>,
    storage_uuid: Option<&str>,
    storage_id: i64,
    name: &str,
    mount: &str,
    d: &DatabaseRow,
) {
    crate::audit::log(
        state,
        ctx,
        meta,
        &format!("ui.application.storage_{action}"),
        json!({
            "team_id": d.team_id,
            "application_uuid": application_uuid,
            "application_name": application_name,
            "storage_uuid": storage_uuid,
            "storage_id": storage_id,
            "storage_type": "persistent",
            "storage_name": name,
            "mount_path": mount,
            "host_path": null,
            "standalone_sqlite_uuid": d.uuid,
            "standalone_sqlite_name": d.str("name"),
        }),
        "info",
    )
    .await;
}

/// `ConnectApplication::connect`.
async fn sqlite_connect(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path(uuid): Path<String>,
    body: Bytes,
) -> ApiResult {
    let d = find(&state, &ctx, &uuid).await?;
    if d.kind != Kind::Sqlite {
        return Err(ApiError::NotPorted);
    }
    let input: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let application_uuid = input
        .get("application_uuid")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let mount = input
        .get("mount_path")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let volume = sqlite_volume(&state, &d).await?;
    let mut errors = Map::new();
    if application_uuid.is_empty() {
        errors.insert(
            "application_uuid".into(),
            json!(["Select an application first."]),
        );
    }
    if mount.is_empty() {
        errors.insert(
            "mount_path".into(),
            json!(["The mount path field is required."]),
        );
    } else if !DIRECTORY_PATH.is_match(mount) {
        errors.insert(
            "mount_path".into(),
            json!(["Mount path must start with / and only contain safe path characters."]),
        );
    }
    if !VOLUME_NAME.is_match(&volume) {
        errors.insert(
            "volume_name".into(),
            json!(["The volume name must start with an alphanumeric character and contain only alphanumeric characters, dots, underscores, and hyphens."]),
        );
    }
    if !errors.is_empty() {
        return Err(ApiError::Validation(errors));
    }
    let apps = match d.server_id() {
        Some(id) => pages::connectable_applications(&state.db, d.team_id, id).await?,
        None => vec![],
    };
    let Some(app) = apps.iter().find(|a| a.uuid == application_uuid) else {
        return Ok(toast(
            "error",
            "The selected application is not available on this server.",
        ));
    };
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    if app.build_pack == "dockercompose" {
        if compose_external_names(app.docker_compose_raw.as_deref()).contains(&volume) {
            return Ok(toast(
                "error",
                &format!(
                    "{} already mounts this database volume in its compose file.",
                    app.name
                ),
            ));
        }
        return Ok(ok(json!({
            "compose_application_name": app.name,
            "compose_snippet": compose_snippet(app.docker_compose_raw.as_deref(), &volume, mount),
        }))
        .into_response());
    }
    if app.has_additional_servers {
        return Ok(toast(
            "error",
            "Applications that use multiple servers cannot have persistent storage because volumes are local to a server.",
        ));
    }
    if pages::application_mounts_sqlite(&state.db, app.id, d.id).await? {
        return Ok(toast(
            "error",
            &format!("{} already mounts this database volume.", app.name),
        ));
    }
    let storage_uuid = new_public_id();
    let id = pages::create_sqlite_connection(
        &state.db,
        &storage_uuid,
        &volume,
        mount,
        d.id,
        app.id,
        state.now(),
    )
    .await?;
    audit_connection(
        &state,
        &ctx,
        &meta,
        "created",
        Some(&app.uuid),
        Some(&app.name),
        Some(&storage_uuid),
        id,
        &volume,
        mount,
        &d,
    )
    .await;
    Ok(ok(json!({
        "redirect": format!(
            "/project/{}/environment/{}/application/{}/persistent-storage",
            app.project_uuid.as_deref().unwrap_or_default(),
            app.environment_uuid.as_deref().unwrap_or_default(),
            app.uuid
        ),
    }))
    .into_response())
}

/// `ConnectApplication::unlink`: the Docker volume and its data stay untouched.
async fn sqlite_unlink(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path((uuid, volume)): Path<(String, i64)>,
) -> ApiResult {
    let d = find(&state, &ctx, &uuid).await?;
    let c = pages::sqlite_connection(&state.db, d.id, volume)
        .await?
        .ok_or(ApiError::Message(StatusCode::NOT_FOUND, "Not found."))?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    pages::delete_volume(&state.db, c.id).await?;
    if c.application_uuid.is_some() {
        audit_connection(
            &state,
            &ctx,
            &meta,
            "deleted",
            c.application_uuid.as_deref(),
            c.application_name.as_deref(),
            c.uuid.as_deref(),
            c.id,
            &c.name,
            &c.mount_path,
            &d,
        )
        .await;
    }
    Ok(toast(
        "success",
        "Application unlinked. Redeploy it to apply the change.",
    ))
}
