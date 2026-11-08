//! `DatabasesController`: list, show, create, start, stop, restart and delete.
//! The other routes are in `database_more`, `database_envs`, `database_storages`
//! and `database_backups`.
//!
//! Responses follow `removeSensitiveData` + `serializeApiResponse`: the model
//! attributes sorted, with the destination (its server and settings, loaded
//! by the `server_status` accessor) nested as the models serialize them.

use axum::Extension;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::IntoResponse;
use coolify_db::databases::{self as dbs, DatabaseRow, Kind, NewDatabase};
use coolify_db::records::{Record, SERVER_SETTING};
use coolify_db::servers::{self, ColumnValue, ServerRow};
use coolify_domain::naming::strip_tags;
use coolify_jobs::databases::connection as conn;
use rand::Rng as _;
use serde_json::{Map, Value, json};

use crate::auth::AuthCtx;
use crate::input::{Input, lenient_json, query_param, validated_json};
use crate::response::{ApiError, ApiResult, created, ok};
use crate::routes::servers::{ip_accessor, user_accessor};
use crate::serialize::{php_truthy, serialize_record};
use crate::state::AppState;
use crate::validation::{CUSTOM_API_MESSAGES, Field, Rule, validate};
use coolify_domain::grants::AccessLevel;

pub(crate) const NOT_FOUND: ApiError =
    ApiError::Message(StatusCode::NOT_FOUND, "Database not found.");

/// `ServerSetting` attributes hidden by the model; the sensitive ones are made
/// visible for `can_read_sensitive` (`exposeNestedServerSecrets`).
const SETTING_HIDDEN: &[&str] = &[
    "is_build_server",
    "sentinel_token",
    "sentinel_custom_url",
    "logdrain_newrelic_license_key",
    "logdrain_axiom_api_key",
    "logdrain_custom_config",
    "logdrain_custom_config_parser",
    "geoip_maxmind_license_key",
];
const SETTING_SENSITIVE: &[&str] = &[
    "sentinel_token",
    "sentinel_custom_url",
    "logdrain_newrelic_license_key",
    "logdrain_axiom_api_key",
    "logdrain_custom_config",
    "logdrain_custom_config_parser",
];
const SETTING_ENCRYPTED: &[&str] = &["sentinel_token", "logdrain_custom_config"];

fn decrypt(state: &AppState, cipher: &str) -> Result<String, ApiError> {
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    enc.decrypt_string(cipher).map_err(|e| {
        tracing::error!(error = %e, "cannot decrypt a database attribute (APP_KEY mismatch?)");
        ApiError::Internal
    })
}

/// `$server->toArray()` as nested in a database (`$hidden`, accessors, appends, settings).
pub(crate) fn nested_server(
    state: &AppState,
    ctx: &AuthCtx,
    s: &ServerRow,
) -> Result<Value, ApiError> {
    nested_server_with(state, s, ctx.can_read_sensitive, ctx.can_read_sensitive)
}

/// `nested_server` with the two switches apart: `expose_secrets` (`exposeNestedServerSecrets`)
/// and `sensitive_payload` (whether `serializeApiResponse` keeps the proxy configuration).
pub(crate) fn nested_server_with(
    state: &AppState,
    s: &ServerRow,
    expose_secrets: bool,
    sensitive_payload: bool,
) -> Result<Value, ApiError> {
    let mut r = s.record.clone();
    r.shift_remove("proxy_error_page");
    if let Some(Value::String(ip)) = r.get("ip") {
        let v = ip_accessor(ip);
        r.insert("ip".into(), Value::String(v));
    }
    if let Some(Value::String(u)) = r.get("user") {
        let v = user_accessor(u);
        r.insert("user".into(), Value::String(v));
    }
    if !sensitive_payload {
        // ApiSensitiveData strips these wherever they appear.
        r.shift_remove("validation_logs");
        if let Some(Value::Object(proxy)) = r.get_mut("proxy") {
            proxy.shift_remove("last_saved_proxy_configuration");
        }
    }
    r.insert("is_coolify_host".into(), Value::Bool(s.id == 0));
    let mut settings: Record = s
        .settings
        .clone()
        .map(|x| SERVER_SETTING.present(x))
        .unwrap_or_default();
    for key in SETTING_HIDDEN {
        let visible = expose_secrets && SETTING_SENSITIVE.contains(key);
        if !visible {
            settings.shift_remove(*key);
        } else if SETTING_ENCRYPTED.contains(key)
            && let Some(Value::String(c)) = settings.get(*key)
        {
            let plain = decrypt(state, c)?;
            settings.insert((*key).into(), Value::String(plain));
        }
    }
    r.insert("settings".into(), Value::Object(settings));
    Ok(Value::Object(r))
}

/// The decrypted credentials of a database.
pub(crate) async fn credentials(
    state: &AppState,
    d: &DatabaseRow,
) -> Result<conn::Credentials, ApiError> {
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    conn::credentials(&state.db, enc, d).await.map_err(|e| {
        tracing::error!(error = %e, "cannot decrypt a database attribute (APP_KEY mismatch?)");
        ApiError::Internal
    })
}

/// `isConfigurationChanged`'s hash, with the database's variables.
pub(crate) async fn config_hash(state: &AppState, d: &DatabaseRow) -> Result<String, ApiError> {
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let variables = coolify_jobs::databases::env::load_stored(&state.db, enc, d)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "cannot read the variables of a database");
            ApiError::Internal
        })?;
    Ok(coolify_jobs::databases::config_hash(
        d,
        &coolify_jobs::databases::env::config_hash_values(&variables),
    ))
}

type Generated = (
    Vec<(&'static str, String)>,
    Vec<coolify_db::environment_variables::NewVariable>,
);

/// What `create_standalone_*` generates: the encrypted passwords and, for Redis, the
/// `REDIS_PASSWORD` and `REDIS_USERNAME` variables.
pub(crate) fn generated_defaults(
    state: &AppState,
    kind: Kind,
    redis_password: Option<String>,
) -> Result<Generated, ApiError> {
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let secrets = kind
        .generated_secrets()
        .iter()
        .map(|c| (*c, enc.encrypt_string(&random_password())))
        .collect();
    let variables = if kind == Kind::Redis {
        let variable = |key: &str, value: &str| coolify_db::environment_variables::NewVariable {
            key: key.into(),
            value_encrypted: Some(enc.encrypt_serialized(value)),
            is_shared: false,
            version: state.version.clone(),
        };
        vec![
            variable(
                "REDIS_PASSWORD",
                &redis_password.unwrap_or_else(random_password),
            ),
            variable("REDIS_USERNAME", "default"),
        ]
    } else {
        Vec::new()
    };
    Ok((secrets, variables))
}

fn server_ip(server: Option<&ServerRow>) -> Option<&str> {
    server
        .and_then(|s| s.record.get("ip"))
        .and_then(Value::as_str)
}

/// `isFunctional()` of the destination's server.
pub(crate) fn server_functional(s: &ServerRow) -> bool {
    let settings = s.settings.clone().unwrap_or_default();
    let flag = |k: &str| settings.get(k).and_then(Value::as_bool);
    let ip = s
        .record
        .get("ip")
        .and_then(Value::as_str)
        .unwrap_or_default();
    flag("is_reachable") == Some(true)
        && flag("is_usable") == Some(true)
        && flag("force_disabled") == Some(false)
        && !(ip.is_empty() || ["1.2.3.4", "0.0.0.0", "::"].contains(&ip))
}

/// `$database::CONFIGURATION_ATTRIBUTES`: free-form configuration that can embed credentials. Only
/// users who can update the database and `read:sensitive` tokens read it.
pub(crate) fn configuration_attributes(kind: Kind) -> &'static [&'static str] {
    match kind {
        Kind::Postgresql => &["postgres_conf", "custom_docker_run_options"],
        Kind::Redis => &["redis_conf", "custom_docker_run_options"],
        Kind::Mongodb => &["mongo_conf", "custom_docker_run_options"],
        Kind::Mysql => &["mysql_conf", "custom_docker_run_options"],
        Kind::Mariadb => &["mariadb_conf", "custom_docker_run_options"],
        Kind::Keydb => &["keydb_conf", "custom_docker_run_options"],
        Kind::Dragonfly | Kind::Clickhouse | Kind::Sqlite => &["custom_docker_run_options"],
    }
}

/// `removeSensitiveData($database)`.
pub(crate) async fn present(
    state: &AppState,
    ctx: &AuthCtx,
    d: &DatabaseRow,
    with_environment: bool,
    backup_configs: Option<Value>,
) -> Result<Value, ApiError> {
    present_as(state, ctx, d, with_environment, backup_configs, false).await
}

/// `model`: the plain Eloquent model of `environment_details`, nothing hidden by the controller.
pub(crate) async fn present_as(
    state: &AppState,
    ctx: &AuthCtx,
    d: &DatabaseRow,
    with_environment: bool,
    backup_configs: Option<Value>,
    model: bool,
) -> Result<Value, ApiError> {
    let server = match d.server_id() {
        Some(id) => servers::find_by_id(&state.db, id).await?,
        None => None,
    };
    let mut r = dbs::present(d.kind, d.raw.clone());
    let status = coolify_jobs::databases::normalize_status(d.str("status").unwrap_or_default());
    r.insert("status".into(), Value::String(status));
    r.insert(
        "database_type".into(),
        Value::String(d.kind.type_name().into()),
    );
    r.insert(
        "server_status".into(),
        Value::Bool(server.as_ref().is_some_and(server_functional)),
    );
    if ctx.can_read_sensitive && d.kind != Kind::Sqlite {
        // `makeVisible([...])`: the hidden password columns, `init_scripts` and the URLs.
        let c = credentials(state, d).await?;
        for column in d.kind.encrypted_columns() {
            r.insert((*column).into(), Value::String(c.secret(column)));
        }
        if d.kind == Kind::Postgresql {
            r.insert(
                "init_scripts".into(),
                d.raw.get("init_scripts").cloned().unwrap_or(Value::Null),
            );
        }
        // A locked password variable is never shown again, also not inside the URLs.
        let (internal, external) = if c.locked {
            (Value::Null, Value::Null)
        } else {
            (
                Value::String(conn::internal_url(d, &c)),
                conn::external_url(d, &c, server_ip(server.as_ref()))
                    .map_or(Value::Null, Value::String),
            )
        };
        r.insert("internal_db_url".into(), internal);
        r.insert("external_db_url".into(), external);
    }
    if ctx.can_read_sensitive {
        // `makeVisible([...$database::CONFIGURATION_ATTRIBUTES])`.
        for column in configuration_attributes(d.kind) {
            if let Some(v) = d.raw.get(*column) {
                r.insert((*column).into(), v.clone());
            }
        }
    }
    if let Some(dest) = &d.destination {
        let mut dest = dest.clone();
        if let Some(s) = &server {
            let nested = if model {
                nested_server_with(state, s, false, ctx.can_read_sensitive)?
            } else {
                nested_server(state, ctx, s)?
            };
            dest.insert("server".into(), nested);
        }
        r.insert("destination".into(), Value::Object(dest));
    }
    if with_environment {
        let mut project = d.project.clone();
        project.insert("team".into(), Value::Object(d.team.clone()));
        let mut env = d.environment.clone();
        env.insert("project".into(), Value::Object(project));
        r.insert("environment".into(), Value::Object(env));
    }
    if let Some(b) = backup_configs {
        r.insert("backup_configs".into(), b);
    }
    if model {
        r.shift_remove("laravel_through_key");
        if !ctx.can_read_sensitive {
            return Ok(Value::Object(r));
        }
        // `makeVisible($model->getHidden())`: every hidden column, in the order of the table.
        let mut out = Record::new();
        for (k, raw) in &d.raw {
            if k == "laravel_through_key" {
                continue;
            }
            let v = r.shift_remove(k).unwrap_or_else(|| raw.clone());
            out.insert(k.clone(), v);
        }
        for k in ["internal_db_url", "external_db_url"] {
            if let Some(v) = r.shift_remove(k) {
                out.insert(k.into(), v);
            }
        }
        out.extend(r);
        return Ok(Value::Object(out));
    }
    for hidden in ["id", "laravel_through_key"] {
        r.shift_remove(hidden);
    }
    Ok(Value::Object(serialize_record(r)))
}

/// `GET /databases`.
pub async fn list(State(state): State<AppState>, Extension(ctx): Extension<AuthCtx>) -> ApiResult {
    let rows = dbs::list_for_team(&state.db, ctx.team_id).await?;
    let mut items = Vec::with_capacity(rows.len());
    for d in &rows {
        let configs =
            crate::routes::database_backups::configs_with_latest_log(&state, &ctx, d).await?;
        items.push(present(&state, &ctx, d, false, Some(configs)).await?);
    }
    Ok(ok(Value::Array(items)).into_response())
}

/// `GET /databases/{uuid}`.
pub async fn show(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let d = dbs::find_in_team(&state.db, ctx.team_id, &uuid)
        .await?
        .ok_or(NOT_FOUND)?;
    // `authorize('view')`: the query is team-scoped.
    Ok(ok(present(&state, &ctx, &d, true, None).await?).into_response())
}

const ALLOWED_ANY: &[&str] = &[
    "name",
    "description",
    "image",
    "public_port",
    "public_port_timeout",
    "is_public",
    "project_uuid",
    "environment_name",
    "environment_uuid",
    "server_uuid",
    "destination_uuid",
    "instant_deploy",
    "limits_memory",
    "limits_memory_swap",
    "limits_memory_swappiness",
    "limits_memory_reservation",
    "limits_cpus",
    "limits_cpuset",
    "limits_cpu_shares",
    "postgres_user",
    "postgres_password",
    "postgres_db",
    "postgres_initdb_args",
    "postgres_host_auth_method",
    "postgres_conf",
    "clickhouse_admin_user",
    "clickhouse_admin_password",
    "dragonfly_password",
    "redis_password",
    "redis_conf",
    "keydb_password",
    "keydb_conf",
    "mariadb_conf",
    "mariadb_root_password",
    "mariadb_user",
    "mariadb_password",
    "mariadb_database",
    "mongo_conf",
    "mongo_initdb_root_username",
    "mongo_initdb_root_password",
    "mongo_initdb_database",
    "mysql_root_password",
    "mysql_password",
    "mysql_user",
    "mysql_database",
    "mysql_conf",
    "sqlite_databases",
    "tags",
];
/// The fields every `create_database` type allows.
const ALLOWED_COMMON: &[&str] = &[
    "name",
    "description",
    "image",
    "public_port",
    "public_port_timeout",
    "is_public",
    "project_uuid",
    "environment_name",
    "environment_uuid",
    "server_uuid",
    "destination_uuid",
    "instant_deploy",
    "limits_memory",
    "limits_memory_swap",
    "limits_memory_swappiness",
    "limits_memory_reservation",
    "limits_cpus",
    "limits_cpuset",
    "limits_cpu_shares",
];

/// The type's own fields of `create_database` (`$allowedFields` after the common ones,
/// before `tags`), with their rules.
pub(crate) fn kind_fields(kind: Kind) -> Vec<Field> {
    let id = |name| Field {
        name,
        rules: vec![
            Rule::Nullable,
            Rule::String,
            Rule::Min(1),
            Rule::Max(63),
            Rule::Regex(&DB_IDENTIFIER),
        ],
    };
    let pw = |name| Field {
        name,
        rules: vec![
            Rule::Nullable,
            Rule::String,
            Rule::Min(1),
            Rule::Max(128),
            Rule::Regex(&DB_PASSWORD),
        ],
    };
    let string = |name| Field {
        name,
        rules: vec![Rule::String],
    };
    match kind {
        Kind::Postgresql => vec![
            id("postgres_user"),
            pw("postgres_password"),
            id("postgres_db"),
            string("postgres_initdb_args"),
            string("postgres_host_auth_method"),
            string("postgres_conf"),
        ],
        Kind::Redis => vec![pw("redis_password"), string("redis_conf")],
        Kind::Dragonfly => vec![pw("dragonfly_password")],
        Kind::Keydb => vec![pw("keydb_password"), string("keydb_conf")],
        Kind::Clickhouse => vec![id("clickhouse_admin_user"), pw("clickhouse_admin_password")],
        Kind::Mongodb => vec![
            string("mongo_conf"),
            id("mongo_initdb_root_username"),
            pw("mongo_initdb_root_password"),
            id("mongo_initdb_database"),
        ],
        Kind::Mysql => vec![
            pw("mysql_root_password"),
            pw("mysql_password"),
            id("mysql_user"),
            id("mysql_database"),
            string("mysql_conf"),
        ],
        Kind::Mariadb => vec![
            string("mariadb_conf"),
            pw("mariadb_root_password"),
            id("mariadb_user"),
            pw("mariadb_password"),
            id("mariadb_database"),
        ],
        Kind::Sqlite => vec![Field {
            name: "sqlite_databases",
            rules: vec![Rule::String, Rule::Max(255), Rule::Regex(&SQLITE_DATABASES)],
        }],
    }
}

/// `isPublicPortAlreadyUsed` with `$id`.
pub(crate) async fn public_port_used_except(
    db: &coolify_db::Db,
    server_id: i64,
    port: i64,
    id: i64,
) -> Result<bool, ApiError> {
    Ok(dbs::list_for_server(db, server_id)
        .await?
        .iter()
        .any(|d| d.bool("is_public") && d.int("public_port") == Some(port) && d.id != id))
}

/// The base64 configuration field of a type (`isBase64Encoded` + UTF-8 check).
pub(crate) fn conf_field(kind: Kind) -> Option<&'static str> {
    match kind {
        Kind::Postgresql => Some("postgres_conf"),
        Kind::Redis => Some("redis_conf"),
        Kind::Keydb => Some("keydb_conf"),
        Kind::Mongodb => Some("mongo_conf"),
        Kind::Mysql => Some("mysql_conf"),
        Kind::Mariadb => Some("mariadb_conf"),
        Kind::Dragonfly | Kind::Clickhouse | Kind::Sqlite => None,
    }
}

/// Integer columns among the fillable ones.
pub(crate) const INT_COLUMNS: &[&str] = &[
    "public_port",
    "public_port_timeout",
    "limits_memory_swappiness",
    "limits_cpu_shares",
];

/// `StandaloneSqlite::DATABASES_PATTERN`.
static SQLITE_DATABASES: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"^[A-Za-z0-9][A-Za-z0-9._-]*(,[A-Za-z0-9][A-Za-z0-9._-]*)*\n?\z")
        .unwrap_or_else(|_| unreachable!())
});
static DB_IDENTIFIER: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"^[A-Za-z_][A-Za-z0-9_]{0,62}\n?\z").unwrap_or_else(|_| unreachable!())
});
static DB_PASSWORD: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"^[A-Za-z0-9!@#%^*()_+\-=\[\]{}:,.?/~]+\n?\z")
        .unwrap_or_else(|_| unreachable!())
});

pub(crate) fn errors_with(field: &str, msg: &str) -> ApiError {
    let mut m = Map::new();
    m.insert(field.into(), Value::String(msg.into()));
    ApiError::Validation(m)
}

/// PHP `$request->boolean($key)`.
pub(crate) fn input_bool(input: &Input, key: &str) -> bool {
    match input.get(key) {
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_i64() == Some(1),
        Some(Value::String(s)) => {
            matches!(s.to_ascii_lowercase().as_str(), "1" | "true" | "on" | "yes")
        }
        _ => false,
    }
}

/// `Str::password(64, symbols: false)`: letters and digits, at least one of each.
pub(crate) fn random_password() -> String {
    const LETTERS: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";
    const DIGITS: &[u8] = b"0123456789";
    let mut rng = rand::thread_rng();
    let mut chars: Vec<u8> = vec![
        LETTERS[rng.gen_range(0..LETTERS.len())],
        DIGITS[rng.gen_range(0..DIGITS.len())],
    ];
    let all: Vec<u8> = [LETTERS, DIGITS].concat();
    while chars.len() < 64 {
        chars.push(all[rng.gen_range(0..all.len())]);
    }
    for i in (1..chars.len()).rev() {
        let j = rng.gen_range(0..=i);
        chars.swap(i, j);
    }
    String::from_utf8(chars).unwrap_or_default()
}

/// `isBase64Encoded` + `base64_decode` + UTF-8 check.
pub(crate) fn decode_conf(v: &Value) -> Option<String> {
    use base64::Engine as _;
    let s = v.as_str()?;
    let bytes = base64::engine::general_purpose::STANDARD.decode(s).ok()?;
    (base64::engine::general_purpose::STANDARD.encode(&bytes) == s).then_some(())?;
    String::from_utf8(bytes).ok()
}

macro_rules! create_handlers {
    ($($name:ident => $kind:expr),* $(,)?) => {$(
        /// `POST /databases/{type}` (`create_database`).
        pub async fn $name(
            State(state): State<AppState>,
            Extension(ctx): Extension<AuthCtx>,
            headers: HeaderMap,
            uri: Uri,
            body: Bytes,
        ) -> ApiResult {
            create_database(state, ctx, headers, uri, body, $kind).await
        }
    )*};
}

create_handlers! {
    create_postgresql => Kind::Postgresql,
    create_mysql => Kind::Mysql,
    create_mariadb => Kind::Mariadb,
    create_mongodb => Kind::Mongodb,
    create_redis => Kind::Redis,
    create_clickhouse => Kind::Clickhouse,
    create_dragonfly => Kind::Dragonfly,
    create_keydb => Kind::Keydb,
    create_sqlite => Kind::Sqlite,
}

async fn create_database(
    state: AppState,
    ctx: AuthCtx,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
    kind: Kind,
) -> ApiResult {
    use coolify_domain::Ability;
    let raw_input = lenient_json(&headers, &uri, &body);
    let instant = input_bool(&raw_input, "instant_deploy");
    if instant && !(ctx.abilities.can(Ability::Deploy) || ctx.abilities.can(Ability::Root)) {
        return Err(ApiError::Abort(
            StatusCode::FORBIDDEN,
            "Missing required permissions: deploy",
        ));
    }
    let mut input = validated_json(&headers, &uri, &body)?;
    let extra: Vec<&String> = input
        .keys()
        .filter(|k| !ALLOWED_ANY.contains(&k.as_str()))
        .collect();
    if !extra.is_empty() {
        // Upstream builds these errors with `Collection::add($field, $message)`, which
        // keeps only the field name: the errors come out as a list of names.
        let names: Vec<Value> = extra
            .into_iter()
            .map(|k| Value::String(k.clone()))
            .collect();
        let body = json!({"message": "Validation failed.", "errors": names});
        return Ok(
            crate::response::PhpJson(StatusCode::UNPROCESSABLE_ENTITY, body).into_response(),
        );
    }
    let env_uuid = input
        .get("environment_uuid")
        .filter(|v| php_truthy(v))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let env_name = input
        .get("environment_name")
        .filter(|v| php_truthy(v))
        .and_then(Value::as_str)
        .map(str::to_owned);
    if env_uuid.is_none() && env_name.is_none() {
        return Err(ApiError::Message(
            StatusCode::UNPROCESSABLE_ENTITY,
            "You need to provide at least one of environment_name or environment_uuid.",
        ));
    }
    let mut is_public = input.get("is_public").is_some_and(php_truthy);
    if is_public && !input.get("public_port").is_some_and(php_truthy) {
        is_public = false;
        input.0.insert("is_public".into(), Value::Bool(false));
    }
    let project_uuid = input.str("project_uuid").unwrap_or_default();
    let project = coolify_db::projects::find(&state.db, ctx.team_id, project_uuid)
        .await?
        .ok_or(ApiError::Message(
            StatusCode::NOT_FOUND,
            "Project not found.",
        ))?;
    let mut env = None;
    for wanted in [env_name.as_deref(), env_uuid.as_deref()]
        .into_iter()
        .flatten()
    {
        if env.is_none() {
            env = coolify_db::projects::find_environment(&state.db, project.id, wanted).await?;
        }
    }
    let env = env.ok_or(ApiError::Message(
        StatusCode::UNPROCESSABLE_ENTITY,
        "You need to provide a valid environment_name or environment_uuid.",
    ))?;
    // `$this->authorize('create', [StandalonePostgresql::class, $environment])`.
    if !ctx.can_create_in(Some(env.id)) {
        return Err(ApiError::Unauthorized);
    }
    let server = servers::find(
        &state.db,
        ctx.team_id,
        input.str("server_uuid").unwrap_or_default(),
    )
    .await?
    .ok_or(ApiError::Message(
        StatusCode::NOT_FOUND,
        "Server not found.",
    ))?;
    if crate::routes::servers::effective_role_is_build(&server.settings.clone().unwrap_or_default())
    {
        let mut m = Map::new();
        m.insert(
            "server_uuid".into(),
            json!([
                "The specified server is configured as a build server and cannot host resources."
            ]),
        );
        return Err(ApiError::Validation(m));
    }
    let destinations = dbs::all_destinations(&state.db, server.id).await?;
    if destinations.is_empty() {
        return Err(ApiError::Message(
            StatusCode::BAD_REQUEST,
            "Server has no destinations.",
        ));
    }
    if destinations.len() > 1 && !input.has("destination_uuid") {
        return Err(ApiError::Message(
            StatusCode::BAD_REQUEST,
            "Server has multiple destinations and you do not set destination_uuid.",
        ));
    }
    let (mut destination, mut destination_type) = (destinations[0].0, destinations[0].2);
    if destinations.len() > 1 {
        let wanted = input.str("destination_uuid").unwrap_or_default();
        (destination, destination_type) = destinations
            .iter()
            .find(|(_, u, _)| u == wanted)
            .map(|(id, _, morph)| (*id, *morph))
            .ok_or_else(|| {
                errors_with(
                    "destination_uuid",
                    "Provided destination_uuid does not belong to the specified server.",
                )
            })?;
    }
    let public_port = input
        .get("public_port")
        .and_then(|v| v.as_i64().or_else(|| v.as_str()?.trim().parse().ok()));
    if input.has("public_port")
        && is_public
        && let Some(port) = public_port
        && dbs::public_port_used(&state.db, server.id, port).await?
    {
        return Err(ApiError::Message(
            StatusCode::BAD_REQUEST,
            "Public port already used by another database.",
        ));
    }
    // normalizeNumericLimits
    for key in [
        "limits_memory",
        "limits_memory_swap",
        "limits_memory_reservation",
        "limits_cpus",
        "limits_cpuset",
    ] {
        if let Some(Value::Number(n)) = input.get(key) {
            let text = n.to_string();
            input.0.insert(key.into(), Value::String(text));
        }
    }
    let present_keys: Vec<&str> = input.keys().map(String::as_str).collect();
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
                name: "project_uuid",
                rules: vec![Rule::String, Rule::Required],
            },
            Field {
                name: "environment_name",
                rules: vec![Rule::String, Rule::Nullable],
            },
            Field {
                name: "environment_uuid",
                rules: vec![Rule::String, Rule::Nullable],
            },
            Field {
                name: "server_uuid",
                rules: vec![Rule::String, Rule::Required],
            },
            Field {
                name: "destination_uuid",
                rules: vec![Rule::String],
            },
            Field {
                name: "is_public",
                rules: vec![Rule::Boolean],
            },
            Field {
                name: "public_port",
                rules: vec![Rule::Numeric, Rule::Nullable],
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
                name: "tags",
                rules: vec![Rule::Array, Rule::Nullable],
            },
        ],
        CUSTOM_API_MESSAGES,
        &present_keys,
    )?;
    // `tags.*` => `string|min:2`.
    if let Some(Value::Array(list)) = input.get("tags") {
        let mut errors = Map::new();
        for (i, tag) in list.iter().enumerate() {
            match tag.as_str() {
                None => {
                    errors.insert(
                        format!("tags.{i}"),
                        json!([format!("The tags.{i} field must be a string.")]),
                    );
                }
                Some(s) if s.chars().count() < 2 => {
                    errors.insert(
                        format!("tags.{i}"),
                        json!([format!("The tags.{i} field must be at least 2 characters.")]),
                    );
                }
                _ => {}
            }
        }
        if !errors.is_empty() {
            return Err(ApiError::Validation(errors));
        }
    }
    // validateTagsParameter
    let tag_names: Vec<String> = match input.get("tags") {
        Some(Value::Array(list)) => crate::routes::database_more::normalize_tag_names(list),
        _ => Vec::new(),
    };
    if tag_names.iter().any(|n| n.chars().count() < 2) {
        let mut m = Map::new();
        m.insert(
            "tags".into(),
            json!(["Each tag name must be at least 2 characters after sanitization."]),
        );
        return Err(ApiError::Validation(m));
    }
    if let Some(port) = public_port.filter(|p| *p != 0)
        && !(1024..=65535).contains(&port)
    {
        return Err(errors_with(
            "public_port",
            "The public port should be between 1024 and 65535.",
        ));
    }
    let own = kind_fields(kind);
    // SQLite has no public port.
    let common: Vec<&'static str> = ALLOWED_COMMON
        .iter()
        .copied()
        .filter(|f| {
            kind != Kind::Sqlite || !["public_port", "public_port_timeout", "is_public"].contains(f)
        })
        .collect();
    let mut allowed: Vec<&str> = common.clone();
    allowed.extend(own.iter().map(|f| f.name));
    allowed.push("tags");
    validate(&input, &own, CUSTOM_API_MESSAGES, &allowed)?;
    let mut conf: Option<String> = None;
    if let Some(field) = conf_field(kind)
        && input.has(field)
    {
        conf = Some(input.get(field).and_then(decode_conf).ok_or_else(|| {
            ApiError::Validation(Map::from_iter([(
                field.to_owned(),
                Value::String(format!("The {field} should be base64 encoded.")),
            )]))
        })?);
    }

    // `$request->only($allowedFields)` minus the request-only keys, then `fill()`.
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let mut fields: Vec<(&'static str, ColumnValue)> = Vec::new();
    let mut name: Option<String> = None;
    // `create_standalone_*` sets the generated passwords before `fill()`.
    let mut secrets: Vec<(&'static str, String)> = Vec::new();
    let mut redis_password: Option<String> = None;
    let allowed_static: Vec<&'static str> = common
        .iter()
        .copied()
        .chain(own.iter().map(|f| f.name))
        .collect();
    for key in &allowed_static {
        if matches!(
            *key,
            "project_uuid"
                | "environment_name"
                | "environment_uuid"
                | "destination_uuid"
                | "server_uuid"
                | "instant_deploy"
                | "tags"
        ) || !input.has(key)
        {
            continue;
        }
        let v = input.get(key).cloned().unwrap_or(Value::Null);
        match *key {
            "name" => name = Some(strip_tags(v.as_str())),
            "description" => fields.push((
                "description",
                ColumnValue::Text(Some(strip_tags(v.as_str()))),
            )),
            // `isset($otherData['redis_password'])`: the variable, not a column.
            "redis_password" => {
                redis_password = v.as_str().map(str::to_owned);
            }
            k if kind.encrypted_columns().contains(&k) => {
                secrets.push((k, String::new()));
                fields.push((
                    k,
                    ColumnValue::Text(v.as_str().map(|p| enc.encrypt_string(p))),
                ));
            }
            k if Some(k) == conf_field(kind) => fields.push((k, ColumnValue::Text(conf.clone()))),
            "is_public" => fields.push(("is_public", ColumnValue::Bool(is_public))),
            k if INT_COLUMNS.contains(&k) => match &v {
                Value::Null => fields.push((k, ColumnValue::Text(None))),
                other => {
                    let n = other
                        .as_i64()
                        .or_else(|| other.as_str().and_then(|s| s.trim().parse().ok()));
                    fields.push((k, n.map(ColumnValue::Int).ok_or(ApiError::Internal)?));
                }
            },
            k => {
                let text = match &v {
                    Value::Null => None,
                    Value::String(s) => Some(s.clone()),
                    // normalizeNumericLimits: numbers are stored as their string.
                    other => Some(other.to_string()),
                };
                fields.push((k, ColumnValue::Text(text)));
            }
        }
    }
    if input.has("is_public") && !fields.iter().any(|(c, _)| *c == "is_public") {
        fields.push(("is_public", ColumnValue::Bool(is_public)));
    }
    let given: Vec<&str> = secrets.iter().map(|(c, _)| *c).collect();
    let (mut generated, variables) = generated_defaults(&state, kind, redis_password)?;
    generated.retain(|(c, _)| !given.contains(c));
    let secrets = generated;
    let new = NewDatabase {
        environment_id: env.id,
        destination_id: destination,
        destination_type: Some(destination_type),
        image: (kind == Kind::Postgresql).then(|| "postgres:16-alpine".to_owned()),
        secrets,
        fields,
        name,
        variables,
    };
    let (new_id, uuid) = dbs::create(&state.db, kind, new, state.now()).await?;
    let d = dbs::find_in_team(&state.db, ctx.team_id, &uuid)
        .await?
        .ok_or(ApiError::Internal)?;
    if !tag_names.is_empty() {
        coolify_db::database_parts::attach_tags(
            &state.db,
            ctx.team_id,
            kind.morph(),
            new_id,
            &tag_names,
            state.now(),
        )
        .await?;
    }
    if instant {
        start_or_queue(&state, &ctx, d.clone(), false)?;
    }
    tracing::info!(target: "audit", event = "api.database.created", team_id = ctx.team_id, database_uuid = %uuid, database_type = kind.name_prefix(), instant_deploy = instant, "database created");
    let mut payload = Map::new();
    payload.insert("uuid".into(), Value::String(uuid));
    // `createdDatabasePayload` (Dragonfly and SQLite answer with the UUID only).
    if ctx.can_read_sensitive && !matches!(kind, Kind::Dragonfly | Kind::Sqlite) {
        let c = credentials(&state, &d).await?;
        payload.insert(
            "internal_db_url".into(),
            Value::String(conn::internal_url(&d, &c)),
        );
        if d.bool("is_public") && d.int("public_port").is_some_and(|p| p != 0) {
            let external = conn::external_url(&d, &c, server_ip(Some(&server)));
            payload.insert(
                "external_db_url".into(),
                external.map_or(Value::Null, Value::String),
            );
        }
    }
    Ok(created(Value::Object(serialize_record(payload))).into_response())
}

/// `$database->destination->server` of a row whose destination is gone: upstream's error page.
pub(crate) async fn startable(state: &AppState, d: &DatabaseRow) -> Result<(), ApiError> {
    let _ = state;
    if d.destination.is_none() {
        Err(ApiError::Internal)
    } else {
        Ok(())
    }
}

/// The database branch of `deploy_resource()`: the message for the caller.
pub(crate) async fn deploy_database(
    state: &AppState,
    ctx: &AuthCtx,
    d: DatabaseRow,
) -> Result<String, ApiError> {
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    if jobs.start_prerequisite_error(&d).await.is_some() {
        return Ok(coolify_jobs::databases::ssl::MISSING_CA.to_owned());
    }
    let name = d.str("name").unwrap_or_default().to_owned();
    let uuid = d.uuid.clone();
    let (kind, id, started) = (d.kind, d.id, d.str("started_at").is_some());
    if let Err(message) = jobs.spawn_database_start(d, Some(ctx.user_id)) {
        return Ok(message.to_owned());
    }
    if !started {
        dbs::update_if_changed(
            &state.db,
            kind,
            id,
            &[("started_at", servers::ColumnValue::Time(state.now()))],
            state.now(),
        )
        .await?;
    }
    tracing::info!(target: "audit", event = "api.database.started", team_id = ctx.team_id, database_uuid = %uuid, "database started");
    Ok(format!("Database {name} started."))
}

/// `StartDatabase::prerequisiteError`.
async fn prerequisite(state: &AppState, d: &DatabaseRow) -> Result<(), ApiError> {
    if let Some(jobs) = state.jobs.as_ref()
        && jobs.start_prerequisite_error(d).await.is_some()
    {
        return Err(ApiError::Message(
            StatusCode::UNPROCESSABLE_ENTITY,
            coolify_jobs::databases::ssl::MISSING_CA,
        ));
    }
    Ok(())
}

fn start_or_queue(
    state: &AppState,
    ctx: &AuthCtx,
    d: DatabaseRow,
    restart: bool,
) -> Result<(), ApiError> {
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    let result = if restart {
        jobs.spawn_database_restart(d, Some(ctx.user_id))
    } else {
        jobs.spawn_database_start(d, Some(ctx.user_id))
    };
    result.map_err(|m| ApiError::Message(StatusCode::CONFLICT, m))
}

/// `POST /databases/{uuid}/start` (`action_deploy`).
pub async fn start(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let d = dbs::find_in_team(&state.db, ctx.team_id, &uuid)
        .await?
        .ok_or(NOT_FOUND)?;
    if !ctx.can_db(AccessLevel::Write, &d) {
        return Err(ApiError::Unauthorized);
    }
    if d.str("status").unwrap_or_default().contains("running") {
        return Err(ApiError::Message(
            StatusCode::BAD_REQUEST,
            "Database is already running.",
        ));
    }
    prerequisite(&state, &d).await?;
    startable(&state, &d).await?;
    start_or_queue(&state, &ctx, d.clone(), false)?;
    tracing::info!(target: "audit", event = "api.database.started", team_id = ctx.team_id, database_uuid = %d.uuid, "database start queued");
    Ok(ok(json!({"message": "Database starting request queued."})).into_response())
}

/// `POST /databases/{uuid}/stop` (`action_stop`).
pub async fn stop(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let d = dbs::find_in_team(&state.db, ctx.team_id, &uuid)
        .await?
        .ok_or(NOT_FOUND)?;
    if !ctx.can_db(AccessLevel::Write, &d) {
        return Err(ApiError::Unauthorized);
    }
    let status = d.str("status").unwrap_or_default();
    if status.contains("stopped") || status.contains("exited") {
        return Err(ApiError::Message(
            StatusCode::BAD_REQUEST,
            "Database is already stopped.",
        ));
    }
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    let input = lenient_json(&headers, &uri, &body);
    jobs.spawn_database_stop(d.clone(), query_bool(&uri, &input, "docker_cleanup", true));
    tracing::info!(target: "audit", event = "api.database.stopped", team_id = ctx.team_id, database_uuid = %d.uuid, "database stop queued");
    Ok(ok(json!({"message": "Database stopping request queued."})).into_response())
}

/// `POST /databases/{uuid}/restart` (`action_restart`).
pub async fn restart(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let d = dbs::find_in_team(&state.db, ctx.team_id, &uuid)
        .await?
        .ok_or(NOT_FOUND)?;
    if !ctx.can_db(AccessLevel::Write, &d) {
        return Err(ApiError::Unauthorized);
    }
    prerequisite(&state, &d).await?;
    startable(&state, &d).await?;
    start_or_queue(&state, &ctx, d.clone(), true)?;
    tracing::info!(target: "audit", event = "api.database.restarted", team_id = ctx.team_id, database_uuid = %d.uuid, "database restart queued");
    Ok(ok(json!({"message": "Database restarting request queued."})).into_response())
}

pub(crate) fn query_bool(uri: &Uri, input: &Input, key: &str, default: bool) -> bool {
    if let Some(v) = query_param(uri, key) {
        return matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "on" | "yes");
    }
    if input.has(key) {
        input_bool(input, key)
    } else {
        default
    }
}

/// `DELETE /databases/{uuid}` (`delete_by_uuid`).
pub async fn delete(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let d = dbs::find_in_team(&state.db, ctx.team_id, &uuid)
        .await?
        .ok_or(NOT_FOUND)?;
    if !ctx.can_db(AccessLevel::Delete, &d) {
        return Err(ApiError::Unauthorized);
    }
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
    let input = lenient_json(&headers, &uri, &body);
    let delete_volumes = query_bool(&uri, &input, "delete_volumes", true);
    let delete_configurations = query_bool(&uri, &input, "delete_configurations", true);
    let docker_cleanup = query_bool(&uri, &input, "docker_cleanup", true);
    dbs::soft_delete(&state.db, d.kind, d.id, state.now()).await?;
    jobs.spawn_database_delete(
        d.clone(),
        delete_volumes,
        delete_configurations,
        docker_cleanup,
    );
    tracing::info!(target: "audit", event = "api.database.deleted", team_id = ctx.team_id, database_uuid = %d.uuid, "database deletion queued");
    Ok(ok(json!({"message": "Database deletion request queued."})).into_response())
}
