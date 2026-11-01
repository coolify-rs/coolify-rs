//! `Project\Application\Advanced`, the settings half of `Project\Application\Rollback` and
//! `Project\Application\Swarm`.

use std::sync::LazyLock;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use coolify_db::applications as apps;
use coolify_db::servers::{self, ColumnValue as C};
use coolify_jobs::compose_parser::php::slug;
use regex::Regex;
use serde_json::{Map, Value, json};

use super::config::save_model;
use super::{Res, find};
use crate::audit::Meta;
use crate::auth::AuthCtx;
use crate::response::{ApiError, ApiResult, ok};
use crate::state::AppState;
use crate::ui_notifications::{toast_error, unauthorized};

type P = Path<(String, String)>;

pub fn routes() -> Router<AppState> {
    let b = "/ui/resources/{kind}/{uuid}/advanced";
    Router::new()
        .route(b, get(show))
        .route(&format!("{b}/instant"), post(instant))
        .route(&format!("{b}/save"), post(save_field))
        .route(
            "/ui/resources/{kind}/{uuid}/rollback",
            get(rollback_show).post(rollback_save),
        )
}

/// `$this->{property}` <-> `application_settings.{column}`, the listboxes of `instantSave`.
const FLAGS: [&str; 15] = [
    "disable_build_cache",
    "inject_build_args_to_dockerfile",
    "include_source_commit_in_build",
    "is_auto_deploy_enabled",
    "is_git_submodules_enabled",
    "is_git_lfs_enabled",
    "is_git_shallow_clone_enabled",
    "is_raw_compose_deployment_enabled",
    "connect_to_docker_network",
    "is_consistent_container_name_enabled",
    "is_log_drain_enabled",
    "is_gpu_enabled",
    "is_gzip_enabled",
    "is_stripprefix_enabled",
    "is_force_https_enabled",
];

static PREFIX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\A[a-z0-9]+(?:-[a-z0-9]+)*\z").unwrap_or_else(|_| unreachable!())
});
static RESOLVER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z0-9_-]+\n?\z").unwrap_or_else(|_| unreachable!()));
static GPU_COUNT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(all|\d+)\n?\z").unwrap_or_else(|_| unreachable!()));

async fn app_of(state: &AppState, ctx: &AuthCtx, kind: &str, uuid: &str) -> Result<Res, ApiError> {
    let res = find(state, ctx, kind, uuid).await?;
    if !res.is_application() {
        return Err(super::NOT_FOUND);
    }
    Ok(res)
}

async fn audit_settings(state: &AppState, ctx: &AuthCtx, meta: &Meta, res: &Res, names: Vec<&str>) {
    if names.is_empty() {
        return;
    }
    crate::audit::log(
        state,
        ctx,
        meta,
        "ui.application.settings_updated",
        json!({
            "team_id": res.team_id,
            "application_uuid": res.uuid,
            "application_name": res.name,
            "changed_fields": names,
        }),
        "info",
    )
    .await;
}

async fn write_settings(
    state: &AppState,
    ctx: &AuthCtx,
    meta: &Meta,
    res: &Res,
    values: Vec<(&'static str, C)>,
) -> Result<(), ApiError> {
    let names: Vec<&str> = values.iter().map(|(k, _)| *k).collect();
    apps::update_settings(&state.db, res.id, &values, state.now()).await?;
    audit_settings(state, ctx, meta, res, names).await;
    Ok(())
}

async fn show(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid)): P,
) -> ApiResult {
    let res = app_of(&state, &ctx, &kind, &uuid).await?;
    let settings = res.settings.clone().unwrap_or_default();
    let server = match res.server_id {
        Some(id) => servers::find_by_id(&state.db, id).await?,
        None => None,
    };
    let proxy = server
        .as_ref()
        .and_then(|s| s.record.get("proxy"))
        .and_then(|p| {
            p.as_str()
                .and_then(|t| serde_json::from_str::<Value>(t).ok())
                .or_else(|| Some(p.clone()))
        });
    let proxy_type = proxy
        .as_ref()
        .and_then(|p| p.get("type"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_uppercase();
    let server_setting = |k: &str| {
        server
            .as_ref()
            .and_then(|s| s.settings.as_ref())
            .and_then(|s| s.get(k).cloned())
    };
    let resolver = settings
        .get("traefik_certificate_resolver")
        .and_then(Value::as_str)
        .unwrap_or_default();
    Ok(ok(json!({
        "can_update": ctx.is_team_admin(),
        "uuid": res.uuid,
        "git_based": res.git_based(),
        "build_pack": res.str("build_pack"),
        "settings": settings,
        "max_restart_count": res.int("max_restart_count").unwrap_or(0),
        "is_traefik": proxy_type == "TRAEFIK",
        "server_certificate_resolver": server_setting("traefik_certificate_resolver").and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_else(|| "letsencrypt".to_owned()),
        "certificate_resolver_mode": if resolver.is_empty() { "server" } else if resolver == "none" { "none" } else { "custom" },
        "certificate_resolver_name": (!resolver.is_empty() && resolver != "none").then_some(resolver),
        "gpu_servers": gpu_servers(&state, &res, server.as_ref()).await?,
    }))
    .into_response())
}

/// `gpuServers`: `$application->maintenanceServers()` with what `x-server.gpu-list` shows.
async fn gpu_servers(
    state: &AppState,
    res: &Res,
    primary: Option<&servers::ServerRow>,
) -> Result<Vec<Value>, ApiError> {
    let mut rows: Vec<servers::ServerRow> = primary.cloned().into_iter().collect();
    for id in coolify_db::resource_ops::additional_server_ids(&state.db, res.id).await? {
        if !rows.iter().any(|r| r.id == id)
            && let Some(row) = servers::find_by_id(&state.db, id).await?
        {
            rows.push(row);
        }
    }
    Ok(rows
        .iter()
        .map(|s| {
            let metadata = s.record.get("server_metadata").and_then(|m| match m {
                Value::String(t) => serde_json::from_str::<Value>(t).ok(),
                other => Some(other.clone()),
            });
            let field = |k: &str| metadata.as_ref().and_then(|m| m.get(k)).cloned();
            json!({
                "uuid": s.uuid,
                "name": s.record.get("name"),
                "is_functional": coolify_jobs::server::ServerInfo::from_row(s).is_functional(),
                "collected_at": field("collected_at"),
                "gpus": field("gpus"),
                "nvidia_container_toolkit": field("nvidia_container_toolkit"),
            })
        })
        .collect())
}

/// `Advanced::instantSave`.
async fn instant(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path((kind, uuid)): P,
    Json(body): Json<Value>,
) -> ApiResult {
    let res = app_of(&state, &ctx, &kind, &uuid).await?;
    if !ctx.is_team_admin() {
        return Ok(unauthorized());
    }
    let mut values: Vec<(&'static str, C)> = Vec::new();
    let mut reset = false;
    for k in FLAGS {
        let Some(b) = body.get(k).and_then(Value::as_bool) else {
            continue;
        };
        if k == "is_log_drain_enabled" && b {
            let enabled = match res.server_id {
                Some(id) => servers::find_by_id(&state.db, id)
                    .await?
                    .and_then(|s| s.settings)
                    .is_some_and(|s| {
                        [
                            "is_logdrain_newrelic_enabled",
                            "is_logdrain_highlight_enabled",
                            "is_logdrain_axiom_enabled",
                            "is_logdrain_custom_enabled",
                        ]
                        .iter()
                        .any(|c| s.get(*c).and_then(Value::as_bool).unwrap_or(false))
                    }),
                None => false,
            };
            if !enabled {
                return Ok(toast_error(
                    StatusCode::CONFLICT,
                    "Log drain is not enabled on this server.",
                ));
            }
        }
        if res.setting(k).and_then(Value::as_bool) != Some(b) {
            if [
                "is_force_https_enabled",
                "is_gzip_enabled",
                "is_stripprefix_enabled",
            ]
            .contains(&k)
            {
                reset = true;
            }
            values.push((k, C::Bool(b)));
        }
    }
    write_settings(&state, &ctx, &meta, &res, values).await?;
    if reset {
        crate::ui_applications::regenerate_labels(&state, res.id, false).await?;
    }
    Ok(ok(json!({"message": "Settings saved."})).into_response())
}

/// The forms of the page: `saveCustomName`, `saveCustomNamePrefix`, `saveCertificateResolver`,
/// `saveStopGracePeriod`, `saveRollingUpdateDrainSeconds`, `saveMaxRestartCount` and `submit`.
async fn save_field(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path((kind, uuid)): P,
    Json(body): Json<Value>,
) -> ApiResult {
    let res = app_of(&state, &ctx, &kind, &uuid).await?;
    if !ctx.is_team_admin() {
        return Ok(unauthorized());
    }
    let form = body.get("form").and_then(Value::as_str).unwrap_or_default();
    let text = |k: &str| {
        body.get(k).and_then(|v| match v {
            Value::String(s) => Some(s.clone()),
            Value::Number(n) => Some(n.to_string()),
            _ => None,
        })
    };
    match form {
        "custom_name" => {
            let name = text("customInternalName")
                .filter(|v| !v.is_empty())
                .map(|v| slug(&v));
            if let (Some(n), Some(server)) = (&name, res.server_id) {
                let used: bool = sqlx::query_scalar(
                    "SELECT EXISTS (SELECT 1 FROM application_settings s JOIN applications a ON a.id = s.application_id \
                     JOIN standalone_dockers d ON a.destination_id = d.id AND a.destination_type = 'App\\Models\\StandaloneDocker' \
                     WHERE d.server_id = $1 AND a.id <> $2 AND a.deleted_at IS NULL AND s.custom_internal_name = $3)",
                )
                .bind(server)
                .bind(res.id)
                .bind(n)
                .fetch_one(state.db.pool())
                .await
                .map_err(coolify_db::DbError::from)?;
                if used {
                    return Ok(toast_error(
                        StatusCode::CONFLICT,
                        "This custom container name is already in use by another application on this server.",
                    ));
                }
            }
            let mut values = Vec::new();
            if res.setting("custom_internal_name").and_then(Value::as_str) != name.as_deref() {
                values.push(("custom_internal_name", C::Text(name.clone())));
            }
            write_settings(&state, &ctx, &meta, &res, values).await?;
            Ok(ok(json!({"message": "Custom name saved.", "value": name})).into_response())
        }
        "name_prefix" => {
            let prefix = text("customContainerNamePrefix")
                .map(|v| slug(&v))
                .filter(|v| !v.is_empty());
            if let Some(p) = &prefix {
                if res
                    .setting("custom_container_name_prefix")
                    .and_then(Value::as_str)
                    != Some(p)
                    && !PREFIX.is_match(p)
                {
                    return Ok(toast_error(
                        StatusCode::UNPROCESSABLE_ENTITY,
                        "The container name prefix may only contain lowercase letters, numbers, and single hyphens between them.",
                    ));
                }
                if let Some(server) = res.server_id {
                    let used: bool = sqlx::query_scalar(
                        "SELECT EXISTS (SELECT 1 FROM application_settings s JOIN applications a ON a.id = s.application_id \
                         JOIN standalone_dockers d ON a.destination_id = d.id AND a.destination_type = 'App\\Models\\StandaloneDocker' \
                         WHERE d.server_id = $1 AND a.id <> $2 AND a.deleted_at IS NULL AND (a.uuid = $3 OR s.custom_container_name_prefix = $3 OR s.custom_internal_name = $3))",
                    )
                    .bind(server)
                    .bind(res.id)
                    .bind(p)
                    .fetch_one(state.db.pool())
                    .await
                    .map_err(coolify_db::DbError::from)?;
                    if used {
                        return Ok(toast_error(
                            StatusCode::CONFLICT,
                            "This container name prefix is already in use by another application on this Coolify instance.",
                        ));
                    }
                }
            }
            let mut values = Vec::new();
            if res
                .setting("custom_container_name_prefix")
                .and_then(Value::as_str)
                != prefix.as_deref()
            {
                values.push(("custom_container_name_prefix", C::Text(prefix.clone())));
            }
            write_settings(&state, &ctx, &meta, &res, values).await?;
            Ok(
                ok(json!({"message": "Container name prefix saved.", "value": prefix}))
                    .into_response(),
            )
        }
        "resolver" => {
            let mode = text("certificateResolverMode").unwrap_or_default();
            let name = text("certificateResolverName").unwrap_or_default();
            let new = match mode.as_str() {
                "none" => Some("none".to_owned()),
                "custom" => {
                    let message = if name.is_empty() {
                        Some("The resolver name field is required.")
                    } else if name == "none" {
                        Some("Choose \"None\" instead of a custom resolver named \"none\".")
                    } else if !RESOLVER.is_match(&name) {
                        Some(
                            "The resolver name may only contain letters, numbers, hyphens, and underscores.",
                        )
                    } else {
                        None
                    };
                    if let Some(m) = message {
                        return Ok(crate::response::PhpJson(
                            StatusCode::UNPROCESSABLE_ENTITY,
                            json!({"message": m, "errors": {"certificateResolverName": [m]}}),
                        )
                        .into_response());
                    }
                    Some(name)
                }
                _ => None,
            };
            let changed = res
                .setting("traefik_certificate_resolver")
                .and_then(Value::as_str)
                != new.as_deref();
            let values = if changed {
                vec![("traefik_certificate_resolver", C::Text(new))]
            } else {
                Vec::new()
            };
            write_settings(&state, &ctx, &meta, &res, values).await?;
            if changed {
                crate::ui_applications::regenerate_labels(&state, res.id, false).await?;
            }
            Ok(ok(json!({"message": "Certificate resolver saved."})).into_response())
        }
        "stop_grace" => {
            let v = text("stopGracePeriod").filter(|v| !v.is_empty());
            let parsed = match &v {
                None => None,
                Some(s) => match s.parse::<i64>() {
                    Err(_) => {
                        return Ok(field_error(
                            "stopGracePeriod",
                            "The stop grace period field must be an integer.",
                        ));
                    }
                    Ok(n) if n < 1 => {
                        return Ok(field_error(
                            "stopGracePeriod",
                            "The stop grace period field must be at least 1.",
                        ));
                    }
                    Ok(n) if n > 3600 => {
                        return Ok(field_error(
                            "stopGracePeriod",
                            "The stop grace period field must not be greater than 3600.",
                        ));
                    }
                    Ok(n) => Some(n),
                },
            };
            let mut values = Vec::new();
            if res.setting("stop_grace_period").and_then(Value::as_i64) != parsed {
                values.push(("stop_grace_period", parsed.map_or(C::Text(None), C::Int)));
            }
            write_settings(&state, &ctx, &meta, &res, values).await?;
            Ok(ok(json!({"message": "Stop grace period updated."})).into_response())
        }
        "drain" => {
            let n = match text("rollingUpdateDrainSeconds").filter(|v| !v.is_empty()) {
                None => {
                    return Ok(field_error(
                        "rollingUpdateDrainSeconds",
                        "The rolling update drain time field is required.",
                    ));
                }
                Some(s) => match s.parse::<i64>() {
                    Err(_) => {
                        return Ok(field_error(
                            "rollingUpdateDrainSeconds",
                            "The rolling update drain time field must be an integer.",
                        ));
                    }
                    Ok(n) if n < 0 => {
                        return Ok(field_error(
                            "rollingUpdateDrainSeconds",
                            "The rolling update drain time field must be at least 0.",
                        ));
                    }
                    Ok(n) if n > 300 => {
                        return Ok(field_error(
                            "rollingUpdateDrainSeconds",
                            "The rolling update drain time field must not be greater than 300.",
                        ));
                    }
                    Ok(n) => n,
                },
            };
            let mut values = Vec::new();
            if res
                .setting("rolling_update_drain_seconds")
                .and_then(Value::as_i64)
                != Some(n)
            {
                values.push(("rolling_update_drain_seconds", C::Int(n)));
            }
            write_settings(&state, &ctx, &meta, &res, values).await?;
            Ok(ok(json!({"message": "Rolling update drain time updated."})).into_response())
        }
        "max_restart" => {
            let n = text("maxRestartCount").unwrap_or_default();
            let n = match n.parse::<i64>() {
                Err(_) => {
                    return Ok(toast_error(
                        StatusCode::UNPROCESSABLE_ENTITY,
                        "The max restart count field must be an integer.",
                    ));
                }
                Ok(n) if n < 0 => {
                    return Ok(toast_error(
                        StatusCode::UNPROCESSABLE_ENTITY,
                        "The max restart count field must be at least 0.",
                    ));
                }
                Ok(n) => n,
            };
            let mut values = Vec::new();
            let mut shown = Map::new();
            if res.int("max_restart_count") != Some(n) {
                shown.insert("max_restart_count".into(), json!(n));
                values.push(("max_restart_count", C::Int(n)));
            }
            save_model(&state, &ctx, &meta, &res, values, &shown).await?;
            Ok(ok(json!({"message": "Max restart count saved."})).into_response())
        }
        "gpu" => {
            let count = text("gpuCount").filter(|v| !v.is_empty());
            let ids = text("gpuDeviceIds").filter(|v| !v.is_empty());
            let options = text("gpuOptions").filter(|v| !v.is_empty());
            let driver = text("gpuDriver").unwrap_or_default();
            let cur = |k: &str| res.setting(k).and_then(Value::as_str).map(str::to_owned);
            if (count != cur("gpu_count") || ids != cur("gpu_device_ids"))
                && count.is_some()
                && ids.is_some()
            {
                return Ok(toast_error(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "You cannot set both GPU count and GPU device IDs.",
                ));
            }
            if count != cur("gpu_count")
                && count
                    .as_deref()
                    .is_some_and(|c| !GPU_COUNT.is_match(c.trim()))
            {
                return Ok(toast_error(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "GPU count must be a number or \"all\".",
                ));
            }
            let mut values = Vec::new();
            for (k, v) in [
                ("gpu_driver", Some(driver)),
                ("gpu_count", count),
                ("gpu_device_ids", ids),
                ("gpu_options", options),
            ] {
                if cur(k) != v {
                    values.push((k, C::Text(v)));
                }
            }
            write_settings(&state, &ctx, &meta, &res, values).await?;
            Ok(ok(json!({"message": "Settings saved."})).into_response())
        }
        _ => Ok(toast_error(StatusCode::BAD_REQUEST, "Unknown form.")),
    }
}

fn field_error(field: &str, message: &str) -> axum::response::Response {
    crate::response::PhpJson(
        StatusCode::UNPROCESSABLE_ENTITY,
        json!({"message": message, "errors": {field: [message]}}),
    )
    .into_response()
}

// ---------------------------------------------------------------------------
// Rollback
// ---------------------------------------------------------------------------

/// `GetRollbackImages` for the images on the server.
async fn rollback_show(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid)): P,
    uri: axum::http::Uri,
) -> ApiResult {
    // `mount()` without `loadImages`, which the page calls after its first render.
    let settings_only = uri.query().is_some_and(|q| q.contains("settings=1"));
    let res = app_of(&state, &ctx, &kind, &uuid).await?;
    let server = match res.server_id {
        Some(id) => servers::find_by_id(&state.db, id).await?,
        None => None,
    };
    let retention_disabled = server
        .as_ref()
        .and_then(|s| s.settings.as_ref())
        .and_then(|s| s.get("disable_application_image_retention"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let keep = res
        .setting("docker_images_to_keep")
        .and_then(Value::as_i64)
        .unwrap_or(2);
    let mut images: Vec<Value> = Vec::new();
    if let (Some(s), Some(jobs), false) = (&server, state.jobs.as_ref(), settings_only) {
        let image = res
            .str("docker_registry_image_name")
            .filter(|v| !v.is_empty())
            .map_or_else(|| res.uuid.clone(), str::to_owned);
        let out = jobs
            .instant_remote(
                s,
                &["docker images --format '{{.Repository}}#{{.Tag}}#{{.CreatedAt}}'".to_owned()],
                false,
                "application.rollback.images",
            )
            .await
            .ok()
            .flatten()
            .unwrap_or_default();
        let current = jobs
            .instant_remote(
                s,
                &[format!("docker ps --filter 'label=coolify.applicationUuid={}' --format '{{{{.Image}}}}'", res.uuid)],
                false,
                "application.rollback.current",
            )
            .await
            .ok()
            .flatten()
            .unwrap_or_default();
        let current_tag = current
            .lines()
            .next()
            .and_then(|i| i.rsplit_once(':'))
            .map(|(_, t)| t.to_owned());
        for line in out.lines() {
            let parts: Vec<&str> = line.split('#').collect();
            // Exact repository match only, so other images with a similar name are not listed.
            if parts.len() < 3 || parts[0] != image || parts[1] == "<none>" {
                continue;
            }
            images.push(json!({
                "tag": parts[1],
                "created_at": parts[2],
                "is_current": current_tag.as_deref() == Some(parts[1]),
                "source": "server",
            }));
        }
    }
    Ok(ok(json!({
        "can_update": ctx.is_team_admin(),
        "docker_images_to_keep": keep,
        "server_retention_disabled": retention_disabled,
        "images": images,
    }))
    .into_response())
}

/// `Rollback::saveSettings`.
async fn rollback_save(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path((kind, uuid)): P,
    Json(body): Json<Value>,
) -> ApiResult {
    let res = app_of(&state, &ctx, &kind, &uuid).await?;
    if !ctx.is_team_admin() {
        return Ok(unauthorized());
    }
    let raw = body.get("dockerImagesToKeep").map(|v| match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    });
    let n = match raw.unwrap_or_default().parse::<i64>() {
        Err(_) => {
            return Ok(toast_error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "The docker images to keep field must be an integer.",
            ));
        }
        Ok(n) if n < 0 => {
            return Ok(toast_error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "The docker images to keep field must be at least 0.",
            ));
        }
        Ok(n) if n > 100 => {
            return Ok(toast_error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "The docker images to keep field must not be greater than 100.",
            ));
        }
        Ok(n) => n,
    };
    let mut values = Vec::new();
    if res.setting("docker_images_to_keep").and_then(Value::as_i64) != Some(n) {
        values.push(("docker_images_to_keep", C::Int(n)));
    }
    write_settings(&state, &ctx, &meta, &res, values).await?;
    Ok(ok(json!({"message": "Settings saved."})).into_response())
}
