//! `DatabasesController`: `update_by_uuid`, tags (`HandlesTagsApi`), `logs_by_uuid`,
//! `move_by_uuid`, `migrate_by_uuid`, `clone_by_uuid`, imports (`HandlesDatabaseImportsApi`).

use std::sync::LazyLock;

use axum::Extension;
use axum::body::Bytes;
use axum::extract::{Multipart, Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri, header};
use axum::response::IntoResponse;
use coolify_db::database_parts as parts;
use coolify_db::databases::{self as dbs, DatabaseRow, Kind, STANDALONE_DOCKER_MORPH};
use coolify_db::records::Record;
use coolify_db::servers::{self, ColumnValue};
use coolify_domain::naming::strip_tags;
use coolify_domain::{Ability, new_public_id};
use coolify_jobs::databases::import;
use regex::Regex;
use serde_json::{Map, Value, json};

use crate::auth::AuthCtx;
use crate::input::{Input, lenient_json, query_param, validated_json};
use crate::response::{ApiError, ApiResult, PhpJson, created, ok};
use crate::routes::databases::{
    INT_COLUMNS, NOT_FOUND, conf_field, decode_conf, kind_fields, public_port_used_except,
    startable,
};
use crate::serialize::php_truthy;
use crate::state::AppState;
use crate::validation::{CUSTOM_API_MESSAGES, Field, Rule, validate};

fn can_deploy(ctx: &AuthCtx) -> bool {
    ctx.abilities.can(Ability::Deploy) || ctx.abilities.can(Ability::Root)
}

async fn find(state: &AppState, ctx: &AuthCtx, uuid: &str) -> Result<DatabaseRow, ApiError> {
    dbs::find_in_team(&state.db, ctx.team_id, uuid)
        .await?
        .ok_or(NOT_FOUND)
}

fn bool_of(v: Option<&Value>) -> Option<bool> {
    match v? {
        Value::Bool(b) => Some(*b),
        Value::Number(n) => Some(n.as_i64() == Some(1)),
        Value::String(s) => Some(s == "1"),
        _ => None,
    }
}

/// The columns of `$allowedFields` the model's `$fillable` holds, per kind.
fn fillable_kind_fields(kind: Kind) -> &'static [&'static str] {
    match kind {
        Kind::Postgresql => &[
            "postgres_user",
            "postgres_password",
            "postgres_db",
            "postgres_initdb_args",
            "postgres_host_auth_method",
            "postgres_conf",
        ],
        Kind::Clickhouse => &["clickhouse_admin_user", "clickhouse_admin_password"],
        Kind::Dragonfly => &["dragonfly_password"],
        Kind::Redis => &["redis_conf"],
        Kind::Keydb => &["keydb_password", "keydb_conf"],
        Kind::Mariadb => &[
            "mariadb_conf",
            "mariadb_root_password",
            "mariadb_user",
            "mariadb_password",
            "mariadb_database",
        ],
        Kind::Mongodb => &[
            "mongo_conf",
            "mongo_initdb_root_username",
            "mongo_initdb_root_password",
            "mongo_initdb_database",
        ],
        Kind::Mysql => &[
            "mysql_root_password",
            "mysql_password",
            "mysql_user",
            "mysql_database",
            "mysql_conf",
        ],
        Kind::Sqlite => &["sqlite_databases"],
    }
}

/// `allowedFields` of `update_by_uuid` for a kind (all but the `instant_deploy` and health ones).
fn update_allowed(kind: Kind) -> Vec<&'static str> {
    let mut out: Vec<&'static str> = vec!["name", "description", "image"];
    if kind != Kind::Sqlite {
        out.extend(["public_port", "public_port_timeout", "is_public"]);
    }
    out.extend([
        "instant_deploy",
        "limits_memory",
        "limits_memory_swap",
        "limits_memory_swappiness",
        "limits_memory_reservation",
        "limits_cpus",
        "limits_cpuset",
        "limits_cpu_shares",
    ]);
    if kind == Kind::Redis {
        out.extend(["redis_password", "redis_conf"]);
    } else {
        out.extend(fillable_kind_fields(kind).iter().copied());
    }
    out.extend([
        "health_check_enabled",
        "health_check_interval",
        "health_check_timeout",
        "health_check_retries",
        "health_check_start_period",
    ]);
    out
}

/// `PATCH /databases/{uuid}` (`update_by_uuid`).
pub async fn update(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let mut input = validated_json(&headers, &uri, &body)?;
    let present_keys: Vec<String> = input.keys().cloned().collect();
    let present: Vec<&str> = present_keys.iter().map(String::as_str).collect();
    validate(
        &input,
        &[
            Field {
                name: "name",
                rules: vec![Rule::String, Rule::Max(255)],
            },
            Field {
                name: "description",
                rules: vec![Rule::String, Rule::Nullable],
            },
            Field {
                name: "image",
                rules: vec![Rule::String],
            },
            Field {
                name: "is_public",
                rules: vec![Rule::Boolean],
            },
            Field {
                name: "public_port",
                rules: vec![Rule::Numeric, Rule::Nullable, Rule::Between(1024, 65535)],
            },
            Field {
                name: "public_port_timeout",
                rules: vec![Rule::Integer, Rule::Nullable, Rule::MinNum(1)],
            },
            Field {
                name: "limits_memory",
                rules: vec![Rule::String],
            },
            Field {
                name: "limits_memory_swap",
                rules: vec![Rule::String],
            },
            Field {
                name: "limits_memory_swappiness",
                rules: vec![Rule::Numeric],
            },
            Field {
                name: "limits_memory_reservation",
                rules: vec![Rule::String],
            },
            Field {
                name: "limits_cpus",
                rules: vec![Rule::String],
            },
            Field {
                name: "limits_cpuset",
                rules: vec![Rule::String, Rule::Nullable],
            },
            Field {
                name: "limits_cpu_shares",
                rules: vec![Rule::Numeric],
            },
            Field {
                name: "instant_deploy",
                rules: vec![Rule::Boolean],
            },
            Field {
                name: "redact_all_env_values_in_logs",
                rules: vec![Rule::Boolean],
            },
            Field {
                name: "include_coolify_default_variables",
                rules: vec![Rule::Boolean],
            },
            Field {
                name: "is_log_drain_enabled",
                rules: vec![Rule::Boolean],
            },
        ],
        CUSTOM_API_MESSAGES,
        &present,
    )?;
    let instant = crate::routes::databases::input_bool(&input, "instant_deploy");
    let redact = input
        .has("redact_all_env_values_in_logs")
        .then(|| crate::routes::databases::input_bool(&input, "redact_all_env_values_in_logs"));
    let log_drain = input
        .has("is_log_drain_enabled")
        .then(|| crate::routes::databases::input_bool(&input, "is_log_drain_enabled"));
    let include_defaults = input
        .has("include_coolify_default_variables")
        .then(|| crate::routes::databases::input_bool(&input, "include_coolify_default_variables"));
    // removeUnnecessaryFieldsFromRequest
    for k in [
        "project_uuid",
        "environment_name",
        "environment_uuid",
        "destination_uuid",
        "server_uuid",
        "type",
        "domains",
        "instant_deploy",
        "github_app_uuid",
        "private_key_uuid",
        "use_build_server",
        "use_build_secrets",
        "is_static",
        "is_spa",
        "is_auto_deploy_enabled",
        "is_force_https_enabled",
        "is_preview_deployments_enabled",
        "connect_to_docker_network",
        "force_domain_override",
        "autogenerate_domain",
        "is_container_label_escape_enabled",
        "is_preserve_repository_enabled",
        "include_source_commit_in_build",
        "is_git_submodules_enabled",
        "is_git_lfs_enabled",
        "is_git_shallow_clone_enabled",
        "disable_build_cache",
        "inject_build_args_to_dockerfile",
        "is_env_sorting_enabled",
        "redact_all_env_values_in_logs",
        "include_coolify_default_variables",
        "is_pr_deployments_public_enabled",
        "stop_grace_period",
        "rolling_update_drain_seconds",
        "docker_images_to_keep",
        "is_gzip_enabled",
        "is_log_drain_enabled",
    ] {
        input.0.shift_remove(k);
    }
    let d = find(&state, &ctx, &uuid).await?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    if instant && !can_deploy(&ctx) {
        return Err(ApiError::Abort(
            StatusCode::FORBIDDEN,
            "Missing required permissions: deploy",
        ));
    }
    let server_row = match d.server_id() {
        Some(id) => servers::find_by_id(&state.db, id).await?,
        None => None,
    };
    if log_drain == Some(true) {
        let enabled = server_row
            .as_ref()
            .map(coolify_jobs::server::ServerInfo::from_row)
            .is_some_and(|s| coolify_jobs::databases::cleanup::server_log_drain_enabled(&s));
        if !enabled {
            let mut m = Map::new();
            m.insert(
                "is_log_drain_enabled".into(),
                json!(["Log drain is not enabled on the server. Enable it on the server first."]),
            );
            return Err(ApiError::Validation(m));
        }
    }
    let request_public = input.get("is_public").cloned();
    if let Some(v) = &request_public
        && php_truthy(v) != d.bool("is_public")
        && !can_deploy(&ctx)
    {
        return Err(ApiError::Message(
            StatusCode::FORBIDDEN,
            "Missing required permissions: deploy. Making a database public or private starts or stops its proxy on the server, so it needs a token with the deploy permission.",
        ));
    }
    let port_value = input.get("public_port").cloned();
    let public_port = port_value
        .as_ref()
        .and_then(|v| v.as_i64().or_else(|| v.as_str()?.trim().parse().ok()));
    if request_public.as_ref().is_some_and(php_truthy)
        && port_value.as_ref().is_some_and(php_truthy)
        && let (Some(server), Some(port)) = (&server_row, public_port)
        && public_port_used_except(&state.db, server.id, port, d.id).await?
    {
        return Err(ApiError::Message(
            StatusCode::BAD_REQUEST,
            "Public port already used by another database.",
        ));
    }
    // The kind's own rules, the health check rules and the fields that are not allowed.
    let own = kind_fields(d.kind);
    let allowed = update_allowed(d.kind);
    let mut base_fields = own;
    base_fields.extend([
        Field {
            name: "health_check_enabled",
            rules: vec![Rule::Boolean],
        },
        Field {
            name: "health_check_interval",
            rules: vec![Rule::Integer, Rule::MinNum(1)],
        },
        Field {
            name: "health_check_timeout",
            rules: vec![Rule::Integer, Rule::MinNum(1)],
        },
        Field {
            name: "health_check_retries",
            rules: vec![Rule::Integer, Rule::MinNum(1)],
        },
        Field {
            name: "health_check_start_period",
            rules: vec![Rule::Integer, Rule::MinNum(0)],
        },
    ]);
    // The base64 check of the configuration comes before the field check.
    let mut decoded_conf: Option<String> = None;
    if let Some(conf) = conf_field(d.kind)
        && input.has(conf)
    {
        match input.get(conf).and_then(decode_conf) {
            Some(text) => decoded_conf = Some(text),
            None => {
                return Err(ApiError::Validation(Map::from_iter([(
                    conf.to_owned(),
                    Value::String(format!("The {conf} should be base64 encoded.")),
                )])));
            }
        }
    }
    validate(&input, &base_fields, CUSTOM_API_MESSAGES, &allowed)?;

    let jobs = state.jobs.as_ref();
    if instant {
        if let Some(j) = jobs
            && let Some(message) = j.start_prerequisite_error(&d).await
        {
            return Ok(PhpJson(
                StatusCode::UNPROCESSABLE_ENTITY,
                json!({"message": message}),
            )
            .into_response());
        }
        startable(&state, &d).await?;
    }
    let proxy = match request_public.as_ref() {
        Some(Value::Bool(false)) if d.bool("is_public") => Some("stop"),
        Some(Value::Bool(true))
            if port_value.as_ref().is_some_and(php_truthy) && !d.bool("is_public") =>
        {
            Some("start")
        }
        _ => None,
    };
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let now = state.now();
    let mut values: Vec<(&'static str, ColumnValue)> = Vec::new();
    if let Some(v) = redact {
        values.push(("redact_all_env_values_in_logs", ColumnValue::Bool(v)));
    }
    if let Some(v) = log_drain {
        values.push(("is_log_drain_enabled", ColumnValue::Bool(v)));
    }
    if let Some(v) = include_defaults {
        values.push(("include_coolify_default_variables", ColumnValue::Bool(v)));
    }
    for key in &allowed {
        if !input.has(key) || *key == "instant_deploy" || *key == "redis_password" {
            continue;
        }
        let v = input.get(key).cloned().unwrap_or(Value::Null);
        match *key {
            "name" | "description" => {
                values.push((key, ColumnValue::Text(Some(strip_tags(v.as_str())))));
            }
            k if d.kind.encrypted_columns().contains(&k) => {
                let plain = v.as_str();
                let same = d
                    .str(k)
                    .and_then(|c| enc.decrypt_string(c).ok())
                    .is_some_and(|c| Some(c.as_str()) == plain);
                if !same {
                    values.push((k, ColumnValue::Text(plain.map(|p| enc.encrypt_string(p)))));
                }
            }
            k if Some(k) == conf_field(d.kind) => {
                values.push((k, ColumnValue::Text(decoded_conf.clone())));
            }
            "is_public" | "health_check_enabled" => {
                values.push((key, ColumnValue::Bool(php_truthy(&v))));
            }
            k if INT_COLUMNS.contains(&k) || k.starts_with("health_check_") => match &v {
                Value::Null => values.push((k, ColumnValue::Text(None))),
                other => {
                    let n = other
                        .as_i64()
                        .or_else(|| other.as_f64().map(|f| f as i64))
                        .or_else(|| other.as_str().and_then(|s| s.trim().parse().ok()))
                        .ok_or(ApiError::Internal)?;
                    values.push((k, ColumnValue::Int(n)));
                }
            },
            k => {
                values.push((
                    k,
                    ColumnValue::Text(match &v {
                        Value::Null => None,
                        Value::String(s) => Some(s.clone()),
                        other => Some(other.to_string()),
                    }),
                ));
            }
        }
    }
    dbs::update_if_changed(&state.db, d.kind, d.id, &values, now).await?;
    if d.kind == Kind::Redis
        && let Some(password) = input.str("redis_password").filter(|p| !p.trim().is_empty())
    {
        coolify_db::environment_variables::upsert(
            &state.db,
            d.kind.morph(),
            d.id,
            "REDIS_PASSWORD",
            Some(&enc.encrypt_serialized(password)),
            false,
            &state.version,
            now,
        )
        .await?;
    }
    let fresh = dbs::find_in_team(&state.db, ctx.team_id, &uuid)
        .await?
        .ok_or(ApiError::Internal)?;
    if let Some(action) = proxy {
        let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
        if action == "start" {
            jobs.spawn_database_proxy_start(fresh.clone());
        } else {
            jobs.spawn_database_proxy_stop(fresh.clone());
        }
    }
    if instant {
        let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
        jobs.spawn_database_restart(fresh, Some(ctx.user_id))
            .map_err(|m| ApiError::Message(StatusCode::CONFLICT, m))?;
    }
    tracing::info!(target: "audit", event = "api.database.updated", team_id = ctx.team_id, database_uuid = %d.uuid, "database updated");
    Ok(ok(json!({"message": "Database updated."})).into_response())
}

// ---------------------------------------------------------------------------
// Tags
// ---------------------------------------------------------------------------

fn serialize_tag(r: &Record) -> Value {
    let time = |k: &str| {
        parts::str_of(r, k)
            .map(coolify_db::records::laravel_datetime)
            .map_or(Value::Null, Value::String)
    };
    json!({
        "uuid": r.get("uuid").cloned().unwrap_or(Value::Null),
        "name": r.get("name").cloned().unwrap_or(Value::Null),
        "created_at": time("created_at"),
        "updated_at": time("updated_at"),
    })
}

/// `GET /databases/{uuid}/tags`.
pub async fn tags(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let d = find(&state, &ctx, &uuid).await?;
    let rows = parts::tags_of(&state.db, d.kind.morph(), d.id).await?;
    Ok(ok(Value::Array(rows.iter().map(serialize_tag).collect())).into_response())
}

/// `normalizeTagNames`.
pub(crate) fn normalize_tag_names(names: &[Value]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for n in names {
        let text = match n {
            Value::String(s) => s.clone(),
            Value::Null => String::new(),
            Value::Bool(true) => "1".into(),
            Value::Bool(false) => String::new(),
            other => other.to_string(),
        };
        let name = strip_tags(Some(&text)).trim().to_lowercase();
        if !out.contains(&name) {
            out.push(name);
        }
    }
    out
}

/// `POST /databases/{uuid}/tags` (`createTag`).
pub async fn create_tag(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let input = validated_json(&headers, &uri, &body)?;
    let d = find(&state, &ctx, &uuid).await?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    if input.has("tag_name") && input.has("tag_names") {
        let mut m = Map::new();
        m.insert(
            "tag_name".into(),
            json!(["Provide either tag_name or tag_names, not both."]),
        );
        return Err(ApiError::Validation(m));
    }
    // `required_without`: the default message of the plain validator.
    let mut errors = Map::new();
    let filled = |k: &str| match input.get(k) {
        None | Some(Value::Null) => false,
        Some(Value::Array(a)) => !a.is_empty(),
        Some(_) => true,
    };
    if !filled("tag_name") && !filled("tag_names") {
        errors.insert(
            "tag_name".into(),
            json!(["The tag name field is required when tag names is not present."]),
        );
        errors.insert(
            "tag_names".into(),
            json!(["The tag names field is required when tag name is not present."]),
        );
    }
    if let Some(v) = input.get("tag_name")
        && !v.is_null()
        && !v.is_string()
    {
        errors.insert(
            "tag_name".into(),
            json!(["The tag name field must be a string."]),
        );
    }
    if let Some(v) = input.get("tag_names")
        && !v.is_null()
    {
        match v {
            Value::Array(a) => {
                for (i, item) in a.iter().enumerate() {
                    if !item.is_string() {
                        errors.insert(
                            format!("tag_names.{i}"),
                            json!([format!("The tag_names.{i} field must be a string.")]),
                        );
                    }
                }
            }
            _ => {
                errors.insert(
                    "tag_names".into(),
                    json!(["The tag names field must be an array."]),
                );
            }
        }
    }
    for key in input.keys() {
        if key != "tag_name" && key != "tag_names" {
            errors.insert(key.clone(), json!(["This field is not allowed."]));
        }
    }
    if !errors.is_empty() {
        return Err(ApiError::Validation(errors));
    }
    let raw: Vec<Value> = if input.has("tag_names") {
        input
            .get("tag_names")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    } else {
        vec![input.get("tag_name").cloned().unwrap_or(Value::Null)]
    };
    let names = normalize_tag_names(&raw);
    if names.iter().any(|n| n.chars().count() < 2) {
        let mut m = Map::new();
        m.insert(
            "tag_name".into(),
            json!(["Each tag name must be at least 2 characters after sanitization."]),
        );
        return Err(ApiError::Validation(m));
    }
    parts::attach_tags(
        &state.db,
        ctx.team_id,
        d.kind.morph(),
        d.id,
        &names,
        state.now(),
    )
    .await?;
    let rows = parts::tags_of(&state.db, d.kind.morph(), d.id).await?;
    Ok(created(Value::Array(rows.iter().map(serialize_tag).collect())).into_response())
}

/// `DELETE /databases/{uuid}/tags/{tag_uuid}` (`deleteTag`).
pub async fn delete_tag(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((uuid, tag_uuid)): Path<(String, String)>,
) -> ApiResult {
    let d = find(&state, &ctx, &uuid).await?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let tag = parts::tag_id(&state.db, ctx.team_id, &tag_uuid)
        .await?
        .ok_or(ApiError::Message(StatusCode::NOT_FOUND, "Tag not found."))?;
    if !parts::has_tag(&state.db, d.kind.morph(), d.id, tag).await? {
        return Err(ApiError::Message(
            StatusCode::NOT_FOUND,
            "Tag not found on resource.",
        ));
    }
    parts::detach_tag(&state.db, d.kind.morph(), d.id, tag).await?;
    Ok(ok(json!({"message": "Tag removed."})).into_response())
}

// ---------------------------------------------------------------------------
// Logs
// ---------------------------------------------------------------------------

/// `normalizeLogLines`.
fn normalize_lines(lines: Option<&str>) -> String {
    match lines {
        Some("all") => "all".into(),
        Some(s) => match s.trim().parse::<i64>() {
            Ok(-1) => "all".into(),
            Ok(n) if n >= 0 => n.min(10000).to_string(),
            _ => "100".into(),
        },
        None => "100".into(),
    }
}

/// `GET /databases/{uuid}/logs` (`logs_by_uuid`).
pub async fn logs(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    uri: Uri,
) -> ApiResult {
    let d = find(&state, &ctx, &uuid).await?;
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    let lines = normalize_lines(query_param(&uri, "lines").as_deref());
    let timestamps = query_param(&uri, "show_timestamps")
        .is_some_and(|v| matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "on" | "yes"));
    match jobs.database_logs(&d, &lines, timestamps).await {
        Ok(Some(text)) => Ok(ok(json!({"logs": text})).into_response()),
        Ok(None) => Err(ApiError::Message(
            StatusCode::BAD_REQUEST,
            "Database is not running.",
        )),
        Err(e) => {
            tracing::warn!(database = %d.uuid, error = %e, "database logs could not be read");
            Err(ApiError::Internal)
        }
    }
}

// ---------------------------------------------------------------------------
// Move, migrate
// ---------------------------------------------------------------------------

/// `POST /databases/{uuid}/move` (`moveResourceToEnvironment`).
pub async fn move_database(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let d = find(&state, &ctx, &uuid).await?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let input = lenient_json(&headers, &uri, &body);
    // `Validator::make`: Laravel's default messages.
    let required = Field {
        name: "environment_uuid",
        rules: vec![Rule::Required, Rule::String],
    };
    let keys: Vec<&str> = input.keys().map(String::as_str).collect();
    validate(&input, &[required], &[], &keys)?;
    let mut extra = Map::new();
    for key in input.keys() {
        if key != "environment_uuid" {
            extra.insert(
                key.clone(),
                Value::String("This field is not allowed.".into()),
            );
        }
    }
    if !extra.is_empty() {
        return Err(ApiError::Validation(extra));
    }
    let wanted = input.str("environment_uuid").unwrap_or_default();
    let Some((env_id, project_uuid, env_uuid)) =
        coolify_db::projects::team_environment_by_uuid(&state.db, ctx.team_id, wanted).await?
    else {
        return Err(ApiError::Message(
            StatusCode::NOT_FOUND,
            "Target environment not found or not owned by your team.",
        ));
    };
    if d.int("environment_id") == Some(env_id) {
        return Err(ApiError::Message(
            StatusCode::BAD_REQUEST,
            "Database is already in this environment.",
        ));
    }
    dbs::update_if_changed(
        &state.db,
        d.kind,
        d.id,
        &[("environment_id", ColumnValue::Int(env_id))],
        state.now(),
    )
    .await?;
    tracing::info!(target: "audit", event = "api.database.moved", team_id = ctx.team_id, resource_uuid = %d.uuid, to_environment_uuid = %env_uuid, "database moved");
    Ok(ok(json!({
        "message": "Database moved successfully.",
        "uuid": d.uuid,
        "project_uuid": project_uuid,
        "environment_uuid": env_uuid,
    }))
    .into_response())
}

/// `POST /databases/{uuid}/migrate`: `migrateResourceToDestination` answers 404 outside development.
pub async fn migrate(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    find(&state, &ctx, &uuid).await?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    Err(ApiError::Abort(StatusCode::NOT_FOUND, ""))
}

// ---------------------------------------------------------------------------
// Clone
// ---------------------------------------------------------------------------

/// The new name of a volume in `clone_by_uuid`.
fn cloned_volume_name(original: &str, source_uuid: &str, new_uuid: &str) -> String {
    for prefix in [
        "postgres-data-",
        "mysql-data-",
        "redis-data-",
        "clickhouse-data-",
        "mariadb-data-",
        "mongodb-data-",
        "keydb-data-",
        "dragonfly-data-",
        "sqlite-data-",
    ] {
        if original.starts_with(prefix) {
            return format!("{prefix}{new_uuid}");
        }
    }
    if original.starts_with(source_uuid) {
        return original.replace(source_uuid, new_uuid);
    }
    format!("{new_uuid}-{original}")
}

/// `POST /databases/{uuid}/clone` (`clone_by_uuid`).
pub async fn clone_database(
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
        &[
            Field {
                name: "destination_uuid",
                rules: vec![Rule::Required, Rule::String],
            },
            Field {
                name: "name",
                rules: vec![Rule::String, Rule::Max(255), Rule::Nullable],
            },
            Field {
                name: "clone_volumes",
                rules: vec![Rule::Boolean],
            },
        ],
        CUSTOM_API_MESSAGES,
        &["destination_uuid", "name", "clone_volumes"],
    )?;
    let d = find(&state, &ctx, &uuid).await?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let clone_volumes = crate::routes::databases::input_bool(&input, "clone_volumes");
    if clone_volumes && !can_deploy(&ctx) {
        return Err(ApiError::Message(
            StatusCode::FORBIDDEN,
            "Missing required permissions: deploy. Cloning volume data stops and starts the source resource, so it needs a token with the deploy permission.",
        ));
    }
    let wanted = input.str("destination_uuid").unwrap_or_default();
    let not_found = ApiError::Message(StatusCode::NOT_FOUND, "Destination not found.");
    let Some((destination_id, server_id)) =
        parts::team_standalone_destination(&state.db, ctx.team_id, wanted).await?
    else {
        if parts::team_swarm_destination_exists(&state.db, ctx.team_id, wanted).await? {
            return Err(ApiError::NotPorted);
        }
        return Err(not_found);
    };
    let server = servers::find_by_id(&state.db, server_id)
        .await?
        .ok_or(ApiError::Message(
            StatusCode::NOT_FOUND,
            "Destination not found.",
        ))?;
    if crate::routes::servers::effective_role_is_build(&server.settings.clone().unwrap_or_default())
    {
        return Err(not_found);
    }
    let new_uuid = new_public_id();
    let name = match input.str("name").filter(|n| !n.trim().is_empty()) {
        Some(n) => n.to_owned(),
        None => format!("{}-clone-{new_uuid}", d.str("name").unwrap_or_default()),
    };
    let now = state.now();
    let morph = d.kind.morph();
    let new_id = parts::replicate_database(
        &state.db,
        d.kind.table(),
        d.id,
        &new_uuid,
        &strip_tags(Some(&name)),
        destination_id,
        STANDALONE_DOCKER_MORPH,
        now,
    )
    .await?;
    parts::copy_tags(&state.db, morph, d.id, new_id).await?;
    let mut clones: Vec<(String, String)> = Vec::new();
    for volume in parts::persistent_storages(&state.db, morph, d.id).await? {
        let original = parts::str_of(&volume, "name").unwrap_or_default();
        let new_name = cloned_volume_name(original, &d.uuid, &new_uuid);
        parts::replicate_persistent_volume(
            &state.db,
            parts::id_of(&volume),
            &new_name,
            new_id,
            now,
        )
        .await?;
        if clone_volumes {
            clones.push((original.to_owned(), new_name));
        }
    }
    if !clones.is_empty() {
        let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
        jobs.spawn_volume_clones(d.clone(), server_id, clones);
    }
    let fresh = dbs::find_in_team(&state.db, ctx.team_id, &new_uuid)
        .await?
        .ok_or(ApiError::Internal)?;
    for file in parts::file_storages(&state.db, morph, d.id).await? {
        let copy =
            parts::replicate_file_volume(&state.db, parts::id_of(&file), new_id, now).await?;
        if let Some(jobs) = state.jobs.as_ref()
            && !parts::bool_of(&copy, "is_host_file")
        {
            let dir = format!("/data/coolify/databases/{}", d.uuid);
            let fs = parts::str_of(&copy, "fs_path").unwrap_or_default();
            if !coolify_jobs::databases::paths::is_outside_resource_directory(fs, &dir) {
                let _ = jobs;
            }
        }
    }
    let _ = fresh;
    parts::copy_backups_to(&state.db, morph, d.id, new_id, ctx.team_id, now).await?;
    parts::replicate_variables(&state.db, morph, d.id, new_id, now).await?;
    tracing::info!(target: "audit", event = "api.database.cloned", team_id = ctx.team_id, source_uuid = %d.uuid, database_uuid = %new_uuid, "database cloned");
    Ok(created(json!({"uuid": new_uuid, "message": "Database cloned."})).into_response())
}

// ---------------------------------------------------------------------------
// Imports
// ---------------------------------------------------------------------------

static UUID: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^[\da-f]{8}-[\da-f]{4}-[\da-f]{4}-[\da-f]{4}-[\da-f]{12}$")
        .unwrap_or_else(|_| unreachable!())
});

/// `Storage::path('')`: where finished uploads wait for the import.
fn import_data_dir(_state: &AppState) -> std::path::PathBuf {
    std::env::var_os("COOLIFY_RS_DATA_DIR")
        .map_or_else(std::env::temp_dir, std::path::PathBuf::from)
}

/// `POST /databases/{uuid}/imports/uploads` (`uploadDatabaseImport`): one file, or Dropzone chunks.
pub async fn upload_import(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    mut multipart: Multipart,
) -> ApiResult {
    let d = find(&state, &ctx, &uuid).await?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let base = import_data_dir(&state).join("upload-tmp");
    let _ = std::fs::create_dir_all(&base);
    let temp = base.join(new_public_id());
    let mut original: Option<String> = None;
    let mut fields: Map<String, Value> = Map::new();
    let mut size: u64 = 0;
    while let Ok(Some(mut field)) = multipart.next_field().await {
        let name = field.name().unwrap_or_default().to_owned();
        if name == "file" {
            original = field.file_name().map(str::to_owned);
            let mut f = std::fs::File::create(&temp).map_err(|_| ApiError::Internal)?;
            while let Ok(Some(chunk)) = field.chunk().await {
                use std::io::Write as _;
                f.write_all(&chunk).map_err(|_| ApiError::Internal)?;
                size += chunk.len() as u64;
            }
        } else if let Ok(text) = field.text().await {
            fields.insert(name, Value::String(text));
        }
    }
    let cleanup = || {
        let _ = std::fs::remove_file(&temp);
    };
    let upload_id = fields
        .get("upload_id")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let mut errors = Map::new();
    match &upload_id {
        None => {
            errors.insert(
                "upload_id".into(),
                json!(["The upload id field is required."]),
            );
        }
        Some(u) if !UUID.is_match(u) => {
            errors.insert(
                "upload_id".into(),
                json!(["The upload id field must be a valid UUID."]),
            );
        }
        _ => {}
    }
    if original.is_none() {
        errors.insert("file".into(), json!(["The file field is required."]));
    }
    if !errors.is_empty() {
        cleanup();
        return Err(ApiError::Validation(errors));
    }
    let original = original.unwrap_or_default();
    let upload_id = upload_id.unwrap_or_default();
    if !import::has_allowed_extension(&original) {
        cleanup();
        let mut m = Map::new();
        m.insert("file".into(), json!(["Unsupported backup file extension."]));
        return Err(ApiError::Validation(m));
    }
    let total: u64 = fields
        .get("dzTotalFilesize")
        .and_then(Value::as_str)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    if total > import::MAX_BYTES {
        cleanup();
        let mut m = Map::new();
        m.insert(
            "file".into(),
            json!(["The backup exceeds the 10 GiB limit."]),
        );
        return Err(ApiError::Validation(m));
    }
    // Chunks are appended until the last one (`pion/laravel-chunk-upload`).
    let chunk_index: Option<u64> = fields
        .get("dzchunkindex")
        .and_then(Value::as_str)
        .and_then(|s| s.parse().ok());
    let chunk_total: Option<u64> = fields
        .get("dztotalchunkcount")
        .and_then(Value::as_str)
        .and_then(|s| s.parse().ok());
    let assembled = base.join(format!("{upload_id}.part"));
    let finished_path = if let (Some(index), Some(count)) = (chunk_index, chunk_total) {
        use std::io::Write as _;
        let mut out = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&assembled)
            .map_err(|_| ApiError::Internal)?;
        let data = std::fs::read(&temp).map_err(|_| ApiError::Internal)?;
        out.write_all(&data).map_err(|_| ApiError::Internal)?;
        cleanup();
        if index + 1 < count {
            let done = ((index + 1) * 100 / count.max(1)) as i64;
            return Ok(
                ok(json!({"upload_id": upload_id, "done": done, "status": true})).into_response(),
            );
        }
        assembled
    } else {
        temp.clone()
    };
    if !import::is_upload_allowed(&finished_path, &original, import::MAX_BYTES) {
        let _ = std::fs::remove_file(&finished_path);
        let mut m = Map::new();
        m.insert("file".into(), json!(["Uploaded file failed validation."]));
        return Err(ApiError::Validation(m));
    }
    let dir = import::upload_dir(&import_data_dir(&state), ctx.team_id, &d.uuid, &upload_id);
    std::fs::create_dir_all(&dir).map_err(|_| ApiError::Internal)?;
    let final_size = std::fs::metadata(&finished_path).map_or(size, |m| m.len());
    let mime = guess_mime(&finished_path);
    std::fs::rename(&finished_path, dir.join("restore")).map_err(|_| ApiError::Internal)?;
    Ok(created(json!({
        "upload_id": upload_id,
        "filename": original,
        "mime_type": mime,
        "size": final_size,
    }))
    .into_response())
}

/// `$file->getMimeType()` for the formats a backup can have.
fn guess_mime(path: &std::path::Path) -> &'static str {
    use std::io::Read as _;
    let mut head = [0u8; 300];
    let n = std::fs::File::open(path)
        .and_then(|mut f| f.read(&mut head))
        .unwrap_or(0);
    let h = &head[..n];
    if h.starts_with(&[0x1f, 0x8b]) {
        "application/gzip"
    } else if h.starts_with(b"PK\x03\x04") {
        "application/zip"
    } else if h.starts_with(b"BZh") {
        "application/x-bzip2"
    } else if h.starts_with(b"\xfd7zXZ\x00") {
        "application/x-xz"
    } else if h.starts_with(b"SQLite format 3\0") {
        "application/vnd.sqlite3"
    } else if h.get(257..262) == Some(b"ustar") {
        "application/x-tar"
    } else if h.contains(&0) {
        "application/octet-stream"
    } else {
        "text/plain"
    }
}

/// `POST /databases/{uuid}/imports` (`startDatabaseImport`).
pub async fn create_import(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let d = find(&state, &ctx, &uuid).await?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let input: Input = lenient_json(&headers, &uri, &body);
    let mut errors = Map::new();
    let mut add = |k: &str, m: String| {
        let list = errors
            .entry(k.to_owned())
            .or_insert_with(|| Value::Array(vec![]));
        if let Value::Array(a) = list {
            a.push(Value::String(m));
        }
    };
    let source = input.str("source").unwrap_or_default().to_owned();
    if !input.has("source") || input.get("source").is_some_and(Value::is_null) {
        add("source", "The source field is required.".into());
    } else if !["upload", "s3", "server"].contains(&source.as_str()) {
        add("source", "The selected source is invalid.".into());
    }
    let is = |k: &str| input.has(k) && !input.get(k).is_some_and(Value::is_null);
    // upload_id: required_if:source,upload | prohibited_unless:source,upload | uuid
    if source == "upload" && !is("upload_id") {
        add(
            "upload_id",
            "The upload id field is required when source is upload.".into(),
        );
    }
    if source != "upload" && input.has("upload_id") {
        add(
            "upload_id",
            "The upload id field is prohibited unless source is in upload.".to_string(),
        );
    }
    if is("upload_id") && !input.str("upload_id").is_some_and(|u| UUID.is_match(u)) {
        add(
            "upload_id",
            "The upload id field must be a valid UUID.".into(),
        );
    }
    if source == "s3" && !is("s3_storage_uuid") {
        add(
            "s3_storage_uuid",
            "The s3 storage uuid field is required when source is s3.".into(),
        );
    }
    if source != "s3" && input.has("s3_storage_uuid") {
        add(
            "s3_storage_uuid",
            "The s3 storage uuid field is prohibited unless source is in s3.".into(),
        );
    }
    if is("s3_storage_uuid") && !input.get("s3_storage_uuid").is_some_and(Value::is_string) {
        add(
            "s3_storage_uuid",
            "The s3 storage uuid field must be a string.".into(),
        );
    }
    if (source == "s3" || source == "server") && !is("path") {
        add(
            "path",
            format!("The path field is required when source is {source}."),
        );
    }
    if source == "upload" && input.has("path") {
        add(
            "path",
            "The path field is prohibited when source is upload.".into(),
        );
    }
    if let Some(v) = input.get("path")
        && !v.is_null()
    {
        match v.as_str() {
            Some(p) if p.chars().count() <= 4096 => {}
            Some(_) => add(
                "path",
                "The path field must not be greater than 4096 characters.".into(),
            ),
            None => add("path", "The path field must be a string.".into()),
        }
    }
    for k in [
        "dump_all",
        "replace_existing",
        "keep_owners",
        "restore_mysql_users",
    ] {
        if let Some(v) = input.get(k)
            && !matches!(v, Value::Bool(_))
            && !matches!(v.as_i64(), Some(0 | 1))
            && !matches!(v.as_str(), Some("0" | "1"))
        {
            add(
                k,
                format!("The {} field must be true or false.", k.replace('_', " ")),
            );
        }
    }
    if let Some(v) = input.get("sqlite_database") {
        match v.as_str() {
            Some(s) if s.chars().count() <= 255 => {}
            Some(_) => add(
                "sqlite_database",
                "The sqlite database field must not be greater than 255 characters.".into(),
            ),
            None => add(
                "sqlite_database",
                "The sqlite database field must be a string.".into(),
            ),
        }
    }
    for key in input.keys() {
        if ![
            "source",
            "upload_id",
            "s3_storage_uuid",
            "path",
            "dump_all",
            "replace_existing",
            "keep_owners",
            "restore_mysql_users",
            "sqlite_database",
        ]
        .contains(&key.as_str())
        {
            add(key, "This field is not allowed.".into());
        }
    }
    if !errors.is_empty() {
        return Err(ApiError::Validation(errors));
    }
    if !can_deploy(&ctx) {
        return Err(ApiError::Message(
            StatusCode::FORBIDDEN,
            "Missing required permissions: deploy. An import runs the dump in the database container, so it needs a token with the deploy permission.",
        ));
    }
    let flag = |k: &str| bool_of(input.get(k)).unwrap_or(false);
    let import_source = import::ImportSource {
        kind: source.clone(),
        upload_id: input.str("upload_id").map(str::to_owned),
        path: input.str("path").map(str::to_owned),
        s3_storage_uuid: input.str("s3_storage_uuid").map(str::to_owned),
        dump_all: flag("dump_all"),
        replace_existing: flag("replace_existing"),
        keep_owners: flag("keep_owners"),
        sqlite_database: input.str("sqlite_database").map(str::to_owned),
        restore_mysql_users: flag("restore_mysql_users"),
    };
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    let (id, status) = match jobs
        .start_database_import(&d, import_source, ctx.team_id, &import_data_dir(&state))
        .await
    {
        Ok(v) => v,
        Err(e) => {
            let code = StatusCode::from_u16(e.status).unwrap_or(StatusCode::UNPROCESSABLE_ENTITY);
            return Ok(PhpJson(code, json!({"message": e.message})).into_response());
        }
    };
    tracing::info!(target: "audit", event = "api.database.import_started", team_id = ctx.team_id, database_uuid = %d.uuid, source = %source, activity_id = id, "database import started");
    let url = format!("/api/v1/databases/{uuid}/imports/{id}");
    let mut resp = PhpJson(
        StatusCode::ACCEPTED,
        json!({"id": id, "status": status, "message": "Database import queued.", "status_url": url}),
    )
    .into_response();
    if let Ok(v) = HeaderValue::from_str(&url) {
        resp.headers_mut().insert(header::LOCATION, v);
    }
    Ok(resp)
}

/// `GET /databases/{uuid}/imports/{activity_id}` (`showDatabaseImport`).
pub async fn show_import(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((uuid, activity_id)): Path<(String, String)>,
) -> ApiResult {
    // `whereNumber('activity_id')`
    let Ok(activity_id) = activity_id.parse::<i64>() else {
        return Err(ApiError::RouteNotFound);
    };
    let d = find(&state, &ctx, &uuid).await?;
    let row = dbs::import_activity(&state.db, activity_id, ctx.team_id, &d.uuid)
        .await?
        .ok_or(ApiError::Message(
            StatusCode::NOT_FOUND,
            "Database import not found.",
        ))?;
    let (props, description, created_at, updated_at) = row;
    let status = props.get("status").cloned().unwrap_or(Value::Null);
    let terminal = status
        .as_str()
        .is_some_and(|s| ["finished", "error", "killed", "cancelled", "closed"].contains(&s));
    let output = if ctx.can_read_sensitive && ctx.is_team_admin() {
        Value::String(decode_output(&description))
    } else {
        Value::Null
    };
    let time = |t: &str| Value::String(coolify_db::records::laravel_datetime(t));
    Ok(ok(json!({
        "id": activity_id,
        "status": status,
        "exit_code": props.get("exitCode").cloned().unwrap_or(Value::Null),
        "output": output,
        "created_at": time(&created_at),
        "updated_at": time(&updated_at),
        "finished_at": if terminal { time(&updated_at) } else { Value::Null },
    }))
    .into_response())
}

/// `RunRemoteProcess::decodeOutput`.
fn decode_output(description: &str) -> String {
    let entries: Vec<Value> = serde_json::from_str(description).unwrap_or_default();
    let mut sorted: Vec<&Value> = entries.iter().collect();
    sorted.sort_by_key(|e| e.get("order").and_then(Value::as_i64).unwrap_or(0));
    sorted
        .iter()
        .filter_map(|e| e.get("output").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("")
}
