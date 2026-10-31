//! The Compose part of `Application\General` and `Application\Heading`: `loadComposeFile()` and the
//! `getDockerCompose{Build,Start}CommandPreviewProperty()` computed fields.

use axum::Extension;
use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use coolify_jobs::applications::{inject_build_args, inject_compose_flags};
use coolify_jobs::compose_parser::{application_parser, load_compose_file};
use serde_json::{Value, json};

use super::find;
use crate::auth::AuthCtx;
use crate::response::{ApiError, ApiResult, ok};
use crate::state::AppState;

/// `ApplicationDeploymentJob::BUILD_TIME_ENV_PATH`.
const BUILD_TIME_ENV_PATH: &str = concat!("/artifacts/build-time.", "env");

/// `Heading::loadComposeFile` and `General::loadComposeFile`.
pub async fn load(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult {
    let a = find(&state, &ctx, &uuid).await?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    // `$isInit && ! $this->needsComposeFile()`.
    let is_init = body
        .get("is_init")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if is_init
        && (a.str("build_pack") != Some("dockercompose") || a.str("docker_compose_raw").is_some())
    {
        return Ok(ok(json!({})).into_response());
    }
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    if let Err(e) = load_compose_file(jobs, a.id, false).await {
        return Ok((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({"message": e.message()})),
        )
            .into_response());
    }
    Ok(ok(json!({"message": "Docker compose file loaded."})).into_response())
}

/// `General::mount`: `$this->application->parse()`, which saves the deployable file on every visit,
/// and `redactLockedValuesInGeneratedCompose()` for what the form shows.
pub async fn parse(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let a = find(&state, &ctx, &uuid).await?;
    if a.str("build_pack") != Some("dockercompose") || a.str("docker_compose_raw").is_none() {
        return Ok(ok(json!({})).into_response());
    }
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    let parsed = application_parser(jobs, a.id, 0, None).await;
    if let Err(message) = &parsed {
        return Ok(ok(json!({"error": message})).into_response());
    }
    if !ctx.is_team_admin() {
        return Ok(ok(json!({})).into_response());
    }
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT row_to_json(v)::text FROM environment_variables v \
         WHERE v.resourceable_type = $1 AND v.resourceable_id = $2",
    )
    .bind(coolify_db::applications::MORPH)
    .bind(a.id)
    .fetch_all(state.db.pool())
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "variables could not be read");
        ApiError::Internal
    })?;
    let rows: Vec<_> = rows
        .iter()
        .filter_map(|t| coolify_db::records::parse_row(t).ok())
        .map(|r| {
            let value = r
                .get("value")
                .and_then(Value::as_str)
                .filter(|v| !v.is_empty())
                .and_then(|v| enc.decrypt_maybe_serialized(v).ok());
            (r, value)
        })
        .collect();
    let fresh = find(&state, &ctx, &uuid).await?;
    Ok(ok(json!({
        "docker_compose": crate::ui_services::redact_locked(fresh.str("docker_compose"), &rows),
    }))
    .into_response())
}

/// The relative compose path of a preview: `.{$normalizedBase}{$this->dockerComposeLocation}`.
fn compose_file(base: &str, location: &str) -> String {
    let normalized = if base == "/" {
        ""
    } else {
        base.trim_end_matches('/')
    };
    format!(".{normalized}{location}")
}

/// The two computed preview fields, for the values currently in the form.
pub async fn previews(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult {
    let a = find(&state, &ctx, &uuid).await?;
    let text = |k: &str| body.get(k).and_then(Value::as_str).unwrap_or_default();
    let file = compose_file(text("base_directory"), text("docker_compose_location"));
    let mut build = String::new();
    if !text("build").is_empty() {
        build = inject_compose_flags(text("build"), &file, BUILD_TIME_ENV_PATH);
        if !a.setting_bool("use_build_secrets") {
            let keys: Vec<String> = sqlx::query_scalar(
                "SELECT key FROM environment_variables WHERE resourceable_type = $1 AND resourceable_id = $2 \
                 AND is_buildtime AND NOT is_preview ORDER BY key",
            )
            .bind(coolify_db::applications::MORPH)
            .bind(a.id)
            .fetch_all(state.db.pool())
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "build variables could not be read");
                ApiError::Internal
            })?;
            if !keys.is_empty() {
                let args = keys
                    .iter()
                    .map(|k| format!("--build-arg {}", coolify_remote::shell::quote(k)))
                    .collect::<Vec<_>>()
                    .join(" ");
                build = inject_build_args(&build, &args);
            }
        }
    }
    let start = if text("start").is_empty() {
        String::new()
    } else {
        inject_compose_flags(text("start"), &file, concat!("{workdir}/.", "env"))
    };
    Ok(ok(json!({"build": build, "start": start})).into_response())
}
