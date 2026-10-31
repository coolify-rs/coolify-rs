//! `CloudProviderTokensController` and `CloudInitScriptsController`.

use axum::Extension;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::IntoResponse;
use coolify_db::cloud;
use coolify_db::records::{Record, laravel_datetime};
use coolify_domain::new_public_id;
use serde_json::{Value, json};

use crate::auth::AuthCtx;
use crate::input::{Input, validated_json};
use crate::response::{ApiError, ApiResult, ok};
use crate::routes::server_settings::{authorize_update, json_status};
use crate::serialize::serialize_record;
use crate::state::AppState;
use crate::validation::{CUSTOM_API_MESSAGES, Field, Rule, validate};

const TOKEN_NOT_FOUND: ApiError =
    ApiError::Message(StatusCode::NOT_FOUND, "Cloud provider token not found.");
const SCRIPT_NOT_FOUND: ApiError =
    ApiError::Message(StatusCode::NOT_FOUND, "Cloud-init script not found.");

fn dates(r: &mut Record) {
    for k in ["created_at", "updated_at"] {
        if let Some(Value::String(t)) = r.get(k) {
            let v = laravel_datetime(t);
            r.insert(k.into(), Value::String(v));
        }
    }
}

fn decrypt(state: &AppState, r: &Record, k: &str) -> Result<Value, ApiError> {
    let Some(c) = r.get(k).and_then(Value::as_str) else {
        return Ok(Value::Null);
    };
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    enc.decrypt_string(c)
        .map(Value::String)
        .map_err(|_| ApiError::Internal)
}

/// `CloudProviderTokensController::removeSensitiveData`.
fn present_token(state: &AppState, ctx: &AuthCtx, mut r: Record) -> Result<Value, ApiError> {
    r.shift_remove("id");
    let plain = decrypt(state, &r, "token")?;
    r.shift_remove("token");
    if ctx.can_read_sensitive {
        r.insert("token".into(), plain);
    }
    dates(&mut r);
    Ok(Value::Object(serialize_record(r)))
}

pub async fn tokens_index(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
) -> ApiResult {
    let rows = cloud::list_tokens(&state.db, ctx.team_id).await?;
    let items = rows
        .into_iter()
        .map(|r| present_token(&state, &ctx, r))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ok(Value::Array(items)).into_response())
}

pub async fn tokens_show(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let r = cloud::find_token(&state.db, ctx.team_id, &uuid)
        .await?
        .ok_or(TOKEN_NOT_FOUND)?;
    authorize_update(&ctx)?;
    Ok(ok(present_token(&state, &ctx, r)?).into_response())
}

/// The JSON body only: `$request->json()->all()`, without query parameters.
fn body_only(input: Input, uri: &Uri, body: &[u8]) -> Input {
    let query = uri.query().unwrap_or_default();
    if query.is_empty() {
        return input;
    }
    let keys: Vec<String> = serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|v| v.as_object().map(|m| m.keys().cloned().collect()))
        .unwrap_or_default();
    Input(
        input
            .0
            .into_iter()
            .filter(|(k, _)| keys.contains(k))
            .collect(),
    )
}

pub async fn tokens_store(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    authorize_update(&ctx)?;
    let input = body_only(validated_json(&headers, &uri, &body)?, &uri, &body);
    validate(
        &input,
        &[
            Field {
                name: "provider",
                rules: vec![
                    Rule::Required,
                    Rule::String,
                    Rule::In(&["hetzner", "digitalocean", "vultr", "hostinger"]),
                ],
            },
            Field {
                name: "token",
                rules: vec![Rule::Required, Rule::String],
            },
            Field {
                name: "name",
                rules: vec![Rule::Required, Rule::String, Rule::Max(255)],
            },
        ],
        CUSTOM_API_MESSAGES,
        &["provider", "token", "name"],
    )?;
    let provider = input.str("provider").unwrap_or_default();
    let token = input.str("token").unwrap_or_default();
    if let Err(e) = coolify_jobs::cloud::validate_provider_token(provider, token).await {
        return Ok(json_status(StatusCode::BAD_REQUEST, json!({"message": e})));
    }
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let uuid = new_public_id();
    cloud::create_token(
        &state.db,
        ctx.team_id,
        &uuid,
        provider,
        &enc.encrypt_string(token),
        input.str("name").unwrap_or_default(),
        state.now(),
    )
    .await?;
    tracing::info!(target: "audit", event = "api.cloud_token.created", team_id = ctx.team_id, cloud_token_uuid = %uuid, "cloud token created");
    Ok(json_status(StatusCode::CREATED, json!({"uuid": uuid})))
}

pub async fn tokens_update(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let input = body_only(validated_json(&headers, &uri, &body)?, &uri, &body);
    validate(
        &input,
        &[Field {
            name: "name",
            rules: vec![Rule::Required, Rule::String, Rule::Max(255)],
        }],
        CUSTOM_API_MESSAGES,
        &["name"],
    )?;
    let r = cloud::find_token(&state.db, ctx.team_id, &uuid)
        .await?
        .ok_or(TOKEN_NOT_FOUND)?;
    authorize_update(&ctx)?;
    let id = r.get("id").and_then(Value::as_i64).unwrap_or_default();
    cloud::update_token_name(
        &state.db,
        id,
        input.str("name").unwrap_or_default(),
        state.now(),
    )
    .await?;
    Ok(ok(json!({"uuid": uuid})).into_response())
}

pub async fn tokens_destroy(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let r = cloud::find_token(&state.db, ctx.team_id, &uuid)
        .await?
        .ok_or(TOKEN_NOT_FOUND)?;
    authorize_update(&ctx)?;
    if r.get("servers_count").and_then(Value::as_i64).unwrap_or(0) > 0 {
        return Err(ApiError::Message(
            StatusCode::BAD_REQUEST,
            "Cannot delete token that is used by servers.",
        ));
    }
    let id = r.get("id").and_then(Value::as_i64).unwrap_or_default();
    cloud::delete_token(&state.db, id).await?;
    Ok(ok(json!({"message": "Cloud provider token deleted."})).into_response())
}

pub async fn tokens_validate(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let r = cloud::find_token(&state.db, ctx.team_id, &uuid)
        .await?
        .ok_or(TOKEN_NOT_FOUND)?;
    authorize_update(&ctx)?;
    let token = decrypt(&state, &r, "token")?;
    let provider = r
        .get("provider")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let result =
        coolify_jobs::cloud::validate_provider_token(provider, token.as_str().unwrap_or_default())
            .await;
    Ok(ok(match result {
        Ok(()) => json!({"valid": true, "message": "Token is valid."}),
        Err(e) => json!({"valid": false, "message": e}),
    })
    .into_response())
}

// ── Cloud-init scripts ──────────────────────────────────────────────────

/// `CloudInitScriptsController::removeSensitiveData`.
fn present_script(state: &AppState, ctx: &AuthCtx, mut r: Record) -> Result<Value, ApiError> {
    r.shift_remove("id");
    r.shift_remove("team_id");
    let plain = decrypt(state, &r, "script")?;
    r.shift_remove("script");
    if ctx.can_read_sensitive {
        r.insert("script".into(), plain);
    }
    dates(&mut r);
    Ok(Value::Object(serialize_record(r)))
}

/// `ValidCloudInitYaml`.
pub(crate) fn cloud_init_rule(v: &Value) -> Option<String> {
    let text = v.as_str()?;
    if text.is_empty() || text == "0" {
        return None;
    }
    let script = coolify_remote::text::php_trim(text);
    if script.starts_with("#!") {
        return None;
    }
    if let Some(rest) = script.strip_prefix("#cloud-config") {
        let yaml = rest.trim_start();
        return coolify_compat::yaml_parse::parse(yaml, 128).err().map(|e| {
            format!(
                "The :attribute must be valid YAML format. Error: {}",
                e.message()
            )
        });
    }
    coolify_compat::yaml_parse::parse(script, 128).err().map(|e| {
        format!(
            "The :attribute must be either a valid bash script (starting with #!) or valid cloud-config YAML. YAML parse error: {}",
            e.message()
        )
    })
}

pub async fn scripts_index(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
) -> ApiResult {
    authorize_update(&ctx)?;
    let rows = cloud::list_scripts(&state.db, ctx.team_id).await?;
    let items = rows
        .into_iter()
        .map(|r| present_script(&state, &ctx, r))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ok(Value::Array(items)).into_response())
}

pub async fn scripts_show(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let r = cloud::find_script(&state.db, ctx.team_id, &uuid)
        .await?
        .ok_or(SCRIPT_NOT_FOUND)?;
    authorize_update(&ctx)?;
    Ok(ok(present_script(&state, &ctx, r)?).into_response())
}

pub async fn scripts_store(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    authorize_update(&ctx)?;
    let input = validated_json(&headers, &uri, &body)?;
    validate(
        &input,
        &[
            Field {
                name: "name",
                rules: vec![Rule::Required, Rule::String, Rule::Max(255)],
            },
            Field {
                name: "script",
                rules: vec![
                    Rule::Required,
                    Rule::String,
                    Rule::CustomOwned(cloud_init_rule),
                ],
            },
        ],
        CUSTOM_API_MESSAGES,
        &["name", "script"],
    )?;
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let uuid = new_public_id();
    cloud::create_script(
        &state.db,
        ctx.team_id,
        &uuid,
        input.str("name").unwrap_or_default(),
        &enc.encrypt_string(input.str("script").unwrap_or_default()),
        state.now(),
    )
    .await?;
    let r = cloud::find_script(&state.db, ctx.team_id, &uuid)
        .await?
        .ok_or(ApiError::Internal)?;
    Ok(json_status(
        StatusCode::CREATED,
        present_script(&state, &ctx, r)?,
    ))
}

pub async fn scripts_update(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let input = validated_json(&headers, &uri, &body)?;
    let r = cloud::find_script(&state.db, ctx.team_id, &uuid)
        .await?
        .ok_or(SCRIPT_NOT_FOUND)?;
    authorize_update(&ctx)?;
    validate(
        &input,
        &[
            Field {
                name: "name",
                rules: vec![Rule::String, Rule::Max(255)],
            },
            Field {
                name: "script",
                rules: vec![Rule::String, Rule::CustomOwned(cloud_init_rule)],
            },
        ],
        CUSTOM_API_MESSAGES,
        &["name", "script"],
    )?;
    let id = r.get("id").and_then(Value::as_i64).unwrap_or_default();
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let name = input
        .str("name")
        .filter(|n| r.get("name").and_then(Value::as_str) != Some(*n));
    let script = input
        .str("script")
        .filter(|s| {
            decrypt(&state, &r, "script")
                .ok()
                .as_ref()
                .and_then(Value::as_str)
                != Some(*s)
        })
        .map(|s| enc.encrypt_string(s));
    if name.is_some() || script.is_some() {
        cloud::update_script(&state.db, id, name, script.as_deref(), state.now()).await?;
    }
    let fresh = cloud::find_script(&state.db, ctx.team_id, &uuid)
        .await?
        .ok_or(ApiError::Internal)?;
    Ok(ok(present_script(&state, &ctx, fresh)?).into_response())
}

pub async fn scripts_destroy(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let r = cloud::find_script(&state.db, ctx.team_id, &uuid)
        .await?
        .ok_or(SCRIPT_NOT_FOUND)?;
    authorize_update(&ctx)?;
    let id = r.get("id").and_then(Value::as_i64).unwrap_or_default();
    cloud::delete_script(&state.db, id).await?;
    Ok(ok(json!({"message": "Cloud-init script deleted."})).into_response())
}
