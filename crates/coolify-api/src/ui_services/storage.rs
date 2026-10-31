//! `Project\Service\Storage` (the service stack part) and `Project\Service\FileStorage`.

use axum::Extension;
use axum::extract::{Path, State};
use axum::response::IntoResponse;
use coolify_db::records::Record;
use coolify_db::servers::ColumnValue;
use coolify_db::services::{self as svc, ServiceRow, SubKind, SubRow};
use coolify_jobs::services::storage as job_storage;
use coolify_jobs::services::ui_ops::{BINARY_PLACEHOLDER, MAX_CONTENT_SIZE, TOO_LARGE_PLACEHOLDER};
use serde_json::{Value, json};

use super::{find_service, jobs_of, require_admin, toast};
use crate::auth::AuthCtx;
use crate::response::{ApiError, ApiResult, ok};
use crate::state::AppState;

const FILE_VOLUME_MORPH: &str = r"App\Models\LocalFileVolume";

/// `$fileStorage->shouldBeReadOnlyInUI()`, `canDeleteFromServer()` and the appended attributes.
async fn file_json(
    state: &AppState,
    ctx: &AuthCtx,
    s: &ServiceRow,
    part: &SubRow,
    row: &Record,
) -> Result<Value, ApiError> {
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let text = |k: &str| row.get(k).and_then(Value::as_str).unwrap_or_default();
    let flag = |k: &str| row.get(k).and_then(Value::as_bool).unwrap_or(false);
    let id = row.get("id").and_then(Value::as_i64).unwrap_or_default();
    let content = row
        .get("content")
        .and_then(Value::as_str)
        .and_then(|c| enc.decrypt_string(c).ok());
    // `loadFileStorageForActiveTab`: the placeholder instead of a large stored content.
    let content = content.map(|c| {
        if c.len() > MAX_CONTENT_SIZE {
            TOO_LARGE_PLACEHOLDER.to_owned()
        } else {
            c
        }
    });
    let is_too_large = content.as_deref() == Some(TOO_LARGE_PLACEHOLDER);
    let is_binary = content.as_deref() == Some(BINARY_PLACEHOLDER);
    let dir = coolify_jobs::services::workdir(&s.uuid);
    let read_only =
        job_storage::file_is_read_only(s.str("docker_compose_raw"), &part.name, text("mount_path"));
    let fs_path = text("fs_path");
    let outside = job_storage::is_outside_resource_directory(fs_path, &dir);
    let confirm_path = if fs_path.starts_with('.') {
        fs_path.split_once('.').map_or(fs_path, |x| x.1)
    } else {
        fs_path
    };
    let backup = svc::storage_has_backups(&state.db, FILE_VOLUME_MORPH, id).await?;
    Ok(json!({
        "id": id,
        "fs_path": fs_path,
        "confirm_path": confirm_path,
        "mount_path": text("mount_path"),
        "is_directory": flag("is_directory"),
        "is_host_file": flag("is_host_file"),
        "is_based_on_git": flag("is_based_on_git"),
        "is_read_only": read_only || is_too_large,
        "is_too_large": is_too_large,
        "is_binary": is_binary,
        "has_backup": backup,
        "can_delete_from_server": !flag("is_host_file") && !outside,
        "content": if ctx.is_team_admin() && !flag("is_directory") { json!(content) } else { Value::Null },
    }))
}

/// `composeExternalVolumeMounts()` of one part, without the volumes that still have a storage entry.
fn external_volumes(raw: Option<&str>, part: &str, uuid: &str, storages: &[Record]) -> Vec<Value> {
    let names: Vec<&str> = storages
        .iter()
        .filter_map(|r| r.get("name").and_then(Value::as_str))
        .collect();
    let mut out = Vec::new();
    for (key, docker_name, service, paths) in job_storage::compose_external_volume_mounts(raw) {
        let generated = format!("{uuid}_{}", coolify_jobs::compose_parser::php::slug(&key));
        if service == part && !names.contains(&generated.as_str()) {
            out.push(json!({"key": key, "dockerName": docker_name, "service": service, "mountPaths": paths}));
        }
    }
    out
}

async fn part_json(
    state: &AppState,
    ctx: &AuthCtx,
    s: &ServiceRow,
    part: &SubRow,
) -> Result<Value, ApiError> {
    let persistent = svc::persistent_storages(&state.db, part.kind, part.id).await?;
    let files_rows = svc::file_storages(&state.db, part.kind, part.id).await?;
    let mut files = Vec::new();
    let mut directories = Vec::new();
    for row in &files_rows {
        let is_directory = row
            .get("is_directory")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let v = file_json(state, ctx, s, part, row).await?;
        if is_directory {
            directories.push(v);
        } else {
            files.push(v);
        }
    }
    Ok(json!({
        "id": part.id,
        "uuid": part.uuid,
        "name": part.name,
        "kind": if part.is_application() { "application" } else { "database" },
        "volume_count": persistent.len(),
        "files": files,
        "directories": directories,
        "external_volumes": external_volumes(s.str("docker_compose_raw"), &part.name, &s.uuid, &persistent),
    }))
}

/// The storage sections of the configuration page: applications, then databases.
pub async fn index(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let s = find_service(&state, &ctx, &uuid).await?;
    let mut parts = Vec::new();
    for kind in [SubKind::Application, SubKind::Database] {
        let mut rows = svc::subs(&state.db, kind, s.id).await?;
        rows.sort_by_key(|r| r.id);
        for part in &rows {
            parts.push(part_json(&state, &ctx, &s, part).await?);
        }
    }
    Ok(ok(json!({"parts": parts})).into_response())
}

/// The mount, checked to belong to a part of this service.
async fn owned_file(
    state: &AppState,
    s: &ServiceRow,
    id: i64,
) -> Result<(Record, SubRow), ApiError> {
    for kind in [SubKind::Application, SubKind::Database] {
        for part in svc::subs(&state.db, kind, s.id).await? {
            for row in svc::file_storages(&state.db, kind, part.id).await? {
                if row.get("id").and_then(Value::as_i64) == Some(id) {
                    return Ok((row, part));
                }
            }
        }
    }
    Err(ApiError::Message(
        axum::http::StatusCode::NOT_FOUND,
        "Storage not found.",
    ))
}

/// `FileStorage::submit`.
pub async fn file_save(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((uuid, id)): Path<(String, i64)>,
    body: axum::body::Bytes,
) -> ApiResult {
    let s = find_service(&state, &ctx, &uuid).await?;
    require_admin(&ctx)?;
    let (row, _) = owned_file(&state, &s, id).await?;
    let flag = |k: &str| row.get(k).and_then(Value::as_bool).unwrap_or(false);
    if flag("is_host_file") {
        return Ok(toast(
            "Host file mounts are bind-only and cannot be edited from the UI.",
        ));
    }
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let current = row
        .get("content")
        .and_then(Value::as_str)
        .and_then(|c| enc.decrypt_string(c).ok());
    if current.as_deref() == Some(TOO_LARGE_PLACEHOLDER) {
        return Ok(toast("File on server is too large to edit from the UI."));
    }
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let content = if flag("is_directory") {
        None
    } else {
        body.get("content")
            .and_then(Value::as_str)
            .map(str::to_owned)
    };
    let raw = row
        .get("content")
        .and_then(Value::as_str)
        .map(str::to_owned);
    svc::storage_update(
        &state.db,
        "local_file_volumes",
        id,
        &[(
            "content",
            ColumnValue::Text(content.as_deref().map(|c| enc.encrypt_string(c))),
        )],
        state.now(),
    )
    .await?;
    let jobs = jobs_of(&state)?;
    let saved = match jobs.service_target(&s).await {
        Ok((_, t)) => job_storage::save_storage_on_server(jobs, &s, &t, id).await,
        Err(e) => Err(e),
    };
    if let Err(e) = saved {
        // `setRawAttributes($original); save()`.
        svc::storage_update(
            &state.db,
            "local_file_volumes",
            id,
            &[("content", ColumnValue::Text(raw))],
            state.now(),
        )
        .await?;
        return Ok(toast(e));
    }
    Ok(ok(json!({"message": "File updated."})).into_response())
}

/// `FileStorage::loadStorageOnServer`.
pub async fn file_load(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((uuid, id)): Path<(String, i64)>,
) -> ApiResult {
    let s = find_service(&state, &ctx, &uuid).await?;
    require_admin(&ctx)?;
    let (row, _) = owned_file(&state, &s, id).await?;
    if row
        .get("is_host_file")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Ok(toast(
            "Host file mounts are bind-only and cannot be loaded from the server.",
        ));
    }
    let jobs = jobs_of(&state)?;
    let loaded = match jobs.service_target(&s).await {
        Ok((_, t)) => jobs.load_service_storage(&s, &t, id).await,
        Err(e) => Err(e),
    };
    if let Err(e) = loaded {
        return Ok(toast(e));
    }
    Ok(ok(json!({"message": "File storage loaded from server."})).into_response())
}

/// `FileStorage::convertToDirectory` and `convertToFile`.
pub async fn file_convert(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((uuid, id)): Path<(String, i64)>,
    body: axum::body::Bytes,
) -> ApiResult {
    let s = find_service(&state, &ctx, &uuid).await?;
    require_admin(&ctx)?;
    let (row, _) = owned_file(&state, &s, id).await?;
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let to_directory = body.get("to") == Some(&json!("directory"));
    if !to_directory && svc::storage_has_backups(&state.db, FILE_VOLUME_MORPH, id).await? {
        return Ok(toast(
            "Delete this directory backup schedule and its archives before converting it to a file.",
        ));
    }
    if row
        .get("is_host_file")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Ok(toast(
            "Host file mounts are bind-only and cannot be converted.",
        ));
    }
    let jobs = jobs_of(&state)?;
    let (_, t) = match jobs.service_target(&s).await {
        Ok(v) => v,
        Err(e) => return Ok(toast(e)),
    };
    if let Err(e) = job_storage::delete_storage_on_server(jobs, &s, &t, id).await {
        return Ok(toast(e));
    }
    let mut values = vec![
        ("is_directory", ColumnValue::Bool(to_directory)),
        ("content", ColumnValue::Text(None)),
    ];
    if to_directory {
        values.push(("is_based_on_git", ColumnValue::Bool(false)));
    }
    svc::storage_update(&state.db, "local_file_volumes", id, &values, state.now()).await?;
    if let Err(e) = job_storage::save_storage_on_server(jobs, &s, &t, id).await {
        // `$this->fileStorage->refresh()`: the row keeps what it held before the conversion.
        let restore = vec![
            (
                "is_directory",
                ColumnValue::Bool(
                    row.get("is_directory")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                ),
            ),
            (
                "content",
                ColumnValue::Text(
                    row.get("content")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                ),
            ),
            (
                "is_based_on_git",
                ColumnValue::Bool(
                    row.get("is_based_on_git")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                ),
            ),
        ];
        svc::storage_update(&state.db, "local_file_volumes", id, &restore, state.now()).await?;
        return Ok(toast(e));
    }
    Ok(ok(json!({"message": "converted"})).into_response())
}

/// `FileStorage::delete`.
pub async fn file_delete(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((uuid, id)): Path<(String, i64)>,
    body: axum::body::Bytes,
) -> ApiResult {
    let s = find_service(&state, &ctx, &uuid).await?;
    require_admin(&ctx)?;
    let (row, _) = owned_file(&state, &s, id).await?;
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    if !crate::ui::password_confirmed(
        &state,
        ctx.user_id,
        body.get("password").and_then(Value::as_str),
    )
    .await?
    {
        return Err(ApiError::Message(
            axum::http::StatusCode::UNPROCESSABLE_ENTITY,
            "The provided password is incorrect.",
        ));
    }
    if svc::storage_has_backups(&state.db, FILE_VOLUME_MORPH, id).await? {
        return Ok(toast(
            "Delete this directory backup schedule and its archives before deleting the directory.",
        ));
    }
    let flag = |k: &str| row.get(k).and_then(Value::as_bool).unwrap_or(false);
    let dir = coolify_jobs::services::workdir(&s.uuid);
    let fs_path = row
        .get("fs_path")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let can_delete =
        !flag("is_host_file") && !job_storage::is_outside_resource_directory(fs_path, &dir);
    let from_server = can_delete
        && body
            .get("selected")
            .and_then(Value::as_array)
            .is_some_and(|a| a.iter().any(|v| v == "permanently_delete"));
    let mut message = if flag("is_directory") {
        "Directory deleted."
    } else if flag("is_host_file") {
        "Host file mount removed."
    } else {
        "File deleted."
    };
    if from_server {
        message = if flag("is_directory") {
            "Directory deleted from the server."
        } else {
            "File deleted from the server."
        };
        let jobs = jobs_of(&state)?;
        let deleted = match jobs.service_target(&s).await {
            Ok((_, t)) => job_storage::delete_storage_on_server(jobs, &s, &t, id).await,
            Err(e) => Err(e),
        };
        if let Err(e) = deleted {
            return Ok(toast(e));
        }
    }
    svc::storage_delete(&state.db, "local_file_volumes", id).await?;
    Ok(ok(json!({"message": message})).into_response())
}
