//! Session endpoints behind the database pages of the web UI: what the
//! Livewire components (`Project\Resource\Index`, `Project\Resource\Create`,
//! `Project\Database\Configuration`, `Heading`, `Postgresql\General`,
//! `Shared\Danger`) read and do on the server.

use std::sync::LazyLock;

use axum::Extension;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use coolify_db::databases::{self as dbs, NewPostgresql};
use coolify_db::servers::ColumnValue;
use coolify_domain::naming::strip_tags;
use regex::Regex;
use serde_json::{Map, Value, json};

use crate::auth::AuthCtx;
use crate::input::Input;
use crate::response::{ApiError, ApiResult, created, ok};
use crate::routes::databases::present;
use crate::state::AppState;
use crate::validation::{Field, Messages, Rule, patterns, validate};

const NOT_FOUND: ApiError = ApiError::Message(StatusCode::NOT_FOUND, "Database not found.");

/// `Project\Resource\Index`: the environment's resources.
pub async fn resources(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((project, env)): Path<(String, String)>,
) -> ApiResult {
    let p = coolify_db::projects::find(&state.db, ctx.team_id, &project)
        .await?
        .ok_or(ApiError::Message(
            StatusCode::NOT_FOUND,
            "Project not found.",
        ))?;
    let e = coolify_db::projects::find_environment(&state.db, p.id, &env)
        .await?
        .ok_or(ApiError::Message(
            StatusCode::NOT_FOUND,
            "Environment not found.",
        ))?;
    let mut databases = Vec::new();
    let mut unported = dbs::count_other_resources(&state.db, e.id).await?;
    for d in dbs::list_for_environment(&state.db, e.id).await? {
        if d.kind.ported() {
            databases.push(present(&state, &ctx, &d, false, None).await?);
        } else {
            unported += 1;
        }
    }
    Ok(ok(json!({"databases": databases, "unported": unported})).into_response())
}

/// `Project\New\Select`: servers that can host resources, with their destinations.
pub async fn destinations(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
) -> ApiResult {
    let mut servers = Vec::new();
    for s in coolify_db::servers::list_by_name(&state.db, ctx.team_id).await? {
        let settings = s.settings.clone().unwrap_or_default();
        let flag = |k: &str| settings.get(k).and_then(Value::as_bool);
        let ip = s
            .record
            .get("ip")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let functional = flag("is_reachable") == Some(true)
            && flag("is_usable") == Some(true)
            && flag("force_disabled") == Some(false)
            && !(ip.is_empty() || ["1.2.3.4", "0.0.0.0", "::"].contains(&ip));
        let dests: Vec<Value> = dbs::destination_records(&state.db, s.id)
            .await?
            .into_iter()
            .map(|(uuid, name, network)| json!({"uuid": uuid, "name": name, "network": network}))
            .collect();
        servers.push(json!({
            "uuid": s.uuid,
            "name": s.record.get("name"),
            "description": s.record.get("description"),
            "ip": ip,
            "functional": functional,
            "is_build": crate::routes::servers::effective_role_is_build(&settings),
            "destinations": dests,
        }));
    }
    Ok(ok(json!({"servers": servers})).into_response())
}

/// `Project\Resource\Create` for `?type=postgresql&destination=...&database_image=...`.
pub async fn create(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    body: axum::body::Bytes,
) -> ApiResult {
    // `createAnyResource`: admins and owners.
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let input: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let s = |k: &str| {
        input
            .get(k)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    if s("type") != "postgresql" {
        return Err(ApiError::NotPorted);
    }
    let project = coolify_db::projects::find(&state.db, ctx.team_id, &s("project_uuid"))
        .await?
        .ok_or(ApiError::Message(
            StatusCode::NOT_FOUND,
            "Project not found.",
        ))?;
    let env = coolify_db::projects::find_environment(&state.db, project.id, &s("environment_uuid"))
        .await?
        .ok_or(ApiError::Message(
            StatusCode::NOT_FOUND,
            "Environment not found.",
        ))?;
    let (destination_id, _server_id) =
        dbs::team_destination(&state.db, ctx.team_id, &s("destination_uuid"))
            .await?
            .ok_or(ApiError::Message(
                StatusCode::NOT_FOUND,
                "Destination not found.",
            ))?;
    let image = s("database_image");
    if image.is_empty() {
        let mut m = Map::new();
        m.insert(
            "database_image".into(),
            json!(["The database image field is required."]),
        );
        return Err(ApiError::Validation(m));
    }
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let new = NewPostgresql {
        environment_id: env.id,
        destination_id,
        image,
        password_encrypted: enc.encrypt_string(&crate::routes::databases::random_password()),
        fields: Vec::new(),
        name: None,
    };
    let (_, uuid) = dbs::create_postgresql(&state.db, new, state.now()).await?;
    tracing::info!(target: "audit", event = "database.created", team_id = ctx.team_id, database_uuid = %uuid, "database created");
    Ok(created(json!({"uuid": uuid})).into_response())
}

/// What the database page polls: the latest start, the restart hint, and whether the viewer may change it.
pub async fn state(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let d = dbs::find_in_team(&state.db, ctx.team_id, &uuid)
        .await?
        .ok_or(NOT_FOUND)?;
    if !d.kind.ported() {
        return Err(ApiError::NotPorted);
    }
    // `Database\Configuration::mount`: a running database without a hash gets one.
    if d.str("status").unwrap_or_default().starts_with("running")
        && d.str("config_hash").is_none()
        && ctx.is_team_admin()
    {
        let hash = coolify_jobs::databases::config_hash(&d);
        dbs::update_if_changed(
            &state.db,
            d.kind,
            d.id,
            &[("config_hash", ColumnValue::Text(Some(hash)))],
            state.now(),
        )
        .await?;
    }
    let activity = dbs::latest_start_activity(&state.db, &uuid).await?.map(|(id, props, description)| {
        let entries: Value = serde_json::from_str(&description).unwrap_or(Value::Array(vec![]));
        json!({"id": id, "status": props.get("status").cloned().unwrap_or(Value::Null), "entries": entries})
    });
    Ok(ok(json!({
        "activity": activity,
        "configuration_changed": coolify_jobs::databases::configuration_changed(&d),
        "can_update": ctx.is_team_admin(),
    }))
    .into_response())
}

static DB_IDENTIFIER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[A-Za-z_][A-Za-z0-9_]{0,62}\n?\z").unwrap_or_else(|_| unreachable!())
});
static DB_PASSWORD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[A-Za-z0-9!@#%^*()_+\-=\[\]{}:,.?/~]+\n?\z").unwrap_or_else(|_| unreachable!())
});
/// `ValidationPatterns::PORT_MAPPINGS_PATTERN`.
static PORT_MAPPINGS: LazyLock<Regex> = LazyLock::new(|| {
    let ip = r"(?:(?:\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3}|\[[\da-fA-F:]+\]):)?";
    let host = r"(?:\d+(?:-\d+)?(?:/(?:tcp|udp|sctp))?)?";
    let container = r"\d+(?:-\d+)?(?:/(?:tcp|udp|sctp))?";
    let pair = format!("{ip}{host}:{ip}{container}");
    Regex::new(&format!(r"^{pair}(?:,{pair})*\n?\z")).unwrap_or_else(|_| unreachable!())
});

/// `Postgresql\General::messages()` with its `$validationAttributes`.
const GENERAL_MESSAGES: Messages = &[
    ("name.required", "The Name field is required."),
    ("name.string", "The Name field must be a string."),
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
        "description.string",
        "The Description field must be a string.",
    ),
    (
        "description.regex",
        "The description may only contain letters (including Unicode), numbers, spaces, and common punctuation: - _ . , ! ? ( ) ' \" + = * / @ &",
    ),
    (
        "description.max",
        "The description may not be greater than :max characters.",
    ),
    (
        "postgres_user.required",
        "The Postgres User field is required.",
    ),
    (
        "postgres_user.string",
        "The Postgres User field must be a string.",
    ),
    (
        "postgres_user.regex",
        "The Postgres User may only contain letters, digits, and underscores, and must start with a letter or underscore.",
    ),
    (
        "postgres_user.min",
        "The Postgres User must be at least :min character.",
    ),
    (
        "postgres_user.max",
        "The Postgres User may not be greater than :max characters.",
    ),
    (
        "postgres_password.required",
        "The Postgres Password field is required.",
    ),
    (
        "postgres_password.string",
        "The Postgres Password field must be a string.",
    ),
    (
        "postgres_password.regex",
        "The Postgres Password may not contain shell-unsafe characters (backtick, $, ;, |, &, <, >, \\, quotes, spaces, or control characters).",
    ),
    (
        "postgres_password.min",
        "The Postgres Password must be at least :min character.",
    ),
    (
        "postgres_password.max",
        "The Postgres Password may not be greater than :max characters.",
    ),
    ("postgres_db.required", "The Postgres DB field is required."),
    (
        "postgres_db.string",
        "The Postgres DB field must be a string.",
    ),
    (
        "postgres_db.regex",
        "The Postgres Database may only contain letters, digits, and underscores, and must start with a letter or underscore.",
    ),
    (
        "postgres_db.min",
        "The Postgres Database must be at least :min character.",
    ),
    (
        "postgres_db.max",
        "The Postgres Database may not be greater than :max characters.",
    ),
    ("image.required", "The Docker Image field is required."),
    (
        "ports_mappings.string",
        "The Port Mapping field must be a string.",
    ),
    (
        "ports_mappings.regex",
        "Port mappings must be a comma-separated list of port pairs or ranges with optional IP and protocol (e.g. 3000:3000, 8080:80/udp, 127.0.0.1:8080:80, [::1]::80).",
    ),
    (
        "is_public.boolean",
        "The Is Public field must be true or false.",
    ),
    ("public_port.integer", "The Public Port must be an integer."),
    (
        "public_port.min.numeric",
        "The Public Port must be at least 1.",
    ),
    (
        "public_port.max.numeric",
        "The Public Port must not exceed 65535.",
    ),
    (
        "public_port_timeout.integer",
        "The Public Port Timeout must be an integer.",
    ),
    (
        "public_port_timeout.min.numeric",
        "The Public Port Timeout must be at least 1.",
    ),
    (
        "is_log_drain_enabled.boolean",
        "The is log drain enabled field must be true or false.",
    ),
];

/// `Postgresql\General::submit`.
pub async fn update(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    body: axum::body::Bytes,
) -> ApiResult {
    let d = dbs::find_in_team(&state.db, ctx.team_id, &uuid)
        .await?
        .ok_or(NOT_FOUND)?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    if !d.kind.ported() {
        return Err(ApiError::NotPorted);
    }
    let mut map = match serde_json::from_slice::<Value>(&body) {
        Ok(Value::Object(m)) => m,
        _ => Map::new(),
    };
    // submit(): spaces out of port mappings, an empty public port means none.
    if let Some(Value::String(p)) = map.get("ports_mappings") {
        let v = p.replace(' ', "").trim().to_owned();
        map.insert("ports_mappings".into(), Value::String(v));
    }
    if map
        .get("public_port")
        .is_some_and(|v| v.as_str() == Some(""))
    {
        map.insert("public_port".into(), Value::Null);
    }
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let current_password = d
        .str("postgres_password")
        .and_then(|c| enc.decrypt_string(c).ok())
        .unwrap_or_default();
    let changed = |k: &str, current: Option<&str>| map.get(k).and_then(Value::as_str) != current;
    let id_rules = |enforce: bool| {
        let mut r = vec![Rule::Required, Rule::String, Rule::Min(1), Rule::Max(63)];
        if enforce {
            r.push(Rule::Regex(&DB_IDENTIFIER));
        }
        r
    };
    let mut pw_rules = vec![Rule::Required, Rule::String, Rule::Min(1), Rule::Max(128)];
    if changed("postgres_password", Some(&current_password)) {
        pw_rules.push(Rule::Regex(&DB_PASSWORD));
    }
    let mut name_field = patterns::name(true);
    name_field.rules.retain(|_| true);
    let input = Input(map.clone());
    let keys: Vec<&str> = map.keys().map(String::as_str).collect();
    validate(
        &input,
        &[
            name_field,
            patterns::description(),
            Field {
                name: "postgres_user",
                rules: id_rules(changed("postgres_user", d.str("postgres_user"))),
            },
            Field {
                name: "postgres_password",
                rules: pw_rules,
            },
            Field {
                name: "postgres_db",
                rules: id_rules(changed("postgres_db", d.str("postgres_db"))),
            },
            Field {
                name: "image",
                rules: vec![Rule::Required],
            },
            Field {
                name: "ports_mappings",
                rules: vec![Rule::Nullable, Rule::String, Rule::Regex(&PORT_MAPPINGS)],
            },
            Field {
                name: "is_public",
                rules: vec![Rule::Nullable, Rule::Boolean],
            },
            Field {
                name: "public_port",
                rules: vec![
                    Rule::Nullable,
                    Rule::Integer,
                    Rule::MinNum(1),
                    Rule::MaxNum(65535),
                ],
            },
            Field {
                name: "public_port_timeout",
                rules: vec![Rule::Nullable, Rule::Integer, Rule::MinNum(1)],
            },
            Field {
                name: "is_log_drain_enabled",
                rules: vec![Rule::Nullable, Rule::Boolean],
            },
        ],
        GENERAL_MESSAGES,
        &keys,
    )?;
    // Making the database public (TCP proxy) and log drains are not ported.
    let flag = |k: &str| map.get(k).is_some_and(crate::serialize::php_truthy);
    if (flag("is_public") && !d.bool("is_public"))
        || (flag("is_log_drain_enabled") && !d.bool("is_log_drain_enabled"))
    {
        return Err(ApiError::NotPorted);
    }

    let text = |k: &str| match map.get(k) {
        Some(Value::String(s)) => Some(s.clone()),
        Some(Value::Null) | None => None,
        Some(other) => Some(other.to_string()),
    };
    let mut values: Vec<(&'static str, ColumnValue)> = Vec::new();
    for key in ["name", "description"] {
        if map.contains_key(key) {
            values.push((
                key,
                ColumnValue::Text(Some(strip_tags(text(key).as_deref()))),
            ));
        }
    }
    for key in [
        "postgres_user",
        "postgres_db",
        "postgres_initdb_args",
        "postgres_host_auth_method",
        "postgres_conf",
        "image",
        "ports_mappings",
        "custom_docker_run_options",
    ] {
        if map.contains_key(key) {
            // `portsMappings` mutator: '' is stored as null.
            let v = text(key).filter(|v| !(key == "ports_mappings" && v.is_empty()));
            values.push((key, ColumnValue::Text(v)));
        }
    }
    let int_or_null = |k: &str| match map.get(k) {
        Some(Value::Number(n)) => n.as_i64().filter(|v| *v != 0),
        Some(Value::String(s)) => s.trim().parse::<i64>().ok().filter(|v| *v != 0),
        _ => None,
    };
    for key in ["public_port", "public_port_timeout"] {
        if map.contains_key(key) {
            values.push((
                key,
                int_or_null(key).map_or(ColumnValue::Text(None), ColumnValue::Int),
            ));
        }
    }
    let now = state.now();
    let mut changed_any = dbs::update_if_changed(&state.db, d.kind, d.id, &values, now).await?;
    // The `encrypted` cast compares decrypted values.
    if let Some(Value::String(p)) = map.get("postgres_password")
        && *p != current_password
    {
        changed_any |= dbs::update_if_changed(
            &state.db,
            d.kind,
            d.id,
            &[(
                "postgres_password",
                ColumnValue::Text(Some(enc.encrypt_string(p))),
            )],
            now,
        )
        .await?;
    }
    let _ = changed_any;
    if d.str("config_hash").is_none()
        && let Some(fresh) = dbs::find_in_team(&state.db, ctx.team_id, &uuid).await?
    {
        let hash = coolify_jobs::databases::config_hash(&fresh);
        dbs::update_if_changed(
            &state.db,
            d.kind,
            d.id,
            &[("config_hash", ColumnValue::Text(Some(hash)))],
            now,
        )
        .await?;
    }
    Ok(ok(json!({"message": "Database updated."})).into_response())
}

/// `Shared\Danger::delete`, with the page's confirmation steps.
pub async fn delete(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    body: axum::body::Bytes,
) -> ApiResult {
    let d = dbs::find_in_team(&state.db, ctx.team_id, &uuid)
        .await?
        .ok_or(NOT_FOUND)?;
    let input: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    if !crate::ui::password_confirmed(
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
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    if !d.kind.ported() || d.bool("is_public") {
        return Err(ApiError::NotPorted);
    }
    let unported = dbs::unported_parts(&state.db, d.kind, d.id).await?;
    if unported.iter().any(|p| {
        matches!(
            *p,
            "file storages" | "SSL certificates" | "scheduled backups"
        )
    }) {
        return Err(ApiError::NotPorted);
    }
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    let flag = |k: &str| input.get(k).and_then(Value::as_bool).unwrap_or(true);
    dbs::soft_delete(&state.db, d.kind, d.id, state.now()).await?;
    jobs.spawn_database_delete(
        d.clone(),
        flag("delete_volumes"),
        flag("delete_configurations"),
    );
    tracing::info!(target: "audit", event = "database.deleted", team_id = ctx.team_id, database_uuid = %d.uuid, "database deletion queued");
    Ok(ok(json!({"message": "Database deletion request queued."})).into_response())
}
