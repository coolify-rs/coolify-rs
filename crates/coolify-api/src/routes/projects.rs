//! `ProjectController`: projects and environments.
//!
//! Check order follows upstream exactly (it is observable): e.g. request
//! validation happens before the project lookup on PATCH, so an invalid body
//! on a missing project is a 422, not a 404.
//!
//! Policies (`ProjectPolicy`, `EnvironmentPolicy`): view = team member,
//! create/update/delete = admin or owner of the owning team. Queries are
//! already scoped to the token's team, so membership is implied.

use axum::Extension;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use coolify_db::projects::{self, CreateEnvError, EnvironmentRef, ProjectRef};
use serde_json::{Value, json};

use crate::auth::AuthCtx;
use crate::input::validated_json;
use crate::response::{ApiError, ApiResult, created, ok};
use crate::serialize::{serialize_record, serialize_value};
use crate::state::AppState;
use crate::validation::patterns::{COMBINED_MESSAGES, NAME_MESSAGES, description, name};
use crate::validation::validate;

const PROJECT_NOT_FOUND: ApiError = ApiError::Message(StatusCode::NOT_FOUND, "Project not found.");
const ENV_NOT_FOUND: ApiError = ApiError::Message(StatusCode::NOT_FOUND, "Environment not found.");
const ENV_EXISTS: ApiError = ApiError::Message(
    StatusCode::CONFLICT,
    "Environment with this name already exists.",
);

fn require_admin(ctx: &AuthCtx) -> Result<(), ApiError> {
    if ctx.is_team_admin() {
        Ok(())
    } else {
        Err(ApiError::Unauthorized)
    }
}

async fn find_project(state: &AppState, ctx: &AuthCtx, uuid: &str) -> ApiResult<ProjectRef> {
    projects::find(&state.db, ctx.team_id, uuid)
        .await?
        .ok_or(PROJECT_NOT_FOUND)
}

async fn find_env(
    state: &AppState,
    project: &ProjectRef,
    name_or_uuid: &str,
) -> ApiResult<EnvironmentRef> {
    projects::find_environment(&state.db, project.id, name_or_uuid)
        .await?
        .ok_or(ENV_NOT_FOUND)
}

fn env_error(e: CreateEnvError) -> ApiError {
    match e {
        CreateEnvError::NameTaken => ENV_EXISTS,
        CreateEnvError::Db(e) => e.into(),
    }
}

pub async fn list(State(state): State<AppState>, Extension(ctx): Extension<AuthCtx>) -> ApiResult {
    let rows = projects::list(&state.db, ctx.team_id).await?;
    Ok(ok(serialize_value(Value::Array(
        rows.into_iter().map(Value::Object).collect(),
    )))
    .into_response())
}

pub async fn show(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let project = find_project(&state, &ctx, &uuid).await?;
    let mut record = projects::record(&state.db, project.id).await?;
    let envs = projects::environment_records(&state.db, project.id).await?;
    record.insert(
        "environments".into(),
        Value::Array(envs.into_iter().map(Value::Object).collect()),
    );
    Ok(ok(Value::Object(serialize_record(record))).into_response())
}

pub async fn environments(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let project = find_project(&state, &ctx, &uuid).await?;
    let rows = projects::environment_briefs(&state.db, project.id).await?;
    Ok(ok(serialize_value(Value::Array(
        rows.into_iter().map(Value::Object).collect(),
    )))
    .into_response())
}

/// Relations loaded by `environment_details`, in upstream order.
const DETAIL_RELATIONS: &[&str] = &[
    "applications",
    "postgresqls",
    "redis",
    "mongodbs",
    "mysqls",
    "mariadbs",
    "services",
];

pub async fn environment_details(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((uuid, env)): Path<(String, String)>,
) -> ApiResult {
    let project = find_project(&state, &ctx, &uuid).await?;
    let env = find_env(&state, &project, &env).await?;
    let mut record = projects::environment_record(&state.db, env.id).await?;
    for rel in DETAIL_RELATIONS {
        record.insert((*rel).to_string(), Value::Array(vec![]));
    }
    // `exposeSensitiveFields()`: the models themselves, so the `id` stays.
    let apps = coolify_db::applications::list_for_environment(&state.db, env.id).await?;
    let mut items = Vec::with_capacity(apps.len());
    for a in &apps {
        items.push(super::applications::present_as(&state, &ctx, a, false, true).await?);
    }
    record.insert("applications".into(), Value::Array(items));
    for d in coolify_db::databases::list_for_environment(&state.db, env.id).await? {
        let rel = match d.kind {
            coolify_db::databases::Kind::Postgresql => "postgresqls",
            coolify_db::databases::Kind::Redis => "redis",
            coolify_db::databases::Kind::Mongodb => "mongodbs",
            coolify_db::databases::Kind::Mysql => "mysqls",
            coolify_db::databases::Kind::Mariadb => "mariadbs",
            _ => continue,
        };
        let v = super::databases::present_as(&state, &ctx, &d, false, None, true).await?;
        if let Some(Value::Array(list)) = record.get_mut(rel) {
            list.push(v);
        }
    }
    let mut items = Vec::new();
    for s in coolify_db::services::list_for_environment(&state.db, env.id).await? {
        items.push(super::services::present_for_environment(&state, &ctx, &s).await?);
    }
    record.insert("services".into(), Value::Array(items));
    Ok(ok(Value::Object(serialize_record(record))).into_response())
}

pub async fn create(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    require_admin(&ctx)?;
    let input = validated_json(&headers, &uri, &body)?;
    validate(
        &input,
        &[name(true), description()],
        COMBINED_MESSAGES,
        &["name", "description"],
    )?;
    let project = projects::create(
        &state.db,
        ctx.team_id,
        input.str("name"),
        input.str("description"),
        state.now(),
    )
    .await?;
    Ok(created(json!({"uuid": project.uuid})).into_response())
}

pub async fn update(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let input = validated_json(&headers, &uri, &body)?;
    validate(
        &input,
        &[name(false), description()],
        COMBINED_MESSAGES,
        &["name", "description"],
    )?;
    let project = find_project(&state, &ctx, &uuid).await?;
    require_admin(&ctx)?;
    let p = projects::update(
        &state.db,
        &project,
        input.patch_str("name"),
        input.patch_str("description"),
        state.now(),
    )
    .await?;
    // Upstream answers 201 here.
    Ok(
        created(json!({"uuid": p.uuid, "name": p.name, "description": p.description}))
            .into_response(),
    )
}

pub async fn delete(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let project = find_project(&state, &ctx, &uuid).await?;
    require_admin(&ctx)?;
    if !projects::project_is_empty(&state.db, project.id).await? {
        return Err(ApiError::Message(
            StatusCode::BAD_REQUEST,
            "Project has resources, so it cannot be deleted.",
        ));
    }
    projects::delete(&state.db, project.id).await?;
    Ok(ok(json!({"message": "Project deleted."})).into_response())
}

pub async fn create_environment(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let input = validated_json(&headers, &uri, &body)?;
    validate(&input, &[name(true)], NAME_MESSAGES, &["name"])?;
    let project = find_project(&state, &ctx, &uuid).await?;
    require_admin(&ctx)?;
    let env_name = input.str("name").unwrap_or_default();
    if projects::environment_name_taken(&state.db, project.id, env_name, None).await? {
        return Err(ENV_EXISTS);
    }
    let env = projects::create_environment(&state.db, project.id, env_name, state.now())
        .await
        .map_err(env_error)?;
    Ok(created(json!({"uuid": env.uuid})).into_response())
}

pub async fn update_environment(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((uuid, env)): Path<(String, String)>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> Result<Response, ApiError> {
    let input = validated_json(&headers, &uri, &body)?;
    validate(
        &input,
        &[name(false), description()],
        COMBINED_MESSAGES,
        &["name", "description"],
    )?;
    let project = find_project(&state, &ctx, &uuid).await?;
    let env = find_env(&state, &project, &env).await?;
    require_admin(&ctx)?;
    if input.filled("name")
        && let Some(new_name) = input.str("name")
        && new_name != env.name
        && projects::environment_name_taken(&state.db, project.id, new_name, Some(env.id)).await?
    {
        return Err(ENV_EXISTS);
    }
    let e = projects::update_environment(
        &state.db,
        &env,
        input.patch_str("name"),
        input.patch_str("description"),
        state.now(),
    )
    .await
    .map_err(env_error)?;
    Ok(ok(json!({"uuid": e.uuid, "name": e.name, "description": e.description})).into_response())
}

pub async fn delete_environment(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((uuid, env)): Path<(String, String)>,
) -> ApiResult {
    let project = find_project(&state, &ctx, &uuid).await?;
    let env = find_env(&state, &project, &env).await?;
    require_admin(&ctx)?;
    if !projects::environment_is_empty(&state.db, env.id).await? {
        return Err(ApiError::Message(
            StatusCode::BAD_REQUEST,
            "Environment has resources, so it cannot be deleted.",
        ));
    }
    projects::delete_environment(&state.db, env.id).await?;
    Ok(ok(json!({"message": "Environment deleted."})).into_response())
}
