//! `ServersController` (read side): `GET /servers`, `GET /servers/{uuid}`.
//!
//! Upstream builds these responses from the `Server` and `ServerSetting`
//! models, so the presenter reproduces their accessors and casts:
//! * `Server`: `ip`, `user` and `port` accessors; `proxy` is a schemaless
//!   attribute that the `retrieved` hook gives `redirect_enabled: true`;
//!   `id` and `proxy_error_page` hidden; `is_coolify_host` appended;
//! * `ServerSetting`: `docker_cleanup_frequency` accessor; secrets hidden
//!   unless the token can read sensitive data (then `sentinel_token` and
//!   `logdrain_custom_config` are decrypted); `is_build_server` replaced by
//!   the effective role after serialization.
//!
//! Without `can_read_sensitive`, `removeServerProxySecrets` drops
//! `proxy.last_saved_proxy_configuration` and `validation_logs`.
//!
//! `POST /servers` writes the rows upstream's model hooks write (settings with
//! a Sentinel token and URL, the default `coolify` Docker destination, the two
//! server shared variables) without connecting to the server; asking for
//! `instant_validate` answers 501 until remote execution is ported.

use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::sync::LazyLock;

use axum::Extension;
use axum::body::Bytes;
use axum::extract::{ConnectInfo, Path, State};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::IntoResponse;
use coolify_db::records::Record;
use coolify_db::servers::{self, NewServer, ServerRow};
use coolify_domain::naming::strip_tags;
use coolify_domain::new_public_id;
use regex::Regex;
use serde_json::{Map, Value, json};

use crate::auth::AuthCtx;
use crate::input::{Input, query_param, validated_json};
use crate::response::{ApiError, ApiResult, created, ok};
use crate::serialize::{php_truthy, serialize_record};
use crate::state::AppState;
use crate::validation::{Field, Rule, validate};

const NOT_FOUND: ApiError = ApiError::Message(StatusCode::NOT_FOUND, "Server not found.");

/// `ServerSetting::$hidden` and the attributes the controller makes visible
/// again for tokens with `can_read_sensitive` (`geoip_maxmind_license_key`
/// and `is_build_server` stay hidden).
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
/// `encrypted` casts among the visible-for-sensitive settings.
const SETTING_ENCRYPTED: &[&str] = &["sentinel_token", "logdrain_custom_config"];

use coolify_compat::cron::translate_cron_expression as translate_cron;

/// `Server::ip()` getter: `preg_replace('/[^0-9a-zA-Z.:%-]/', '', $value)`.
fn ip_accessor(v: &str) -> String {
    v.chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | ':' | '%' | '-'))
        .collect()
}

/// `Server::user()` getter: `INVALID_SERVER_USERNAME_CHARACTERS_PATTERN` removed.
fn user_accessor(v: &str) -> String {
    v.chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        .collect()
}

/// `Server::port()` getter: `(int) preg_replace('/[^0-9]/', '', $value)`.
fn port_accessor(v: &Value) -> Value {
    let digits: String = match v {
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        _ => String::new(),
    }
    .chars()
    .filter(char::is_ascii_digit)
    .collect();
    // PHP's (int) cast saturates; ports never get near it.
    Value::from(digits.parse::<i64>().unwrap_or(0))
}

/// The schemaless `proxy` attribute after the `retrieved` hook.
fn proxy_attribute(raw: Option<&Value>) -> Value {
    let mut proxy = match raw {
        Some(Value::Object(m)) => m.clone(),
        _ => Map::new(),
    };
    if !proxy.contains_key("redirect_enabled") {
        proxy.insert("redirect_enabled".into(), Value::Bool(true));
    }
    Value::Object(proxy)
}

/// Accessors on the attributes present in `r` (the list selects only some).
fn apply_server_accessors(r: &mut Record) {
    if let Some(Value::String(ip)) = r.get("ip") {
        let v = ip_accessor(ip);
        r.insert("ip".into(), Value::String(v));
    }
    if let Some(Value::String(user)) = r.get("user") {
        let v = user_accessor(user);
        r.insert("user".into(), Value::String(v));
    }
    if let Some(port) = r.get("port") {
        let v = if port.is_null() {
            Value::from(0)
        } else {
            port_accessor(port)
        };
        r.insert("port".into(), v);
    }
}

fn effective_role_is_build(settings: &Record) -> bool {
    match settings.get("server_role").and_then(Value::as_str) {
        Some(role) => role == "build",
        None => settings.get("is_build_server").is_some_and(php_truthy),
    }
}

/// `removeSensitiveDataFromSettings`.
fn present_settings(state: &AppState, ctx: &AuthCtx, raw: &Record) -> ApiResult<Value> {
    let mut s = raw.clone();
    let is_build = effective_role_is_build(&s);
    for key in SETTING_HIDDEN {
        let visible = ctx.can_read_sensitive && SETTING_SENSITIVE.contains(key);
        if !visible {
            s.shift_remove(*key);
        } else if SETTING_ENCRYPTED.contains(key)
            && let Some(Value::String(cipher)) = s.get(*key)
        {
            let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
            let plain = enc.decrypt_string(cipher).map_err(|e| {
                tracing::error!(field = %key, error = %e, "cannot decrypt server setting (APP_KEY mismatch?)");
                ApiError::Internal
            })?;
            s.insert((*key).into(), Value::String(plain));
        }
    }
    if let Some(Value::String(f)) = s.get("docker_cleanup_frequency") {
        let v = translate_cron(f).to_string();
        s.insert("docker_cleanup_frequency".into(), Value::String(v));
    }
    let mut out = serialize_record(s);
    out.insert("is_build_server".into(), Value::Bool(is_build));
    Ok(Value::Object(out))
}

/// `removeServerProxySecrets` for one serialized server.
fn strip_proxy_secrets(ctx: &AuthCtx, r: &mut Record) {
    if ctx.can_read_sensitive {
        return;
    }
    if let Some(Value::Object(proxy)) = r.get_mut("proxy") {
        proxy.shift_remove("last_saved_proxy_configuration");
        r.shift_remove("validation_logs");
    }
}

/// `GET /servers` item: the selected columns plus `is_reachable`/`is_usable`.
fn present_list_item(state: &AppState, ctx: &AuthCtx, s: &ServerRow) -> ApiResult<Value> {
    let mut r = Record::new();
    for key in ["id", "name", "uuid", "ip", "user", "port", "description"] {
        r.insert(
            key.into(),
            s.record.get(key).cloned().unwrap_or(Value::Null),
        );
    }
    let settings = s.settings.clone().unwrap_or_default();
    let flag = |k: &str| settings.get(k).cloned().unwrap_or(Value::Null);
    r.insert("is_reachable".into(), flag("is_reachable"));
    r.insert("is_usable".into(), flag("is_usable"));
    // Not selected, so the schemaless attribute starts empty.
    r.insert("proxy".into(), proxy_attribute(None));
    r.insert("is_coolify_host".into(), Value::Bool(s.id == 0));
    r.insert("settings".into(), present_settings(state, ctx, &settings)?);
    apply_server_accessors(&mut r);
    r.shift_remove("id");
    strip_proxy_secrets(ctx, &mut r);
    Ok(Value::Object(serialize_record(r)))
}

pub async fn list(State(state): State<AppState>, Extension(ctx): Extension<AuthCtx>) -> ApiResult {
    let rows = servers::list(&state.db, ctx.team_id).await?;
    let items = rows
        .iter()
        .map(|s| present_list_item(&state, &ctx, s))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ok(Value::Array(items)).into_response())
}

pub async fn show(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    uri: Uri,
) -> ApiResult {
    let resources = query_param(&uri, "resources").is_some_and(|v| php_truthy(&Value::String(v)));
    let s = servers::find(&state.db, ctx.team_id, &uuid)
        .await?
        .ok_or(NOT_FOUND)?;
    // `view`: member of the server's team (the query is already team-scoped).
    if resources {
        // Listing resources needs the application/database/service models (not ported yet).
        return Err(ApiError::NotPorted);
    }
    let mut r = s.record.clone();
    r.insert("proxy".into(), proxy_attribute(r.get("proxy")));
    r.insert("is_coolify_host".into(), Value::Bool(s.id == 0));
    r.insert(
        "unreachable_notification_sent".into(),
        Value::Bool(servers::unreachable_notification_sent(&state.db, s.id).await?),
    );
    let settings = s.settings.clone().unwrap_or_default();
    r.insert(
        "settings".into(),
        present_settings(&state, &ctx, &settings)?,
    );
    apply_server_accessors(&mut r);
    for hidden in ["id", "proxy_error_page"] {
        r.shift_remove(hidden);
    }
    strip_proxy_secrets(&ctx, &mut r);
    Ok(ok(Value::Object(serialize_record(r))).into_response())
}

/// `SERVER_NAME_CONTROL_CHARACTERS_PATTERN`.
static CONTROL_CHARS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[\x00-\x1F\x7F]").unwrap_or_else(|_| unreachable!()));
/// `SERVER_USERNAME_PATTERN` (`/^[a-zA-Z0-9._-]+$/`; PHP's `$` also matches
/// before a final newline).
static USERNAME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-zA-Z0-9._-]+\n?\z").unwrap_or_else(|_| unreachable!()));

const CREATE_FIELDS: &[&str] = &[
    "name",
    "description",
    "ip",
    "port",
    "user",
    "private_key_uuid",
    "server_role",
    "is_build_server",
    "instant_validate",
    "proxy_type",
];
/// `customApiValidator` messages plus the controller's.
const MESSAGES: crate::validation::Messages = &[
    ("required", "This field is required."),
    (
        "user.regex",
        "The User may only contain letters, numbers, dots, hyphens, and underscores.",
    ),
    (
        "name.not_regex",
        "The name must not contain control characters such as new lines.",
    ),
];

/// PHP `filter_var($v, FILTER_VALIDATE_IP, FILTER_FLAG_IPV4|IPV6)`.
fn php_ipv4(s: &str) -> bool {
    s.parse::<std::net::Ipv4Addr>().is_ok()
}
fn php_ipv6(s: &str) -> bool {
    s.parse::<Ipv6Addr>().is_ok()
}

/// `ValidHostname` (RFC 1123): only whether it passes matters here, since
/// `ValidServerIp` replaces its message.
fn valid_hostname(v: &str) -> bool {
    let h = v.trim();
    if h.len() > 253 || h.chars().any(|c| c.is_ascii_control()) {
        return false;
    }
    const DANGEROUS: &str = ";|&$`(){}<>\n\r\0\"'\\!*?[]~^:#@%=+, ";
    if h.chars().any(|c| DANGEROUS.contains(c)) {
        return false;
    }
    let h = h.to_ascii_lowercase();
    if h.starts_with('.') || h.ends_with('.') || h.contains("..") {
        return false;
    }
    h.split('.').all(|label| {
        (1..=63).contains(&label.len())
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    })
}

/// `ValidServerIp`: IPv4, IPv6 (optionally bracketed) or a hostname.
fn valid_server_ip(v: &Value) -> Option<&'static str> {
    let s = v.as_str()?;
    // `empty($value)`: "" and "0" pass.
    if s.is_empty() || s == "0" {
        return None;
    }
    let t = s.trim();
    let bracketed =
        t.starts_with('[') && t.ends_with(']') && t.len() > 2 && php_ipv6(&t[1..t.len() - 1]);
    if php_ipv4(t) || php_ipv6(t) || bracketed || valid_hostname(t) {
        None
    } else {
        Some("The :attribute must be a valid IPv4 address, IPv6 address, or hostname.")
    }
}

/// `Server::normalizeIp`: unsafe characters removed, IPv6 in short lower-case form.
pub(crate) fn normalize_ip(v: &str) -> String {
    let stripped = ip_accessor(v);
    let t = stripped.trim();
    let inner = t.trim_matches(|c| c == '[' || c == ']');
    match inner.parse::<Ipv6Addr>() {
        Ok(ip) => ip.to_string(),
        Err(_) => t.to_string(),
    }
}

fn create_fields() -> Vec<Field> {
    vec![
        Field {
            name: "name",
            rules: vec![Rule::String, Rule::Max(255), Rule::NotRegex(&CONTROL_CHARS)],
        },
        Field {
            name: "description",
            rules: vec![Rule::String, Rule::Nullable],
        },
        Field {
            name: "ip",
            rules: vec![Rule::String, Rule::Required, Rule::Custom(valid_server_ip)],
        },
        Field {
            name: "port",
            rules: vec![Rule::Integer, Rule::Nullable, Rule::Between(1, 65535)],
        },
        Field {
            name: "private_key_uuid",
            rules: vec![Rule::String, Rule::Required],
        },
        Field {
            name: "user",
            rules: vec![Rule::Nullable, Rule::String, Rule::Regex(&USERNAME)],
        },
        Field {
            name: "server_role",
            rules: vec![
                Rule::String,
                Rule::Nullable,
                Rule::In(&["deployment", "build", "both"]),
            ],
        },
        Field {
            name: "is_build_server",
            rules: vec![Rule::Boolean, Rule::Nullable],
        },
        Field {
            name: "instant_validate",
            rules: vec![Rule::Boolean, Rule::Nullable],
        },
        Field {
            name: "proxy_type",
            rules: vec![Rule::String, Rule::Nullable],
        },
    ]
}

/// Laravel `$request->boolean()`.
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

/// `$request->filled($key)`.
fn filled(input: &Input, key: &str) -> bool {
    match input.get(key) {
        None | Some(Value::Null) => false,
        Some(Value::String(s)) => !s.trim().is_empty(),
        Some(_) => true,
    }
}

fn as_php_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Bool(true) => "1".into(),
        Value::Bool(false) | Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// `legacyBuildServerRoleConflict`.
fn legacy_role_conflict(input: &Input) -> Result<(), ApiError> {
    if !filled(input, "is_build_server") || !filled(input, "server_role") {
        return Ok(());
    }
    let wants_build = input_bool(input, "is_build_server");
    let role_build = input.get("server_role").map(as_php_string).as_deref() == Some("build");
    if wants_build == role_build {
        return Ok(());
    }
    let mut errors = Map::new();
    errors.insert(
        "is_build_server".into(),
        json!([
            "is_build_server is deprecated and conflicts with server_role. Send only server_role."
        ]),
    );
    Err(ApiError::Validation(errors))
}

fn role_error(msg: &str) -> ApiError {
    let mut errors = Map::new();
    errors.insert("server_role".into(), json!([msg]));
    ApiError::Validation(errors)
}

/// `ProxyTypes` cases, matched case-insensitively.
fn proxy_type_valid(t: &str) -> bool {
    matches!(
        t.to_ascii_lowercase().as_str(),
        "none" | "traefik" | "nginx" | "caddy"
    )
}

/// `ServerSetting::generateSentinelUrl()` for a new server.
async fn sentinel_url(
    state: &AppState,
    ip: &str,
    peer: SocketAddr,
    headers: &HeaderMap,
) -> ApiResult<Option<String>> {
    if ip == "host.docker.internal" {
        return Ok(Some("http://coolify:8080".into()));
    }
    let (fqdn, v4, v6) = servers::instance_urls(&state.db).await?;
    let filled = |v: &Option<String>| v.as_deref().is_some_and(|s| !s.trim().is_empty());
    if filled(&fqdn) {
        return Ok(fqdn);
    }
    if filled(&v4) {
        return Ok(v4.map(|ip| format!("http://{ip}:8000")));
    }
    if let Some(ip6) = v6.filter(|s| !s.trim().is_empty()) {
        let n = normalize_ip(&ip6);
        let host = if php_ipv6(&n) { format!("[{n}]") } else { n };
        return Ok(Some(format!("http://{host}:8000")));
    }
    // sentinelUrlFromCurrentRequest()
    let (scheme, host, port) = state.trusted_proxies.request_origin(peer, headers);
    let bare = normalize_ip(&host);
    let local = bare == "localhost"
        || bare.ends_with(".localhost")
        || bare == "::1"
        || bare == "::"
        || bare == "0.0.0.0"
        || bare.starts_with("127.")
        || host.is_empty();
    if local {
        return Ok(None);
    }
    let default_port = if scheme == "https" { 443 } else { 80 };
    Ok(Some(match port {
        Some(p) if p != default_port => format!("{scheme}://{host}:{p}"),
        _ => format!("{scheme}://{host}"),
    }))
}

pub async fn create(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    // `authorize('create', Server::class)`: admin or owner.
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let input = validated_json(&headers, &uri, &body)?;
    validate(&input, &create_fields(), MESSAGES, CREATE_FIELDS)?;
    let name = match input.get("name") {
        Some(v) if php_truthy(v) => as_php_string(v),
        _ => crate::routes::private_keys::random_name(),
    };
    let user = match input.get("user") {
        Some(v) if php_truthy(v) => as_php_string(v),
        _ => "root".to_string(),
    };
    let port = match input.get("port") {
        None | Some(Value::Null) => 22,
        Some(v) => as_php_string(v)
            .trim()
            .parse::<f64>()
            .map(|f| f as i64)
            .unwrap_or(0),
    };
    legacy_role_conflict(&input)?;
    let role = if filled(&input, "server_role") {
        match input.get("server_role").map(as_php_string).as_deref() {
            Some("deployment") => "deployment",
            Some("build") => "build",
            _ => "both",
        }
    } else if filled(&input, "is_build_server") && input_bool(&input, "is_build_server") {
        "build"
    } else {
        "both"
    };
    if role == "deployment" && !servers::has_build_server(&state.db, ctx.team_id, None).await? {
        return Err(role_error(
            "Add a usable build server before you set this server to deployments only.",
        ));
    }
    let proxy_type = input
        .get("proxy_type")
        .filter(|v| php_truthy(v))
        .map(as_php_string);
    if let Some(t) = &proxy_type
        && !proxy_type_valid(t)
    {
        return Err(ApiError::Message(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Invalid proxy type.",
        ));
    }
    let key_uuid = input.str("private_key_uuid").unwrap_or_default();
    let key = coolify_db::private_keys::find(&state.db, ctx.team_id, key_uuid)
        .await?
        .ok_or(ApiError::Message(
            StatusCode::NOT_FOUND,
            "Private key not found.",
        ))?;
    let ip_raw = input.get("ip").map(as_php_string).unwrap_or_default();
    let ip = normalize_ip(&ip_raw);
    if servers::ip_in_use(&state.db, &ip, None).await? {
        return Err(ApiError::Message(
            StatusCode::BAD_REQUEST,
            "A server with this IP/Domain is already in use.",
        ));
    }
    if input_bool(&input, "instant_validate") {
        // ValidateServer needs SSH (remote execution is not ported yet).
        return Err(ApiError::NotPorted);
    }
    let enc = state.encrypter.as_ref().ok_or(ApiError::Message(
        StatusCode::SERVICE_UNAVAILABLE,
        "APP_KEY is not configured; servers cannot be created.",
    ))?;
    let uuid = new_public_id();
    let name = strip_tags(Some(&name));
    let description = strip_tags(input.str("description"));
    let peer = peer
        .map(|c| (c.0).0)
        .unwrap_or_else(|| SocketAddr::from((IpAddr::from([127, 0, 0, 1]), 0)));
    let token =
        enc.encrypt_string(&enc.encrypt_serialized(&json!({"server_uuid": uuid}).to_string()));
    let proxy = json!({
        "redirect_enabled": true,
        "type": proxy_type.map(|t| t.to_ascii_uppercase()).unwrap_or_else(|| "TRAEFIK".into()),
        "status": "exited",
    });
    let new = NewServer {
        sentinel_custom_url: sentinel_url(&state, &ip, peer, &headers).await?,
        sentinel_token_encrypted: token,
        shared_vars: [
            ("COOLIFY_SERVER_UUID", enc.encrypt_string(&uuid)),
            ("COOLIFY_SERVER_NAME", enc.encrypt_string(&name)),
        ],
        uuid,
        name,
        description,
        ip,
        port,
        user: user_accessor(user.trim()),
        private_key_id: key.id,
        team_id: ctx.team_id,
        proxy: proxy.to_string(),
        docker_uuid: new_public_id(),
        server_role: role,
    };
    servers::create(&state.db, &new, state.now()).await?;
    Ok(created(json!({"uuid": new.uuid})).into_response())
}

const UPDATE_FIELDS: &[&str] = &[
    "name",
    "description",
    "ip",
    "port",
    "user",
    "private_key_uuid",
    "server_role",
    "is_build_server",
    "instant_validate",
    "proxy_type",
    "concurrent_builds",
    "dynamic_timeout",
    "deployment_queue_limit",
    "server_disk_usage_notification_threshold",
    "server_disk_usage_check_frequency",
    "server_disk_usage_notification_interval_hours",
    "connection_timeout",
    "is_terminal_enabled",
    "wildcard_domain",
    "server_timezone",
    "backup_compression_cpu_percentage",
];

fn valid_timezone_rule(v: &Value) -> Option<&'static str> {
    (!coolify_compat::timezones::valid_timezone(&as_php_string(v)))
        .then_some("The server_timezone must be a valid timezone.")
}

fn update_fields() -> Vec<Field> {
    let int_min1 = |name: &'static str| Field {
        name,
        rules: vec![Rule::Integer, Rule::MinNum(1)],
    };
    vec![
        Field {
            name: "name",
            rules: vec![
                Rule::String,
                Rule::Max(255),
                Rule::Nullable,
                Rule::NotRegex(&CONTROL_CHARS),
            ],
        },
        Field {
            name: "description",
            rules: vec![Rule::String, Rule::Nullable],
        },
        Field {
            name: "ip",
            rules: vec![Rule::String, Rule::Nullable, Rule::Custom(valid_server_ip)],
        },
        Field {
            name: "port",
            rules: vec![Rule::Integer, Rule::Nullable, Rule::Between(1, 65535)],
        },
        Field {
            name: "private_key_uuid",
            rules: vec![Rule::String, Rule::Nullable],
        },
        Field {
            name: "user",
            rules: vec![Rule::Nullable, Rule::String, Rule::Regex(&USERNAME)],
        },
        Field {
            name: "server_role",
            rules: vec![
                Rule::String,
                Rule::Nullable,
                Rule::In(&["deployment", "build", "both"]),
            ],
        },
        Field {
            name: "is_build_server",
            rules: vec![Rule::Boolean, Rule::Nullable],
        },
        Field {
            name: "instant_validate",
            rules: vec![Rule::Boolean, Rule::Nullable],
        },
        Field {
            name: "proxy_type",
            rules: vec![Rule::String, Rule::Nullable],
        },
        int_min1("concurrent_builds"),
        int_min1("dynamic_timeout"),
        int_min1("deployment_queue_limit"),
        Field {
            name: "server_disk_usage_notification_threshold",
            rules: vec![Rule::Integer, Rule::MinNum(1), Rule::MaxNum(100)],
        },
        Field {
            name: "server_disk_usage_check_frequency",
            rules: vec![Rule::String],
        },
        Field {
            name: "server_disk_usage_notification_interval_hours",
            rules: vec![Rule::Integer, Rule::MinNum(1), Rule::MaxNum(720)],
        },
        Field {
            name: "connection_timeout",
            rules: vec![Rule::Integer, Rule::MinNum(1), Rule::MaxNum(300)],
        },
        Field {
            name: "is_terminal_enabled",
            rules: vec![Rule::Boolean, Rule::Nullable],
        },
        Field {
            name: "wildcard_domain",
            rules: vec![Rule::Nullable, Rule::Url],
        },
        Field {
            name: "server_timezone",
            rules: vec![Rule::String, Rule::Custom(valid_timezone_rule)],
        },
        Field {
            name: "backup_compression_cpu_percentage",
            rules: vec![Rule::Integer, Rule::In(&["25", "50", "75", "100"])],
        },
    ]
}

/// PHP `(int) preg_replace('/[^0-9]/', '', $value)`, the `port` mutator.
fn port_mutator(v: &Value) -> i64 {
    match port_accessor(v) {
        Value::Number(n) => n.as_i64().unwrap_or(0),
        _ => 0,
    }
}

fn int_value(v: &Value) -> i64 {
    match v {
        Value::Number(n) => n
            .as_i64()
            .unwrap_or_else(|| n.as_f64().map(|f| f as i64).unwrap_or(0)),
        Value::Bool(b) => i64::from(*b),
        other => php_number(other).map(|f| f as i64).unwrap_or(0),
    }
}

fn php_number(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    }
}

fn validation_error(field: &str, msg: &str) -> ApiError {
    let mut errors = Map::new();
    errors.insert(field.into(), json!([msg]));
    ApiError::Validation(errors)
}

pub async fn update(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    use coolify_db::servers::ColumnValue as C;
    let input = validated_json(&headers, &uri, &body)?;
    validate(&input, &update_fields(), MESSAGES, UPDATE_FIELDS)?;
    let s = servers::find(&state.db, ctx.team_id, &uuid)
        .await?
        .ok_or(NOT_FOUND)?;
    // `authorize('update', $server)`: admin or owner of the server's team.
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let raw = |k: &str| s.record.get(k).cloned().unwrap_or(Value::Null);
    let raw_str = |k: &str| raw(k).as_str().map(str::to_string);
    let transferred = s
        .record
        .get("server_metadata")
        .and_then(|m| m.pointer("/transfer/status"))
        .and_then(Value::as_str)
        == Some("transferred");
    if transferred {
        let mut errors = Map::new();
        let msg = json!(["This field cannot be changed on a transferred server."]);
        if filled(&input, "ip")
            && normalize_ip(&input.get("ip").map(as_php_string).unwrap_or_default())
                != normalize_ip(&raw_str("ip").unwrap_or_default())
        {
            errors.insert("ip".into(), msg.clone());
        }
        if filled(&input, "user") && input.get("user").map(as_php_string) != raw_str("user") {
            errors.insert("user".into(), msg.clone());
        }
        if filled(&input, "port") && input.get("port").map(int_value) != raw("port").as_i64() {
            errors.insert("port".into(), msg.clone());
        }
        if filled(&input, "private_key_uuid") {
            let current = coolify_db::private_keys::uuid_by_id(
                &state.db,
                raw("private_key_id").as_i64().unwrap_or(-1),
            )
            .await?;
            if input.str("private_key_uuid").map(str::to_string) != current {
                errors.insert("private_key_uuid".into(), msg.clone());
            }
        }
        if !errors.is_empty() {
            return Err(ApiError::Validation(errors));
        }
    }
    if let Some(t) = input
        .get("proxy_type")
        .filter(|v| php_truthy(v))
        .map(as_php_string)
        && !proxy_type_valid(&t)
    {
        return Err(ApiError::Message(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Invalid proxy type.",
        ));
    }

    // `$request->only([...])` through the model's mutators, then the dirty check.
    let mut changes: Vec<(&'static str, C)> = Vec::new();
    let push_if_dirty = |col: &'static str, new: C, changes: &mut Vec<(&'static str, C)>| {
        let original = match &new {
            C::Int(_) => C::Int(raw(col).as_i64().unwrap_or(i64::MIN)),
            C::Text(_) => C::Text(raw_str(col)),
            C::Bool(_) => C::Bool(raw(col).as_bool().unwrap_or(false)),
        };
        if original != new {
            changes.push((col, new));
        }
    };
    let text = |k: &str| input.get(k).map(as_php_string).unwrap_or_default();
    if input.has("name") {
        push_if_dirty(
            "name",
            C::Text(Some(strip_tags(Some(&text("name"))))),
            &mut changes,
        );
    }
    if input.has("description") {
        push_if_dirty(
            "description",
            C::Text(Some(strip_tags(Some(&text("description"))))),
            &mut changes,
        );
    }
    let mut ip_changed = false;
    if input.has("ip") {
        let before = changes.len();
        push_if_dirty("ip", C::Text(Some(normalize_ip(&text("ip")))), &mut changes);
        ip_changed = changes.len() > before;
    }
    if input.has("port") {
        let v = input.get("port").cloned().unwrap_or(Value::Null);
        push_if_dirty("port", C::Int(port_mutator(&v)), &mut changes);
    }
    if input.has("user") {
        push_if_dirty(
            "user",
            C::Text(Some(user_accessor(text("user").trim()))),
            &mut changes,
        );
    }
    if filled(&input, "private_key_uuid") {
        let key = coolify_db::private_keys::find(
            &state.db,
            ctx.team_id,
            input.str("private_key_uuid").unwrap_or_default(),
        )
        .await?
        .ok_or(ApiError::Message(
            StatusCode::NOT_FOUND,
            "Private key not found.",
        ))?;
        push_if_dirty("private_key_id", C::Int(key.id), &mut changes);
    }
    // The saving hook records the previous IP.
    if ip_changed && raw_str("ip").is_some_and(|ip| !ip.is_empty() && ip != "0") {
        changes.push(("ip_previous", C::Text(raw_str("ip"))));
    }

    if input.has("server_disk_usage_check_frequency")
        && !coolify_compat::cron::validate_cron_expression(&text(
            "server_disk_usage_check_frequency",
        ))
    {
        return Err(validation_error(
            "server_disk_usage_check_frequency",
            "Invalid Cron / Human expression for Disk Usage Check Frequency.",
        ));
    }
    legacy_role_conflict(&input)?;
    let settings = s.settings.clone().unwrap_or_default();
    let is_build_server = effective_role_is_build(&settings);
    let role: Option<&'static str> = if filled(&input, "server_role") {
        match text("server_role").as_str() {
            "deployment" => Some("deployment"),
            "build" => Some("build"),
            _ => Some("both"),
        }
    } else if filled(&input, "is_build_server") {
        if input_bool(&input, "is_build_server") {
            Some("build")
        } else if is_build_server {
            Some("both")
        } else {
            None
        }
    } else {
        None
    };
    if role == Some("build")
        && !is_build_server
        && servers::resource_count(&state.db, s.id).await? > 0
    {
        return Err(role_error(
            "A server with existing resources cannot be configured as build only.",
        ));
    }
    if role == Some("deployment") && servers::has_enabled_runners(&state.db, s.id).await? {
        return Err(role_error(
            "Disable the GitHub runners before you set this server to deployments only.",
        ));
    }
    if role == Some("deployment")
        && !servers::has_build_server(&state.db, ctx.team_id, Some(s.id)).await?
    {
        return Err(role_error(
            "Add a usable build server before you set this server to deployments only.",
        ));
    }
    let current_proxy = s
        .record
        .get("proxy")
        .and_then(|p| p.get("type"))
        .map(as_php_string)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let proxy_changed = input
        .get("proxy_type")
        .filter(|v| php_truthy(v))
        .is_some_and(|t| as_php_string(t).to_ascii_lowercase() != current_proxy);
    if proxy_changed {
        deploy_forbidden(
            &ctx,
            "Missing required permissions: deploy. Changing the proxy type starts the new proxy on the server, so it needs a token with the deploy permission.",
        )?;
    }
    // Starting the new proxy and validating the server need SSH (not ported).
    if proxy_changed || input_bool(&input, "instant_validate") {
        return Err(ApiError::NotPorted);
    }

    let now = state.now();
    servers::update_columns(&state.db, s.id, &changes, now).await?;
    if let Some(role) = role {
        servers::update_settings(
            &state.db,
            s.id,
            &[
                ("server_role", C::Text(Some(role.into()))),
                ("is_build_server", C::Bool(role == "build")),
            ],
            now,
        )
        .await?;
    }
    if input.has("is_terminal_enabled") {
        servers::update_settings(
            &state.db,
            s.id,
            &[(
                "is_terminal_enabled",
                C::Bool(input_bool(&input, "is_terminal_enabled")),
            )],
            now,
        )
        .await?;
    }
    const ADVANCED: [(&str, bool); 9] = [
        ("concurrent_builds", true),
        ("dynamic_timeout", true),
        ("deployment_queue_limit", true),
        ("server_disk_usage_notification_threshold", true),
        ("server_disk_usage_check_frequency", false),
        ("server_disk_usage_notification_interval_hours", true),
        ("connection_timeout", true),
        ("server_timezone", false),
        ("backup_compression_cpu_percentage", true),
    ];
    if ADVANCED.iter().any(|(k, _)| input.has(k)) {
        let values: Vec<(&'static str, C)> = ADVANCED
            .iter()
            .filter_map(|(k, numeric)| match input.get(k) {
                None | Some(Value::Null) => None,
                Some(v) if *numeric => Some((*k, C::Int(int_value(v)))),
                Some(v) => Some((*k, C::Text(Some(as_php_string(v))))),
            })
            .collect();
        servers::update_settings(&state.db, s.id, &values, now).await?;
    }
    if input.has("wildcard_domain") {
        let v = input
            .get("wildcard_domain")
            .filter(|v| !v.is_null())
            .map(as_php_string);
        servers::update_settings(&state.db, s.id, &[("wildcard_domain", C::Text(v))], now).await?;
    }
    Ok(created(json!({"uuid": s.uuid})).into_response())
}

/// PHP `filter_var($v, FILTER_VALIDATE_BOOLEAN)`: `Some(bool)`, or `None`
/// when the value is not a boolean string (`FILTER_NULL_ON_FAILURE`).
fn php_filter_bool(v: Option<&str>) -> Option<bool> {
    match v.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
        None | Some("0" | "false" | "off" | "no" | "") => Some(false),
        Some("1" | "true" | "on" | "yes") => Some(true),
        _ => None,
    }
}

fn deploy_forbidden(ctx: &AuthCtx, reason: &'static str) -> Result<(), ApiError> {
    use coolify_domain::Ability;
    if ctx.abilities.can(Ability::Deploy) || ctx.abilities.can(Ability::Root) {
        return Ok(());
    }
    Err(ApiError::Message(StatusCode::FORBIDDEN, reason))
}

pub async fn delete(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    uri: Uri,
) -> ApiResult {
    let s = servers::find(&state.db, ctx.team_id, &uuid)
        .await?
        .ok_or(NOT_FOUND)?;
    // `authorize('delete', $server)`: admin or owner of the server's team.
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let force = php_filter_bool(query_param(&uri, "force").as_deref()).unwrap_or(false);
    let from_provider = php_filter_bool(query_param(&uri, "delete_from_provider").as_deref())
        .ok_or(ApiError::Message(
            StatusCode::UNPROCESSABLE_ENTITY,
            "delete_from_provider must be a boolean.",
        ))?;
    let transferred = s
        .record
        .get("server_metadata")
        .and_then(|m| m.pointer("/transfer/status"))
        .and_then(Value::as_str)
        == Some("transferred");
    if transferred {
        // Deleting the transfer archive is not ported.
        return Err(ApiError::NotPorted);
    }
    let resources = servers::resource_count(&state.db, s.id).await?;
    if resources > 0 && !force {
        return Err(ApiError::Message(
            StatusCode::BAD_REQUEST,
            "Server has resources. Use ?force=true to delete all resources and the server, or delete resources manually first.",
        ));
    }
    if s.id == 0 {
        return Err(ApiError::Message(
            StatusCode::BAD_REQUEST,
            "Local server cannot be deleted.",
        ));
    }
    if from_provider {
        let linked = [
            "hetzner_server_id",
            "vultr_instance_id",
            "digitalocean_droplet_id",
        ]
        .iter()
        .any(|k| s.record.get(*k).is_some_and(php_truthy));
        if !linked {
            return Err(ApiError::Message(
                StatusCode::UNPROCESSABLE_ENTITY,
                "Server is not linked to a cloud provider.",
            ));
        }
    }
    if force || from_provider {
        deploy_forbidden(
            &ctx,
            "Missing required permissions: deploy. Deleting the resources or the cloud server needs a token with the deploy permission.",
        )?;
    }
    // Deleting resources, cloud servers, queued deployments' logs and GitHub
    // runners needs code that is not ported yet.
    if from_provider
        || resources > 0
        || servers::has_unported_delete_side_effects(&state.db, s.id).await?
    {
        return Err(ApiError::NotPorted);
    }
    servers::delete(&state.db, s.id, state.now()).await?;
    Ok(ok(json!({"message": "Server deleted."})).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accessors() {
        assert_eq!(ip_accessor("[2a01::1] "), "2a01::1");
        assert_eq!(user_accessor("ro ot$"), "root");
        assert_eq!(port_accessor(&Value::from(22)), Value::from(22));
        assert_eq!(translate_cron("@daily"), "0 0 * * *");
        assert_eq!(
            proxy_attribute(Some(&serde_json::json!({"type": "TRAEFIK"}))),
            serde_json::json!({"type": "TRAEFIK", "redirect_enabled": true})
        );
        assert_eq!(
            proxy_attribute(None),
            serde_json::json!({"redirect_enabled": true})
        );
    }
}
