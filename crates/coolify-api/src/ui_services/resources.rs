//! `Project\Service\Index` (a compose application or database of a service), and the restart of
//! `Project\Service\ResourceCard` and removal of `Project\Service\Heading`.

use axum::Extension;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use coolify_db::servers::ColumnValue;
use coolify_db::services::{self as svc, ServiceRow, SubKind, SubRow};
use coolify_jobs::applications::domains as app_domains;
use coolify_jobs::services::templates;
use serde_json::{Map, Value, json};

use super::{NOT_FOUND, database_type, find_service, jobs_of, require_admin, sub_json, toast};
use crate::auth::AuthCtx;
use crate::response::{ApiError, ApiResult, ok};
use crate::state::AppState;

const RESOLVER_NONE: &str = "none";

/// Finds the stack service by uuid: `applications()->whereUuid()` first, then `databases()`.
async fn find_part(state: &AppState, id: i64, uuid: &str) -> Result<SubRow, ApiError> {
    if let Some(a) = svc::sub_by_uuid(&state.db, SubKind::Application, id, uuid).await? {
        return Ok(a);
    }
    svc::sub_by_uuid(&state.db, SubKind::Database, id, uuid)
        .await?
        .ok_or(NOT_FOUND)
}

/// `$server->proxyType()` and `$server->traefikCertificateResolver()`.
async fn proxy_facts(
    state: &AppState,
    s: &ServiceRow,
) -> Result<(bool, String, bool, String), ApiError> {
    let Some(id) = s.server_id() else {
        return Ok((false, RESOLVER_NONE.to_owned(), false, String::new()));
    };
    let Some(server) = coolify_db::servers::find_by_id(&state.db, id).await? else {
        return Ok((false, RESOLVER_NONE.to_owned(), false, String::new()));
    };
    let is_traefik = server
        .record
        .get("proxy")
        .and_then(|p| p.get("type"))
        .and_then(Value::as_str)
        .is_some_and(|t| t.eq_ignore_ascii_case("traefik"));
    let settings = server.settings.clone().unwrap_or_default();
    let resolver = settings
        .get("traefik_certificate_resolver")
        .and_then(Value::as_str)
        .filter(|r| !r.trim().is_empty())
        .unwrap_or("letsencrypt")
        .to_owned();
    let drain =
        coolify_jobs::compose_parser::service::is_log_drain_enabled(&Value::Object(settings));
    let ip = server
        .record
        .get("ip")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    Ok((is_traefik, resolver, drain, ip))
}

/// `ServiceApplication::url` (`fqdn` with the overrides applied), as the form edits it.
fn editable_urls(sub: &SubRow) -> Value {
    crate::routes::services::url_attribute(sub)
}

/// `$serviceApplication->serviceType()?->contains(str($image)->before(':'))`.
fn hides_domains(sub: &SubRow) -> bool {
    coolify_jobs::compose_parser::service_parser::service_type(sub.str("image")).is_some()
}

/// `ServiceDatabase::getServiceDatabaseUrl()`.
fn public_url(sub: &SubRow, ip: &str) -> Value {
    if !sub.bool("is_public") {
        return Value::Null;
    }
    let host = if ip.contains(':') {
        format!("[{ip}]")
    } else {
        ip.to_owned()
    };
    json!(format!(
        "{host}:{}",
        sub.int("public_port").unwrap_or_default()
    ))
}

/// `countDomains($fqdn)`.
fn count_domains(v: &Value) -> usize {
    v.as_str()
        .unwrap_or_default()
        .split(',')
        .filter(|d| !d.trim().is_empty())
        .count()
}

/// What `Index::mount` and the blade read.
pub async fn show(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((uuid, resource)): Path<(String, String)>,
) -> ApiResult {
    let s = find_service(&state, &ctx, &uuid).await?;
    let part = find_part(&state, s.id, &resource).await?;
    let (is_traefik, resolver, _, ip) = proxy_facts(&state, &s).await?;
    let raw = |k: &str| part.raw.get(k).cloned().unwrap_or(Value::Null);
    let mut out = sub_json(&part);
    let Value::Object(m) = &mut out else {
        return Err(ApiError::Internal);
    };
    m.insert(
        "kind".into(),
        json!(if part.is_application() {
            "application"
        } else {
            "database"
        }),
    );
    m.insert(
        "exclude_from_status".into(),
        json!(part.bool("exclude_from_status")),
    );
    m.insert(
        "is_removed_from_compose".into(),
        json!(part.bool("is_removed_from_compose")),
    );
    m.insert(
        "is_log_drain_enabled".into(),
        json!(part.bool("is_log_drain_enabled")),
    );
    if part.is_application() {
        let url = editable_urls(&part);
        let stored = part.str("traefik_certificate_resolver");
        let (mode, name) = match stored {
            None | Some("") => ("server", Value::Null),
            Some(RESOLVER_NONE) => ("none", Value::Null),
            Some(custom) => ("custom", json!(custom)),
        };
        m.insert("domain_count".into(), json!(count_domains(&url)));
        m.insert("url".into(), url);
        m.insert("hides_domains".into(), json!(hides_domains(&part)));
        m.insert(
            "is_gzip_enabled".into(),
            json!(part.bool_or("is_gzip_enabled", true)),
        );
        m.insert(
            "is_stripprefix_enabled".into(),
            json!(part.bool_or("is_stripprefix_enabled", true)),
        );
        m.insert("certificate_resolver_mode".into(), json!(mode));
        m.insert("certificate_resolver_name".into(), name);
        m.insert("show_certificate_resolver".into(), json!(is_traefik));
        m.insert("server_certificate_resolver".into(), json!(resolver));
        m.insert(
            "required_port".into(),
            json!(templates::required_port(s.str("service_type"))),
        );
    } else {
        m.insert("is_public".into(), json!(part.bool("is_public")));
        m.insert("public_port".into(), raw("public_port"));
        m.insert("public_port_timeout".into(), raw("public_port_timeout"));
        m.insert("db_url_public".into(), public_url(&part, &ip));
    }
    Ok(ok(json!({
        "service": {"uuid": s.uuid, "name": s.name()},
        "resource": out,
        "can_update": ctx.is_team_admin(),
    }))
    .into_response())
}

fn errors_one(key: &str, message: String) -> ApiError {
    let mut m = Map::new();
    m.insert(key.to_owned(), json!([message]));
    ApiError::Validation(m)
}

/// `ServiceApplication::extractPortFromUrl($fqdn)`.
fn port_of_url(url: &str) -> Option<i64> {
    let with_scheme = if url.starts_with("http://") || url.starts_with("https://") {
        url.to_owned()
    } else {
        format!("http://{url}")
    };
    app_domains::UrlParts::parse(&with_scheme).and_then(|p| p.port_number())
}

/// `ServiceApplication::portRequiresConfirmation($fqdn, $requiredPort)`: true when saving this
/// URL should confirm that it does not use the required template port. A domain without a port
/// routes to the required port (v4.7), so only another explicit port asks.
pub(super) fn port_requires_confirmation(sub: &SubRow, fqdn: &str, required: i64) -> bool {
    let fqdn = fqdn.trim();
    if fqdn.is_empty() {
        return false;
    }
    let Some(explicit) = port_of_url(fqdn).filter(|port| *port != required) else {
        return false;
    };
    let existing = sub
        .raw
        .get("domain_port_overrides")
        .and_then(|o| o.get(app_domains::without_port(fqdn)))
        .and_then(Value::as_i64)
        .unwrap_or(0);
    existing != explicit
}

/// `Index::submitApplication` and `Index::submitDatabase`.
pub async fn submit(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((uuid, resource)): Path<(String, String)>,
    body: axum::body::Bytes,
) -> ApiResult {
    let s = find_service(&state, &ctx, &uuid).await?;
    let part = find_part(&state, s.id, &resource).await?;
    require_admin(&ctx)?;
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    super::validation_toast(if part.is_application() {
        submit_application(&state, &ctx, &s, &part, &body).await
    } else {
        submit_database(&state, &s, &part, &body).await
    })
}

fn text(body: &Value, key: &str) -> Option<String> {
    body.get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .filter(|v| !v.is_empty())
}

fn flag(body: &Value, key: &str, default: bool) -> bool {
    body.get(key).and_then(Value::as_bool).unwrap_or(default)
}

async fn submit_application(
    state: &AppState,
    ctx: &AuthCtx,
    s: &ServiceRow,
    part: &SubRow,
    body: &Value,
) -> ApiResult {
    let mode = body
        .get("certificate_resolver_mode")
        .and_then(Value::as_str)
        .unwrap_or("server");
    if !["server", "none", "custom"].contains(&mode) {
        return Err(errors_one(
            "certificate_resolver_mode",
            "The selected certificate resolver is invalid.".into(),
        ));
    }
    let resolver_name = text(body, "certificate_resolver_name");
    if mode == "custom" && resolver_name.is_none() {
        return Err(errors_one(
            "certificate_resolver_name",
            "The certificate resolver name field is required.".into(),
        ));
    }
    let raw_url = text(body, "url");
    let mut errors = app_domains::validate_domains(raw_url.as_deref().unwrap_or_default());
    if let Some(first) = errors.drain(..).next() {
        return Err(errors_one("url", first));
    }
    // `ValidationPatterns::normalizeApplicationDomains`.
    let normalized = app_domains::normalize_domains(raw_url.as_deref());
    let force_domains = flag(body, "force_domains", false);
    if !force_domains && let Some(n) = &normalized {
        let list: Vec<String> = n.split(',').map(str::to_owned).collect();
        let conflicts = super::domain_conflicts(state, s, &list).await?;
        if !conflicts.is_empty() {
            return Ok(ok(json!({"domain_conflicts": conflicts})).into_response());
        }
    }
    if !flag(body, "force_port", false)
        && let Some(required) = templates::required_port(s.str("service_type"))
        && let Some(n) = &normalized
        && let Some(asked) = n
            .split(',')
            .find(|f| port_requires_confirmation(part, f, required))
    {
        // `$requestedPort`: the port of the domain that asks.
        return Ok(ok(json!({
            "port_warning": required,
            "requested_port": port_of_url(asked.trim()),
        }))
        .into_response());
    }
    if text(body, "image").is_none() {
        return Err(errors_one("image", "The image field is required.".into()));
    }
    let max_restart = body.get("max_restart_count").and_then(as_int).unwrap_or(0);
    if max_restart < 0 {
        return Err(errors_one(
            "max_restart_count",
            "The max restart count field must be at least 0.".into(),
        ));
    }
    let mut values: Vec<(&'static str, ColumnValue)> = vec![
        ("human_name", ColumnValue::Text(text(body, "human_name"))),
        ("description", ColumnValue::Text(text(body, "description"))),
        ("image", ColumnValue::Text(text(body, "image"))),
        (
            "exclude_from_status",
            ColumnValue::Bool(flag(body, "exclude_from_status", false)),
        ),
        (
            "is_log_drain_enabled",
            ColumnValue::Bool(flag(body, "is_log_drain_enabled", false)),
        ),
        (
            "is_gzip_enabled",
            ColumnValue::Bool(flag(body, "is_gzip_enabled", true)),
        ),
        (
            "is_stripprefix_enabled",
            ColumnValue::Bool(flag(body, "is_stripprefix_enabled", true)),
        ),
        (
            "traefik_certificate_resolver",
            ColumnValue::Text(match mode {
                "none" => Some(RESOLVER_NONE.to_owned()),
                "custom" => resolver_name,
                _ => None,
            }),
        ),
    ];
    if part.int("max_restart_count").unwrap_or(0) != max_restart {
        values.push(("restart_limit_reached", ColumnValue::Bool(false)));
    }
    values.push(("max_restart_count", ColumnValue::Int(max_restart)));
    let previous_dns_hostnames = crate::ui_shared::dns::previous_hostnames(
        state,
        "App\\Models\\ServiceApplication",
        part.id,
    )
    .await;
    svc::sub_update(
        &state.db,
        SubKind::Application,
        part.id,
        &values,
        state.now(),
    )
    .await?;
    crate::routes::services::save_application_urls(state, s, part, normalized.clone()).await?;
    crate::ui_shared::dns::queue_release_of_removed_hostnames(
        state,
        "App\\Models\\ServiceApplication",
        part.id,
        previous_dns_hostnames,
        ctx.team_id,
    );
    tracing::info!(target: "audit", event = "ui.service_application.updated", team_id = ctx.team_id, service_uuid = %s.uuid, service_application_uuid = %part.uuid, "service application updated");
    let multiple = normalized.as_deref().is_some_and(|f| f.contains(','));
    Ok(ok(json!({
        "message": "Service saved.",
        "warning": multiple.then_some("Some services do not support multiple domains, which can lead to problems and is NOT RECOMMENDED.<br><br>Only use multiple domains if you know what you are doing."),
    }))
    .into_response())
}

fn as_int(v: &Value) -> Option<i64> {
    v.as_i64()
        .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
}

async fn submit_database(
    state: &AppState,
    s: &ServiceRow,
    part: &SubRow,
    body: &Value,
) -> ApiResult {
    if text(body, "image").is_none() {
        return Err(errors_one("image", "The image field is required.".into()));
    }
    let port = body.get("public_port").and_then(as_int);
    if port.is_some_and(|p| !(1..=65535).contains(&p)) {
        return Err(errors_one(
            "public_port",
            "The public port field must be between 1 and 65535.".into(),
        ));
    }
    let timeout = body.get("public_port_timeout").and_then(as_int);
    if timeout.is_some_and(|t| t < 1) {
        return Err(errors_one(
            "public_port_timeout",
            "The public port timeout field must be at least 1.".into(),
        ));
    }
    let values = vec![
        ("human_name", ColumnValue::Text(text(body, "human_name"))),
        ("description", ColumnValue::Text(text(body, "description"))),
        ("image", ColumnValue::Text(text(body, "image"))),
        (
            "exclude_from_status",
            ColumnValue::Bool(flag(body, "exclude_from_status", false)),
        ),
        (
            "is_log_drain_enabled",
            ColumnValue::Bool(flag(body, "is_log_drain_enabled", false)),
        ),
        (
            "public_port",
            port.map_or(ColumnValue::Text(None), ColumnValue::Int),
        ),
        (
            "public_port_timeout",
            timeout.map_or(ColumnValue::Text(None), ColumnValue::Int),
        ),
    ];
    svc::sub_update(&state.db, SubKind::Database, part.id, &values, state.now()).await?;
    let jobs = jobs_of(state)?;
    if let Err(e) = jobs
        .update_service_compose(s.id, SubKind::Database, &part.name)
        .await
    {
        return Ok(toast(e));
    }
    Ok(ok(json!({"message": "Database saved."})).into_response())
}

/// `instantSaveApplicationSettings`, `instantSaveApplicationAdvanced`, `instantSaveLogDrain`,
/// `saveMaxRestartCount`.
pub async fn instant_settings(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((uuid, resource)): Path<(String, String)>,
    body: axum::body::Bytes,
) -> ApiResult {
    let s = find_service(&state, &ctx, &uuid).await?;
    let part = find_part(&state, s.id, &resource).await?;
    require_admin(&ctx)?;
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let (_, _, drain, _) = proxy_facts(&state, &s).await?;
    if body.get("is_log_drain_enabled") == Some(&json!(true)) && !drain {
        return Ok(toast(
            "Log drain is not enabled on the server. Please enable it first.",
        ));
    }
    let mut values: Vec<(&'static str, ColumnValue)> = Vec::new();
    for key in [
        "exclude_from_status",
        "is_log_drain_enabled",
        "is_gzip_enabled",
        "is_stripprefix_enabled",
    ] {
        if let Some(v) = body.get(key).and_then(Value::as_bool) {
            values.push((key, ColumnValue::Bool(v)));
        }
    }
    let mut message = "Settings saved.";
    if body.get("is_log_drain_enabled").is_some() {
        message = "You need to restart the service for the changes to take effect.";
    }
    if let Some(count) = body.get("max_restart_count").and_then(as_int) {
        values.push(("max_restart_count", ColumnValue::Int(count)));
        values.push(("restart_limit_reached", ColumnValue::Bool(false)));
        message = "Max restart count saved.";
    }
    svc::sub_update(&state.db, part.kind, part.id, &values, state.now()).await?;
    Ok(ok(json!({"message": message})).into_response())
}

/// `Index::enablePublicAccess` and `disablePublicAccess` (`instantSave`).
pub async fn public_access(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((uuid, resource)): Path<(String, String)>,
    body: axum::body::Bytes,
) -> ApiResult {
    let s = find_service(&state, &ctx, &uuid).await?;
    let part = svc::sub_by_uuid(&state.db, SubKind::Database, s.id, &resource)
        .await?
        .ok_or(NOT_FOUND)?;
    require_admin(&ctx)?;
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let public = flag(&body, "is_public", false);
    let port = body.get("public_port").and_then(as_int);
    let timeout = body.get("public_port_timeout").and_then(as_int);
    let jobs = jobs_of(&state)?;
    let persist = |public: bool| {
        vec![
            ("is_public", ColumnValue::Bool(public)),
            (
                "public_port",
                port.map_or(ColumnValue::Text(None), ColumnValue::Int),
            ),
            (
                "public_port_timeout",
                timeout.map_or(ColumnValue::Text(None), ColumnValue::Int),
            ),
        ]
    };
    if public {
        if port.is_none() {
            return Ok(toast("Public port is required."));
        }
        if !part
            .str("status")
            .unwrap_or_default()
            .starts_with("running")
        {
            return Ok(toast("Database must be started to be publicly accessible."));
        }
    }
    svc::sub_update(
        &state.db,
        SubKind::Database,
        part.id,
        &persist(public),
        state.now(),
    )
    .await?;
    let fresh = svc::sub_by_uuid(&state.db, SubKind::Database, s.id, &resource)
        .await?
        .ok_or(ApiError::Internal)?;
    let result = if public {
        jobs.start_service_database_proxy(&s, &fresh, &database_type(&fresh))
            .await
    } else {
        jobs.stop_service_database_proxy(&s, &fresh).await
    };
    if let Err(e) = result {
        svc::sub_update(
            &state.db,
            SubKind::Database,
            part.id,
            &persist(!public),
            state.now(),
        )
        .await?;
        return Ok(toast(e));
    }
    Ok(ok(json!({
        "message": if public { "Database is now publicly accessible." } else { "Database is no longer publicly accessible." },
    }))
    .into_response())
}

/// `ResourceCard::restart`.
pub async fn restart(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((uuid, resource)): Path<(String, String)>,
) -> ApiResult {
    let s = find_service(&state, &ctx, &uuid).await?;
    let part = find_part(&state, s.id, &resource).await?;
    require_admin(&ctx)?;
    jobs_of(&state)?.spawn_sub_restart(s, part.clone());
    Ok(ok(json!({
        "message": if part.is_application() {
            "Service application restarted successfully."
        } else {
            "Service database restarted successfully."
        },
    }))
    .into_response())
}

/// `Heading::removeSelectedResourceContainer` (`StopServiceApplication::run($resource, true, true)`).
pub async fn remove_container(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((uuid, resource)): Path<(String, String)>,
) -> ApiResult {
    let s = find_service(&state, &ctx, &uuid).await?;
    let part = find_part(&state, s.id, &resource).await?;
    require_admin(&ctx)?;
    jobs_of(&state)?.stop_sub(&s, &part, true, true).await;
    Ok(ok(json!({"message": "Container removed."})).into_response())
}

/// `Index::convertToApplication` and `convertToDatabase`: the writers of `is_migrated`.
pub async fn convert(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((uuid, resource)): Path<(String, String)>,
) -> ApiResult {
    let s = find_service(&state, &ctx, &uuid).await?;
    let part = find_part(&state, s.id, &resource).await?;
    require_admin(&ctx)?;
    let target = if part.is_application() {
        SubKind::Database
    } else {
        SubKind::Application
    };
    if svc::sub_by_name(&state.db, target, s.id, &part.name)
        .await?
        .is_some()
    {
        return Ok(toast(if part.is_application() {
            "A database with this name already exists."
        } else {
            "An application with this name already exists."
        }));
    }
    let now = state.now();
    let created = svc::sub_create(&state.db, target, s.id, &part.name, now).await?;
    let values = vec![
        (
            "human_name",
            ColumnValue::Text(part.str("human_name").map(str::to_owned)),
        ),
        (
            "description",
            ColumnValue::Text(part.str("description").map(str::to_owned)),
        ),
        (
            "exclude_from_status",
            ColumnValue::Bool(part.bool("exclude_from_status")),
        ),
        (
            "is_log_drain_enabled",
            ColumnValue::Bool(part.bool("is_log_drain_enabled")),
        ),
        (
            "image",
            ColumnValue::Text(part.str("image").map(str::to_owned)),
        ),
        ("is_migrated", ColumnValue::Bool(true)),
    ];
    svc::sub_update(&state.db, target, created.id, &values, now).await?;
    svc::sub_delete(&state.db, part.kind, part.id, state.now()).await?;
    tracing::info!(target: "audit", event = if part.is_application() { "ui.service_application.converted_to_database" } else { "ui.service_database.converted_to_application" }, team_id = ctx.team_id, service_uuid = %s.uuid, "service part converted");
    Ok(ok(json!({"message": "converted", "uuid": created.uuid})).into_response())
}

/// `Index::deleteApplication` and `deleteDatabase`.
pub async fn delete(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((uuid, resource)): Path<(String, String)>,
    body: axum::body::Bytes,
) -> ApiResult {
    let s = find_service(&state, &ctx, &uuid).await?;
    let part = find_part(&state, s.id, &resource).await?;
    require_admin(&ctx)?;
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    if !crate::ui::password_confirmed(
        &state,
        ctx.user_id,
        body.get("password").and_then(Value::as_str),
    )
    .await?
    {
        return Err(ApiError::Message(
            StatusCode::UNPROCESSABLE_ENTITY,
            "The provided password is incorrect.",
        ));
    }
    let removed = jobs_of(&state)?.remove_sub_container(&s, &part).await;
    svc::sub_delete(&state.db, part.kind, part.id, state.now()).await?;
    let kind = if part.is_application() {
        "Application"
    } else {
        "Database"
    };
    let message = if removed {
        format!("{kind} deleted.")
    } else {
        format!(
            "{kind} deleted from Coolify. The server does not respond, so its container is removed when the service starts again."
        )
    };
    tracing::info!(target: "audit", event = if part.is_application() { "ui.service_application.deleted" } else { "ui.service_database.deleted" }, team_id = ctx.team_id, service_uuid = %s.uuid, container_removed = removed, "service part deleted");
    Ok(ok(json!({"message": message, "warning": !removed})).into_response())
}
