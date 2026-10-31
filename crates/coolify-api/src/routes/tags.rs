//! `TagsController`, plus the row functions the tag pages share.

use axum::Extension;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::IntoResponse;
use coolify_db::records::{Record, laravel_datetime};
use coolify_db::shared_variables as sv;
use coolify_domain::naming::strip_tags;
use serde_json::{Value, json};

use crate::audit::Meta;
use crate::auth::AuthCtx;
use crate::input::{Input, validated_json};
use crate::response::{ApiError, ApiResult, created, ok};
use crate::state::AppState;
use crate::validation::{Field, Rule, validate};

const NOT_FOUND: ApiError = ApiError::Message(StatusCode::NOT_FOUND, "Tag not found.");
const EXISTS: ApiError =
    ApiError::Message(StatusCode::CONFLICT, "Tag with this name already exists.");

/// `TagsController::serializeTag`.
fn present(r: &Record) -> Value {
    let date = |k: &str| {
        r.get(k)
            .and_then(Value::as_str)
            .map_or(Value::Null, |t| Value::String(laravel_datetime(t)))
    };
    json!({
        "uuid": r.get("uuid"),
        "name": r.get("name"),
        "created_at": date("created_at"),
        "updated_at": date("updated_at"),
    })
}

/// `normalizeTagName`.
pub(crate) fn normalize(name: &str) -> String {
    strip_tags(Some(name)).trim().to_lowercase()
}

/// `validateTagWriteRequest`.
fn validated(input: &Input) -> Result<String, ApiError> {
    validate(
        input,
        &[Field {
            name: "name",
            rules: vec![Rule::Required, Rule::String, Rule::Min(2), Rule::Max(255)],
        }],
        &[],
        &["name"],
    )?;
    let name = normalize(input.str("name").unwrap_or_default());
    if name.chars().count() < 2 {
        let mut errors = serde_json::Map::new();
        errors.insert(
            "name".into(),
            json!(["The tag name must be at least 2 characters after sanitization."]),
        );
        return Err(ApiError::Validation(errors));
    }
    Ok(name)
}

/// `Auditable` context of a tag.
pub(crate) fn mutation_context(
    team_id: i64,
    uuid: &Value,
    name: &Value,
    changed: &[&str],
) -> Value {
    json!({"team_id": team_id, "tag_uuid": uuid, "tag_name": name, "changed_fields": changed})
}

pub async fn index(State(state): State<AppState>, Extension(ctx): Extension<AuthCtx>) -> ApiResult {
    let rows = sv::tags(&state.db, ctx.team_id).await?;
    Ok(ok(Value::Array(rows.iter().map(present).collect())).into_response())
}

pub async fn create(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let input = validated_json(&headers, &uri, &body)?;
    let name = validated(&input)?;
    if sv::tag_name_exists(&state.db, ctx.team_id, &name, None).await? {
        return Err(EXISTS);
    }
    let uuid = coolify_domain::new_public_id();
    let row = sv::create_tag(&state.db, ctx.team_id, &uuid, &name, state.now())
        .await?
        .ok_or(EXISTS)?;
    crate::audit::mutation(
        &state,
        &ctx,
        &meta,
        "tag",
        "created",
        mutation_context(ctx.team_id, &json!(uuid), &json!(name), &[]),
        json!({"name": {"old": null, "new": name}}),
    )
    .await;
    crate::audit::log(
        &state,
        &ctx,
        &meta,
        "api.tag.created",
        json!({"team_id": ctx.team_id_raw, "tag_uuid": uuid, "tag_name": name}),
        "info",
    )
    .await;
    Ok(created(present(&row)).into_response())
}

pub async fn update(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path(uuid): Path<String>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let input = validated_json(&headers, &uri, &body)?;
    let name = validated(&input)?;
    let tag = sv::find_tag(&state.db, ctx.team_id, &uuid)
        .await?
        .ok_or(NOT_FOUND)?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let id = tag.get("id").and_then(Value::as_i64).unwrap_or(0);
    let old = tag.get("name").and_then(Value::as_str).unwrap_or_default();
    if name != old && sv::tag_name_exists(&state.db, ctx.team_id, &name, Some(id)).await? {
        return Err(EXISTS);
    }
    let row = if name == old {
        tag.clone()
    } else {
        let row = sv::rename_tag(&state.db, id, &name, state.now())
            .await
            .map_err(|_| EXISTS)?
            .ok_or(NOT_FOUND)?;
        crate::audit::mutation(
            &state,
            &ctx,
            &meta,
            "tag",
            "updated",
            mutation_context(ctx.team_id, &json!(uuid), &json!(name), &["name"]),
            json!({"name": {"old": old, "new": name}}),
        )
        .await;
        row
    };
    crate::audit::log(
        &state,
        &ctx,
        &meta,
        "api.tag.updated",
        json!({"team_id": ctx.team_id_raw, "tag_uuid": uuid, "tag_name": name, "changed_fields": ["name"]}),
        "info",
    )
    .await;
    Ok(ok(present(&row)).into_response())
}

pub async fn delete(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path(uuid): Path<String>,
) -> ApiResult {
    let tag = sv::find_tag(&state.db, ctx.team_id, &uuid)
        .await?
        .ok_or(NOT_FOUND)?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let id = tag.get("id").and_then(Value::as_i64).unwrap_or(0);
    let name = tag.get("name").cloned().unwrap_or(Value::Null);
    sv::delete_tag(&state.db, id).await?;
    crate::audit::mutation(
        &state,
        &ctx,
        &meta,
        "tag",
        "deleted",
        mutation_context(ctx.team_id, &json!(uuid), &name, &[]),
        json!({"name": {"old": name, "new": null}}),
    )
    .await;
    crate::audit::log(
        &state,
        &ctx,
        &meta,
        "api.tag.deleted",
        json!({"team_id": ctx.team_id_raw, "tag_uuid": uuid, "tag_name": name}),
        "info",
    )
    .await;
    Ok(ok(json!({"message": "Tag deleted."})).into_response())
}
