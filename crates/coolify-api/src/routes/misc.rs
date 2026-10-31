//! `OtherController`: health, version, disable.

use axum::Extension;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use coolify_db::instance;
use serde_json::json;

use crate::auth::AuthCtx;
use crate::response::{ApiError, ApiResult, ok, text};
use crate::state::AppState;

/// `GET /api/health`: plain `OK`, deliberately without a version header.
pub async fn health() -> Response {
    text("OK".into())
}

/// `GET /api/v1/version`.
pub async fn version(State(state): State<AppState>) -> Response {
    text(state.version.clone())
}

/// `OtherController@post_required`.
pub async fn post_required() -> Response {
    ApiError::PostRequired.into_response()
}

/// `POST /api/v1/disable`: only tokens of the root team (`team_id === '0'`).
pub async fn disable_api(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
) -> ApiResult {
    if ctx.team_id_raw != "0" {
        return Err(ApiError::Message(
            StatusCode::FORBIDDEN,
            "You are not allowed to disable the API.",
        ));
    }
    instance::disable_api(&state.db, state.now()).await?;
    Ok(ok(json!({"message": "API disabled."})).into_response())
}

async fn set_mcp(
    state: &AppState,
    ctx: &AuthCtx,
    meta: &crate::audit::Meta,
    enabled: bool,
) -> ApiResult {
    let (word, past) = if enabled {
        ("enable", "enabled")
    } else {
        ("disable", "disabled")
    };
    if ctx.team_id_raw != "0" {
        crate::audit::log(
            state,
            ctx,
            meta,
            &format!("api.mcp.{word}_denied"),
            json!({"team_id": ctx.team_id_raw}),
            "warning",
        )
        .await;
        return Err(ApiError::Message(
            StatusCode::FORBIDDEN,
            if enabled {
                "You are not allowed to enable the MCP server."
            } else {
                "You are not allowed to disable the MCP server."
            },
        ));
    }
    coolify_db::instance_settings::update(
        &state.db,
        &[(
            "is_mcp_server_enabled",
            coolify_db::servers::ColumnValue::Bool(enabled),
        )],
        state.now(),
    )
    .await?;
    crate::audit::log(
        state,
        ctx,
        meta,
        &format!("api.mcp.{past}"),
        json!({"team_id": ctx.team_id_raw}),
        "info",
    )
    .await;
    Ok(ok(json!({"message": format!("MCP server {past}.")})).into_response())
}

/// `OtherController::enable_mcp`.
pub async fn enable_mcp(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: crate::audit::Meta,
) -> ApiResult {
    set_mcp(&state, &ctx, &meta, true).await
}

/// `OtherController::disable_mcp`.
pub async fn disable_mcp(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: crate::audit::Meta,
) -> ApiResult {
    set_mcp(&state, &ctx, &meta, false).await
}

/// `OtherController::feedback` with `throttle:feedback` (3 per minute per address).
pub async fn feedback(
    meta: crate::audit::Meta,
    headers: axum::http::HeaderMap,
    uri: axum::http::Uri,
    body: axum::body::Bytes,
) -> ApiResult {
    let key = format!("feedback:{}", meta.ip.as_deref().unwrap_or(""));
    if crate::rules::throttle(&key, 3, std::time::Duration::from_secs(60)).is_some() {
        return Err(ApiError::Message(
            StatusCode::TOO_MANY_REQUESTS,
            "Too Many Attempts.",
        ));
    }
    let input = crate::input::lenient_json(&headers, &uri, &body);
    let content = input.get("content");
    let msg = match content {
        None | Some(serde_json::Value::Null) => Some("The content field is required."),
        Some(serde_json::Value::String(s)) if s.chars().count() < 10 => {
            Some("The content field must be at least 10 characters.")
        }
        Some(serde_json::Value::String(s)) if s.chars().count() > 2000 => {
            Some("The content field must not be greater than 2000 characters.")
        }
        Some(serde_json::Value::String(_)) => None,
        Some(_) => Some("The content field must be a string."),
    };
    if let Some(m) = msg {
        let mut errors = serde_json::Map::new();
        errors.insert("content".into(), json!([m]));
        return Err(ApiError::ValidationWithMessage(m, errors));
    }
    let webhook = std::env::var("FEEDBACK_DISCORD_WEBHOOK").unwrap_or_default();
    if !webhook.is_empty() {
        let sent = reqwest::Client::new()
            .post(&webhook)
            .timeout(std::time::Duration::from_secs(5))
            .json(&json!({"content": input.str("content"), "allowed_mentions": {"parse": []}}))
            .send()
            .await;
        if !sent.is_ok_and(|r| r.status().is_success()) {
            return Err(ApiError::Message(
                StatusCode::BAD_GATEWAY,
                "Feedback could not be delivered.",
            ));
        }
    }
    Ok(ok(json!({"message": "Feedback sent."})).into_response())
}
