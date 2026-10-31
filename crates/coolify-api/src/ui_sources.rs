//! The sources pages: `routes/web.php`'s `source.all`, `Source\{Github,Gitlab,Gitea}\{Create,Change}`
//! and the browser callbacks `Webhook\{Github,Gitlab,Gitea}::{redirect,install}`.
//!
//! The setup and OAuth states live in this process (upstream keeps them in its cache for an hour).

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use axum::Extension;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use coolify_compat::sshkey::{format_private_key, inspect};
use coolify_db::private_keys::{self, NewKey};
use coolify_db::records::Record;
use coolify_db::servers::ColumnValue;
use serde_json::{Map, Value, json};

use crate::auth::AuthCtx;
use crate::response::{ApiError, ApiResult, PhpJson, created, ok};
use crate::routes::git_apps::{
    GITEA_MORPH, GITHUB_MORPH, GITLAB_MORPH, application_count, can_access_system_resources,
    delete_github_row, random_token,
};
use crate::safe_url;
use crate::state::AppState;

fn db_err(e: sqlx::Error) -> ApiError {
    tracing::error!(error = %e, "source query failed");
    ApiError::Internal
}

fn body(bytes: &[u8]) -> Map<String, Value> {
    serde_json::from_slice(bytes).unwrap_or_default()
}

/// A Livewire property as text, trimmed by nothing: `null` for an empty input.
fn text(m: &Map<String, Value>, k: &str) -> Option<String> {
    match m.get(k) {
        Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
        Some(Value::Number(n)) => Some(n.to_string()),
        _ => None,
    }
}

fn flag(m: &Map<String, Value>, k: &str) -> bool {
    m.get(k).and_then(Value::as_bool).unwrap_or(false)
}

/// `handleError()`: the message as an error toast.
fn failure(message: impl Into<String>) -> ApiResult {
    Ok(PhpJson(
        StatusCode::UNPROCESSABLE_ENTITY,
        json!({"message": message.into()}),
    )
    .into_response())
}

/// Accumulates `$this->validate()` failures per field.
#[derive(Default)]
struct Errors(Map<String, Value>);

impl Errors {
    fn push(&mut self, field: &str, message: impl Into<String>) {
        if let Value::Array(list) = self.0.entry(field.to_owned()).or_insert_with(|| json!([])) {
            list.push(Value::String(message.into()));
        }
    }

    fn into_result(self) -> Result<(), Response> {
        if self.0.is_empty() {
            return Ok(());
        }
        let message = crate::ui_applications::summary(&self.0);
        Err(PhpJson(
            StatusCode::UNPROCESSABLE_ENTITY,
            json!({"message": message, "errors": self.0}),
        )
        .into_response())
    }
}

fn is_int(v: &str) -> bool {
    v.parse::<i64>().is_ok()
}

async fn check_url(
    state: &AppState,
    headers: &HeaderMap,
    errors: &mut Errors,
    field: &str,
    label: &str,
    value: Option<&str>,
) -> Result<(), ApiError> {
    let Some(v) = value else {
        errors.push(field, format!("The {label} field is required."));
        return Ok(());
    };
    if !crate::laravel_url::is_url(v) {
        errors.push(field, format!("The {label} field must be a valid URL."));
    }
    if let Some(msg) = safe_url::check_with(state, field, v, headers, true).await? {
        errors.push(field, msg);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// States
// ---------------------------------------------------------------------------

/// `Cache::put(..., now()->addMinutes(60))`.
static STATES: LazyLock<Mutex<HashMap<String, (Value, Instant)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn put_state(key_prefix: &str, payload: Value) -> String {
    let state = random_token(64);
    let mut states = STATES.lock().unwrap_or_else(|e| e.into_inner());
    states.retain(|_, (_, at)| at.elapsed() < Duration::from_secs(3600));
    states.insert(format!("{key_prefix}:{state}"), (payload, Instant::now()));
    state
}

/// `Cache::pull()`.
fn pull_state(key_prefix: &str, state: &str) -> Option<Value> {
    let mut states = STATES.lock().unwrap_or_else(|e| e.into_inner());
    let (value, at) = states.remove(&format!("{key_prefix}:{state}"))?;
    (at.elapsed() < Duration::from_secs(3600)).then_some(value)
}

// ---------------------------------------------------------------------------
// Rows
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Provider {
    Github,
    Gitlab,
    Gitea,
}

impl Provider {
    fn table(self) -> &'static str {
        match self {
            Provider::Github => "github_apps",
            Provider::Gitlab => "gitlab_apps",
            Provider::Gitea => "gitea_apps",
        }
    }
    fn morph(self) -> &'static str {
        match self {
            Provider::Github => GITHUB_MORPH,
            Provider::Gitlab => GITLAB_MORPH,
            Provider::Gitea => GITEA_MORPH,
        }
    }
    fn name(self) -> &'static str {
        match self {
            Provider::Github => "GitHub",
            Provider::Gitlab => "GitLab",
            Provider::Gitea => "Gitea",
        }
    }
    fn slug(self) -> &'static str {
        match self {
            Provider::Github => "github",
            Provider::Gitlab => "gitlab",
            Provider::Gitea => "gitea",
        }
    }
    fn api_suffix(self) -> &'static str {
        match self {
            Provider::Gitlab => "/api/v4",
            _ => "/api/v1",
        }
    }
    fn organization_column(self) -> &'static str {
        match self {
            Provider::Gitlab => "group_name",
            _ => "organization",
        }
    }
}

fn str_of<'a>(r: &'a Record, k: &str) -> Option<&'a str> {
    r.get(k).and_then(Value::as_str).filter(|s| !s.is_empty())
}

/// `isConnected()`.
fn is_connected(p: Provider, r: &Record) -> bool {
    match p {
        Provider::Github => {
            r.get("is_public").and_then(Value::as_bool) == Some(true)
                || (r.get("app_id").is_some_and(|v| !v.is_null())
                    && r.get("installation_id").is_some_and(|v| !v.is_null()))
        }
        _ => str_of(r, "access_token").is_some() && str_of(r, "refresh_token").is_some(),
    }
}

/// `ownedByCurrentTeam()->whereUuid($uuid)->firstOrFail()`.
async fn find_row(
    state: &AppState,
    ctx: &AuthCtx,
    p: Provider,
    uuid: &str,
) -> Result<Record, ApiError> {
    let row: Option<String> = sqlx::query_scalar(&format!(
        "SELECT row_to_json(s)::text FROM {} s WHERE (team_id = $1 OR is_system_wide = true) AND uuid = $2",
        p.table()
    ))
    .bind(ctx.team_id)
    .bind(uuid)
    .fetch_optional(state.db.pool())
    .await
    .map_err(db_err)?;
    let text = row.ok_or(ApiError::Message(StatusCode::NOT_FOUND, "Not Found"))?;
    coolify_db::records::parse_row(&text).map_err(|_| ApiError::Internal)
}

async fn can_update(state: &AppState, ctx: &AuthCtx, row: &Record) -> Result<bool, ApiError> {
    if row.get("is_system_wide").and_then(Value::as_bool) == Some(true) {
        return can_access_system_resources(state, ctx.user_id).await;
    }
    Ok(ctx.is_team_admin() && row.get("team_id").and_then(Value::as_i64) == Some(ctx.team_id))
}

async fn require_update(state: &AppState, ctx: &AuthCtx, row: &Record) -> Result<(), ApiError> {
    if can_update(state, ctx, row).await? {
        Ok(())
    } else {
        Err(ApiError::Unauthorized)
    }
}

fn id_of(row: &Record) -> i64 {
    row.get("id").and_then(Value::as_i64).unwrap_or(-1)
}

fn secret(state: &AppState, row: &Record, k: &str) -> Option<String> {
    let enc = state.encrypter.as_ref()?;
    let c = str_of(row, k)?;
    enc.decrypt_maybe_serialized(c).ok()
}

/// `config('app.url')`: the address this request came in on.
fn app_url(headers: &HeaderMap) -> String {
    let host = headers
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("localhost");
    format!("http://{host}")
}

/// `config('app.port')`.
const APP_PORT: u16 = 8000;

/// `$fqdn`, `$ipv4`, `$ipv6` and `config('app.url')` of the Change components.
async fn endpoints(state: &AppState, headers: &HeaderMap) -> Result<Value, ApiError> {
    let row: Option<(Option<String>, Option<String>, Option<String>)> =
        sqlx::query_as("SELECT fqdn, public_ipv4, public_ipv6 FROM instance_settings WHERE id = 0")
            .fetch_optional(state.db.pool())
            .await
            .map_err(db_err)?;
    let (fqdn, ipv4, ipv6) = row.unwrap_or_default();
    let ipv4 = ipv4
        .filter(|v| !v.is_empty())
        .map(|v| format!("http://{v}:{APP_PORT}"));
    let ipv6 = ipv6.filter(|v| !v.is_empty()).map(|v| {
        // formatHostForUrl()
        let host = if v.contains(':') && !v.starts_with('[') {
            format!("[{v}]")
        } else {
            v
        };
        format!("http://{host}:{APP_PORT}")
    });
    Ok(json!({"fqdn": fqdn, "ipv4": ipv4, "ipv6": ipv6, "app_url": app_url(headers)}))
}

/// `$this->applications` with what the Resources lists show.
async fn applications_of(
    state: &AppState,
    ctx: &AuthCtx,
    p: Provider,
    id: i64,
) -> Result<Value, ApiError> {
    let rows: Vec<(String, String, Option<String>, Option<String>, String, String, String, String)> = sqlx::query_as(
        "SELECT a.uuid, a.name, a.git_repository, a.git_branch, e.name, e.uuid, pr.name, pr.uuid \
         FROM applications a JOIN environments e ON e.id = a.environment_id JOIN projects pr ON pr.id = e.project_id \
         WHERE a.source_type = $1 AND a.source_id = $2 AND a.deleted_at IS NULL AND pr.team_id = $3",
    )
    .bind(p.morph())
    .bind(id)
    .bind(ctx.team_id)
    .fetch_all(state.db.pool())
    .await
    .map_err(db_err)?;
    Ok(Value::Array(
        rows.into_iter()
            .map(
                |(uuid, name, repo, branch, env, env_uuid, project, project_uuid)| {
                    json!({
                        "uuid": uuid, "name": name, "git_repository": repo, "git_branch": branch,
                        "environment": env, "environment_uuid": env_uuid,
                        "project": project, "project_uuid": project_uuid,
                    })
                },
            )
            .collect(),
    ))
}

// ---------------------------------------------------------------------------
// source.all
// ---------------------------------------------------------------------------

/// `Team::sources()` as `source/all.blade.php` shows them.
pub async fn index(State(state): State<AppState>, Extension(ctx): Extension<AuthCtx>) -> ApiResult {
    let mut out = Vec::new();
    for p in [Provider::Github, Provider::Gitlab, Provider::Gitea] {
        let rows: Vec<String> = sqlx::query_scalar(&format!(
            "SELECT row_to_json(s)::text FROM {} s WHERE (team_id = $1 OR is_system_wide = true) AND is_public = false",
            p.table()
        ))
        .bind(ctx.team_id)
        .fetch_all(state.db.pool())
        .await
        .map_err(db_err)?;
        for t in rows {
            let r = coolify_db::records::parse_row(&t).map_err(|_| ApiError::Internal)?;
            out.push(json!({
                "uuid": r.get("uuid"),
                "name": r.get("name"),
                "type": p.slug(),
                "provider": p.name(),
                "organization": r.get(p.organization_column()),
                "is_connected": is_connected(p, &r),
            }));
        }
    }
    let system = can_access_system_resources(&state, ctx.user_id).await?;
    Ok(ok(json!({
        "sources": out,
        "can_create": ctx.is_team_admin(),
        "can_make_system_wide": system,
        "random_name": random_name(),
    }))
    .into_response())
}

/// `substr(generate_random_name(), 0, 30)`.
fn random_name() -> String {
    crate::routes::private_keys::random_name()
        .chars()
        .take(30)
        .collect()
}

// ---------------------------------------------------------------------------
// Create
// ---------------------------------------------------------------------------

fn port_of(m: &Map<String, Value>) -> Option<i64> {
    match m.get("custom_port") {
        Some(Value::Number(n)) => n.as_i64(),
        Some(Value::String(s)) => s.trim().parse().ok(),
        _ => None,
    }
}

/// `Source\Github\Create::createGitHubApp()`.
pub async fn create_github(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> ApiResult {
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let m = body(&bytes);
    let organization =
        coolify_jobs::github::normalize_organization(text(&m, "organization").as_deref());
    let html = text(&m, "html_url");
    let api = text(&m, "api_url").or_else(|| {
        html.as_deref()
            .map(coolify_jobs::github::api_url_from_html_url)
    });
    let runners = flag(&m, "use_for_github_runners");
    let mut errors = Errors::default();
    let name = text(&m, "name");
    if name.is_none() {
        errors.push("name", "The name field is required.");
    }
    match organization.as_deref() {
        None if runners => errors.push(
            "organization",
            "GitHub Actions runners are registered at organization level. Enter the organization.",
        ),
        Some(o) if !organization_valid(o) => {
            errors.push("organization", "The organization field format is invalid.");
        }
        _ => {}
    }
    check_url(
        &state,
        &headers,
        &mut errors,
        "api_url",
        "api url",
        api.as_deref(),
    )
    .await?;
    check_url(
        &state,
        &headers,
        &mut errors,
        "html_url",
        "html url",
        html.as_deref(),
    )
    .await?;
    let user = text(&m, "custom_user");
    if user.is_none() {
        errors.push("custom_user", "The custom user field is required.");
    }
    let port = port_of(&m);
    if port.is_none() {
        errors.push("custom_port", "The custom port field is required.");
    }
    if let Err(r) = errors.into_result() {
        return Ok(r);
    }
    let system = flag(&m, "is_system_wide");
    if system && !can_access_system_resources(&state, ctx.user_id).await? {
        return Err(ApiError::Unauthorized);
    }
    let uuid = coolify_domain::new_public_id();
    sqlx::query(
        "INSERT INTO github_apps (uuid, name, organization, api_url, html_url, custom_user, custom_port, is_system_wide, team_id, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $10)",
    )
    .bind(&uuid)
    .bind(&name)
    .bind(&organization)
    .bind(&api)
    .bind(&html)
    .bind(&user)
    .bind(port.unwrap_or(22) as i32)
    .bind(system)
    .bind(ctx.team_id)
    .bind(state.now())
    .execute(state.db.pool())
    .await
    .map_err(db_err)?;
    Ok(created(json!({
        "uuid": uuid,
        "previews": i32::from(flag(&m, "use_for_pull_request_previews")),
        "runners": i32::from(runners),
    }))
    .into_response())
}

fn organization_valid(o: &str) -> bool {
    !o.is_empty()
        && !o
            .chars()
            .any(|c| c.is_whitespace() || matches!(c, '/' | '?' | '#'))
}

/// `Source\Gitlab\Create::createGitLabApp()` and `Source\Gitea\Create::createGiteaApp()`.
async fn create_oauth(
    state: AppState,
    ctx: AuthCtx,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
    p: Provider,
) -> ApiResult {
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let m = body(&bytes);
    let html = text(&m, "html_url").map(|h| h.trim_end_matches('/').to_owned());
    let api = text(&m, "api_url")
        .map(|a| a.trim_end_matches('/').to_owned())
        .or_else(|| html.as_deref().map(|h| format!("{h}{}", p.api_suffix())));
    let mut errors = Errors::default();
    let name = text(&m, "name");
    if name.is_none() {
        errors.push("name", "The name field is required.");
    }
    check_url(
        &state,
        &headers,
        &mut errors,
        "html_url",
        "html url",
        html.as_deref(),
    )
    .await?;
    check_url(
        &state,
        &headers,
        &mut errors,
        "api_url",
        "api url",
        api.as_deref(),
    )
    .await?;
    let user = text(&m, "custom_user");
    if user.is_none() {
        errors.push("custom_user", "The custom user field is required.");
    }
    let port = port_of(&m);
    if port.is_none() {
        errors.push("custom_port", "The custom port field is required.");
    }
    if let Err(r) = errors.into_result() {
        return Ok(r);
    }
    let system = flag(&m, "is_system_wide");
    if system && !can_access_system_resources(&state, ctx.user_id).await? {
        return Err(ApiError::Unauthorized);
    }
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let uuid = coolify_domain::new_public_id();
    let token = random_token(32);
    let (org_col, secret_col, secret_value) = match p {
        Provider::Gitlab => ("group_name", "webhook_token", enc.encrypt_string(&token)),
        _ => (
            "organization",
            "webhook_secret",
            enc.encrypt_serialized(&token),
        ),
    };
    sqlx::query(&format!(
        "INSERT INTO {} (uuid, name, api_url, html_url, custom_user, custom_port, is_system_wide, {org_col}, {secret_col}, team_id, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $11)",
        p.table()
    ))
    .bind(&uuid)
    .bind(&name)
    .bind(&api)
    .bind(&html)
    .bind(&user)
    .bind(port.unwrap_or(22) as i32)
    .bind(system)
    .bind(text(&m, org_col))
    .bind(&secret_value)
    .bind(ctx.team_id)
    .bind(state.now())
    .execute(state.db.pool())
    .await
    .map_err(db_err)?;
    Ok(created(json!({"uuid": uuid})).into_response())
}

pub async fn create_gitlab(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> ApiResult {
    create_oauth(state, ctx, headers, bytes, Provider::Gitlab).await
}

pub async fn create_gitea(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> ApiResult {
    create_oauth(state, ctx, headers, bytes, Provider::Gitea).await
}

// ---------------------------------------------------------------------------
// GitHub Change
// ---------------------------------------------------------------------------

/// `Str::kebab($name)` as `kebab()` of `git_urls` computes it.
fn kebab(name: &str) -> String {
    crate::git_urls::kebab(name)
}

/// `getInstallationPath($source)`, with the state it stores.
fn installation_path(row: &Record) -> String {
    let html = str_of(row, "html_url").unwrap_or_default();
    let name = coolify_jobs::gitea::raw_url_encode(&kebab(str_of(row, "name").unwrap_or_default()));
    let organization = coolify_jobs::github::normalize_organization(str_of(row, "organization"));
    let host = html
        .split_once("://")
        .map_or(html, |(_, r)| r)
        .split(['/', '?', '#', ':'])
        .next()
        .unwrap_or_default()
        .to_lowercase();
    let ghe = host.ends_with(".ghe.com") && !host.starts_with("api.");
    let path = if host != "github.com" && !ghe {
        format!("github-apps/{name}")
    } else if let (true, Some(o)) = (ghe, organization.as_deref().filter(|o| !o.is_empty())) {
        format!("apps/{}/{name}", coolify_jobs::gitea::raw_url_encode(o))
    } else {
        format!("apps/{name}")
    };
    let state = put_state(
        "github-app-setup-state",
        json!({"action": "install", "github_app_id": row.get("id"), "team_id": row.get("team_id")}),
    );
    format!(
        "{}/{path}/installations/new?state={state}",
        html.trim_end_matches('/')
    )
}

fn organization_settings_path(row: &Record, suffix: &str) -> String {
    let html = str_of(row, "html_url")
        .unwrap_or_default()
        .trim_end_matches('/');
    let name = str_of(row, "name").unwrap_or_default();
    let name = coolify_jobs::gitea::raw_url_encode(&if suffix.is_empty() {
        name.to_owned()
    } else {
        kebab(name)
    });
    match coolify_jobs::github::normalize_organization(str_of(row, "organization"))
        .filter(|o| !o.is_empty())
    {
        Some(o) => format!(
            "{html}/organizations/{}/settings/apps/{name}{suffix}",
            coolify_jobs::gitea::raw_url_encode(&o)
        ),
        None => format!("{html}/settings/apps/{name}{suffix}"),
    }
}

/// `GithubApp::missingRunnerRequirements()`.
fn missing_runner_requirements(row: &Record) -> Vec<&'static str> {
    let mut missing = Vec::new();
    if str_of(row, "organization").is_none() {
        missing.push("An organization (runners are registered at organization level)");
    }
    if str_of(row, "organization_self_hosted_runners") != Some("write") {
        missing.push("Organization permission \"Self-hosted runners\": write");
    }
    if !matches!(str_of(row, "actions"), Some("read" | "write")) {
        missing.push("Repository permission \"Actions\": read");
    }
    let has_event = row
        .get("webhook_events")
        .and_then(Value::as_array)
        .is_some_and(|a| a.iter().any(|e| e.as_str() == Some("workflow_job")));
    if !has_event {
        missing.push("Webhook event \"Workflow job\"");
    }
    missing
}

/// `Source\Github\Change::mount()`.
pub async fn show_github(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    headers: HeaderMap,
    Path(uuid): Path<String>,
) -> ApiResult {
    let row = find_row(&state, &ctx, Provider::Github, &uuid).await?;
    let update = can_update(&state, &ctx, &row).await?;
    let keys: Vec<(i64, String)> =
        sqlx::query_as("SELECT id, name FROM private_keys WHERE team_id = $1 ORDER BY id")
            .bind(ctx.team_id)
            .fetch_all(state.db.pool())
            .await
            .map_err(db_err)?;
    let endpoints = endpoints(&state, &headers).await?;
    let has_app_id = row.get("app_id").is_some_and(|v| !v.is_null());
    let has_installation = row.get("installation_id").is_some_and(|v| !v.is_null());
    let manifest_state = put_state(
        "github-app-setup-state",
        json!({"action": "manifest", "github_app_id": row.get("id"), "team_id": row.get("team_id")}),
    );
    Ok(ok(json!({
        "uuid": row.get("uuid"),
        "name": row.get("name"),
        "display_name": kebab(str_of(&row, "name").unwrap_or_default()),
        "organization": row.get("organization"),
        "api_url": row.get("api_url"),
        "html_url": row.get("html_url"),
        "custom_user": row.get("custom_user"),
        "custom_port": row.get("custom_port"),
        "app_id": row.get("app_id"),
        "installation_id": row.get("installation_id"),
        "client_id": row.get("client_id"),
        "client_secret": if update { secret(&state, &row, "client_secret") } else { None },
        "webhook_secret": if update { secret(&state, &row, "webhook_secret") } else { None },
        "is_system_wide": row.get("is_system_wide"),
        "private_key_id": row.get("private_key_id"),
        "contents": row.get("contents"),
        "metadata": row.get("metadata"),
        "pull_requests": row.get("pull_requests"),
        "organization_self_hosted_runners": row.get("organization_self_hosted_runners"),
        "actions": row.get("actions"),
        "webhook_events": row.get("webhook_events"),
        "missing_runner_requirements": missing_runner_requirements(&row),
        "has_app_id": has_app_id,
        "has_installation": has_installation,
        "is_connected": is_connected(Provider::Github, &row),
        "private_keys": keys.iter().map(|(id, n)| json!({"id": id, "name": n})).collect::<Vec<_>>(),
        "applications": applications_of(&state, &ctx, Provider::Github, id_of(&row)).await?,
        "endpoints": endpoints,
        "manifest_state": manifest_state,
        "installation_path": if update { Some(installation_path(&row)) } else if !has_installation { Some(installation_path(&row)) } else { None },
        "permissions_path": organization_settings_path(&row, "/permissions"),
        "settings_url": organization_settings_path(&row, ""),
        "can_create": ctx.is_team_admin(),
        "can_update": update,
        "can_delete": update,
        "can_make_system_wide": can_access_system_resources(&state, ctx.user_id).await?,
    }))
    .into_response())
}

/// `Source\Github\Change::submit()` and `instantSave()` (`instant: true` only validates the key).
pub async fn update_github(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    headers: HeaderMap,
    Path(uuid): Path<String>,
    bytes: axum::body::Bytes,
) -> ApiResult {
    let row = find_row(&state, &ctx, Provider::Github, &uuid).await?;
    require_update(&state, &ctx, &row).await?;
    let m = body(&bytes);
    let system = flag(&m, "is_system_wide");
    let was_system = row.get("is_system_wide").and_then(Value::as_bool) == Some(true);
    if system && !was_system && !can_access_system_resources(&state, ctx.user_id).await? {
        return Err(ApiError::Unauthorized);
    }
    let organization =
        coolify_jobs::github::normalize_organization(text(&m, "organization").as_deref());
    let html = text(&m, "html_url");
    let api = text(&m, "api_url").or_else(|| {
        html.as_deref()
            .map(coolify_jobs::github::api_url_from_html_url)
    });
    let mut errors = Errors::default();
    let name = text(&m, "name");
    if name.is_none() {
        errors.push("name", "The name field is required.");
    }
    if let Some(o) = organization.as_deref()
        && !organization_valid(o)
    {
        errors.push("organization", "The organization field format is invalid.");
    }
    check_url(
        &state,
        &headers,
        &mut errors,
        "apiUrl",
        "api url",
        api.as_deref(),
    )
    .await?;
    check_url(
        &state,
        &headers,
        &mut errors,
        "htmlUrl",
        "html url",
        html.as_deref(),
    )
    .await?;
    let user = text(&m, "custom_user");
    if user.is_none() {
        errors.push("customUser", "The custom user field is required.");
    }
    let port = text(&m, "custom_port");
    match port.as_deref() {
        None => errors.push("customPort", "The custom port field is required."),
        Some(p) if !is_int(p) => {
            errors.push("customPort", "The custom port field must be an integer.")
        }
        _ => {}
    }
    let app_id = text(&m, "app_id");
    if let Some(v) = app_id.as_deref().filter(|v| !is_int(v)) {
        let _ = v;
        errors.push("appId", "The app id field must be an integer.");
    }
    let installation_id = text(&m, "installation_id");
    if installation_id.as_deref().is_some_and(|v| !is_int(v)) {
        errors.push(
            "installationId",
            "The installation id field must be an integer.",
        );
    }
    let key = m
        .get("private_key_id")
        .and_then(Value::as_i64)
        .filter(|k| *k != 0);
    if let Some(k) = key {
        let team = row.get("team_id").and_then(Value::as_i64).unwrap_or(-1);
        let exists = private_keys::exists_in_team(&state.db, team, k).await?;
        if !exists {
            errors.push("privateKeyId", "The selected private key id is invalid.");
        }
    }
    if let Err(r) = errors.into_result() {
        return Ok(r);
    }
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let opt_int = |v: Option<String>| match v.and_then(|s| s.parse::<i64>().ok()) {
        Some(i) => ColumnValue::Int(i),
        None => ColumnValue::Text(None),
    };
    let mut values: Vec<(&'static str, ColumnValue)> = vec![
        ("name", ColumnValue::Text(name)),
        ("organization", ColumnValue::Text(organization)),
        ("api_url", ColumnValue::Text(api)),
        ("html_url", ColumnValue::Text(html)),
        ("custom_user", ColumnValue::Text(user)),
        ("custom_port", opt_int(port)),
        ("app_id", opt_int(app_id)),
        ("installation_id", opt_int(installation_id)),
        ("client_id", ColumnValue::Text(text(&m, "client_id"))),
        ("is_system_wide", ColumnValue::Bool(system)),
        (
            "private_key_id",
            key.map_or(ColumnValue::Text(None), ColumnValue::Int),
        ),
        ("contents", ColumnValue::Text(text(&m, "contents"))),
        ("metadata", ColumnValue::Text(text(&m, "metadata"))),
        (
            "pull_requests",
            ColumnValue::Text(text(&m, "pull_requests")),
        ),
    ];
    for k in ["client_secret", "webhook_secret"] {
        let plain = text(&m, k);
        let current = secret(&state, &row, k);
        if plain != current {
            values.push((
                k,
                ColumnValue::Text(plain.map(|p| enc.encrypt_serialized(&p))),
            ));
        }
    }
    coolify_db::database_parts::update_row(
        &state.db,
        "github_apps",
        id_of(&row),
        &values,
        state.now(),
    )
    .await?;
    let fresh = find_row(&state, &ctx, Provider::Github, &uuid).await?;
    Ok(ok(json!({"message": "Github App updated.", "is_connected": is_connected(Provider::Github, &fresh)})).into_response())
}

/// `createGithubAppManually()`.
pub async fn github_manual(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let row = find_row(&state, &ctx, Provider::Github, &uuid).await?;
    require_update(&state, &ctx, &row).await?;
    coolify_db::database_parts::update_row(
        &state.db,
        "github_apps",
        id_of(&row),
        &[
            ("app_id", ColumnValue::Int(1_234_567_890)),
            ("installation_id", ColumnValue::Int(1_234_567_890)),
        ],
        state.now(),
    )
    .await?;
    Ok(
        ok(json!({"message": "Github App updated. You can now configure the details."}))
            .into_response(),
    )
}

/// `syncGithubAppName()`: stores the slug on the App and names its key after it.
async fn sync_name(state: &AppState, row: &Record) -> Result<Option<String>, String> {
    let enc = state
        .encrypter
        .as_ref()
        .ok_or("No application encryption key has been specified.")?;
    let Some(slug) = coolify_jobs::github::app_slug(&state.db, enc, row).await? else {
        return Ok(None);
    };
    let id = id_of(row);
    let key = row.get("private_key_id").and_then(Value::as_i64);
    let now = state.now();
    let update = async {
        sqlx::query("UPDATE github_apps SET name = $1, updated_at = $2 WHERE id = $3 AND name IS DISTINCT FROM $1")
            .bind(&slug)
            .bind(now)
            .bind(id)
            .execute(state.db.pool())
            .await?;
        sqlx::query("UPDATE private_keys SET name = $1, updated_at = $2 WHERE id = $3 AND name IS DISTINCT FROM $1")
            .bind(format!("github-app-{slug}"))
            .bind(now)
            .bind(key)
            .execute(state.db.pool())
            .await
    };
    update.await.map_err(|e| e.to_string())?;
    Ok(Some(slug))
}

const KEY_FORMAT_ERROR: &str = "The selected private key format is not supported for GitHub Apps. <br><br>Please use an RSA private key in PEM format (BEGIN RSA PRIVATE KEY). <br><br>OpenSSH format keys (BEGIN OPENSSH PRIVATE KEY) are not supported.";

/// The message `checkPermissions()` and `testConnection()` show for an exception.
fn key_failure(e: String) -> String {
    if e.contains("not a valid RSA key") || e.contains("DECODER routines::unsupported") {
        KEY_FORMAT_ERROR.to_owned()
    } else {
        e
    }
}

/// `checkPermissions()`: `syncGithubAppName()`, then `GithubAppPermissionJob`.
pub async fn github_check_permissions(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let row = find_row(&state, &ctx, Provider::Github, &uuid).await?;
    let mut missing = Vec::new();
    if row.get("app_id").is_none_or(Value::is_null) {
        missing.push("App ID");
    }
    if row.get("private_key_id").is_none_or(Value::is_null) {
        missing.push("Private Key");
    }
    if !missing.is_empty() {
        return failure(format!(
            "Cannot fetch permissions. Please set the following required fields first: {}",
            missing.join(", ")
        ));
    }
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    if coolify_jobs::github::app_private_key(&state.db, enc, &row)
        .await
        .is_err()
    {
        return failure("Private Key not found. Please select a valid private key.");
    }
    // syncGithubAppName($this->github_app) without throwing.
    let _ = sync_name(&state, &row).await;
    let row = find_row(&state, &ctx, Provider::Github, &uuid).await?;
    let info = match coolify_jobs::github::fetch_permissions(&state.db, enc, &row).await {
        Ok(i) => i,
        Err(e) => return failure(key_failure(e)),
    };
    let permissions = info.get("permissions").cloned().unwrap_or(Value::Null);
    let perm = |k: &str| {
        permissions
            .get(k)
            .and_then(Value::as_str)
            .map(str::to_owned)
    };
    sqlx::query(
        "UPDATE github_apps SET contents = $1, metadata = $2, pull_requests = $3, administration = $4, \
         organization_self_hosted_runners = $5, actions = $6, webhook_events = $7::json, updated_at = $8 WHERE id = $9",
    )
    .bind(perm("contents"))
    .bind(perm("metadata"))
    .bind(perm("pull_requests"))
    .bind(perm("administration"))
    .bind(perm("organization_self_hosted_runners"))
    .bind(perm("actions"))
    .bind(info.get("events").cloned().unwrap_or_else(|| json!([])).to_string())
    .bind(state.now())
    .bind(id_of(&row))
    .execute(state.db.pool())
    .await
    .map_err(db_err)?;
    Ok(ok(json!({"message": "Github App permissions updated."})).into_response())
}

/// `testConnection()`.
pub async fn github_test(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let row = find_row(&state, &ctx, Provider::Github, &uuid).await?;
    if !is_connected(Provider::Github, &row) {
        return failure("GitHub App is not fully set up. Please complete installation first.");
    }
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    if coolify_jobs::github::app_private_key(&state.db, enc, &row)
        .await
        .is_err()
    {
        return failure("Private Key not found. Please select a valid private key.");
    }
    let answer = async {
        let jwt = coolify_jobs::github::jwt(&state.db, enc, &row).await?;
        coolify_jobs::github::app_info(&state.db, &row, &jwt).await
    }
    .await;
    let (status, info) = match answer {
        Ok(a) => a,
        Err(e) => return failure(key_failure(e)),
    };
    if !(200..300).contains(&status) {
        let error = info
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("Unknown error");
        return failure(format!("Connection failed: {error}"));
    }
    if let Err(e) = coolify_jobs::github::installation_token(&state.db, Some(enc), Some(&row)).await
    {
        return failure(key_failure(e));
    }
    let name = info
        .get("name")
        .or_else(|| info.get("slug"))
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    Ok(ok(
        json!({"message": format!("Connection successful! Authenticated as GitHub App: {name}")}),
    )
    .into_response())
}

/// `updateGithubAppName()`.
pub async fn github_sync_name(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    bytes: axum::body::Bytes,
) -> ApiResult {
    let mut row = find_row(&state, &ctx, Provider::Github, &uuid).await?;
    require_update(&state, &ctx, &row).await?;
    let m = body(&bytes);
    let app_id = text(&m, "app_id");
    let Some(app_id) = app_id else {
        return failure("App ID is required before synchronizing the GitHub App name.");
    };
    let key = m
        .get("private_key_id")
        .and_then(Value::as_i64)
        .unwrap_or(-1);
    let team = row.get("team_id").and_then(Value::as_i64).unwrap_or(-1);
    if !private_keys::exists_in_team(&state.db, team, key).await? {
        return failure("No private key found for this GitHub App.");
    }
    row.insert("app_id".into(), Value::String(app_id));
    row.insert("private_key_id".into(), json!(key));
    match sync_name(&state, &row).await {
        Ok(Some(slug)) => Ok(ok(json!({
            "message": "GitHub App name and private key name synchronized successfully.",
            "name": kebab(&slug),
        }))
        .into_response()),
        Ok(None) => Ok(
            ok(json!({"info": "Could not find App Name (slug) in GitHub response."}))
                .into_response(),
        ),
        Err(e) => failure(e),
    }
}

/// `delete()` of the Change components.
async fn delete_source(state: AppState, ctx: AuthCtx, p: Provider, uuid: String) -> ApiResult {
    let row = find_row(&state, &ctx, p, &uuid).await?;
    require_update(&state, &ctx, &row).await?;
    if application_count(&state, p.morph(), id_of(&row)).await? > 0 {
        return failure(
            "This source is being used by an application. Please delete all applications first.",
        );
    }
    if p == Provider::Github {
        if let Err(e) = delete_github_row(&state, &row).await? {
            return failure(e);
        }
    } else {
        sqlx::query(&format!("DELETE FROM {} WHERE id = $1", p.table()))
            .bind(id_of(&row))
            .execute(state.db.pool())
            .await
            .map_err(db_err)?;
    }
    Ok(ok(json!({"message": "Deleted."})).into_response())
}

pub async fn delete_github(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    delete_source(state, ctx, Provider::Github, uuid).await
}

pub async fn delete_gitlab(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    delete_source(state, ctx, Provider::Gitlab, uuid).await
}

pub async fn delete_gitea(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    delete_source(state, ctx, Provider::Gitea, uuid).await
}

// ---------------------------------------------------------------------------
// GitLab and Gitea Change
// ---------------------------------------------------------------------------

async fn show_oauth(
    state: AppState,
    ctx: AuthCtx,
    headers: HeaderMap,
    p: Provider,
    uuid: String,
) -> ApiResult {
    let row = find_row(&state, &ctx, p, &uuid).await?;
    let update = can_update(&state, &ctx, &row).await?;
    let keys: Vec<(i64, String)> =
        sqlx::query_as("SELECT id, name FROM private_keys WHERE team_id = $1 ORDER BY id")
            .bind(ctx.team_id)
            .fetch_all(state.db.pool())
            .await
            .map_err(db_err)?;
    let id_key = format!("{}_app_id", p.slug());
    let oauth_state = put_state(
        &format!("{}-app-oauth-state", p.slug()),
        json!({id_key: row.get("id"), "team_id": ctx.team_id}),
    );
    let (secret_col, webhook) = match p {
        Provider::Gitlab => (
            "webhook_token",
            str_of(&row, "webhook_token").map(|c| {
                state
                    .encrypter
                    .as_ref()
                    .and_then(|e| e.decrypt_string(c).ok())
                    .unwrap_or_else(|| c.to_owned())
            }),
        ),
        _ => ("webhook_secret", secret(&state, &row, "webhook_secret")),
    };
    let _ = secret_col;
    Ok(ok(json!({
        "uuid": row.get("uuid"),
        "name": row.get("name"),
        "api_url": row.get("api_url"),
        "html_url": row.get("html_url"),
        "custom_user": row.get("custom_user"),
        "custom_port": row.get("custom_port"),
        "client_id": row.get("client_id"),
        "client_secret": if update { secret(&state, &row, "client_secret") } else { None },
        "has_client_secret": str_of(&row, "client_secret").is_some(),
        "webhook_secret": if update { webhook } else { None },
        "organization": row.get(p.organization_column()),
        "is_system_wide": row.get("is_system_wide"),
        "private_key_id": row.get("private_key_id"),
        "redirect_uri": row.get("redirect_uri"),
        "is_connected": is_connected(p, &row),
        "private_keys": keys.iter().map(|(id, n)| json!({"id": id, "name": n})).collect::<Vec<_>>(),
        "applications": applications_of(&state, &ctx, p, id_of(&row)).await?,
        "endpoints": endpoints(&state, &headers).await?,
        "oauth_state": oauth_state,
        "can_update": update,
        "can_delete": update,
        "can_make_system_wide": can_access_system_resources(&state, ctx.user_id).await?,
    }))
    .into_response())
}

pub async fn show_gitlab(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    headers: HeaderMap,
    Path(uuid): Path<String>,
) -> ApiResult {
    show_oauth(state, ctx, headers, Provider::Gitlab, uuid).await
}

pub async fn show_gitea(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    headers: HeaderMap,
    Path(uuid): Path<String>,
) -> ApiResult {
    show_oauth(state, ctx, headers, Provider::Gitea, uuid).await
}

/// `submit()` of `Source\Gitlab\Change` and `Source\Gitea\Change`.
async fn update_oauth(
    state: AppState,
    ctx: AuthCtx,
    headers: HeaderMap,
    p: Provider,
    uuid: String,
    bytes: axum::body::Bytes,
) -> ApiResult {
    let row = find_row(&state, &ctx, p, &uuid).await?;
    require_update(&state, &ctx, &row).await?;
    let m = body(&bytes);
    let system = flag(&m, "is_system_wide");
    let was_system = row.get("is_system_wide").and_then(Value::as_bool) == Some(true);
    if system && !was_system && !can_access_system_resources(&state, ctx.user_id).await? {
        return Err(ApiError::Unauthorized);
    }
    let mut errors = Errors::default();
    let name = text(&m, "name");
    if name.is_none() {
        errors.push("name", "The name field is required.");
    }
    let api = text(&m, "api_url");
    let html = text(&m, "html_url");
    check_url(
        &state,
        &headers,
        &mut errors,
        "apiUrl",
        "api url",
        api.as_deref(),
    )
    .await?;
    check_url(
        &state,
        &headers,
        &mut errors,
        "htmlUrl",
        "html url",
        html.as_deref(),
    )
    .await?;
    let user = text(&m, "custom_user");
    if user.is_none() {
        errors.push("customUser", "The custom user field is required.");
    }
    let port = text(&m, "custom_port");
    match port.as_deref() {
        None => errors.push("customPort", "The custom port field is required."),
        Some(v) if !is_int(v) => {
            errors.push("customPort", "The custom port field must be an integer.")
        }
        _ => {}
    }
    let key = m
        .get("private_key_id")
        .and_then(Value::as_i64)
        .filter(|k| *k != 0);
    if let Some(k) = key {
        let team = row.get("team_id").and_then(Value::as_i64).unwrap_or(-1);
        if !private_keys::exists_in_team(&state.db, team, k).await? {
            errors.push("privateKeyId", "The selected private key id is invalid.");
        }
    }
    let redirect_uri = text(&m, "redirect_uri");
    if let Err(r) = errors.into_result() {
        return Ok(r);
    }
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let mut values: Vec<(&'static str, ColumnValue)> = vec![
        ("name", ColumnValue::Text(name)),
        ("api_url", ColumnValue::Text(api)),
        (
            "html_url",
            ColumnValue::Text(html.map(|h| h.trim_end_matches('/').to_owned())),
        ),
        ("custom_user", ColumnValue::Text(user)),
        (
            "custom_port",
            ColumnValue::Int(port.and_then(|v| v.parse().ok()).unwrap_or(22)),
        ),
        ("client_id", ColumnValue::Text(text(&m, "client_id"))),
        (
            p.organization_column(),
            ColumnValue::Text(text(&m, "organization")),
        ),
        ("is_system_wide", ColumnValue::Bool(system)),
        (
            "private_key_id",
            key.map_or(ColumnValue::Text(None), ColumnValue::Int),
        ),
        ("redirect_uri", ColumnValue::Text(redirect_uri)),
    ];
    // Empty inputs keep the stored secrets.
    if let Some(s) = text(&m, "client_secret")
        && secret(&state, &row, "client_secret").as_deref() != Some(s.as_str())
    {
        values.push((
            "client_secret",
            ColumnValue::Text(Some(enc.encrypt_serialized(&s))),
        ));
    }
    if let Some(w) = text(&m, "webhook_secret") {
        if p == Provider::Gitlab {
            values.push((
                "webhook_token",
                ColumnValue::Text(Some(enc.encrypt_string(&w))),
            ));
        } else if secret(&state, &row, "webhook_secret").as_deref() != Some(w.as_str()) {
            values.push((
                "webhook_secret",
                ColumnValue::Text(Some(enc.encrypt_serialized(&w))),
            ));
        }
    }
    coolify_db::database_parts::update_row(&state.db, p.table(), id_of(&row), &values, state.now())
        .await?;
    Ok(ok(json!({"message": format!("{} App updated.", p.name())})).into_response())
}

pub async fn update_gitlab(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    headers: HeaderMap,
    Path(uuid): Path<String>,
    bytes: axum::body::Bytes,
) -> ApiResult {
    update_oauth(state, ctx, headers, Provider::Gitlab, uuid, bytes).await
}

pub async fn update_gitea(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    headers: HeaderMap,
    Path(uuid): Path<String>,
    bytes: axum::body::Bytes,
) -> ApiResult {
    update_oauth(state, ctx, headers, Provider::Gitea, uuid, bytes).await
}

/// `persistRedirectUriFromEndpoint()`.
async fn persist_redirect(
    state: AppState,
    ctx: AuthCtx,
    p: Provider,
    uuid: String,
    bytes: axum::body::Bytes,
) -> ApiResult {
    let row = find_row(&state, &ctx, p, &uuid).await?;
    let m = body(&bytes);
    if let Some(uri) = text(&m, "redirect_uri")
        && can_update(&state, &ctx, &row).await?
    {
        coolify_db::database_parts::update_row(
            &state.db,
            p.table(),
            id_of(&row),
            &[("redirect_uri", ColumnValue::Text(Some(uri)))],
            state.now(),
        )
        .await?;
    }
    Ok(ok(json!({"ok": true})).into_response())
}

pub async fn redirect_gitlab(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    bytes: axum::body::Bytes,
) -> ApiResult {
    persist_redirect(state, ctx, Provider::Gitlab, uuid, bytes).await
}

pub async fn redirect_gitea(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    bytes: axum::body::Bytes,
) -> ApiResult {
    persist_redirect(state, ctx, Provider::Gitea, uuid, bytes).await
}

/// `testConnection()`: `GET /user` with the access token.
async fn test_oauth(state: AppState, ctx: AuthCtx, p: Provider, uuid: String) -> ApiResult {
    let row = find_row(&state, &ctx, p, &uuid).await?;
    if !is_connected(p, &row) {
        return failure(format!(
            "{} App is not connected. Please complete the OAuth flow first.",
            p.name()
        ));
    }
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let answer = if p == Provider::Gitlab {
        coolify_jobs::gitlab::api(
            &state.db,
            enc,
            &row,
            coolify_jobs::git_http::Method::GET,
            "/user",
            None,
        )
        .await
    } else {
        coolify_jobs::gitea::api(
            &state.db,
            enc,
            &row,
            coolify_jobs::git_http::Method::GET,
            "/user",
            None,
        )
        .await
    };
    match answer {
        Ok((user, _)) => {
            let key = if p == Provider::Gitlab {
                "username"
            } else {
                "login"
            };
            let who = user.get(key).and_then(Value::as_str).unwrap_or("unknown");
            Ok(
                ok(json!({"message": format!("Connection successful! Authenticated as: {who}")}))
                    .into_response(),
            )
        }
        Err(e) => {
            let prefix = format!("{} API call failed: ", p.name());
            match e.strip_prefix(&prefix) {
                Some(rest) => failure(format!("Connection failed: {rest}")),
                None => failure(e),
            }
        }
    }
}

pub async fn test_gitlab(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    test_oauth(state, ctx, Provider::Gitlab, uuid).await
}

pub async fn test_gitea(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    test_oauth(state, ctx, Provider::Gitea, uuid).await
}

// ---------------------------------------------------------------------------
// Browser callbacks
// ---------------------------------------------------------------------------

fn redirect(to: &str) -> Response {
    (StatusCode::FOUND, [(header::LOCATION, to.to_owned())]).into_response()
}

/// `redirect()->route(...)->with('error', $message)`: the page shows the flash as a toast.
fn redirect_with_error(to: &str, message: &str) -> Response {
    let sep = if to.contains('?') { '&' } else { '?' };
    redirect(&format!(
        "{to}{sep}error={}",
        coolify_jobs::gitea::raw_url_encode(message)
    ))
}

type QueryMap = Query<HashMap<String, String>>;

/// `consumeGithubAppSetupState()`.
async fn consume_setup_state(
    state: &AppState,
    ctx: &crate::session::WebUser,
    q: &HashMap<String, String>,
    action: &str,
) -> Result<Record, Response> {
    let token = q.get("state").map_or("", String::as_str);
    let payload = (!token.is_empty())
        .then(|| pull_state("github-app-setup-state", token))
        .flatten()
        .filter(|p| p.get("action").and_then(Value::as_str) == Some(action));
    let Some(payload) = payload else {
        return Err(redirect("/sources"));
    };
    let team = payload.get("team_id").and_then(Value::as_i64).unwrap_or(-1);
    if !ctx.member_of(team) {
        return Err(StatusCode::FORBIDDEN.into_response());
    }
    let id = payload
        .get("github_app_id")
        .and_then(Value::as_i64)
        .unwrap_or(-1);
    let row = coolify_db::git_sources::source_row(&state.db, "github_apps", id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())?;
    let Some(row) = row.filter(|r| r.get("team_id").and_then(Value::as_i64) == Some(team)) else {
        return Err(StatusCode::NOT_FOUND.into_response());
    };
    // refreshSession($github_app->team) when the user switched teams on GitHub.
    ctx.switch_team(state, team);
    Ok(row)
}

fn abort(status: StatusCode, message: &str) -> Response {
    (status, message.to_owned()).into_response()
}

/// `Webhook\Github::redirect()`: exchanges the manifest code for the App's credentials.
pub async fn github_redirect(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): QueryMap,
) -> Response {
    let user = match crate::session::web_user(&state, &headers).await {
        Ok(u) => u,
        Err(r) => return r,
    };
    let code = q.get("code").map_or("", String::as_str).to_owned();
    if code.is_empty() {
        return abort(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Missing GitHub App manifest code.",
        );
    }
    let row = match consume_setup_state(&state, &user, &q, "manifest").await {
        Ok(r) => r,
        Err(r) => return r,
    };
    let configured = row.get("app_id").is_some_and(|v| !v.is_null())
        || str_of(&row, "client_id").is_some()
        || str_of(&row, "client_secret").is_some()
        || str_of(&row, "webhook_secret").is_some()
        || row.get("private_key_id").is_some_and(|v| !v.is_null());
    if configured {
        return abort(
            StatusCode::FORBIDDEN,
            "GitHub App credentials are already configured.",
        );
    }
    let api = str_of(&row, "api_url").unwrap_or_default().to_owned();
    let data = match coolify_jobs::github::manifest_conversion(&state.db, &api, &code).await {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!(error = %e, "github manifest conversion failed");
            return abort(StatusCode::INTERNAL_SERVER_ERROR, "Server Error");
        }
    };
    let field = |k: &str| {
        data.get(k)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    let app_id = data.get("id").and_then(Value::as_i64);
    let (slug, client_id, client_secret, pem, webhook_secret) = (
        field("slug"),
        field("client_id"),
        field("client_secret"),
        field("pem"),
        field("webhook_secret"),
    );
    if app_id.is_none()
        || [&slug, &client_id, &client_secret, &pem, &webhook_secret]
            .iter()
            .any(|v| v.is_empty())
    {
        return abort(
            StatusCode::UNPROCESSABLE_ENTITY,
            "GitHub App manifest conversion response is incomplete.",
        );
    }
    let Some(enc) = state.encrypter.as_ref() else {
        return abort(StatusCode::INTERNAL_SERVER_ERROR, "Server Error");
    };
    let Some(info) = inspect(&pem) else {
        return abort(
            StatusCode::UNPROCESSABLE_ENTITY,
            "The private key is invalid.",
        );
    };
    let team = row.get("team_id").and_then(Value::as_i64).unwrap_or(-1);
    let now = state.now();
    let stored = format_private_key(&pem);
    let key_name = format!("github-app-{slug}");
    let created = private_keys::create(
        &state.db,
        NewKey {
            team_id: team,
            name: &key_name,
            description: "",
            private_key_encrypted: &enc.encrypt_string(&stored),
            fingerprint: &info.fingerprint,
        },
        now,
    )
    .await;
    let Ok(key_uuid) = created else {
        return abort(StatusCode::INTERNAL_SERVER_ERROR, "Server Error");
    };
    // `PrivateKey::create(['is_git_related' => true])`, without a description.
    let key_id: Result<i64, _> = sqlx::query_scalar(
        "UPDATE private_keys SET is_git_related = true, description = NULL WHERE uuid = $1 RETURNING id",
    )
        .bind(&key_uuid)
        .fetch_one(state.db.pool())
        .await;
    let Ok(key_id) = key_id else {
        return abort(StatusCode::INTERNAL_SERVER_ERROR, "Server Error");
    };
    let saved = coolify_db::database_parts::update_row(
        &state.db,
        "github_apps",
        id_of(&row),
        &[
            ("name", ColumnValue::Text(Some(slug))),
            ("app_id", ColumnValue::Int(app_id.unwrap_or_default())),
            ("client_id", ColumnValue::Text(Some(client_id))),
            (
                "client_secret",
                ColumnValue::Text(Some(enc.encrypt_serialized(&client_secret))),
            ),
            (
                "webhook_secret",
                ColumnValue::Text(Some(enc.encrypt_serialized(&webhook_secret))),
            ),
            ("private_key_id", ColumnValue::Int(key_id)),
        ],
        now,
    )
    .await;
    if saved.is_err() {
        return abort(StatusCode::INTERNAL_SERVER_ERROR, "Server Error");
    }
    redirect(&format!(
        "/source/github/{}",
        str_of(&row, "uuid").unwrap_or_default()
    ))
}

/// `Webhook\Github::install()`.
pub async fn github_install(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): QueryMap,
) -> Response {
    let user = match crate::session::web_user(&state, &headers).await {
        Ok(u) => u,
        Err(r) => return r,
    };
    let action = q.get("setup_action").map_or("", String::as_str);
    if !matches!(action, "install" | "update") {
        return abort(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Invalid GitHub App setup action.",
        );
    }
    let installation = q
        .get("installation_id")
        .map_or("", String::as_str)
        .to_owned();
    if installation.is_empty() || !installation.bytes().all(|b| b.is_ascii_digit()) {
        return abort(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Missing GitHub App installation id.",
        );
    }
    if action == "update" {
        // redirectAfterGithubAppInstallationUpdate()
        let uuid: Result<Option<String>, _> = sqlx::query_scalar(
            "SELECT uuid FROM github_apps WHERE (team_id = $1 OR is_system_wide = true) AND installation_id = $2::integer",
        )
        .bind(user.team_id)
        .bind(&installation)
        .fetch_optional(state.db.pool())
        .await;
        return match uuid {
            Ok(Some(u)) => redirect(&format!("/source/github/{u}")),
            _ => redirect("/sources"),
        };
    }
    let row = match consume_setup_state(&state, &user, &q, "install").await {
        Ok(r) => r,
        Err(r) => return r,
    };
    let Some(enc) = state.encrypter.as_ref() else {
        return abort(StatusCode::INTERNAL_SERVER_ERROR, "Server Error");
    };
    let page = format!(
        "/source/github/{}",
        str_of(&row, "uuid").unwrap_or_default()
    );
    if let Some(reason) =
        coolify_jobs::github::installation_verification_error(&state.db, enc, &row, &installation)
            .await
    {
        tracing::warn!(github_app_id = id_of(&row), installation_id = %installation, reason = %reason, "GitHub App installation could not be verified.");
        return redirect_with_error(
            &page,
            &format!("GitHub App installation could not be verified: {reason}"),
        );
    }
    let saved = coolify_db::database_parts::update_row(
        &state.db,
        "github_apps",
        id_of(&row),
        &[(
            "installation_id",
            ColumnValue::Int(installation.parse().unwrap_or_default()),
        )],
        state.now(),
    )
    .await;
    if saved.is_err() {
        return abort(StatusCode::INTERNAL_SERVER_ERROR, "Server Error");
    }
    redirect(&page)
}

/// `Webhook\Gitlab::redirect()` and `Webhook\Gitea::redirect()`.
async fn oauth_redirect(
    state: AppState,
    headers: HeaderMap,
    q: HashMap<String, String>,
    p: Provider,
) -> Response {
    let user = match crate::session::web_user(&state, &headers).await {
        Ok(u) => u,
        Err(r) => return r,
    };
    let name = p.name();
    let (Some(code), Some(token)) = (
        q.get("code").filter(|c| !c.is_empty()),
        q.get("state").filter(|c| !c.is_empty()),
    ) else {
        return redirect_with_error(
            "/sources",
            &format!("Invalid {name} OAuth callback. Missing code or state."),
        );
    };
    let payload = pull_state(&format!("{}-app-oauth-state", p.slug()), token)
        .filter(|v| v.get("team_id").and_then(Value::as_i64) == Some(user.team_id));
    let Some(payload) = payload else {
        return redirect_with_error(
            "/sources",
            &format!(
                "Invalid or expired {name} OAuth state. Please start the authorization again."
            ),
        );
    };
    let id = payload
        .get(format!("{}_app_id", p.slug()))
        .and_then(Value::as_i64)
        .unwrap_or(-1);
    let row = match coolify_db::git_sources::source_row(&state.db, p.table(), id).await {
        Ok(Some(r)) => r,
        _ => return redirect_with_error("/sources", "No query results for model."),
    };
    let ctx = user.ctx();
    match can_update(&state, &ctx, &row).await {
        Ok(true) => {}
        _ => {
            return redirect_with_error(
                "/sources",
                &format!("You are not authorized to connect this {name} App."),
            );
        }
    }
    let page = format!(
        "/source/{}/{}",
        p.slug(),
        str_of(&row, "uuid").unwrap_or_default()
    );
    let base = str_of(&row, "html_url")
        .unwrap_or_default()
        .trim_end_matches('/')
        .to_owned();
    let path = if p == Provider::Gitlab {
        "/oauth/token"
    } else {
        "/login/oauth/access_token"
    };
    let Some(enc) = state.encrypter.as_ref() else {
        return abort(StatusCode::INTERNAL_SERVER_ERROR, "Server Error");
    };
    let exchange = async {
        let http =
            coolify_jobs::git_http::client(Some(&state.db), &base, Duration::from_secs(30)).await?;
        let client_id = str_of(&row, "client_id").unwrap_or_default().to_owned();
        let client_secret = secret(&state, &row, "client_secret").unwrap_or_default();
        let redirect_uri = str_of(&row, "redirect_uri").unwrap_or_default().to_owned();
        let form = [
            ("client_id", client_id.as_str()),
            ("client_secret", client_secret.as_str()),
            ("code", code.as_str()),
            ("grant_type", "authorization_code"),
            ("redirect_uri", redirect_uri.as_str()),
        ];
        http.post(format!("{base}{path}"))
            .form(&form)
            .send()
            .await
            .map_err(|e| e.to_string())
    }
    .await;
    let response = match exchange {
        Ok(r) => r,
        Err(e) => return redirect_with_error("/sources", &e),
    };
    let ok_status = response.status().is_success();
    let data: Value = response.json().await.unwrap_or(Value::Null);
    if !ok_status {
        let error = data
            .get("error_description")
            .and_then(Value::as_str)
            .unwrap_or("Token exchange failed");
        return redirect_with_error(&page, &format!("{name} OAuth failed: {error}"));
    }
    let access = data
        .get("access_token")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let refresh = data
        .get("refresh_token")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let default_expiry = if p == Provider::Gitlab { 7200 } else { 3600 };
    let expires_in = data
        .get("expires_in")
        .and_then(Value::as_i64)
        .unwrap_or(default_expiry);
    let stored = coolify_db::git_sources::store_oauth_tokens(
        &state.db,
        p.table(),
        id,
        &enc.encrypt_serialized(access),
        &enc.encrypt_serialized(refresh),
        chrono::Utc::now().timestamp() + expires_in,
    )
    .await;
    if let Err(e) = stored {
        return redirect_with_error("/sources", &e.to_string());
    }
    redirect(&page)
}

pub async fn gitlab_redirect(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): QueryMap,
) -> Response {
    oauth_redirect(state, headers, q, Provider::Gitlab).await
}

pub async fn gitea_redirect(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): QueryMap,
) -> Response {
    oauth_redirect(state, headers, q, Provider::Gitea).await
}
