//! `ApplicationSecretManagerController::update`: stores which secret manager source an application
//! uses. Only the row is written; fetching secrets from the provider is not part of this path.

use axum::Extension;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::IntoResponse;
use coolify_compat::php_json;
use coolify_db::applications as apps;
use coolify_db::secret_managers as store;
use serde_json::{Map, Value, json};

use crate::auth::AuthCtx;
use crate::input::{Input, validated_json};
use crate::response::{ApiError, ApiResult, ok};
use crate::state::AppState;
use crate::validation::{CUSTOM_API_MESSAGES, Field, Rule, validate};

/// `filled($value)`.
fn filled(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::String(s) => !s.trim().is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
        _ => true,
    }
}

/// `PATCH /applications/{uuid}/secret-manager`.
pub async fn application_update(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    // `validateIncomingRequest`, then `$request->json()->all()`: the query string does not count.
    validated_json(&headers, &uri, &body)?;
    let input = validated_json(&headers, &Uri::from_static("/"), &body)?;
    let a = apps::find_in_team(&state.db, ctx.team_id, &uuid)
        .await?
        .ok_or(ApiError::Message(
            StatusCode::NOT_FOUND,
            "Application not found.",
        ))?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let not_found = ApiError::Message(
        StatusCode::NOT_FOUND,
        "Secret manager integration token not found.",
    );
    let Some(token) = (match input.str("integration_token_uuid") {
        Some(u) => store::token_in_team(&state.db, ctx.team_id, u).await?,
        None => None,
    }) else {
        return Err(not_found);
    };
    let capabilities: Vec<Value> = serde_json::from_str(&token.capabilities).unwrap_or_default();
    if !capabilities.iter().any(|c| c.as_str() == Some("secrets")) {
        return Err(not_found);
    }
    // The nested rules of the provider, as `settings.*` fields.
    let required = |name: &'static str| Field {
        name,
        rules: vec![Rule::Required, Rule::String],
    };
    let mut fields = vec![
        Field {
            name: "integration_token_uuid",
            rules: vec![Rule::Required, Rule::String],
        },
        Field {
            name: "settings",
            rules: vec![Rule::Array],
        },
    ];
    let nested: Vec<&'static str> = match token.provider.as_str() {
        "doppler" => {
            let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
            let plain = enc
                .decrypt_string(&token.token_encrypted)
                .map_err(|_| ApiError::Internal)?;
            if plain.starts_with("dp.sa.") {
                fields.push(required("settings.project"));
                fields.push(required("settings.config"));
                vec!["project", "config"]
            } else {
                vec![]
            }
        }
        "infisical" => {
            fields.push(required("settings.project_id"));
            fields.push(required("settings.environment"));
            fields.push(Field {
                name: "settings.secret_path",
                rules: vec![Rule::String, Rule::Nullable],
            });
            vec!["project_id", "environment", "secret_path"]
        }
        "vault" => {
            fields.push(required("settings.mount"));
            fields.push(required("settings.path"));
            vec!["mount", "path"]
        }
        _ => vec![],
    };
    let settings_input = input.get("settings").and_then(Value::as_object);
    let mut flat: Map<String, Value> = input.0.clone();
    for k in &nested {
        if let Some(v) = settings_input.and_then(|s| s.get(*k)) {
            flat.insert(format!("settings.{k}"), v.clone());
        }
    }
    let allowed: Vec<String> = ["integration_token_uuid", "settings"]
        .into_iter()
        .map(str::to_owned)
        .chain(nested.iter().map(|k| format!("settings.{k}")))
        .collect();
    let allowed: Vec<&str> = allowed.iter().map(String::as_str).collect();
    validate(&Input(flat), &fields, CUSTOM_API_MESSAGES, &allowed)?;

    // `$validator->validated()['settings']`: with nested rules only those keys, in rule order.
    let mut settings = Map::new();
    if let Some(given) = settings_input {
        if nested.is_empty() {
            settings.extend(
                given
                    .iter()
                    .filter(|(_, v)| filled(v))
                    .map(|(k, v)| (k.clone(), v.clone())),
            );
        } else {
            for k in &nested {
                if let Some(v) = given.get(*k).filter(|v| filled(v)) {
                    settings.insert((*k).to_owned(), v.clone());
                }
            }
        }
    }
    let settings = (!settings.is_empty()).then_some(Value::Object(settings));
    let encoded = settings.as_ref().map(php_json::encode);
    store::update_or_create_link(
        &state.db,
        apps::MORPH,
        a.id,
        token.id,
        encoded.as_deref(),
        state.now(),
    )
    .await?;
    tracing::info!(target: "audit", event = "api.application.secret_manager.updated", team_id = ctx.team_id, application_uuid = %a.uuid, integration_token_uuid = %token.uuid, "secret manager configured");
    Ok(ok(json!({
        "integration_token_uuid": token.uuid,
        "provider": token.provider,
        "settings": settings,
    }))
    .into_response())
}
