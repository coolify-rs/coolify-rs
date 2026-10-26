//! `TeamController`. Every endpoint is restricted to the token's team.

use axum::Extension;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use coolify_db::records::Record;
use coolify_db::teams;
use serde_json::Value;

use crate::auth::AuthCtx;
use crate::input::lenient_json;
use crate::response::{ApiError, ApiResult, ok};
use crate::serialize::{serialize_record, serialize_value};
use crate::state::AppState;
use crate::validation::{CUSTOM_API_MESSAGES, Field, Rule, validate};

const TEAM_NOT_FOUND: ApiError = ApiError::Message(StatusCode::NOT_FOUND, "Team not found.");

/// `TeamController::removeSensitiveData`: hide `custom_server_limit`, `pivot`.
fn present_team(mut team: Record) -> Value {
    team.shift_remove("custom_server_limit");
    Value::Object(serialize_record(team))
}

/// Members hide `pivot`, `email_change_code`, `email_change_code_expires_at`.
fn present_members(members: Vec<Record>) -> Value {
    serialize_value(Value::Array(
        members
            .into_iter()
            .map(|mut m| {
                m.shift_remove("email_change_code");
                m.shift_remove("email_change_code_expires_at");
                Value::Object(m)
            })
            .collect(),
    ))
}

async fn token_team(state: &AppState, ctx: &AuthCtx) -> ApiResult<Record> {
    teams::get(&state.db, ctx.team_id)
        .await?
        .ok_or(TEAM_NOT_FOUND)
}

/// `GET /teams`: only the token's team, as an array.
pub async fn teams(State(state): State<AppState>, Extension(ctx): Extension<AuthCtx>) -> ApiResult {
    let team = token_team(&state, &ctx).await?;
    Ok(ok(Value::Array(vec![present_team(team)])).into_response())
}

pub async fn current_team(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
) -> ApiResult {
    Ok(ok(present_team(token_team(&state, &ctx).await?)).into_response())
}

pub async fn current_team_members(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
) -> ApiResult {
    token_team(&state, &ctx).await?;
    Ok(ok(present_members(
        teams::members(&state.db, ctx.team_id).await?,
    ))
    .into_response())
}

/// PHP `(int) $id`: leading digits, otherwise 0.
fn php_int(s: &str) -> i64 {
    let s = s.trim_start();
    let (sign, digits) = match s.strip_prefix('-') {
        Some(rest) => (-1, rest),
        None => (1, s.strip_prefix('+').unwrap_or(s)),
    };
    let end = digits
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(digits.len());
    digits[..end].parse::<i64>().map(|n| sign * n).unwrap_or(0)
}

pub async fn team_by_id(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(id): Path<String>,
) -> ApiResult {
    if php_int(&id) != ctx.team_id {
        return Err(TEAM_NOT_FOUND);
    }
    Ok(ok(present_team(token_team(&state, &ctx).await?)).into_response())
}

pub async fn members_by_id(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(id): Path<String>,
) -> ApiResult {
    if php_int(&id) != ctx.team_id {
        return Err(TEAM_NOT_FOUND);
    }
    token_team(&state, &ctx).await?;
    Ok(ok(present_members(
        teams::members(&state.db, ctx.team_id).await?,
    ))
    .into_response())
}

/// `PATCH /team`: admin/owner only; body `is_build_server_fallback_enabled`.
pub async fn update_current_team(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> Result<Response, ApiError> {
    token_team(&state, &ctx).await?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let input = lenient_json(&headers, &uri, &body);
    const FIELD: &str = "is_build_server_fallback_enabled";
    validate(
        &input,
        &[Field {
            name: FIELD,
            rules: vec![Rule::Required, Rule::Boolean],
        }],
        CUSTOM_API_MESSAGES,
        &[FIELD],
    )?;
    let enabled = match input.get(FIELD) {
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_i64() == Some(1),
        Some(Value::String(s)) => s == "1",
        _ => false,
    };
    teams::set_build_server_fallback(&state.db, ctx.team_id, enabled, state.now()).await?;
    Ok(ok(present_team(token_team(&state, &ctx).await?)).into_response())
}

#[cfg(test)]
mod tests {
    use super::php_int;

    #[test]
    fn php_int_cast() {
        assert_eq!(php_int("4"), 4);
        assert_eq!(php_int("4abc"), 4);
        assert_eq!(php_int("abc"), 0);
        assert_eq!(php_int("-3"), -3);
        assert_eq!(php_int(""), 0);
    }
}
