//! `DatabasesController`: environment variables of a database (`envs`,
//! `create_env`, `update_env_by_uuid`, `create_bulk_envs`, `delete_env_by_uuid`).
//! Application variables live in their own module until they are unified.

use std::sync::LazyLock;

use axum::Extension;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::IntoResponse;
use coolify_db::database_parts as parts;
use coolify_db::databases::{self as dbs, DatabaseRow};
use coolify_db::records::Record;
use coolify_db::servers::ColumnValue;
use coolify_jobs::databases::variables as jobvars;
use regex::Regex;
use serde_json::{Map, Value, json};

use crate::auth::AuthCtx;
use crate::input::{Input, lenient_json};
use crate::response::{ApiError, ApiResult, created, ok};
use crate::routes::databases::NOT_FOUND;
use crate::serialize::serialize_record;
use crate::state::AppState;
use crate::validation::{CUSTOM_API_MESSAGES, Field, Rule, validate};

/// `ValidationPatterns::ENVIRONMENT_VARIABLE_KEY_PATTERN`.
static KEY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\A[A-Za-z_][A-Za-z0-9_.]*\z").unwrap_or_else(|_| unreachable!()));
const ENV_NOT_FOUND: ApiError =
    ApiError::Message(StatusCode::NOT_FOUND, "Environment variable not found.");

fn rules() -> Vec<Field> {
    vec![
        Field {
            name: "key",
            rules: vec![
                Rule::Required,
                Rule::String,
                Rule::Max(255),
                Rule::Regex(&KEY),
            ],
        },
        Field {
            name: "value",
            rules: vec![Rule::String, Rule::Nullable],
        },
        Field {
            name: "is_literal",
            rules: vec![Rule::Boolean],
        },
        Field {
            name: "is_multiline",
            rules: vec![Rule::Boolean],
        },
        Field {
            name: "is_shown_once",
            rules: vec![Rule::Boolean],
        },
        Field {
            name: "comment",
            rules: vec![Rule::String, Rule::Nullable, Rule::Max(256)],
        },
    ]
}

fn no_extra_check(input: &Input) -> Vec<&str> {
    input.keys().map(String::as_str).collect()
}

/// `ValidationPatterns::normalizeEnvironmentVariableKey((string) $key)`.
fn normalize_key(input: &mut Input) {
    if let Some(v) = input.get("key").cloned() {
        let text = match v {
            Value::String(s) => s,
            Value::Null => String::new(),
            Value::Bool(true) => "1".into(),
            Value::Bool(false) => String::new(),
            other => other.to_string(),
        };
        input
            .0
            .insert("key".into(), Value::String(text.trim().to_owned()));
    }
}

/// `str($key)->trim()->replace(' ', '_')`.
fn stored_key(input: &Input) -> String {
    input
        .str("key")
        .unwrap_or_default()
        .trim()
        .replace(' ', "_")
}

/// A Laravel boolean of a validated field.
fn flag(v: Option<&Value>) -> Option<bool> {
    match v? {
        Value::Bool(b) => Some(*b),
        Value::Number(n) => Some(n.as_i64() == Some(1)),
        Value::String(s) => Some(s == "1"),
        _ => None,
    }
}

/// `isSharedReference` for the saving hook: `{{team.KEY}}`-style values.
fn is_shared_reference(value: Option<&str>) -> bool {
    static SHARED: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?s)^\{\{\s*(?:team|project|environment|server)\..*\}\}$")
            .unwrap_or_else(|_| unreachable!())
    });
    value
        .map(str::trim)
        .is_some_and(|v| !v.is_empty() && SHARED.is_match(v))
}

async fn find(state: &AppState, ctx: &AuthCtx, uuid: &str) -> Result<DatabaseRow, ApiError> {
    dbs::find_in_team(&state.db, ctx.team_id, uuid)
        .await?
        .ok_or(NOT_FOUND)
}

/// `removeSensitiveEnvData($env)` of a stored row (or a fresh model's attributes).
async fn present(
    state: &AppState,
    ctx: &AuthCtx,
    d: &DatabaseRow,
    row: &Record,
) -> Result<Value, ApiError> {
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let mut r = row.clone();
    for hidden in [
        "id",
        "resourceable",
        "resourceable_id",
        "resourceable_type",
        "uses_legacy_escaping",
    ] {
        r.shift_remove(hidden);
    }
    let cipher = r
        .shift_remove("value")
        .and_then(|v| v.as_str().map(str::to_owned));
    let plain = match cipher.as_deref().filter(|c| !c.is_empty()) {
        Some(c) => Some(
            enc.decrypt_maybe_serialized(c)
                .map_err(|_| ApiError::Internal)?
                .trim()
                .to_owned(),
        ),
        None => None,
    };
    let key = r
        .get("key")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let shown_once = r
        .get("is_shown_once")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let server_id = d.server_id();
    if ctx.can_read_sensitive && !shown_once {
        let display = jobvars::display_real_value(
            &state.db,
            enc,
            d,
            server_id,
            plain.as_deref(),
            row.get("uses_legacy_escaping")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            row.get("is_literal")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            row.get("is_multiline")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        )
        .await
        .map_err(|_| ApiError::Internal)?;
        if row.contains_key("value") {
            r.insert(
                "value".into(),
                plain.clone().map_or(Value::Null, Value::String),
            );
        }
        r.insert(
            "real_value".into(),
            display.map_or(Value::Null, Value::String),
        );
    }
    // The appended accessors.
    let real = jobvars::real_value(&state.db, enc, d, server_id, plain.as_deref())
        .await
        .map_err(|_| ApiError::Internal)?;
    let required = r
        .get("is_required")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    r.insert(
        "is_shared".into(),
        Value::Bool(is_shared_reference(plain.as_deref())),
    );
    r.insert(
        "is_really_required".into(),
        Value::Bool(required && real.as_deref().unwrap_or_default().is_empty()),
    );
    r.insert(
        "is_buildpack_control".into(),
        Value::Bool(key.starts_with("NIXPACKS_") || key.starts_with("RAILPACK_")),
    );
    r.insert(
        "is_coolify".into(),
        Value::Bool(key.starts_with("SERVICE_")),
    );
    for k in ["created_at", "updated_at"] {
        if let Some(Value::String(s)) = r.get(k) {
            let v = coolify_db::records::laravel_datetime(s);
            r.insert(k.into(), Value::String(v));
        }
    }
    Ok(Value::Object(serialize_record(r)))
}

/// The attributes of a model that was just created: the columns the insert set, not the
/// defaults of the table (the model is not refreshed).
fn created_model(row: &Record) -> Record {
    let mut out = Map::new();
    for k in [
        "key",
        "value",
        "is_literal",
        "is_multiline",
        "is_shown_once",
        "comment",
        "resourceable_type",
        "resourceable_id",
        "is_runtime",
        "is_buildtime",
        "uuid",
        "updated_at",
        "created_at",
        "id",
        "version",
    ] {
        if let Some(v) = row.get(k) {
            out.insert(k.into(), v.clone());
        }
    }
    out
}

/// `GET /databases/{uuid}/envs`.
pub async fn list(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let d = find(&state, &ctx, &uuid).await?;
    let rows =
        coolify_db::environment_variables::for_resource(&state.db, d.kind.morph(), d.id).await?;
    let mut out = Vec::with_capacity(rows.len());
    for v in rows {
        out.push(present(&state, &ctx, &d, &v.record).await?);
    }
    Ok(ok(Value::Array(out)).into_response())
}

/// `POST /databases/{uuid}/envs` (`create_env`).
pub async fn create(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let d = find(&state, &ctx, &uuid).await?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let mut input = lenient_json(&headers, &uri, &body);
    normalize_key(&mut input);
    validate(
        &input,
        &rules(),
        CUSTOM_API_MESSAGES,
        &no_extra_check(&input),
    )?;
    let key = stored_key(&input);
    if parts::variable_by_key(&state.db, d.kind.morph(), d.id, &key)
        .await?
        .is_some()
    {
        return Err(ApiError::Message(
            StatusCode::CONFLICT,
            "Environment variable already exists. Use PATCH request to update it.",
        ));
    }
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let value = input.str("value").map(|v| v.trim().to_owned());
    let row = parts::create_variable(
        &state.db,
        d.kind.morph(),
        d.id,
        &key,
        value
            .as_deref()
            .map(|v| enc.encrypt_serialized(v))
            .as_deref(),
        flag(input.get("is_literal")).unwrap_or(false),
        flag(input.get("is_multiline")).unwrap_or(false),
        flag(input.get("is_shown_once")).unwrap_or(false),
        input.str("comment"),
        is_shared_reference(value.as_deref()),
        &state.version,
        state.now(),
    )
    .await?;
    tracing::info!(target: "audit", event = "api.database.env_created", team_id = ctx.team_id, database_uuid = %d.uuid, env_key = %key, "database variable created");
    let shown = present(&state, &ctx, &d, &created_model(&row)).await?;
    Ok(created(shown).into_response())
}

/// `PATCH /databases/{uuid}/envs` (`update_env_by_uuid`).
pub async fn update(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let d = find(&state, &ctx, &uuid).await?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let mut input = lenient_json(&headers, &uri, &body);
    normalize_key(&mut input);
    validate(
        &input,
        &rules(),
        CUSTOM_API_MESSAGES,
        &no_extra_check(&input),
    )?;
    let key = stored_key(&input);
    let row = parts::variable_by_key(&state.db, d.kind.morph(), d.id, &key)
        .await?
        .ok_or(ENV_NOT_FOUND)?;
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let id = parts::id_of(&row);
    let value = input.str("value").map(|v| v.trim().to_owned());
    let mut values: Vec<(&'static str, ColumnValue)> = vec![
        (
            "value",
            ColumnValue::Text(value.as_deref().map(|v| enc.encrypt_serialized(v))),
        ),
        (
            "is_shared",
            ColumnValue::Bool(is_shared_reference(value.as_deref())),
        ),
    ];
    for (field, column) in [
        ("is_literal", "is_literal"),
        ("is_multiline", "is_multiline"),
        ("is_shown_once", "is_shown_once"),
    ] {
        if let Some(b) = flag(input.get(field)) {
            values.push((column, ColumnValue::Bool(b)));
        }
    }
    if input.has("comment") {
        values.push((
            "comment",
            ColumnValue::Text(input.str("comment").map(str::to_owned)),
        ));
    }
    parts::update_row(&state.db, "environment_variables", id, &values, state.now()).await?;
    tracing::info!(target: "audit", event = "api.database.env_updated", team_id = ctx.team_id, database_uuid = %d.uuid, env_key = %key, "database variable updated");
    let fresh = parts::find_row(&state.db, "environment_variables", id)
        .await?
        .ok_or(ApiError::Internal)?;
    let shown = present(&state, &ctx, &d, &fresh).await?;
    Ok(created(shown).into_response())
}

/// `PATCH /databases/{uuid}/envs/bulk` (`create_bulk_envs`): `updateOrCreate` per item.
pub async fn bulk(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let d = find(&state, &ctx, &uuid).await?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let input = lenient_json(&headers, &uri, &body);
    let data = match input.get("data") {
        Some(v) if crate::serialize::php_truthy(v) => v.clone(),
        _ => {
            return Err(ApiError::Message(
                StatusCode::BAD_REQUEST,
                "Bulk data is required.",
            ));
        }
    };
    let items: Vec<Value> = match data {
        Value::Array(a) => a,
        Value::Object(o) => o.into_iter().map(|(_, v)| v).collect(),
        _ => return Err(ApiError::Internal),
    };
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let now = state.now();
    let mut out = Vec::new();
    for item in items {
        let Value::Object(map) = item else {
            return Err(ApiError::Internal);
        };
        let mut item = Input(map);
        normalize_key(&mut item);
        validate(&item, &rules(), CUSTOM_API_MESSAGES, &no_extra_check(&item))?;
        let key = stored_key(&item);
        let value = item.str("value").map(|v| v.trim().to_owned());
        let existing = parts::variable_by_key(&state.db, d.kind.morph(), d.id, &key).await?;
        let cipher = value.as_deref().map(|v| enc.encrypt_serialized(v));
        let shared = is_shared_reference(value.as_deref());
        let row = match existing {
            Some(row) => {
                let id = parts::id_of(&row);
                let mut values: Vec<(&'static str, ColumnValue)> = Vec::new();
                if item.has("value") {
                    values.push(("value", ColumnValue::Text(cipher)));
                    values.push(("is_shared", ColumnValue::Bool(shared)));
                }
                for field in ["is_literal", "is_multiline", "is_shown_once"] {
                    if let Some(b) = flag(item.get(field)) {
                        values.push((field, ColumnValue::Bool(b)));
                    }
                }
                if item.has("comment") {
                    values.push((
                        "comment",
                        ColumnValue::Text(item.str("comment").map(str::to_owned)),
                    ));
                }
                parts::update_row(&state.db, "environment_variables", id, &values, now).await?;
                parts::find_row(&state.db, "environment_variables", id)
                    .await?
                    .ok_or(ApiError::Internal)?
            }
            None => {
                let row = parts::create_variable(
                    &state.db,
                    d.kind.morph(),
                    d.id,
                    &key,
                    cipher.as_deref(),
                    flag(item.get("is_literal")).unwrap_or(false),
                    flag(item.get("is_multiline")).unwrap_or(false),
                    flag(item.get("is_shown_once")).unwrap_or(false),
                    item.str("comment"),
                    shared,
                    &state.version,
                    now,
                )
                .await?;
                // `updateOrCreate` assigns only what the item holds.
                let mut model = created_model(&row);
                for optional in ["is_literal", "is_multiline", "is_shown_once", "comment"] {
                    if !item.has(optional) {
                        model.shift_remove(optional);
                    }
                }
                if !item.has("value") {
                    model.shift_remove("value");
                }
                model
            }
        };
        out.push(present(&state, &ctx, &d, &row).await?);
    }
    tracing::info!(target: "audit", event = "api.database.env_bulk_upserted", team_id = ctx.team_id, database_uuid = %d.uuid, env_count = out.len(), "database variables upserted");
    Ok(created(Value::Array(out)).into_response())
}

/// `DELETE /databases/{uuid}/envs/{env_uuid}` (`delete_env_by_uuid`).
pub async fn delete(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((uuid, env_uuid)): Path<(String, String)>,
) -> ApiResult {
    let d = find(&state, &ctx, &uuid).await?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let row = parts::variable_by_uuid(&state.db, d.kind.morph(), d.id, &env_uuid)
        .await?
        .ok_or(ENV_NOT_FOUND)?;
    parts::delete_variable(&state.db, parts::id_of(&row)).await?;
    tracing::info!(target: "audit", event = "api.database.env_deleted", team_id = ctx.team_id, database_uuid = %d.uuid, env_uuid = %env_uuid, "database variable deleted");
    Ok(ok(json!({"message": "Environment variable deleted."})).into_response())
}
