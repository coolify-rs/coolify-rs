//! `DatabasesController`: storages of a database (`storages`, `create_storage`,
//! `update_storage`, `delete_storage`). Application storages are ported separately
//! and unified at merge.

use std::sync::LazyLock;

use axum::Extension;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::IntoResponse;
use coolify_db::database_parts::{
    self as parts, LOCAL_FILE_VOLUME, LOCAL_FILE_VOLUME_MORPH, LOCAL_PERSISTENT_VOLUME,
    LOCAL_PERSISTENT_VOLUME_MORPH, NewFileVolume,
};
use coolify_db::databases::{self as dbs, DatabaseRow, Kind};
use coolify_db::records::Record;
use coolify_db::servers::{self, ColumnValue};
use coolify_domain::Ability;
use coolify_jobs::databases::paths;
use coolify_jobs::server::ServerInfo;
use regex::Regex;
use serde_json::{Map, Value, json};

use crate::auth::AuthCtx;
use crate::input::validated_json;
use crate::response::{ApiError, ApiResult, created, ok};
use crate::routes::databases::{NOT_FOUND, input_bool};
use crate::state::AppState;
use crate::validation::{CUSTOM_API_MESSAGES, Field, Rule, validate};

static VOLUME_NAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[a-zA-Z0-9][a-zA-Z0-9._-]*\n?\z").unwrap_or_else(|_| unreachable!())
});

const CONFIGURATION_DIR: &str = "/data/coolify/databases";
const WRITE_CONTENT_REASON: &str = "Missing required permissions: deploy. Coolify writes the file content on the server immediately, so it needs a token with the deploy permission.";
const OUTSIDE_REASON: &str = "Missing required permissions: deploy. A mount outside the resource directory needs a token with the deploy permission.";

fn can_deploy(ctx: &AuthCtx) -> bool {
    ctx.abilities.can(Ability::Deploy) || ctx.abilities.can(Ability::Root)
}

fn validation(field: &str, message: impl Into<String>) -> ApiError {
    let mut m = Map::new();
    m.insert(field.into(), Value::String(message.into()));
    ApiError::Validation(m)
}

async fn find(state: &AppState, ctx: &AuthCtx, uuid: &str) -> Result<DatabaseRow, ApiError> {
    dbs::find_in_team(&state.db, ctx.team_id, uuid)
        .await?
        .ok_or(NOT_FOUND)
}

/// A file volume's `toArray()`: `is_binary`, `is_too_large`, `content` for sensitive tokens.
fn present_file(state: &AppState, ctx: &AuthCtx, row: &Record) -> Value {
    let mut r = LOCAL_FILE_VOLUME.present(row.clone());
    let plain = parts::str_of(row, "content").and_then(|c| {
        state
            .encrypter
            .as_ref()
            .and_then(|e| e.decrypt_string(c).ok())
    });
    if ctx.can_read_sensitive {
        r.insert(
            "content".into(),
            plain.clone().map_or(Value::Null, Value::String),
        );
    } else {
        r.shift_remove("content");
    }
    r.insert(
        "is_binary".into(),
        Value::Bool(plain.as_deref() == Some("[binary file]")),
    );
    r.insert(
        "is_too_large".into(),
        Value::Bool(plain.as_deref() == Some("[file too large to display]")),
    );
    Value::Object(r)
}

fn present_persistent(row: &Record) -> Value {
    Value::Object(LOCAL_PERSISTENT_VOLUME.present(row.clone()))
}

/// The attributes of a model that was just created, in assignment order.
fn created_model(mut full: Map<String, Value>, order: &[&str]) -> Value {
    let mut out = Map::new();
    for k in order {
        if let Some(v) = full.shift_remove(*k) {
            out.insert((*k).into(), v);
        }
    }
    Value::Object(out)
}

/// `GET /databases/{uuid}/storages`.
pub async fn list(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let d = find(&state, &ctx, &uuid).await?;
    let persistent = parts::persistent_storages(&state.db, d.kind.morph(), d.id).await?;
    let files = parts::file_storages(&state.db, d.kind.morph(), d.id).await?;
    Ok(ok(json!({
        "persistent_storages": persistent.iter().map(present_persistent).collect::<Vec<_>>(),
        "file_storages": files.iter().map(|f| present_file(&state, &ctx, f)).collect::<Vec<_>>(),
    }))
    .into_response())
}

fn server_info_of(row: &coolify_db::servers::ServerRow) -> ServerInfo {
    ServerInfo::from_row(row)
}

async fn server_for(state: &AppState, d: &DatabaseRow) -> Result<ServerInfo, ApiError> {
    let id = d.server_id().ok_or(ApiError::Internal)?;
    let row = servers::find_by_id(&state.db, id)
        .await?
        .ok_or(ApiError::Internal)?;
    Ok(server_info_of(&row))
}

/// `POST /databases/{uuid}/storages` (`create_storage`).
pub async fn create(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let input = validated_json(&headers, &uri, &body)?;
    let d = find(&state, &ctx, &uuid).await?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    validate(
        &input,
        &[
            Field {
                name: "type",
                rules: vec![
                    Rule::Required,
                    Rule::String,
                    Rule::In(&["persistent", "file"]),
                ],
            },
            Field {
                name: "name",
                rules: vec![Rule::String, Rule::Regex(&VOLUME_NAME)],
            },
            Field {
                name: "mount_path",
                rules: vec![Rule::Required, Rule::String],
            },
            Field {
                name: "content",
                rules: vec![Rule::String, Rule::Nullable],
            },
            Field {
                name: "is_directory",
                rules: vec![Rule::Boolean],
            },
            Field {
                name: "is_host_file",
                rules: vec![Rule::Boolean],
            },
            Field {
                name: "fs_path",
                rules: vec![Rule::String],
            },
        ],
        CUSTOM_API_MESSAGES,
        &[
            "type",
            "name",
            "mount_path",
            "content",
            "is_directory",
            "is_host_file",
            "fs_path",
        ],
    )?;
    let now = state.now();
    let morph = d.kind.morph();
    let dir = format!("{CONFIGURATION_DIR}/{}", d.uuid);
    let mount_input = input.str("mount_path").unwrap_or_default().to_owned();
    if input.str("type") == Some("persistent") {
        if !input.get("name").is_some_and(crate::serialize::php_truthy) {
            return Err(validation(
                "name",
                "The name field is required for persistent storages.",
            ));
        }
        let invalid: Vec<&str> = ["content", "is_directory", "is_host_file", "fs_path"]
            .into_iter()
            .filter(|k| input.has(k))
            .collect();
        if !invalid.is_empty() {
            let mut m = Map::new();
            for k in invalid {
                m.insert(
                    k.into(),
                    Value::String(format!("Field '{k}' is not valid for type 'persistent'.")),
                );
            }
            return Err(ApiError::Validation(m));
        }
        let name = format!("{}-{}", d.uuid, input.str("name").unwrap_or_default());
        // The `mountPath` mutator.
        let mount = if mount_input.trim().starts_with('/') {
            mount_input.trim().to_owned()
        } else {
            format!("/{}", mount_input.trim())
        };
        let row =
            parts::create_persistent_storage(&state.db, morph, d.id, &name, &mount, now).await?;
        let shown = present_persistent(&row);
        let Value::Object(full) = shown else {
            return Err(ApiError::Internal);
        };
        return Ok(created(created_model(
            full,
            &[
                "name",
                "mount_path",
                "resource_id",
                "resource_type",
                "uuid",
                "updated_at",
                "created_at",
                "id",
            ],
        ))
        .into_response());
    }

    let invalid: Vec<&str> = ["name", "host_path"]
        .into_iter()
        .filter(|k| input.has(k))
        .collect();
    if !invalid.is_empty() {
        let mut m = Map::new();
        for k in invalid {
            m.insert(
                k.into(),
                Value::String(format!("Field '{k}' is not valid for type 'file'.")),
            );
        }
        return Err(ApiError::Validation(m));
    }
    let is_directory = input_bool(&input, "is_directory");
    let is_host_file = input_bool(&input, "is_host_file");
    if is_directory && is_host_file {
        return Err(validation(
            "is_host_file",
            "Host file mounts cannot also be directory mounts.",
        ));
    }
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let jobs = state.jobs.as_ref();
    let fs_input = input
        .get("fs_path")
        .filter(|v| crate::serialize::php_truthy(v))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let mut new = NewFileVolume::default();
    if is_directory {
        let Some(fs) = fs_input else {
            return Err(validation(
                "fs_path",
                "The fs_path field is required for directory mounts.",
            ));
        };
        let fs_error = |e: String| validation("fs_path", e);
        let fs_path =
            paths::resolve_host_path(&dir, &fs, "storage source path").map_err(fs_error)?;
        let mount = paths::validate_file_mount_path(&mount_input, "storage destination path")
            .map_err(fs_error)?;
        if paths::is_outside_resource_directory(&fs_path, &dir) && !can_deploy(&ctx) {
            return Err(ApiError::Message(StatusCode::FORBIDDEN, OUTSIDE_REASON));
        }
        let server = server_for(&state, &d).await?;
        let jobs = jobs.ok_or(ApiError::NotPorted)?;
        jobs.assert_host_path_on_server(&dir, &fs_path, &server, true)
            .await
            .map_err(fs_error)?;
        new.fs_path = fs_path;
        new.mount_path = mount;
        new.is_directory = true;
    } else if is_host_file {
        let Some(fs) = fs_input else {
            return Err(validation(
                "fs_path",
                "The fs_path field is required for host file mounts.",
            ));
        };
        if input.filled("content") {
            return Err(validation(
                "content",
                "Content is not valid for host file mounts.",
            ));
        }
        let err = |e: String| validation("mount_path", e);
        new.fs_path =
            paths::validate_host_file_mount_path(&fs, "host file source path").map_err(err)?;
        new.mount_path =
            paths::validate_file_mount_path(&mount_input, "host file destination path")
                .map_err(err)?;
        new.is_host_file = true;
    } else {
        let mount = paths::validate_file_mount_path(&mount_input, "file storage path")
            .map_err(|e| validation("mount_path", e))?;
        let fs_path = if input.filled("fs_path") {
            let fs = input.str("fs_path").unwrap_or_default();
            let err = |e: String| validation("fs_path", e);
            let fs_path =
                paths::resolve_host_path(&dir, fs, "file storage source path").map_err(err)?;
            if paths::is_outside_resource_directory(&fs_path, &dir) && !can_deploy(&ctx) {
                return Err(ApiError::Message(StatusCode::FORBIDDEN, OUTSIDE_REASON));
            }
            let server = server_for(&state, &d).await?;
            let jobs = jobs.ok_or(ApiError::NotPorted)?;
            jobs.assert_host_path_on_server(&dir, &fs_path, &server, false)
                .await
                .map_err(err)?;
            fs_path
        } else {
            paths::confine_file_mount_path(&dir, &mount, "file storage path")
                .map_err(|e| validation("mount_path", e))?
        };
        if input.filled("content") && !can_deploy(&ctx) {
            return Err(ApiError::Message(
                StatusCode::FORBIDDEN,
                WRITE_CONTENT_REASON,
            ));
        }
        new.fs_path = fs_path;
        new.mount_path = mount;
        new.content_encrypted = input.str("content").map(|c| enc.encrypt_string(c));
    }
    let row = parts::create_file_storage(&state.db, morph, d.id, &new, now).await?;
    // `created` hook: a host file, or a mount outside the resource directory, is not written now.
    if !new.is_host_file && !paths::is_outside_resource_directory(&new.fs_path, &dir) {
        let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
        jobs.spawn_file_storage_save(row.clone(), d.clone());
    }
    tracing::info!(target: "audit", event = "api.database.storage_created", team_id = ctx.team_id, database_uuid = %d.uuid, storage_type = "file", mount_path = %new.mount_path, "database storage created");
    let Value::Object(mut full) = present_file(&state, &ctx, &row) else {
        return Err(ApiError::Internal);
    };
    // Only what `LocalFileVolume::create([...])` assigned is on the model.
    if new.is_directory {
        full.shift_remove("content");
    }
    if !new.is_host_file {
        full.shift_remove("is_host_file");
    }
    Ok(created(created_model(
        full,
        &[
            "fs_path",
            "mount_path",
            "content",
            "is_directory",
            "is_host_file",
            "resource_id",
            "resource_type",
            "uuid",
            "updated_at",
            "created_at",
            "id",
            "is_binary",
            "is_too_large",
        ],
    ))
    .into_response())
}

/// `PATCH /databases/{uuid}/storages` (`update_storage`).
pub async fn update(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let input = validated_json(&headers, &uri, &body)?;
    let d = find(&state, &ctx, &uuid).await?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    validate(
        &input,
        &[
            Field {
                name: "uuid",
                rules: vec![Rule::String],
            },
            Field {
                name: "id",
                rules: vec![Rule::Integer],
            },
            Field {
                name: "type",
                rules: vec![
                    Rule::Required,
                    Rule::String,
                    Rule::In(&["persistent", "file"]),
                ],
            },
            Field {
                name: "is_preview_suffix_enabled",
                rules: vec![Rule::Boolean],
            },
            Field {
                name: "name",
                rules: vec![Rule::String, Rule::Regex(&VOLUME_NAME)],
            },
            Field {
                name: "mount_path",
                rules: vec![Rule::String],
            },
            Field {
                name: "content",
                rules: vec![Rule::String, Rule::Nullable],
            },
        ],
        CUSTOM_API_MESSAGES,
        &[
            "uuid",
            "id",
            "type",
            "is_preview_suffix_enabled",
            "name",
            "mount_path",
            "content",
        ],
    )?;
    let lookup_uuid = input.str("uuid").filter(|u| !u.is_empty() && *u != "0");
    let lookup_id = input.get("id").filter(|v| crate::serialize::php_truthy(v));
    if lookup_uuid.is_none() && lookup_id.is_none() {
        return Err(validation("uuid", "Either uuid or id is required."));
    }
    let persistent = input.str("type") == Some("persistent");
    let rows = if persistent {
        parts::persistent_storages(&state.db, d.kind.morph(), d.id).await?
    } else {
        parts::file_storages(&state.db, d.kind.morph(), d.id).await?
    };
    let row = rows
        .into_iter()
        .find(|r| match lookup_uuid {
            Some(u) => parts::str_of(r, "uuid") == Some(u),
            None => lookup_id.is_some_and(|i| {
                i.as_i64()
                    .or_else(|| i.as_str().and_then(|s| s.parse().ok()))
                    == Some(parts::id_of(r))
            }),
        })
        .ok_or(ApiError::Message(
            StatusCode::NOT_FOUND,
            "Storage not found.",
        ))?;
    let invalid: &[&str] = if persistent {
        &["content"]
    } else {
        &["name", "host_path"]
    };
    let invalid: Vec<&&str> = invalid.iter().filter(|k| input.has(k)).collect();
    if !invalid.is_empty() {
        let kind = input.str("type").unwrap_or_default();
        let mut m = Map::new();
        for k in invalid {
            m.insert(
                (*k).to_string(),
                Value::String(format!("Field '{k}' is not valid for type '{kind}'.")),
            );
        }
        return Err(ApiError::Validation(m));
    }
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let current_content = parts::str_of(&row, "content")
        .and_then(|c| enc.decrypt_string(c).ok())
        .unwrap_or_default();
    // contentChangeForbiddenResponse
    if !persistent
        && !parts::bool_of(&row, "is_directory")
        && !parts::bool_of(&row, "is_host_file")
        && input.has("content")
        && input.str("content").unwrap_or_default() != current_content
        && !can_deploy(&ctx)
    {
        return Err(ApiError::Message(
            StatusCode::FORBIDDEN,
            WRITE_CONTENT_REASON,
        ));
    }
    let mut values: Vec<(&'static str, ColumnValue)> = Vec::new();
    if input.has("is_preview_suffix_enabled") {
        values.push((
            "is_preview_suffix_enabled",
            ColumnValue::Bool(input_bool(&input, "is_preview_suffix_enabled")),
        ));
    }
    let mut content_changed = false;
    if persistent {
        if let Some(n) = input.str("name") {
            values.push(("name", ColumnValue::Text(Some(n.to_owned()))));
        }
        if let Some(m) = input.str("mount_path") {
            let m = m.trim();
            let m = if m.starts_with('/') {
                m.to_owned()
            } else {
                format!("/{m}")
            };
            values.push(("mount_path", ColumnValue::Text(Some(m))));
        }
    } else {
        if input.has("mount_path") {
            values.push((
                "mount_path",
                ColumnValue::Text(input.str("mount_path").map(str::to_owned)),
            ));
        }
        if input.has("content") {
            let new_content = input.str("content");
            if new_content.unwrap_or_default() != current_content {
                content_changed = true;
                values.push((
                    "content",
                    ColumnValue::Text(new_content.map(|c| enc.encrypt_string(c))),
                ));
            }
        }
    }
    let id = parts::id_of(&row);
    let table = if persistent {
        "local_persistent_volumes"
    } else {
        "local_file_volumes"
    };
    parts::update_row(&state.db, table, id, &values, state.now()).await?;
    let fresh = parts::find_row(&state.db, table, id)
        .await?
        .ok_or(ApiError::Internal)?;
    if content_changed {
        let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
        jobs.spawn_file_storage_save(fresh.clone(), d.clone());
    }
    tracing::info!(target: "audit", event = "api.database.storage_updated", team_id = ctx.team_id, database_uuid = %d.uuid, "database storage updated");
    Ok(if persistent {
        ok(present_persistent(&fresh)).into_response()
    } else {
        ok(present_file(&state, &ctx, &fresh)).into_response()
    })
}

/// `DELETE /databases/{uuid}/storages/{storage_uuid}` (`delete_storage`).
pub async fn delete(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((uuid, storage_uuid)): Path<(String, String)>,
) -> ApiResult {
    let d = find(&state, &ctx, &uuid).await?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let morph = d.kind.morph();
    let persistent = parts::persistent_storages(&state.db, morph, d.id)
        .await?
        .into_iter()
        .find(|r| parts::str_of(r, "uuid") == Some(storage_uuid.as_str()));
    let file = if persistent.is_some() {
        None
    } else {
        parts::file_storages(&state.db, morph, d.id)
            .await?
            .into_iter()
            .find(|r| parts::str_of(r, "uuid") == Some(storage_uuid.as_str()))
    };
    if persistent.is_none() && file.is_none() {
        return Err(ApiError::Message(
            StatusCode::NOT_FOUND,
            "Storage not found.",
        ));
    }
    if let Some(row) = &persistent {
        // isSharedWithAnotherResource
        let shared = row
            .get("standalone_sqlite_id")
            .is_some_and(|v| !v.is_null())
            || (d.kind == Kind::Sqlite
                && parts::sqlite_volume_connected(
                    &state.db,
                    d.id,
                    parts::str_of(row, "name").unwrap_or_default(),
                )
                .await?);
        if shared {
            return Err(ApiError::Message(
                StatusCode::UNPROCESSABLE_ENTITY,
                "This volume is mounted by an application. Unlink it on the SQLite database page first.",
            ));
        }
        if parts::volume_has_backups(&state.db, LOCAL_PERSISTENT_VOLUME_MORPH, parts::id_of(row))
            .await?
        {
            return Err(ApiError::Abort(
                StatusCode::UNPROCESSABLE_ENTITY,
                "Delete this volume backup schedule and its archives before deleting the volume.",
            ));
        }
        parts::delete_row(&state.db, "local_persistent_volumes", parts::id_of(row)).await?;
    } else if let Some(row) = &file {
        if parts::volume_has_backups(&state.db, LOCAL_FILE_VOLUME_MORPH, parts::id_of(row)).await? {
            return Err(ApiError::Abort(
                StatusCode::UNPROCESSABLE_ENTITY,
                "Delete this directory backup schedule and its archives before deleting the directory.",
            ));
        }
        let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
        if let Err(e) = jobs.delete_file_storage(row, &d).await {
            tracing::warn!(database = %d.uuid, error = %e, "file storage could not be deleted on the server");
            return Err(ApiError::Internal);
        }
        parts::delete_row(&state.db, "local_file_volumes", parts::id_of(row)).await?;
    }
    tracing::info!(target: "audit", event = "api.database.storage_deleted", team_id = ctx.team_id, database_uuid = %d.uuid, storage_uuid = %storage_uuid, "database storage deleted");
    Ok(ok(json!({"message": "Storage deleted."})).into_response())
}
