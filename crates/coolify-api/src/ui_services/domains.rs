//! `Project\Service\Domains`: the domains of the compose applications of a service.
//! DNS checks and the Cloudflare automation are not ported (they contact external services).

use axum::Extension;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use coolify_db::servers::ColumnValue;
use coolify_db::services::{self as svc, ServiceRow, SubKind, SubRow};
use coolify_jobs::applications::domains as app_domains;
use coolify_jobs::compose_parser::domains::{UrlBase, get_fqdn_without_port};
use coolify_jobs::services::templates;
use serde_json::{Map, Value, json};

use super::resources::port_requires_confirmation;
use super::{find_service, jobs_of, require_admin, toast};
use crate::auth::AuthCtx;
use crate::response::{ApiError, ApiResult, ok};
use crate::state::AppState;

const MULTIPLE_DOMAINS: &str = "Some services do not support multiple domains, which can lead to problems and is NOT RECOMMENDED.<br><br>Only use multiple domains if you know what you are doing.";
const SSLIP: &str = "Your configuration is saved, but sslip domain with https is <span class='dark:text-red-500 text-red-500 font-bold'>NOT</span> recommended, because Let's Encrypt servers with this public domain are rate limited (SSL certificate validation will fail). <br><br>Use your own domain instead.";

fn split(value: Option<&str>) -> Vec<String> {
    value
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .map(str::to_owned)
        .collect()
}

/// `DomainUrlParts::compose`.
fn compose(scheme: &str, host: &str, port: &str, path: &str) -> String {
    let scheme = if scheme.trim().eq_ignore_ascii_case("http") {
        "http"
    } else {
        "https"
    };
    let (host, port, path) = (host.trim(), port.trim(), path.trim());
    let path = if !path.is_empty() && !path.starts_with(['/', '?', '#']) {
        format!("/{path}")
    } else {
        path.to_owned()
    };
    let port = if port.is_empty() {
        String::new()
    } else {
        format!(":{port}")
    };
    format!("{scheme}://{host}{port}{path}")
}

/// `DomainUrlParts::split`.
fn parts_json(url: &str) -> Value {
    let Some(p) = app_domains::UrlParts::parse(url).filter(|p| !p.host.is_empty()) else {
        return json!({"scheme": "https", "host": "", "port": "", "path": ""});
    };
    let mut path = p.path.clone().unwrap_or_default();
    if let Some(q) = &p.query {
        path.push('?');
        path.push_str(q);
    }
    if let Some(f) = &p.fragment {
        path.push('#');
        path.push_str(f);
    }
    let scheme = p.scheme.to_lowercase();
    json!({
        "scheme": if scheme == "http" || scheme == "https" { scheme } else { "https".to_owned() },
        "host": p.host,
        "port": p.port.clone().unwrap_or_default(),
        "path": path,
    })
}

/// `wwwCounterpartUrl($url, $forRedirectPairing)`.
fn www_counterpart(url: &str, for_pairing: bool) -> Option<String> {
    let p = app_domains::UrlParts::parse(url)?;
    let host = p.host.clone();
    if host.is_empty() {
        return None;
    }
    let lower = host.to_lowercase();
    if host
        .trim_matches(['[', ']'])
        .parse::<std::net::IpAddr>()
        .is_ok()
        || lower == "localhost"
    {
        return None;
    }
    if !for_pairing && (lower.contains("sslip.io") || lower.contains("nip.io")) {
        return None;
    }
    let counterpart = if lower.starts_with("www.") {
        host[4..].to_owned()
    } else {
        format!("www.{host}")
    };
    if counterpart.is_empty() || counterpart == host {
        return None;
    }
    let port = p.port.map_or(String::new(), |x| format!(":{x}"));
    let path = p.path.unwrap_or_default();
    let query = p.query.map_or(String::new(), |x| format!("?{x}"));
    let fragment = p.fragment.map_or(String::new(), |x| format!("#{x}"));
    Some(format!(
        "{}://{counterpart}{port}{path}{query}{fragment}",
        p.scheme
    ))
}

fn normalize_noindex(domain: &str) -> String {
    app_domains::without_port(&app_domains::normalize_url(domain))
}

fn noindex_list(app: &SubRow) -> Vec<String> {
    app.raw
        .get("noindex_domains")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(normalize_noindex)
                .collect()
        })
        .unwrap_or_default()
}

/// Sorted as `sortBy(strtolower(human_name ?: name))`.
async fn applications(state: &AppState, s: &ServiceRow) -> Result<Vec<SubRow>, ApiError> {
    let mut apps = svc::subs(&state.db, SubKind::Application, s.id).await?;
    let key = |a: &SubRow| {
        a.str("human_name")
            .filter(|h| !h.is_empty())
            .unwrap_or(&a.name)
            .to_lowercase()
    };
    apps.sort_by_key(key);
    Ok(apps)
}

/// `domainRowFromStored` and `effectiveDomainInternalPort`.
fn row_json(s: &ServiceRow, app: &SubRow, url: &str) -> Value {
    let stored = app.raw.get("domain_dns_statuses").and_then(|d| d.get(url));
    let canonical = app_domains::without_port(url);
    let overridden = app
        .raw
        .get("domain_port_overrides")
        .and_then(|o| o.get(&canonical))
        .and_then(Value::as_i64);
    let legacy = app_domains::UrlParts::parse(url)
        .and_then(|p| p.port_number())
        .filter(|p| *p > 0);
    let required = templates::required_port(s.str("service_type")).filter(|p| *p > 0);
    let (port, has_override) = match (overridden, legacy) {
        (Some(p), _) | (None, Some(p)) => (Some(p), true),
        _ => (required, false),
    };
    let status = stored
        .and_then(|e| e.get("status"))
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty());
    let mut parts = parts_json(url);
    if has_override && let Value::Object(m) = &mut parts {
        m.insert("port".into(), json!(port.unwrap_or_default().to_string()));
    }
    json!({
        "service_application_id": app.id,
        "url": url,
        "public_url": get_fqdn_without_port(url),
        "internal_port": port,
        "has_port_override": has_override,
        "dns_status": status.unwrap_or("pending"),
        "dns_message": status
            .and_then(|_| stored.and_then(|e| e.get("message")).and_then(Value::as_str))
            .unwrap_or("Not checked yet."),
        "is_noindexed": noindex_list(app).contains(&normalize_noindex(url)),
        "editing_parts": parts,
    })
}

/// `Domains::loadDomainState`.
pub async fn index(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let s = find_service(&state, &ctx, &uuid).await?;
    let apps = applications(&state, &s).await?;
    let mut rows: Vec<Value> = Vec::new();
    let mut list = Vec::new();
    for app in &apps {
        for url in split(app.str("fqdn")) {
            rows.push(row_json(&s, app, &url));
        }
        let redirect = app.str("redirect").unwrap_or("both");
        list.push(json!({
            "id": app.id,
            "uuid": app.uuid,
            "name": app.str("human_name").filter(|h| !h.is_empty()).unwrap_or(&app.name),
            "image": app.raw.get("image"),
            "required_port": templates::required_port(s.str("service_type")),
            "redirect": if ["www", "non-www", "both"].contains(&redirect) { redirect } else { "both" },
            "force_https": app.bool_or("is_force_https_enabled", true),
            "permanent_redirect": app.bool("is_permanent_redirect_enabled"),
        }));
    }
    // `sortBy(dns_status === 'failed' ? 0 : 1)`.
    rows.sort_by_key(|r| i32::from(r["dns_status"] != "failed"));
    Ok(ok(json!({"apps": list, "rows": rows, "can_update": ctx.is_team_admin()})).into_response())
}

async fn find_app(state: &AppState, s: &ServiceRow, id: i64) -> Result<SubRow, ApiError> {
    svc::subs(&state.db, SubKind::Application, s.id)
        .await?
        .into_iter()
        .find(|a| a.id == id)
        .ok_or(ApiError::Message(
            StatusCode::NOT_FOUND,
            "Service application not found.",
        ))
}

fn inline_error(message: String) -> ApiError {
    let mut m = Map::new();
    m.insert("domain".into(), json!([message]));
    ApiError::Validation(m)
}

struct SaveOptions<'a> {
    check_conflicts: bool,
    check_ports: bool,
    force_domains: bool,
    force_port: bool,
    noindex: Option<&'a [String]>,
    redirect: Option<&'a str>,
}

/// `Domains::saveDomainListForApp`: `Ok(None)` when saved, else the answer for the page.
async fn save_list(
    state: &AppState,
    s: &ServiceRow,
    app: &SubRow,
    domains: &[String],
    options: SaveOptions<'_>,
) -> Result<(Option<axum::response::Response>, Vec<&'static str>), ApiError> {
    let mut unique: Vec<String> = Vec::new();
    for d in domains.iter().filter(|d| !d.is_empty()) {
        if !unique.contains(d) {
            unique.push(d.clone());
        }
    }
    let joined = (!unique.is_empty()).then(|| unique.join(","));
    let normalized = app_domains::normalize_domains(joined.as_deref());
    if let Some(n) = &normalized
        && let Some(first) = app_domains::validate_domains(n).into_iter().next()
    {
        return Ok((Some(toast(first)), Vec::new()));
    }
    let list = split(normalized.as_deref());
    if options.check_conflicts && !options.force_domains && !list.is_empty() {
        let conflicts = super::domain_conflicts(state, s, &list).await?;
        if !conflicts.is_empty() {
            return Ok((
                Some(ok(json!({"domain_conflicts": conflicts})).into_response()),
                Vec::new(),
            ));
        }
    }
    if options.check_ports
        && !options.force_port
        && let Some(required) = templates::required_port(s.str("service_type"))
        && list
            .iter()
            .any(|f| port_requires_confirmation(app, f, required, app.str("fqdn")))
    {
        return Ok((
            Some(ok(json!({"port_warning": required})).into_response()),
            Vec::new(),
        ));
    }
    let mut warnings = Vec::new();
    if list
        .iter()
        .any(|d| d.contains("https") && d.contains("sslip"))
    {
        warnings.push(SSLIP);
    }
    let now = state.now();
    if let Some(noindex) = options.noindex {
        let mut kept: Vec<String> = Vec::new();
        for d in noindex {
            let n = normalize_noindex(d);
            if list.iter().any(|x| normalize_noindex(x) == n) && !kept.contains(&n) {
                kept.push(n);
            }
        }
        svc::sub_set_json(
            &state.db,
            SubKind::Application,
            app.id,
            "noindex_domains",
            Some(&json!(kept).to_string()),
            now,
        )
        .await?;
    }
    if let Some(redirect) = options.redirect {
        svc::sub_update(
            &state.db,
            SubKind::Application,
            app.id,
            &[("redirect", ColumnValue::Text(Some(redirect.to_owned())))],
            now,
        )
        .await?;
    }
    crate::routes::services::save_application_urls(state, s, app, normalized.clone()).await?;
    let jobs = jobs_of(state)?;
    if let Err(e) = jobs.parse_service(s.id).await {
        return Ok((Some(toast(e)), Vec::new()));
    }
    if normalized.as_deref().is_some_and(|f| f.contains(',')) {
        warnings.push(MULTIPLE_DOMAINS);
    }
    Ok((None, warnings))
}

fn text<'a>(body: &'a Value, key: &str) -> &'a str {
    body.get(key).and_then(Value::as_str).unwrap_or_default()
}

fn flag(body: &Value, key: &str) -> bool {
    body.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn composed(body: &Value) -> String {
    let port = match body.get("port") {
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::String(p)) => p.clone(),
        _ => String::new(),
    };
    compose(
        text(body, "scheme"),
        text(body, "host"),
        &port,
        text(body, "path"),
    )
}

/// `Domains::addDomain`.
pub async fn add(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    body: axum::body::Bytes,
) -> ApiResult {
    let s = find_service(&state, &ctx, &uuid).await?;
    require_admin(&ctx)?;
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let domain = composed(&body);
    if let Some(first) = app_domains::validate_domains(&domain).into_iter().next() {
        return Ok(toast(first));
    }
    let Some(id) = body.get("app_id").and_then(Value::as_i64) else {
        return Ok(toast("Select a service application."));
    };
    let app = find_app(&state, &s, id).await?;
    let Some(normalized) = app_domains::normalize_domains(Some(&domain)) else {
        return Err(inline_error("Please enter a valid domain URL.".into()));
    };
    let new_urls = split(Some(&normalized));
    let current = split(app.str("fqdn"));
    let canonical: Vec<String> = current
        .iter()
        .map(|u| app_domains::without_port(u))
        .collect();
    for url in &new_urls {
        if canonical.contains(&app_domains::without_port(url)) {
            return Err(inline_error(format!(
                "Domain {url} is already configured for this service."
            )));
        }
    }
    let mut merged = current;
    merged.extend(new_urls.iter().cloned());
    if matches!(app.str("redirect"), Some("www" | "non-www")) {
        merged.extend(new_urls.iter().filter_map(|u| www_counterpart(u, false)));
    }
    let (answer, warnings) = save_list(
        &state,
        &s,
        &app,
        &merged,
        SaveOptions {
            check_conflicts: true,
            check_ports: true,
            force_domains: flag(&body, "force_domains"),
            force_port: flag(&body, "force_port"),
            noindex: None,
            redirect: None,
        },
    )
    .await?;
    if let Some(answer) = answer {
        return Ok(answer);
    }
    Ok(ok(json!({"message": "Domain added.", "warnings": warnings})).into_response())
}

/// `Domains::updateDomain`.
pub async fn update(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    body: axum::body::Bytes,
) -> ApiResult {
    let s = find_service(&state, &ctx, &uuid).await?;
    require_admin(&ctx)?;
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let id = body
        .get("app_id")
        .and_then(Value::as_i64)
        .unwrap_or_default();
    let app = find_app(&state, &s, id).await?;
    let old_url = text(&body, "old_url").to_owned();
    let indexing = text(&body, "indexing");
    let redirect = text(&body, "redirect");
    if !["index", "noindex"].contains(&indexing) || !["both", "www", "non-www"].contains(&redirect)
    {
        return Ok(toast("The selected value is invalid."));
    }
    let domain = composed(&body);
    if let Some(first) = app_domains::validate_domains(&domain).into_iter().next() {
        return Ok(toast(first));
    }
    let normalized = app_domains::normalize_domains(Some(&domain));
    let list = split(normalized.as_deref());
    let [new_url] = list.as_slice() else {
        return Err(inline_error(
            "Please enter a single valid domain URL.".into(),
        ));
    };
    let current = split(app.str("fqdn"));
    let other: Vec<String> = current
        .iter()
        .filter(|u| **u != old_url)
        .map(|u| app_domains::without_port(u))
        .collect();
    if other.contains(&app_domains::without_port(new_url)) {
        return Err(inline_error(format!(
            "Domain {new_url} is already configured for this service."
        )));
    }
    let mut app = app;
    let new_has_port = app_domains::UrlParts::parse(new_url).is_some_and(|p| p.port.is_some());
    if !new_has_port {
        // The port field was cleared: the override of the old and the new domain goes away.
        let mut overrides = app
            .raw
            .get("domain_port_overrides")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        overrides.remove(&app_domains::without_port(&old_url));
        overrides.remove(&app_domains::without_port(new_url));
        app.raw.insert(
            "domain_port_overrides".into(),
            if overrides.is_empty() {
                Value::Null
            } else {
                Value::Object(overrides.clone())
            },
        );
        svc::sub_set_json(
            &state.db,
            SubKind::Application,
            app.id,
            "domain_port_overrides",
            (!overrides.is_empty())
                .then(|| Value::Object(overrides).to_string())
                .as_deref(),
            state.now(),
        )
        .await?;
    }
    let original_redirect = app.str("redirect").unwrap_or("both").to_owned();
    let mut updated: Vec<String> = current
        .iter()
        .map(|u| {
            if *u == old_url {
                new_url.clone()
            } else {
                u.clone()
            }
        })
        .collect();
    if redirect != original_redirect && ["www", "non-www"].contains(&redirect) {
        for url in updated.clone() {
            if let Some(counterpart) = www_counterpart(&url, true) {
                let host = app_domains::UrlParts::parse(&counterpart).map(|p| p.host);
                if !updated
                    .iter()
                    .any(|c| app_domains::UrlParts::parse(c).map(|p| p.host) == host)
                {
                    updated.push(counterpart);
                }
            }
        }
    }
    let mut noindex: Vec<String> = noindex_list(&app)
        .into_iter()
        .filter(|d| *d != normalize_noindex(&old_url))
        .collect();
    if indexing == "noindex" {
        noindex.push(new_url.clone());
    }
    let owner = crate::ui_shared::dns::Owner {
        morph: "App\\Models\\ServiceApplication",
        id: app.id,
        uuid: &app.uuid,
        name: &app.name,
        type_name: "service_application",
        team_id: s.team_id,
    };
    let previous_hostnames = crate::ui_shared::dns::hostnames_before(&state, &owner).await;
    let (answer, warnings) = save_list(
        &state,
        &s,
        &app,
        &updated,
        SaveOptions {
            check_conflicts: true,
            check_ports: true,
            force_domains: flag(&body, "force_domains"),
            force_port: flag(&body, "force_port"),
            noindex: Some(&noindex),
            redirect: Some(redirect),
        },
    )
    .await?;
    if let Some(answer) = answer {
        return Ok(answer);
    }
    let mut warnings: Vec<String> = warnings.iter().map(ToString::to_string).collect();
    warnings.extend(
        crate::ui_shared::dns::release_after_edit(&state, &owner, &previous_hostnames).await,
    );
    Ok(ok(json!({"message": "Domain updated.", "warnings": warnings})).into_response())
}

/// `Domains::removeDomain`.
pub async fn remove(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    body: axum::body::Bytes,
) -> ApiResult {
    let s = find_service(&state, &ctx, &uuid).await?;
    require_admin(&ctx)?;
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let id = body
        .get("app_id")
        .and_then(Value::as_i64)
        .unwrap_or_default();
    let app = find_app(&state, &s, id).await?;
    let url = text(&body, "url");
    let updated: Vec<String> = split(app.str("fqdn"))
        .into_iter()
        .filter(|d| d != url)
        .collect();
    let owner = crate::ui_shared::dns::Owner {
        morph: "App\\Models\\ServiceApplication",
        id: app.id,
        uuid: &app.uuid,
        name: &app.name,
        type_name: "service_application",
        team_id: s.team_id,
    };
    let (answer, warnings) = save_list(
        &state,
        &s,
        &app,
        &updated,
        SaveOptions {
            check_conflicts: false,
            check_ports: true,
            force_domains: true,
            force_port: true,
            noindex: None,
            redirect: None,
        },
    )
    .await?;
    if let Some(answer) = answer {
        return Ok(answer);
    }
    let mut warnings: Vec<String> = warnings.iter().map(ToString::to_string).collect();
    let delete_record = body
        .get("delete_managed_dns")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    warnings.extend(
        crate::ui_shared::dns::release_after_remove(&state, &owner, url, delete_record).await,
    );
    Ok(ok(json!({"message": "Domain removed.", "warnings": warnings})).into_response())
}

/// `Domains::updateForceHttps` and `updatePermanentRedirect`.
pub async fn settings(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    body: axum::body::Bytes,
) -> ApiResult {
    let s = find_service(&state, &ctx, &uuid).await?;
    require_admin(&ctx)?;
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let id = body
        .get("app_id")
        .and_then(Value::as_i64)
        .unwrap_or_default();
    let app = find_app(&state, &s, id).await?;
    let (column, value, message) = if let Some(v) = body.get("force_https").and_then(Value::as_bool)
    {
        (
            "is_force_https_enabled",
            v,
            "HTTP to HTTPS redirect updated.",
        )
    } else if let Some(v) = body.get("permanent_redirect").and_then(Value::as_bool) {
        ("is_permanent_redirect_enabled", v, "Redirect type updated.")
    } else {
        return Err(ApiError::NotPorted);
    };
    svc::sub_update(
        &state.db,
        SubKind::Application,
        app.id,
        &[(column, ColumnValue::Bool(value))],
        state.now(),
    )
    .await?;
    if let Err(e) = jobs_of(&state)?.parse_service(s.id).await {
        return Ok(toast(e));
    }
    Ok(ok(json!({"message": message})).into_response())
}

/// `Domains::generateDomain` and `regenerateEditingDomain`: `generateUrl(server, new_public_id())`.
pub async fn generate(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let s = find_service(&state, &ctx, &uuid).await?;
    require_admin(&ctx)?;
    let Some(server) = (match s.server_id() {
        Some(id) => coolify_db::servers::find_by_id(&state.db, id).await?,
        None => None,
    }) else {
        return Ok(toast("Service application or server not found."));
    };
    let settings = server.settings.clone().unwrap_or_default();
    let ip = server
        .record
        .get("ip")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let (_, ipv4, ipv6) = coolify_db::servers::instance_urls(&state.db).await?;
    let public = ipv4.or(ipv6);
    let base = UrlBase::for_server(
        settings.get("wildcard_domain").and_then(Value::as_str),
        &ip,
        public.as_deref(),
    )
    .await;
    let domain = base.generate_url(&coolify_domain::new_public_id());
    let mut parts = parts_json(&domain);
    if let (Some(port), Value::Object(m)) =
        (templates::required_port(s.str("service_type")), &mut parts)
        && m.get("port").and_then(Value::as_str) == Some("")
    {
        m.insert("port".into(), json!(port.to_string()));
    }
    Ok(ok(json!({"parts": parts})).into_response())
}
