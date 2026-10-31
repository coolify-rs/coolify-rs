//! `Concerns\HandlesTagsApi`: tags of any taggable resource (morph type + id), and the application
//! wrappers of `ApplicationsController::tags`, `create_tag` and `delete_tag`.

use axum::Extension;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::IntoResponse;
use coolify_db::applications as apps;
use coolify_db::records::TAG;
use coolify_db::tags as store;
use coolify_domain::naming::strip_tags;
use coolify_remote::text::php_trim;
use serde_json::{Map, Value, json};

use crate::auth::AuthCtx;
use crate::input::{Input, validated_json};
use crate::response::{ApiError, ApiResult, created, ok};
use crate::state::AppState;

const TOO_SHORT: &str = "Each tag name must be at least 2 characters after sanitization.";

/// `strtolower(trim(strip_tags((string) $name)))` for each name, `->unique()`.
pub fn normalize_tag_names<'a>(
    names: impl IntoIterator<Item = &'a Value>,
) -> Result<Vec<String>, ApiError> {
    let mut out: Vec<String> = Vec::new();
    for name in names {
        let text = match name {
            Value::String(s) => s.clone(),
            Value::Null | Value::Bool(false) => String::new(),
            Value::Bool(true) => "1".to_owned(),
            Value::Number(n) => n.to_string(),
            // "Array to string conversion".
            _ => return Err(ApiError::Internal),
        };
        let normalized = php_trim(&strip_tags(Some(&text))).to_ascii_lowercase();
        if !out.contains(&normalized) {
            out.push(normalized);
        }
    }
    Ok(out)
}

fn single_error(field: &str, message: &str) -> ApiError {
    let mut errors = Map::new();
    errors.insert(field.to_owned(), json!([message]));
    ApiError::Validation(errors)
}

/// The `'tags.*' => 'string|min:2'` rule of the create endpoints.
pub fn tag_item_errors(input: &Input, errors: &mut Map<String, Value>) {
    let items: Vec<(String, &Value)> = match input.get("tags") {
        Some(Value::Array(a)) => a
            .iter()
            .enumerate()
            .map(|(i, v)| (i.to_string(), v))
            .collect(),
        Some(Value::Object(o)) => o.iter().map(|(k, v)| (k.clone(), v)).collect(),
        _ => return,
    };
    for (k, v) in items {
        let field = format!("tags.{k}");
        let mut messages = Vec::new();
        if !v.is_string() {
            messages.push(format!("The {field} field must be a string."));
        }
        let (size, unit) = match v {
            Value::Array(a) => (a.len(), "items"),
            Value::Object(o) => (o.len(), "items"),
            Value::String(s) => (s.chars().count(), "characters"),
            Value::Null => (0, "characters"),
            Value::Bool(b) => (usize::from(*b), "characters"),
            Value::Number(n) => (n.to_string().chars().count(), "characters"),
        };
        if size < 2 {
            let verb = if unit == "items" { "have" } else { "be" };
            messages.push(format!("The {field} field must {verb} at least 2 {unit}."));
        }
        if !messages.is_empty() {
            errors.insert(field, json!(messages));
        }
    }
}

/// `validateTagsParameter($request)`: the normalized `tags`, empty when absent.
pub fn validate_tags_parameter(input: &Input) -> Result<Vec<String>, ApiError> {
    let names = match input.get("tags") {
        None => return Ok(Vec::new()),
        Some(Value::Array(a)) => normalize_tag_names(a)?,
        Some(Value::Object(o)) => normalize_tag_names(o.values())?,
        // `normalizeTagNames(array $tagNames)` gets null: a TypeError.
        Some(_) => return Err(ApiError::Internal),
    };
    if names.iter().any(|n| n.chars().count() < 2) {
        return Err(single_error("tags", TOO_SHORT));
    }
    Ok(names)
}

/// `attachTagsToResource($resource, $tagNames, $teamId)`.
pub async fn attach_tags_to_resource(
    state: &AppState,
    morph: &str,
    resource_id: i64,
    names: &[String],
    team_id: i64,
) -> Result<(), ApiError> {
    for name in names {
        if name.chars().count() < 2 {
            continue;
        }
        let tag = store::create_or_first(&state.db, team_id, name, state.now()).await?;
        store::attach(&state.db, morph, resource_id, tag).await?;
    }
    Ok(())
}

/// `$resource->tags->map(TagsController::serializeTag(...))`.
pub async fn serialized_tags(
    state: &AppState,
    morph: &str,
    resource_id: i64,
) -> Result<Value, ApiError> {
    let rows = store::for_resource(&state.db, morph, resource_id).await?;
    Ok(Value::Array(
        rows.into_iter()
            .map(|r| {
                let r = TAG.present(r);
                let mut m = Map::new();
                for k in ["uuid", "name", "created_at", "updated_at"] {
                    m.insert(k.into(), r.get(k).cloned().unwrap_or(Value::Null));
                }
                Value::Object(m)
            })
            .collect(),
    ))
}

/// The `Validator::make` of `createTag`, whose failures and extra fields come back together.
fn validate_create(input: &Input) -> Result<(), ApiError> {
    let mut errors = Map::new();
    let filled = |v: Option<&Value>| match v {
        None | Some(Value::Null) => false,
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
        Some(_) => true,
    };
    let (name, names) = (input.get("tag_name"), input.get("tag_names"));
    if !filled(names) && !filled(name) {
        errors.insert(
            "tag_name".into(),
            json!(["The tag name field is required when tag names is not present."]),
        );
    } else if name.is_some_and(|v| !v.is_string()) {
        errors.insert(
            "tag_name".into(),
            json!(["The tag name field must be a string."]),
        );
    }
    if !filled(name) && !filled(names) {
        errors.insert(
            "tag_names".into(),
            json!(["The tag names field is required when tag name is not present."]),
        );
    } else if names.is_some_and(|v| !(v.is_array() || v.is_object())) {
        errors.insert(
            "tag_names".into(),
            json!(["The tag names field must be an array."]),
        );
    }
    let entries: Vec<(String, &Value)> = match names {
        Some(Value::Array(a)) => a
            .iter()
            .enumerate()
            .map(|(i, v)| (i.to_string(), v))
            .collect(),
        Some(Value::Object(o)) => o.iter().map(|(k, v)| (k.clone(), v)).collect(),
        _ => Vec::new(),
    };
    for (k, v) in entries {
        if !v.is_string() {
            let field = format!("tag_names.{k}");
            errors.insert(
                field.clone(),
                json!([format!("The {field} field must be a string.")]),
            );
        }
    }
    for k in input.keys() {
        if k != "tag_name" && k != "tag_names" {
            errors.insert(k.clone(), json!(["This field is not allowed."]));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(ApiError::Validation(errors))
    }
}

/// `createTag($request)` after the authorization.
pub async fn create_tag(
    state: &AppState,
    morph: &str,
    resource_id: i64,
    team_id: i64,
    input: &Input,
) -> Result<Value, ApiError> {
    if input.has("tag_name") && input.has("tag_names") {
        return Err(single_error(
            "tag_name",
            "Provide either tag_name or tag_names, not both.",
        ));
    }
    validate_create(input)?;
    let names = match input.get("tag_names") {
        Some(Value::Array(a)) => normalize_tag_names(a)?,
        Some(Value::Object(o)) => normalize_tag_names(o.values())?,
        _ => normalize_tag_names(input.get("tag_name"))?,
    };
    if names.iter().any(|n| n.chars().count() < 2) {
        return Err(single_error("tag_name", TOO_SHORT));
    }
    attach_tags_to_resource(state, morph, resource_id, &names, team_id).await?;
    serialized_tags(state, morph, resource_id).await
}

/// `deleteTag($request)` after the authorization. The tag stays, also without resources.
pub async fn remove_tag(
    state: &AppState,
    morph: &str,
    resource_id: i64,
    team_id: i64,
    tag_uuid: &str,
) -> Result<Value, ApiError> {
    let tag = store::id_in_team(&state.db, team_id, tag_uuid)
        .await?
        .ok_or(ApiError::Message(StatusCode::NOT_FOUND, "Tag not found."))?;
    if !store::detach(&state.db, morph, resource_id, tag).await? {
        return Err(ApiError::Message(
            StatusCode::NOT_FOUND,
            "Tag not found on resource.",
        ));
    }
    Ok(json!({"message": "Tag removed."}))
}

const APP_NOT_FOUND: ApiError = ApiError::Message(StatusCode::NOT_FOUND, "Application not found.");

/// `GET /applications/{uuid}/tags`.
pub async fn application_list(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let a = apps::find_in_team(&state.db, ctx.team_id, &uuid)
        .await?
        .ok_or(APP_NOT_FOUND)?;
    Ok(ok(serialized_tags(&state, apps::MORPH, a.id).await?).into_response())
}

/// `POST /applications/{uuid}/tags`.
pub async fn application_create(
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
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let tags = create_tag(&state, apps::MORPH, a.id, ctx.team_id, &input).await?;
    Ok(created(tags).into_response())
}

/// `DELETE /applications/{uuid}/tags/{tag_uuid}`.
pub async fn application_delete(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((uuid, tag_uuid)): Path<(String, String)>,
) -> ApiResult {
    let a = apps::find_in_team(&state.db, ctx.team_id, &uuid)
        .await?
        .ok_or(APP_NOT_FOUND)?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let message = remove_tag(&state, apps::MORPH, a.id, ctx.team_id, &tag_uuid).await?;
    Ok(ok(message).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_names() {
        let names = [json!(" <b>Prod</b> "), json!("prod"), json!(12)];
        assert_eq!(normalize_tag_names(&names).unwrap(), vec!["prod", "12"]);
    }
}
