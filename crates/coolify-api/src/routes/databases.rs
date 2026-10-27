//! `DatabasesController`: list, show, create (PostgreSQL), start, stop,
//! restart and delete. Other database kinds, backups, environment variables,
//! storages, tags, imports, logs and updates are not ported yet (501).
//!
//! Responses follow `removeSensitiveData` + `serializeApiResponse`: the model
//! attributes sorted, with the destination (its server and settings, loaded
//! by the `server_status` accessor) nested as the models serialize them.

use axum::Extension;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::IntoResponse;
use coolify_db::databases::{self as dbs, DatabaseRow, NewPostgresql};
use coolify_db::records::{Record, SERVER_SETTING};
use coolify_db::servers::{self, ColumnValue, ServerRow};
use coolify_domain::naming::strip_tags;
use rand::Rng as _;
use serde_json::{Map, Value, json};

use crate::auth::AuthCtx;
use crate::input::{Input, lenient_json, query_param, validated_json};
use crate::response::{ApiError, ApiResult, created, ok};
use crate::routes::servers::{ip_accessor, user_accessor};
use crate::serialize::{php_truthy, serialize_record};
use crate::state::AppState;
use crate::validation::{CUSTOM_API_MESSAGES, Field, Rule, validate};

const NOT_FOUND: ApiError = ApiError::Message(StatusCode::NOT_FOUND, "Database not found.");

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
fn nested_server(state: &AppState, ctx: &AuthCtx, s: &ServerRow) -> Result<Value, ApiError> {
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
    if !ctx.can_read_sensitive {
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
        let visible = ctx.can_read_sensitive && SETTING_SENSITIVE.contains(key);
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

/// `postgres://user:pass@host:port/db` with `rawurlencode`d credentials.
fn rawurlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn connection_urls(d: &DatabaseRow, password: &str, server: Option<&ServerRow>) -> (String, Value) {
    let user = rawurlencode(d.str("postgres_user").unwrap_or_default());
    let pass = rawurlencode(password);
    let db = d.str("postgres_db").unwrap_or_default();
    let internal = format!("postgres://{user}:{pass}@{}:5432/{db}", d.uuid);
    let public_port = d.int("public_port").filter(|_| d.bool("is_public"));
    let external = match (
        public_port,
        server
            .and_then(|s| s.record.get("ip"))
            .and_then(Value::as_str),
    ) {
        (Some(port), Some(ip)) if !ip.is_empty() => {
            let host = if ip.contains(':') {
                format!("[{ip}]")
            } else {
                ip.to_owned()
            };
            Value::String(format!("postgres://{user}:{pass}@{host}:{port}/{db}"))
        }
        _ => Value::Null,
    };
    (internal, external)
}

/// `isFunctional()` of the destination's server.
fn server_functional(s: &ServerRow) -> bool {
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

/// `removeSensitiveData($database)` for a PostgreSQL database.
pub(crate) async fn present(
    state: &AppState,
    ctx: &AuthCtx,
    d: &DatabaseRow,
    with_environment: bool,
    backup_configs: Option<Value>,
) -> Result<Value, ApiError> {
    if !d.kind.ported() {
        return Err(ApiError::NotPorted);
    }
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
    if ctx.can_read_sensitive {
        let password = d
            .str("postgres_password")
            .map(|c| decrypt(state, c))
            .transpose()?
            .unwrap_or_default();
        let (internal, external) = connection_urls(d, &password, server.as_ref());
        r.insert("postgres_password".into(), Value::String(password));
        r.insert(
            "init_scripts".into(),
            d.raw.get("init_scripts").cloned().unwrap_or(Value::Null),
        );
        r.insert("internal_db_url".into(), Value::String(internal));
        r.insert("external_db_url".into(), external);
    }
    if let Some(dest) = &d.destination {
        let mut dest = dest.clone();
        if let Some(s) = &server {
            dest.insert("server".into(), nested_server(state, ctx, s)?);
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
        // Backup configurations are not ported: only databases without them are listed faithfully.
        if !dbs::unported_parts(&state.db, d.kind, d.id)
            .await?
            .iter()
            .all(|p| *p != "scheduled backups")
        {
            return Err(ApiError::NotPorted);
        }
        items.push(present(&state, &ctx, d, false, Some(json!([]))).await?);
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
const ALLOWED_POSTGRESQL: &[&str] = &[
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
    "tags",
];
/// Integer columns among the fillable ones.
const INT_COLUMNS: &[&str] = &[
    "public_port",
    "public_port_timeout",
    "limits_memory_swappiness",
    "limits_cpu_shares",
];

static DB_IDENTIFIER: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"^[A-Za-z_][A-Za-z0-9_]{0,62}\n?\z").unwrap_or_else(|_| unreachable!())
});
static DB_PASSWORD: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"^[A-Za-z0-9!@#%^*()_+\-=\[\]{}:,.?/~]+\n?\z")
        .unwrap_or_else(|_| unreachable!())
});

fn errors_with(field: &str, msg: &str) -> ApiError {
    let mut m = Map::new();
    m.insert(field.into(), Value::String(msg.into()));
    ApiError::Validation(m)
}

/// PHP `$request->boolean($key)`.
fn input_bool(input: &Input, key: &str) -> bool {
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
fn decode_conf(v: &Value) -> Option<String> {
    use base64::Engine as _;
    let s = v.as_str()?;
    let bytes = base64::engine::general_purpose::STANDARD.decode(s).ok()?;
    (base64::engine::general_purpose::STANDARD.encode(&bytes) == s).then_some(())?;
    String::from_utf8(bytes).ok()
}

/// `POST /databases/postgresql` (`create_database`).
pub async fn create_postgresql(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    use coolify_domain::Ability;
    // `authorize('create', StandalonePostgresql::class)`: admin of the current team.
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
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
    if dbs::has_swarm_destinations(&state.db, server.id).await? {
        return Err(ApiError::NotPorted);
    }
    let destinations = dbs::destinations(&state.db, server.id).await?;
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
    let mut destination = destinations[0].0;
    if destinations.len() > 1 {
        let wanted = input.str("destination_uuid").unwrap_or_default();
        destination = destinations
            .iter()
            .find(|(_, u)| u == wanted)
            .map(|(id, _)| *id)
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
    // Tags need the tag models (not ported).
    if input
        .get("tags")
        .is_some_and(|t| t.as_array().is_some_and(|a| !a.is_empty()))
    {
        return Err(ApiError::NotPorted);
    }
    if let Some(port) = public_port.filter(|p| *p != 0)
        && !(1024..=65535).contains(&port)
    {
        return Err(errors_with(
            "public_port",
            "The public port should be between 1024 and 65535.",
        ));
    }
    let id_rules = || {
        vec![
            Rule::Nullable,
            Rule::String,
            Rule::Min(1),
            Rule::Max(63),
            Rule::Regex(&DB_IDENTIFIER),
        ]
    };
    let pw_rules = || {
        vec![
            Rule::Nullable,
            Rule::String,
            Rule::Min(1),
            Rule::Max(128),
            Rule::Regex(&DB_PASSWORD),
        ]
    };
    let allowed_pg: Vec<&str> = ALLOWED_POSTGRESQL.to_vec();
    validate(
        &input,
        &[
            Field {
                name: "postgres_user",
                rules: id_rules(),
            },
            Field {
                name: "postgres_password",
                rules: pw_rules(),
            },
            Field {
                name: "postgres_db",
                rules: id_rules(),
            },
            Field {
                name: "postgres_initdb_args",
                rules: vec![Rule::String],
            },
            Field {
                name: "postgres_host_auth_method",
                rules: vec![Rule::String],
            },
            Field {
                name: "postgres_conf",
                rules: vec![Rule::String],
            },
        ],
        CUSTOM_API_MESSAGES,
        &allowed_pg,
    )?;
    let mut conf: Option<String> = None;
    if input.has("postgres_conf") {
        conf = Some(
            input
                .get("postgres_conf")
                .and_then(decode_conf)
                .ok_or_else(|| {
                    errors_with(
                        "postgres_conf",
                        "The postgres_conf should be base64 encoded.",
                    )
                })?,
        );
    }

    // `$request->only($allowedFields)` minus the request-only keys, then `fill()`.
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let mut fields: Vec<(&'static str, ColumnValue)> = Vec::new();
    let mut name: Option<String> = None;
    let mut password = random_password();
    for key in ALLOWED_POSTGRESQL {
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
            "postgres_password" => password = v.as_str().unwrap_or_default().to_owned(),
            "postgres_conf" => fields.push(("postgres_conf", ColumnValue::Text(conf.clone()))),
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
    let new = NewPostgresql {
        environment_id: env.id,
        destination_id: destination,
        image: "postgres:16-alpine".into(),
        password_encrypted: enc.encrypt_string(&password),
        fields,
        name,
    };
    let (_, uuid) = dbs::create_postgresql(&state.db, new, state.now()).await?;
    let d = dbs::find_in_team(&state.db, ctx.team_id, &uuid)
        .await?
        .ok_or(ApiError::Internal)?;
    if instant {
        start_or_queue(&state, &ctx, d.clone(), false)?;
    }
    tracing::info!(target: "audit", event = "api.database.created", team_id = ctx.team_id, database_uuid = %uuid, instant_deploy = instant, "database created");
    let mut payload = Map::new();
    payload.insert("uuid".into(), Value::String(uuid));
    if ctx.can_read_sensitive {
        let (internal, external) = connection_urls(&d, &password, Some(&server));
        payload.insert("internal_db_url".into(), Value::String(internal));
        if is_public && public_port.is_some() {
            payload.insert("external_db_url".into(), external);
        }
    }
    Ok(created(Value::Object(serialize_record(payload))).into_response())
}

/// What a start would need that coolify-rs cannot do yet.
pub(crate) async fn startable(state: &AppState, d: &DatabaseRow) -> Result<(), ApiError> {
    let unsupported = !d.kind.ported()
        || d.bool("enable_ssl")
        || (d.bool("is_public") && d.int("public_port").is_some())
        || d.str("custom_docker_run_options")
            .is_some_and(|o| !o.trim().is_empty())
        || d.destination.is_none()
        || !dbs::unported_parts(&state.db, d.kind, d.id)
            .await?
            .iter()
            .all(|p| *p == "scheduled backups");
    if unsupported {
        Err(ApiError::NotPorted)
    } else {
        Ok(())
    }
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
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    if d.str("status").unwrap_or_default().contains("running") {
        return Err(ApiError::Message(
            StatusCode::BAD_REQUEST,
            "Database is already running.",
        ));
    }
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
) -> ApiResult {
    let d = dbs::find_in_team(&state.db, ctx.team_id, &uuid)
        .await?
        .ok_or(NOT_FOUND)?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let status = d.str("status").unwrap_or_default();
    if status.contains("stopped") || status.contains("exited") {
        return Err(ApiError::Message(
            StatusCode::BAD_REQUEST,
            "Database is already stopped.",
        ));
    }
    if !d.kind.ported() || d.bool("is_public") {
        return Err(ApiError::NotPorted);
    }
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    jobs.spawn_database_stop(d.clone());
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
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    startable(&state, &d).await?;
    start_or_queue(&state, &ctx, d.clone(), true)?;
    tracing::info!(target: "audit", event = "api.database.restarted", team_id = ctx.team_id, database_uuid = %d.uuid, "database restart queued");
    Ok(ok(json!({"message": "Database restarting request queued."})).into_response())
}

fn query_bool(uri: &Uri, input: &Input, key: &str, default: bool) -> bool {
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
    let input = lenient_json(&headers, &uri, &body);
    let delete_volumes = query_bool(&uri, &input, "delete_volumes", true);
    let delete_configurations = query_bool(&uri, &input, "delete_configurations", true);
    dbs::soft_delete(&state.db, d.kind, d.id, state.now()).await?;
    jobs.spawn_database_delete(d.clone(), delete_volumes, delete_configurations);
    tracing::info!(target: "audit", event = "api.database.deleted", team_id = ctx.team_id, database_uuid = %d.uuid, "database deletion queued");
    Ok(ok(json!({"message": "Database deletion request queued."})).into_response())
}
