//! Page endpoints of the instance settings: `App\Livewire\Settings\Index`, `Settings\Advanced`,
//! `Settings\Updates`, `SettingsEmail` and `SettingsBackup` (with `Project\Database\BackupEdit`
//! and `BackupExecutions` for the instance database). A Livewire action is one POST with the form.

use std::time::Duration;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::middleware::from_fn_with_state;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Extension, Json, Router};
use coolify_db::audit::Actor;
use coolify_db::instance_settings as settings;
use coolify_db::records::Record;
use coolify_db::servers::{self, ColumnValue};
use coolify_jobs::notifications::Mail;
use coolify_jobs::server_ops::proxy as proxy_ops;
use coolify_jobs::server_ops::{Srv, version_compare, versions_data};
use serde_json::{Map, Value, json};

use crate::audit::Meta;
use crate::auth::AuthCtx;
use crate::response::{ApiError, ApiResult, ok};
use crate::state::AppState;
use crate::ui_notifications::{blank, numeric, summarize, toast_error, unauthorized};

pub fn routes(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/ui/settings/general", get(general).post(general_action))
        .route("/ui/settings/advanced", get(advanced).post(advanced_action))
        .route("/ui/settings/updates", get(updates).post(updates_action))
        .route("/ui/settings/upgrade", get(upgrade_state).post(upgrade))
        .route("/ui/settings/upgrade/status", get(upgrade_status))
        .route("/ui/settings/email", get(email).post(email_action))
        .route("/ui/settings/backup", get(backup).post(backup_action))
        .route("/ui/settings/backup/executions", get(backup_executions))
        .route_layer(from_fn_with_state(state, crate::session::ui_guard))
}

pub const TIMEZONES: &str = include_str!("timezones.txt");

/// `validate_timezone` and the `timezone` rule: PHP's `timezone_identifiers_list()`.
fn valid_timezone(tz: &str) -> bool {
    TIMEZONES.lines().any(|l| l == tz)
}

fn text<'a>(values: &'a Map<String, Value>, k: &str) -> Option<&'a str> {
    values.get(k).and_then(Value::as_str)
}

/// Livewire: an empty string on a nullable property arrives as null.
fn nullable(values: &Map<String, Value>, k: &str) -> Value {
    match values.get(k) {
        None | Some(Value::Null) => Value::Null,
        Some(Value::String(s)) if s.is_empty() => Value::Null,
        Some(v) => v.clone(),
    }
}

fn boolean(values: &Map<String, Value>, k: &str) -> bool {
    values.get(k).and_then(Value::as_bool).unwrap_or(false)
}

/// PHP `==` between what a column holds and what a form set.
fn loose_eq(a: &Value, b: &Value) -> bool {
    let empty = |v: &Value| match v {
        Value::Null => true,
        Value::Bool(b) => !b,
        Value::String(s) => s.is_empty() || s == "0",
        Value::Number(n) => n.as_f64() == Some(0.0),
        Value::Array(a) => a.is_empty(),
        Value::Object(_) => false,
    };
    match (a, b) {
        (Value::Null, Value::String(s)) | (Value::String(s), Value::Null) => s.is_empty(),
        (Value::Bool(_), _) | (_, Value::Bool(_)) | (Value::Null, _) | (_, Value::Null) => {
            empty(a) == empty(b)
        }
        (Value::Number(_) | Value::String(_), Value::Number(_) | Value::String(_)) => {
            if numeric(a) && numeric(b) {
                as_f64(a) == as_f64(b)
            } else {
                a == b
            }
        }
        _ => a == b,
    }
}

fn as_f64(v: &Value) -> f64 {
    match v {
        Value::Number(n) => n.as_f64().unwrap_or(0.0),
        Value::String(s) => s.trim().parse().unwrap_or(0.0),
        _ => 0.0,
    }
}

const INTEGER_COLUMNS: &[&str] = &[
    "smtp_port",
    "smtp_timeout",
    "public_port_min",
    "public_port_max",
    "avatar_s3_storage_id",
    "file_download_s3_storage_id",
];

/// The stored value as the model casts it, secrets decrypted.
fn plain(state: &AppState, row: &Record, k: &str) -> Result<Value, ApiError> {
    let v = row.get(k).cloned().unwrap_or(Value::Null);
    if settings::SECRET_COLUMNS.contains(&k)
        && let Some(c) = v.as_str()
    {
        let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
        return enc
            .decrypt_string(c)
            .map(Value::String)
            .map_err(|_| ApiError::Internal);
    }
    Ok(v)
}

/// `$settings->...; auditChangedFields($settings); $settings->save();`: writes what `getDirty`
/// holds, in attribute order, and returns what `auditChangedFields` keeps.
async fn save(
    state: &AppState,
    pairs: &[(&'static str, Value)],
) -> Result<(Vec<String>, Vec<String>), ApiError> {
    let row = settings::row(&state.db).await?;
    let mut values: Vec<(&'static str, ColumnValue)> = Vec::new();
    let mut json_value: Option<String> = None;
    let mut changed = Vec::new();
    let mut dirty = Vec::new();
    for key in row.keys() {
        let Some((col, new)) = pairs.iter().find(|(c, _)| c == key) else {
            continue;
        };
        let current = plain(state, &row, col)?;
        let integer = INTEGER_COLUMNS.contains(col);
        let new = if integer && blank(Some(new)) {
            &Value::Null
        } else {
            new
        };
        let same = if integer && !new.is_null() && !current.is_null() {
            as_f64(new) == as_f64(&current)
        } else {
            *new == current
        };
        if same {
            continue;
        }
        dirty.push(col.to_string());
        if !loose_eq(&current, new) {
            changed.push(col.to_string());
        }
        if *col == "webhook_allowed_internal_hosts" {
            json_value = Some(coolify_compat::php_json::encode(new));
            continue;
        }
        let value = if settings::SECRET_COLUMNS.contains(col) {
            let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
            ColumnValue::Text(new.as_str().map(|s| enc.encrypt_string(s)))
        } else {
            match new {
                Value::Bool(b) => ColumnValue::Bool(*b),
                Value::Null => ColumnValue::Text(None),
                Value::Number(_) | Value::String(_) if integer => {
                    ColumnValue::Int(as_f64(new) as i64)
                }
                Value::String(s) => ColumnValue::Text(Some(s.clone())),
                other => ColumnValue::Text(Some(other.to_string())),
            }
        };
        values.push((col, value));
    }
    settings::update(&state.db, &values, state.now()).await?;
    if let Some(j) = json_value {
        state.db.ensure_writable()?;
        sqlx::query(
            "UPDATE instance_settings SET webhook_allowed_internal_hosts = $1::json, updated_at = $2 WHERE id = 0",
        )
        .bind(j)
        .bind(state.now())
        .execute(state.db.pool())
        .await
        .map_err(|_| ApiError::Internal)?;
    }
    // `InstanceSettings::updated`.
    if dirty
        .iter()
        .any(|c| matches!(c.as_str(), "fqdn" | "public_ipv4" | "public_ipv6"))
    {
        let fresh = settings::row(&state.db).await?;
        follow_instance_url_change(state, &row, &fresh).await?;
    }
    Ok((changed, dirty))
}

/// `ServerSetting::instanceSentinelUrl`.
fn instance_sentinel_url(row: &Record) -> Option<String> {
    let filled = |k: &str| {
        row.get(k)
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
    };
    if let Some(f) = filled("fqdn") {
        return Some(f.to_string());
    }
    if let Some(ip) = filled("public_ipv4") {
        return Some(format!("http://{ip}:8000"));
    }
    filled("public_ipv6").map(|ip| {
        // `formatHostForUrl`.
        let host = if ip.parse::<std::net::Ipv6Addr>().is_ok() {
            format!("[{ip}]")
        } else {
            ip.to_string()
        };
        format!("http://{host}:8000")
    })
}

/// `ServerSetting::followInstanceUrlChange`: each saved row runs its `updated` hook.
async fn follow_instance_url_change(
    state: &AppState,
    old: &Record,
    new: &Record,
) -> Result<(), ApiError> {
    let (Some(from), Some(to)) = (instance_sentinel_url(old), instance_sentinel_url(new)) else {
        return Ok(());
    };
    if from == to {
        return Ok(());
    }
    let ids: Vec<i64> = sqlx::query_scalar(
        "UPDATE server_settings SET sentinel_custom_url = $1, updated_at = $3 WHERE sentinel_custom_url = $2 AND server_id <> 0 RETURNING server_id",
    )
    .bind(&to)
    .bind(&from)
    .bind(state.now())
    .fetch_all(state.db.pool())
    .await
    .map_err(|_| ApiError::Internal)?;
    for id in ids {
        if let Some(s) = servers::find_by_id(&state.db, id).await? {
            crate::routes::server_settings::sentinel_settings_hook(
                state,
                &s,
                &["sentinel_custom_url"],
            );
        }
    }
    Ok(())
}

/// `auditLog($event, ['team_id' => null, 'resource' => 'instance', 'section' => ...])`.
async fn audit_instance(
    state: &AppState,
    ctx: &AuthCtx,
    meta: &Meta,
    event: &str,
    section: &str,
    changed: Vec<String>,
) {
    if changed.is_empty() {
        return;
    }
    let actor: Actor = crate::audit::actor(state, ctx, meta).await;
    crate::audit::log_as(
        state,
        None,
        &actor,
        event,
        json!({
            "team_id": null,
            "resource": "instance",
            "section": section,
            "changed_fields": changed,
        }),
    )
    .await;
}

async fn admin(state: &AppState, ctx: &AuthCtx) -> Result<bool, ApiError> {
    Ok(settings::is_instance_admin(&state.db, ctx.user_id).await?)
}

fn to_dashboard() -> Response {
    ok(json!({"redirect": "/"})).into_response()
}

fn ok_message(message: &str, extra: Value) -> Response {
    let mut m = extra.as_object().cloned().unwrap_or_default();
    m.insert("message".into(), json!(message));
    ok(Value::Object(m)).into_response()
}

#[derive(serde::Deserialize)]
struct Action {
    action: String,
    #[serde(default)]
    values: Map<String, Value>,
}

fn server_error(message: impl Into<String>) -> Response {
    toast_error(StatusCode::BAD_REQUEST, message)
}

fn is_url(s: &str) -> bool {
    crate::laravel_url::is_url(s)
}

// ---- Settings\Index ----

fn general_values(row: &Record) -> Value {
    json!({
        "fqdn": row.get("fqdn"),
        "is_dashboard_force_https_enabled": row.get("is_dashboard_force_https_enabled"),
        "public_port_min": row.get("public_port_min"),
        "public_port_max": row.get("public_port_max"),
        "instance_name": row.get("instance_name"),
        "public_ipv4": row.get("public_ipv4"),
        "public_ipv6": row.get("public_ipv6"),
        "instance_timezone": row.get("instance_timezone"),
        "dev_helper_version": row.get("dev_helper_version"),
    })
}

async fn general(State(state): State<AppState>, Extension(ctx): Extension<AuthCtx>) -> ApiResult {
    if !admin(&state, &ctx).await? {
        return Ok(to_dashboard());
    }
    let row = settings::row(&state.db).await?;
    let mut timezones: Vec<&str> = TIMEZONES.lines().collect();
    timezones.sort_unstable();
    Ok(ok(json!({
        "settings": general_values(&row),
        "timezones": timezones,
    }))
    .into_response())
}

/// The `#[Validate]` attributes of `Settings\Index`, in property order.
fn general_rules(v: &Map<String, Value>) -> Vec<String> {
    let mut e = Vec::new();
    if let Some(f) = text(v, "fqdn").filter(|s| !s.trim().is_empty()) {
        if f.chars().count() > 255 {
            e.push("URL must not exceed 255 characters.".to_owned());
        }
        if !is_url(f) {
            e.push("Invalid instance URL.".to_owned());
        }
    }
    for (k, attr) in [
        ("public_port_min", "public port min"),
        ("public_port_max", "public port max"),
    ] {
        match v.get(k) {
            _ if blank(v.get(k)) => e.push(format!("The {attr} field is required.")),
            Some(p) if !numeric(p) || as_f64(p).fract() != 0.0 => {
                e.push(format!("The {attr} field must be an integer."));
            }
            Some(p) if as_f64(p) < 1025.0 => {
                e.push(format!("The {attr} field must be at least 1025."));
            }
            Some(p) if as_f64(p) > 65535.0 => {
                e.push(format!("The {attr} field must not be greater than 65535."));
            }
            _ => {}
        }
    }
    if let Some(n) = text(v, "instance_name")
        && n.chars().count() > 255
    {
        e.push("The instance name field must not be greater than 255 characters.".to_owned());
    }
    if let Some(ip) = text(v, "public_ipv4").filter(|s| !s.trim().is_empty())
        && ip.parse::<std::net::Ipv4Addr>().is_err()
    {
        e.push("The public ipv4 field must be a valid IPv4 address.".to_owned());
    }
    if let Some(ip) = text(v, "public_ipv6").filter(|s| !s.trim().is_empty())
        && ip.parse::<std::net::Ipv6Addr>().is_err()
    {
        e.push("The public ipv6 field must be a valid IPv6 address.".to_owned());
    }
    match text(v, "instance_timezone") {
        None | Some("") => e.push("The instance timezone field is required.".to_owned()),
        Some(t) if !valid_timezone(t) => {
            e.push("The instance timezone field must be a valid timezone.".to_owned());
        }
        _ => {}
    }
    if let Some(h) = text(v, "dev_helper_version").filter(|s| !s.trim().is_empty()) {
        if h.chars().count() > 128 {
            e.push(
                "The dev helper version field must not be greater than 128 characters.".to_owned(),
            );
        }
        let mut chars = h.chars();
        let first = chars
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
        if !first || !chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-')) {
            e.push(
                "Dev helper version must match Docker tag format (alphanumeric, _, ., -; first char cannot be . or -)."
                    .to_owned(),
            );
        }
    }
    e
}

/// `domainConflictKey`: scheme and trailing slash do not matter.
fn conflict_key(domain: &str) -> String {
    let d = domain.trim().trim_end_matches('/').to_lowercase();
    d.strip_prefix("https://")
        .or_else(|| d.strip_prefix("http://"))
        .unwrap_or(&d)
        .to_owned()
}

/// `checkDomainUsage(domain: $fqdn)`: applications and service applications of every team.
pub(crate) async fn domain_usage(state: &AppState, fqdn: &str) -> Result<Vec<Value>, ApiError> {
    let wanted = conflict_key(fqdn.trim_end_matches('/'));
    let mut conflicts = Vec::new();
    let pool = state.db.pool();
    type AppRow = (String, Option<String>, String, String, String, String);
    let apps: Vec<AppRow> = sqlx::query_as(
        "SELECT a.name, a.fqdn, a.uuid, p.uuid, e.uuid, COALESCE(a.build_pack, '') FROM applications a
         JOIN environments e ON e.id = a.environment_id JOIN projects p ON p.id = e.project_id
         WHERE a.deleted_at IS NULL ORDER BY a.id",
    )
    .fetch_all(pool)
    .await
    .map_err(|_| ApiError::Internal)?;
    for (name, list, uuid, project, env, _pack) in apps {
        for d in list.as_deref().unwrap_or("").split(',') {
            if d.is_empty() {
                continue;
            }
            let naked = d.trim_end_matches('/');
            if conflict_key(naked) == wanted {
                conflicts.push(json!({
                    "domain": naked,
                    "resource_name": name,
                    "resource_link": format!("/project/{project}/environment/{env}/application/{uuid}"),
                    "resource_type": "application",
                    "message": format!("Domain {naked} is already in use by application '{name}'"),
                }));
            }
        }
    }
    type SvcRow = (Option<String>, String, String, String, String);
    let services: Vec<SvcRow> = sqlx::query_as(
        "SELECT sa.fqdn, s.name, s.uuid, p.uuid, e.uuid FROM service_applications sa
         JOIN services s ON s.id = sa.service_id JOIN environments e ON e.id = s.environment_id
         JOIN projects p ON p.id = e.project_id WHERE sa.deleted_at IS NULL AND s.deleted_at IS NULL ORDER BY sa.id",
    )
    .fetch_all(pool)
    .await
    .map_err(|_| ApiError::Internal)?;
    for (list, name, uuid, project, env) in services {
        for d in list.as_deref().unwrap_or("").split(',') {
            if d.is_empty() {
                continue;
            }
            let naked = d.trim_end_matches('/');
            if conflict_key(naked) == wanted {
                conflicts.push(json!({
                    "domain": naked,
                    "resource_name": name,
                    "resource_link": format!("/project/{project}/environment/{env}/service/{uuid}"),
                    "resource_type": "service",
                    "message": format!("Domain {naked} is already in use by service '{name}'"),
                }));
            }
        }
    }
    Ok(conflicts)
}

/// `Server::setupDynamicProxyConfiguration()` on the localhost server.
async fn setup_instance_proxy(state: &AppState) -> Result<(), String> {
    let Some(jobs) = state.jobs.as_ref() else {
        return Ok(());
    };
    let row = servers::find_by_id(&state.db, 0)
        .await
        .map_err(|e| e.to_string())?
        .ok_or("No query results for model [App\\Models\\Server] 0")?;
    let srv = Srv::from_row(jobs, row);
    proxy_ops::setup_dynamic_proxy_configuration(&srv).await
}

/// `resolveServerIpAddress` / `serverDnsTargetIp` for the localhost server.
async fn dns_target(state: &AppState, row: &Record) -> Result<Option<String>, ApiError> {
    let configured = row
        .get("public_ipv4")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            row.get("public_ipv6")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
        })
        .map(str::to_owned);
    let configured = match configured {
        Some(c) => Some(c),
        None => servers::find_by_id(&state.db, 0).await?.and_then(|s| {
            s.record
                .get("ip")
                .and_then(Value::as_str)
                .map(str::to_owned)
        }),
    };
    let Some(c) = configured else {
        return Ok(None);
    };
    if c.parse::<std::net::IpAddr>().is_ok() {
        return Ok(Some(c));
    }
    Ok(Some(
        tokio::net::lookup_host((c.as_str(), 0))
            .await
            .ok()
            .and_then(|mut a| a.next())
            .map_or(c, |a| a.ip().to_string()),
    ))
}

/// `validateDNSEntry($fqdn, $server)`.
async fn validate_dns_entry(fqdn: &str, row: &Record, ip: Option<&str>) -> bool {
    let host = fqdn
        .split_once("://")
        .map_or(fqdn, |(_, r)| r)
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("");
    let host = host
        .rsplit_once('@')
        .map_or(host, |(_, h)| h)
        .split(':')
        .next()
        .unwrap_or("");
    if host.contains("sslip.io") {
        return true;
    }
    let servers_list = row
        .get("custom_dns_servers")
        .and_then(Value::as_str)
        .unwrap_or("");
    let want_v6 = ip.is_some_and(|i| i.parse::<std::net::Ipv6Addr>().is_ok());
    for server in servers_list.split(',') {
        let answers = crate::dns::query(server.trim(), host, want_v6).await;
        for a in answers {
            if crate::ip::is_cloudflare_ip(a) {
                return true;
            }
            if ip.and_then(|i| i.parse::<std::net::IpAddr>().ok()) == Some(a) {
                return true;
            }
        }
    }
    false
}

/// `dnsMismatchGuidanceMessage($target, $target)`.
fn dns_guidance(target: Option<&str>) -> String {
    match target {
        None => "DNS validation failed. Check your DNS records.".to_owned(),
        Some(t) => {
            let kind = if t.parse::<std::net::Ipv6Addr>().is_ok() {
                "AAAA"
            } else {
                "A"
            };
            format!("Required DNS record type {kind} pointing to {t}")
        }
    }
}

const GENERAL_COLUMNS: &[&str] = &[
    "is_dashboard_force_https_enabled",
    "public_port_min",
    "public_port_max",
    "instance_name",
    "public_ipv4",
    "public_ipv6",
    "instance_timezone",
    "dev_helper_version",
];

/// `detect_public_ips()`. `COOLIFY_RS_IFCONFIG_URL` points it at a local fake in tests.
async fn detect_public_ips() -> Vec<(&'static str, String)> {
    let url = std::env::var("COOLIFY_RS_IFCONFIG_URL")
        .unwrap_or_else(|_| "https://ifconfig.io".to_owned());
    let lookup = |flag: &'static str| {
        let url = url.clone();
        async move {
            let out = tokio::process::Command::new("curl")
                .args([flag, "--max-time", "10", &url])
                .output()
                .await
                .ok()?;
            Some(String::from_utf8_lossy(&out.stdout).trim().to_owned())
        }
    };
    let (v4, v6) = tokio::join!(lookup("-4s"), lookup("-6s"));
    let mut detected = Vec::new();
    if let Some(ip) = v4.filter(|i| i.parse::<std::net::Ipv4Addr>().is_ok()) {
        detected.push(("public_ipv4", ip));
    }
    if let Some(ip) = v6.filter(|i| i.parse::<std::net::Ipv6Addr>().is_ok()) {
        detected.push(("public_ipv6", ip));
    }
    detected
}

/// `get_public_ips()` at startup: looks up only what is blank.
pub async fn get_public_ips(state: &AppState) -> Result<(), ApiError> {
    let row = settings::row(&state.db).await?;
    let blank = |k: &str| row.get(k).and_then(Value::as_str).is_none_or(str::is_empty);
    if !blank("public_ipv4") && !blank("public_ipv6") {
        return Ok(());
    }
    for (column, ip) in detect_public_ips().await {
        if blank(column) {
            settings::update(
                &state.db,
                &[(column, ColumnValue::Text(Some(ip)))],
                state.now(),
            )
            .await?;
        }
    }
    Ok(())
}

async fn general_action(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Json(body): Json<Action>,
) -> ApiResult {
    if !admin(&state, &ctx).await? {
        return Ok(unauthorized());
    }
    let mut v = body.values;
    let saved = settings::row(&state.db).await?;
    match body.action.as_str() {
        "detect" => {
            let detected = detect_public_ips().await;
            if detected.is_empty() {
                return Ok(server_error("Could not detect a public IP address."));
            }
            let pairs: Vec<(&'static str, Value)> =
                detected.iter().map(|(k, ip)| (*k, json!(ip))).collect();
            let (changed, _) = save(&state, &pairs).await?;
            audit_instance(
                &state,
                &ctx,
                &meta,
                "ui.instance.settings.updated",
                "general",
                changed,
            )
            .await;
            let fresh = settings::row(&state.db).await?;
            let ips: Vec<&str> = detected.iter().map(|(_, ip)| ip.as_str()).collect();
            return Ok(ok_message(
                &format!("Detected {}.", ips.join(" and ")),
                json!({"settings": general_values(&fresh)}),
            ));
        }
        "submit" | "confirm" => {}
        _ => return Err(ApiError::Message(StatusCode::NOT_FOUND, "Not found.")),
    }
    let forced = body.action == "confirm";
    // `$this->fill`: the values the page holds.
    if !valid_timezone(text(&v, "instance_timezone").unwrap_or("")) {
        return Ok(ok(json!({
            "error": "Invalid timezone.",
            "instance_timezone": "UTC",
        }))
        .into_response());
    }
    let min = saved.get("public_port_min").map_or(0.0, as_f64);
    let max = saved.get("public_port_max").map_or(0.0, as_f64);
    if min > max {
        return Ok(ok(json!({
            "errors": {"settings.public_port_min": ["The minimum port must be lower than the maximum port."]},
        }))
        .into_response());
    }
    // Trim the FQDN to remove leading and trailing whitespace before validation.
    let fqdn = text(&v, "fqdn").map(|f| f.trim().to_owned());
    v.insert("fqdn".into(), json!(fqdn));
    v.insert(
        "public_port_min".into(),
        saved.get("public_port_min").cloned().unwrap_or(json!(9000)),
    );
    v.insert(
        "public_port_max".into(),
        saved.get("public_port_max").cloned().unwrap_or(json!(9100)),
    );
    if let Some(m) = summarize(&general_rules(&v)) {
        return Ok(toast_error(StatusCode::UNPROCESSABLE_ENTITY, m));
    }
    let fqdn = nullable(&v, "fqdn");
    let fqdn_text = fqdn.as_str().unwrap_or("").to_owned();
    let mut dns_error = None;
    if saved.get("is_dns_validation_enabled") == Some(&json!(true)) && !fqdn_text.is_empty() {
        let target = dns_target(&state, &saved).await?;
        if !validate_dns_entry(&fqdn_text, &saved, target.as_deref()).await {
            dns_error = Some(format!(
                "Validating DNS failed.<br><br>{}<br><br>Check this <a target='_blank' class='underline dark:text-white' href='https://coolify.io/docs/knowledge-base/dns-configuration'>documentation</a> for further help.",
                dns_guidance(target.as_deref())
            ));
        }
    }
    if !fqdn_text.is_empty() && !forced {
        let conflicts = domain_usage(&state, &fqdn_text).await?;
        if !conflicts.is_empty() {
            return Ok(ok(json!({
                "dns_error": dns_error,
                "conflicts": conflicts,
            }))
            .into_response());
        }
    }
    let mut pairs: Vec<(&'static str, Value)> = vec![("fqdn", fqdn)];
    for col in GENERAL_COLUMNS {
        let value = if matches!(*col, "is_dashboard_force_https_enabled") {
            json!(boolean(&v, col))
        } else if matches!(
            *col,
            "public_port_min" | "public_port_max" | "instance_timezone"
        ) {
            v.get(*col).cloned().unwrap_or(Value::Null)
        } else {
            nullable(&v, col)
        };
        pairs.push((col, value));
    }
    let (changed, _) = save(&state, &pairs).await?;
    audit_instance(
        &state,
        &ctx,
        &meta,
        "ui.instance.settings.updated",
        "general",
        changed,
    )
    .await;
    let fresh = settings::row(&state.db).await?;
    let proxy_error = setup_instance_proxy(&state).await.err();
    let mut res = json!({"settings": general_values(&fresh), "dns_error": dns_error});
    if let Some(e) = proxy_error {
        // `handleError`: the settings stay saved.
        res["error"] = json!(e);
    } else if dns_error_is_none(&res) {
        res["message"] = json!("Instance settings updated successfully!");
    }
    Ok(ok(res).into_response())
}

fn dns_error_is_none(res: &Value) -> bool {
    res.get("dns_error").is_none_or(Value::is_null)
}

// ---- Settings\Advanced ----

fn filter_ip(s: &str) -> bool {
    s.parse::<std::net::IpAddr>().is_ok()
}

/// `App\Rules\ValidDnsServers`.
fn valid_dns_servers(v: &str) -> Option<String> {
    if v.is_empty() || v == "0" {
        return None;
    }
    let bad: Vec<&str> = v
        .split(',')
        .map(str::trim)
        .filter(|e| !e.is_empty() && *e != "0" && !filter_ip(e))
        .collect();
    (!bad.is_empty()).then(|| {
        format!(
            "The following entries are not valid DNS server IP addresses: {}",
            bad.join(", ")
        )
    })
}

/// `App\Rules\ValidIpOrCidr`.
fn valid_ip_or_cidr(v: &str) -> Option<String> {
    if v.is_empty() || v == "0" || v == "0.0.0.0" {
        return None;
    }
    let mut bad = Vec::new();
    for entry in v.split(',').map(str::trim) {
        if entry.is_empty() || entry == "0" || entry.starts_with("0.0.0.0") {
            continue;
        }
        if entry.contains('/') {
            let parts: Vec<&str> = entry.split('/').collect();
            if parts.len() != 2 || !cidr_ok(parts[0], parts[1]) {
                bad.push(entry);
            }
        } else if !filter_ip(entry) {
            bad.push(entry);
        }
    }
    (!bad.is_empty()).then(|| {
        format!(
            "The following entries are not valid IP addresses or CIDR notations: {}",
            bad.join(", ")
        )
    })
}

fn cidr_ok(ip: &str, mask: &str) -> bool {
    let is_v6 = ip.parse::<std::net::Ipv6Addr>().is_ok();
    let max = if is_v6 { 128.0 } else { 32.0 };
    filter_ip(ip)
        && mask
            .trim()
            .parse::<f64>()
            .is_ok_and(|m| (0.0..=max).contains(&m))
}

/// `Str::replaceEnd(',', '')->trim()`.
fn strip_end_comma(s: &str) -> String {
    s.strip_suffix(',').unwrap_or(s).trim().to_owned()
}

/// `deduplicateAllowlist`.
fn deduplicate_allowlist(entries: Vec<String>) -> Vec<String> {
    if entries.len() <= 1 {
        return entries;
    }
    let parsed: Vec<(String, String, u32)> = entries
        .iter()
        .map(|e| e.trim())
        .filter(|e| !e.is_empty() && *e != "0")
        .map(|e| {
            if e == "0.0.0.0" {
                (e.to_owned(), e.to_owned(), 0)
            } else if let Some((ip, mask)) = e.split_once('/') {
                (e.to_owned(), ip.to_owned(), mask.parse().unwrap_or(0))
            } else {
                let m = if e.parse::<std::net::Ipv6Addr>().is_ok() {
                    128
                } else {
                    32
                };
                (e.to_owned(), e.to_owned(), m)
            }
        })
        .collect();
    let mut redundant = vec![false; parsed.len()];
    for i in 0..parsed.len() {
        if redundant[i] {
            continue;
        }
        for j in 0..parsed.len() {
            if i == j || redundant[j] {
                continue;
            }
            if parsed[j].2 >= parsed[i].2
                && let Ok(ip) = parsed[j].1.parse::<std::net::IpAddr>()
                && crate::ip::api_ip_allowed(Some(&format!("{}/{}", parsed[i].1, parsed[i].2)), ip)
            {
                redundant[j] = true;
            }
        }
    }
    parsed
        .into_iter()
        .zip(redundant)
        .filter(|(_, r)| !r)
        .map(|(p, _)| p.0)
        .collect()
}

/// `filter_var($entry, FILTER_VALIDATE_DOMAIN, FILTER_FLAG_HOSTNAME)`.
fn valid_hostname(entry: &str) -> bool {
    let h = entry.strip_suffix('.').unwrap_or(entry);
    if h.is_empty() || h.len() > 253 {
        return false;
    }
    h.split('.').all(|l| {
        !l.is_empty()
            && l.len() <= 63
            && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
            && !l.starts_with('-')
            && !l.ends_with('-')
    })
}

/// `isValidWebhookAllowlistEntry`.
fn valid_webhook_entry(entry: &str) -> bool {
    if filter_ip(entry) {
        return true;
    }
    if entry.contains('/') {
        let (ip, mask) = entry.split_once('/').unwrap_or((entry, ""));
        return cidr_ok(ip, mask);
    }
    valid_hostname(entry)
}

async fn storage_options(state: &AppState) -> Result<Vec<Value>, ApiError> {
    let rows: Vec<(i64, String)> = sqlx::query_as(
        "SELECT id, name FROM s3_storages WHERE team_id = 0 AND is_usable = true ORDER BY name",
    )
    .fetch_all(state.db.pool())
    .await
    .map_err(|_| ApiError::Internal)?;
    let mut out = vec![json!({"value": "local", "label": "Local storage"})];
    out.extend(
        rows.iter().map(
            |(id, name)| json!({"value": format!("s3:{id}"), "label": format!("{name} (S3)")}),
        ),
    );
    Ok(out)
}

fn advanced_values(row: &Record) -> Value {
    let or = |k: &str, d: bool| row.get(k).and_then(Value::as_bool).unwrap_or(d);
    let hosts = row
        .get("webhook_allowed_internal_hosts")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(",")
        })
        .unwrap_or_default();
    let avatar = match (
        row.get("avatar_storage_type").and_then(Value::as_str),
        row.get("avatar_s3_storage_id").and_then(Value::as_i64),
    ) {
        (Some("s3"), Some(id)) if id != 0 => format!("s3:{id}"),
        _ => "local".to_owned(),
    };
    let download = match row
        .get("file_download_s3_storage_id")
        .and_then(Value::as_i64)
    {
        Some(id) if id != 0 => format!("s3:{id}"),
        _ => "stream".to_owned(),
    };
    json!({
        "custom_dns_servers": row.get("custom_dns_servers"),
        "allowed_ips": row.get("allowed_ips"),
        "do_not_track": or("do_not_track", false),
        "is_registration_enabled": or("is_registration_enabled", false),
        "disable_registration_when_oauth_enabled": or("disable_registration_when_oauth_enabled", false),
        "is_dns_validation_enabled": or("is_dns_validation_enabled", false),
        "is_api_enabled": or("is_api_enabled", false),
        "disable_two_step_confirmation": or("disable_two_step_confirmation", false),
        "is_passkey_enabled": or("is_passkey_enabled", true),
        "is_sponsorship_popup_enabled": or("is_sponsorship_popup_enabled", false),
        "is_wire_navigate_enabled": or("is_wire_navigate_enabled", true),
        "is_mcp_server_enabled": or("is_mcp_server_enabled", false),
        "webhook_allowed_internal_hosts": hosts,
        "webhook_allow_localhost": or("webhook_allow_localhost", false),
        "avatar_storage": avatar,
        "image_cdn_url": row.get("image_cdn_url"),
        "file_download_storage": download,
    })
}

async fn advanced(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    headers: axum::http::HeaderMap,
) -> ApiResult {
    if !admin(&state, &ctx).await? {
        return Ok(to_dashboard());
    }
    let row = settings::row(&state.db).await?;
    let host = headers
        .get(axum::http::header::HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("localhost");
    let mcp = row
        .get("fqdn")
        .and_then(Value::as_str)
        .filter(|f| !f.is_empty())
        .map_or_else(
            || format!("http://{host}/mcp"),
            |f| format!("{}/mcp", f.trim_end_matches('/')),
        );
    Ok(ok(json!({
        "settings": advanced_values(&row),
        "storage_options": storage_options(&state).await?,
        "mcp_url": mcp,
    }))
    .into_response())
}

/// `saveAvatarStorageSetting`.
async fn avatar_columns(
    state: &AppState,
    choice: &str,
    pairs: &mut Vec<(&'static str, Value)>,
) -> Result<Result<(), String>, ApiError> {
    if choice == "local" {
        pairs.push(("avatar_storage_type", json!("local")));
        pairs.push(("avatar_s3_storage_id", Value::Null));
        return Ok(Ok(()));
    }
    let id = choice
        .strip_prefix("s3:")
        .unwrap_or(choice)
        .parse::<i64>()
        .unwrap_or(0);
    let found: Option<i64> = sqlx::query_scalar(
        "SELECT id FROM s3_storages WHERE team_id = 0 AND is_usable = true AND id = $1",
    )
    .bind(id)
    .fetch_optional(state.db.pool())
    .await
    .map_err(|_| ApiError::Internal)?;
    if found.is_none() || choice != format!("s3:{id}") {
        return Ok(Err(
            "The selected avatar storage is not available.".to_owned()
        ));
    }
    pairs.push(("avatar_storage_type", json!("s3")));
    pairs.push(("avatar_s3_storage_id", json!(id)));
    Ok(Ok(()))
}

/// `saveFileDownloadStorageSetting`. `FileDownloadStorageService` (the private-bucket check and
/// the lifecycle rule) is not ported, so an S3 choice other than the stored one is refused.
async fn download_columns(
    row: &Record,
    choice: &str,
    pairs: &mut Vec<(&'static str, Value)>,
) -> Result<(), String> {
    if choice == "stream" {
        pairs.push(("file_download_s3_storage_id", Value::Null));
        return Ok(());
    }
    let id = choice
        .strip_prefix("s3:")
        .unwrap_or(choice)
        .parse::<i64>()
        .unwrap_or(0);
    if row
        .get("file_download_s3_storage_id")
        .and_then(Value::as_i64)
        == Some(id)
    {
        return Ok(());
    }
    Err("The selected file download storage is not available.".to_owned())
}

#[allow(clippy::too_many_lines)]
async fn advanced_action(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Json(body): Json<Action>,
) -> ApiResult {
    if !admin(&state, &ctx).await? {
        return Ok(unauthorized());
    }
    let mut v = body.values;
    let row = settings::row(&state.db).await?;
    let mut hosts: Option<Vec<String>> = None;
    if body.action == "submit" {
        let mut e: Vec<String> = Vec::new();
        if let Some(m) = valid_dns_servers(text(&v, "custom_dns_servers").unwrap_or("")) {
            e.push(m);
        }
        if let Some(m) = valid_ip_or_cidr(text(&v, "allowed_ips").unwrap_or("")) {
            e.push(m);
        }
        if let Some(u) = text(&v, "image_cdn_url").filter(|s| !s.trim().is_empty()) {
            if !is_url(u) {
                e.push("The image cdn url field must be a valid URL.".to_owned());
            }
            if u.chars().count() > 255 {
                e.push(
                    "The image cdn url field must not be greater than 255 characters.".to_owned(),
                );
            }
        }
        if let Some(m) = summarize(&e) {
            return Ok(toast_error(StatusCode::UNPROCESSABLE_ENTITY, m));
        }

        let dns = strip_end_comma(text(&v, "custom_dns_servers").unwrap_or(""));
        let mut seen: Vec<String> = Vec::new();
        for d in dns.trim().split(',') {
            let d = d.trim().to_lowercase();
            if !seen.contains(&d) {
                seen.push(d);
            }
        }
        v.insert("custom_dns_servers".into(), json!(seen.join(",")));

        let ips = strip_end_comma(text(&v, "allowed_ips").unwrap_or(""));
        let mut ips_final = ips.clone();
        if !ips.is_empty() && !ips.split(',').any(|e| e.trim() == "0.0.0.0") {
            let mut invalid = Vec::new();
            let mut valid = Vec::new();
            for entry in ips.trim().split(',').map(str::trim) {
                if entry.is_empty() || entry == "0" {
                    continue;
                }
                if entry.contains('/') {
                    let mut parts = entry.split('/');
                    let (ip, mask) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
                    if cidr_ok(ip, mask) {
                        valid.push(entry.to_owned());
                    } else {
                        invalid.push(entry);
                    }
                } else if filter_ip(entry) {
                    valid.push(entry.to_owned());
                } else {
                    invalid.push(entry);
                }
            }
            if !invalid.is_empty() {
                return Ok(server_error(format!(
                    "Invalid IP addresses or subnets: {}",
                    invalid.join(", ")
                )));
            }
            if valid.is_empty() {
                return Ok(server_error("No valid IP addresses or subnets provided"));
            }
            ips_final = deduplicate_allowlist(valid).join(",");
        }
        v.insert("allowed_ips".into(), json!(ips_final));

        // `normalizeWebhookAllowedInternalHosts`.
        let mut entries: Vec<String> = Vec::new();
        for raw in text(&v, "webhook_allowed_internal_hosts")
            .unwrap_or("")
            .split([',', '\r', '\n'])
        {
            let e = raw.trim().to_lowercase();
            let e = e.trim_end_matches('.').to_owned();
            if !e.is_empty() && e != "0" && !entries.contains(&e) {
                entries.push(e);
            }
        }
        let bad: Vec<&str> = entries
            .iter()
            .map(String::as_str)
            .filter(|e| !valid_webhook_entry(e))
            .collect();
        if !bad.is_empty() {
            return Ok(server_error(format!(
                "Invalid webhook internal allowlist entries: {}",
                bad.join(", ")
            )));
        }
        v.insert(
            "webhook_allowed_internal_hosts".into(),
            json!(entries.join(",")),
        );
        hosts = Some(entries);
    } else if body.action != "instant" {
        return Err(ApiError::Message(StatusCode::NOT_FOUND, "Not found."));
    }

    // `instantSave`.
    let mut pairs: Vec<(&'static str, Value)> = Vec::new();
    for col in [
        "is_registration_enabled",
        "disable_registration_when_oauth_enabled",
        "do_not_track",
        "is_dns_validation_enabled",
        "is_api_enabled",
        "is_sponsorship_popup_enabled",
        "disable_two_step_confirmation",
        "is_passkey_enabled",
        "is_wire_navigate_enabled",
        "is_mcp_server_enabled",
        "webhook_allow_localhost",
    ] {
        pairs.push((col, json!(boolean(&v, col))));
    }
    // A submit stores what was normalized, including an empty string. An instant save stores the
    // property as it stands; Livewire already turned an empty nullable property into null.
    for col in ["custom_dns_servers", "allowed_ips"] {
        let value = if body.action == "submit" {
            v.get(col).cloned().unwrap_or(Value::Null)
        } else {
            nullable(&v, col)
        };
        pairs.push((col, value));
    }
    let stored_hosts = row
        .get("webhook_allowed_internal_hosts")
        .cloned()
        .filter(|h| !h.is_null())
        .unwrap_or_else(|| json!([]));
    pairs.push((
        "webhook_allowed_internal_hosts",
        hosts.map_or(stored_hosts, |h| json!(h)),
    ));
    let cdn = text(&v, "image_cdn_url")
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.trim_end_matches('/').to_owned());
    pairs.push(("image_cdn_url", json!(cdn)));
    if let Err(m) = avatar_columns(
        &state,
        text(&v, "avatar_storage").unwrap_or("local"),
        &mut pairs,
    )
    .await?
    {
        return Ok(server_error(m));
    }
    if let Err(m) = download_columns(
        &row,
        text(&v, "file_download_storage").unwrap_or("stream"),
        &mut pairs,
    )
    .await
    {
        return Ok(ok(json!({"error": m, "file_download_storage": "stream"})).into_response());
    }
    let (changed, _) = save(&state, &pairs).await?;
    audit_instance(
        &state,
        &ctx,
        &meta,
        "ui.instance.settings.updated",
        "advanced",
        changed,
    )
    .await;
    let fresh = settings::row(&state.db).await?;
    Ok(ok_message(
        "Settings updated!",
        json!({"settings": advanced_values(&fresh)}),
    ))
}

// ---- Settings\Updates and Livewire\Upgrade ----

/// `config('constants.coolify.autoupdate')`: `env('AUTOUPDATE')`, null when unset.
fn autoupdate_env() -> Value {
    match std::env::var("AUTOUPDATE") {
        Ok(v) => match v.to_lowercase().as_str() {
            "true" | "(true)" => json!(true),
            "false" | "(false)" => json!(false),
            "null" | "(null)" => Value::Null,
            "" | "empty" | "(empty)" => json!(""),
            _ => json!(v),
        },
        Err(_) => Value::Null,
    }
}

fn updates_values(row: &Record) -> Value {
    json!({
        "auto_update_frequency": row.get("auto_update_frequency"),
        "update_check_frequency": row.get("update_check_frequency"),
        "is_auto_update_enabled": row.get("is_auto_update_enabled"),
        "docker_registry_url": row.get("docker_registry_url").and_then(Value::as_str).filter(|s| !s.is_empty()).unwrap_or("docker.io"),
    })
}

/// `get_latest_version_of_coolify()`, including what `CheckForUpdatesJob` fetched.
fn latest_version(state: &AppState) -> String {
    let path = data_versions_path(state);
    let data = std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .unwrap_or_else(versions_data);
    data.pointer("/coolify/v4/version")
        .and_then(Value::as_str)
        .unwrap_or("0.0.0")
        .to_owned()
}

fn data_versions_path(_state: &AppState) -> std::path::PathBuf {
    let dir = std::env::var("COOLIFY_RS_DATA_DIR").unwrap_or_else(|_| "/data/coolify".to_owned());
    std::path::Path::new(&dir).join("versions.json")
}

/// `Upgrade::refreshUpgradeState`.
async fn refresh_upgrade_state(state: &AppState) -> Result<Value, ApiError> {
    let latest = latest_version(state);
    let current = state.version.to_string();
    let row = settings::row(&state.db).await?;
    let newer = version_compare(&latest, &current) == std::cmp::Ordering::Greater;
    let mut flag = row.get("new_version_available") == Some(&json!(true));
    if flag && !newer {
        settings::update(
            &state.db,
            &[("new_version_available", ColumnValue::Bool(false))],
            state.now(),
        )
        .await?;
        flag = false;
    }
    Ok(json!({
        "available": newer && flag,
        "latest_version": latest,
        "current_version": current,
    }))
}

async fn upgrade_state(State(state): State<AppState>) -> ApiResult {
    Ok(ok(refresh_upgrade_state(&state).await?).into_response())
}

/// `Upgrade::upgrade`: `UpdateCoolify::run(manual_update: true)`.
async fn upgrade(State(state): State<AppState>, Extension(ctx): Extension<AuthCtx>) -> ApiResult {
    if !admin(&state, &ctx).await? {
        return Ok(toast_error(StatusCode::FORBIDDEN, "Forbidden"));
    }
    let Some(jobs) = &state.jobs else {
        return Ok(server_error("Remote execution is not configured."));
    };
    match coolify_jobs::updates::update_coolify(jobs, &state.version, true).await {
        Ok(_) => Ok(ok(json!({})).into_response()),
        Err(message) => Ok(server_error(&message)),
    }
}

/// `Upgrade::getUpgradeStatus`: only root team members can view it.
async fn upgrade_status(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
) -> ApiResult {
    let Some(jobs) = state.jobs.as_ref().filter(|_| ctx.team_id == 0) else {
        return Ok(ok(json!({"status": "none"})).into_response());
    };
    let latest = latest_version(&state);
    Ok(
        ok(coolify_jobs::updates::upgrade_status(jobs, &state.version, &latest).await)
            .into_response(),
    )
}

async fn updates(State(state): State<AppState>, Extension(ctx): Extension<AuthCtx>) -> ApiResult {
    if !admin(&state, &ctx).await? {
        return Ok(to_dashboard());
    }
    if servers::find_by_id(&state.db, 0).await?.is_none() {
        return Err(ApiError::Message(StatusCode::NOT_FOUND, "Not found."));
    }
    let row = settings::row(&state.db).await?;
    Ok(ok(json!({
        "settings": updates_values(&row),
        "autoupdate": autoupdate_env(),
        "upgrade": refresh_upgrade_state(&state).await?,
    }))
    .into_response())
}

/// `registryEnvSyncCommand`: one command per line for the non-root sudo parser.
fn registry_env_sync_command(url: &str) -> Vec<String> {
    let file = concat!("/data/coolify/source/.", "env");
    let sed =
        coolify_jobs::server_ops::shell_arg(&format!("s|^REGISTRY_URL=.*|REGISTRY_URL={url}|"));
    let line = coolify_jobs::server_ops::shell_arg(&format!("REGISTRY_URL={url}"));
    vec![
        format!("if grep -q '^REGISTRY_URL=' {file}; then"),
        format!("    sed -i {sed} {file}"),
        "else".to_owned(),
        format!("    printf '%s\\n' {line} | tee -a {file} > /dev/null"),
        "fi".to_owned(),
    ]
}

/// `refresh_versions_file` with `cdnTries: 1`. The URL is `constants.coolify.versions_url`.
async fn refresh_versions_file(state: &AppState) -> Option<Value> {
    let url = std::env::var("COOLIFY_RS_VERSIONS_URL")
        .unwrap_or_else(|_| "https://cdn.coollabs.io/coolify/versions.json".to_owned());
    let response = reqwest::Client::new()
        .get(&url)
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    let text = response.text().await.ok()?;
    let data: Value = serde_json::from_str(&text).ok()?;
    let _ = std::fs::write(data_versions_path(state), &text);
    Some(data)
}

#[allow(clippy::too_many_lines)]
async fn updates_action(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Json(body): Json<Action>,
) -> ApiResult {
    if !admin(&state, &ctx).await? {
        return Ok(unauthorized());
    }
    let v = body.values;
    match body.action.as_str() {
        "check" => {
            // `checkManually`.
            let Some(versions) = refresh_versions_file(&state).await else {
                return Ok(server_error(
                    "Could not check for updates. Try again later.",
                ));
            };
            let latest = versions
                .pointer("/coolify/v4/version")
                .and_then(Value::as_str)
                .unwrap_or("0.0.0");
            let newer = version_compare(latest, &state.version) == std::cmp::Ordering::Greater;
            settings::update(
                &state.db,
                &[("new_version_available", ColumnValue::Bool(newer))],
                state.now(),
            )
            .await?;
            let upgrade = refresh_upgrade_state(&state).await?;
            return Ok(ok_message(
                if newer {
                    "New version available!"
                } else {
                    "No new version available."
                },
                json!({"upgrade": upgrade}),
            ));
        }
        "submit" | "instant" => {}
        _ => return Err(ApiError::Message(StatusCode::NOT_FOUND, "Not found.")),
    }
    let submit = body.action == "submit";
    let mut field_errors = Map::new();
    let string_blank = |k: &str| text(&v, k).is_none_or(str::is_empty);
    if submit {
        // The `#[Validate]` attributes, in property order.
        if string_blank("update_check_frequency") {
            field_errors.insert(
                "update_check_frequency".into(),
                json!(["The update check frequency field is required."]),
            );
        }
        if !matches!(
            text(&v, "docker_registry_url"),
            Some("docker.io" | "ghcr.io")
        ) {
            field_errors.insert(
                "docker_registry_url".into(),
                json!(["The selected docker registry url is invalid."]),
            );
        }
        if !field_errors.is_empty() {
            return Ok(ok(json!({"errors": field_errors})).into_response());
        }
        let auto_on = boolean(&v, "is_auto_update_enabled");
        let auto_freq = text(&v, "auto_update_frequency").unwrap_or("");
        if auto_on && !coolify_compat::cron::validate_cron_expression(auto_freq) {
            let mut res =
                json!({"error": "Invalid Cron / Human expression for Auto Update Frequency."});
            if auto_freq.is_empty() {
                res["auto_update_frequency"] = json!(random_auto_update_frequency());
            }
            return Ok(ok(res).into_response());
        }
        let check = text(&v, "update_check_frequency").unwrap_or("");
        if !coolify_compat::cron::validate_cron_expression(check) {
            return Ok(ok(
                json!({"error": "Invalid Cron / Human expression for Update Check Frequency."}),
            )
            .into_response());
        }
    }
    // `instantSave`.
    let row = settings::row(&state.db).await?;
    if row.get("is_auto_update_enabled") == Some(&json!(true))
        && string_blank("auto_update_frequency")
    {
        return Ok(ok(json!({"errors": {"auto_update_frequency": ["The auto update frequency field is required."]}})).into_response());
    }
    let Some(registry) = text(&v, "docker_registry_url")
        .filter(|r| matches!(*r, "docker.io" | "ghcr.io"))
        .map(str::to_owned)
    else {
        return Ok(ok(json!({"errors": {"docker_registry_url": ["The selected docker registry url is invalid."]}})).into_response());
    };
    // `syncRegistryUrlToEnv`: the write happens before the save, and a failure stops it.
    if let (Some(jobs), Some(server)) = (
        state.jobs.as_ref(),
        servers::find_by_id(&state.db, 0).await?,
    ) && jobs
        .instant_remote(
            &server,
            &registry_env_sync_command(&registry),
            true,
            "settings.registry",
        )
        .await
        .is_err()
    {
        return Ok(server_error(concat!(
            "Failed to sync REGISTRY_URL to .",
            "env. Settings were not saved."
        )));
    }
    let pairs = vec![
        (
            "auto_update_frequency",
            v.get("auto_update_frequency")
                .cloned()
                .unwrap_or(Value::Null),
        ),
        (
            "update_check_frequency",
            v.get("update_check_frequency")
                .cloned()
                .unwrap_or(Value::Null),
        ),
        (
            "is_auto_update_enabled",
            json!(boolean(&v, "is_auto_update_enabled")),
        ),
        ("docker_registry_url", json!(registry)),
    ];
    let (changed, _) = save(&state, &pairs).await?;
    audit_instance(
        &state,
        &ctx,
        &meta,
        "ui.instance.settings.updated",
        "updates",
        changed,
    )
    .await;
    let proxy_error = if submit {
        setup_instance_proxy(&state).await.err()
    } else {
        None
    };
    let fresh = settings::row(&state.db).await?;
    let mut res = json!({"settings": updates_values(&fresh), "message": "Settings updated!"});
    if let Some(e) = proxy_error {
        res["error"] = json!(e);
    }
    Ok(ok(res).into_response())
}

/// `InstanceSettings::randomAutoUpdateFrequency`.
fn random_auto_update_frequency() -> String {
    use rand::Rng as _;
    format!("{} 0 * * *", rand::thread_rng().gen_range(0..60))
}

// ---- SettingsEmail ----

const EMAIL_KEYS: &[(&str, &str)] = &[
    ("smtpEnabled", "smtp_enabled"),
    ("smtpHost", "smtp_host"),
    ("smtpPort", "smtp_port"),
    ("smtpEncryption", "smtp_encryption"),
    ("smtpUsername", "smtp_username"),
    ("smtpPassword", "smtp_password"),
    ("smtpTimeout", "smtp_timeout"),
    ("smtpEhloDomain", "smtp_ehlo_domain"),
    ("smtpFromAddress", "smtp_from_address"),
    ("smtpFromName", "smtp_from_name"),
    ("resendEnabled", "resend_enabled"),
    ("resendApiKey", "resend_api_key"),
];

fn email_values(state: &AppState, row: &Record) -> Result<Value, ApiError> {
    let mut out = Map::new();
    for (prop, col) in EMAIL_KEYS {
        let v = plain(state, row, col)?;
        // `?string` properties: integers arrive as strings.
        let v = match v {
            Value::Number(n) => json!(n.to_string()),
            other => other,
        };
        out.insert((*prop).to_owned(), v);
    }
    Ok(Value::Object(out))
}

/// `is_transactional_emails_enabled()`.
fn transactional_enabled(row: &Record) -> bool {
    row.get("smtp_enabled") == Some(&json!(true)) || row.get("resend_enabled") == Some(&json!(true))
}

async fn email(State(state): State<AppState>, Extension(ctx): Extension<AuthCtx>) -> ApiResult {
    if !admin(&state, &ctx).await? {
        return Ok(to_dashboard());
    }
    let row = settings::row(&state.db).await?;
    let address: Option<String> = sqlx::query_scalar("SELECT email FROM users WHERE id = $1")
        .bind(ctx.user_id)
        .fetch_optional(state.db.pool())
        .await
        .map_err(|_| ApiError::Internal)?;
    Ok(ok(json!({
        "settings": email_values(&state, &row)?,
        "can_send_test": transactional_enabled(&row) && ctx.is_team_admin(),
        "test_email": address,
    }))
    .into_response())
}

/// The `#[Validate]` attributes of `SettingsEmail`, plus the `ValidHostname` pass of `syncData`.
fn email_rules(v: &Map<String, Value>) -> Vec<String> {
    let mut e = Vec::new();
    if let Some(a) = text(v, "smtpFromAddress").filter(|s| !s.trim().is_empty())
        && !crate::rules::is_email(a)
    {
        e.push("The smtp from address field must be a valid email address.".to_owned());
    }
    if let Some(p) = v.get("smtpPort").filter(|p| !blank(Some(p))) {
        if !numeric(p) {
            e.push("The smtp port field must be a number.".to_owned());
        } else if as_f64(p) < 1.0 {
            e.push("The smtp port field must be at least 1.".to_owned());
        } else if as_f64(p) > 65535.0 {
            e.push("The smtp port field must not be greater than 65535.".to_owned());
        }
    }
    if let Some(enc) = text(v, "smtpEncryption").filter(|s| !s.trim().is_empty())
        && !matches!(enc, "starttls" | "tls" | "none")
    {
        e.push("The selected smtp encryption is invalid.".to_owned());
    }
    if let Some(t) = v.get("smtpTimeout").filter(|p| !blank(Some(p)))
        && !numeric(t)
    {
        e.push("The smtp timeout field must be a number.".to_owned());
    }
    if let Some(a) = text(v, "testEmailAddress").filter(|s| !s.trim().is_empty())
        && !crate::rules::is_email(a)
    {
        e.push("The test email address field must be a valid email address.".to_owned());
    }
    e
}

fn hostname_rule(v: &Map<String, Value>, attr: &str) -> Option<String> {
    let host = v.get("smtpEhloDomain")?;
    if blank(Some(host)) {
        return None;
    }
    crate::rules::valid_hostname(host).map(|m| m.replace(":attribute", attr))
}

/// `validateSmtpSettings`.
fn smtp_rules(v: &Map<String, Value>) -> Vec<String> {
    let mut e = Vec::new();
    match text(v, "smtpFromAddress") {
        None => e.push("From Address is required.".to_owned()),
        Some(s) if s.trim().is_empty() => e.push("From Address is required.".to_owned()),
        Some(a) if !crate::rules::is_email(a) => {
            e.push("Please enter a valid email address.".to_owned());
        }
        _ => {}
    }
    if blank(v.get("smtpFromName")) {
        e.push("From Name is required.".to_owned());
    }
    if blank(v.get("smtpHost")) {
        e.push("SMTP Host is required.".to_owned());
    }
    match v.get("smtpPort") {
        p if blank(p) => e.push("SMTP Port is required.".to_owned()),
        Some(p) if !numeric(p) => e.push("SMTP Port must be a number.".to_owned()),
        _ => {}
    }
    match text(v, "smtpEncryption") {
        None | Some("") => e.push("Encryption type is required.".to_owned()),
        Some(s) if !matches!(s, "starttls" | "tls" | "none") => {
            e.push("The selected smtp encryption is invalid.".to_owned());
        }
        _ => {}
    }
    if let Some(t) = v.get("smtpTimeout").filter(|p| !blank(Some(p)))
        && !numeric(t)
    {
        e.push("The smtp timeout field must be a number.".to_owned());
    }
    if let Some(m) = hostname_rule(v, "smtp ehlo domain") {
        e.push(m);
    }
    e
}

/// `validateResendSettings`.
fn resend_rules(v: &Map<String, Value>) -> Vec<String> {
    let mut e = Vec::new();
    if boolean(v, "resendEnabled") && blank(v.get("resendApiKey")) {
        e.push("Resend API Key is required.".to_owned());
    }
    match text(v, "smtpFromAddress") {
        None => e.push("From Address is required.".to_owned()),
        Some(s) if s.trim().is_empty() => e.push("From Address is required.".to_owned()),
        Some(a) if !crate::rules::is_email(a) => {
            e.push("Please enter a valid email address.".to_owned());
        }
        _ => {}
    }
    if blank(v.get("smtpFromName")) {
        e.push("From Name is required.".to_owned());
    }
    e
}

fn email_pair(v: &Map<String, Value>, prop: &str) -> Value {
    if matches!(prop, "smtpEnabled" | "resendEnabled") {
        json!(boolean(v, prop))
    } else {
        nullable(v, prop)
    }
}

/// The columns of `EMAIL_KEYS` the action assigns, with `overrides` on top.
fn email_pairs(
    v: &Map<String, Value>,
    props: &[&str],
    overrides: &[(&'static str, Value)],
) -> Vec<(&'static str, Value)> {
    let mut out: Vec<(&'static str, Value)> = Vec::new();
    for (prop, col) in EMAIL_KEYS {
        if props.contains(prop) {
            out.push((col, email_pair(v, prop)));
        }
    }
    for (col, val) in overrides {
        out.retain(|(c, _)| c != col);
        out.push((col, val.clone()));
    }
    out
}

const ALL_EMAIL_PROPS: &[&str] = &[
    "smtpEnabled",
    "smtpHost",
    "smtpPort",
    "smtpEncryption",
    "smtpUsername",
    "smtpPassword",
    "smtpTimeout",
    "smtpEhloDomain",
    "smtpFromAddress",
    "smtpFromName",
    "resendEnabled",
    "resendApiKey",
];
const SMTP_PROPS: &[&str] = &[
    "smtpEnabled",
    "smtpHost",
    "smtpPort",
    "smtpEncryption",
    "smtpUsername",
    "smtpPassword",
    "smtpTimeout",
    "smtpEhloDomain",
    "smtpFromAddress",
    "smtpFromName",
];
const RESEND_PROPS: &[&str] = &[
    "resendEnabled",
    "resendApiKey",
    "smtpFromAddress",
    "smtpFromName",
];

#[allow(clippy::too_many_lines)]
async fn email_action(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Json(body): Json<Action>,
) -> ApiResult {
    if !admin(&state, &ctx).await? {
        return Ok(unauthorized());
    }
    let mut v = body.values;
    // Livewire does not hydrate a typed property from null: it keeps its default, here `'starttls'`.
    if v.get("smtpEncryption").is_none_or(Value::is_null) {
        v.insert("smtpEncryption".into(), json!("starttls"));
    }
    let saved = settings::row(&state.db).await?;
    // Sent back on failure: `syncData()` or the property reset of the catch.
    let reset = |row: &Record, m: String, keys: Vec<(&str, Value)>| -> ApiResult {
        let mut res = json!({"error": m, "settings": email_values(&state, row)?});
        for (k, val) in keys {
            res["overrides"][k] = val;
        }
        Ok(ok(res).into_response())
    };
    let run_save = |pairs: Vec<(&'static str, Value)>| {
        let state = state.clone();
        let ctx = ctx.clone();
        let meta = meta.clone();
        async move {
            let (changed, _) = save(&state, &pairs).await?;
            // `auditChangedFields` hides nothing here, but the password and key stay out of the
            // log only by being field names, as upstream.
            audit_instance(
                &state,
                &ctx,
                &meta,
                "ui.settings.email.updated",
                "email",
                changed,
            )
            .await;
            Ok::<_, ApiError>(())
        }
    };
    let message: Option<&str>;
    match body.action.as_str() {
        "submit" => {
            let mut e = email_rules(&v);
            if e.is_empty()
                && let Some(m) = hostname_rule(&v, "smtp ehlo domain")
            {
                e.push(m);
            }
            if let Some(m) = summarize(&e) {
                return Ok(toast_error(StatusCode::UNPROCESSABLE_ENTITY, m));
            }
            run_save(email_pairs(&v, ALL_EMAIL_PROPS, &[])).await?;
            message = Some("Transactional email settings updated.");
        }
        "instantSmtp" => {
            // `instantSave('SMTP')`: `submitSmtp`, whose own catch keeps the page going, then
            // Resend off and a second save.
            let e = smtp_rules(&v);
            if let Some(m) = summarize(&e) {
                // `submitSmtp` swallows the error with a toast and turns SMTP off in the page;
                // the outer save then writes Resend off and the rest of `$settings` unchanged.
                run_save(vec![("resend_enabled", json!(false))]).await?;
                let fresh = settings::row(&state.db).await?;
                return reset(
                    &fresh,
                    m,
                    vec![
                        ("smtpEnabled", json!(false)),
                        ("resendEnabled", json!(false)),
                    ],
                );
            }
            let on = boolean(&v, "smtpEnabled");
            let mut pairs = email_pairs(&v, SMTP_PROPS, &[]);
            if on {
                pairs.push(("resend_enabled", json!(false)));
            }
            run_save(pairs).await?;
            run_save(vec![("resend_enabled", json!(false))]).await?;
            v.insert("resendEnabled".into(), json!(false));
            message = Some("SMTP settings updated.");
        }
        "instantResend" => {
            let e = resend_rules(&v);
            if let Some(m) = summarize(&e) {
                run_save(vec![("smtp_enabled", json!(false))]).await?;
                let fresh = settings::row(&state.db).await?;
                return reset(
                    &fresh,
                    m,
                    vec![
                        ("resendEnabled", json!(false)),
                        ("smtpEnabled", json!(false)),
                    ],
                );
            }
            let on = boolean(&v, "resendEnabled");
            let mut pairs = email_pairs(&v, RESEND_PROPS, &[]);
            if on {
                pairs.push(("smtp_enabled", json!(false)));
            }
            run_save(pairs).await?;
            run_save(vec![("smtp_enabled", json!(false))]).await?;
            v.insert("smtpEnabled".into(), json!(false));
            message = Some("Resend settings updated.");
        }
        "test" => {
            // `sendTestEmail`.
            let mut e = Vec::new();
            match text(&v, "testEmailAddress") {
                None => e.push("Test email address is required.".to_owned()),
                Some(s) if s.trim().is_empty() => {
                    e.push("Test email address is required.".to_owned());
                }
                Some(a) if !crate::rules::is_email(a) => {
                    e.push("Please enter a valid email address.".to_owned());
                }
                _ => {}
            }
            match text(&v, "smtpFromAddress") {
                None => e.push("From Address is required.".to_owned()),
                Some(s) if s.trim().is_empty() => e.push("From Address is required.".to_owned()),
                Some(a) if !crate::rules::is_email(a) => {
                    e.push("Please enter a valid email address.".to_owned());
                }
                _ => {}
            }
            if blank(v.get("smtpFromName")) {
                e.push("From Name is required.".to_owned());
            }
            if let Some(m) = summarize(&e) {
                return Ok(toast_error(StatusCode::UNPROCESSABLE_ENTITY, m));
            }
            run_save(email_pairs(&v, &["smtpFromAddress", "smtpFromName"], &[])).await?;
            // `RateLimiter::attempt('test-email:'.$team->id, 0, ..., 10)`.
            let key = format!("test-email:{}", ctx.team_id);
            if crate::rules::throttle(&key, 1, Duration::from_secs(10)).is_some() {
                return Ok(server_error("Too many messages sent!"));
            }
            let to = text(&v, "testEmailAddress").unwrap_or_default().to_owned();
            if let Err(m) = coolify_jobs::notifications::send_transactional(
                &state.db,
                state.encrypter.as_ref(),
                &to,
                Mail::Test,
            )
            .await
            {
                return Ok(server_error(m));
            }
            message = Some("Test Email sent.");
        }
        _ => return Err(ApiError::Message(StatusCode::NOT_FOUND, "Not found.")),
    }
    let _ = saved;
    let row = settings::row(&state.db).await?;
    let mut res = json!({
        "settings": email_values(&state, &row)?,
        "can_send_test": transactional_enabled(&row) && ctx.is_team_admin(),
    });
    if let Some(m) = message {
        res["message"] = json!(m);
    }
    Ok(ok(res).into_response())
}

// ---- SettingsBackup ----

const BACKUP_EDIT_FIELDS: &[&str] = &[
    "enabled",
    "frequency",
    "database_backup_retention_amount_locally",
    "database_backup_retention_days_locally",
    "database_backup_retention_max_storage_locally",
    "database_backup_retention_amount_s3",
    "database_backup_retention_days_s3",
    "database_backup_retention_max_storage_s3",
    "save_s3",
    "disable_local_backup",
    "databases_to_backup",
    "dump_all",
    "timeout",
    "missing_backup_notification_days",
];

/// `Server::isFunctional()`.
fn server_functional(s: &servers::ServerRow) -> bool {
    let settings = s.settings.clone().unwrap_or_default();
    let flag = |k: &str| settings.get(k).and_then(Value::as_bool).unwrap_or(false);
    let transferred = s
        .record
        .get("server_metadata")
        .and_then(|m| m.pointer("/transfer/status"))
        .and_then(Value::as_str)
        == Some("transferred");
    flag("is_reachable") && flag("is_usable") && !flag("force_disabled") && !transferred
}

async fn backup_row(state: &AppState) -> Result<Option<Record>, ApiError> {
    let t: Option<String> = sqlx::query_scalar(
        "SELECT row_to_json(b)::text FROM scheduled_database_backups b
         WHERE database_id = 0 AND database_type = 'App\\Models\\StandalonePostgresql' ORDER BY id LIMIT 1",
    )
    .fetch_optional(state.db.pool())
    .await
    .map_err(|_| ApiError::Internal)?;
    t.map(|t| serde_json::from_str(&t).map_err(|_| ApiError::Internal))
        .transpose()
}

async fn database_zero(state: &AppState) -> Result<Option<Record>, ApiError> {
    let t: Option<String> = sqlx::query_scalar(
        "SELECT row_to_json(d)::text FROM standalone_postgresqls d WHERE id = 0",
    )
    .fetch_optional(state.db.pool())
    .await
    .map_err(|_| ApiError::Internal)?;
    t.map(|t| serde_json::from_str(&t).map_err(|_| ApiError::Internal))
        .transpose()
}

/// `selectedS3Storages`.
pub(crate) async fn selected_storages(
    state: &AppState,
    backup_id: i64,
) -> Result<Vec<i64>, ApiError> {
    sqlx::query_scalar(
        "SELECT s3_storage_id FROM scheduled_database_backup_s3_storage WHERE scheduled_database_backup_id = $1 ORDER BY s3_storage_id",
    )
    .bind(backup_id)
    .fetch_all(state.db.pool())
    .await
    .map_err(|_| ApiError::Internal)
}

/// `availableS3StorageIds`.
pub(crate) async fn available_storages(
    state: &AppState,
    team_id: i64,
    selected: &[i64],
) -> Result<Vec<(i64, String, bool)>, ApiError> {
    sqlx::query_as(
        "SELECT id, name, is_usable FROM s3_storages WHERE team_id = $2 AND (is_usable = true OR id = ANY($1)) ORDER BY name",
    )
    .bind(selected)
    .bind(team_id)
    .fetch_all(state.db.pool())
    .await
    .map_err(|_| ApiError::Internal)
}

async fn backup_payload(state: &AppState, row: &Record) -> Result<Value, ApiError> {
    let server = servers::find_by_id(&state.db, 0).await?;
    let functional = server.as_ref().is_some_and(server_functional);
    let database = database_zero(state).await?;
    let backup = backup_row(state).await?;
    let mut res = json!({
        "server_uuid": server.as_ref().and_then(|s| s.record.get("uuid")),
        "functional": functional,
        "is_backup_before_update_enabled": row.get("is_backup_before_update_enabled"),
        "database": null,
        "backup": null,
    });
    if let Some(d) = &database {
        res["database"] = json!({
            "name": d.get("name"),
            "description": d.get("description"),
            "uuid": d.get("uuid"),
            "postgres_user": d.get("postgres_user"),
            "status": d.get("status"),
        });
    }
    if let Some(b) = &backup {
        let id = b.get("id").and_then(Value::as_i64).unwrap_or(0);
        let selected = selected_storages(state, id).await?;
        let available = available_storages(state, 0, &selected).await?;
        let mut form = Map::new();
        for k in BACKUP_EDIT_FIELDS {
            form.insert((*k).to_owned(), b.get(*k).cloned().unwrap_or(Value::Null));
        }
        let ids: Vec<i64> = if selected.is_empty() {
            available.iter().take(1).map(|s| s.0).collect()
        } else {
            selected.clone()
        };
        form.insert("s3_storage_ids".into(), json!(ids));
        res["backup"] = json!({
            "uuid": b.get("uuid"),
            "form": form,
            "storages": available.iter().map(|(id, name, usable)| json!({"id": id, "name": name, "is_usable": usable})).collect::<Vec<_>>(),
        });
    }
    Ok(res)
}

async fn backup(State(state): State<AppState>, Extension(ctx): Extension<AuthCtx>) -> ApiResult {
    if !admin(&state, &ctx).await? {
        return Ok(to_dashboard());
    }
    // `mount`: the instance database always counts as running, and an unusable localhost cannot
    // keep a schedule enabled.
    if let Some(d) = database_zero(&state).await?
        && d.get("status").and_then(Value::as_str) != Some("running")
    {
        sqlx::query(
            "UPDATE standalone_postgresqls SET status = 'running', updated_at = $1 WHERE id = 0",
        )
        .bind(state.now())
        .execute(state.db.pool())
        .await
        .map_err(|_| ApiError::Internal)?;
    }
    let server = servers::find_by_id(&state.db, 0).await?;
    let functional = server.as_ref().is_some_and(server_functional);
    if !functional
        && let Some(b) = backup_row(&state).await?
        && b.get("enabled") == Some(&json!(true))
    {
        sqlx::query(
            "UPDATE scheduled_database_backups SET enabled = false, updated_at = $1 WHERE id = $2",
        )
        .bind(state.now())
        .bind(b.get("id").and_then(Value::as_i64))
        .execute(state.db.pool())
        .await
        .map_err(|_| ApiError::Internal)?;
    }
    let row = settings::row(&state.db).await?;
    Ok(ok(backup_payload(&state, &row).await?).into_response())
}

/// `BackupExecutions`: the rows of the instance schedule.
async fn backup_executions(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> ApiResult {
    if !admin(&state, &ctx).await? {
        return Ok(unauthorized());
    }
    let Some(b) = backup_row(&state).await? else {
        return Ok(ok(json!({"executions": [], "total": 0})).into_response());
    };
    let id = b.get("id").and_then(Value::as_i64).unwrap_or(0);
    let skip = q
        .get("skip")
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(0);
    let total: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM scheduled_database_backup_executions WHERE scheduled_database_backup_id = $1",
    )
    .bind(id)
    .fetch_one(state.db.pool())
    .await
    .map_err(|_| ApiError::Internal)?;
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT row_to_json(e)::text FROM scheduled_database_backup_executions e WHERE scheduled_database_backup_id = $1 ORDER BY created_at DESC, id DESC OFFSET $2 LIMIT 10",
    )
    .bind(id)
    .bind(skip)
    .fetch_all(state.db.pool())
    .await
    .map_err(|_| ApiError::Internal)?;
    let list: Vec<Value> = rows
        .iter()
        .filter_map(|r| serde_json::from_str(r).ok())
        .collect();
    Ok(ok(json!({"executions": list, "total": total})).into_response())
}

/// `format_docker_envs_to_json`.
fn docker_envs(out: &str) -> Map<String, Value> {
    let parsed: Value = serde_json::from_str(out).unwrap_or(Value::Null);
    let mut m = Map::new();
    if let Some(list) = parsed.pointer("/0/Config/Env").and_then(Value::as_array) {
        for e in list.iter().filter_map(Value::as_str) {
            if let Some((k, v)) = e.split_once('=') {
                m.insert(k.to_owned(), json!(v));
            }
        }
    }
    m
}

fn int_field(v: &Map<String, Value>, k: &str) -> Option<i64> {
    match v.get(k) {
        Some(Value::Number(n)) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        Some(Value::String(s)) => s.trim().parse::<f64>().ok().map(|f| f as i64),
        _ => None,
    }
}

/// `BackupEdit::customValidate` and the `#[Validate]` attributes it runs.
fn backup_rules(v: &Map<String, Value>) -> Vec<String> {
    let mut e = Vec::new();
    for (k, attr) in [
        (
            "database_backup_retention_amount_locally",
            "database backup retention amount locally",
        ),
        (
            "database_backup_retention_days_locally",
            "database backup retention days locally",
        ),
        (
            "database_backup_retention_amount_s3",
            "database backup retention amount s3",
        ),
        (
            "database_backup_retention_days_s3",
            "database backup retention days s3",
        ),
    ] {
        match v.get(k) {
            x if blank(x) => e.push(format!("The {attr} field is required.")),
            Some(x) if !numeric(x) || as_f64(x).fract() != 0.0 => {
                e.push(format!("The {attr} field must be an integer."));
            }
            _ => {}
        }
    }
    for (k, attr) in [
        (
            "database_backup_retention_max_storage_locally",
            "database backup retention max storage locally",
        ),
        (
            "database_backup_retention_max_storage_s3",
            "database backup retention max storage s3",
        ),
    ] {
        match v.get(k) {
            x if blank(x) => e.push(format!("The {attr} field is required.")),
            Some(x) if !numeric(x) => e.push(format!("The {attr} field must be a number.")),
            Some(x) if as_f64(x) < 0.0 => e.push(format!("The {attr} field must be at least 0.")),
            _ => {}
        }
    }
    match v.get("timeout") {
        x if blank(x) => e.push("The timeout field is required.".to_owned()),
        Some(x) if !numeric(x) || as_f64(x).fract() != 0.0 => {
            e.push("The timeout field must be an integer.".to_owned());
        }
        Some(x) if as_f64(x) < 60.0 => e.push("The timeout field must be at least 60.".to_owned()),
        Some(x) if as_f64(x) > 36000.0 => {
            e.push("The timeout field must not be greater than 36000.".to_owned());
        }
        _ => {}
    }
    match v.get("missing_backup_notification_days") {
        x if blank(x) => {
            e.push("The missing backup notification days field is required.".to_owned());
        }
        Some(x) if !numeric(x) || as_f64(x).fract() != 0.0 => {
            e.push("The missing backup notification days field must be an integer.".to_owned());
        }
        Some(x) if as_f64(x) < 0.0 => {
            e.push("The missing backup notification days field must be at least 0.".to_owned());
        }
        Some(x) if as_f64(x) > 365.0 => {
            e.push(
                "The missing backup notification days field must not be greater than 365."
                    .to_owned(),
            );
        }
        _ => {}
    }
    e
}

/// `BackupEdit::syncData(true)`, `customValidate`, `syncS3Storages` and the audit event.
pub(crate) async fn save_backup(
    state: &AppState,
    ctx: &AuthCtx,
    meta: &Meta,
    b: &Record,
    v: &Map<String, Value>,
    team_id: i64,
    database: Option<(&str, &str)>,
) -> Result<Result<(), String>, ApiError> {
    let id = b.get("id").and_then(Value::as_i64).unwrap_or(0);
    let selected_now = selected_storages(state, id).await?;
    let available = available_storages(state, team_id, &selected_now).await?;
    let wanted: Vec<i64> = {
        let mut out: Vec<i64> = Vec::new();
        for raw in v
            .get("s3_storage_ids")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
        {
            let n = raw
                .as_i64()
                .or_else(|| raw.as_str().and_then(|s| s.parse().ok()));
            if let Some(n) = n
                && available.iter().any(|a| a.0 == n)
                && !out.contains(&n)
            {
                out.push(n);
            }
        }
        out
    };
    let mut save_s3 = boolean(v, "save_s3");
    let mut disable_local = boolean(v, "disable_local_backup");
    if available.is_empty() {
        save_s3 = false;
    } else if save_s3 && wanted.is_empty() {
        return Ok(Err("Select at least one usable S3 storage.".to_owned()));
    }
    if disable_local && !save_s3 {
        disable_local = false;
    }
    let frequency = text(v, "frequency").unwrap_or("");
    if !coolify_compat::cron::validate_cron_expression(frequency) {
        return Ok(Err("Invalid Cron / Human expression".to_owned()));
    }
    if let Some(m) = summarize(&backup_rules(v)) {
        return Ok(Err(m));
    }
    let dbs = nullable(v, "databases_to_backup");
    if let Some(s) = dbs.as_str()
        && let Err(m) = crate::routes::database_backups::validate_databases_input(s)
    {
        return Ok(Err(m));
    }
    let mut changed: Vec<String> = Vec::new();
    let mark = |k: &str, new: &Value, changed: &mut Vec<String>| {
        let old = b.get(k).cloned().unwrap_or(Value::Null);
        if !loose_eq(&old, new) {
            changed.push(k.to_owned());
        }
    };
    let enabled = boolean(v, "enabled");
    for (k, val) in [
        ("enabled", json!(enabled)),
        ("frequency", json!(frequency)),
        ("save_s3", json!(save_s3)),
        ("databases_to_backup", dbs.clone()),
        ("timeout", json!(int_field(v, "timeout"))),
        ("dump_all", json!(boolean(v, "dump_all"))),
        ("disable_local_backup", json!(disable_local)),
        (
            "missing_backup_notification_days",
            json!(int_field(v, "missing_backup_notification_days")),
        ),
    ] {
        mark(k, &val, &mut changed);
    }
    for k in [
        "database_backup_retention_amount_locally",
        "database_backup_retention_days_locally",
        "database_backup_retention_amount_s3",
        "database_backup_retention_days_s3",
        "database_backup_retention_max_storage_locally",
        "database_backup_retention_max_storage_s3",
    ] {
        mark(k, v.get(k).unwrap_or(&Value::Null), &mut changed);
    }
    let order: Vec<String> = b.keys().filter(|k| changed.contains(k)).cloned().collect();
    state.db.ensure_writable()?;
    sqlx::query(
        "UPDATE scheduled_database_backups SET enabled=$1, frequency=$2, save_s3=$3, databases_to_backup=$4, timeout=$5, dump_all=$6, disable_local_backup=$7, missing_backup_notification_days=$8,
           database_backup_retention_amount_locally=$9, database_backup_retention_days_locally=$10, database_backup_retention_amount_s3=$11, database_backup_retention_days_s3=$12,
           database_backup_retention_max_storage_locally=$13, database_backup_retention_max_storage_s3=$14, updated_at=$15 WHERE id=$16",
    )
    .bind(enabled)
    .bind(frequency)
    .bind(save_s3)
    .bind(dbs.as_str())
    .bind(int_field(v, "timeout").unwrap_or(3600) as i32)
    .bind(boolean(v, "dump_all"))
    .bind(disable_local)
    .bind(int_field(v, "missing_backup_notification_days").unwrap_or(0) as i32)
    .bind(int_field(v, "database_backup_retention_amount_locally").unwrap_or(0) as i32)
    .bind(int_field(v, "database_backup_retention_days_locally").unwrap_or(0) as i32)
    .bind(int_field(v, "database_backup_retention_amount_s3").unwrap_or(0) as i32)
    .bind(int_field(v, "database_backup_retention_days_s3").unwrap_or(0) as i32)
    .bind(v.get("database_backup_retention_max_storage_locally").map_or(0.0, as_f64))
    .bind(v.get("database_backup_retention_max_storage_s3").map_or(0.0, as_f64))
    .bind(state.now())
    .bind(id)
    .execute(state.db.pool())
    .await
    .map_err(|_| ApiError::Internal)?;
    let mut changed = order;
    let mut sorted = wanted.clone();
    sorted.sort_unstable();
    if selected_now != sorted {
        changed.push("s3_storages".to_owned());
        coolify_db::database_parts::sync_backup_storages(
            &state.db,
            id,
            b.get("s3_storage_id").and_then(Value::as_i64),
            &sorted,
            state.now(),
        )
        .await?;
    }
    if !changed.is_empty() {
        let (uuid, name) = match database {
            Some((u, n)) => (json!(u), json!(n)),
            None => {
                let d = database_zero(state).await?.unwrap_or_default();
                (
                    d.get("uuid").cloned().unwrap_or(Value::Null),
                    d.get("name").cloned().unwrap_or(Value::Null),
                )
            }
        };
        let actor = crate::audit::actor(state, ctx, meta).await;
        crate::audit::log_as(
            state,
            Some(team_id),
            &actor,
            "ui.database.backup_schedule_updated",
            json!({
                "team_id": team_id,
                "database_uuid": uuid,
                "database_name": name,
                "backup_uuid": b.get("uuid"),
                "changed_fields": changed,
            }),
        )
        .await;
    }
    Ok(Ok(()))
}

#[allow(clippy::too_many_lines)]
async fn backup_action(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Json(body): Json<Action>,
) -> ApiResult {
    if !admin(&state, &ctx).await? {
        return Ok(unauthorized());
    }
    let v = body.values;
    let mut message = None;
    match body.action.as_str() {
        "configure" => {
            // `addCoolifyDatabase`.
            let mut database = database_zero(&state).await?;
            if database.is_none() {
                let Some(jobs) = state.jobs.as_ref() else {
                    return Err(ApiError::NotPorted);
                };
                let Some(server) = servers::find_by_id(&state.db, 0).await? else {
                    return Err(ApiError::Message(StatusCode::NOT_FOUND, "Not found."));
                };
                let out = match jobs
                    .instant_remote(
                        &server,
                        &["docker inspect coolify-db".to_owned()],
                        true,
                        "settings.backup",
                    )
                    .await
                {
                    Ok(o) => o.unwrap_or_default(),
                    Err(m) => return Ok(server_error(m)),
                };
                let envs = docker_envs(&out);
                let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
                let password = envs
                    .get("POSTGRES_PASSWORD")
                    .and_then(Value::as_str)
                    .map(|p| enc.encrypt_string(p));
                sqlx::query(
                    "INSERT INTO standalone_postgresqls (id, uuid, name, description, postgres_user, postgres_password, postgres_db, status, destination_type, destination_id, created_at, updated_at)
                     VALUES (0, $1, 'coolify-db', 'Coolify database', $2, $3, $4, 'running', 'App\\Models\\StandaloneDocker', 0, $5, $5)",
                )
                .bind(coolify_domain::new_public_id())
                .bind(envs.get("POSTGRES_USER").and_then(Value::as_str))
                .bind(password)
                .bind(envs.get("POSTGRES_DB").and_then(Value::as_str))
                .bind(state.now())
                .execute(state.db.pool())
                .await
                .map_err(|_| ApiError::Internal)?;
                database = database_zero(&state).await?;
            }
            if backup_row(&state).await?.is_none() {
                let uuid = coolify_domain::new_public_id();
                let d = database.clone().unwrap_or_default();
                sqlx::query(
                    "INSERT INTO scheduled_database_backups (uuid, enabled, save_s3, frequency, database_id, database_type, team_id, created_at, updated_at)
                     VALUES ($1, true, false, '0 0 * * *', 0, 'App\\Models\\StandalonePostgresql', 0, $2, $2)",
                )
                .bind(&uuid)
                .bind(state.now())
                .execute(state.db.pool())
                .await
                .map_err(|_| ApiError::Internal)?;
                let actor = crate::audit::actor(&state, &ctx, &meta).await;
                crate::audit::log_as(
                    &state,
                    Some(0),
                    &actor,
                    "ui.database.backup_schedule_created",
                    json!({
                        "team_id": 0,
                        "database_uuid": d.get("uuid"),
                        "database_name": d.get("name"),
                        "backup_uuid": uuid,
                        "frequency": "0 0 * * *",
                        "save_s3": false,
                    }),
                )
                .await;
            }
        }
        "instant" => {
            // `instantSave`.
            let pairs = vec![(
                "is_backup_before_update_enabled",
                json!(boolean(&v, "is_backup_before_update_enabled")),
            )];
            let (changed, _) = save(&state, &pairs).await?;
            audit_instance(
                &state,
                &ctx,
                &meta,
                "ui.instance.settings.updated",
                "backup",
                changed,
            )
            .await;
            message = Some("Backup updated.".to_owned());
        }
        "submit" => {
            // Credentials come from the coolify-db container and are display-only.
            sqlx::query("UPDATE standalone_postgresqls SET description = $1, updated_at = $2 WHERE id = 0 AND description IS DISTINCT FROM $1")
                .bind(nullable(&v, "description").as_str())
                .bind(state.now())
                .execute(state.db.pool())
                .await
                .map_err(|_| ApiError::Internal)?;
            message = Some("Backup updated.".to_owned());
        }
        "toggleEnabled" => {
            // `toggleDatabaseBackup`: an empty `changed_fields` is still logged.
            let Some(b) = backup_row(&state).await? else {
                return Err(ApiError::Message(StatusCode::NOT_FOUND, "Not found."));
            };
            let on = b.get("enabled") != Some(&json!(true));
            sqlx::query(
                "UPDATE scheduled_database_backups SET enabled = $1, updated_at = $2 WHERE id = $3",
            )
            .bind(on)
            .bind(state.now())
            .bind(b.get("id").and_then(Value::as_i64))
            .execute(state.db.pool())
            .await
            .map_err(|_| ApiError::Internal)?;
            let d = database_zero(&state).await?.unwrap_or_default();
            let actor = crate::audit::actor(&state, &ctx, &meta).await;
            crate::audit::log_as(
                &state,
                Some(0),
                &actor,
                "ui.database.backup_schedule_updated",
                json!({
                    "team_id": 0,
                    "database_uuid": d.get("uuid"),
                    "database_name": d.get("name"),
                    "backup_uuid": b.get("uuid"),
                    "changed_fields": ["enabled"],
                }),
            )
            .await;
            message = Some(
                if on {
                    "Backup enabled."
                } else {
                    "Backup disabled."
                }
                .to_owned(),
            );
        }
        "edit" | "toggleS3" => {
            let Some(b) = backup_row(&state).await? else {
                return Err(ApiError::Message(StatusCode::NOT_FOUND, "Not found."));
            };
            let mut v = v;
            if body.action == "toggleS3" {
                let id = b.get("id").and_then(Value::as_i64).unwrap_or(0);
                let selected_now = selected_storages(&state, id).await?;
                let available = available_storages(&state, 0, &selected_now).await?;
                let on = boolean(&v, "save_s3");
                let picked = v
                    .get("s3_storage_ids")
                    .and_then(Value::as_array)
                    .is_some_and(|a| {
                        a.iter().any(|x| {
                            x.as_i64()
                                .or_else(|| x.as_str().and_then(|s| s.parse().ok()))
                                .is_some_and(|n| available.iter().any(|s| s.0 == n))
                        })
                    });
                if !on && !picked {
                    return Ok(server_error(
                        "Select a usable S3 storage before enabling S3 backups.",
                    ));
                }
                v.insert("save_s3".into(), json!(!on));
                let keep = !on && boolean(&v, "disable_local_backup");
                v.insert("disable_local_backup".into(), json!(keep));
            }
            match save_backup(&state, &ctx, &meta, &b, &v, 0, None).await? {
                Ok(()) => message = Some("Backup updated successfully.".to_owned()),
                Err(m) => return Ok(server_error(m)),
            }
        }
        "backupNow" => {
            let Some(b) = backup_row(&state).await? else {
                return Err(ApiError::Message(StatusCode::NOT_FOUND, "Not found."));
            };
            // `queueDatabaseBackup`: the instance database counts as running; stays on the page.
            let Some(jobs) = state.jobs.as_ref() else {
                return Err(ApiError::NotPorted);
            };
            jobs.spawn_database_backup(b.get("id").and_then(Value::as_i64).unwrap_or(0));
            let d = database_zero(&state).await?.unwrap_or_default();
            let actor = crate::audit::actor(&state, &ctx, &meta).await;
            crate::audit::log_as(
                &state,
                Some(0),
                &actor,
                "ui.database.backup_started",
                json!({
                    "team_id": 0,
                    "database_uuid": d.get("uuid"),
                    "database_name": d.get("name"),
                    "backup_uuid": b.get("uuid"),
                }),
            )
            .await;
            message = Some("Backup queued. It will be available in a few minutes.".to_owned());
        }
        _ => return Err(ApiError::Message(StatusCode::NOT_FOUND, "Not found.")),
    }
    let row = settings::row(&state.db).await?;
    let mut res = backup_payload(&state, &row).await?;
    if let Some(m) = message {
        res["message"] = json!(m);
    }
    Ok(ok(res).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowlist_dedup() {
        let out = deduplicate_allowlist(vec![
            "10.0.0.0/8".into(),
            "10.1.2.3".into(),
            "192.168.1.1".into(),
        ]);
        assert_eq!(out, vec!["10.0.0.0/8".to_owned(), "192.168.1.1".to_owned()]);
    }

    #[test]
    fn dns_and_ip_rules() {
        assert!(valid_dns_servers("1.1.1.1, 8.8.8.8").is_none());
        assert!(valid_dns_servers("nope").is_some());
        assert!(valid_ip_or_cidr("10.0.0.0/8,::1/128").is_none());
        assert!(valid_ip_or_cidr("10.0.0.0/33").is_some());
        assert!(valid_webhook_entry("hooks.company.local"));
        assert!(!valid_webhook_entry("-bad-.local"));
    }

    #[test]
    fn php_loose_equality() {
        assert!(loose_eq(&Value::Null, &json!("")));
        assert!(loose_eq(&Value::Null, &json!([])));
        assert!(loose_eq(&json!(false), &Value::Null));
        assert!(loose_eq(&json!(587), &json!("587")));
        assert!(!loose_eq(&json!("a"), &json!("b")));
    }
}
