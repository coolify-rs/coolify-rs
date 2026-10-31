//! `Project\Application\Previews`, `Preview\Form` and `PreviewDomains`. DNS checks are not ported,
//! as on the domains page (`domains.rs`): new domains stay "Not checked".

use std::collections::BTreeMap;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Extension, Json, Router};
use coolify_db::application_ops::{self as ops, NewPreview};
use coolify_db::applications::{self as apps, ApplicationRow};
use coolify_db::records::Record;
use coolify_db::servers::{self, ColumnValue};
use coolify_jobs::applications::domains;
use coolify_jobs::applications::ops::{
    compose_service_names, generated_preview_domain, normalized_domains, preview_fqdn,
    preview_fqdn_compose_keeping,
};
use coolify_jobs::applications::queue::{QueueOptions, Queued};
use coolify_jobs::compose_parser::domains::{
    compose_domain_entry_string, get_compose_service_domain_string, get_fqdn_without_port,
};
use coolify_jobs::compose_parser::php;
use coolify_compat::yaml::PhpValue;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use super::domains::{
    available_ports, compose, compose_services, generated_url, inherited_port, parsed_compose,
    parts_json, split,
};
use super::find;
use crate::auth::AuthCtx;
use crate::response::{ApiError, ApiResult, ok};
use crate::state::AppState;

const PREVIEW_NOT_FOUND: ApiError = ApiError::Message(StatusCode::NOT_FOUND, "Preview not found.");

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/ui/resources/{kind}/{uuid}/previews", get(index))
        .route(
            "/ui/resources/{kind}/{uuid}/previews/settings",
            post(save_settings),
        )
        .route(
            "/ui/resources/{kind}/{uuid}/previews/template",
            post(save_template),
        )
        .route(
            "/ui/resources/{kind}/{uuid}/previews/template/reset",
            post(reset_template),
        )
        .route(
            "/ui/resources/{kind}/{uuid}/previews/pull-requests",
            post(load_prs),
        )
        .route("/ui/resources/{kind}/{uuid}/previews/add", post(add))
        .route("/ui/resources/{kind}/{uuid}/previews/deploy", post(deploy))
        .route(
            "/ui/resources/{kind}/{uuid}/previews/manual",
            post(add_docker_image_preview),
        )
        .route(
            "/ui/resources/{kind}/{uuid}/previews/{pr}/rebuild",
            post(force_deploy_without_cache),
        )
        .route(
            "/ui/resources/{kind}/{uuid}/previews/{pr}/stop",
            post(stop),
        )
        .route(
            "/ui/resources/{kind}/{uuid}/previews/{pr}/tag",
            post(save_tag),
        )
        .route(
            "/ui/resources/{kind}/{uuid}/previews/{pr}",
            delete(destroy),
        )
        .route(
            "/ui/resources/{kind}/{uuid}/previews/{pr}/domains/add",
            post(domain_add),
        )
        .route(
            "/ui/resources/{kind}/{uuid}/previews/{pr}/domains/generate",
            post(domain_generate),
        )
        .route(
            "/ui/resources/{kind}/{uuid}/previews/{pr}/domains/update",
            post(domain_update),
        )
        .route(
            "/ui/resources/{kind}/{uuid}/previews/{pr}/domains/remove",
            post(domain_remove),
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

/// `$this->authorize('update' | 'deploy' | 'delete', $this->application)`: all three ask for an
/// admin of the application's team.
async fn admin_application(
    state: &AppState,
    ctx: &AuthCtx,
    kind: &str,
    uuid: &str,
) -> Result<ApplicationRow, ApiError> {
    let a = application(state, ctx, kind, uuid).await?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    Ok(a)
}

fn toast(message: impl Into<String>) -> Response {
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        Json(json!({"message": message.into()})),
    )
        .into_response()
}

fn answer(message: &str) -> Response {
    ok(json!({"message": message})).into_response()
}

fn field_error(field: &str, message: &str) -> Response {
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        Json(json!({"message": message, "errors": {field: [message]}})),
    )
        .into_response()
}

fn is_compose(a: &ApplicationRow) -> bool {
    a.str("build_pack") == Some("dockercompose")
}

fn is_docker_image(a: &ApplicationRow) -> bool {
    a.str("build_pack") == Some("dockerimage")
}

fn text<'a>(r: &'a Record, key: &str) -> Option<&'a str> {
    r.get(key).and_then(Value::as_str)
}

fn id_of(r: &Record) -> i64 {
    r.get("id").and_then(Value::as_i64).unwrap_or_default()
}

fn pull_request_of(r: &Record) -> i64 {
    r.get("pull_request_id")
        .and_then(Value::as_i64)
        .unwrap_or_default()
}

fn body_text<'a>(body: &'a Value, key: &str) -> &'a str {
    body.get(key).and_then(Value::as_str).unwrap_or_default()
}

fn body_port(body: &Value) -> String {
    match body.get("port") {
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::String(s)) => s.clone(),
        _ => String::new(),
    }
}

/// `int $pull_request_id` of the Livewire actions.
fn body_pull_request(body: &Value) -> Option<i64> {
    match body.get("pull_request_id") {
        Some(Value::Number(n)) => n.as_i64(),
        Some(Value::String(s)) => s.trim().parse().ok(),
        _ => None,
    }
}

/// `$preview->domain_port_overrides`.
fn overrides_of(preview: &Record) -> BTreeMap<String, i64> {
    let Some(Value::Object(map)) = ops::json_column(preview, "domain_port_overrides") else {
        return BTreeMap::new();
    };
    map.iter()
        .filter_map(|(k, v)| {
            v.as_i64()
                .or_else(|| v.as_str()?.parse().ok())
                .map(|p| (k.clone(), p))
        })
        .collect()
}

/// `statusKey($url, $service)`.
fn status_key(url: &str, service: Option<&str>) -> String {
    let digest = Sha256::digest(format!("{url}|{}", service.unwrap_or_default()));
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

#[derive(Debug, Clone, PartialEq)]
struct Row {
    url: String,
    service: Option<String>,
}

/// `refreshDomains()`: the rows of a preview.
fn rows_of(a: &ApplicationRow, preview: &Record) -> Vec<Row> {
    if is_compose(a) {
        let stored = php::json_array(text(preview, "docker_compose_domains"));
        let mut rows = Vec::new();
        for (service, entry) in &stored {
            let service = service.as_string();
            for url in split(compose_domain_entry_string(entry).as_deref()) {
                rows.push(Row {
                    url,
                    service: Some(service.clone()),
                });
            }
        }
        return rows;
    }
    split(text(preview, "fqdn"))
        .into_iter()
        .map(|url| Row { url, service: None })
        .collect()
}

/// `makeRow()` with `effectiveDomainInternalPort()`.
fn row_json(a: &ApplicationRow, preview: &Record, row: &Row) -> Value {
    let service = row.service.as_deref();
    let statuses = ops::json_column(preview, "domain_dns_statuses").unwrap_or(Value::Null);
    let status = statuses.get(status_key(&row.url, service));
    let canonical = domains::without_port(&row.url);
    let overridden = overrides_of(preview).get(&canonical).copied();
    let legacy = domains::UrlParts::parse(&row.url)
        .and_then(|p| p.port_number())
        .filter(|p| *p > 0);
    let (port, has_override) = match (overridden, legacy) {
        (Some(p), _) | (None, Some(p)) => (Some(p), true),
        _ => (inherited_port(a, service), false),
    };
    let mut redirect = "both".to_owned();
    if is_compose(a)
        && let Some(service) = service
    {
        let version: i64 = a
            .str("compose_parsing_version")
            .and_then(|v| v.parse().ok())
            .unwrap_or_default();
        let stored = if version >= 3 {
            text(preview, "docker_compose_domains")
        } else {
            a.str("docker_compose_domains")
        };
        let entries = php::json_array(stored);
        let found = entries
            .iter()
            .find(|(k, _)| k.as_string() == service)
            .and_then(|(_, entry)| php::get(entry, "redirect"));
        if let Some(PhpValue::Str(s)) = found
            && matches!(s.as_str(), "www" | "non-www" | "both")
        {
            redirect = s.clone();
        }
    }
    let mut parts = parts_json(&row.url);
    if has_override && let Value::Object(m) = &mut parts {
        m.insert("port".into(), json!(port.unwrap_or_default().to_string()));
    }
    json!({
        "url": row.url,
        "service": service,
        "public_url": get_fqdn_without_port(&row.url),
        "redirect": redirect,
        "internal_port": port,
        "has_port_override": has_override,
        "dns_status": status.and_then(|s| s.get("status")).and_then(Value::as_str).unwrap_or("pending"),
        "dns_message": status.and_then(|s| s.get("message")).and_then(Value::as_str).unwrap_or("DNS has not been checked yet."),
        "key": status_key(&row.url, service),
        "editing_parts": parts,
    })
}

/// `stoppedAfterRestartLimit()` of `HasRestartLimit` and `restartLimitMaximum()`.
fn restart_limit(a: &ApplicationRow, preview: &Record) -> Option<i64> {
    let exited = text(preview, "status").unwrap_or_default().starts_with("exited");
    let reached = preview
        .get("restart_limit_reached")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !(exited && reached) {
        return None;
    }
    let maximum = a
        .int("max_restart_count")
        .or_else(|| preview.get("max_restart_count").and_then(Value::as_i64))
        .unwrap_or_default();
    let count = preview
        .get("restart_count")
        .and_then(Value::as_i64)
        .unwrap_or_default();
    Some(count.max(maximum))
}

fn preview_json(a: &ApplicationRow, preview: &Record) -> Value {
    let rows: Vec<Value> = rows_of(a, preview)
        .iter()
        .map(|r| row_json(a, preview, r))
        .collect();
    json!({
        "id": id_of(preview),
        "pull_request_id": pull_request_of(preview),
        "status": text(preview, "status"),
        "fqdn": text(preview, "fqdn"),
        "pull_request_html_url": text(preview, "pull_request_html_url"),
        "docker_registry_image_tag": text(preview, "docker_registry_image_tag"),
        "restart_limit": restart_limit(a, preview),
        "rows": rows,
    })
}

/// `Preview\Form::generateRealUrl()`.
fn real_url(a: &ApplicationRow) -> String {
    let template = a.str("preview_url_template").unwrap_or_default();
    let Some(fqdn) = a.str("fqdn").filter(|f| !f.is_empty() && *f != "0") else {
        return template.to_owned();
    };
    let first = fqdn.split(',').next().unwrap_or(fqdn);
    let host = domains::UrlParts::parse(first)
        .map(|p| p.host)
        .unwrap_or_default();
    template.replace("{{domain}}", &host)
}

/// `$application->git_based()`.
fn git_based(a: &ApplicationRow) -> bool {
    a.str("dockerfile").is_none_or(str::is_empty) && !is_docker_image(a)
}

const GITHUB_APP: &str = r"App\Models\GithubApp";

/// `Previews::render` with `Preview\Form`.
async fn index(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid)): Path<(String, String)>,
) -> ApiResult {
    let a = application(&state, &ctx, &kind, &uuid).await?;
    let previews: Vec<Value> = ops::previews(&state.db, a.id)
        .await?
        .iter()
        .map(|p| preview_json(&a, p))
        .collect();
    let server = match a.server_id() {
        Some(id) => servers::find_by_id(&state.db, id).await?,
        None => None,
    };
    let additional = ops::additional_networks(&state.db, a.id).await?.len();
    Ok(ok(json!({
        "can_update": ctx.is_team_admin(),
        "git_based": git_based(&a),
        "is_github_app_source": a.str("source_type") == Some(GITHUB_APP),
        "is_compose": is_compose(&a),
        "is_docker_image": is_docker_image(&a),
        "enabled": a.setting_bool("is_preview_deployments_enabled"),
        "public_enabled": a.setting_bool("is_pr_deployments_public_enabled"),
        "preview_url_template": real_url(&a),
        "has_additional_servers": additional > 0,
        "server_name": server.as_ref().and_then(|s| s.record.get("name")).and_then(Value::as_str),
        "force_https": a.setting_bool("is_force_https_enabled"),
        "compose_services": if is_compose(&a) { compose_services(&a) } else { Vec::new() },
        "previews": previews,
    }))
    .into_response())
}

/// `savePreviewSettings()` and `togglePreviewDeployments()`.
async fn save_settings(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> ApiResult {
    let a = admin_application(&state, &ctx, &kind, &uuid).await?;
    let enabled = body
        .get("enabled")
        .and_then(Value::as_bool)
        .unwrap_or_else(|| a.setting_bool("is_preview_deployments_enabled"));
    let public = body
        .get("public_enabled")
        .and_then(Value::as_bool)
        .unwrap_or_else(|| a.setting_bool("is_pr_deployments_public_enabled"));
    apps::update_settings(
        &state.db,
        a.id,
        &[
            ("is_preview_deployments_enabled", ColumnValue::Bool(enabled)),
            ("is_pr_deployments_public_enabled", ColumnValue::Bool(public)),
        ],
        state.now(),
    )
    .await?;
    tracing::info!(target: "audit", event = "ui.application.settings_updated", team_id = a.team_id, application_uuid = %a.uuid, "preview settings saved");
    Ok(answer("Settings saved."))
}

async fn store_template(state: &AppState, a: &ApplicationRow, template: &str) -> ApiResult {
    apps::update_if_changed(
        &state.db,
        a.id,
        &[(
            "preview_url_template",
            ColumnValue::Text(Some(template.to_owned())),
        )],
        state.now(),
    )
    .await?;
    let fresh = apps::find_in_team(&state.db, a.team_id, &a.uuid)
        .await?
        .ok_or(super::NOT_FOUND)?;
    Ok(ok(json!({
        "message": "Preview url template updated.",
        "preview_url_template": real_url(&fresh),
    }))
    .into_response())
}

/// `Preview\Form::submit`.
async fn save_template(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> ApiResult {
    let a = admin_application(&state, &ctx, &kind, &uuid).await?;
    let template = body_text(&body, "template");
    if template.trim().is_empty() {
        return Ok(toast("The preview url template field is required."));
    }
    store_template(&state, &a, &template.replace(' ', "")).await
}

/// `Preview\Form::resetToDefault`.
async fn reset_template(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid)): Path<(String, String)>,
) -> ApiResult {
    let a = admin_application(&state, &ctx, &kind, &uuid).await?;
    store_template(&state, &a, "{{pr_id}}.{{domain}}").await
}

/// `load_prs()`.
async fn load_prs(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid)): Path<(String, String)>,
) -> ApiResult {
    let a = admin_application(&state, &ctx, &kind, &uuid).await?;
    let source = match (a.str("source_type"), a.int("source_id")) {
        (Some(GITHUB_APP), Some(id)) => coolify_jobs::github::source_by_id(&state.db, id).await?,
        _ => None,
    };
    let Some(source) = source else {
        return Ok(toast(
            "Loading pull requests is only supported for GitHub App sources.",
        ));
    };
    let repository = a.str("git_repository").unwrap_or_default();
    match coolify_jobs::github::pull_requests(
        &state.db,
        state.encrypter.as_ref(),
        &source,
        repository,
    )
    .await
    {
        Ok((remaining, pull_requests)) => Ok(ok(json!({
            "rate_limit_remaining": remaining,
            "pull_requests": pull_requests,
        }))
        .into_response()),
        Err(message) => Ok(toast(message)),
    }
}

/// `ApplicationPreview::create([...])` of `add` and `deploy`, or the row that exists.
async fn find_or_create(
    state: &AppState,
    a: &ApplicationRow,
    pull_request_id: i64,
    html_url: Option<&str>,
    tag: Option<&str>,
) -> Result<(Option<Record>, bool), ApiError> {
    if let Some(found) = ops::preview(&state.db, a.id, pull_request_id).await? {
        return Ok((Some(found), false));
    }
    let tag = tag.filter(|t| !t.is_empty());
    let creates = if is_compose(a) {
        html_url.is_some()
    } else {
        html_url.is_some() || (is_docker_image(a) && tag.is_some())
    };
    if !creates {
        return Ok((None, false));
    }
    let created = ops::create_preview(
        &state.db,
        &NewPreview {
            application_id: a.id,
            pull_request_id,
            pull_request_html_url: html_url.unwrap_or_default().to_owned(),
            git_type: None,
            docker_compose_domains: if is_compose(a) {
                a.str("docker_compose_domains").map(str::to_owned)
            } else {
                None
            },
            docker_registry_image_tag: if is_compose(a) {
                None
            } else {
                tag.map(str::to_owned)
            },
        },
        state.now(),
    )
    .await?;
    tracing::info!(target: "audit", event = "ui.application.preview_created", team_id = a.team_id, application_uuid = %a.uuid, application_name = a.str("name"), pull_request_id, "preview created");
    Ok((Some(created), true))
}

/// `saveDockerTagWithAudit()`.
async fn save_docker_tag(
    state: &AppState,
    a: &ApplicationRow,
    preview: &Record,
    tag: Option<&str>,
) -> Result<(), ApiError> {
    let Some(tag) = tag.filter(|t| !t.is_empty() && is_docker_image(a)) else {
        return Ok(());
    };
    if text(preview, "docker_registry_image_tag") == Some(tag) {
        return Ok(());
    }
    ops::update_preview(
        &state.db,
        id_of(preview),
        &[(
            "docker_registry_image_tag",
            ColumnValue::Text(Some(tag.to_owned())),
        )],
        state.now(),
    )
    .await?;
    tracing::info!(target: "audit", event = "ui.application.preview_updated", team_id = a.team_id, application_uuid = %a.uuid, pull_request_id = pull_request_of(preview), changed_fields = "docker_registry_image_tag", "preview updated");
    Ok(())
}

async fn save_generated(
    state: &AppState,
    preview: &Record,
    fqdn: Option<String>,
    overrides: Option<String>,
    compose: Option<String>,
) -> Result<(), ApiError> {
    let mut values = vec![
        ("fqdn", ColumnValue::Text(fqdn)),
        ("domain_port_overrides", ColumnValue::Text(overrides)),
    ];
    if let Some(compose) = compose {
        values.push(("docker_compose_domains", ColumnValue::Text(Some(compose))));
    }
    ops::update_preview(&state.db, id_of(preview), &values, state.now()).await?;
    Ok(())
}

/// `generate_preview_fqdn_compose($generateWithoutApplicationDomain, $keepExistingDomains)`.
async fn generate_compose(
    state: &AppState,
    a: &ApplicationRow,
    preview: &Record,
    keep: bool,
) -> Result<(), ApiError> {
    let names = compose_service_names(a.str("docker_compose_raw").unwrap_or_default());
    let own = overrides_of(preview);
    let generated = preview_fqdn_compose_keeping(
        a.str("docker_compose_domains"),
        text(preview, "docker_compose_domains"),
        &names,
        a.str("preview_url_template").unwrap_or_default(),
        pull_request_of(preview),
        &application_overrides(a),
        keep.then_some(&own),
    );
    save_generated(
        state,
        preview,
        generated.fqdn,
        generated.overrides,
        generated.compose,
    )
    .await
}

fn application_overrides(a: &ApplicationRow) -> BTreeMap<String, i64> {
    let Some(raw) = a.raw.get("domain_port_overrides") else {
        return BTreeMap::new();
    };
    let parsed = match raw {
        Value::String(s) => serde_json::from_str(s).unwrap_or(Value::Null),
        other => other.clone(),
    };
    let Value::Object(map) = parsed else {
        return BTreeMap::new();
    };
    map.iter()
        .filter_map(|(k, v)| {
            v.as_i64()
                .or_else(|| v.as_str()?.parse().ok())
                .map(|p| (k.clone(), p))
        })
        .collect()
}

/// `generate_preview_fqdn(generateWithoutApplicationDomain: true, $keepExistingDomains)`.
async fn generate_single(
    state: &AppState,
    a: &ApplicationRow,
    preview: &Record,
    keep: bool,
) -> Result<(), ApiError> {
    if keep && text(preview, "fqdn").is_some_and(|f| !f.is_empty()) {
        let (fqdn, overrides) = normalized_domains(text(preview, "fqdn"), &overrides_of(preview));
        return save_generated(state, preview, fqdn, overrides, None).await;
    }
    let own_fqdn = a.str("fqdn").filter(|f| !f.is_empty() && *f != "0");
    let source = match own_fqdn {
        Some(f) => Some(f.to_owned()),
        None => generated_url(state, a, &a.uuid).await?,
    };
    let generated = preview_fqdn(
        source.as_deref(),
        a.str("preview_url_template").unwrap_or_default(),
        pull_request_of(preview),
        &application_overrides(a),
    );
    if let Some(generated) = generated {
        save_generated(state, preview, generated.fqdn, generated.overrides, None).await?;
    }
    Ok(())
}

/// `add($pull_request_id, $pull_request_html_url, $docker_registry_image_tag)`: `true` when the
/// non-Compose branch reports "Preview added."
async fn add_preview(
    state: &AppState,
    a: &ApplicationRow,
    pull_request_id: i64,
    html_url: Option<&str>,
    tag: Option<&str>,
) -> Result<Result<bool, Response>, ApiError> {
    let (found, created) = find_or_create(state, a, pull_request_id, html_url, tag).await?;
    // `$found->generate_...` on null, caught by the `try`.
    let Some(found) = found else {
        return Ok(Err(toast(if is_compose(a) {
            "Call to a member function generate_preview_fqdn_compose() on null"
        } else {
            "Call to a member function generate_preview_fqdn() on null"
        })));
    };
    if is_compose(a) {
        generate_compose(state, a, &found, !created).await?;
        return Ok(Ok(false));
    }
    save_docker_tag(state, a, &found, tag).await?;
    let found = ops::preview_by_id(&state.db, id_of(&found))
        .await?
        .ok_or(ApiError::Internal)?;
    generate_single(state, a, &found, true).await?;
    Ok(Ok(true))
}

/// `add()`.
async fn add(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> ApiResult {
    let a = admin_application(&state, &ctx, &kind, &uuid).await?;
    let Some(pr) = body_pull_request(&body) else {
        return Err(ApiError::Internal);
    };
    let html_url = body.get("html_url").and_then(Value::as_str);
    match add_preview(&state, &a, pr, html_url, None).await? {
        Err(failure) => Ok(failure),
        Ok(true) => Ok(answer("Preview added.")),
        Ok(false) => Ok(ok(json!({})).into_response()),
    }
}

/// `queue_application_deployment()` as `deploy()` answers it.
async fn queue_preview(
    state: &AppState,
    a: &ApplicationRow,
    pull_request_id: i64,
    force_rebuild: bool,
    tag: Option<&str>,
) -> ApiResult {
    let found = ops::preview(&state.db, a.id, pull_request_id).await?;
    let git_type = found
        .as_ref()
        .and_then(|p| text(p, "git_type"))
        .map(str::to_owned);
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    let deployment_uuid = coolify_domain::new_public_id();
    let result = jobs
        .queue_application_deployment(
            a,
            &deployment_uuid,
            QueueOptions {
                force_rebuild,
                pull_request_id,
                git_type,
                docker_registry_image_tag: tag.map(str::to_owned),
                ..Default::default()
            },
        )
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "preview deployment could not be queued");
            ApiError::Internal
        })?;
    Ok(match result {
        Queued::QueueFull => ok(json!({"toast": "error", "title": "Deployment queue full", "message": result.message()})),
        Queued::Skipped { .. } => ok(json!({"toast": "success", "title": "Deployment skipped", "message": result.message()})),
        Queued::Queued { deployment_uuid } => ok(json!({"deployment_uuid": deployment_uuid})),
    }
    .into_response())
}

/// `deploy()` and `add_and_deploy()`.
async fn deploy(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> ApiResult {
    let a = admin_application(&state, &ctx, &kind, &uuid).await?;
    let Some(pr) = body_pull_request(&body) else {
        return Err(ApiError::Internal);
    };
    let html_url = body.get("html_url").and_then(Value::as_str);
    let tag = body.get("tag").and_then(Value::as_str);
    if body.get("add").and_then(Value::as_bool).unwrap_or(false)
        && let Err(failure) = add_preview(&state, &a, pr, html_url, tag).await?
    {
        return Ok(failure);
    }
    let (found, _) = find_or_create(&state, &a, pr, html_url, tag).await?;
    if let Some(found) = &found {
        save_docker_tag(&state, &a, found, tag).await?;
    }
    queue_preview(&state, &a, pr, false, tag).await
}

/// `force_deploy_without_cache()`.
async fn force_deploy_without_cache(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid, pr)): Path<(String, String, i64)>,
) -> ApiResult {
    let a = admin_application(&state, &ctx, &kind, &uuid).await?;
    let found = ops::preview(&state.db, a.id, pr).await?;
    let tag = if is_docker_image(&a) {
        found
            .as_ref()
            .and_then(|p| text(p, "docker_registry_image_tag"))
    } else {
        None
    };
    queue_preview(&state, &a, pr, true, tag).await
}

/// `addDockerImagePreview()`.
async fn add_docker_image_preview(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> ApiResult {
    let a = admin_application(&state, &ctx, &kind, &uuid).await?;
    // `validateOnly('manualPullRequestId')`: outside a `try`, so a message under the field.
    let raw = body_text(&body, "pull_request_id").trim();
    let pr = if raw.is_empty() {
        None
    } else {
        match raw.parse::<i64>() {
            Err(_) => {
                return Ok(field_error(
                    "pull_request_id",
                    "The manual pull request id field must be an integer.",
                ));
            }
            Ok(id) if id < 1 => {
                return Ok(field_error(
                    "pull_request_id",
                    "The manual pull request id field must be at least 1.",
                ));
            }
            Ok(id) => Some(id),
        }
    };
    if !is_docker_image(&a) {
        return Ok(toast(
            "Manual Docker Image previews are only available for Docker Image applications.",
        ));
    }
    let tag = body_text(&body, "tag").trim();
    let (Some(pr), false) = (pr, tag.is_empty()) else {
        return Ok(toast("Both pull request id and docker tag are required."));
    };
    if let Err(failure) = add_preview(&state, &a, pr, None, Some(tag)).await? {
        return Ok(failure);
    }
    queue_preview(&state, &a, pr, false, Some(tag)).await
}

async fn preview_of(state: &AppState, a: &ApplicationRow, pr: i64) -> Result<Record, ApiError> {
    ops::preview(&state.db, a.id, pr)
        .await?
        .ok_or(PREVIEW_NOT_FOUND)
}

/// `stop()`.
async fn stop(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid, pr)): Path<(String, String, i64)>,
) -> ApiResult {
    let a = admin_application(&state, &ctx, &kind, &uuid).await?;
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    if let Err(message) = jobs.stop_preview_deployment(&a, pr).await {
        return Ok(toast(message));
    }
    if let Some(preview) = ops::preview(&state.db, a.id, pr).await? {
        ops::update_preview(
            &state.db,
            id_of(&preview),
            &[("status", ColumnValue::Text(Some("exited".to_owned())))],
            state.now(),
        )
        .await?;
    }
    tracing::info!(target: "audit", event = "ui.application.preview_stopped", team_id = a.team_id, application_uuid = %a.uuid, application_name = a.str("name"), pull_request_id = pr, "preview stopped");
    Ok(answer("Preview Deployment stopped."))
}

/// `delete()`: the row goes first and the containers afterwards.
async fn destroy(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid, pr)): Path<(String, String, i64)>,
) -> ApiResult {
    let a = admin_application(&state, &ctx, &kind, &uuid).await?;
    let Some(preview) = ops::preview(&state.db, a.id, pr).await? else {
        return Ok(toast("Preview not found."));
    };
    let preview_id = id_of(&preview);
    ops::soft_delete_preview(&state.db, preview_id, state.now()).await?;
    tracing::info!(target: "audit", event = "ui.application.preview_deleted", team_id = a.team_id, application_uuid = %a.uuid, application_name = a.str("name"), pull_request_id = pr, "preview deleted");
    if let Some(jobs) = state.jobs.clone() {
        tokio::spawn(async move {
            jobs.cleanup_preview_deployment(&a, pr, preview_id).await;
        });
    }
    Ok(answer(
        "Preview deletion started. It may take a few moments to complete.",
    ))
}

/// `save_preview()`.
async fn save_tag(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid, pr)): Path<(String, String, i64)>,
    Json(body): Json<Value>,
) -> ApiResult {
    let a = admin_application(&state, &ctx, &kind, &uuid).await?;
    let preview = preview_of(&state, &a, pr).await?;
    let tag = body.get("tag").and_then(Value::as_str);
    if text(&preview, "docker_registry_image_tag") != tag {
        ops::update_preview(
            &state.db,
            id_of(&preview),
            &[(
                "docker_registry_image_tag",
                ColumnValue::Text(tag.map(str::to_owned)),
            )],
            state.now(),
        )
        .await?;
        tracing::info!(target: "audit", event = "ui.application.preview_updated", team_id = a.team_id, application_uuid = %a.uuid, pull_request_id = pr, changed_fields = "docker_registry_image_tag", "preview updated");
    }
    Ok(answer(
        "Preview saved.<br><br>Do not forget to redeploy the preview to apply the changes.",
    ))
}

// ---------------------------------------------------------------------------
// `PreviewDomains`

/// `persistDomains()` and `persistDnsStatuses()`: `None` when saved, the toast otherwise.
async fn persist(
    state: &AppState,
    a: &ApplicationRow,
    preview: &Record,
    rows: &[Row],
    overrides: BTreeMap<String, i64>,
) -> Result<Option<Response>, ApiError> {
    let mut values: Vec<(&'static str, ColumnValue)> = Vec::new();
    let fqdn = if is_compose(a) {
        if parsed_compose(a).is_none() {
            return Ok(Some(toast(
                "Compose configuration could not be parsed. Preview domains were not changed.",
            )));
        }
        let services = compose_services(a);
        let existing = php::json_array(text(preview, "docker_compose_domains"));
        let valid: Vec<&Row> = rows
            .iter()
            .filter(|r| r.service.as_ref().is_some_and(|s| services.contains(s)))
            .collect();
        let joined = valid
            .iter()
            .map(|r| r.url.as_str())
            .collect::<Vec<_>>()
            .join(",");
        let (fqdn, normalized) =
            normalized_domains((!joined.is_empty()).then_some(joined.as_str()), &overrides);
        let mut domains_json = Map::new();
        for service in &services {
            let mut entry = match existing.iter().find(|(k, _)| k.as_string() == *service) {
                Some((_, PhpValue::Array(items))) => {
                    php_entry_to_json(&PhpValue::Array(items.clone()))
                }
                _ => Map::new(),
            };
            // `withoutPort` of every URL of the service.
            let urls = valid
                .iter()
                .filter(|r| r.service.as_deref() == Some(service.as_str()))
                .map(|r| domains::without_port(&r.url))
                .collect::<Vec<_>>()
                .join(",");
            entry.insert("domain".to_owned(), Value::String(urls));
            domains_json.insert(service.clone(), Value::Object(entry));
        }
        let encoded = php::json_encode(&PhpValue::Array(php::json_array(Some(
            &Value::Object(domains_json).to_string(),
        ))));
        values.push(("docker_compose_domains", ColumnValue::Text(Some(encoded))));
        values.push(("domain_port_overrides", ColumnValue::Text(normalized)));
        fqdn
    } else {
        let joined = rows
            .iter()
            .map(|r| r.url.as_str())
            .collect::<Vec<_>>()
            .join(",");
        let (fqdn, normalized) =
            normalized_domains((!joined.is_empty()).then_some(joined.as_str()), &overrides);
        values.push(("domain_port_overrides", ColumnValue::Text(normalized)));
        fqdn
    };
    values.push(("fqdn", ColumnValue::Text(fqdn)));

    // `persistDnsStatuses()`: the rows without their port, with what they had.
    let stored = ops::json_column(preview, "domain_dns_statuses").unwrap_or(Value::Null);
    let mut statuses = Map::new();
    for row in rows {
        let url = domains::without_port(&row.url);
        let key = status_key(&url, row.service.as_deref());
        let status = stored.get(&key).cloned().unwrap_or_else(|| {
            json!({"status": "pending", "message": "DNS has not been checked yet.", "check_id": null})
        });
        statuses.insert(key, status);
    }
    values.push((
        "domain_dns_statuses",
        ColumnValue::Text((!statuses.is_empty()).then(|| Value::Object(statuses).to_string())),
    ));
    ops::update_preview(&state.db, id_of(preview), &values, state.now()).await?;
    tracing::info!(target: "audit", event = "ui.application.preview_updated", team_id = a.team_id, application_uuid = %a.uuid, application_name = a.str("name"), pull_request_id = pull_request_of(preview), "preview domains updated");
    Ok(None)
}

fn php_entry_to_json(entry: &PhpValue) -> Map<String, Value> {
    match serde_json::from_str(&php::json_encode(entry)) {
        Ok(Value::Object(map)) => map,
        _ => Map::new(),
    }
}

/// `validatedDomain()`: the URL, or the message under the host field.
fn validated_domain(body: &Value, port: &str) -> Result<String, Response> {
    let domain = compose(
        body_text(body, "scheme"),
        body_text(body, "host"),
        port,
        body_text(body, "path"),
    );
    if let Some(first) = domains::validate_domains(&domain).into_iter().next() {
        return Err(field_error("domain", &first));
    }
    domains::normalize_domains(Some(&domain))
        .ok_or_else(|| field_error("domain", "Please enter a valid domain URL."))
}

/// `addDomain()`.
async fn domain_add(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid, pr)): Path<(String, String, i64)>,
    Json(body): Json<Value>,
) -> ApiResult {
    let a = admin_application(&state, &ctx, &kind, &uuid).await?;
    let preview = preview_of(&state, &a, pr).await?;
    let service = body
        .get("service")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let service = if is_compose(&a) {
        let services = compose_services(&a);
        match service.filter(|s| services.iter().any(|n| n == s)) {
            Some(s) => Some(s),
            None => {
                return Ok(field_error("service", "Select a valid Compose service."));
            }
        }
    } else {
        None
    };
    let port = body_port(&body);
    let domain = match validated_domain(&body, &port) {
        Ok(d) => d,
        Err(failure) => return Ok(failure),
    };
    let canonical = domains::without_port(&domain);
    let mut rows = rows_of(&a, &preview);
    if rows
        .iter()
        .any(|r| domains::without_port(&r.url) == canonical && r.service.as_deref() == service)
    {
        return Ok(field_error("domain", "This domain is already configured."));
    }
    let wanted = port.trim().parse::<i64>().ok().filter(|p| *p > 0);
    if let Some(p) = wanted
        && !body
            .get("force_port")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        && !available_ports(&a, service).contains(&p)
    {
        return Ok(ok(json!({"port_warning": p})).into_response());
    }
    rows.push(Row {
        url: domain,
        service: service.map(str::to_owned),
    });
    if let Some(failure) = persist(&state, &a, &preview, &rows, overrides_of(&preview)).await? {
        return Ok(failure);
    }
    Ok(answer("Domain added."))
}

/// `generateComposeDomains($service)`.
async fn generate_compose_domains(
    state: &AppState,
    a: &ApplicationRow,
    preview: &Record,
    service: &str,
    overrides: &mut BTreeMap<String, i64>,
) -> Result<Vec<String>, ApiError> {
    let app_domains = php::json_array(a.str("docker_compose_domains"));
    let mut source = get_compose_service_domain_string(&app_domains, service)
        .filter(|d| !d.is_empty() && d != "0");
    if source.is_none() {
        source = generated_url(state, a, &format!("{}-{}", php::slug(service), a.uuid)).await?;
    }
    let template = a.str("preview_url_template").unwrap_or_default();
    let mut urls = Vec::new();
    for domain in split(source.as_deref()) {
        let (url, port) = generated_preview_domain(
            template,
            pull_request_of(preview),
            &application_overrides(a),
            &domain,
        );
        if let Some(port) = port {
            overrides.insert(url.clone(), port);
        }
        urls.push(url);
    }
    Ok(urls)
}

/// `generateDomain()`.
async fn domain_generate(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid, pr)): Path<(String, String, i64)>,
    Json(body): Json<Value>,
) -> ApiResult {
    let a = admin_application(&state, &ctx, &kind, &uuid).await?;
    let preview = preview_of(&state, &a, pr).await?;
    if is_compose(&a) {
        let mut rows = rows_of(&a, &preview);
        let service = body
            .get("service")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .or_else(|| rows.first().and_then(|r| r.service.clone()));
        let Some(service) = service else {
            generate_compose(&state, &a, &preview, false).await?;
            return Ok(answer("Domain generated."));
        };
        let mut overrides = overrides_of(&preview);
        for url in generate_compose_domains(&state, &a, &preview, &service, &mut overrides).await? {
            let canonical = domains::without_port(&url);
            let exists = rows.iter().any(|r| {
                domains::without_port(&r.url) == canonical
                    && r.service.as_deref() == Some(service.as_str())
            });
            if !exists {
                rows.push(Row {
                    url,
                    service: Some(service.clone()),
                });
            }
        }
        if let Some(failure) = persist(&state, &a, &preview, &rows, overrides).await? {
            return Ok(failure);
        }
    } else {
        generate_single(&state, &a, &preview, false).await?;
    }
    Ok(answer("Domain generated."))
}

/// `updateDomain()`.
async fn domain_update(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid, pr)): Path<(String, String, i64)>,
    Json(body): Json<Value>,
) -> ApiResult {
    let a = admin_application(&state, &ctx, &kind, &uuid).await?;
    let preview = preview_of(&state, &a, pr).await?;
    let old_url = body_text(&body, "old_url");
    let service = body
        .get("service")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let mut rows = rows_of(&a, &preview);
    let Some(index) = rows
        .iter()
        .position(|r| r.url == old_url && r.service.as_deref() == service)
    else {
        return Ok(ok(json!({})).into_response());
    };
    let port = body_port(&body);
    let domain = match validated_domain(&body, &port) {
        Ok(d) => d,
        Err(failure) => return Ok(failure),
    };
    let mut overrides = overrides_of(&preview);
    // `currentRowPort()`.
    let current = overrides
        .get(&domains::without_port(old_url))
        .copied()
        .filter(|p| *p > 0)
        .or_else(|| domains::UrlParts::parse(old_url).and_then(|p| p.port_number()));
    let wanted = port.trim().parse::<i64>().ok().filter(|p| *p > 0);
    if let Some(p) = wanted
        && !body
            .get("force_port")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        && current != Some(p)
        && !available_ports(&a, service).contains(&p)
    {
        return Ok(ok(json!({"port_warning": p})).into_response());
    }
    if port.trim().is_empty() {
        overrides.remove(&domains::without_port(old_url));
        overrides.remove(&domains::without_port(&domain));
    }
    rows[index].url = domain;
    if let Some(failure) = persist(&state, &a, &preview, &rows, overrides).await? {
        return Ok(failure);
    }
    Ok(answer("Domain updated."))
}

/// `removeDomainByKey()`.
async fn domain_remove(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid, pr)): Path<(String, String, i64)>,
    Json(body): Json<Value>,
) -> ApiResult {
    let a = admin_application(&state, &ctx, &kind, &uuid).await?;
    let preview = preview_of(&state, &a, pr).await?;
    let key = body_text(&body, "key");
    let mut rows = rows_of(&a, &preview);
    let Some(index) = rows
        .iter()
        .position(|r| status_key(&r.url, r.service.as_deref()) == key)
    else {
        return Ok(ok(json!({})).into_response());
    };
    rows.remove(index);
    if let Some(failure) = persist(&state, &a, &preview, &rows, overrides_of(&preview)).await? {
        return Ok(failure);
    }
    Ok(answer("Domain removed."))
}
