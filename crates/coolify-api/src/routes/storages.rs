//! Storages of a resource: `storages`, `create_storage`, `update_storage` and `delete_storage` of
//! `ApplicationsController`, and `Concerns\RequiresDeployForOutsideHostPaths`. The logic is generic
//! over [`StorageResource`]; the `application_*` handlers at the end are the application wrappers.
//! Volume backup routes (`/storages/{uuid}/backups*`) belong to backups and are not here.

use std::sync::LazyLock;

use axum::Extension;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::IntoResponse;
use coolify_db::applications::{self as apps, ApplicationRow};
use coolify_db::records::{LOCAL_FILE_VOLUME, LOCAL_PERSISTENT_VOLUME, Record};
use coolify_db::servers::ColumnValue;
use coolify_db::storages::{self as store, NewFileVolume, StorageKind};
use coolify_domain::Ability;
use coolify_jobs::storages::{
    FileStorage, StorageOwner, confine_file_mount_path, is_outside_resource_directory,
    resolve_host_path, validate_file_mount_path, validate_host_file_mount_path,
};
use regex::Regex;
use serde_json::{Map, Value, json};

use crate::auth::AuthCtx;
use crate::input::{Input, lenient_json, validated_json};
use crate::response::{ApiError, ApiResult, PhpJson, created, ok};
use crate::routes::databases::input_bool;
use crate::routes::env_vars::magic_uuid;
use crate::serialize::php_truthy;
use crate::state::AppState;
use crate::validation::{CUSTOM_API_MESSAGES, Field, Rule, validate};

/// `ValidationPatterns::VOLUME_NAME_PATTERN`.
static VOLUME_NAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[a-zA-Z0-9][a-zA-Z0-9._-]*\n?\z").unwrap_or_else(|_| unreachable!())
});

const OUTSIDE_FORBIDDEN: ApiError = ApiError::Message(
    StatusCode::FORBIDDEN,
    "Missing required permissions: deploy. A mount outside the resource directory needs a token with the deploy permission.",
);
const CONTENT_FORBIDDEN: ApiError = ApiError::Message(
    StatusCode::FORBIDDEN,
    "Missing required permissions: deploy. Coolify writes the file content on the server immediately, so it needs a token with the deploy permission.",
);

/// What the storage endpoints need from the resource that owns the storages.
#[derive(Debug, Clone)]
pub struct StorageResource {
    pub morph: &'static str,
    pub id: i64,
    pub uuid: String,
    /// The name of the resource in audit events (`application`).
    pub label: &'static str,
    pub owner: StorageOwner,
    /// `$resource->destination->server`.
    pub server_id: Option<i64>,
    /// `persistentStorageUnavailableReason()`.
    pub persistent_unavailable: Option<&'static str>,
    /// `shouldBeReadOnlyInUI()`. Always false for an application: `isReadOnlyVolume()` only looks
    /// at resources with a `service` relation, and `isDockerComposeResource()` needs the `resource`
    /// relation, which these collections do not load. A service wrapper passes the real answer.
    pub read_only: bool,
    /// `$resource->toArray()`: the responses of a created or updated storage carry it, because
    /// the model's checks loaded the morph relation (`resource` or `service`).
    pub model_array: Value,
}

impl StorageResource {
    pub async fn application(state: &AppState, a: &ApplicationRow) -> Result<Self, ApiError> {
        let additional = store::additional_server_ids(&state.db, a.id).await?;
        Ok(Self {
            morph: apps::MORPH,
            id: a.id,
            uuid: a.uuid.clone(),
            label: "application",
            owner: StorageOwner::for_application(&state.db, a).await?,
            server_id: a.server_id(),
            persistent_unavailable: (!additional.is_empty()).then_some(
                "Applications that use multiple servers cannot have persistent storage because volumes are not shared between servers.",
            ),
            read_only: false,
            // `response()->json($storage)` skips `serializeApiResponse`: nothing is redacted.
            model_array: crate::routes::applications::model_array(state, a, false, true).await?,
        })
    }
}

fn str_of<'a>(r: &'a Record, k: &str) -> Option<&'a str> {
    r.get(k).and_then(Value::as_str)
}

fn id_of(r: &Record) -> i64 {
    r.get("id").and_then(Value::as_i64).unwrap_or_default()
}

fn flag(r: &Record, k: &str) -> bool {
    r.get(k).and_then(Value::as_bool).unwrap_or(false)
}

fn decrypted_content(state: &AppState, r: &Record) -> Result<Option<String>, ApiError> {
    match str_of(r, "content") {
        None => Ok(None),
        Some(c) => {
            let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
            enc.decrypt_string(c)
                .map(Some)
                .map_err(|_| ApiError::Internal)
        }
    }
}

/// A `local_file_volumes` row as the server operations want it.
pub fn file_storage(state: &AppState, r: &Record) -> Result<FileStorage, ApiError> {
    Ok(FileStorage {
        id: id_of(r),
        fs_path: str_of(r, "fs_path").unwrap_or_default().to_owned(),
        mount_path: str_of(r, "mount_path").map(str::to_owned),
        content: decrypted_content(state, r)?,
        is_directory: flag(r, "is_directory"),
        is_host_file: flag(r, "is_host_file"),
        is_based_on_git: flag(r, "is_based_on_git"),
        chmod: str_of(r, "chmod").map(str::to_owned),
        chown: str_of(r, "chown").map(str::to_owned),
    })
}

/// `$storage` of `LocalFileVolume` as JSON: `$hidden` `content` unless the token may read it,
/// with the appended `is_binary` and `is_too_large`. `order` lists the attributes of a model that
/// was just created; a row read from the database keeps its column order.
fn present_file(
    state: &AppState,
    ctx: &AuthCtx,
    row: Record,
    order: Option<&[&str]>,
) -> Result<Value, ApiError> {
    let content = decrypted_content(state, &row)?;
    let mut m = LOCAL_FILE_VOLUME.present(row);
    if let Some(order) = order {
        let mut fresh = Map::new();
        for k in order.iter().copied().chain([
            "resource_id",
            "resource_type",
            "uuid",
            "updated_at",
            "created_at",
            "id",
        ]) {
            fresh.insert(k.to_owned(), m.get(k).cloned().unwrap_or(Value::Null));
        }
        m = fresh;
    }
    if m.contains_key("content") {
        if ctx.can_read_sensitive {
            m.insert("content".into(), json!(content));
        } else {
            m.shift_remove("content");
        }
    }
    m.insert(
        "is_binary".into(),
        Value::Bool(content.as_deref() == Some("[binary file]")),
    );
    m.insert(
        "is_too_large".into(),
        Value::Bool(content.as_deref() == Some("[file too large to display]")),
    );
    Ok(Value::Object(m))
}

fn validation(entries: &[(&str, String)]) -> ApiError {
    let mut errors = Map::new();
    for (k, v) in entries {
        errors.insert((*k).to_owned(), Value::String(v.clone()));
    }
    ApiError::Validation(errors)
}

fn invalid_for_type(input: &Input, fields: &[&str], kind: &str) -> Result<(), ApiError> {
    let entries: Vec<(&str, String)> = input
        .keys()
        .filter(|k| fields.contains(&k.as_str()))
        .map(|k| {
            (
                k.as_str(),
                format!("Field '{k}' is not valid for type '{kind}'."),
            )
        })
        .collect();
    if entries.is_empty() {
        Ok(())
    } else {
        Err(validation(&entries))
    }
}

pub(super) fn can_deploy(ctx: &AuthCtx) -> bool {
    ctx.abilities.can(Ability::Deploy) || ctx.abilities.can(Ability::Root)
}

/// `str($value)->trim()->start('/')` (`LocalPersistentVolume::mountPath`).
fn persistent_mount_path(value: &str) -> String {
    format!("/{}", value.trim().trim_start_matches('/'))
}

/// `storages()` after the authorization.
pub async fn list_storages(
    state: &AppState,
    ctx: &AuthCtx,
    resource: &StorageResource,
) -> Result<Value, ApiError> {
    let persistent = store::list(
        &state.db,
        StorageKind::Persistent,
        resource.morph,
        resource.id,
    )
    .await?
    .into_iter()
    .map(|r| Value::Object(LOCAL_PERSISTENT_VOLUME.present(r)))
    .collect();
    let mut files = Vec::new();
    for r in store::list(&state.db, StorageKind::File, resource.morph, resource.id).await? {
        files.push(present_file(state, ctx, r, None)?);
    }
    Ok(json!({"persistent_storages": Value::Array(persistent), "file_storages": files}))
}

fn field(name: &'static str, rules: Vec<Rule>) -> Field {
    Field { name, rules }
}

fn name_rules() -> Vec<Rule> {
    vec![Rule::String, Rule::Regex(LazyLock::force(&VOLUME_NAME))]
}

/// `$collection->where($lookupField, $lookupValue)->first()`: `id` compares loosely.
fn find_row(rows: Vec<Record>, uuid: Option<&Value>, id: Option<&Value>) -> Option<Record> {
    if let Some(uuid) = uuid.filter(|v| php_truthy(v)) {
        return rows
            .into_iter()
            .find(|r| r.get("uuid").is_some_and(|u| u == uuid));
    }
    let id = id?;
    rows.into_iter().find(|r| {
        let own = id_of(r);
        match id {
            Value::Number(n) => n.as_i64() == Some(own),
            Value::String(s) => s.trim().parse::<i64>().ok() == Some(own),
            Value::Bool(b) => *b == (own != 0),
            _ => false,
        }
    })
}

/// `update_storage()` after the authorization.
pub async fn update_storage(
    state: &AppState,
    ctx: &AuthCtx,
    resource: &StorageResource,
    input: &Input,
) -> Result<Value, ApiError> {
    let fields = vec![
        field("uuid", vec![Rule::String]),
        field("id", vec![Rule::Integer]),
        field(
            "type",
            vec![
                Rule::Required,
                Rule::String,
                Rule::In(&["persistent", "file"]),
            ],
        ),
        field("is_preview_suffix_enabled", vec![Rule::Boolean]),
        field("name", name_rules()),
        field("mount_path", vec![Rule::String]),
        field("content", vec![Rule::String, Rule::Nullable]),
    ];
    validate(
        input,
        &fields,
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
    let (uuid, id) = (input.get("uuid"), input.get("id"));
    if !uuid.is_some_and(php_truthy) && !id.is_some_and(php_truthy) {
        return Err(validation(&[(
            "uuid",
            "Either uuid or id is required.".to_owned(),
        )]));
    }
    let persistent = input.str("type") == Some("persistent");
    let kind = if persistent {
        StorageKind::Persistent
    } else {
        StorageKind::File
    };
    let rows = store::list(&state.db, kind, resource.morph, resource.id).await?;
    let row = find_row(rows, uuid, id).ok_or(ApiError::Message(
        StatusCode::NOT_FOUND,
        "Storage not found.",
    ))?;
    let requested: Vec<&str> = ["name", "mount_path", "content"]
        .into_iter()
        .filter(|k| input.has(k))
        .collect();
    if resource.read_only && !requested.is_empty() {
        return Err(ApiError::Status(
            StatusCode::UNPROCESSABLE_ENTITY,
            json!({
                "message": "This storage is read-only (managed by docker-compose or service definition). Only is_preview_suffix_enabled can be updated.",
                "read_only_fields": requested,
            }),
        ));
    }
    if !resource.read_only {
        if persistent {
            invalid_for_type(input, &["content"], "persistent")?;
        } else {
            invalid_for_type(input, &["name", "host_path"], "file")?;
        }
    }
    let old_content = if persistent {
        None
    } else {
        decrypted_content(state, &row)?
    };
    // `contentChangeForbiddenResponse`.
    if !resource.read_only
        && !persistent
        && !flag(&row, "is_directory")
        && !flag(&row, "is_host_file")
        && input.has("content")
        && input.str("content").unwrap_or_default() != old_content.as_deref().unwrap_or_default()
        && !can_deploy(ctx)
    {
        return Err(CONTENT_FORBIDDEN);
    }

    let mut dirty: Vec<(&'static str, ColumnValue)> = Vec::new();
    if let Some(v) = input.get("is_preview_suffix_enabled")
        && php_truthy(v) != flag(&row, "is_preview_suffix_enabled")
    {
        dirty.push((
            "is_preview_suffix_enabled",
            ColumnValue::Bool(php_truthy(v)),
        ));
    }
    let mut content_changed = false;
    if !resource.read_only {
        if persistent {
            if let Some(name) = input.str("name")
                && Some(name) != str_of(&row, "name")
            {
                dirty.push(("name", ColumnValue::Text(Some(name.to_owned()))));
            }
            if let Some(path) = input.str("mount_path").map(persistent_mount_path)
                && Some(path.as_str()) != str_of(&row, "mount_path")
            {
                dirty.push(("mount_path", ColumnValue::Text(Some(path))));
            }
        } else {
            if let Some(path) = input.str("mount_path")
                && Some(path) != str_of(&row, "mount_path")
            {
                dirty.push(("mount_path", ColumnValue::Text(Some(path.to_owned()))));
            }
            if input.has("content") {
                let new = input.str("content");
                if new != old_content.as_deref() {
                    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
                    dirty.push((
                        "content",
                        ColumnValue::Text(new.map(|c| enc.encrypt_string(c))),
                    ));
                    content_changed = true;
                }
            }
        }
    }
    let saved = store::save_dirty(&state.db, kind, id_of(&row), &dirty, state.now()).await?;
    if content_changed && let Some(jobs) = state.jobs.as_ref() {
        jobs.spawn_storage_save(resource.owner.clone(), file_storage(state, &saved)?);
    }
    tracing::info!(target: "audit", event = %format!("api.{}.storage_updated", resource.label), team_id = ctx.team_id, resource_uuid = %resource.uuid, storage_id = id_of(&saved), "storage updated");
    if persistent {
        let mut m = LOCAL_PERSISTENT_VOLUME.present(saved);
        m.insert("resource".into(), resource.model_array.clone());
        Ok(Value::Object(m))
    } else {
        let mut v = present_file(state, ctx, saved, None)?;
        if let Value::Object(m) = &mut v {
            m.insert("service".into(), resource.model_array.clone());
        }
        Ok(v)
    }
}

/// `$this->outsideHostPathForbiddenResponse(...)` and `assertHostPathOnServer(...)`.
async fn check_host_path(
    state: &AppState,
    ctx: &AuthCtx,
    resource: &StorageResource,
    fs_path: &str,
    is_directory: bool,
) -> Result<Result<(), String>, ApiError> {
    if is_outside_resource_directory(fs_path, &resource.owner.base_directories) && !can_deploy(ctx)
    {
        return Err(OUTSIDE_FORBIDDEN);
    }
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    let server = resource.server_id.ok_or(ApiError::Internal)?;
    Ok(jobs
        .assert_storage_host_path(&resource.owner.workdir, fs_path, server, is_directory)
        .await)
}

/// `create_storage()` after the authorization.
#[allow(clippy::too_many_lines)]
pub async fn create_storage(
    state: &AppState,
    ctx: &AuthCtx,
    resource: &StorageResource,
    input: &Input,
) -> Result<Value, ApiError> {
    let fields = vec![
        field(
            "type",
            vec![
                Rule::Required,
                Rule::String,
                Rule::In(&["persistent", "file"]),
            ],
        ),
        field("name", name_rules()),
        field("mount_path", vec![Rule::Required, Rule::String]),
        field("content", vec![Rule::String, Rule::Nullable]),
        field("is_directory", vec![Rule::Boolean]),
        field("is_host_file", vec![Rule::Boolean]),
        field("fs_path", vec![Rule::String]),
    ];
    validate(
        input,
        &fields,
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
    let mount = input.str("mount_path").unwrap_or_default();
    if input.str("type") == Some("persistent") {
        if !input.get("name").is_some_and(php_truthy) {
            return Err(validation(&[(
                "name",
                "The name field is required for persistent storages.".to_owned(),
            )]));
        }
        invalid_for_type(
            input,
            &["content", "is_directory", "is_host_file", "fs_path"],
            "persistent",
        )?;
        if let Some(reason) = resource.persistent_unavailable {
            return Err(ApiError::Message(StatusCode::UNPROCESSABLE_ENTITY, reason));
        }
        let name = format!(
            "{}-{}",
            resource.uuid,
            input.str("name").unwrap_or_default()
        );
        let row = store::create_persistent(
            &state.db,
            resource.morph,
            resource.id,
            &name,
            &persistent_mount_path(mount),
            state.now(),
        )
        .await?;
        let r = LOCAL_PERSISTENT_VOLUME.present(row);
        let mut fresh = Map::new();
        for k in [
            "name",
            "mount_path",
            "resource_id",
            "resource_type",
            "uuid",
            "updated_at",
            "created_at",
            "id",
        ] {
            fresh.insert(k.to_owned(), r.get(k).cloned().unwrap_or(Value::Null));
        }
        return Ok(Value::Object(fresh));
    }

    invalid_for_type(input, &["name", "host_path"], "file")?;
    let is_directory = input_bool(input, "is_directory");
    let is_host_file = input_bool(input, "is_host_file");
    if is_directory && is_host_file {
        return Err(validation(&[(
            "is_host_file",
            "Host file mounts cannot also be directory mounts.".to_owned(),
        )]));
    }
    let fs_input = input.get("fs_path").filter(|v| php_truthy(v));
    let fs_str = input.str("fs_path").unwrap_or_default();
    let dirs = &resource.owner.base_directories;
    let mut new = NewFileVolume {
        is_directory,
        is_host_file,
        ..Default::default()
    };
    let order: &[&str];
    let mut remote_check_ran = false;
    if is_directory {
        if fs_input.is_none() {
            return Err(validation(&[(
                "fs_path",
                "The fs_path field is required for directory mounts.".to_owned(),
            )]));
        }
        let fail = |m: String| validation(&[("fs_path", m)]);
        let fs_path = resolve_host_path(dirs, fs_str, "storage source path").map_err(&fail)?;
        let mount_path =
            validate_file_mount_path(mount, "storage destination path").map_err(&fail)?;
        check_host_path(state, ctx, resource, &fs_path, true)
            .await?
            .map_err(&fail)?;
        remote_check_ran = !is_outside_resource_directory(&fs_path, dirs);
        new.fs_path = fs_path;
        new.mount_path = mount_path;
        order = &["fs_path", "mount_path", "is_directory"];
    } else if is_host_file {
        if fs_input.is_none() {
            return Err(validation(&[(
                "fs_path",
                "The fs_path field is required for host file mounts.".to_owned(),
            )]));
        }
        if input.filled("content") {
            return Err(validation(&[(
                "content",
                "Content is not valid for host file mounts.".to_owned(),
            )]));
        }
        let fail = |m: String| validation(&[("mount_path", m)]);
        new.fs_path =
            validate_host_file_mount_path(fs_str, "host file source path").map_err(&fail)?;
        new.mount_path =
            validate_file_mount_path(mount, "host file destination path").map_err(&fail)?;
        order = &[
            "fs_path",
            "mount_path",
            "content",
            "is_directory",
            "is_host_file",
        ];
    } else {
        let mount_path = validate_file_mount_path(mount, "file storage path")
            .map_err(|m| validation(&[("mount_path", m)]))?;
        if input.filled("fs_path") {
            let fail = |m: String| validation(&[("fs_path", m)]);
            let fs_path =
                resolve_host_path(dirs, fs_str, "file storage source path").map_err(&fail)?;
            check_host_path(state, ctx, resource, &fs_path, false)
                .await?
                .map_err(&fail)?;
            remote_check_ran = true;
            new.fs_path = fs_path;
        } else {
            new.fs_path =
                confine_file_mount_path(&resource.owner.workdir, &mount_path, "file storage path")
                    .map_err(|m| validation(&[("mount_path", m)]))?;
        }
        if input.filled("content") && !can_deploy(ctx) {
            return Err(CONTENT_FORBIDDEN);
        }
        let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
        new.content_encrypted = input.str("content").map(|c| enc.encrypt_string(c));
        new.mount_path = mount_path;
        order = &["fs_path", "mount_path", "content", "is_directory"];
    }
    let row = store::create_file(&state.db, resource.morph, resource.id, &new, state.now()).await?;
    // The model's `created` hook: a mount outside the resource directory is written at deployment.
    if !is_host_file
        && !is_outside_resource_directory(&new.fs_path, dirs)
        && let Some(jobs) = state.jobs.as_ref()
    {
        jobs.spawn_storage_save(resource.owner.clone(), file_storage(state, &row)?);
    }
    tracing::info!(target: "audit", event = %format!("api.{}.storage_created", resource.label), team_id = ctx.team_id, resource_uuid = %resource.uuid, storage_id = id_of(&row), "storage created");
    let mut v = present_file(state, ctx, row, Some(order))?;
    if !is_host_file && let Value::Object(m) = &mut v {
        let mut model = resource.model_array.clone();
        if remote_check_ran {
            add_loaded_private_key(state, &mut model).await?;
        }
        m.insert("resource".into(), model);
    }
    Ok(v)
}

/// A remote command loads `$server->privateKey`, which the serialized model then carries.
async fn add_loaded_private_key(state: &AppState, model: &mut Value) -> Result<(), ApiError> {
    let Some(server) = model
        .get_mut("destination")
        .and_then(|d| d.get_mut("server"))
        .and_then(Value::as_object_mut)
    else {
        return Ok(());
    };
    let team = server
        .get("team_id")
        .and_then(Value::as_i64)
        .unwrap_or_default();
    let id = server
        .get("private_key_id")
        .and_then(Value::as_i64)
        .unwrap_or_default();
    if let Some(key) = coolify_db::private_keys::find_by_id(&state.db, team, id).await? {
        server.insert(
            "private_key".into(),
            crate::routes::private_keys::model_array(state, &key)?,
        );
    }
    Ok(())
}

/// `delete_storage()` after the authorization.
pub async fn delete_storage(
    state: &AppState,
    ctx: &AuthCtx,
    resource: &StorageResource,
    storage_uuid: &str,
) -> Result<Value, ApiError> {
    let by_uuid = |rows: Vec<Record>| {
        rows.into_iter()
            .find(|r| str_of(r, "uuid") == Some(storage_uuid))
    };
    let mut found = by_uuid(
        store::list(
            &state.db,
            StorageKind::Persistent,
            resource.morph,
            resource.id,
        )
        .await?,
    )
    .map(|r| (StorageKind::Persistent, r));
    if found.is_none() {
        found =
            by_uuid(store::list(&state.db, StorageKind::File, resource.morph, resource.id).await?)
                .map(|r| (StorageKind::File, r));
    }
    let (kind, row) = found.ok_or(ApiError::Message(
        StatusCode::NOT_FOUND,
        "Storage not found.",
    ))?;
    if resource.read_only {
        return Err(ApiError::Message(
            StatusCode::UNPROCESSABLE_ENTITY,
            "This storage is read-only (managed by docker-compose or service definition) and cannot be deleted.",
        ));
    }
    if store::has_scheduled_backups(&state.db, kind, id_of(&row)).await? {
        return Err(ApiError::Abort(
            StatusCode::UNPROCESSABLE_ENTITY,
            match kind {
                StorageKind::Persistent => {
                    "Delete this volume backup schedule and its archives before deleting the volume."
                }
                StorageKind::File => {
                    "Delete this directory backup schedule and its archives before deleting the directory."
                }
            },
        ));
    }
    if kind == StorageKind::File {
        let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
        let file = file_storage(state, &row)?;
        jobs.delete_storage_on_server(&resource.owner, &file)
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "storage could not be deleted on the server");
                ApiError::Internal
            })?;
    }
    store::delete(&state.db, kind, id_of(&row)).await?;
    tracing::info!(target: "audit", event = %format!("api.{}.storage_deleted", resource.label), team_id = ctx.team_id, resource_uuid = %resource.uuid, storage_uuid = %storage_uuid, "storage deleted");
    Ok(json!({"message": "Storage deleted."}))
}

// ---------------------------------------------------------------------------
// Application wrappers

const APP_NOT_FOUND: ApiError = ApiError::Message(StatusCode::NOT_FOUND, "Application not found");
const APP_NOT_FOUND_PERIOD: ApiError =
    ApiError::Message(StatusCode::NOT_FOUND, "Application not found.");

/// `GET /applications/{uuid}/storages`.
pub async fn application_list(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let input = lenient_json(&headers, &uri, &body);
    let uuid = magic_uuid(&uuid, &input).ok_or(APP_NOT_FOUND)?;
    let a = apps::find_in_team(&state.db, ctx.team_id, &uuid)
        .await?
        .ok_or(APP_NOT_FOUND)?;
    let resource = StorageResource::application(&state, &a).await?;
    Ok(ok(list_storages(&state, &ctx, &resource).await?).into_response())
}

/// `POST /applications/{uuid}/storages`.
pub async fn application_create(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let input = validated_json(&headers, &uri, &body)?;
    let uuid = magic_uuid(&uuid, &input).ok_or(APP_NOT_FOUND_PERIOD)?;
    let a = apps::find_in_team(&state.db, ctx.team_id, &uuid)
        .await?
        .ok_or(APP_NOT_FOUND_PERIOD)?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let resource = StorageResource::application(&state, &a).await?;
    Ok(created(create_storage(&state, &ctx, &resource, &input).await?).into_response())
}

/// `PATCH /applications/{uuid}/storages`.
pub async fn application_update(
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
    let resource = StorageResource::application(&state, &a).await?;
    Ok(ok(update_storage(&state, &ctx, &resource, &input).await?).into_response())
}

/// `DELETE /applications/{uuid}/storages/{storage_uuid}`.
pub async fn application_delete(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((uuid, storage_uuid)): Path<(String, String)>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> Result<PhpJson, ApiError> {
    let input = lenient_json(&headers, &uri, &body);
    let uuid = magic_uuid(&uuid, &input).ok_or(APP_NOT_FOUND_PERIOD)?;
    let a = apps::find_in_team(&state.db, ctx.team_id, &uuid)
        .await?
        .ok_or(APP_NOT_FOUND_PERIOD)?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let resource = StorageResource::application(&state, &a).await?;
    Ok(ok(
        delete_storage(&state, &ctx, &resource, &storage_uuid).await?
    ))
}
