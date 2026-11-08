//! Environment variables of a resource: `envs`, `create_env`, `update_env_by_uuid`,
//! `create_bulk_envs` and `delete_env_by_uuid` of `ApplicationsController`, plus the accessors of
//! `App\Models\EnvironmentVariable`. The logic is generic over [`ResourceScope`]; the
//! `application_*` handlers at the end are the application wrappers.

use std::sync::LazyLock;

use axum::Extension;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use coolify_db::applications::{self as apps, ApplicationRow};
use coolify_db::environment_variables as store;
use coolify_db::records::{ENVIRONMENT_VARIABLE, Record};
use coolify_db::servers::ColumnValue;
use coolify_jobs::databases::env::escape_env_variables;
use coolify_remote::text::php_trim;
use regex::Regex;
use serde_json::{Map, Value, json};

use crate::auth::AuthCtx;
use crate::input::{Input, lenient_json, validated_json};
use crate::response::{ApiError, ApiResult, created, ok};
use crate::serialize::{php_truthy, serialize_record};
use crate::state::AppState;
use crate::validation::{CUSTOM_API_MESSAGES, Field, Rule, validate};
use coolify_domain::grants::AccessLevel;

/// `ValidationPatterns::ENVIRONMENT_VARIABLE_KEY_PATTERN`.
static KEY_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\A[A-Za-z_][A-Za-z0-9_.]*\z").unwrap_or_else(|_| unreachable!()));
/// `isSharedReference()`.
static SHARED_REFERENCE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?s)^\{\{\s*(?:team|project|environment|server)\..*\}\}\n?\z")
        .unwrap_or_else(|_| unreachable!())
});
/// `get_real_environment_variables_internal`: `/{{(.*?)}}/`.
static SHARED_PLACEHOLDER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\{\{(.*?)\}\}").unwrap_or_else(|_| unreachable!()));

const ALLOWED: &[&str] = &[
    "key",
    "value",
    "is_preview",
    "is_literal",
    "is_multiline",
    "is_shown_once",
    "is_runtime",
    "is_buildtime",
    "comment",
];

/// What the variables of a resource need from it (`$env->resourceable`).
#[derive(Debug, Clone)]
pub struct ResourceScope {
    pub morph: &'static str,
    pub id: i64,
    pub uuid: String,
    pub team_id: i64,
    pub environment_id: Option<i64>,
    pub project_id: Option<i64>,
    /// `$resource->server` or `$resource->destination->server`.
    pub server_id: Option<i64>,
}

impl ResourceScope {
    pub fn application(a: &ApplicationRow) -> Self {
        Self {
            morph: apps::MORPH,
            id: a.id,
            uuid: a.uuid.clone(),
            team_id: a.team_id,
            environment_id: a.environment.get("id").and_then(Value::as_i64),
            project_id: a.project.get("id").and_then(Value::as_i64),
            server_id: a.server_id(),
        }
    }
}

/// `$request->uuid`: the request's own `uuid` input wins over the route parameter
/// (`Request::__get`), and `null` finds nothing.
pub(crate) fn magic_uuid(route: &str, input: &Input) -> Option<String> {
    match input.get("uuid") {
        None => Some(route.to_owned()),
        Some(Value::String(s)) => Some(s.clone()),
        Some(Value::Number(n)) => Some(n.to_string()),
        Some(_) => None,
    }
}

/// `isSharedReference()` for a value (blank is false).
pub fn is_shared_reference(value: Option<&str>) -> bool {
    value.is_some_and(|v| {
        let t = php_trim(v);
        !t.trim().is_empty() && SHARED_REFERENCE.is_match(t)
    })
}

/// `get_real_environment_variables_internal($value, $resource, null, $reveal)`.
pub async fn resolve_real_value(
    state: &AppState,
    scope: &ResourceScope,
    value: Option<&str>,
    reveal_locked: bool,
) -> Result<Option<String>, ApiError> {
    let Some(value) = value.filter(|v| !v.is_empty()) else {
        return Ok(value.map(str::to_owned));
    };
    let mut out = php_trim(value).to_owned();
    let found: Vec<String> = SHARED_PLACEHOLDER
        .captures_iter(&out)
        .map(|c| c[1].to_owned())
        .collect();
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    for shared in found {
        let trimmed = shared.trim();
        let Some((kind, variable)) = trimmed.split_once('.') else {
            continue;
        };
        let id = match kind {
            "environment" => scope.environment_id,
            "project" => scope.project_id,
            "team" => Some(scope.team_id),
            "server" => scope.server_id,
            _ => None,
        };
        let Some(id) = id else { continue };
        let row = store::shared(&state.db, scope.team_id, kind, variable, id).await?;
        if let Some((cipher, shown_once)) = row
            && (reveal_locked || !shown_once)
        {
            let plain = match cipher.as_deref() {
                Some(c) => enc.decrypt_string(c).map_err(|_| ApiError::Internal)?,
                None => String::new(),
            };
            out = out.replace(&format!("{{{{{shared}}}}}"), &plain);
        }
    }
    Ok(Some(out))
}

/// `legacyEscapedValue($value)`.
fn legacy_escaped_value(value: &str, literal_or_multiline: bool) -> String {
    let container = (value.starts_with('{') || value.starts_with('['))
        && serde_json::from_str::<Value>(value).is_ok();
    if container {
        value.to_owned()
    } else if literal_or_multiline {
        format!("'{value}'")
    } else {
        escape_env_variables(value)
    }
}

fn flag(r: &Record, k: &str) -> bool {
    r.get(k).is_some_and(php_truthy)
}

/// The decrypted, trimmed `value` of a row (the model's `value` getter).
fn plain_value(state: &AppState, row: &Record) -> Result<Option<String>, ApiError> {
    let Some(cipher) = row
        .get("value")
        .and_then(Value::as_str)
        .filter(|c| !c.is_empty() && *c != "0")
    else {
        return Ok(None);
    };
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let plain = enc
        .decrypt_maybe_serialized(cipher)
        .map_err(|_| ApiError::Internal)?;
    Ok(Some(php_trim(&plain).to_owned()))
}

/// How the model that is serialized was obtained.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Loaded {
    /// Read from the database: every column.
    Row,
    /// Just created: only the attributes `create()` set.
    Fresh,
}

/// `removeSensitiveData($env)` + `serializeApiResponse`. `request` carries the attributes without
/// a cast, which the saved model still holds as they were sent.
pub async fn present_variable(
    state: &AppState,
    ctx: &AuthCtx,
    scope: &ResourceScope,
    row: Record,
    loaded: Loaded,
    request: Option<&Input>,
) -> Result<Value, ApiError> {
    let value = plain_value(state, &row)?;
    let quoted = flag(&row, "is_literal") || flag(&row, "is_multiline");
    let legacy = flag(&row, "uses_legacy_escaping");
    let finish = |v: Option<String>| match v {
        Some(v) if legacy => Some(legacy_escaped_value(&v, quoted)),
        other => other,
    };
    let real = finish(resolve_real_value(state, scope, value.as_deref(), true).await?);
    let key = row
        .get("key")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let is_required = flag(&row, "is_required");

    let mut m = ENVIRONMENT_VARIABLE.present(row);
    for k in ["id", "resourceable_id", "resourceable_type"] {
        m.shift_remove(k);
    }
    if loaded == Loaded::Fresh {
        for k in ["is_required", "order", "preserve_compose_null"] {
            m.shift_remove(k);
        }
    }
    // `is_shown_once` has no cast: the saved model serializes it as it was sent. `is_literal`
    // has had the `boolean` cast since v4.5.3, so it reads back as the stored boolean.
    if let Some(input) = request
        && let Some(v) = input.get("is_shown_once")
    {
        m.insert("is_shown_once".into(), v.clone());
    }
    m.insert(
        "is_shared".into(),
        Value::Bool(is_shared_reference(value.as_deref())),
    );
    m.insert(
        "is_really_required".into(),
        Value::Bool(is_required && real.as_deref().is_none_or(str::is_empty)),
    );
    m.insert(
        "is_buildpack_control".into(),
        Value::Bool(key.starts_with("NIXPACKS_") || key.starts_with("RAILPACK_")),
    );
    m.insert(
        "is_coolify".into(),
        Value::Bool(key.starts_with("SERVICE_")),
    );
    let shown_once = m.get("is_shown_once").is_some_and(php_truthy);
    if ctx.can_read_sensitive && !shown_once {
        // `apiEnvironmentVariableDisplayValue`: locked shared variables keep their reference.
        let display = finish(resolve_real_value(state, scope, value.as_deref(), false).await?);
        m.insert("value".into(), json!(value));
        m.insert("real_value".into(), json!(display));
    }
    Ok(Value::Object(serialize_record(m)))
}

/// `$resource->environment_variables->sortBy('id')->merge($resource->environment_variables_preview->sortBy('id'))`, presented.
pub async fn list_variables(
    state: &AppState,
    ctx: &AuthCtx,
    scope: &ResourceScope,
) -> Result<Value, ApiError> {
    let mut items = Vec::new();
    for is_preview in [false, true] {
        for row in store::rows_for_resource(&state.db, scope.morph, scope.id, is_preview).await? {
            items.push(present_variable(state, ctx, scope, row, Loaded::Row, None).await?);
        }
    }
    Ok(Value::Array(items))
}

/// The rules every controller gives `customApiValidator`.
pub fn variable_rules() -> Vec<Field> {
    let boolean = |name| Field {
        name,
        rules: vec![Rule::Boolean],
    };
    vec![
        Field {
            name: "key",
            rules: vec![
                Rule::Required,
                Rule::String,
                Rule::Max(255),
                Rule::Regex(LazyLock::force(&KEY_PATTERN)),
            ],
        },
        Field {
            name: "value",
            rules: vec![Rule::String, Rule::Nullable],
        },
        boolean("is_preview"),
        boolean("is_literal"),
        boolean("is_multiline"),
        boolean("is_shown_once"),
        boolean("is_runtime"),
        boolean("is_buildtime"),
        Field {
            name: "comment",
            rules: vec![Rule::String, Rule::Nullable, Rule::Max(256)],
        },
    ]
}

/// `$request->merge(['key' => normalizeEnvironmentVariableKey((string) $request->key)])`.
fn normalize_key(input: &mut Input) -> Result<(), ApiError> {
    let Some(v) = input.0.get_mut("key") else {
        return Ok(());
    };
    let text = match v {
        Value::String(s) => s.trim().to_owned(),
        Value::Null | Value::Bool(false) => String::new(),
        Value::Bool(true) => "1".to_owned(),
        Value::Number(n) => n.to_string(),
        // "Array to string conversion".
        _ => return Err(ApiError::Internal),
    };
    *v = if text.is_empty() {
        Value::Null
    } else {
        Value::String(text)
    };
    Ok(())
}

/// The attributes `create()` gets, with both encryptions (the variable and its preview copy).
fn new_variable(
    state: &AppState,
    input: &Input,
) -> Result<(store::NewResourceVariable, Option<String>), ApiError> {
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let value = input.str("value").map(php_trim);
    let truthy = |k: &str, default: bool| input.get(k).map_or(default, php_truthy);
    Ok((
        store::NewResourceVariable {
            key: input.str("key").unwrap_or_default().to_owned(),
            value_encrypted: value.map(|v| enc.encrypt_serialized(v)),
            is_preview: truthy("is_preview", false),
            is_literal: truthy("is_literal", false),
            is_multiline: truthy("is_multiline", false),
            is_shown_once: truthy("is_shown_once", false),
            is_runtime: truthy("is_runtime", true),
            is_buildtime: truthy("is_buildtime", true),
            comment: input.str("comment").map(str::to_owned),
            is_shared: is_shared_reference(value),
        },
        value.map(|v| enc.encrypt_serialized(v)),
    ))
}

/// `create_env` after the authorization: the new variable's uuid.
pub async fn create_variable(
    state: &AppState,
    scope: &ResourceScope,
    mut input: Input,
) -> Result<String, ApiError> {
    normalize_key(&mut input)?;
    validate(&input, &variable_rules(), CUSTOM_API_MESSAGES, ALLOWED)?;
    let is_preview = input.get("is_preview").is_some_and(php_truthy);
    let key = input.str("key").unwrap_or_default();
    if store::first_by_key(&state.db, scope.morph, scope.id, key, is_preview)
        .await?
        .is_some()
    {
        return Err(ApiError::Message(
            StatusCode::CONFLICT,
            "Environment variable already exists. Use PATCH request to update it.",
        ));
    }
    let (v, copy) = new_variable(state, &input)?;
    let row = store::create_on_resource(
        &state.db,
        (scope.morph, scope.id),
        &v,
        copy.as_deref(),
        &state.version,
        state.now(),
    )
    .await?;
    Ok(row
        .get("uuid")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned())
}

/// `if ($request->has('value')) $env->value = ...; $env->fill(...); $env->save()` with Eloquent's
/// dirty check and the model's `saving` hook. A request without a value keeps the stored one.
async fn save_variable(state: &AppState, row: &Record, input: &Input) -> Result<Record, ApiError> {
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let id = row.get("id").and_then(Value::as_i64).unwrap_or_default();
    let old = plain_value(state, row)?;
    let new = if input.has("value") {
        input.str("value").map(php_trim)
    } else {
        old.as_deref()
    };
    let mut dirty: Vec<(&'static str, ColumnValue)> = Vec::new();
    let value_dirty = old.as_deref() != new;
    if value_dirty {
        dirty.push((
            "value",
            ColumnValue::Text(new.map(|v| enc.encrypt_serialized(v))),
        ));
        if new.is_some() && flag(row, "preserve_compose_null") {
            dirty.push(("preserve_compose_null", ColumnValue::Bool(false)));
        }
    }
    // Cast to boolean: compared after the cast (`is_literal` since v4.5.3).
    for k in ["is_multiline", "is_literal", "is_runtime", "is_buildtime"] {
        if let Some(v) = input.get(k)
            && php_truthy(v) != flag(row, k)
        {
            dirty.push((k, ColumnValue::Bool(php_truthy(v))));
        }
    }
    // No cast: only an identical boolean is clean.
    if let Some(v) = input.get("is_shown_once")
        && v.as_bool() != Some(flag(row, "is_shown_once"))
    {
        dirty.push(("is_shown_once", ColumnValue::Bool(php_truthy(v))));
    }
    if input.has("comment") {
        let comment = input.str("comment");
        if comment != row.get("comment").and_then(Value::as_str) {
            dirty.push(("comment", ColumnValue::Text(comment.map(str::to_owned))));
        }
    }
    let shared = is_shared_reference(new);
    if shared != flag(row, "is_shared") {
        dirty.push(("is_shared", ColumnValue::Bool(shared)));
    }
    Ok(store::save_dirty(&state.db, id, &dirty, state.now()).await?)
}

/// `update_env_by_uuid` after the authorization.
pub async fn update_variable(
    state: &AppState,
    ctx: &AuthCtx,
    scope: &ResourceScope,
    mut input: Input,
) -> Result<Value, ApiError> {
    normalize_key(&mut input)?;
    validate(&input, &variable_rules(), CUSTOM_API_MESSAGES, ALLOWED)?;
    let is_preview = input.get("is_preview").is_some_and(php_truthy);
    let key = input.str("key").unwrap_or_default();
    let row = store::first_by_key(&state.db, scope.morph, scope.id, key, is_preview)
        .await?
        .ok_or(ApiError::Message(
            StatusCode::NOT_FOUND,
            "Environment variable not found.",
        ))?;
    let saved = save_variable(state, &row, &input).await?;
    present_variable(state, ctx, scope, saved, Loaded::Row, Some(&input)).await
}

/// `create_bulk_envs` after the authorization. Items before a failing one stay saved.
pub async fn bulk_upsert_variables(
    state: &AppState,
    ctx: &AuthCtx,
    scope: &ResourceScope,
    input: &Input,
) -> Result<Value, ApiError> {
    let data = input
        .get("data")
        .filter(|d| php_truthy(d))
        .ok_or(ApiError::Message(
            StatusCode::BAD_REQUEST,
            "Bulk data is required.",
        ))?;
    let items: Vec<&Value> = match data {
        Value::Array(a) => a.iter().collect(),
        Value::Object(o) => o.values().collect(),
        other => vec![other],
    };
    // `collect($item)->only([...])`: everything else in an item is dropped first.
    let mut normalized = Vec::with_capacity(items.len());
    for item in items {
        let mut m = Map::new();
        if let Value::Object(o) = item {
            for (k, v) in o {
                if ALLOWED.contains(&k.as_str()) {
                    m.insert(k.clone(), v.clone());
                }
            }
        }
        let mut one = Input(m);
        normalize_key(&mut one)?;
        normalized.push(one);
    }
    let mut out = Vec::with_capacity(normalized.len());
    for item in &normalized {
        validate(item, &variable_rules(), CUSTOM_API_MESSAGES, ALLOWED)?;
        let is_preview = item.get("is_preview").is_some_and(php_truthy);
        let key = item.str("key").unwrap_or_default();
        let found = store::first_by_key(&state.db, scope.morph, scope.id, key, is_preview).await?;
        let presented = match found {
            Some(row) => {
                let saved = save_variable(state, &row, item).await?;
                present_variable(state, ctx, scope, saved, Loaded::Row, Some(item)).await?
            }
            None => {
                let (v, copy) = new_variable(state, item)?;
                let row = store::create_on_resource(
                    &state.db,
                    (scope.morph, scope.id),
                    &v,
                    copy.as_deref(),
                    &state.version,
                    state.now(),
                )
                .await?;
                present_variable(state, ctx, scope, row, Loaded::Fresh, Some(item)).await?
            }
        };
        out.push(presented);
    }
    Ok(Value::Array(out))
}

/// `delete_env_by_uuid` after the authorization: the deleted key.
pub async fn delete_variable(
    state: &AppState,
    scope: &ResourceScope,
    env_uuid: &str,
) -> Result<String, ApiError> {
    let (id, key) = store::id_by_uuid(&state.db, scope.morph, scope.id, env_uuid)
        .await?
        .ok_or(ApiError::Message(
            StatusCode::NOT_FOUND,
            "Environment variable not found.",
        ))?;
    store::force_delete(&state.db, id).await?;
    Ok(key)
}

// ---------------------------------------------------------------------------
// Application wrappers

const APP_NOT_FOUND: ApiError = ApiError::Message(StatusCode::NOT_FOUND, "Application not found");
const APP_NOT_FOUND_PERIOD: ApiError =
    ApiError::Message(StatusCode::NOT_FOUND, "Application not found.");

/// `GET /applications/{uuid}/envs`.
pub async fn application_list(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let input = lenient_json(&headers, &uri, &body);
    let uuid = magic_uuid(&uuid, &input).ok_or(APP_NOT_FOUND)?;
    let a = apps::find_in_team(&state.db, ctx.team_id, &uuid)
        .await?
        .ok_or(APP_NOT_FOUND)?;
    let list = list_variables(&state, &ctx, &ResourceScope::application(&a)).await?;
    Ok(ok(list).into_response())
}

/// `POST /applications/{uuid}/envs`.
pub async fn application_create(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let a = apps::find_in_team(&state.db, ctx.team_id, &uuid)
        .await?
        .ok_or(APP_NOT_FOUND)?;
    if !ctx.can_app(AccessLevel::Write, &a) {
        return Err(ApiError::Unauthorized);
    }
    let input = lenient_json(&headers, &uri, &body);
    let scope = ResourceScope::application(&a);
    let env_uuid = create_variable(&state, &scope, input).await?;
    tracing::info!(target: "audit", event = "api.application.env_created", team_id = ctx.team_id, application_uuid = %a.uuid, env_uuid = %env_uuid, "environment variable created");
    Ok(created(json!({"uuid": env_uuid})).into_response())
}

/// `PATCH /applications/{uuid}/envs`.
pub async fn application_update(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let input = validated_json(&headers, &uri, &body)?;
    let a = apps::find_in_team(&state.db, ctx.team_id, &uuid)
        .await?
        .ok_or(APP_NOT_FOUND)?;
    if !ctx.can_app(AccessLevel::Write, &a) {
        return Err(ApiError::Unauthorized);
    }
    let scope = ResourceScope::application(&a);
    let presented = update_variable(&state, &ctx, &scope, input).await?;
    tracing::info!(target: "audit", event = "api.application.env_updated", team_id = ctx.team_id, application_uuid = %a.uuid, "environment variable updated");
    Ok(created(presented).into_response())
}

/// `PATCH /applications/{uuid}/envs/bulk`.
pub async fn application_bulk(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let input = validated_json(&headers, &uri, &body)?;
    let a = apps::find_in_team(&state.db, ctx.team_id, &uuid)
        .await?
        .ok_or(APP_NOT_FOUND)?;
    if !ctx.can_app(AccessLevel::Write, &a) {
        return Err(ApiError::Unauthorized);
    }
    let scope = ResourceScope::application(&a);
    let presented = bulk_upsert_variables(&state, &ctx, &scope, &input).await?;
    tracing::info!(target: "audit", event = "api.application.env_bulk_upserted", team_id = ctx.team_id, application_uuid = %a.uuid, "environment variables upserted");
    Ok(created(presented).into_response())
}

/// `DELETE /applications/{uuid}/envs/{env_uuid}`.
pub async fn application_delete(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((uuid, env_uuid)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let a = apps::find_in_team(&state.db, ctx.team_id, &uuid)
        .await?
        .ok_or(APP_NOT_FOUND_PERIOD)?;
    if !ctx.can_app(AccessLevel::Write, &a) {
        return Err(ApiError::Unauthorized);
    }
    let scope = ResourceScope::application(&a);
    let key = delete_variable(&state, &scope, &env_uuid).await?;
    tracing::info!(target: "audit", event = "api.application.env_deleted", team_id = ctx.team_id, application_uuid = %a.uuid, env_uuid = %env_uuid, env_key = %key, "environment variable deleted");
    Ok(ok(json!({"message": "Environment variable deleted."})).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_references() {
        assert!(is_shared_reference(Some("{{team.A}}")));
        assert!(is_shared_reference(Some(" {{ server.A.B }} ")));
        assert!(!is_shared_reference(Some("x{{team.A}}")));
        assert!(!is_shared_reference(Some("{{other.A}}")));
        assert!(!is_shared_reference(None));
    }

    #[test]
    fn legacy_escaping() {
        assert_eq!(legacy_escaped_value(r#"{"a":1}"#, true), r#"{"a":1}"#);
        assert_eq!(legacy_escaped_value("a b", true), "'a b'");
        assert_eq!(legacy_escaped_value("a'b", false), r"a\'b");
    }
}
