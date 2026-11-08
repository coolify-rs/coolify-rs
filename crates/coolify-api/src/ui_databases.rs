//! Session endpoints behind the database pages of the web UI: what the
//! Livewire components (`Project\Resource\Index`, `Project\Resource\Create`,
//! `Project\Database\Configuration`, `Heading`, `Postgresql\General`,
//! `Shared\Danger`) read and do on the server.

use std::sync::LazyLock;

use axum::Extension;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use coolify_db::databases::{self as dbs, Kind, NewDatabase};
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
use coolify_domain::grants::AccessLevel;

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
    let mut unported = 0;
    for d in dbs::list_for_environment(&state.db, e.id).await? {
        if d.kind.ported() {
            let mut v = present(&state, &ctx, &d, false, None).await?;
            // The page reads the type for the logo (`$resource->type()`), also for KeyDB.
            if let Value::Object(m) = &mut v {
                m.entry("database_type")
                    .or_insert_with(|| Value::String(d.kind.type_name().into()));
                m.insert(
                    "tags".into(),
                    resource_tags(&state, d.kind.morph(), d.id).await?,
                );
            }
            databases.push(v);
        } else {
            unported += 1;
        }
    }
    // `$this->applications->sortBy('name')`.
    let mut applications = Vec::new();
    let mut rows = coolify_db::applications::list_for_environment(&state.db, e.id).await?;
    rows.sort_by(|a, b| a.str("name").cmp(&b.str("name")));
    for a in &rows {
        let mut v = crate::routes::applications::present(&state, &ctx, a, false).await?;
        if let (Value::Object(m), Some(id)) = (&mut v, a.int("id")) {
            m.insert(
                "tags".into(),
                resource_tags(&state, "App\\Models\\Application", id).await?,
            );
        }
        applications.push(v);
    }
    Ok(ok(json!({
        "applications": applications,
        "databases": databases,
        "services": crate::ui_services::environment_services(&state, e.id).await?,
        "unported": unported,
    }))
    .into_response())
}

/// `Resource\Index::formatResource()`: `$item->tags->map(...)`.
async fn resource_tags(state: &AppState, morph: &str, id: i64) -> Result<Value, ApiError> {
    let rows = coolify_db::database_parts::tags_of(&state.db, morph, id).await?;
    Ok(Value::Array(
        rows.iter()
            .map(|t| json!({"id": t.get("id"), "name": t.get("name")}))
            .collect(),
    ))
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
            "id": s.id,
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
    let input: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let s = |k: &str| {
        input
            .get(k)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    let Some(kind) = Kind::ALL
        .into_iter()
        .find(|k| k.name_prefix() == s("type") && k.ported())
    else {
        return Err(ApiError::NotPorted);
    };
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
    // `createAnyResource` in the environment.
    if !ctx.can_create_in(Some(env.id)) {
        return Err(ApiError::Unauthorized);
    }
    let (destination_id, _server_id) =
        dbs::team_destination(&state.db, ctx.team_id, &s("destination_uuid"))
            .await?
            .ok_or(ApiError::Message(
                StatusCode::NOT_FOUND,
                "Destination not found.",
            ))?;
    let image = s("database_image");
    // PostgreSQL requires database_image to be explicitly set.
    if kind == Kind::Postgresql && image.is_empty() {
        let mut m = Map::new();
        m.insert(
            "database_image".into(),
            json!(["The database image field is required."]),
        );
        return Err(ApiError::Validation(m));
    }
    let (secrets, variables) = crate::routes::databases::generated_defaults(&state, kind, None)?;
    let new = NewDatabase {
        destination_type: Some(dbs::destination_morph(&state.db, &s("destination_uuid")).await?),
        environment_id: env.id,
        destination_id,
        image: (kind == Kind::Postgresql).then_some(image),
        secrets,
        fields: Vec::new(),
        name: None,
        variables,
    };
    let (_, uuid) = dbs::create(&state.db, kind, new, state.now()).await?;
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
        && ctx.can_db(AccessLevel::Write, &d)
    {
        let hash = crate::routes::databases::config_hash(&state, &d).await?;
        dbs::update_if_changed(
            &state.db,
            d.kind,
            d.id,
            &[("config_hash", ColumnValue::Text(Some(hash)))],
            state.now(),
        )
        .await?;
    }
    let saved = d.str("config_hash");
    let configuration_changed = saved.is_some()
        && saved
            != Some(
                crate::routes::databases::config_hash(&state, &d)
                    .await?
                    .as_str(),
            )
        && !d.str("status").unwrap_or_default().starts_with("exited");
    let activity = dbs::latest_start_activity(&state.db, &uuid).await?.map(|(id, props, description)| {
        let entries: Value = serde_json::from_str(&description).unwrap_or(Value::Array(vec![]));
        json!({"id": id, "status": props.get("status").cloned().unwrap_or(Value::Null), "entries": entries})
    });
    // What the General form and the Connection block read besides the record.
    let creds = crate::routes::databases::credentials(&state, &d).await?;
    let shared = |key: &str| creds.variables.iter().any(|v| v.key == key && v.is_shared);
    let mut form = json!({"credential_locked": creds.locked});
    if d.kind == Kind::Redis && ctx.can_db(AccessLevel::Write, &d) {
        form["redis_username"] = json!(creds.redis_username());
        form["redis_password"] = if creds.locked {
            Value::Null
        } else {
            json!(
                creds
                    .variables
                    .iter()
                    .find(|v| v.key == "REDIS_PASSWORD")
                    .and_then(|v| v.value.clone())
            )
        };
    }
    // `HasDatabaseStatusInfo::refresh`: `$certificateValidUntil`.
    if !matches!(d.kind, Kind::Clickhouse | Kind::Sqlite)
        && let Some(cert) =
            coolify_db::database_parts::resource_certificate(&state.db, d.kind.morph(), d.id)
                .await?
    {
        let now = state.now();
        form["certificate_valid_until"] =
            json!(cert.valid_until.format("%d.%m.%Y %H:%M:%S").to_string());
        form["certificate_state"] = json!(if now > cert.valid_until {
            "expired"
        } else if now + chrono::Duration::days(30) > cert.valid_until {
            "soon"
        } else {
            "valid"
        });
    }
    if d.kind == Kind::Redis {
        form["redis_version_6"] =
            json!(redis_version_at_least_6(d.str("image").unwrap_or_default()));
        form["shared_redis_username"] = json!(shared("REDIS_USERNAME"));
        form["shared_redis_password"] = json!(shared("REDIS_PASSWORD"));
    }
    Ok(ok(json!({
        "activity": activity,
        "flushable": matches!(d.kind, Kind::Redis | Kind::Keydb | Kind::Dragonfly),
        "configuration_changed": configuration_changed,
        "can_update": ctx.can_db(AccessLevel::Write, &d),
        "kind": d.kind.name_prefix(),
        "form": form,
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
pub(crate) static PORT_MAPPINGS: LazyLock<Regex> = LazyLock::new(|| {
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

/// `StandaloneSqlite::DATABASES_PATTERN`.
static SQLITE_DATABASES: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[A-Za-z0-9][A-Za-z0-9._-]*(,[A-Za-z0-9][A-Za-z0-9._-]*)*\n?\z")
        .unwrap_or_else(|_| unreachable!())
});

/// `Sqlite\General::rules()` for `sqliteDatabases`, with its `messages()`.
fn sqlite_databases_error(value: Option<&Value>) -> Option<&'static str> {
    match value.and_then(Value::as_str) {
        None | Some("") => Some("The Database Files field is required."),
        Some(v) if v.chars().count() > 255 => {
            Some("The sqlite databases field must not be greater than 255 characters.")
        }
        Some(v) if !SQLITE_DATABASES.is_match(v) => Some(
            "The Database Files must be comma-separated file names containing only letters, numbers, dots, dashes and underscores.",
        ),
        _ => None,
    }
}

/// How a credential or configuration field of a General form is validated.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Check {
    /// `databaseIdentifierRules` (pattern only when the value changed).
    Identifier,
    /// `databasePasswordRules` (pattern only when the value changed).
    Password,
    /// `nullable`.
    Conf,
}

/// A type-specific field of a General form: column, check, and the label its messages use.
#[derive(Clone, Copy)]
struct KindField {
    column: &'static str,
    check: Check,
    label: &'static str,
}

const fn field(column: &'static str, check: Check, label: &'static str) -> KindField {
    KindField {
        column,
        check,
        label,
    }
}

/// The type-specific fields of `<Type>\General::rules()`, in order.
fn kind_fields(kind: Kind) -> Vec<KindField> {
    use Check::{Conf, Identifier as Id, Password as Pw};
    match kind {
        Kind::Postgresql => vec![
            field("postgres_user", Id, "Postgres User"),
            field("postgres_password", Pw, "Postgres Password"),
            field("postgres_db", Id, "Postgres Database"),
        ],
        Kind::Redis => vec![
            field("redis_conf", Conf, ""),
            field("redis_username", Id, "Redis Username"),
            field("redis_password", Pw, "Redis Password"),
        ],
        Kind::Keydb => vec![
            field("keydb_conf", Conf, ""),
            field("keydb_password", Pw, "KeyDB Password"),
        ],
        Kind::Dragonfly => vec![field("dragonfly_password", Pw, "Dragonfly Password")],
        Kind::Mongodb => vec![
            field("mongo_conf", Conf, ""),
            field("mongo_initdb_root_username", Id, "Root Username"),
            field("mongo_initdb_root_password", Pw, "Root Password"),
            field("mongo_initdb_database", Id, "MongoDB Database"),
        ],
        Kind::Mysql => vec![
            field("mysql_root_password", Pw, "Root Password"),
            field("mysql_user", Id, "MySQL User"),
            field("mysql_password", Pw, "MySQL Password"),
            field("mysql_database", Id, "MySQL Database"),
            field("mysql_conf", Conf, ""),
        ],
        Kind::Mariadb => vec![
            field("mariadb_root_password", Pw, "Root Password"),
            field("mariadb_user", Id, "MariaDB User"),
            field("mariadb_password", Pw, "MariaDB Password"),
            field("mariadb_database", Id, "MariaDB Database"),
            field("mariadb_conf", Conf, ""),
        ],
        Kind::Clickhouse => vec![
            field("clickhouse_admin_user", Id, "Admin User"),
            field("clickhouse_admin_password", Pw, "Admin Password"),
        ],
        Kind::Sqlite => vec![],
    }
}

/// `$validationAttributes` of the General forms that define them (column => label).
fn validation_attributes(kind: Kind) -> &'static [(&'static str, &'static str)] {
    const COMMON: [(&str, &str); 7] = [
        ("name", "Name"),
        ("description", "Description"),
        ("image", "Image"),
        ("ports_mappings", "Port Mapping"),
        ("is_public", "Is Public"),
        ("public_port", "Public Port"),
        ("public_port_timeout", "Public Port Timeout"),
    ];
    type Attrs = Vec<(Kind, Vec<(&'static str, &'static str)>)>;
    static ATTRS: LazyLock<Attrs> = LazyLock::new(|| {
        let with = |extra: &[(&'static str, &'static str)]| {
            let mut v = COMMON.to_vec();
            v.extend_from_slice(extra);
            v
        };
        vec![
            (
                Kind::Redis,
                with(&[
                    ("redis_conf", "Redis Configuration"),
                    ("custom_docker_run_options", "Custom Docker Options"),
                    ("redis_username", "Redis Username"),
                    ("redis_password", "Redis Password"),
                ]),
            ),
            (
                Kind::Mongodb,
                with(&[
                    ("mongo_conf", "Mongo Configuration"),
                    ("mongo_initdb_root_username", "Root Username"),
                    ("mongo_initdb_root_password", "Root Password"),
                    ("mongo_initdb_database", "Database"),
                    ("custom_docker_run_options", "Custom Docker Run Options"),
                ]),
            ),
            (
                Kind::Mysql,
                with(&[
                    ("mysql_root_password", "Root Password"),
                    ("mysql_user", "User"),
                    ("mysql_password", "Password"),
                    ("mysql_database", "Database"),
                    ("mysql_conf", "MySQL Configuration"),
                    ("custom_docker_run_options", "Custom Docker Run Options"),
                ]),
            ),
            (
                Kind::Mariadb,
                with(&[
                    ("mariadb_root_password", "Root Password"),
                    ("mariadb_user", "User"),
                    ("mariadb_password", "Password"),
                    ("mariadb_database", "Database"),
                    ("mariadb_conf", "MariaDB Configuration"),
                    ("custom_docker_run_options", "Custom Docker Options"),
                ]),
            ),
        ]
    });
    ATTRS
        .iter()
        .find(|(k, _)| *k == kind)
        .map_or(&[], |(_, v)| v.as_slice())
}

/// `<Type>\General::messages()` with its `$validationAttributes` (PostgreSQL keeps its own list).
fn general_messages(kind: Kind) -> Messages {
    static CACHE: LazyLock<Vec<(Kind, Messages)>> = LazyLock::new(|| {
        Kind::ALL
            .into_iter()
            .filter(|k| *k != Kind::Postgresql)
            .map(|k| {
                let mut m: Vec<(String, String)> = Vec::new();
                let mut push = |k: &str, v: &str| m.push((k.to_owned(), v.to_owned()));
                for (k, v) in patterns::COMBINED_MESSAGES {
                    push(k, v);
                }
                push("ports_mappings.regex", PORT_MAPPINGS_MESSAGE);
                // The forms with a `name.required` override (and attributes).
                if matches!(k, Kind::Redis | Kind::Mongodb | Kind::Mysql | Kind::Mariadb) {
                    push("name.required", "The Name field is required.");
                }
                for f in kind_fields(k) {
                    let regex = match f.check {
                        Check::Identifier => format!(
                                "The {} may only contain letters, digits, and underscores, and must start with a letter or underscore.",
                                f.label
                            ),
                        Check::Password => format!(
                                "The {} may not contain shell-unsafe characters (backtick, $, ;, |, &, <, >, \\, quotes, spaces, or control characters).",
                                f.label
                            ),
                        Check::Conf => continue,
                    };
                    push(&format!("{}.regex", f.column), &regex);
                    push(
                        &format!("{}.min", f.column),
                        &format!("The {} must be at least :min character.", f.label),
                    );
                    push(
                        &format!("{}.max", f.column),
                        &format!("The {} may not be greater than :max characters.", f.label),
                    );
                }
                push("image.required", "The Docker Image field is required.");
                if matches!(k, Kind::Keydb | Kind::Dragonfly | Kind::Clickhouse) {
                    push("image.string", "The Docker Image must be a string.");
                }
                push("public_port.integer", "The Public Port must be an integer.");
                push("public_port.min.numeric", "The Public Port must be at least 1.");
                push("public_port.max.numeric", "The Public Port must not exceed 65535.");
                push(
                    "public_port_timeout.integer",
                    "The Public Port Timeout must be an integer.",
                );
                push(
                    "public_port_timeout.min.numeric",
                    "The Public Port Timeout must be at least 1.",
                );
                for (col, label) in validation_attributes(k) {
                    push(&format!("attribute:{col}"), label);
                }
                let leaked: Vec<(&'static str, &'static str)> = m
                    .into_iter()
                    .map(|(a, b)| (&*Box::leak(a.into_boxed_str()), &*Box::leak(b.into_boxed_str())))
                    .collect();
                (k, &*Box::leak(leaked.into_boxed_slice()))
            })
            .collect()
    });
    if kind == Kind::Postgresql {
        return GENERAL_MESSAGES;
    }
    CACHE
        .iter()
        .find(|(k, _)| *k == kind)
        .map_or(GENERAL_MESSAGES, |(_, m)| m)
}

const PORT_MAPPINGS_MESSAGE: &str = "Port mappings must be a comma-separated list of port pairs or ranges with optional IP and protocol (e.g. 3000:3000, 8080:80/udp, 127.0.0.1:8080:80, [::1]::80).";

/// PHP `version_compare($v, '6.0', '>=')` for an image tag (leading number).
fn redis_version_at_least_6(image: &str) -> bool {
    let tag = image.split(':').nth(1).unwrap_or("0.0");
    tag.chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse::<u64>()
        .is_ok_and(|n| n >= 6)
}

/// `<Type>\General::submit`.
pub async fn update(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    body: axum::body::Bytes,
) -> ApiResult {
    let d = dbs::find_in_team(&state.db, ctx.team_id, &uuid)
        .await?
        .ok_or(NOT_FOUND)?;
    if !ctx.can_db(AccessLevel::Write, &d) {
        return Err(ApiError::Unauthorized);
    }
    if !d.kind.ported() {
        return Err(ApiError::NotPorted);
    }
    let kind = d.kind;
    let mut map = match serde_json::from_slice::<Value>(&body) {
        Ok(Value::Object(m)) => m,
        _ => Map::new(),
    };
    // submit(): spaces out of port mappings, an empty public port means none (not in Redis).
    if let Some(Value::String(p)) = map.get("ports_mappings") {
        let v = p.replace(' ', "").trim().to_owned();
        map.insert("ports_mappings".into(), Value::String(v));
    }
    if kind != Kind::Redis
        && map
            .get("public_port")
            .is_some_and(|v| v.as_str() == Some(""))
    {
        map.insert("public_port".into(), Value::Null);
    }
    if kind == Kind::Mongodb
        && map
            .get("mongo_conf")
            .is_some_and(|v| v.as_str().is_none_or(str::is_empty))
    {
        map.insert("mongo_conf".into(), Value::Null);
    }
    // `Sqlite\General::submit`: the file names are trimmed, then `DATABASES_PATTERN` applies.
    if kind == Kind::Sqlite
        && let Some(Value::String(list)) = map.get("sqlite_databases")
    {
        let joined = list
            .split(',')
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .collect::<Vec<_>>()
            .join(",");
        map.insert("sqlite_databases".into(), Value::String(joined));
    }
    let creds = crate::routes::databases::credentials(&state, &d).await?;
    let started = d.str("started_at").is_some();
    // The values the model holds now (the `!==` of the conditional patterns).
    let current = |column: &str| -> Option<String> {
        match column {
            "redis_username" => Some(creds.redis_username()),
            "redis_password" => creds
                .variables
                .iter()
                .find(|v| v.key == "REDIS_PASSWORD")
                .and_then(|v| v.value.clone()),
            c if kind.encrypted_columns().contains(&c) => Some(creds.secret(c)),
            c => d.str(c).map(str::to_owned),
        }
    };
    let changed =
        |column: &str| map.get(column).and_then(Value::as_str) != current(column).as_deref();
    let locked = kind == Kind::Redis && creds.locked;
    let mut fields: Vec<Field> = vec![patterns::name(true), patterns::description()];
    let mut redis_fields: Vec<Field> = Vec::new();
    for f in kind_fields(kind) {
        let rules = match f.check {
            Check::Conf => vec![Rule::Nullable],
            Check::Password if f.column == "redis_password" && locked => vec![Rule::Nullable],
            check => {
                let max = if check == Check::Identifier { 63 } else { 128 };
                let mut r = vec![Rule::Required, Rule::String, Rule::Min(1), Rule::Max(max)];
                if changed(f.column) {
                    r.push(Rule::Regex(if check == Check::Identifier {
                        &DB_IDENTIFIER
                    } else {
                        &DB_PASSWORD
                    }));
                }
                r
            }
        };
        let field = Field {
            name: f.column,
            rules,
        };
        // Redis validates its credentials after the common fields.
        if kind == Kind::Redis && f.check != Check::Conf {
            redis_fields.push(field);
        } else {
            fields.push(field);
        }
    }
    let image_rules = if matches!(
        kind,
        Kind::Keydb | Kind::Dragonfly | Kind::Clickhouse | Kind::Sqlite
    ) {
        vec![Rule::Required, Rule::String]
    } else {
        vec![Rule::Required]
    };
    fields.extend([
        Field {
            name: "image",
            rules: image_rules,
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
    ]);
    fields.extend(redis_fields);
    let input = Input(map.clone());
    let keys: Vec<&str> = map.keys().map(String::as_str).collect();
    validate(&input, &fields, general_messages(kind), &keys)?;
    if kind == Kind::Sqlite
        && let Some(message) = sqlite_databases_error(map.get("sqlite_databases"))
    {
        let mut errors = Map::new();
        errors.insert("sqlite_databases".into(), json!([message]));
        return Err(ApiError::Validation(errors));
    }
    let text = |k: &str| match map.get(k) {
        Some(Value::String(s)) => Some(s.clone()),
        Some(Value::Null) | None => None,
        Some(other) => Some(other.to_string()),
    };
    // Credentials only the first start reads (KeyDB, Dragonfly, ClickHouse) are kept after it.
    let before_start_only = matches!(kind, Kind::Keydb | Kind::Dragonfly | Kind::Clickhouse);
    let mut values: Vec<(&'static str, ColumnValue)> = Vec::new();
    for key in ["name", "description"] {
        if map.contains_key(key) {
            values.push((
                key,
                ColumnValue::Text(Some(strip_tags(text(key).as_deref()))),
            ));
        }
    }
    let mut columns: Vec<&'static str> = Vec::new();
    if kind == Kind::Sqlite {
        columns.push("sqlite_databases");
    }
    for f in kind_fields(kind) {
        if kind == Kind::Redis && f.check != Check::Conf {
            continue;
        }
        if f.check != Check::Conf && before_start_only && started {
            continue;
        }
        columns.push(f.column);
    }
    if kind == Kind::Postgresql {
        columns.extend([
            "postgres_initdb_args",
            "postgres_host_auth_method",
            "postgres_conf",
        ]);
    }
    columns.extend(["image", "ports_mappings", "custom_docker_run_options"]);
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let now = state.now();
    let mut encrypted: Vec<(&'static str, String)> = Vec::new();
    for key in columns {
        if !map.contains_key(key) {
            continue;
        }
        if kind.encrypted_columns().contains(&key) {
            // The `encrypted` cast compares decrypted values.
            if let Some(p) = text(key)
                && Some(p.as_str()) != current(key).as_deref()
            {
                encrypted.push((key, p));
            }
            continue;
        }
        // `portsMappings` mutator: '' is stored as null.
        let v = text(key).filter(|v| !(key == "ports_mappings" && v.is_empty()));
        values.push((key, ColumnValue::Text(v)));
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
    for (key, plain) in &encrypted {
        values.push((key, ColumnValue::Text(Some(enc.encrypt_string(plain)))));
        // The KeyDB and Dragonfly `saving` hooks: a new password is quoted the current way.
        if matches!(kind, Kind::Keydb | Kind::Dragonfly) {
            values.push(("legacy_password_quoting", ColumnValue::Bool(false)));
        }
    }
    dbs::update_if_changed(&state.db, kind, d.id, &values, now).await?;

    if kind == Kind::Redis {
        // `runtime_environment_variables()->updateOrCreate(...)` for the username (Redis 6+) and
        // the password (unless it is locked: the form never loads it).
        let mut writes: Vec<(&str, Option<String>)> = Vec::new();
        if redis_version_at_least_6(&text("image").unwrap_or_default()) {
            writes.push(("REDIS_USERNAME", text("redis_username")));
        }
        if !creds.locked {
            writes.push(("REDIS_PASSWORD", text("redis_password")));
        }
        for (key, value) in writes {
            let existing = creds.variables.iter().find(|v| v.key == key);
            if existing.is_some_and(|v| {
                v.value.as_deref() == value.as_deref().map(str::trim).filter(|v| !v.is_empty())
            }) {
                continue;
            }
            let cipher = value.as_deref().map(|v| enc.encrypt_serialized(v.trim()));
            coolify_db::environment_variables::upsert(
                &state.db,
                kind.morph(),
                d.id,
                key,
                cipher.as_deref(),
                value.as_deref().is_some_and(is_shared_reference),
                &state.version,
                now,
            )
            .await?;
        }
    } else if d.str("config_hash").is_none()
        && let Some(fresh) = dbs::find_in_team(&state.db, ctx.team_id, &uuid).await?
    {
        let hash = crate::routes::databases::config_hash(&state, &fresh).await?;
        dbs::update_if_changed(
            &state.db,
            kind,
            d.id,
            &[("config_hash", ColumnValue::Text(Some(hash)))],
            now,
        )
        .await?;
    }
    Ok(ok(json!({"message": "Database updated."})).into_response())
}

/// `EnvironmentVariable::isSharedReference` (the `saving` hook sets `is_shared` from it).
fn is_shared_reference(value: &str) -> bool {
    static RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?s)^\{\{\s*(?:team|project|environment|server)\..*\}\}$")
            .unwrap_or_else(|_| unreachable!())
    });
    !value.trim().is_empty() && RE.is_match(value.trim())
}

/// `Shared\Danger::flush($password)`.
pub async fn flush(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    body: axum::body::Bytes,
) -> ApiResult {
    let d = dbs::find_in_team(&state.db, ctx.team_id, &uuid)
        .await?
        .ok_or(NOT_FOUND)?;
    if !matches!(d.kind, Kind::Redis | Kind::Keydb | Kind::Dragonfly) {
        return Err(NOT_FOUND);
    }
    if !ctx.can_db(AccessLevel::Write, &d) {
        return Err(ApiError::Unauthorized);
    }
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
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    if let Err(message) = jobs.flush_database(&d).await {
        return Ok(ok(json!({"error": message})).into_response());
    }
    tracing::info!(target: "audit", event = "ui.database.flushed", team_id = ctx.team_id, database_uuid = %d.uuid, "database flushed");
    Ok(ok(json!({"message": "Database flushed."})).into_response())
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
    if !ctx.can_db(AccessLevel::Delete, &d) {
        return Err(ApiError::Unauthorized);
    }
    // `Shared\Danger::delete` refuses a SQLite database that applications still mount.
    if d.kind == Kind::Sqlite {
        let names =
            coolify_db::database_parts::sqlite_connected_applications(&state.db, d.id).await?;
        if !names.is_empty() {
            let message = format!(
                "This database volume is mounted by {}. Remove those mounts before deleting the database.",
                names.join(", ")
            );
            return Ok(crate::response::PhpJson(
                StatusCode::UNPROCESSABLE_ENTITY,
                json!({"message": message}),
            )
            .into_response());
        }
    }
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    let flag = |k: &str| input.get(k).and_then(Value::as_bool).unwrap_or(true);
    dbs::soft_delete(&state.db, d.kind, d.id, state.now()).await?;
    jobs.spawn_database_delete_ui(
        d.clone(),
        flag("delete_volumes"),
        flag("delete_configurations"),
    );
    tracing::info!(target: "audit", event = "database.deleted", team_id = ctx.team_id, database_uuid = %d.uuid, "database deletion queued");
    Ok(ok(json!({"message": "Database deletion request queued."})).into_response())
}
