//! `GithubController` and `GitlabController`: the `/github-apps` and `/gitlab-apps` API.

use axum::Extension;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::IntoResponse;
use coolify_db::records::Record;
use coolify_db::servers::ColumnValue;
use serde_json::{Map, Value, json};

use crate::auth::AuthCtx;
use crate::input::{Input, lenient_json, validated_json};
use crate::response::{ApiError, ApiResult, PhpJson, created, ok};
use crate::safe_url;
use crate::serialize::serialize_record;
use crate::state::AppState;
use crate::validation::{CUSTOM_API_MESSAGES, Field, Rule, validate};

pub const GITHUB_MORPH: &str = r"App\Models\GithubApp";
pub const GITLAB_MORPH: &str = r"App\Models\GitlabApp";
pub const GITEA_MORPH: &str = r"App\Models\GiteaApp";

fn db_err(e: sqlx::Error) -> ApiError {
    tracing::error!(error = %e, "git source query failed");
    ApiError::Internal
}

/// PHP `FILTER_VALIDATE_BOOLEAN` on a JSON value.
fn truthy_bool(v: Option<&Value>) -> bool {
    match v {
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_i64() == Some(1),
        Some(Value::String(s)) => {
            matches!(s.to_lowercase().as_str(), "1" | "true" | "on" | "yes")
        }
        _ => false,
    }
}

/// `organization`'s `regex:/\A[^\s\/?#]+\z/`.
static ORGANIZATION: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"\A[^\s/?#]+\z").unwrap_or_else(|_| unreachable!())
});

/// `User::canAccessSystemResources()`: admin or owner of the root team.
pub async fn can_access_system_resources(state: &AppState, user_id: i64) -> Result<bool, ApiError> {
    let role: Option<String> =
        sqlx::query_scalar("SELECT role FROM team_user WHERE team_id = 0 AND user_id = $1")
            .bind(user_id)
            .fetch_optional(state.db.pool())
            .await
            .map_err(db_err)?;
    Ok(matches!(role.as_deref(), Some("admin" | "owner")))
}

/// `$user->isAdminOfTeam($teamId)` for the policies' `update` and `delete` on a source row.
async fn authorize_change(state: &AppState, ctx: &AuthCtx, row: &Record) -> Result<(), ApiError> {
    let system = row
        .get("is_system_wide")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let allowed = if system {
        can_access_system_resources(state, ctx.user_id).await?
    } else {
        ctx.is_team_admin()
    };
    if allowed {
        Ok(())
    } else {
        Err(ApiError::Unauthorized)
    }
}

/// Model attributes as `toArray()` shows them: timestamps in Laravel's format, hidden columns
/// dropped.
fn attributes(raw: &Record, hidden: &[&str]) -> Record {
    let mut out = Record::new();
    for (k, v) in raw {
        if hidden.contains(&k.as_str()) {
            continue;
        }
        let v = match (k.as_str(), v) {
            ("created_at" | "updated_at", Value::String(s)) => {
                Value::String(coolify_db::records::laravel_datetime(s))
            }
            _ => v.clone(),
        };
        out.insert(k.clone(), v);
    }
    out
}

fn decrypted_into(
    state: &AppState,
    raw: &Record,
    out: &mut Record,
    columns: &[&str],
) -> Result<(), ApiError> {
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    for k in columns {
        if let Some(Value::String(c)) = raw.get(*k) {
            let plain = enc
                .decrypt_maybe_serialized(c)
                .unwrap_or_else(|_| c.clone());
            out.insert((*k).to_owned(), Value::String(plain));
        } else if let Some(v) = raw.get(*k) {
            out.insert((*k).to_owned(), v.clone());
        }
    }
    Ok(())
}

const GITHUB_HIDDEN: &[&str] = &["client_secret", "webhook_secret"];
const GITLAB_HIDDEN: &[&str] = &[
    "webhook_token",
    "app_secret",
    "client_secret",
    "access_token",
    "refresh_token",
];

/// `GithubController::removeSensitiveData()`.
fn present_github(state: &AppState, ctx: &AuthCtx, raw: &Record) -> Result<Value, ApiError> {
    let mut r = attributes(raw, GITHUB_HIDDEN);
    if ctx.can_read_sensitive && raw.get("team_id").and_then(Value::as_i64) == Some(ctx.team_id) {
        decrypted_into(state, raw, &mut r, GITHUB_HIDDEN)?;
    }
    r.insert("type".into(), Value::String("github".into()));
    Ok(Value::Object(serialize_record(r)))
}

/// `GitlabController::removeSensitiveData()`.
fn present_gitlab(state: &AppState, ctx: &AuthCtx, raw: &Record) -> Result<Value, ApiError> {
    let mut r = attributes(raw, GITLAB_HIDDEN);
    if ctx.can_read_sensitive && raw.get("team_id").and_then(Value::as_i64) == Some(ctx.team_id) {
        decrypted_into(
            state,
            raw,
            &mut r,
            &[
                "client_secret",
                "webhook_token",
                "access_token",
                "refresh_token",
            ],
        )?;
    }
    Ok(Value::Object(serialize_record(r)))
}

async fn list_rows(state: &AppState, table: &str, team: i64) -> Result<Vec<Record>, ApiError> {
    let rows: Vec<String> = sqlx::query_scalar(&format!(
        "SELECT row_to_json(s)::text FROM {table} s WHERE team_id = $1 OR is_system_wide = true"
    ))
    .bind(team)
    .fetch_all(state.db.pool())
    .await
    .map_err(db_err)?;
    rows.iter()
        .map(|t| coolify_db::records::parse_row(t).map_err(|_| ApiError::Internal))
        .collect()
}

/// `where('id', $id)->where('team_id', $teamId)->firstOrFail()`. PostgreSQL refuses a route id
/// that is no integer, which upstream answers with a 500.
async fn team_row(
    state: &AppState,
    table: &str,
    id: &str,
    team: i64,
) -> Result<Option<Record>, ApiError> {
    let Ok(id) = id.parse::<i64>() else {
        return Err(ApiError::Message(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Server Error",
        ));
    };
    let row: Option<String> = sqlx::query_scalar(&format!(
        "SELECT row_to_json(s)::text FROM {table} s WHERE id = $1 AND team_id = $2"
    ))
    .bind(id)
    .bind(team)
    .fetch_optional(state.db.pool())
    .await
    .map_err(db_err)?;
    row.map(|t| coolify_db::records::parse_row(&t).map_err(|_| ApiError::Internal))
        .transpose()
}

async fn fresh_row(state: &AppState, table: &str, id: i64) -> Result<Record, ApiError> {
    coolify_db::git_sources::source_row(&state.db, table, id)
        .await?
        .ok_or(ApiError::Internal)
}

/// How many applications use a source (`$app->applications`).
pub async fn application_count(state: &AppState, morph: &str, id: i64) -> Result<i64, ApiError> {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM applications WHERE source_type = $1 AND source_id = $2 AND deleted_at IS NULL",
    )
    .bind(morph)
    .bind(id)
    .fetch_one(state.db.pool())
    .await
    .map_err(db_err)
}

/// Merges `SafeExternalUrl` failures, computed before `validate`, into the field order.
async fn validate_with_urls(
    state: &AppState,
    headers: &HeaderMap,
    input: &Input,
    specs: &[Field],
    allowed: &[&str],
    urls: &[&'static str],
    git_source: bool,
) -> Result<(), ApiError> {
    let first = validate(input, specs, CUSTOM_API_MESSAGES, allowed);
    let mut url_errors: Vec<(&str, String)> = Vec::new();
    for k in urls {
        if let Some(v) = input.get(k).and_then(Value::as_str)
            && let Some(msg) = safe_url::check_with(state, k, v, headers, git_source).await?
        {
            url_errors.push((k, msg));
        }
    }
    if url_errors.is_empty() {
        return first;
    }
    let mut errors = match first {
        Err(ApiError::Validation(m)) => m,
        Err(other) => return Err(other),
        Ok(()) => Map::new(),
    };
    for (k, msg) in url_errors {
        match errors.get_mut(k) {
            Some(Value::Array(list)) => list.push(Value::String(msg)),
            _ => {
                errors.insert(k.to_owned(), json!([msg]));
            }
        }
    }
    let mut ordered = Map::new();
    for f in specs {
        if let Some(v) = errors.shift_remove(f.name) {
            ordered.insert(f.name.to_owned(), v);
        }
    }
    ordered.extend(errors);
    Err(ApiError::Validation(ordered))
}

fn github_create_fields() -> Vec<Field> {
    let f = |name, rules| Field { name, rules };
    vec![
        f("name", vec![Rule::Required, Rule::String, Rule::Max(255)]),
        f(
            "organization",
            vec![
                Rule::Nullable,
                Rule::String,
                Rule::Max(255),
                Rule::Regex(&ORGANIZATION),
            ],
        ),
        f("api_url", vec![Rule::Nullable, Rule::String, Rule::Url]),
        f("html_url", vec![Rule::Required, Rule::String, Rule::Url]),
        f(
            "custom_user",
            vec![Rule::Nullable, Rule::String, Rule::Max(255)],
        ),
        f(
            "custom_port",
            vec![
                Rule::Nullable,
                Rule::Integer,
                Rule::MinNum(1),
                Rule::MaxNum(65535),
            ],
        ),
        f("app_id", vec![Rule::Required, Rule::Integer]),
        f("installation_id", vec![Rule::Required, Rule::Integer]),
        f(
            "client_id",
            vec![Rule::Required, Rule::String, Rule::Max(255)],
        ),
        f("client_secret", vec![Rule::Required, Rule::String]),
        f("webhook_secret", vec![Rule::Required, Rule::String]),
        f("private_key_uuid", vec![Rule::Required, Rule::String]),
        f("is_system_wide", vec![Rule::Boolean]),
    ]
}

const GITHUB_ALLOWED: &[&str] = &[
    "name",
    "organization",
    "api_url",
    "html_url",
    "custom_user",
    "custom_port",
    "app_id",
    "installation_id",
    "client_id",
    "client_secret",
    "webhook_secret",
    "private_key_uuid",
    "is_system_wide",
];

/// `GET /github-apps`.
pub async fn github_index(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
) -> ApiResult {
    let rows = list_rows(&state, "github_apps", ctx.team_id).await?;
    let mut out = Vec::with_capacity(rows.len());
    for r in &rows {
        out.push(present_github(&state, &ctx, r)?);
    }
    Ok(ok(Value::Array(out)).into_response())
}

/// `$request->merge(['organization' => normalizeGithubOrganization(...)])`.
fn with_normalized_organization(input: &mut Input) {
    let organization = coolify_jobs::github::normalize_organization(input.str("organization"))
        .map_or(Value::Null, Value::String);
    input.0.insert("organization".into(), organization);
}

fn int_of(v: Option<&Value>) -> Option<i64> {
    match v {
        Some(Value::Number(n)) => n.as_i64(),
        Some(Value::String(s)) => s.trim().parse().ok(),
        _ => None,
    }
}

/// `POST /github-apps`.
pub async fn github_store(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let mut input = validated_json(&headers, &uri, &body)?;
    with_normalized_organization(&mut input);
    validate_with_urls(
        &state,
        &headers,
        &input,
        &github_create_fields(),
        GITHUB_ALLOWED,
        &["api_url", "html_url"],
        true,
    )
    .await?;
    if truthy_bool(input.get("is_system_wide"))
        && !can_access_system_resources(&state, ctx.user_id).await?
    {
        return Err(ApiError::Unauthorized);
    }
    let key: Option<i64> =
        sqlx::query_scalar("SELECT id FROM private_keys WHERE uuid = $1 AND team_id = $2")
            .bind(input.str("private_key_uuid").unwrap_or_default())
            .bind(ctx.team_id)
            .fetch_optional(state.db.pool())
            .await
            .map_err(db_err)?;
    let Some(key) = key else {
        return Err(ApiError::Message(
            StatusCode::NOT_FOUND,
            "Private key not found or does not belong to your team.",
        ));
    };
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let html = input.str("html_url").unwrap_or_default().to_owned();
    let api = input
        .str("api_url")
        .filter(|a| !a.trim().is_empty())
        .map_or_else(
            || coolify_jobs::github::api_url_from_html_url(&html),
            str::to_owned,
        );
    let uuid = coolify_domain::new_public_id();
    let now = state.now();
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO github_apps (uuid, name, organization, api_url, html_url, custom_user, custom_port, app_id, installation_id, \
         client_id, client_secret, webhook_secret, private_key_id, is_public, team_id, is_system_wide, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, false, $14, $15, $16, $16) RETURNING id",
    )
    .bind(&uuid)
    .bind(input.str("name"))
    .bind(input.str("organization"))
    .bind(&api)
    .bind(&html)
    .bind(input.str("custom_user").unwrap_or("git"))
    .bind(int_of(input.get("custom_port")).unwrap_or(22) as i32)
    .bind(int_of(input.get("app_id")).map(|v| v as i32))
    .bind(int_of(input.get("installation_id")).map(|v| v as i32))
    .bind(input.str("client_id"))
    .bind(enc.encrypt_serialized(input.str("client_secret").unwrap_or_default()))
    .bind(enc.encrypt_serialized(input.str("webhook_secret").unwrap_or_default()))
    .bind(key)
    .bind(ctx.team_id)
    .bind(truthy_bool(input.get("is_system_wide")))
    .bind(now)
    .fetch_one(state.db.pool())
    .await
    .map_err(db_err)?;
    tracing::info!(target: "audit", event = "api.github_app.created", team_id = ctx.team_id, github_app_uuid = %uuid, "github app created");
    // `response()->json($githubApp, 201)`: the new model, not serialized for the API.
    let raw = fresh_row(&state, "github_apps", id).await?;
    let mut shape = created_github_shape(&raw);
    // `$teamId` from the token stays the string it was read as.
    shape.insert("team_id".into(), Value::String(ctx.team_id_raw.clone()));
    Ok(created(Value::Object(shape)).into_response())
}

/// The order `GithubApp::create($payload)` leaves the attributes in: the fillable payload, then
/// the timestamps and the id; `uuid` comes from the model's `creating` hook; `type` is appended.
fn created_github_shape(raw: &Record) -> Record {
    let mut out = Record::new();
    let mut put = |k: &str| {
        if let Some(v) = raw.get(k) {
            let v = match (k, v) {
                ("created_at" | "updated_at", Value::String(s)) => {
                    Value::String(coolify_db::records::laravel_datetime(s))
                }
                _ => v.clone(),
            };
            out.insert(k.to_owned(), v);
        }
    };
    for k in [
        "name",
        "organization",
        "api_url",
        "html_url",
        "custom_user",
        "custom_port",
        "app_id",
        "installation_id",
        "client_id",
        "private_key_id",
        "is_public",
        "team_id",
        "is_system_wide",
        "uuid",
        "updated_at",
        "created_at",
        "id",
    ] {
        put(k);
    }
    out.insert("type".into(), Value::String("github".into()));
    out
}

/// A `handleError()` answer from a helper's exception.
fn handled(message: String) -> ApiResult {
    tracing::warn!(error = %message, "github app request failed");
    Ok(PhpJson(
        StatusCode::INTERNAL_SERVER_ERROR,
        json!({"message": "Server Error"}),
    )
    .into_response())
}

/// `GET /github-apps/{id}/repositories`.
pub async fn github_repositories(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(id): Path<String>,
) -> ApiResult {
    let Some(row) = team_row(&state, "github_apps", &id, ctx.team_id).await? else {
        return Err(ApiError::Message(
            StatusCode::NOT_FOUND,
            "GitHub app not found",
        ));
    };
    let token = match coolify_jobs::github::installation_token(
        &state.db,
        state.encrypter.as_ref(),
        Some(&row),
    )
    .await
    {
        Ok(t) => t,
        Err(e) => return handled(e),
    };
    match coolify_jobs::github::installation_repositories(&state.db, &row, &token).await {
        Ok(Ok(mut repositories)) => {
            // Collection::sortBy('name')->values() (stable).
            repositories.sort_by(|a, b| {
                let name = |v: &Value| {
                    v.get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned()
                };
                name(a).cmp(&name(b))
            });
            Ok(ok(json!({"repositories": repositories})).into_response())
        }
        Ok(Err((status, body))) => {
            let message = body
                .get("message")
                .cloned()
                .unwrap_or_else(|| json!("Failed to load repositories"));
            Ok(PhpJson(
                StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY),
                json!({"message": message}),
            )
            .into_response())
        }
        Err(e) => handled(e),
    }
}

/// `GET /github-apps/{id}/repositories/{owner}/{repo}/branches`.
pub async fn github_branches(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((id, owner, repo)): Path<(String, String, String)>,
) -> ApiResult {
    let Some(row) = team_row(&state, "github_apps", &id, ctx.team_id).await? else {
        return Err(ApiError::Message(
            StatusCode::NOT_FOUND,
            "GitHub app not found",
        ));
    };
    let token = match coolify_jobs::github::installation_token(
        &state.db,
        state.encrypter.as_ref(),
        Some(&row),
    )
    .await
    {
        Ok(t) => t,
        Err(e) => return handled(e),
    };
    let endpoint = format!("/repos/{owner}/{repo}/branches");
    match coolify_jobs::github::get_with_token(&state.db, &row, &token, &endpoint, &[]).await {
        Ok((200, branches)) => Ok(ok(json!({"branches": branches})).into_response()),
        Ok((status, body)) => Ok(PhpJson(
            StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY),
            json!({"message": "Error loading branches from GitHub.", "error": body.get("message")}),
        )
        .into_response()),
        Err(e) => handled(e),
    }
}

/// `$request->only($allowedFields)`: the present keys, in the order of the list.
fn only(input: &Input, allowed: &[&str]) -> Input {
    let mut out = Map::new();
    for k in allowed {
        if let Some(v) = input.get(k) {
            out.insert((*k).to_owned(), v.clone());
        }
    }
    Input(out)
}

/// `isset($payload[$k])`.
fn is_set(input: &Input, k: &str) -> bool {
    input.get(k).is_some_and(|v| !v.is_null())
}

/// `PATCH /github-apps/{id}`.
pub async fn github_update(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(id): Path<String>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let Some(row) = team_row(&state, "github_apps", &id, ctx.team_id).await? else {
        return Err(ApiError::Message(
            StatusCode::NOT_FOUND,
            "GitHub app not found",
        ));
    };
    authorize_change(&state, &ctx, &row).await?;
    let input = lenient_json(&headers, &uri, &body);
    const ALLOWED: &[&str] = &[
        "name",
        "organization",
        "api_url",
        "html_url",
        "custom_user",
        "custom_port",
        "app_id",
        "installation_id",
        "client_id",
        "client_secret",
        "webhook_secret",
        "private_key_uuid",
        "is_system_wide",
    ];
    let mut payload = only(&input, ALLOWED);
    if payload.has("organization") {
        let v = coolify_jobs::github::normalize_organization(payload.str("organization"))
            .map_or(Value::Null, Value::String);
        payload.0.insert("organization".into(), v);
    }
    let mut specs: Vec<Field> = Vec::new();
    let mut add = |name: &'static str, rules: Vec<Rule>| {
        if is_set(&payload, name) {
            specs.push(Field { name, rules });
        }
    };
    add("name", vec![Rule::String]);
    add(
        "organization",
        vec![Rule::Nullable, Rule::String, Rule::Regex(&ORGANIZATION)],
    );
    add("api_url", vec![Rule::Url]);
    add("html_url", vec![Rule::Url]);
    add("custom_user", vec![Rule::String]);
    add(
        "custom_port",
        vec![Rule::Integer, Rule::MinNum(1), Rule::MaxNum(65535)],
    );
    add("app_id", vec![Rule::Integer]);
    add("installation_id", vec![Rule::Integer]);
    add("client_id", vec![Rule::String]);
    add("client_secret", vec![Rule::String]);
    add("webhook_secret", vec![Rule::String]);
    add("private_key_uuid", vec![Rule::String]);
    add("is_system_wide", vec![Rule::Boolean]);
    let result = validate_with_urls(
        &state,
        &headers,
        &payload,
        &specs,
        ALLOWED,
        &["api_url", "html_url"],
        true,
    )
    .await;
    match result {
        Ok(()) => {}
        Err(ApiError::Validation(errors)) => {
            return Err(ApiError::ValidationWithMessage("Validation error", errors));
        }
        Err(other) => return Err(other),
    }
    let system_now = row
        .get("is_system_wide")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if truthy_bool(payload.get("is_system_wide"))
        && !system_now
        && !can_access_system_resources(&state, ctx.user_id).await?
    {
        return Err(ApiError::Unauthorized);
    }
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let mut values: Vec<(&'static str, ColumnValue)> = Vec::new();
    for k in [
        "name",
        "organization",
        "api_url",
        "html_url",
        "custom_user",
        "client_id",
    ] {
        if payload.has(k) {
            values.push((k, ColumnValue::Text(payload.str(k).map(str::to_owned))));
        }
    }
    if is_set(&payload, "html_url") && payload.str("api_url").is_none_or(|a| a.trim().is_empty()) {
        values.retain(|(k, _)| *k != "api_url");
        values.push((
            "api_url",
            ColumnValue::Text(Some(coolify_jobs::github::api_url_from_html_url(
                payload.str("html_url").unwrap_or_default(),
            ))),
        ));
    }
    for k in ["custom_port", "app_id", "installation_id"] {
        if let Some(v) = payload.get(k) {
            match int_of(Some(v)) {
                Some(i) => values.push((k, ColumnValue::Int(i))),
                None => values.push((k, ColumnValue::Text(None))),
            }
        }
    }
    for k in ["client_secret", "webhook_secret"] {
        if let Some(v) = payload.get(k) {
            let plain = v.as_str();
            let current = row
                .get(k)
                .and_then(Value::as_str)
                .and_then(|c| enc.decrypt_maybe_serialized(c).ok());
            if plain.map(str::to_owned) != current {
                values.push((
                    k,
                    ColumnValue::Text(plain.map(|p| enc.encrypt_serialized(p))),
                ));
            }
        }
    }
    if let Some(v) = payload.get("is_system_wide") {
        values.push(("is_system_wide", ColumnValue::Bool(truthy_bool(Some(v)))));
    }
    if let Some(uuid) = payload.str("private_key_uuid") {
        let key: Option<i64> =
            sqlx::query_scalar("SELECT id FROM private_keys WHERE team_id = $1 AND uuid = $2")
                .bind(ctx.team_id)
                .bind(uuid)
                .fetch_optional(state.db.pool())
                .await
                .map_err(db_err)?;
        let Some(key) = key else {
            return Err(ApiError::Message(
                StatusCode::NOT_FOUND,
                "Private key not found or does not belong to your team",
            ));
        };
        values.push(("private_key_id", ColumnValue::Int(key)));
    }
    let row_id = row.get("id").and_then(Value::as_i64).unwrap_or(-1);
    coolify_db::database_parts::update_row(&state.db, "github_apps", row_id, &values, state.now())
        .await?;
    tracing::info!(target: "audit", event = "api.github_app.updated", team_id = ctx.team_id, github_app_id = row_id, "github app updated");
    let fresh = fresh_row(&state, "github_apps", row_id).await?;
    let mut data = attributes(&fresh, GITHUB_HIDDEN);
    data.insert("type".into(), Value::String("github".into()));
    Ok(ok(json!({"message": "GitHub app updated successfully", "data": data})).into_response())
}

/// The `deleting` hook of `GithubApp`: refuses live runners, drops an otherwise unused key.
pub async fn delete_github_row(
    state: &AppState,
    row: &Record,
) -> Result<Result<(), String>, ApiError> {
    let id = row.get("id").and_then(Value::as_i64).unwrap_or(-1);
    let live: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM github_runner_configs WHERE github_app_id = $1 AND is_enabled = true) \
         OR EXISTS (SELECT 1 FROM github_runner_executions WHERE github_app_id = $1 AND status IN ('queued', 'provisioning', 'idle', 'running'))",
    )
    .bind(id)
    .fetch_one(state.db.pool())
    .await
    .map_err(db_err)?;
    if live {
        return Ok(Err("You cannot delete this GitHub App because servers use it for GitHub Actions runners. Disable the runners on these servers and wait for running jobs to finish first.".into()));
    }
    let key = row.get("private_key_id").and_then(Value::as_i64);
    let mut tx = state.db.pool().begin().await.map_err(db_err)?;
    if let Some(key) = key {
        let used: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM servers WHERE private_key_id = $1) \
             OR EXISTS (SELECT 1 FROM applications WHERE private_key_id = $1 AND deleted_at IS NULL) \
             OR EXISTS (SELECT 1 FROM github_apps WHERE private_key_id = $1 AND id <> $2) \
             OR EXISTS (SELECT 1 FROM gitlab_apps WHERE private_key_id = $1) \
             OR EXISTS (SELECT 1 FROM gitea_apps WHERE private_key_id = $1)",
        )
        .bind(key)
        .bind(id)
        .fetch_one(&mut *tx)
        .await
        .map_err(db_err)?;
        if !used {
            // Clear the key first so the foreign key holds; the row goes right after.
            sqlx::query("UPDATE github_apps SET private_key_id = NULL WHERE id = $1")
                .bind(id)
                .execute(&mut *tx)
                .await
                .map_err(db_err)?;
            sqlx::query("DELETE FROM private_keys WHERE id = $1")
                .bind(key)
                .execute(&mut *tx)
                .await
                .map_err(db_err)?;
        }
    }
    sqlx::query("DELETE FROM github_apps WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;
    tx.commit().await.map_err(db_err)?;
    Ok(Ok(()))
}

/// `DELETE /github-apps/{id}`.
pub async fn github_destroy(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(id): Path<String>,
) -> ApiResult {
    let Some(row) = team_row(&state, "github_apps", &id, ctx.team_id).await? else {
        return Err(ApiError::Message(
            StatusCode::NOT_FOUND,
            "GitHub app not found",
        ));
    };
    authorize_change(&state, &ctx, &row).await?;
    let row_id = row.get("id").and_then(Value::as_i64).unwrap_or(-1);
    let count = application_count(&state, GITHUB_MORPH, row_id).await?;
    if count > 0 {
        return Ok(PhpJson(
            StatusCode::CONFLICT,
            json!({"message": format!("This GitHub app is being used by {count} application(s). Please delete all applications first.")}),
        )
        .into_response());
    }
    if let Err(e) = delete_github_row(&state, &row).await? {
        return handled(e);
    }
    tracing::info!(target: "audit", event = "api.github_app.deleted", team_id = ctx.team_id, github_app_id = row_id, "github app deleted");
    Ok(ok(json!({"message": "GitHub app deleted successfully"})).into_response())
}

// ---------------------------------------------------------------------------
// GitLab
// ---------------------------------------------------------------------------

const GITLAB_ALLOWED: &[&str] = &[
    "name",
    "html_url",
    "api_url",
    "custom_user",
    "custom_port",
    "group_name",
    "client_id",
    "client_secret",
    "webhook_token",
    "redirect_uri",
    "is_system_wide",
];

fn gitlab_create_fields() -> Vec<Field> {
    let f = |name, rules| Field { name, rules };
    vec![
        f("name", vec![Rule::Required, Rule::String, Rule::Max(255)]),
        f("html_url", vec![Rule::Required, Rule::String, Rule::Url]),
        f("api_url", vec![Rule::Nullable, Rule::String, Rule::Url]),
        f(
            "custom_user",
            vec![Rule::Nullable, Rule::String, Rule::Max(255)],
        ),
        f(
            "custom_port",
            vec![
                Rule::Nullable,
                Rule::Integer,
                Rule::MinNum(1),
                Rule::MaxNum(65535),
            ],
        ),
        f(
            "group_name",
            vec![Rule::Nullable, Rule::String, Rule::Max(255)],
        ),
        f(
            "client_id",
            vec![Rule::Nullable, Rule::String, Rule::Max(255)],
        ),
        f("client_secret", vec![Rule::Nullable, Rule::String]),
        f("webhook_token", vec![Rule::Nullable, Rule::String]),
        f(
            "redirect_uri",
            vec![Rule::Nullable, Rule::String, Rule::Url],
        ),
        f("is_system_wide", vec![Rule::Boolean]),
    ]
}

/// `GET /gitlab-apps`.
pub async fn gitlab_index(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
) -> ApiResult {
    let rows = list_rows(&state, "gitlab_apps", ctx.team_id).await?;
    let mut out = Vec::with_capacity(rows.len());
    for r in &rows {
        out.push(present_gitlab(&state, &ctx, r)?);
    }
    Ok(ok(Value::Array(out)).into_response())
}

/// `Str::random(32)`.
pub fn random_token(n: usize) -> String {
    use rand::Rng as _;
    const CHARS: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut rng = rand::thread_rng();
    (0..n)
        .map(|_| char::from(CHARS[rng.gen_range(0..CHARS.len())]))
        .collect()
}

fn text_of<'a>(input: &'a Input, k: &str) -> Option<&'a str> {
    input.str(k)
}

/// `POST /gitlab-apps`.
pub async fn gitlab_store(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let input = validated_json(&headers, &uri, &body)?;
    validate_with_urls(
        &state,
        &headers,
        &input,
        &gitlab_create_fields(),
        GITLAB_ALLOWED,
        &["html_url", "api_url"],
        false,
    )
    .await?;
    if truthy_bool(input.get("is_system_wide"))
        && !can_access_system_resources(&state, ctx.user_id).await?
    {
        return Err(ApiError::Unauthorized);
    }
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let html = text_of(&input, "html_url")
        .unwrap_or_default()
        .trim_end_matches('/')
        .to_owned();
    let api = text_of(&input, "api_url")
        .filter(|a| !a.trim().is_empty())
        .map_or_else(
            || format!("{html}/api/v4"),
            |a| a.trim_end_matches('/').to_owned(),
        );
    let webhook = text_of(&input, "webhook_token")
        .filter(|t| !t.is_empty() && *t != "0")
        .map_or_else(|| random_token(32), str::to_owned);
    let now = state.now();
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO gitlab_apps (uuid, name, html_url, api_url, custom_user, custom_port, group_name, client_id, client_secret, \
         webhook_token, redirect_uri, is_public, team_id, is_system_wide, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, false, $12, $13, $14, $14) RETURNING id",
    )
    .bind(coolify_domain::new_public_id())
    .bind(input.str("name"))
    .bind(&html)
    .bind(&api)
    .bind(input.str("custom_user").unwrap_or("git"))
    .bind(int_of(input.get("custom_port")).unwrap_or(22) as i32)
    .bind(input.str("group_name"))
    .bind(input.str("client_id"))
    .bind(input.str("client_secret").map(|v| enc.encrypt_serialized(v)))
    .bind(enc.encrypt_string(&webhook))
    .bind(input.str("redirect_uri"))
    .bind(ctx.team_id)
    .bind(truthy_bool(input.get("is_system_wide")))
    .bind(now)
    .fetch_one(state.db.pool())
    .await
    .map_err(db_err)?;
    tracing::info!(target: "audit", event = "api.gitlab_app.created", team_id = ctx.team_id, gitlab_app_id = id, "gitlab app created");
    let raw = fresh_row(&state, "gitlab_apps", id).await?;
    Ok(created(present_gitlab(&state, &ctx, &raw)?).into_response())
}

/// `PATCH /gitlab-apps/{id}`.
pub async fn gitlab_update(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(id): Path<String>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let Some(row) = team_row(&state, "gitlab_apps", &id, ctx.team_id).await? else {
        return Err(ApiError::Message(
            StatusCode::NOT_FOUND,
            "GitLab app not found",
        ));
    };
    authorize_change(&state, &ctx, &row).await?;
    let input = lenient_json(&headers, &uri, &body);
    let mut payload = only(&input, GITLAB_ALLOWED);
    let mut specs: Vec<Field> = Vec::new();
    let mut add_set = |name: &'static str, rules: Vec<Rule>| {
        if is_set(&payload, name) {
            specs.push(Field { name, rules });
        }
    };
    add_set("name", vec![Rule::String, Rule::Max(255)]);
    add_set("html_url", vec![Rule::Url]);
    add_set("api_url", vec![Rule::Url]);
    add_set("custom_user", vec![Rule::String, Rule::Max(255)]);
    add_set(
        "custom_port",
        vec![Rule::Integer, Rule::MinNum(1), Rule::MaxNum(65535)],
    );
    let mut add_present = |name: &'static str, rules: Vec<Rule>| {
        if payload.has(name) {
            specs.push(Field { name, rules });
        }
    };
    add_present(
        "group_name",
        vec![Rule::Nullable, Rule::String, Rule::Max(255)],
    );
    add_present(
        "client_id",
        vec![Rule::Nullable, Rule::String, Rule::Max(255)],
    );
    add_present("client_secret", vec![Rule::Nullable, Rule::String]);
    add_present("webhook_token", vec![Rule::Nullable, Rule::String]);
    add_present("redirect_uri", vec![Rule::Nullable, Rule::Url]);
    if is_set(&payload, "is_system_wide") {
        specs.push(Field {
            name: "is_system_wide",
            rules: vec![Rule::Boolean],
        });
    }
    match validate_with_urls(
        &state,
        &headers,
        &payload,
        &specs,
        GITLAB_ALLOWED,
        &["html_url", "api_url"],
        false,
    )
    .await
    {
        Ok(()) => {}
        Err(ApiError::Validation(errors)) => {
            return Err(ApiError::ValidationWithMessage("Validation error", errors));
        }
        Err(other) => return Err(other),
    }
    let system_now = row
        .get("is_system_wide")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if truthy_bool(payload.get("is_system_wide"))
        && !system_now
        && !can_access_system_resources(&state, ctx.user_id).await?
    {
        return Err(ApiError::Unauthorized);
    }
    if is_set(&payload, "html_url") {
        let html = payload
            .str("html_url")
            .unwrap_or_default()
            .trim_end_matches('/')
            .to_owned();
        if payload.str("api_url").is_none_or(|a| a.trim().is_empty()) {
            payload
                .0
                .insert("api_url".into(), Value::String(format!("{html}/api/v4")));
        }
        payload.0.insert("html_url".into(), Value::String(html));
    }
    if is_set(&payload, "api_url") {
        let api = payload
            .str("api_url")
            .unwrap_or_default()
            .trim_end_matches('/')
            .to_owned();
        payload.0.insert("api_url".into(), Value::String(api));
    }
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let mut values: Vec<(&'static str, ColumnValue)> = Vec::new();
    for k in [
        "name",
        "html_url",
        "api_url",
        "custom_user",
        "group_name",
        "client_id",
        "redirect_uri",
    ] {
        if payload.has(k) {
            values.push((k, ColumnValue::Text(payload.str(k).map(str::to_owned))));
        }
    }
    if let Some(v) = payload.get("custom_port") {
        match int_of(Some(v)) {
            Some(i) => values.push(("custom_port", ColumnValue::Int(i))),
            None => values.push(("custom_port", ColumnValue::Text(None))),
        }
    }
    if payload.has("client_secret") {
        let plain = payload.str("client_secret");
        let current = row
            .get("client_secret")
            .and_then(Value::as_str)
            .and_then(|c| enc.decrypt_maybe_serialized(c).ok());
        if plain.map(str::to_owned) != current {
            values.push((
                "client_secret",
                ColumnValue::Text(plain.map(|p| enc.encrypt_serialized(p))),
            ));
        }
    }
    if payload.has("webhook_token") {
        // The attribute mutator always writes a new ciphertext; the value decides if it is dirty.
        let plain = payload.str("webhook_token");
        values.push((
            "webhook_token",
            ColumnValue::Text(plain.map(|p| {
                if p.is_empty() {
                    String::new()
                } else {
                    enc.encrypt_string(p)
                }
            })),
        ));
    }
    if let Some(v) = payload.get("is_system_wide") {
        values.push(("is_system_wide", ColumnValue::Bool(truthy_bool(Some(v)))));
    }
    let row_id = row.get("id").and_then(Value::as_i64).unwrap_or(-1);
    coolify_db::database_parts::update_row(&state.db, "gitlab_apps", row_id, &values, state.now())
        .await?;
    tracing::info!(target: "audit", event = "api.gitlab_app.updated", team_id = ctx.team_id, gitlab_app_id = row_id, "gitlab app updated");
    let fresh = fresh_row(&state, "gitlab_apps", row_id).await?;
    Ok(ok(json!({"message": "GitLab app updated successfully", "data": present_gitlab(&state, &ctx, &fresh)?})).into_response())
}

/// `DELETE /gitlab-apps/{id}`.
pub async fn gitlab_destroy(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(id): Path<String>,
) -> ApiResult {
    let Some(row) = team_row(&state, "gitlab_apps", &id, ctx.team_id).await? else {
        return Err(ApiError::Message(
            StatusCode::NOT_FOUND,
            "GitLab app not found",
        ));
    };
    authorize_change(&state, &ctx, &row).await?;
    let row_id = row.get("id").and_then(Value::as_i64).unwrap_or(-1);
    let count = application_count(&state, GITLAB_MORPH, row_id).await?;
    if count > 0 {
        return Ok(PhpJson(
            StatusCode::CONFLICT,
            json!({"message": format!("This GitLab app is being used by {count} application(s). Please delete all applications first.")}),
        )
        .into_response());
    }
    sqlx::query("DELETE FROM gitlab_apps WHERE id = $1")
        .bind(row_id)
        .execute(state.db.pool())
        .await
        .map_err(db_err)?;
    tracing::info!(target: "audit", event = "api.gitlab_app.deleted", team_id = ctx.team_id, gitlab_app_id = row_id, "gitlab app deleted");
    Ok(ok(json!({"message": "GitLab app deleted successfully"})).into_response())
}
