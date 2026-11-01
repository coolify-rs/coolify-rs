//! `Project\Application\Domains`. The DNS checks and DNS providers are in `super::dns`.

use std::collections::BTreeMap;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use coolify_compat::yaml::PhpValue;
use coolify_db::applications::{self as apps, ApplicationRow};
use coolify_db::servers::{self, ColumnValue};
use coolify_jobs::applications::domains;
use coolify_jobs::compose_parser::domains::get_fqdn_without_port;
use coolify_jobs::compose_parser::service::{first_docker_compose_service_port, is_database_image};
use serde_json::{Map, Value, json};

use super::{dns, find};
use crate::auth::AuthCtx;
use crate::response::{ApiError, ApiResult, ok};
use crate::state::AppState;
use crate::ui_applications::regenerate_labels;

const SSLIP: &str = "Your configuration is saved, but sslip domain with https is <span class='dark:text-red-500 text-red-500 font-bold'>NOT</span> recommended, because Let's Encrypt servers with this public domain are rate limited (SSL certificate validation will fail). <br><br>Use your own domain instead.";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/ui/resources/{kind}/{uuid}/domains", get(index))
        .route("/ui/resources/{kind}/{uuid}/domains/add", post(add))
        .route("/ui/resources/{kind}/{uuid}/domains/update", post(update))
        .route("/ui/resources/{kind}/{uuid}/domains/remove", post(remove))
        .route(
            "/ui/resources/{kind}/{uuid}/domains/generate",
            post(generate),
        )
        .route(
            "/ui/resources/{kind}/{uuid}/domains/regenerate",
            post(regenerate),
        )
        .route(
            "/ui/resources/{kind}/{uuid}/domains/settings",
            post(settings),
        )
}

async fn application(
    state: &AppState,
    ctx: &AuthCtx,
    kind: &str,
    uuid: &str,
) -> Result<ApplicationRow, ApiError> {
    let res = find(state, ctx, kind, uuid).await?;
    if !res.is_application() {
        return Err(super::NOT_FOUND);
    }
    apps::find_in_team(&state.db, res.team_id, &res.uuid)
        .await?
        .ok_or(super::NOT_FOUND)
}

fn toast(message: impl Into<String>) -> Response {
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        Json(json!({"message": message.into()})),
    )
        .into_response()
}

fn field_error(message: String) -> Response {
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        Json(json!({"message": message, "errors": {"domain": [message]}})),
    )
        .into_response()
}

pub(super) fn split(value: Option<&str>) -> Vec<String> {
    value
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .map(str::to_owned)
        .collect()
}

fn is_compose(a: &ApplicationRow) -> bool {
    a.str("build_pack") == Some("dockercompose")
}

/// `labelsAreWritable`.
fn labels_writable(a: &ApplicationRow) -> bool {
    !a.setting_bool("is_container_label_readonly_enabled")
}

fn json_map(a: &ApplicationRow, key: &str) -> Map<String, Value> {
    let parsed = match a.raw.get(key) {
        Some(Value::String(s)) => serde_json::from_str::<Value>(s).ok(),
        Some(v) => Some(v.clone()),
        None => None,
    };
    match parsed {
        Some(Value::Object(o)) => o,
        _ => Map::new(),
    }
}

/// `$application->domain_port_overrides`.
fn stored_overrides(a: &ApplicationRow) -> BTreeMap<String, i64> {
    json_map(a, "domain_port_overrides")
        .iter()
        .filter_map(|(k, v)| {
            v.as_i64()
                .or_else(|| v.as_str()?.parse().ok())
                .map(|p| (k.clone(), p))
        })
        .collect()
}

fn compose_domains(a: &ApplicationRow) -> Map<String, Value> {
    json_map(a, "docker_compose_domains")
}

/// `data_get($value, $key)` of a parsed Compose file.
fn php_get<'a>(value: &'a PhpValue, key: &str) -> Option<&'a PhpValue> {
    match value {
        PhpValue::Array(items) => items
            .iter()
            .find(|(k, _)| k.as_string() == key)
            .map(|(_, v)| v),
        _ => None,
    }
}

pub(super) fn parsed_compose(a: &ApplicationRow) -> Option<PhpValue> {
    coolify_jobs::compose_parser::validate::parse_docker_compose_yaml(a.str("docker_compose_raw")?)
        .ok()
}

/// `composeServices`: the services that are not databases.
pub(super) fn compose_services(a: &ApplicationRow) -> Vec<String> {
    let Some(parsed) = parsed_compose(a) else {
        return Vec::new();
    };
    let Some(PhpValue::Array(services)) = php_get(&parsed, "services") else {
        return Vec::new();
    };
    services
        .iter()
        .filter_map(|(key, service)| {
            let image = match php_get(service, "image") {
                Some(PhpValue::Str(s)) => s.as_str(),
                _ => "",
            };
            (!is_database_image(image, service)).then(|| key.as_string())
        })
        .collect()
}

/// `dockerComposeServicePorts($compose, $service)`.
fn compose_service_ports(a: &ApplicationRow, service: Option<&str>) -> Vec<i64> {
    let (Some(parsed), Some(service)) = (parsed_compose(a), service) else {
        return Vec::new();
    };
    let Some(entry) = php_get(&parsed, "services").and_then(|s| php_get(s, service)) else {
        return Vec::new();
    };
    let mut ports = Vec::new();
    for key in ["expose", "ports"] {
        if let Some(PhpValue::Array(list)) = php_get(entry, key) {
            for (_, definition) in list {
                let wrapped = PhpValue::map([("expose", PhpValue::list([definition.clone()]))]);
                if let Some(p) = first_docker_compose_service_port(&wrapped)
                    && !ports.contains(&p)
                {
                    ports.push(p);
                }
            }
        }
    }
    ports
}

/// `$application->ports_exposes_array`.
fn exposed_ports(a: &ApplicationRow) -> Vec<i64> {
    a.str("ports_exposes")
        .unwrap_or_default()
        .split(',')
        .filter_map(|p| p.trim().parse::<i64>().ok())
        .filter(|p| *p > 0)
        .collect()
}

/// `inheritedDomainInternalPort($service)`.
pub(super) fn inherited_port(a: &ApplicationRow, service: Option<&str>) -> Option<i64> {
    if let Some(p) = compose_service_ports(a, service).first() {
        return Some(*p);
    }
    if is_compose(a) && service.is_some() {
        return None;
    }
    if a.setting_bool("is_static") {
        return Some(80);
    }
    exposed_ports(a).first().copied()
}

/// `availableInternalPorts($service)`.
pub(super) fn available_ports(a: &ApplicationRow, service: Option<&str>) -> Vec<i64> {
    if is_compose(a) {
        return compose_service_ports(a, service);
    }
    let mut ports = if a.setting_bool("is_static") {
        vec![80]
    } else {
        exposed_ports(a)
    };
    ports.extend(stored_overrides(a).values().filter(|p| **p > 0));
    for url in split(a.str("fqdn")) {
        if let Some(p) = domains::UrlParts::parse(&url)
            .and_then(|u| u.port_number())
            .filter(|p| *p > 0)
        {
            ports.push(p);
        }
    }
    ports
}

/// `DomainUrlParts::compose`.
pub(super) fn compose(scheme: &str, host: &str, port: &str, path: &str) -> String {
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
pub(super) fn parts_json(url: &str) -> Value {
    let Some(p) = domains::UrlParts::parse(url).filter(|p| !p.host.is_empty()) else {
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
    let p = domains::UrlParts::parse(url)?;
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
    domains::without_port(&domains::normalize_url(domain))
}

/// `noindexDomains()`.
fn noindex_list(a: &ApplicationRow) -> Vec<String> {
    let parsed = match a.raw.get("noindex_domains") {
        Some(Value::String(s)) => serde_json::from_str::<Value>(s).ok(),
        Some(v) => Some(v.clone()),
        None => None,
    };
    let mut out: Vec<String> = Vec::new();
    if let Some(Value::Array(list)) = parsed {
        for v in list {
            if let Some(d) = v.as_str().filter(|d| !d.trim().is_empty()) {
                let d = normalize_noindex(d);
                if !out.contains(&d) {
                    out.push(d);
                }
            }
        }
    }
    out
}

/// `normalizeRedirect()`.
fn normalize_redirect(value: Option<&str>) -> &str {
    match value {
        Some(v @ ("www" | "non-www" | "both")) => v,
        _ => "both",
    }
}

/// `serviceRedirectFor($service)` with what is saved.
fn redirect_of(a: &ApplicationRow, service: Option<&str>) -> String {
    match service {
        Some(s) if is_compose(a) => {
            let map = compose_domains(a);
            normalize_redirect(
                map.get(s)
                    .and_then(|e| e.get("redirect"))
                    .and_then(Value::as_str),
            )
            .to_owned()
        }
        _ => normalize_redirect(a.str("redirect")).to_owned(),
    }
}

/// `domainRowFromStored()` and `effectiveDomainInternalPort()`.
fn row_json(a: &ApplicationRow, url: &str, service: Option<&str>) -> Value {
    let statuses = json_map(a, "domain_dns_statuses");
    let key = service.map_or_else(|| url.to_owned(), |s| format!("{s}|{url}"));
    let entry = statuses.get(&key);
    let status = entry
        .and_then(|e| e.get("status"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let canonical = domains::without_port(url);
    let overridden = stored_overrides(a).get(&canonical).copied();
    let legacy = domains::UrlParts::parse(url)
        .and_then(|p| p.port_number())
        .filter(|p| *p > 0);
    let inherited = inherited_port(a, service);
    let (port, has_override) = match (overridden, legacy) {
        (Some(p), _) | (None, Some(p)) => (Some(p), true),
        _ => (inherited, false),
    };
    let mut parts = parts_json(url);
    if has_override && let Value::Object(m) = &mut parts {
        m.insert("port".into(), json!(port.unwrap_or_default().to_string()));
    }
    json!({
        "url": url,
        "service": service,
        "public_url": get_fqdn_without_port(url),
        "internal_port": port,
        "has_port_override": has_override,
        "inherited_port": inherited,
        "dns_status": status.unwrap_or("pending"),
        "dns_message": status
            .and_then(|_| entry.and_then(|e| e.get("message")).and_then(Value::as_str))
            .unwrap_or("Not checked yet."),
        "is_noindexed": noindex_list(a).contains(&normalize_noindex(url)),
        "redirect": redirect_of(a, service),
        "editing_parts": parts,
    })
}

/// The compose service names: `composeServices`, then the keys of `docker_compose_domains`.
fn service_names(a: &ApplicationRow) -> Vec<String> {
    let mut names = compose_services(a);
    for key in compose_domains(a).keys() {
        if !names.contains(key) {
            names.push(key.clone());
        }
    }
    names
}

/// `currentDomainList($service)`.
fn current_list(a: &ApplicationRow, service: Option<&str>) -> Vec<String> {
    if is_compose(a) {
        let map = compose_domains(a);
        return split(
            service
                .and_then(|s| map.get(s))
                .and_then(|e| e.get("domain"))
                .and_then(Value::as_str),
        );
    }
    split(a.str("fqdn"))
}

/// `loadDomainState()`.
async fn index(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid)): Path<(String, String)>,
) -> ApiResult {
    let a = application(&state, &ctx, &kind, &uuid).await?;
    let compose_mode = is_compose(&a);
    let mut rows: Vec<Value> = Vec::new();
    if compose_mode {
        let map = compose_domains(&a);
        for name in service_names(&a) {
            for url in split(
                map.get(&name)
                    .and_then(|e| e.get("domain"))
                    .and_then(Value::as_str),
            ) {
                rows.push(row_json(&a, &url, Some(&name)));
            }
        }
    } else {
        for url in split(a.str("fqdn")) {
            rows.push(row_json(&a, &url, None));
        }
    }
    // `sortDomainRowsByDnsStatus`.
    rows.sort_by_key(|r| i32::from(r["dns_status"] != "failed"));
    Ok(ok(json!({
        "can_update": ctx.is_team_admin(),
        "is_compose": compose_mode,
        "labels_are_writable": labels_writable(&a),
        "compose_services": compose_services(&a),
        "force_https": a.setting_bool("is_force_https_enabled"),
        "permanent_redirect": a.setting_bool("is_permanent_redirect_enabled"),
        "rows": rows,
    }))
    .into_response())
}

fn text<'a>(body: &'a Value, key: &str) -> &'a str {
    body.get(key).and_then(Value::as_str).unwrap_or_default()
}

fn flag(body: &Value, key: &str) -> bool {
    body.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn body_port(body: &Value) -> String {
    match body.get("port") {
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::String(p)) => p.clone(),
        _ => String::new(),
    }
}

fn service_of(body: &Value) -> Option<&str> {
    body.get("service")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

struct Save<'a> {
    service: Option<&'a str>,
    check_conflicts: bool,
    force_domains: bool,
    noindex: Option<Vec<String>>,
    redirect: Option<&'a str>,
    /// Canonical domains whose `domain_port_overrides` entry goes.
    drop_overrides: Vec<String>,
}

/// `saveDomainList()`: `Ok(None)` when saved, else the answer for the page.
async fn save_list(
    state: &AppState,
    a: &ApplicationRow,
    list: &[String],
    options: Save<'_>,
) -> Result<(Option<Response>, Vec<&'static str>), ApiError> {
    let mut unique: Vec<String> = Vec::new();
    for d in list.iter().filter(|d| !d.is_empty()) {
        if !unique.contains(d) {
            unique.push(d.clone());
        }
    }
    let joined = (!unique.is_empty()).then(|| unique.join(","));
    let normalized = domains::normalize_domains(joined.as_deref());
    if let Some(n) = &normalized
        && let Some(first) = domains::validate_domains(n).into_iter().next()
    {
        return Ok((Some(toast(first)), Vec::new()));
    }
    let compose_mode = is_compose(a);
    if compose_mode && options.service.is_none() {
        return Ok((
            Some(toast("A service is required for compose domains.")),
            Vec::new(),
        ));
    }
    let names = split(normalized.as_deref());
    // `checkDomainUsage(resource: $this->application)`.
    if options.check_conflicts && !options.force_domains && !names.is_empty() {
        let own = format!("/application/{}", a.uuid);
        let mut conflicts = Vec::new();
        for d in &names {
            for conflict in crate::ui_settings::domain_usage(state, d).await? {
                let mine = conflict
                    .get("resource_link")
                    .and_then(Value::as_str)
                    .is_some_and(|l| l.ends_with(&own));
                if !mine {
                    conflicts.push(conflict);
                }
            }
        }
        if !conflicts.is_empty() {
            return Ok((
                Some(ok(json!({"domain_conflicts": conflicts})).into_response()),
                Vec::new(),
            ));
        }
    }
    let mut warnings = Vec::new();
    if names
        .iter()
        .any(|d| d.contains("https") && d.contains("sslip"))
    {
        warnings.push(SSLIP);
    }

    let mut overrides = stored_overrides(a);
    for canonical in &options.drop_overrides {
        overrides.remove(canonical);
    }
    let mut columns: Vec<(&'static str, ColumnValue)> = Vec::new();
    let mut compose_map = compose_domains(a);
    let mut current: Vec<String>;
    if compose_mode {
        let service = options.service.unwrap_or_default();
        let (stored, incoming) =
            domains::normalize_port_overrides(normalized.as_deref(), &overrides);
        // `mergeComposeDomainPortOverrides()`.
        let new_canonical: Vec<String> = split(stored.as_deref())
            .iter()
            .map(|u| domains::without_port(u))
            .collect();
        for url in current_list(a, Some(service)) {
            let canonical = domains::without_port(&url);
            if !new_canonical.contains(&canonical) {
                overrides.remove(&canonical);
            }
        }
        overrides.extend(incoming.unwrap_or_default());
        let mut entry = compose_map
            .get(service)
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        entry.insert("domain".into(), json!(stored));
        if let Some(redirect) = options.redirect {
            entry.insert("redirect".into(), json!(redirect));
        }
        compose_map.insert(service.to_owned(), Value::Object(entry));
        columns.push((
            "docker_compose_domains",
            ColumnValue::Text(Some(coolify_compat::php_json::encode(&Value::Object(
                compose_map.clone(),
            )))),
        ));
        columns.push(("fqdn", ColumnValue::Text(None)));
        columns.push((
            "domain_port_overrides",
            ColumnValue::Text(
                (!overrides.is_empty())
                    .then(|| domains::overrides_json(stored.as_deref(), &overrides)),
            ),
        ));
        current = Vec::new();
        for e in compose_map.values() {
            current.extend(split(e.get("domain").and_then(Value::as_str)));
        }
    } else {
        // The `saving` hook: ports move from the domains to the overrides.
        let (stored, kept) = domains::normalize_port_overrides(normalized.as_deref(), &overrides);
        columns.push((
            "domain_port_overrides",
            ColumnValue::Text(kept.map(|o| domains::overrides_json(stored.as_deref(), &o))),
        ));
        current = split(stored.as_deref());
        columns.push(("fqdn", ColumnValue::Text(stored)));
        if let Some(redirect) = options.redirect {
            columns.push(("redirect", ColumnValue::Text(Some(redirect.to_owned()))));
        }
    }
    // `setNoindexDomains()` / `syncNoindexDomains()`: only domains the resource has.
    current = current.iter().map(|d| normalize_noindex(d)).collect();
    let wanted = options.noindex.unwrap_or_else(|| noindex_list(a));
    let mut kept: Vec<Value> = Vec::new();
    for d in wanted {
        let n = normalize_noindex(&d);
        if current.contains(&n) && !kept.iter().any(|k| k.as_str() == Some(&n)) {
            kept.push(Value::String(n));
        }
    }
    if a.raw.get("noindex_domains").is_some_and(|v| !v.is_null()) || !kept.is_empty() {
        columns.push((
            "noindex_domains",
            ColumnValue::Text(Some(coolify_compat::php_json::encode(&Value::Array(kept)))),
        ));
    }
    apps::update_if_changed(&state.db, a.id, &columns, state.now()).await?;
    regenerate_labels(state, a.id, false).await?;
    Ok((None, warnings))
}

/// `addDomain()`.
async fn add(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> ApiResult {
    let a = application(&state, &ctx, &kind, &uuid).await?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    if labels_writable(&a) {
        return Ok(toast(
            "Domains cannot be edited while container labels are writable. Set domains in the Labels section on General.",
        ));
    }
    let port = body_port(&body);
    let new_domain = compose(
        text(&body, "scheme"),
        text(&body, "host"),
        &port,
        text(&body, "path"),
    );
    if let Some(first) = domains::validate_domains(&new_domain).into_iter().next() {
        // `validateOnly()` throws inside the `try`: `handleError()` shows a toast.
        return Ok(toast(first));
    }
    let Some(normalized) = domains::normalize_domains(Some(&new_domain)) else {
        return Ok(field_error("Please enter a valid domain URL.".to_owned()));
    };
    let service = service_of(&body);
    let new_urls = split(Some(&normalized));
    let paired: Vec<String> = new_urls
        .iter()
        .filter_map(|u| www_counterpart(u, false))
        .collect();
    let current = current_list(&a, service);
    let canonical: Vec<String> = current.iter().map(|u| domains::without_port(u)).collect();
    for url in &new_urls {
        if canonical.contains(&domains::without_port(url)) {
            return Ok(field_error(format!("Domain {url} is already configured.")));
        }
    }
    // `shouldConfirmPort()`.
    let wanted_port = port.trim().parse::<i64>().ok().filter(|p| *p > 0);
    if let Some(p) = wanted_port
        && !flag(&body, "force_port")
        && !available_ports(&a, service).contains(&p)
    {
        return Ok(ok(json!({"port_warning": p})).into_response());
    }
    let mut merged = current;
    merged.extend(new_urls);
    merged.extend(paired);
    let (answer, warnings) = save_list(
        &state,
        &a,
        &merged,
        Save {
            service,
            check_conflicts: true,
            force_domains: flag(&body, "force_domains"),
            noindex: None,
            redirect: None,
            drop_overrides: Vec::new(),
        },
    )
    .await?;
    if let Some(answer) = answer {
        return Ok(answer);
    }
    Ok(ok(json!({"message": "Domain added.", "warnings": warnings})).into_response())
}

/// `updateDomain()`.
async fn update(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> ApiResult {
    let a = application(&state, &ctx, &kind, &uuid).await?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    if labels_writable(&a) {
        return Ok(toast(
            "Domains cannot be edited while container labels are writable.",
        ));
    }
    let port = body_port(&body);
    let editing = compose(
        text(&body, "scheme"),
        text(&body, "host"),
        &port,
        text(&body, "path"),
    );
    if let Some(first) = domains::validate_domains(&editing).into_iter().next() {
        return Ok(toast(first));
    }
    let list = split(domains::normalize_domains(Some(&editing)).as_deref());
    let [new_url] = list.as_slice() else {
        return Ok(field_error(
            "Please enter a single valid domain URL.".to_owned(),
        ));
    };
    let old_url = text(&body, "old_url");
    let service = service_of(&body);
    let indexing = text(&body, "indexing");
    let redirect = normalize_redirect(Some(text(&body, "redirect")));
    let current = current_list(&a, service);
    if !current.iter().any(|u| u == old_url) {
        return Ok(ok(json!({})).into_response());
    }
    let mut drop_overrides = Vec::new();
    let has_port = domains::UrlParts::parse(new_url)
        .and_then(|p| p.port)
        .is_some_and(|p| !p.is_empty());
    if !has_port {
        drop_overrides.push(domains::without_port(old_url));
        drop_overrides.push(domains::without_port(new_url));
    }
    let others: Vec<String> = current
        .iter()
        .filter(|u| *u != old_url)
        .map(|u| domains::without_port(u))
        .collect();
    if others.contains(&domains::without_port(new_url)) {
        return Ok(field_error(format!(
            "Domain {new_url} is already configured."
        )));
    }
    // `shouldConfirmPort($port, currentRowPort($oldUrl), $service)`.
    let current_port = stored_overrides(&a)
        .get(&domains::without_port(old_url))
        .copied()
        .filter(|p| *p > 0)
        .or_else(|| domains::UrlParts::parse(old_url).and_then(|p| p.port_number()));
    let wanted_port = port.trim().parse::<i64>().ok().filter(|p| *p > 0);
    if let Some(p) = wanted_port
        && !flag(&body, "force_port")
        && current_port != Some(p)
        && !available_ports(&a, service).contains(&p)
    {
        return Ok(ok(json!({"port_warning": p})).into_response());
    }
    let mut replacements: Vec<(String, String)> = vec![(old_url.to_owned(), new_url.clone())];
    // A regenerated host moves the other half of a www pair with it.
    let original_redirect = normalize_redirect(Some(text(&body, "original_redirect")));
    let generated = text(&body, "generated_host");
    if !generated.is_empty()
        && matches!(redirect, "www" | "non-www")
        && let Some(other_host) = www_counterpart(old_url, true)
            .and_then(|c| domains::UrlParts::parse(&c).map(|p| p.host))
        && let Some(counterpart) = current
            .iter()
            .find(|u| domains::UrlParts::parse(u).is_some_and(|p| p.host == other_host))
    {
        let parts = parts_json(counterpart);
        let host = text(&parts, "host");
        let moved = if host.to_lowercase().starts_with("www.") {
            format!("www.{generated}")
        } else {
            generated.to_owned()
        };
        let keep_port = stored_overrides(&a)
            .get(&domains::without_port(counterpart))
            .map(ToString::to_string)
            .unwrap_or_else(|| text(&parts, "port").to_owned());
        replacements.push((
            counterpart.clone(),
            compose(
                text(&parts, "scheme"),
                &moved,
                &keep_port,
                text(&parts, "path"),
            ),
        ));
    }
    let mut updated: Vec<String> = Vec::new();
    for url in &current {
        let next = replacements
            .iter()
            .find(|(from, _)| from == url)
            .map_or_else(|| url.clone(), |(_, to)| to.clone());
        if !updated.contains(&next) {
            updated.push(next);
        }
    }
    if redirect != original_redirect && matches!(redirect, "www" | "non-www") {
        for url in updated.clone() {
            if let Some(counterpart) = www_counterpart(&url, true) {
                let host = domains::UrlParts::parse(&counterpart).map(|p| p.host);
                let has = updated
                    .iter()
                    .any(|u| domains::UrlParts::parse(u).map(|p| p.host) == host);
                if !has {
                    updated.push(counterpart);
                }
            }
        }
    }
    let mut noindex = noindex_list(&a);
    for (previous, replacement) in &replacements {
        let previous_n = normalize_noindex(previous);
        let was = if previous == old_url {
            indexing == "noindex"
        } else {
            noindex.contains(&previous_n)
        };
        noindex.retain(|d| *d != previous_n);
        if was {
            noindex.push(replacement.clone());
        }
    }
    let owner = dns::Owner {
        morph: "App\\Models\\Application",
        id: a.id,
        uuid: &a.uuid,
        name: a.str("name").unwrap_or_default(),
        type_name: "application",
        team_id: a.team_id,
    };
    let previous_hostnames = dns::hostnames_before(&state, &owner).await;
    let (answer, warnings) = save_list(
        &state,
        &a,
        &updated,
        Save {
            service,
            check_conflicts: true,
            force_domains: flag(&body, "force_domains"),
            noindex: Some(noindex),
            redirect: Some(redirect),
            drop_overrides,
        },
    )
    .await?;
    if let Some(answer) = answer {
        return Ok(answer);
    }
    let mut warnings: Vec<String> = warnings.iter().map(ToString::to_string).collect();
    warnings.extend(dns::release_after_edit(&state, &owner, &previous_hostnames).await);
    Ok(ok(json!({"message": "Domain updated.", "warnings": warnings})).into_response())
}

/// `removeDomain()`.
async fn remove(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> ApiResult {
    let a = application(&state, &ctx, &kind, &uuid).await?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    if labels_writable(&a) {
        return Ok(toast(
            "Domains cannot be edited while container labels are writable.",
        ));
    }
    let url = text(&body, "url");
    let service = service_of(&body);
    let updated: Vec<String> = current_list(&a, service)
        .into_iter()
        .filter(|u| u != url)
        .collect();
    let owner = dns::Owner {
        morph: "App\\Models\\Application",
        id: a.id,
        uuid: &a.uuid,
        name: a.str("name").unwrap_or_default(),
        type_name: "application",
        team_id: a.team_id,
    };
    let (answer, warnings) = save_list(
        &state,
        &a,
        &updated,
        Save {
            service,
            check_conflicts: false,
            force_domains: false,
            noindex: None,
            redirect: None,
            drop_overrides: Vec::new(),
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
    warnings.extend(dns::release_after_remove(&state, &owner, url, delete_record).await);
    Ok(ok(json!({"message": "Domain removed.", "warnings": warnings})).into_response())
}

/// `generateUrl(server: $server, random: $random)`.
pub(super) async fn generated_url(
    state: &AppState,
    a: &ApplicationRow,
    random: &str,
) -> Result<Option<String>, ApiError> {
    let Some(server) = (match a.server_id() {
        Some(id) => servers::find_by_id(&state.db, id).await?,
        None => None,
    }) else {
        return Ok(None);
    };
    let settings = server.settings.clone().unwrap_or_default();
    let ip = server
        .record
        .get("ip")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let instance = coolify_db::instance::deployment_settings(&state.db).await?;
    let public = instance.public_ipv4.or(instance.public_ipv6);
    Ok(Some(
        domains::generate_url(
            settings.get("wildcard_domain").and_then(Value::as_str),
            &ip,
            public.as_deref(),
            random,
        )
        .await,
    ))
}

/// `generateDomain($service)`.
async fn generate(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> ApiResult {
    let a = application(&state, &ctx, &kind, &uuid).await?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    if labels_writable(&a) {
        return Ok(toast(
            "Domains cannot be edited while container labels are writable.",
        ));
    }
    let compose_mode = is_compose(&a);
    let services = compose_services(&a);
    let service = service_of(&body)
        .map(str::to_owned)
        .or_else(|| services.first().cloned());
    if compose_mode && service.is_none() {
        return Ok(toast("No compose service available for domain generation."));
    }
    let random = if compose_mode {
        coolify_domain::new_public_id()
    } else {
        a.uuid.clone()
    };
    let Some(url) = generated_url(&state, &a, &random).await? else {
        return Ok(toast("No server found for this application."));
    };
    let service = if compose_mode {
        service.as_deref()
    } else {
        None
    };
    let mut merged = current_list(&a, service);
    merged.push(url);
    // `syncRedirectDomainPairs()`.
    if matches!(redirect_of(&a, service).as_str(), "www" | "non-www") {
        for u in merged.clone() {
            if let Some(counterpart) = www_counterpart(&u, true) {
                let host = domains::UrlParts::parse(&counterpart).map(|p| p.host.to_lowercase());
                let has = merged
                    .iter()
                    .any(|m| domains::UrlParts::parse(m).map(|p| p.host.to_lowercase()) == host);
                if !has {
                    merged.push(counterpart);
                }
            }
        }
    }
    let (answer, warnings) = save_list(
        &state,
        &a,
        &merged,
        Save {
            service,
            check_conflicts: false,
            force_domains: false,
            noindex: None,
            redirect: None,
            drop_overrides: Vec::new(),
        },
    )
    .await?;
    if let Some(answer) = answer {
        return Ok(answer);
    }
    Ok(ok(json!({"message": "Domain generated.", "warnings": warnings})).into_response())
}

/// `regenerateEditingDomain()`: the new host.
async fn regenerate(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid)): Path<(String, String)>,
) -> ApiResult {
    let a = application(&state, &ctx, &kind, &uuid).await?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let Some(url) = generated_url(&state, &a, &coolify_domain::new_public_id()).await? else {
        return Ok(toast("No server found for this application."));
    };
    let Some(host) = domains::UrlParts::parse(&url)
        .map(|p| p.host)
        .filter(|h| !h.is_empty())
    else {
        return Ok(toast("Could not generate a domain."));
    };
    Ok(ok(json!({"host": host})).into_response())
}

/// `updateForceHttps()` and `updatePermanentRedirect()`.
async fn settings(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> ApiResult {
    let a = application(&state, &ctx, &kind, &uuid).await?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
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
        return Err(super::NOT_FOUND);
    };
    apps::update_settings(
        &state.db,
        a.id,
        &[(column, ColumnValue::Bool(value))],
        state.now(),
    )
    .await?;
    regenerate_labels(&state, a.id, false).await?;
    Ok(ok(json!({"message": message})).into_response())
}
