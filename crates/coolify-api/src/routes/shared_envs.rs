//! `SharedEnvironmentVariablesController`: team, project and environment scopes.
//! The server scope is in `server_proxy.rs`.

use axum::Extension;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::IntoResponse;
use coolify_db::records::{Record, laravel_datetime};
use coolify_db::servers::ColumnValue as C;
use coolify_db::shared_variables::{self as sv, NewShared, Scope};
use regex::Regex;
use serde_json::{Value, json};
use std::sync::LazyLock;

use crate::audit::Meta;
use crate::auth::AuthCtx;
use crate::input::{Input, validated_json};
use crate::response::{ApiError, ApiResult, created, ok};
use crate::routes::servers::input_bool;
use crate::serialize::{php_truthy, serialize_record};
use crate::state::AppState;
use crate::validation::{CUSTOM_API_MESSAGES, Field, Rule, validate};

const ENV_NOT_FOUND: ApiError =
    ApiError::Message(StatusCode::NOT_FOUND, "Environment variable not found.");
/// Comparing `id` with a non-numeric route parameter fails in PostgreSQL.
const DB_ERROR: ApiError = ApiError::Abort(StatusCode::INTERNAL_SERVER_ERROR, "Server Error");
const ALLOWED: &[&str] = &[
    "key",
    "value",
    "is_literal",
    "is_multiline",
    "is_shown_once",
    "comment",
];

static ENV_KEY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\A[A-Za-z_][A-Za-z0-9_.]*\z").unwrap_or_else(|_| unreachable!()));

/// `removeSensitiveData` + `serializeApiResponse`.
pub(crate) fn present(state: &AppState, ctx: &AuthCtx, mut r: Record) -> Result<Value, ApiError> {
    for k in [
        "team_id",
        "project_id",
        "environment_id",
        "server_id",
        "version",
    ] {
        r.shift_remove(k);
    }
    let shown_once = r.get("is_shown_once").is_some_and(php_truthy);
    let cipher = r.shift_remove("value");
    if ctx.can_read_sensitive && !shown_once {
        let plain = cipher
            .as_ref()
            .and_then(Value::as_str)
            .map(|c| {
                state
                    .encrypter
                    .as_ref()
                    .ok_or(ApiError::Internal)
                    .and_then(|e| e.decrypt_string(c).map_err(|_| ApiError::Internal))
            })
            .transpose()?;
        r.insert("value".into(), plain.map_or(Value::Null, Value::String));
    }
    for k in ["created_at", "updated_at"] {
        if let Some(Value::String(t)) = r.get(k) {
            let v = laravel_datetime(t);
            r.insert(k.into(), Value::String(v));
        }
    }
    Ok(Value::Object(serialize_record(r)))
}

fn fields(require_key: bool) -> Vec<Field> {
    let mut key = vec![if require_key {
        Rule::Required
    } else {
        Rule::Nullable
    }];
    key.extend([Rule::String, Rule::Max(255), Rule::Regex(&ENV_KEY)]);
    vec![
        Field {
            name: "key",
            rules: key,
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

async fn list(state: &AppState, ctx: &AuthCtx, scope: Scope) -> ApiResult {
    let rows = sv::list(&state.db, ctx.team_id, scope).await?;
    let items = rows
        .into_iter()
        .map(|r| present(state, ctx, r))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ok(Value::Array(items)).into_response())
}

async fn create_env(
    state: &AppState,
    ctx: &AuthCtx,
    meta: &Meta,
    scope: Scope,
    input: &Input,
) -> ApiResult {
    validate(input, &fields(true), CUSTOM_API_MESSAGES, ALLOWED)?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let key = input.str("key").unwrap_or_default().to_string();
    if sv::key_exists(&state.db, ctx.team_id, scope, &key, None).await? {
        return Err(ApiError::Message(
            StatusCode::CONFLICT,
            "Environment variable already exists. Use PATCH request to update it.",
        ));
    }
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    // `value` is NOT NULL: a request without one fails at the database, as upstream.
    let Some(value) = input.str("value") else {
        return Err(ApiError::Internal);
    };
    let cipher = enc.encrypt_string(value);
    let n = NewShared {
        team_id: ctx.team_id,
        key: &key,
        value_encrypted: Some(&cipher),
        is_literal: input_bool(input, "is_literal"),
        is_multiline: input_bool(input, "is_multiline"),
        is_shown_once: input_bool(input, "is_shown_once"),
        comment: input.str("comment"),
    };
    let id = sv::create(&state.db, scope, &n, state.now()).await?;
    let (p, e, s) = scope.columns();
    let changes = json!({
        "key": {"old": null, "new": key},
        "is_literal": {"old": null, "new": n.is_literal},
        "is_multiline": {"old": null, "new": n.is_multiline},
        "is_shown_once": {"old": null, "new": n.is_shown_once},
        "comment": {"old": null, "new": n.comment},
        "type": {"old": null, "new": scope.kind()},
    });
    let _ = (p, e, s);
    crate::audit::mutation(
        state,
        ctx,
        meta,
        "shared_environment_variable",
        "created",
        json!({
            "team_id": ctx.team_id,
            "shared_environment_variable_uuid": null,
            "shared_environment_variable_name": key,
            "changed_fields": [],
        }),
        changes,
    )
    .await;
    crate::audit::log(
        state,
        ctx,
        meta,
        "api.shared_env.created",
        json!({"team_id": ctx.team_id, "env_id": id, "env_key": key, "type": scope.kind()}),
        "info",
    )
    .await;
    Ok(created(json!({"id": id})).into_response())
}

#[allow(clippy::too_many_arguments)]
async fn update_env(
    state: &AppState,
    ctx: &AuthCtx,
    meta: &Meta,
    scope: Scope,
    env_id: &str,
    headers: &HeaderMap,
    uri: &Uri,
    body: &[u8],
) -> ApiResult {
    let id = env_id.parse::<i64>().map_err(|_| DB_ERROR)?;
    let current = sv::find(&state.db, ctx.team_id, scope, id)
        .await?
        .ok_or(ENV_NOT_FOUND)?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let input = validated_json(headers, uri, body)?;
    validate(&input, &fields(false), CUSTOM_API_MESSAGES, ALLOWED)?;
    let mut changes: Vec<(&'static str, C)> = Vec::new();
    let mut audit_changes = serde_json::Map::new();
    let mut changed: Vec<&'static str> = Vec::new();
    let mut final_key = current
        .get("key")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if input.has("key") {
        let key = input.str("key").unwrap_or_default().to_string();
        if Some(key.as_str()) != current.get("key").and_then(Value::as_str) {
            if sv::key_exists(&state.db, ctx.team_id, scope, &key, Some(id)).await? {
                return Err(ApiError::Message(
                    StatusCode::CONFLICT,
                    "Environment variable already exists with this key.",
                ));
            }
            audit_changes.insert("key".into(), json!({"old": final_key, "new": key}));
            changed.push("key");
            final_key.clone_from(&key);
            changes.push(("key", C::Text(Some(key))));
        }
    }
    if input.has("value") {
        let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
        let new = input.str("value");
        let old = current
            .get("value")
            .and_then(Value::as_str)
            .and_then(|c| enc.decrypt_string(c).ok());
        if new != old.as_deref() {
            let Some(new) = new else {
                return Err(ApiError::Internal);
            };
            changed.push("value");
            changes.push(("value", C::Text(Some(enc.encrypt_string(new)))));
        }
    }
    for k in ["is_literal", "is_multiline", "is_shown_once"] {
        if input.has(k) {
            let v = input_bool(&input, k);
            if current.get(k).and_then(Value::as_bool) != Some(v) {
                audit_changes.insert(k.into(), json!({"old": current.get(k), "new": v}));
                changed.push(k);
                changes.push((k, C::Bool(v)));
            }
        }
    }
    if input.has("comment") {
        let v = input.str("comment").map(str::to_string);
        if current.get("comment").and_then(Value::as_str) != v.as_deref() {
            audit_changes.insert(
                "comment".into(),
                json!({"old": current.get("comment"), "new": v}),
            );
            changed.push("comment");
            changes.push(("comment", C::Text(v)));
        }
    }
    coolify_db::server_ops::update_shared_env(&state.db, id, &changes, state.now()).await?;
    if !changed.is_empty() {
        crate::audit::mutation(
            state,
            ctx,
            meta,
            "shared_environment_variable",
            "updated",
            json!({
                "team_id": ctx.team_id,
                "shared_environment_variable_uuid": null,
                "shared_environment_variable_name": final_key,
                "changed_fields": changed,
            }),
            Value::Object(audit_changes),
        )
        .await;
    }
    crate::audit::log(
        state,
        ctx,
        meta,
        "api.shared_env.updated",
        json!({"team_id": ctx.team_id, "env_id": id, "env_key": final_key, "type": scope.kind()}),
        "info",
    )
    .await;
    let fresh = sv::find(&state.db, ctx.team_id, scope, id)
        .await?
        .ok_or(ENV_NOT_FOUND)?;
    Ok(ok(present(state, ctx, fresh)?).into_response())
}

async fn delete_env(
    state: &AppState,
    ctx: &AuthCtx,
    meta: &Meta,
    scope: Scope,
    env_id: &str,
) -> ApiResult {
    let id = env_id.parse::<i64>().map_err(|_| DB_ERROR)?;
    let current = sv::find(&state.db, ctx.team_id, scope, id)
        .await?
        .ok_or(ENV_NOT_FOUND)?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    coolify_db::server_ops::delete_shared_env(&state.db, id).await?;
    let key = current.get("key").cloned().unwrap_or(Value::Null);
    crate::audit::mutation(
        state,
        ctx,
        meta,
        "shared_environment_variable",
        "deleted",
        json!({
            "team_id": ctx.team_id,
            "shared_environment_variable_uuid": null,
            "shared_environment_variable_name": key,
            "changed_fields": [],
        }),
        json!({
            "key": {"old": key, "new": null},
            "is_literal": {"old": current.get("is_literal"), "new": null},
            "is_multiline": {"old": current.get("is_multiline"), "new": null},
            "is_shown_once": {"old": current.get("is_shown_once"), "new": null},
            "comment": {"old": current.get("comment"), "new": null},
            "type": {"old": current.get("type"), "new": null},
        }),
    )
    .await;
    crate::audit::log(
        state,
        ctx,
        meta,
        "api.shared_env.deleted",
        json!({"team_id": ctx.team_id, "env_id": id, "env_key": key, "type": scope.kind()}),
        "info",
    )
    .await;
    Ok(ok(json!({"message": "Environment variable deleted."})).into_response())
}

const PROJECT_NOT_FOUND: ApiError = ApiError::Message(StatusCode::NOT_FOUND, "Project not found.");
const ENV_SCOPE_NOT_FOUND: ApiError =
    ApiError::Message(StatusCode::NOT_FOUND, "Environment not found.");

async fn project_scope(state: &AppState, ctx: &AuthCtx, uuid: &str) -> Result<Scope, ApiError> {
    let p = coolify_db::projects::find(&state.db, ctx.team_id, uuid)
        .await?
        .ok_or(PROJECT_NOT_FOUND)?;
    Ok(Scope::Project(p.id))
}

async fn environment_scope(
    state: &AppState,
    ctx: &AuthCtx,
    uuid: &str,
    env: &str,
) -> Result<Scope, ApiError> {
    let Scope::Project(project_id) = project_scope(state, ctx, uuid).await? else {
        return Err(PROJECT_NOT_FOUND);
    };
    let id: Option<i64> = sqlx::query_scalar(
        "SELECT id FROM environments WHERE project_id = $1 AND (name = $2 OR uuid = $2) \
         ORDER BY (name = $2) DESC LIMIT 1",
    )
    .bind(project_id)
    .bind(env)
    .fetch_optional(state.db.pool())
    .await
    .map_err(coolify_db::DbError::from)?;
    Ok(Scope::Environment(id.ok_or(ENV_SCOPE_NOT_FOUND)?))
}

pub async fn team_list(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
) -> ApiResult {
    list(&state, &ctx, Scope::Team).await
}

pub async fn team_create(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let input = validated_json(&headers, &uri, &body)?;
    create_env(&state, &ctx, &meta, Scope::Team, &input).await
}

pub async fn team_update(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path(env_id): Path<String>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    update_env(
        &state,
        &ctx,
        &meta,
        Scope::Team,
        &env_id,
        &headers,
        &uri,
        &body,
    )
    .await
}

pub async fn team_delete(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path(env_id): Path<String>,
) -> ApiResult {
    delete_env(&state, &ctx, &meta, Scope::Team, &env_id).await
}

pub async fn project_list(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let s = project_scope(&state, &ctx, &uuid).await?;
    list(&state, &ctx, s).await
}

pub async fn project_create(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path(uuid): Path<String>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let s = project_scope(&state, &ctx, &uuid).await?;
    let input = validated_json(&headers, &uri, &body)?;
    create_env(&state, &ctx, &meta, s, &input).await
}

pub async fn project_update(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path((uuid, env_id)): Path<(String, String)>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let s = project_scope(&state, &ctx, &uuid).await?;
    update_env(&state, &ctx, &meta, s, &env_id, &headers, &uri, &body).await
}

pub async fn project_delete(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path((uuid, env_id)): Path<(String, String)>,
) -> ApiResult {
    let s = project_scope(&state, &ctx, &uuid).await?;
    delete_env(&state, &ctx, &meta, s, &env_id).await
}

pub async fn environment_list(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((uuid, env)): Path<(String, String)>,
) -> ApiResult {
    let s = environment_scope(&state, &ctx, &uuid, &env).await?;
    list(&state, &ctx, s).await
}

pub async fn environment_create(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path((uuid, env)): Path<(String, String)>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let s = environment_scope(&state, &ctx, &uuid, &env).await?;
    let input = validated_json(&headers, &uri, &body)?;
    create_env(&state, &ctx, &meta, s, &input).await
}

pub async fn environment_update(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path((uuid, env, env_id)): Path<(String, String, String)>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let s = environment_scope(&state, &ctx, &uuid, &env).await?;
    update_env(&state, &ctx, &meta, s, &env_id, &headers, &uri, &body).await
}

pub async fn environment_delete(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path((uuid, env, env_id)): Path<(String, String, String)>,
) -> ApiResult {
    let s = environment_scope(&state, &ctx, &uuid, &env).await?;
    delete_env(&state, &ctx, &meta, s, &env_id).await
}
