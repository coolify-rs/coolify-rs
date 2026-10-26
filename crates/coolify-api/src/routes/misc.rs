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
