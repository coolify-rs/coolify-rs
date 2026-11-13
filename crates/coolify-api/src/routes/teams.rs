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
use crate::input::{Input, lenient_json};
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

/// The two fields of `PATCH /teams/current` (4.7.5).
const TEAM_UPDATE_FIELDS: [&str; 2] = [
    "is_build_server_fallback_enabled",
    "scan_images_on_new_servers",
];

/// Laravel `validateRequired` failing: the key is missing, null, a blank string or an empty array.
fn required_fails(input: &Input, key: &str) -> bool {
    match input.get(key) {
        None | Some(Value::Null) => true,
        Some(Value::String(s)) => s.trim().is_empty(),
        Some(Value::Array(a)) => a.is_empty(),
        Some(Value::Object(o)) => o.is_empty(),
        Some(_) => false,
    }
}

/// `required_without:<other>` of `update_current_team` (4.7.5): each field is required when the
/// other one is not sent. The messages go before the field's other errors, as Laravel orders them.
pub(crate) fn team_update_required_errors(input: &Input) -> Vec<(&'static str, String)> {
    let [first, second] = TEAM_UPDATE_FIELDS;
    if !(required_fails(input, first) && required_fails(input, second)) {
        return Vec::new();
    }
    let message = |field: &str, other: &str| {
        format!(
            "The {} field is required when {} is not present.",
            field.replace('_', " "),
            other.replace('_', " ")
        )
    };
    vec![
        (first, message(first, second)),
        (second, message(second, first)),
    ]
}

/// `PATCH /team`: admin/owner only; body `is_build_server_fallback_enabled` and/or (4.7.5)
/// `scan_images_on_new_servers`.
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
    let validated = validate(
        &input,
        &TEAM_UPDATE_FIELDS.map(|name| Field {
            name,
            rules: vec![Rule::Boolean],
        }),
        CUSTOM_API_MESSAGES,
        &TEAM_UPDATE_FIELDS,
    );
    let required = team_update_required_errors(&input);
    let mut found = match validated {
        Ok(()) => serde_json::Map::new(),
        Err(ApiError::Validation(m)) => m,
        Err(e) => return Err(e),
    };
    if !required.is_empty() || !found.is_empty() {
        let mut errors = serde_json::Map::new();
        for field in TEAM_UPDATE_FIELDS {
            let mut list: Vec<Value> = required
                .iter()
                .filter(|(f, _)| *f == field)
                .map(|(_, m)| Value::String(m.clone()))
                .collect();
            if let Some(Value::Array(rest)) = found.shift_remove(field) {
                list.extend(rest);
            }
            if !list.is_empty() {
                errors.insert(field.to_owned(), Value::Array(list));
            }
        }
        errors.extend(found);
        return Err(ApiError::Validation(errors));
    }
    // `$team->update($validator->validated())`.
    let flag = |key: &str| match input.get(key) {
        Some(Value::Bool(b)) => Some(*b),
        Some(Value::Number(n)) => Some(n.as_i64() == Some(1)),
        Some(Value::String(s)) => Some(s == "1"),
        _ => None,
    };
    if let Some(enabled) = flag(TEAM_UPDATE_FIELDS[0]) {
        teams::set_build_server_fallback(&state.db, ctx.team_id, enabled, state.now()).await?;
    }
    if let Some(enabled) = flag(TEAM_UPDATE_FIELDS[1]) {
        teams::set_scan_images_on_new_servers(&state.db, ctx.team_id, enabled, state.now()).await?;
    }
    Ok(ok(present_team(token_team(&state, &ctx).await?)).into_response())
}

#[cfg(test)]
mod tests {
    use super::{php_int, team_update_required_errors};
    use crate::input::Input;
    use serde_json::json;

    fn input(v: serde_json::Value) -> Input {
        Input(v.as_object().cloned().unwrap_or_default())
    }

    /// `update_current_team` (4.7.5): `required_without` on both fields.
    #[test]
    fn team_update_needs_one_of_its_two_fields() {
        assert_eq!(
            team_update_required_errors(&input(json!({}))),
            vec![
                (
                    "is_build_server_fallback_enabled",
                    "The is build server fallback enabled field is required when scan images on new servers is not present.".to_owned()
                ),
                (
                    "scan_images_on_new_servers",
                    "The scan images on new servers field is required when is build server fallback enabled is not present.".to_owned()
                ),
            ]
        );
        assert_eq!(
            team_update_required_errors(&input(json!({"scan_images_on_new_servers": null}))).len(),
            2
        );
        assert!(
            team_update_required_errors(&input(json!({"scan_images_on_new_servers": false})))
                .is_empty()
        );
        assert!(
            team_update_required_errors(&input(json!({"is_build_server_fallback_enabled": true})))
                .is_empty()
        );
    }

    #[test]
    fn php_int_cast() {
        assert_eq!(php_int("4"), 4);
        assert_eq!(php_int("4abc"), 4);
        assert_eq!(php_int("abc"), 0);
        assert_eq!(php_int("-3"), -3);
        assert_eq!(php_int(""), 0);
    }
}
