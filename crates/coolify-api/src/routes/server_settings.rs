//! `ServerSentinelController`, `ServerDockerCleanupController`,
//! `ServerLogDrainsController` and `ServerCloudflareTunnelController`: the small
//! settings controllers of a server.

use std::sync::LazyLock;

use axum::Extension;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use coolify_db::records::Record;
use coolify_db::servers::{self, ColumnValue as C, ServerRow};
use regex::Regex;
use serde_json::{Map, Value, json};

use crate::auth::AuthCtx;
use crate::input::{Input, lenient_json, validated_json};
use crate::response::{ApiError, ApiResult, PhpJson, ok};
use crate::routes::servers::{as_php_string, deploy_forbidden, input_bool, int_value};
use crate::state::AppState;
use crate::validation::{CUSTOM_API_MESSAGES, Field, Rule, validate};

pub(crate) const NOT_FOUND: ApiError =
    ApiError::Message(StatusCode::NOT_FOUND, "Server not found.");

pub(crate) fn validation_errors(pairs: &[(&str, &str)]) -> ApiError {
    let mut errors = Map::new();
    for (k, v) in pairs {
        errors.insert((*k).into(), json!([*v]));
    }
    ApiError::Validation(errors)
}

/// `response()->json($v, $status)`.
pub(crate) fn json_status(status: StatusCode, v: Value) -> Response {
    PhpJson(status, v).into_response()
}

/// `Server::whereTeamId($teamId)->whereUuid($uuid)->first()` or the 404.
pub(crate) async fn find_server(
    state: &AppState,
    ctx: &AuthCtx,
    uuid: &str,
) -> Result<ServerRow, ApiError> {
    servers::find(&state.db, ctx.team_id, uuid)
        .await?
        .ok_or(NOT_FOUND)
}

/// `authorize('update', $server)`: admin or owner of the server's team.
pub(crate) fn authorize_update(ctx: &AuthCtx) -> Result<(), ApiError> {
    if ctx.is_team_admin() {
        Ok(())
    } else {
        Err(ApiError::Unauthorized)
    }
}

/// A new value of a `server_settings` column.
#[derive(Debug, Clone)]
pub(crate) enum Setting {
    Bool(bool),
    Int(i64),
    Text(Option<String>),
    /// An `encrypted` cast: compared in plain text, stored encrypted.
    Encrypted(Option<String>),
}

fn decrypted(state: &AppState, stored: Option<&Value>) -> Option<String> {
    let cipher = stored.and_then(Value::as_str)?;
    let enc = state.encrypter.as_ref()?;
    enc.decrypt_string(cipher).ok()
}

/// `$settings->save()` after attribute changes: writes the dirty columns and
/// returns their names (`wasChanged`).
pub(crate) async fn save_settings(
    state: &AppState,
    s: &ServerRow,
    values: &[(&'static str, Setting)],
) -> Result<Vec<&'static str>, ApiError> {
    let settings = s.settings.clone().unwrap_or_default();
    let mut changes: Vec<(&'static str, C)> = Vec::new();
    for (col, v) in values {
        let stored = settings.get(*col);
        let (dirty, value) = match v {
            Setting::Bool(b) => (stored.and_then(Value::as_bool) != Some(*b), C::Bool(*b)),
            Setting::Int(i) => (stored.and_then(Value::as_i64) != Some(*i), C::Int(*i)),
            Setting::Text(t) => (
                stored.and_then(Value::as_str) != t.as_deref(),
                C::Text(t.clone()),
            ),
            Setting::Encrypted(t) => {
                let dirty = decrypted(state, stored).as_deref() != t.as_deref();
                let cipher = match t {
                    Some(p) => {
                        let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
                        Some(enc.encrypt_string(p))
                    }
                    None => None,
                };
                (dirty, C::Text(cipher))
            }
        };
        if dirty {
            changes.push((*col, value));
        }
    }
    if changes.is_empty() {
        return Ok(Vec::new());
    }
    servers::update_settings(&state.db, s.id, &changes, state.now()).await?;
    Ok(changes.iter().map(|(c, _)| *c).collect())
}

/// `ServerSetting::updated`: settings Sentinel runs with restart it when it is enabled.
pub(crate) fn sentinel_settings_hook(state: &AppState, s: &ServerRow, changed: &[&'static str]) {
    const WATCHED: &[&str] = &[
        "sentinel_token",
        "sentinel_custom_url",
        "is_metrics_enabled",
        "is_sentinel_debug_enabled",
        "sentinel_metrics_refresh_rate_seconds",
        "sentinel_metrics_history_days",
        "sentinel_push_interval_seconds",
        "traffic_topn",
        "traffic_sample_threshold",
        "traffic_retention_1h_days",
        "traffic_retention_1d_days",
        "is_geoip_enabled",
        "geoip_refresh_days",
        "geoip_maxmind_license_key",
        "traffic_ip_mode",
    ];
    let enabled = s
        .settings
        .as_ref()
        .and_then(|r| r.get("is_sentinel_enabled"))
        .and_then(Value::as_bool)
        == Some(true);
    if enabled
        && changed.iter().any(|c| WATCHED.contains(c))
        && let Some(jobs) = &state.jobs
    {
        jobs.spawn_restart_sentinel(s.id);
    }
}

async fn reload(state: &AppState, s: &ServerRow) -> Result<ServerRow, ApiError> {
    servers::find_by_id(&state.db, s.id)
        .await?
        .ok_or(ApiError::Internal)
}

fn raw_settings(s: &ServerRow) -> Record {
    s.settings.clone().unwrap_or_default()
}

fn truthy(r: &Record, k: &str) -> Value {
    Value::Bool(r.get(k).is_some_and(crate::serialize::php_truthy))
}

fn int(r: &Record, k: &str) -> Value {
    Value::from(r.get(k).and_then(Value::as_i64).unwrap_or(0))
}

fn or_null(r: &Record, k: &str) -> Value {
    r.get(k).cloned().unwrap_or(Value::Null)
}

fn plain(state: &AppState, r: &Record, k: &str) -> Value {
    decrypted(state, r.get(k)).map_or(Value::Null, Value::String)
}

fn bool_setting(input: &Input, k: &str) -> Setting {
    Setting::Bool(input_bool(input, k))
}

fn int_setting(input: &Input, k: &str) -> Setting {
    Setting::Int(input.get(k).map(int_value).unwrap_or(0))
}

fn text_setting(input: &Input, k: &str) -> Setting {
    Setting::Text(input.get(k).filter(|v| !v.is_null()).map(as_php_string))
}

fn encrypted_setting(input: &Input, k: &str) -> Setting {
    Setting::Encrypted(input.get(k).filter(|v| !v.is_null()).map(as_php_string))
}

fn audit(event: &'static str, ctx: &AuthCtx, s: &ServerRow) {
    tracing::info!(
        target: "audit",
        event,
        team_id = ctx.team_id,
        server_uuid = %s.uuid,
        "server settings changed"
    );
}

// ── Sentinel ────────────────────────────────────────────────────────────

const SENTINEL_FIELDS: &[&str] = &[
    "is_metrics_enabled",
    "is_sentinel_debug_enabled",
    "sentinel_token",
    "sentinel_metrics_refresh_rate_seconds",
    "sentinel_metrics_history_days",
    "sentinel_push_interval_seconds",
    "sentinel_custom_url",
    "traffic_topn",
    "traffic_sample_threshold",
    "traffic_retention_1h_days",
    "traffic_retention_1d_days",
    "is_geoip_enabled",
    "geoip_refresh_days",
    "geoip_maxmind_license_key",
    "traffic_ip_mode",
];

/// `Server::isSentinelEnabled()`.
fn is_sentinel_enabled(s: &ServerRow) -> bool {
    let info = coolify_jobs::server::ServerInfo::from_row(s);
    !info.is_build_server()
        && !info.is_swarm()
        && info.setting_bool("force_disabled") != Some(true)
        && !info.transferred
}

fn sentinel_transform(state: &AppState, ctx: &AuthCtx, s: &ServerRow) -> Value {
    let r = raw_settings(s);
    let mut m = Map::new();
    m.insert("is_sentinel_enabled".into(), json!(is_sentinel_enabled(s)));
    m.insert(
        "is_metrics_enabled".into(),
        truthy(&r, "is_metrics_enabled"),
    );
    m.insert(
        "is_sentinel_debug_enabled".into(),
        truthy(&r, "is_sentinel_debug_enabled"),
    );
    for k in [
        "sentinel_metrics_refresh_rate_seconds",
        "sentinel_metrics_history_days",
        "sentinel_push_interval_seconds",
    ] {
        m.insert(k.into(), int(&r, k));
    }
    m.insert(
        "sentinel_updated_at".into(),
        s.record
            .get("sentinel_updated_at")
            .cloned()
            .unwrap_or(Value::Null),
    );
    for k in [
        "traffic_topn",
        "traffic_sample_threshold",
        "traffic_retention_1h_days",
        "traffic_retention_1d_days",
    ] {
        m.insert(k.into(), int(&r, k));
    }
    m.insert("is_geoip_enabled".into(), truthy(&r, "is_geoip_enabled"));
    m.insert("geoip_refresh_days".into(), int(&r, "geoip_refresh_days"));
    m.insert(
        "traffic_ip_mode".into(),
        Value::String(
            r.get("traffic_ip_mode")
                .and_then(Value::as_str)
                .unwrap_or("full")
                .to_string(),
        ),
    );
    if ctx.can_read_sensitive {
        m.insert("sentinel_token".into(), plain(state, &r, "sentinel_token"));
        m.insert(
            "sentinel_custom_url".into(),
            or_null(&r, "sentinel_custom_url"),
        );
        m.insert(
            "geoip_maxmind_license_key".into(),
            plain(state, &r, "geoip_maxmind_license_key"),
        );
    }
    Value::Object(m)
}

pub async fn sentinel_show(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    Ok(ok(sentinel_transform(&state, &ctx, &s)).into_response())
}

static SENTINEL_TOKEN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\A[a-zA-Z0-9._\-+=/]+\z").unwrap_or_else(|_| unreachable!()));

pub async fn sentinel_update(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let input = validated_json(&headers, &uri, &body)?;
    let s = find_server(&state, &ctx, &uuid).await?;
    authorize_update(&ctx)?;
    use Rule::{Boolean, In, Integer, Max, MaxNum, MinNum, Nullable, Regex as Re, String as Str};
    let fields = vec![
        Field {
            name: "is_metrics_enabled",
            rules: vec![Boolean],
        },
        Field {
            name: "is_sentinel_debug_enabled",
            rules: vec![Boolean],
        },
        Field {
            name: "sentinel_token",
            rules: vec![Str, Max(500), Re(&SENTINEL_TOKEN)],
        },
        Field {
            name: "sentinel_metrics_refresh_rate_seconds",
            rules: vec![Integer, MinNum(1)],
        },
        Field {
            name: "sentinel_metrics_history_days",
            rules: vec![Integer, MinNum(1)],
        },
        Field {
            name: "sentinel_push_interval_seconds",
            rules: vec![Integer, MinNum(10)],
        },
        Field {
            name: "sentinel_custom_url",
            rules: vec![Nullable, Rule::Url],
        },
        Field {
            name: "traffic_topn",
            rules: vec![Integer, MinNum(1), MaxNum(1000)],
        },
        Field {
            name: "traffic_sample_threshold",
            rules: vec![Integer, MinNum(0)],
        },
        Field {
            name: "traffic_retention_1h_days",
            rules: vec![Integer, MinNum(1), MaxNum(365)],
        },
        Field {
            name: "traffic_retention_1d_days",
            rules: vec![Integer, MinNum(1), MaxNum(3650)],
        },
        Field {
            name: "is_geoip_enabled",
            rules: vec![Boolean],
        },
        Field {
            name: "geoip_refresh_days",
            rules: vec![Integer, MinNum(1)],
        },
        Field {
            name: "geoip_maxmind_license_key",
            rules: vec![Nullable, Str, Max(255)],
        },
        Field {
            name: "traffic_ip_mode",
            rules: vec![Str, In(&["full", "anonymized", "off"])],
        },
    ];
    validate(&input, &fields, CUSTOM_API_MESSAGES, SENTINEL_FIELDS)?;
    let mut values = Vec::new();
    for field in SENTINEL_FIELDS {
        if !input.has(field) {
            continue;
        }
        let v = match *field {
            "is_metrics_enabled" | "is_sentinel_debug_enabled" | "is_geoip_enabled" => {
                bool_setting(&input, field)
            }
            "sentinel_token" | "geoip_maxmind_license_key" => encrypted_setting(&input, field),
            "sentinel_custom_url" | "traffic_ip_mode" => text_setting(&input, field),
            _ => int_setting(&input, field),
        };
        values.push((*field, v));
    }
    let changed = save_settings(&state, &s, &values).await?;
    sentinel_settings_hook(&state, &s, &changed);
    audit("api.server.sentinel.updated", &ctx, &s);
    let fresh = reload(&state, &s).await?;
    Ok(ok(sentinel_transform(&state, &ctx, &fresh)).into_response())
}

// ── Docker cleanup ──────────────────────────────────────────────────────

const CLEANUP_FIELDS: &[&str] = &[
    "docker_cleanup_frequency",
    "docker_cleanup_threshold",
    "force_docker_cleanup",
    "delete_unused_volumes",
    "delete_unused_networks",
    "disable_application_image_retention",
];

fn cleanup_transform(s: &ServerRow) -> Value {
    let r = raw_settings(s);
    let mut m = Map::new();
    m.insert(
        "docker_cleanup_frequency".into(),
        match r.get("docker_cleanup_frequency") {
            Some(Value::String(f)) => {
                Value::String(coolify_compat::cron::translate_cron_expression(f).to_string())
            }
            _ => Value::Null,
        },
    );
    m.insert(
        "docker_cleanup_threshold".into(),
        int(&r, "docker_cleanup_threshold"),
    );
    for k in [
        "force_docker_cleanup",
        "delete_unused_volumes",
        "delete_unused_networks",
        "disable_application_image_retention",
    ] {
        m.insert(k.into(), truthy(&r, k));
    }
    Value::Object(m)
}

pub async fn cleanup_show(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    Ok(ok(cleanup_transform(&s)).into_response())
}

pub async fn cleanup_update(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let input = validated_json(&headers, &uri, &body)?;
    let s = find_server(&state, &ctx, &uuid).await?;
    authorize_update(&ctx)?;
    use Rule::{Boolean, Integer, MaxNum, MinNum, String as Str};
    let fields = vec![
        Field {
            name: "docker_cleanup_frequency",
            rules: vec![Str],
        },
        Field {
            name: "docker_cleanup_threshold",
            rules: vec![Integer, MinNum(1), MaxNum(99)],
        },
        Field {
            name: "force_docker_cleanup",
            rules: vec![Boolean],
        },
        Field {
            name: "delete_unused_volumes",
            rules: vec![Boolean],
        },
        Field {
            name: "delete_unused_networks",
            rules: vec![Boolean],
        },
        Field {
            name: "disable_application_image_retention",
            rules: vec![Boolean],
        },
    ];
    validate(&input, &fields, CUSTOM_API_MESSAGES, CLEANUP_FIELDS)?;
    if input.has("docker_cleanup_frequency")
        && !coolify_compat::cron::validate_cron_expression(
            input.str("docker_cleanup_frequency").unwrap_or_default(),
        )
    {
        return Err(validation_errors(&[(
            "docker_cleanup_frequency",
            "Invalid Cron / Human expression for Docker Cleanup Frequency.",
        )]));
    }
    let settings = raw_settings(&s);
    if input_bool(&input, "delete_unused_volumes")
        && !settings
            .get("delete_unused_volumes")
            .is_some_and(crate::serialize::php_truthy)
    {
        deploy_forbidden(
            &ctx,
            "Missing required permissions: deploy. Deleting unused volumes removes data on the server, so it needs a token with the deploy permission.",
        )?;
    }
    let mut values = Vec::new();
    for field in CLEANUP_FIELDS {
        if !input.has(field) {
            continue;
        }
        let v = match *field {
            "docker_cleanup_frequency" => text_setting(&input, field),
            "docker_cleanup_threshold" => int_setting(&input, field),
            _ => bool_setting(&input, field),
        };
        values.push((*field, v));
    }
    save_settings(&state, &s, &values).await?;
    audit("api.server.docker_cleanup.updated", &ctx, &s);
    let fresh = reload(&state, &s).await?;
    Ok(ok(cleanup_transform(&fresh)).into_response())
}

pub async fn cleanup_run(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    authorize_update(&ctx)?;
    let input = lenient_json(&headers, &uri, &body);
    let fields = vec![
        Field {
            name: "delete_unused_volumes",
            rules: vec![Rule::Boolean],
        },
        Field {
            name: "delete_unused_networks",
            rules: vec![Rule::Boolean],
        },
    ];
    validate(
        &input,
        &fields,
        CUSTOM_API_MESSAGES,
        &["delete_unused_volumes", "delete_unused_networks"],
    )?;
    let settings = raw_settings(&s);
    let choice = |k: &str| {
        if input.has(k) {
            input_bool(&input, k)
        } else {
            settings.get(k).is_some_and(crate::serialize::php_truthy)
        }
    };
    let volumes = choice("delete_unused_volumes");
    let networks = choice("delete_unused_networks");
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    jobs.spawn_docker_cleanup(s.id, true, volumes, networks);
    audit("api.server.docker_cleanup.run", &ctx, &s);
    Ok(ok(json!({
        "message": "Manual cleanup job started. Depending on the amount of data, this might take a while."
    }))
    .into_response())
}

pub async fn cleanup_executions(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    let rows = coolify_db::server_ops::cleanup_executions(&state.db, s.id).await?;
    Ok(ok(Value::Array(rows)).into_response())
}

// ── Log drains ──────────────────────────────────────────────────────────

const DRAIN_FIELDS: &[&str] = &[
    "is_logdrain_newrelic_enabled",
    "logdrain_newrelic_license_key",
    "logdrain_newrelic_base_uri",
    "is_logdrain_axiom_enabled",
    "logdrain_axiom_dataset_name",
    "logdrain_axiom_api_key",
    "is_logdrain_custom_enabled",
    "logdrain_custom_config",
    "logdrain_custom_config_parser",
    "is_logdrain_cloudwatch_enabled",
    "logdrain_cloudwatch_region",
    "logdrain_cloudwatch_group",
    "logdrain_cloudwatch_stream_prefix",
];

fn drain_transform(state: &AppState, ctx: &AuthCtx, s: &ServerRow) -> Value {
    let r = raw_settings(s);
    let mut m = Map::new();
    m.insert(
        "is_logdrain_newrelic_enabled".into(),
        truthy(&r, "is_logdrain_newrelic_enabled"),
    );
    m.insert(
        "logdrain_newrelic_base_uri".into(),
        or_null(&r, "logdrain_newrelic_base_uri"),
    );
    m.insert(
        "is_logdrain_axiom_enabled".into(),
        truthy(&r, "is_logdrain_axiom_enabled"),
    );
    m.insert(
        "logdrain_axiom_dataset_name".into(),
        or_null(&r, "logdrain_axiom_dataset_name"),
    );
    m.insert(
        "is_logdrain_custom_enabled".into(),
        truthy(&r, "is_logdrain_custom_enabled"),
    );
    m.insert(
        "is_logdrain_cloudwatch_enabled".into(),
        truthy(&r, "is_logdrain_cloudwatch_enabled"),
    );
    for k in [
        "logdrain_cloudwatch_region",
        "logdrain_cloudwatch_group",
        "logdrain_cloudwatch_stream_prefix",
    ] {
        m.insert(k.into(), or_null(&r, k));
    }
    if ctx.can_read_sensitive {
        m.insert(
            "logdrain_newrelic_license_key".into(),
            or_null(&r, "logdrain_newrelic_license_key"),
        );
        m.insert(
            "logdrain_axiom_api_key".into(),
            or_null(&r, "logdrain_axiom_api_key"),
        );
        m.insert(
            "logdrain_custom_config".into(),
            plain(state, &r, "logdrain_custom_config"),
        );
        m.insert(
            "logdrain_custom_config_parser".into(),
            or_null(&r, "logdrain_custom_config_parser"),
        );
    }
    Value::Object(m)
}

pub async fn drains_show(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    Ok(ok(drain_transform(&state, &ctx, &s)).into_response())
}

fn re(p: &str) -> Regex {
    Regex::new(p).unwrap_or_else(|_| unreachable!())
}

static DRAIN_KEY: LazyLock<Regex> = LazyLock::new(|| re(r"^[a-zA-Z0-9_\-\.]+\n?\z"));
static CW_REGION: LazyLock<Regex> = LazyLock::new(|| re(r"^[a-z]{2}(-[a-z]+)+-\d+\n?\z"));
static CW_GROUP: LazyLock<Regex> = LazyLock::new(|| re(r"^[\.\-_/#A-Za-z0-9]{1,512}\n?\z"));
static CW_PREFIX: LazyLock<Regex> = LazyLock::new(|| re(r"^[A-Za-z0-9_\-\./#]*\n?\z"));

/// `ValidFluentBitConfig::check`.
pub(crate) fn fluent_bit_check(value: &str, parsers_only: bool) -> Option<String> {
    if value.trim().is_empty() {
        return None;
    }
    let allowed: &[&str] = if parsers_only {
        &["PARSER", "MULTILINE_PARSER"]
    } else {
        &[
            "SERVICE",
            "INPUT",
            "FILTER",
            "OUTPUT",
            "PARSER",
            "MULTILINE_PARSER",
        ]
    };
    static HEADER: LazyLock<Regex> = LazyLock::new(|| re(r"^\[\s*([A-Za-z_]+)\s*\]\n?\z"));
    static KEY: LazyLock<Regex> = LazyLock::new(|| re(r"^[a-z_][a-z0-9_.\-]*\n?\z"));
    static PLUGIN: LazyLock<Regex> = LazyLock::new(|| re(r"^[a-z0-9_]+\n?\z"));
    let normalized = value.replace("\r\n", "\n").replace('\r', "\n");
    let mut section: Option<String> = None;
    for (number, raw) in normalized.split('\n').enumerate() {
        let line = coolify_remote::text::php_trim(raw);
        let n = number + 1;
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('@') {
            let directive = line
                .split_whitespace()
                .next()
                .unwrap_or_default()
                .to_lowercase();
            if parsers_only || directive != "@set" {
                return Some(format!("line {n} uses the {directive} directive."));
            }
            continue;
        }
        if line.starts_with('[') {
            let Some(c) = HEADER.captures(line) else {
                return Some(format!("line {n} is not a valid section header."));
            };
            let name = c.get(1).map_or("", |m| m.as_str()).to_uppercase();
            if !allowed.contains(&name.as_str()) {
                return Some(format!(
                    "line {n} uses the [{name}] section. Allowed sections: {}.",
                    allowed.join(", ")
                ));
            }
            section = Some(name);
            continue;
        }
        let Some(current) = &section else {
            return Some(format!("line {n} is outside a section."));
        };
        let mut parts = line.splitn(2, char::is_whitespace);
        let key = parts.next().unwrap_or_default().to_lowercase();
        if !KEY.is_match(&key) {
            return Some(format!("line {n} has an invalid key."));
        }
        if key == "plugins_file" {
            return Some(format!("line {n} uses the {key} key."));
        }
        if key == "name" && ["INPUT", "FILTER", "OUTPUT"].contains(&current.as_str()) {
            let rest = coolify_remote::text::php_trim(parts.next().unwrap_or_default());
            let plugin = rest.split("://").next().unwrap_or_default().to_lowercase();
            if !PLUGIN.is_match(&plugin) {
                return Some(format!("line {n} must name a plugin literally."));
            }
            if ["lua", "wasm", "exec", "exec_wasi", "file"].contains(&plugin.as_str()) {
                return Some(format!("line {n} uses the {plugin} plugin."));
            }
        }
    }
    None
}

fn fluent_config_rule(v: &Value) -> Option<String> {
    let s = v.as_str()?;
    fluent_bit_check(s, false).map(|e| format!("The :attribute is not allowed: {e}"))
}

fn fluent_parser_rule(v: &Value) -> Option<String> {
    let s = v.as_str()?;
    fluent_bit_check(s, true).map(|e| format!("The :attribute is not allowed: {e}"))
}

pub async fn drains_update(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let input = validated_json(&headers, &uri, &body)?;
    let s = find_server(&state, &ctx, &uuid).await?;
    authorize_update(&ctx)?;
    use Rule::{Boolean, CustomOwned, Max, Nullable, Regex as Re, String as Str, Url};
    let fields = vec![
        Field {
            name: "is_logdrain_newrelic_enabled",
            rules: vec![Boolean],
        },
        Field {
            name: "logdrain_newrelic_license_key",
            rules: vec![Nullable, Str, Re(&DRAIN_KEY)],
        },
        Field {
            name: "logdrain_newrelic_base_uri",
            rules: vec![Nullable, Url],
        },
        Field {
            name: "is_logdrain_axiom_enabled",
            rules: vec![Boolean],
        },
        Field {
            name: "logdrain_axiom_dataset_name",
            rules: vec![Nullable, Str, Re(&DRAIN_KEY)],
        },
        Field {
            name: "logdrain_axiom_api_key",
            rules: vec![Nullable, Str, Re(&DRAIN_KEY)],
        },
        Field {
            name: "is_logdrain_custom_enabled",
            rules: vec![Boolean],
        },
        Field {
            name: "logdrain_custom_config",
            rules: vec![Nullable, Str, CustomOwned(fluent_config_rule)],
        },
        Field {
            name: "logdrain_custom_config_parser",
            rules: vec![Nullable, Str, CustomOwned(fluent_parser_rule)],
        },
        Field {
            name: "is_logdrain_cloudwatch_enabled",
            rules: vec![Boolean],
        },
        Field {
            name: "logdrain_cloudwatch_region",
            rules: vec![Nullable, Str, Max(64), Re(&CW_REGION)],
        },
        Field {
            name: "logdrain_cloudwatch_group",
            rules: vec![Nullable, Str, Re(&CW_GROUP)],
        },
        Field {
            name: "logdrain_cloudwatch_stream_prefix",
            rules: vec![Nullable, Str, Max(256), Re(&CW_PREFIX)],
        },
    ];
    validate(&input, &fields, CUSTOM_API_MESSAGES, DRAIN_FIELDS)?;
    // The attributes after assignment, before saving.
    let current = raw_settings(&s);
    let enabled = |k: &str| {
        if input.has(k) {
            input_bool(&input, k)
        } else {
            current.get(k).is_some_and(crate::serialize::php_truthy)
        }
    };
    let blank = |k: &str| -> bool {
        let is_blank = |v: Option<&Value>| match v {
            None | Some(Value::Null) => true,
            Some(Value::String(t)) => t.trim().is_empty(),
            _ => false,
        };
        if input.has(k) {
            is_blank(input.get(k))
        } else {
            is_blank(current.get(k))
        }
    };
    let collect = |pairs: &[(&'static str, &'static str)]| {
        let found: Vec<(&str, &str)> = pairs
            .iter()
            .filter(|(k, _)| blank(k))
            .map(|(k, m)| (*k, *m))
            .collect();
        (!found.is_empty()).then(|| validation_errors(&found))
    };
    if enabled("is_logdrain_newrelic_enabled")
        && let Some(e) = collect(&[
            (
                "logdrain_newrelic_license_key",
                "The New Relic license key is required when New Relic log drain is enabled.",
            ),
            (
                "logdrain_newrelic_base_uri",
                "The New Relic base URI is required when New Relic log drain is enabled.",
            ),
        ])
    {
        return Err(e);
    }
    if enabled("is_logdrain_axiom_enabled")
        && let Some(e) = collect(&[
            (
                "logdrain_axiom_dataset_name",
                "The Axiom dataset name is required when Axiom log drain is enabled.",
            ),
            (
                "logdrain_axiom_api_key",
                "The Axiom API key is required when Axiom log drain is enabled.",
            ),
        ])
    {
        return Err(e);
    }
    if enabled("is_logdrain_custom_enabled") && blank("logdrain_custom_config") {
        return Err(validation_errors(&[(
            "logdrain_custom_config",
            "The custom log drain config is required when custom log drain is enabled.",
        )]));
    }
    if enabled("is_logdrain_cloudwatch_enabled") {
        if enabled("is_logdrain_newrelic_enabled")
            || enabled("is_logdrain_axiom_enabled")
            || enabled("is_logdrain_custom_enabled")
            || enabled("is_logdrain_highlight_enabled")
        {
            return Err(validation_errors(&[(
                "is_logdrain_cloudwatch_enabled",
                "Amazon CloudWatch Logs cannot be enabled together with another log drain.",
            )]));
        }
        if let Some(e) = collect(&[
            (
                "logdrain_cloudwatch_region",
                "The CloudWatch region is required when Amazon CloudWatch Logs is enabled.",
            ),
            (
                "logdrain_cloudwatch_group",
                "The CloudWatch log group is required when Amazon CloudWatch Logs is enabled.",
            ),
        ]) {
            return Err(e);
        }
    }
    let mut values = Vec::new();
    for field in DRAIN_FIELDS {
        if !input.has(field) {
            continue;
        }
        let v = if field.starts_with("is_") {
            bool_setting(&input, field)
        } else if *field == "logdrain_custom_config" {
            encrypted_setting(&input, field)
        } else {
            text_setting(&input, field)
        };
        values.push((*field, v));
    }
    save_settings(&state, &s, &values).await?;
    let fresh = reload(&state, &s).await?;
    let info = coolify_jobs::server::ServerInfo::from_row(&fresh);
    if info.is_functional()
        && let Some(jobs) = &state.jobs
    {
        let r = raw_settings(&fresh);
        let on = |k: &str| r.get(k).is_some_and(crate::serialize::php_truthy);
        // `isLogDrainEnabled()`.
        let drain = on("is_logdrain_newrelic_enabled")
            || on("is_logdrain_highlight_enabled")
            || on("is_logdrain_axiom_enabled")
            || on("is_logdrain_custom_enabled")
            || on("is_logdrain_cloudwatch_enabled");
        jobs.spawn_log_drain(fresh.id, drain);
    }
    audit("api.server.log_drains.updated", &ctx, &s);
    Ok(ok(drain_transform(&state, &ctx, &fresh)).into_response())
}

// ── Cloudflare tunnel ───────────────────────────────────────────────────

fn tunnel_transform(s: &ServerRow) -> Map<String, Value> {
    let r = raw_settings(s);
    let mut m = Map::new();
    m.insert(
        "is_cloudflare_tunnel".into(),
        truthy(&r, "is_cloudflare_tunnel"),
    );
    m.insert(
        "ip".into(),
        Value::String(crate::routes::servers::ip_accessor(
            s.record
                .get("ip")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        )),
    );
    m.insert(
        "ip_previous".into(),
        s.record.get("ip_previous").cloned().unwrap_or(Value::Null),
    );
    m
}

/// `isLocalhost()`.
pub(crate) fn is_localhost(s: &ServerRow) -> bool {
    s.id == 0 || s.record.get("ip").and_then(Value::as_str) == Some("host.docker.internal")
}

const TUNNEL_LOCALHOST: ApiError = ApiError::Message(
    StatusCode::UNPROCESSABLE_ENTITY,
    "Cloudflare Tunnel cannot be configured on the localhost server.",
);

pub async fn tunnel_show(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    Ok(ok(Value::Object(tunnel_transform(&s))).into_response())
}

async fn restore_previous_ip(state: &AppState, s: &ServerRow) -> Result<bool, ApiError> {
    let previous = s
        .record
        .get("ip_previous")
        .and_then(Value::as_str)
        .filter(|p| !p.is_empty() && *p != "0")
        .map(str::to_string);
    let Some(previous) = previous else {
        return Ok(false);
    };
    // `$server->update(['ip' => $server->ip_previous])`: the saving hook stores the old IP.
    let current = s
        .record
        .get("ip")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if current != previous {
        let mut changes = vec![("ip", C::Text(Some(previous)))];
        if !current.is_empty() && current != "0" {
            changes.push(("ip_previous", C::Text(Some(current))));
        }
        servers::update_columns(&state.db, s.id, &changes, state.now()).await?;
        if let Some(jobs) = &state.jobs {
            jobs.remote().forget_host_key(&s.uuid);
        }
    }
    Ok(true)
}

pub async fn tunnel_update(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let input = validated_json(&headers, &uri, &body)?;
    let s = find_server(&state, &ctx, &uuid).await?;
    authorize_update(&ctx)?;
    if is_localhost(&s) {
        return Err(TUNNEL_LOCALHOST);
    }
    validate(
        &input,
        &[Field {
            name: "is_cloudflare_tunnel",
            rules: vec![Rule::Required, Rule::Boolean],
        }],
        CUSTOM_API_MESSAGES,
        &["is_cloudflare_tunnel"],
    )?;
    let on = input_bool(&input, "is_cloudflare_tunnel");
    save_settings(&state, &s, &[("is_cloudflare_tunnel", Setting::Bool(on))]).await?;
    if !on {
        restore_previous_ip(&state, &s).await?;
    }
    audit("api.server.cloudflare_tunnel.updated", &ctx, &s);
    let fresh = reload(&state, &s).await?;
    Ok(ok(Value::Object(tunnel_transform(&fresh))).into_response())
}

pub async fn tunnel_enable(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    authorize_update(&ctx)?;
    if is_localhost(&s) {
        return Err(TUNNEL_LOCALHOST);
    }
    save_settings(&state, &s, &[("is_cloudflare_tunnel", Setting::Bool(true))]).await?;
    audit("api.server.cloudflare_tunnel.enabled", &ctx, &s);
    let fresh = reload(&state, &s).await?;
    let mut m = Map::new();
    m.insert("message".into(), json!("Cloudflare Tunnel enabled."));
    m.extend(tunnel_transform(&fresh));
    Ok(ok(Value::Object(m)).into_response())
}

pub async fn tunnel_disable(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    authorize_update(&ctx)?;
    if is_localhost(&s) {
        return Err(TUNNEL_LOCALHOST);
    }
    save_settings(
        &state,
        &s,
        &[("is_cloudflare_tunnel", Setting::Bool(false))],
    )
    .await?;
    let message = if restore_previous_ip(&state, &s).await? {
        "Cloudflare Tunnel disabled. Server IP restored to its previous IP address."
    } else {
        "Cloudflare Tunnel disabled. Action required: Update the server IP address to its real IP address if needed."
    };
    audit("api.server.cloudflare_tunnel.disabled", &ctx, &s);
    let fresh = reload(&state, &s).await?;
    let mut m = Map::new();
    m.insert("message".into(), json!(message));
    m.extend(tunnel_transform(&fresh));
    Ok(ok(Value::Object(m)).into_response())
}
