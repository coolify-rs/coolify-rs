//! Page endpoints of `App\Livewire\Tags\Show` (livewire/tags/show.blade.php).

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::middleware::from_fn_with_state;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use coolify_db::shared_variables as sv;
use coolify_db::tag_pages;
use serde_json::{Map, Value, json};

use crate::audit::Meta;
use crate::auth::AuthCtx;
use crate::response::{ApiError, ApiResult, PhpJson, ok};
use crate::routes::applications::deploy_application;
use crate::state::AppState;
use crate::ui_notifications::{toast_error, unauthorized};

pub fn routes(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/ui/tags", get(index).post(create))
        .route("/ui/tags/{name}", get(show).delete(destroy))
        .route("/ui/tags/{name}/redeploy", post(redeploy))
        .route("/ui/tags/{name}/connectable", get(connectable))
        .route("/ui/tags/{name}/toggle", post(toggle))
        .route_layer(from_fn_with_state(state, crate::session::ui_guard))
}

async fn index(State(state): State<AppState>, Extension(ctx): Extension<AuthCtx>) -> ApiResult {
    let tags = tag_pages::tags_with_counts(&state.db, ctx.team_id).await?;
    Ok(ok(json!({"tags": tags, "can_create": ctx.is_team_admin()})).into_response())
}

/// `createTag` in `Concerns\CreatesTags`.
async fn create(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Json(body): Json<Value>,
) -> ApiResult {
    if !ctx.is_team_admin() {
        return Ok(unauthorized());
    }
    let field = |m: &str| {
        let mut errors = Map::new();
        errors.insert("name".into(), json!([m]));
        PhpJson(
            StatusCode::UNPROCESSABLE_ENTITY,
            json!({"message": m, "errors": errors}),
        )
        .into_response()
    };
    let raw = body.get("name").and_then(Value::as_str).unwrap_or_default();
    if raw.trim().is_empty() {
        return Ok(field("The new tag name field is required."));
    }
    let len = raw.chars().count();
    if len < 2 {
        return Ok(field(
            "The new tag name field must be at least 2 characters.",
        ));
    }
    if len > 255 {
        return Ok(field(
            "The new tag name field must not be greater than 255 characters.",
        ));
    }
    if raw.chars().any(char::is_whitespace) {
        return Ok(field("A tag name cannot contain spaces."));
    }
    let name = crate::routes::tags::normalize(raw);
    if name.chars().count() < 2 {
        return Ok(field(
            "The tag name must be at least 2 characters after sanitization.",
        ));
    }
    if sv::tag_name_exists(&state.db, ctx.team_id, &name, None).await? {
        return Ok(field("A tag with this name already exists."));
    }
    let uuid = coolify_domain::new_public_id();
    if sv::create_tag(&state.db, ctx.team_id, &uuid, &name, state.now())
        .await?
        .is_none()
    {
        return Ok(toast_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Duplicate entry found. Please use a different name.",
        ));
    }
    crate::audit::mutation(
        &state,
        &ctx,
        &meta,
        "tag",
        "created",
        crate::routes::tags::mutation_context(ctx.team_id, &json!(uuid), &json!(name), &[]),
        json!({"name": {"old": null, "new": name}}),
    )
    .await;
    Ok(ok(json!({"name": name})).into_response())
}

/// `$this->tags->where('name', $this->tagName)->first()`.
async fn find(state: &AppState, ctx: &AuthCtx, name: &str) -> Result<Option<Value>, ApiError> {
    Ok(tag_pages::tags_with_counts(&state.db, ctx.team_id)
        .await?
        .into_iter()
        .find(|t| t.get("name").and_then(Value::as_str) == Some(name)))
}

fn id_of(v: &Value) -> i64 {
    v.get("id").and_then(Value::as_i64).unwrap_or(0)
}

async fn show(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(name): Path<String>,
) -> ApiResult {
    let Some(tag) = find(&state, &ctx, &name).await? else {
        return Ok(ok(json!({"found": false})).into_response());
    };
    let id = id_of(&tag);
    let applications = tag_pages::tag_applications(&state.db, id).await?;
    let services = tag_pages::tag_services(&state.db, id).await?;
    let ids: Vec<i64> = applications.iter().map(id_of).collect();
    let deployments = tag_pages::active_deployments(&state.db, &ids).await?;
    let ctx_mail = coolify_jobs::notifications::Ctx::load(&state.db).await;
    Ok(ok(json!({
        "found": true,
        "tag": tag,
        "webhook": format!("{}/api/v1/deploy?tag={name}", ctx_mail.base_url),
        "applications": applications,
        "services": services,
        "deployments": deployments,
        "can_delete": ctx.is_team_admin(),
    }))
    .into_response())
}

/// `deleteTag`: taggables rows cascade.
async fn destroy(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path(name): Path<String>,
) -> ApiResult {
    let Some(tag) = find(&state, &ctx, &name).await? else {
        return Ok(toast_error(StatusCode::NOT_FOUND, "Tag not found."));
    };
    if !ctx.is_team_admin() {
        return Ok(unauthorized());
    }
    sv::delete_tag(&state.db, id_of(&tag)).await?;
    crate::audit::mutation(
        &state,
        &ctx,
        &meta,
        "tag",
        "deleted",
        crate::routes::tags::mutation_context(ctx.team_id, &tag["uuid"], &json!(name), &[]),
        json!({"name": {"old": name, "new": null}}),
    )
    .await;
    Ok(ok(json!({})).into_response())
}

/// `redeployAll`.
async fn redeploy(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path(name): Path<String>,
) -> ApiResult {
    let Some(tag) = find(&state, &ctx, &name).await? else {
        return Ok(toast_error(StatusCode::NOT_FOUND, "Tag not found."));
    };
    if !ctx.is_team_admin() {
        return Ok(unauthorized());
    }
    let id = id_of(&tag);
    for a in tag_pages::tag_applications(&state.db, id).await? {
        let uuid = a.get("uuid").and_then(Value::as_str).unwrap_or_default();
        if let Some(row) =
            coolify_db::applications::find_in_team(&state.db, ctx.team_id, uuid).await?
        {
            deploy_application(&state, &ctx, &meta, &row, false).await?;
        }
    }
    for s in tag_pages::tag_services(&state.db, id).await? {
        crate::routes::services::deploy_service(
            &state,
            &ctx,
            &meta,
            id_of(&s),
            s.get("uuid").and_then(Value::as_str).unwrap_or_default(),
            s.get("name").and_then(Value::as_str).unwrap_or_default(),
        )
        .await?;
    }
    Ok(ok(json!({"message": "Mass deployment started."})).into_response())
}

/// `connectableResources` for the "Connect to resources" modal.
async fn connectable(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(name): Path<String>,
) -> ApiResult {
    let Some(tag) = find(&state, &ctx, &name).await? else {
        return Ok(toast_error(StatusCode::NOT_FOUND, "Tag not found."));
    };
    let rows = tag_pages::connectable(&state.db, ctx.team_id, id_of(&tag)).await?;
    let can_update = ctx.is_team_admin();
    Ok(ok(json!({
        "resources": rows.iter().map(|r| json!({
            "context": format!("{} / {}", r.project, r.environment),
            "uuid": r.uuid,
            "name": r.name,
            "tagged": r.tagged,
            "can_update": can_update,
        })).collect::<Vec<_>>(),
    }))
    .into_response())
}

/// `toggleResource`.
async fn toggle(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(name): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult {
    let Some(tag) = find(&state, &ctx, &name).await? else {
        return Ok(toast_error(StatusCode::NOT_FOUND, "Tag not found."));
    };
    let uuid = body.get("uuid").and_then(Value::as_str).unwrap_or_default();
    let id = id_of(&tag);
    let rows = tag_pages::connectable(&state.db, ctx.team_id, id).await?;
    let Some(r) = rows.iter().find(|r| r.uuid == uuid) else {
        return Ok(toast_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Resource not found.",
        ));
    };
    if !ctx.is_team_admin() {
        return Ok(unauthorized());
    }
    if r.tagged {
        tag_pages::detach(&state.db, id, &r.morph, r.id).await?;
    } else {
        tag_pages::attach(&state.db, id, &r.morph, r.id).await?;
    }
    Ok(ok(json!({"tagged": !r.tagged})).into_response())
}
