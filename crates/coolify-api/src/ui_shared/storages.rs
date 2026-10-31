//! `Project\Service\Storage`, `Project\Shared\Storages\All` and `Project\Service\FileStorage`.

use std::sync::LazyLock;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post};
use axum::{Extension, Json, Router};
use coolify_db::database_parts::{self as parts, NewFileVolume};
use coolify_db::records::Record;
use coolify_db::servers::{self, ColumnValue as C};
use coolify_jobs::databases::paths;
use coolify_jobs::server::ServerInfo;
use regex::Regex;
use serde_json::{Map, Value, json};

use super::{Class, Res, find};
use crate::audit::Meta;
use crate::auth::AuthCtx;
use crate::response::{ApiError, ApiResult, PhpJson, ok};
use crate::state::AppState;
use crate::ui_notifications::{toast_error, unauthorized};

type P = Path<(String, String)>;
type PI = Path<(String, String, i64)>;

/// `All::rebuildVolumeBackupMeta`: the Backup column of the volume table.
async fn volume_backup_meta(
    state: &AppState,
    res: &Res,
    available: bool,
) -> Result<(Option<String>, Map<String, Value>), ApiError> {
    use coolify_db::volume_backups as vb;
    let mut meta = Map::new();
    if !available {
        return Ok((None, meta));
    }
    let db = &state.db;
    let (Some(project), Some(environment)) = (&res.project_uuid, &res.environment_uuid) else {
        return Ok((None, meta));
    };
    let root = format!("/project/{project}/environment/{environment}");
    let base = match &res.service_uuid {
        Some(service) => format!("{root}/service/{service}/storage-backups"),
        None => format!("{root}/application/{}/backups", res.uuid),
    };
    let volumes = parts::persistent_storages(db, &res.morph, res.id).await?;
    let mut total = 0;
    let mut schedules = Vec::new();
    for v in &volumes {
        let schedule = vb::schedule_of(db, vb::PERSISTENT_MORPH, parts::id_of(v)).await?;
        total += i64::from(schedule.is_some());
        schedules.push((v, schedule));
    }
    for f in parts::file_storages(db, &res.morph, res.id).await? {
        if f.get("is_directory").and_then(Value::as_bool) == Some(true)
            && f.get("is_host_file").and_then(Value::as_bool) != Some(true)
            && vb::schedule_of(db, vb::FILE_MORPH, parts::id_of(&f))
                .await?
                .is_some()
        {
            total += 1;
        }
    }
    for (v, schedule) in schedules {
        let enabled = schedule
            .as_ref()
            .and_then(|s| s.get("enabled"))
            .and_then(Value::as_bool)
            == Some(true);
        let url = schedule.as_ref().filter(|_| enabled).map(|s| {
            if total > 1 {
                let name = v.get("name").and_then(Value::as_str).unwrap_or_default();
                format!(
                    "{base}?search={}",
                    coolify_jobs::databases::connection::rawurlencode(name)
                )
            } else {
                format!(
                    "{base}/{}",
                    s.get("uuid").and_then(Value::as_str).unwrap_or_default()
                )
            }
        });
        meta.insert(
            parts::id_of(v).to_string(),
            json!({
                "enabled": enabled,
                "s3": enabled && schedule.as_ref().and_then(|s| s.get("save_s3")).and_then(Value::as_bool) == Some(true),
                "url": url,
            }),
        );
    }
    Ok((Some(base), meta))
}

pub fn routes() -> Router<AppState> {
    let b = "/ui/resources/{kind}/{uuid}/storages";
    Router::new()
        .route(b, get(show))
        .route(&format!("{b}/volume"), post(add_volume))
        .route(&format!("{b}/file"), post(add_file))
        .route(&format!("{b}/host-file"), post(add_host_file))
        .route(&format!("{b}/directory"), post(add_directory))
        .route(&format!("{b}/volume/{{id}}"), patch(update_volume))
        .route(&format!("{b}/volume/{{id}}/delete"), post(delete_volume))
        .route(&format!("{b}/file/{{id}}"), patch(save_file))
        .route(&format!("{b}/file/{{id}}/instant"), post(instant_file))
        .route(&format!("{b}/file/{{id}}/load"), post(load_file))
        .route(&format!("{b}/file/{{id}}/convert"), post(convert_file))
        .route(&format!("{b}/file/{{id}}/delete"), post(delete_file))
}

const BINARY: &str = "[binary file]";
const TOO_LARGE: &str = "[file too large to display]";
const VOLUME_TABLE: &str = "local_persistent_volumes";
const FILE_TABLE: &str = "local_file_volumes";

static VOLUME_NAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[a-zA-Z0-9][a-zA-Z0-9._-]*\n?\z").unwrap_or_else(|_| unreachable!())
});
static DIRECTORY_PATH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^/([a-zA-Z0-9._\-/~@+]*)?\n?\z").unwrap_or_else(|_| unreachable!())
});

fn text<'a>(r: &'a Record, k: &str) -> Option<&'a str> {
    r.get(k).and_then(Value::as_str)
}

fn id_of(r: &Record) -> i64 {
    r.get("id").and_then(Value::as_i64).unwrap_or_default()
}

fn flag(r: &Record, k: &str) -> bool {
    r.get(k).and_then(Value::as_bool).unwrap_or(false)
}

fn body_str<'a>(b: &'a Value, k: &str) -> &'a str {
    b.get(k).and_then(Value::as_str).unwrap_or_default()
}

fn field_errors(errors: Map<String, Value>) -> Response {
    let first = errors
        .values()
        .next()
        .and_then(|v| v.get(0))
        .and_then(Value::as_str)
        .unwrap_or("The given data was invalid.")
        .to_owned();
    let count: usize = errors
        .values()
        .map(|v| v.as_array().map_or(0, Vec::len))
        .sum();
    let message = match count.saturating_sub(1) {
        0 => first,
        1 => format!("{first} (and 1 more error)"),
        n => format!("{first} (and {n} more errors)"),
    };
    PhpJson(
        StatusCode::UNPROCESSABLE_ENTITY,
        json!({"message": message, "errors": errors}),
    )
    .into_response()
}

/// `auditStorageChange`.
#[allow(clippy::too_many_arguments)]
async fn audit(
    state: &AppState,
    ctx: &AuthCtx,
    meta: &Meta,
    res: &Res,
    action: &str,
    row: &Record,
    is_volume: bool,
    extra: Value,
) {
    let (prefix, mut context) = match res.class {
        Class::Application => (
            "application",
            json!({"team_id": res.team_id, "application_uuid": res.uuid, "application_name": res.name}),
        ),
        Class::ServiceApplication | Class::ServiceDatabase => (
            "service",
            json!({"team_id": res.team_id, "service_uuid": res.service_uuid, "stack_service_uuid": res.uuid, "stack_service_name": res.name}),
        ),
        _ => (
            "database",
            json!({"team_id": res.team_id, "database_uuid": res.uuid, "database_name": res.name}),
        ),
    };
    let storage_type = if is_volume {
        "persistent"
    } else if flag(row, "is_host_file") {
        "host_file"
    } else if flag(row, "is_directory") {
        "directory"
    } else {
        "file"
    };
    if let Value::Object(m) = &mut context {
        m.insert(
            "storage_uuid".into(),
            row.get("uuid").cloned().unwrap_or(Value::Null),
        );
        m.insert("storage_id".into(), json!(id_of(row)));
        m.insert("storage_type".into(), json!(storage_type));
        m.insert(
            "storage_name".into(),
            if is_volume {
                row.get("name").cloned().unwrap_or(Value::Null)
            } else {
                Value::Null
            },
        );
        m.insert(
            "mount_path".into(),
            row.get("mount_path").cloned().unwrap_or(Value::Null),
        );
        m.insert(
            "host_path".into(),
            row.get(if is_volume { "host_path" } else { "fs_path" })
                .cloned()
                .unwrap_or(Value::Null),
        );
        if let Value::Object(e) = extra {
            for (k, v) in e {
                m.insert(k, v);
            }
        }
    }
    crate::audit::log(
        state,
        ctx,
        meta,
        // `Concerns\\AuditsStorageChanges::auditStorageChange`.
        &format!("ui.{prefix}.storage_{action}"),
        context,
        "info",
    )
    .await;
}

async fn server_info(state: &AppState, res: &Res) -> Result<ServerInfo, ApiError> {
    let id = res.server_id.ok_or(ApiError::Internal)?;
    let row = servers::find_by_id(&state.db, id)
        .await?
        .ok_or(ApiError::Internal)?;
    Ok(ServerInfo::from_row(&row))
}

/// `shouldBeReadOnlyInUI` of a volume: service and Compose application volumes.
fn volume_read_only(res: &Res) -> bool {
    matches!(
        res.class,
        Class::ServiceApplication | Class::ServiceDatabase
    ) || (res.is_application() && res.str("build_pack") == Some("dockercompose"))
}

/// `shouldBeReadOnlyInUI` of a file storage: a `:ro` mount of a service's Compose file.
fn file_read_only(res: &Res, r: &Record) -> bool {
    match (&res.class, res.service_compose.as_deref()) {
        (Class::ServiceApplication | Class::ServiceDatabase, Some(raw)) => {
            coolify_jobs::services::storage::file_is_read_only(
                Some(raw),
                &res.name,
                text(r, "mount_path").unwrap_or_default(),
            )
        }
        _ => false,
    }
}

fn file_json(state: &AppState, res: &Res, r: &Record, can: bool, dir: &str) -> Value {
    let content = text(r, "content").and_then(|c| {
        state
            .encrypter
            .as_ref()
            .and_then(|e| e.decrypt_string(c).ok())
    });
    let too_large = content.as_deref() == Some(TOO_LARGE);
    let fs_path = text(r, "fs_path").unwrap_or_default();
    let shown_path = fs_path.strip_prefix('.').unwrap_or(fs_path);
    let is_directory = flag(r, "is_directory");
    json!({
        "id": id_of(r),
        "uuid": r.get("uuid"),
        "fs_path": fs_path,
        "shown_path": shown_path,
        "mount_path": r.get("mount_path"),
        "content": if can && !is_directory { content.clone() } else { None },
        "is_directory": is_directory,
        "is_host_file": flag(r, "is_host_file"),
        "is_based_on_git": flag(r, "is_based_on_git"),
        "is_preview_suffix_enabled": r.get("is_preview_suffix_enabled").and_then(Value::as_bool).unwrap_or(true),
        "is_binary": content.as_deref() == Some(BINARY),
        "is_too_large": too_large,
        "is_read_only": file_read_only(res, r) || too_large,
        "can_delete_from_server": !flag(r, "is_host_file") && !paths::is_outside_resource_directory(fs_path, dir),
    })
}

/// `Storage::mount` and what `All` and `FileStorage` read.
async fn show(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid)): P,
) -> ApiResult {
    let res = find(&state, &ctx, &kind, &uuid).await?;
    let can = ctx.is_team_admin();
    let dir = res.workdir();
    let mut volumes = Vec::new();
    for v in parts::persistent_storages(&state.db, &res.morph, res.id).await? {
        let shared = v.get("standalone_sqlite_id").is_some_and(|x| !x.is_null())
            || (res.type_name == "standalone-sqlite"
                && parts::sqlite_volume_connected(
                    &state.db,
                    res.id,
                    text(&v, "name").unwrap_or_default(),
                )
                .await?);
        volumes.push(json!({
            "id": id_of(&v),
            "name": v.get("name"),
            "mount_path": v.get("mount_path"),
            "host_path": v.get("host_path"),
            "is_preview_suffix_enabled": v.get("is_preview_suffix_enabled").and_then(Value::as_bool).unwrap_or(true),
            "is_read_only": volume_read_only(&res) || !can,
            "is_shared": shared,
        }));
    }
    let files = parts::file_storages(&state.db, &res.morph, res.id).await?;
    let git = res.setting_bool("is_preserve_repository_enabled");
    let backup_available = matches!(
        res.class,
        Class::Application | Class::ServiceApplication | Class::ServiceDatabase
    );
    let name_slug = coolify_jobs::compose_parser::php::slug(&res.name);
    let (backup_base, volume_backup_meta) =
        volume_backup_meta(&state, &res, backup_available).await?;
    Ok(ok(json!({
        "service_uuid": res.service_uuid,
        "backup_base": backup_base,
        "volume_backup_meta": volume_backup_meta,
        "can_update": can,
        "is_application": res.is_application(),
        "is_standalone": res.class == Class::Database,
        "is_stack": matches!(res.class, Class::ServiceApplication | Class::ServiceDatabase),
        "name": res.name,
        "uuid": res.uuid,
        "git_based": res.git_based(),
        "build_pack": res.str("build_pack"),
        "supports_preview_suffix": res.git_based() && res.str("git_repository").is_some_and(|v| !v.is_empty()),
        "show_backup_action": backup_available,
        "preserve_repository": git,
        "host_path": dir,
        "default_volume_name": format!("{}-data", if name_slug.is_empty() { "volume".to_owned() } else { name_slug }),
        "volume_unavailable_reason": unavailable_reason(&state, &res).await?,
        "volumes": volumes,
        "files": files.iter().map(|r| file_json(&state, &res, r, can, &dir)).collect::<Vec<_>>(),
    }))
    .into_response())
}

/// `persistentStorageUnavailableReason`.
async fn unavailable_reason(state: &AppState, res: &Res) -> Result<Option<&'static str>, ApiError> {
    if !res.is_application() {
        return Ok(None);
    }
    let found = coolify_db::applications::unported_parts(&state.db, res.id)
        .await?
        .contains(&"additional servers");
    Ok(found.then_some(
        "Applications that use multiple servers cannot have persistent storage because volumes are not shared between servers.",
    ))
}

/// `submitPersistentVolume`.
async fn add_volume(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path((kind, uuid)): P,
    Json(body): Json<Value>,
) -> ApiResult {
    let res = find(&state, &ctx, &kind, &uuid).await?;
    if !ctx.is_team_admin() {
        return Ok(unauthorized());
    }
    let name = body_str(&body, "name");
    let mount_path = body_str(&body, "mount_path");
    let mut errors = Map::new();
    if name.trim().is_empty() {
        errors.insert("name".into(), json!(["The name field is required."]));
    } else if name.chars().count() > 255 {
        errors.insert(
            "name".into(),
            json!(["The name field must not be greater than 255 characters."]),
        );
    } else if !VOLUME_NAME.is_match(name) {
        errors.insert("name".into(), json!(["The volume name must start with an alphanumeric character and contain only alphanumeric characters, dots, hyphens, and underscores."]));
    }
    if mount_path.trim().is_empty() {
        errors.insert(
            "mount_path".into(),
            json!(["The mount path field is required."]),
        );
    }
    if !errors.is_empty() {
        return Ok(field_errors(errors));
    }
    if let Some(reason) = unavailable_reason(&state, &res).await? {
        return Ok(PhpJson(
            StatusCode::CONFLICT,
            json!({"message": "Failed to add volume.", "details": reason}),
        )
        .into_response());
    }
    let row = parts::create_persistent_storage(
        &state.db,
        &res.morph,
        res.id,
        &format!("{}-{name}", res.uuid),
        mount_path,
        state.now(),
    )
    .await?;
    audit(&state, &ctx, &meta, &res, "created", &row, true, json!({})).await;
    Ok(ok(json!({"message": "Volume added successfully"})).into_response())
}

async fn after_create(state: &AppState, res: &Res, row: Record, dir: String) {
    // `LocalFileVolume::created`: `ServerStorageSaveJob`, unless it is a host file or outside the
    // resource directory.
    if flag(&row, "is_host_file")
        || paths::is_outside_resource_directory(text(&row, "fs_path").unwrap_or_default(), &dir)
    {
        return;
    }
    let (Some(jobs), Ok(server)) = (state.jobs.clone(), server_info(state, res).await) else {
        return;
    };
    let uuid = res.uuid.clone();
    tokio::spawn(async move {
        if let Err(e) = jobs.save_file_storage_in(&row, &dir, &server).await {
            tracing::warn!(resource = %uuid, error = %e, "file storage could not be written");
        }
    });
}

/// `submitFileStorage`.
async fn add_file(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path((kind, uuid)): P,
    Json(body): Json<Value>,
) -> ApiResult {
    let res = find(&state, &ctx, &kind, &uuid).await?;
    if !ctx.is_team_admin() {
        return Ok(unauthorized());
    }
    let path = body_str(&body, "file_storage_path");
    if path.trim().is_empty() {
        let mut e = Map::new();
        e.insert(
            "file_storage_path".into(),
            json!(["The file storage path field is required."]),
        );
        return Ok(field_errors(e));
    }
    let dir = res.workdir();
    let source = body_str(&body, "file_storage_source");
    let result: Result<(String, String), String> = async {
        let mount = paths::validate_file_mount_path(path, "file storage path")?;
        let fs_path = if source.trim().is_empty() {
            paths::confine_file_mount_path(&dir, &mount, "file storage path")?
        } else {
            let fs = paths::resolve_host_path(&dir, source, "file storage source path")?;
            let server = server_info(&state, &res)
                .await
                .map_err(|_| "Server not found.".to_owned())?;
            let jobs = state.jobs.as_ref().ok_or("Server work is unavailable.")?;
            jobs.assert_host_path_on_server(&dir, &fs, &server, false)
                .await?;
            fs
        };
        Ok((mount, fs_path))
    }
    .await;
    let (mount, fs_path) = match result {
        Ok(v) => v,
        Err(m) => return Ok(toast_error(StatusCode::BAD_REQUEST, m)),
    };
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let content = body.get("file_storage_content").and_then(Value::as_str);
    let row = parts::create_file_storage(
        &state.db,
        &res.morph,
        res.id,
        &NewFileVolume {
            fs_path,
            mount_path: mount,
            content_encrypted: content.map(|c| enc.encrypt_string(c)),
            is_directory: false,
            is_host_file: false,
            chmod: None,
        },
        state.now(),
    )
    .await?;
    audit(&state, &ctx, &meta, &res, "created", &row, false, json!({})).await;
    after_create(&state, &res, row, dir).await;
    Ok(ok(json!({"message": "File mount added successfully"})).into_response())
}

/// `submitHostFileStorage`.
async fn add_host_file(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path((kind, uuid)): P,
    Json(body): Json<Value>,
) -> ApiResult {
    let res = find(&state, &ctx, &kind, &uuid).await?;
    if !ctx.is_team_admin() {
        return Ok(unauthorized());
    }
    let source = body_str(&body, "host_file_storage_source");
    let destination = body_str(&body, "host_file_storage_destination");
    let mut errors = Map::new();
    if source.trim().is_empty() {
        errors.insert(
            "host_file_storage_source".into(),
            json!(["The host file storage source field is required."]),
        );
    }
    if destination.trim().is_empty() {
        errors.insert(
            "host_file_storage_destination".into(),
            json!(["The host file storage destination field is required."]),
        );
    }
    if !errors.is_empty() {
        return Ok(field_errors(errors));
    }
    let checked =
        paths::validate_host_file_mount_path(source, "host file source path").and_then(|s| {
            paths::validate_file_mount_path(destination, "host file destination path")
                .map(|d| (s, d))
        });
    let (fs_path, mount) = match checked {
        Ok(v) => v,
        Err(m) => return Ok(toast_error(StatusCode::BAD_REQUEST, m)),
    };
    let row = parts::create_file_storage(
        &state.db,
        &res.morph,
        res.id,
        &NewFileVolume {
            fs_path,
            mount_path: mount,
            content_encrypted: None,
            is_directory: false,
            is_host_file: true,
            chmod: None,
        },
        state.now(),
    )
    .await?;
    audit(&state, &ctx, &meta, &res, "created", &row, false, json!({})).await;
    Ok(ok(json!({"message": "Host file mount added successfully"})).into_response())
}

/// `submitFileStorageDirectory`.
async fn add_directory(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path((kind, uuid)): P,
    Json(body): Json<Value>,
) -> ApiResult {
    let res = find(&state, &ctx, &kind, &uuid).await?;
    if !ctx.is_team_admin() {
        return Ok(unauthorized());
    }
    let source = body_str(&body, "file_storage_directory_source");
    let destination = body_str(&body, "file_storage_directory_destination");
    let mut errors = Map::new();
    if source.trim().is_empty() {
        errors.insert(
            "file_storage_directory_source".into(),
            json!(["The file storage directory source field is required."]),
        );
    }
    if destination.trim().is_empty() {
        errors.insert(
            "file_storage_directory_destination".into(),
            json!(["The file storage directory destination field is required."]),
        );
    }
    if !errors.is_empty() {
        return Ok(field_errors(errors));
    }
    let dir = res.workdir();
    let result: Result<(String, String), String> = async {
        let fs = paths::resolve_host_path(&dir, source, "storage source path")?;
        let mount = paths::validate_file_mount_path(destination, "storage destination path")?;
        let server = server_info(&state, &res)
            .await
            .map_err(|_| "Server not found.".to_owned())?;
        let jobs = state.jobs.as_ref().ok_or("Server work is unavailable.")?;
        jobs.assert_host_path_on_server(&dir, &fs, &server, true)
            .await?;
        Ok((fs, mount))
    }
    .await;
    let (fs_path, mount) = match result {
        Ok(v) => v,
        Err(m) => return Ok(toast_error(StatusCode::BAD_REQUEST, m)),
    };
    let row = parts::create_file_storage(
        &state.db,
        &res.morph,
        res.id,
        &NewFileVolume {
            fs_path,
            mount_path: mount,
            content_encrypted: None,
            is_directory: true,
            is_host_file: false,
            chmod: None,
        },
        state.now(),
    )
    .await?;
    audit(&state, &ctx, &meta, &res, "created", &row, false, json!({})).await;
    after_create(&state, &res, row, dir).await;
    Ok(ok(json!({"message": "Directory mount added successfully"})).into_response())
}

async fn volume_of(state: &AppState, res: &Res, id: i64) -> Result<Option<Record>, ApiError> {
    Ok(parts::persistent_storages(&state.db, &res.morph, res.id)
        .await?
        .into_iter()
        .find(|v| id_of(v) == id))
}

async fn file_of(state: &AppState, res: &Res, id: i64) -> Result<Option<Record>, ApiError> {
    Ok(parts::file_storages(&state.db, &res.morph, res.id)
        .await?
        .into_iter()
        .find(|v| id_of(v) == id))
}

/// `All::submit`.
async fn update_volume(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path((kind, uuid, id)): PI,
    Json(body): Json<Value>,
) -> ApiResult {
    let res = find(&state, &ctx, &kind, &uuid).await?;
    if !ctx.is_team_admin() {
        return Ok(unauthorized());
    }
    let name = body_str(&body, "name");
    let mount_path = body_str(&body, "mount_path");
    let mut errors = Map::new();
    if name.trim().is_empty() {
        errors.insert("name".into(), json!(["The name field is required."]));
    } else if !VOLUME_NAME.is_match(name) {
        errors.insert("name".into(), json!(["The volume name must start with an alphanumeric character and contain only alphanumeric characters, dots, hyphens, and underscores."]));
    }
    if mount_path.trim().is_empty() {
        errors.insert("mount_path".into(), json!(["The mount field is required."]));
    } else if !DIRECTORY_PATH.is_match(mount_path) {
        errors.insert(
            "mount_path".into(),
            json!(["Mount path must start with / and only contain safe path characters."]),
        );
    }
    if !errors.is_empty() {
        return Ok(field_errors(errors));
    }
    let Some(v) = volume_of(&state, &res, id).await? else {
        return Ok(toast_error(StatusCode::NOT_FOUND, "Storage not found."));
    };
    if volume_read_only(&res) {
        return Ok(toast_error(
            StatusCode::CONFLICT,
            "This volume is read-only.",
        ));
    }
    let shared = v.get("standalone_sqlite_id").is_some_and(|x| !x.is_null());
    let mut values: Vec<(&'static str, C)> = Vec::new();
    let mut changed: Vec<&str> = Vec::new();
    if !shared && text(&v, "name") != Some(name) {
        values.push(("name", C::Text(Some(name.to_owned()))));
        changed.push("name");
    }
    if text(&v, "mount_path") != Some(mount_path) {
        values.push(("mount_path", C::Text(Some(mount_path.to_owned()))));
        changed.push("mount_path");
    }
    let suffix = body
        .get("is_preview_suffix_enabled")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    if v.get("is_preview_suffix_enabled").and_then(Value::as_bool) != Some(suffix) {
        values.push(("is_preview_suffix_enabled", C::Bool(suffix)));
        changed.push("is_preview_suffix_enabled");
    }
    parts::update_row(&state.db, VOLUME_TABLE, id, &values, state.now()).await?;
    if !changed.is_empty() {
        let row = volume_of(&state, &res, id).await?.unwrap_or(v);
        audit(
            &state,
            &ctx,
            &meta,
            &res,
            "updated",
            &row,
            true,
            json!({"changed_fields": changed}),
        )
        .await;
    }
    Ok(ok(json!({"message": "Storage updated successfully"})).into_response())
}

/// `All::delete`.
async fn delete_volume(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path((kind, uuid, id)): PI,
    Json(body): Json<Value>,
) -> ApiResult {
    let res = find(&state, &ctx, &kind, &uuid).await?;
    if !ctx.is_team_admin() {
        return Ok(unauthorized());
    }
    if !crate::ui::password_confirmed(
        &state,
        ctx.user_id,
        body.get("password").and_then(Value::as_str),
    )
    .await?
    {
        return Err(ApiError::Message(
            StatusCode::UNPROCESSABLE_ENTITY,
            "The provided password is incorrect.",
        ));
    }
    let Some(v) = volume_of(&state, &res, id).await? else {
        return Ok(toast_error(StatusCode::NOT_FOUND, "Storage not found."));
    };
    if v.get("standalone_sqlite_id").is_some_and(|x| !x.is_null()) {
        return Ok(toast_error(
            StatusCode::CONFLICT,
            "This volume is connected to a SQLite database. Unlink it on the SQLite database page.",
        ));
    }
    if parts::volume_has_backups(&state.db, parts::LOCAL_PERSISTENT_VOLUME_MORPH, id).await? {
        return Ok(toast_error(
            StatusCode::CONFLICT,
            "Delete this volume backup schedule and its archives before deleting the volume.",
        ));
    }
    let delete_docker = body
        .get("selected")
        .and_then(Value::as_array)
        .is_some_and(|a| a.iter().any(|x| x == "deleteDockerVolume"));
    if delete_docker {
        let server = server_info(&state, &res).await?;
        let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
        let name = text(&v, "name").unwrap_or_default();
        if let Err(e) = jobs.remove_docker_volume(name, &server).await {
            return Ok(toast_error(
                StatusCode::BAD_REQUEST,
                format!("Failed to delete the Docker volume: {e}"),
            ));
        }
    }
    parts::delete_row(&state.db, VOLUME_TABLE, id).await?;
    audit(
        &state,
        &ctx,
        &meta,
        &res,
        "deleted",
        &v,
        true,
        json!({"docker_volume_deleted": delete_docker}),
    )
    .await;
    Ok(ok(json!({"message": null})).into_response())
}

fn editable_file(r: &Record) -> Option<(StatusCode, &'static str)> {
    if flag(r, "is_host_file") {
        return Some((
            StatusCode::CONFLICT,
            "Host file mounts are bind-only and cannot be edited from the UI.",
        ));
    }
    None
}

/// `FileStorage::submit`.
async fn save_file(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path((kind, uuid, id)): PI,
    Json(body): Json<Value>,
) -> ApiResult {
    let res = find(&state, &ctx, &kind, &uuid).await?;
    if !ctx.is_team_admin() {
        return Ok(unauthorized());
    }
    let Some(r) = file_of(&state, &res, id).await? else {
        return Ok(toast_error(StatusCode::NOT_FOUND, "Storage not found."));
    };
    if let Some((code, message)) = editable_file(&r) {
        return Ok(toast_error(code, message));
    }
    let (values, changed) = file_changes(&state, &r, &body)?;
    if is_too_large(&state, &r) {
        return Ok(toast_error(
            StatusCode::CONFLICT,
            "File on server is too large to edit from the UI.",
        ));
    }
    parts::update_row(&state.db, FILE_TABLE, id, &values, state.now()).await?;
    let fresh = file_of(&state, &res, id)
        .await?
        .unwrap_or_else(|| r.clone());
    let server = server_info(&state, &res).await?;
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    if let Err(message) = jobs
        .save_file_storage_in(&fresh, &res.workdir(), &server)
        .await
    {
        // `setRawAttributes($original)`: back to what it was.
        let original: Vec<(&'static str, C)> = vec![
            ("content", C::Text(text(&r, "content").map(str::to_owned))),
            ("is_based_on_git", C::Bool(flag(&r, "is_based_on_git"))),
            (
                "is_preview_suffix_enabled",
                C::Bool(
                    r.get("is_preview_suffix_enabled")
                        .and_then(Value::as_bool)
                        .unwrap_or(true),
                ),
            ),
        ];
        parts::update_row(&state.db, FILE_TABLE, id, &original, state.now()).await?;
        return Ok(toast_error(StatusCode::BAD_REQUEST, message));
    }
    if !changed.is_empty() {
        audit(
            &state,
            &ctx,
            &meta,
            &res,
            "updated",
            &fresh,
            false,
            json!({"changed_fields": changed}),
        )
        .await;
    }
    Ok(ok(json!({"message": "File updated."})).into_response())
}

fn is_too_large(state: &AppState, r: &Record) -> bool {
    text(r, "content")
        .and_then(|c| {
            state
                .encrypter
                .as_ref()
                .and_then(|e| e.decrypt_string(c).ok())
        })
        .as_deref()
        == Some(TOO_LARGE)
}

type Changes<'a> = (Vec<(&'static str, C)>, Vec<&'a str>);

fn file_changes<'a>(state: &AppState, r: &Record, body: &Value) -> Result<Changes<'a>, ApiError> {
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let current = text(r, "content").and_then(|c| enc.decrypt_string(c).ok());
    let new = if flag(r, "is_directory") {
        None
    } else {
        body.get("content")
            .and_then(Value::as_str)
            .map(str::to_owned)
    };
    let mut values: Vec<(&'static str, C)> = Vec::new();
    let mut changed: Vec<&str> = Vec::new();
    if new != current {
        values.push((
            "content",
            C::Text(new.as_deref().map(|c| enc.encrypt_string(c))),
        ));
        changed.push("content");
    }
    let git = body
        .get("is_based_on_git")
        .and_then(Value::as_bool)
        .unwrap_or(flag(r, "is_based_on_git"));
    if git != flag(r, "is_based_on_git") {
        values.push(("is_based_on_git", C::Bool(git)));
        changed.push("is_based_on_git");
    }
    let suffix_now = r
        .get("is_preview_suffix_enabled")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let suffix = body
        .get("is_preview_suffix_enabled")
        .and_then(Value::as_bool)
        .unwrap_or(suffix_now);
    if suffix != suffix_now {
        values.push(("is_preview_suffix_enabled", C::Bool(suffix)));
        changed.push("is_preview_suffix_enabled");
    }
    Ok((values, changed))
}

/// `FileStorage::instantSave`: no write to the server.
async fn instant_file(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path((kind, uuid, id)): PI,
    Json(body): Json<Value>,
) -> ApiResult {
    let res = find(&state, &ctx, &kind, &uuid).await?;
    if !ctx.is_team_admin() {
        return Ok(unauthorized());
    }
    let Some(r) = file_of(&state, &res, id).await? else {
        return Ok(toast_error(StatusCode::NOT_FOUND, "Storage not found."));
    };
    if let Some((code, message)) = editable_file(&r) {
        return Ok(toast_error(code, message));
    }
    if is_too_large(&state, &r) {
        return Ok(toast_error(
            StatusCode::CONFLICT,
            "File on server is too large to edit from the UI.",
        ));
    }
    let (values, changed) = file_changes(&state, &r, &body)?;
    parts::update_row(&state.db, FILE_TABLE, id, &values, state.now()).await?;
    if !changed.is_empty() {
        let fresh = file_of(&state, &res, id).await?.unwrap_or(r);
        audit(
            &state,
            &ctx,
            &meta,
            &res,
            "updated",
            &fresh,
            false,
            json!({"changed_fields": changed}),
        )
        .await;
    }
    Ok(ok(json!({"message": "File updated."})).into_response())
}

/// `FileStorage::loadStorageOnServer`.
async fn load_file(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid, id)): PI,
) -> ApiResult {
    let res = find(&state, &ctx, &kind, &uuid).await?;
    if !ctx.is_team_admin() {
        return Ok(unauthorized());
    }
    let Some(r) = file_of(&state, &res, id).await? else {
        return Ok(toast_error(StatusCode::NOT_FOUND, "Storage not found."));
    };
    if flag(&r, "is_host_file") {
        return Ok(toast_error(
            StatusCode::CONFLICT,
            "Host file mounts are bind-only and cannot be loaded from the server.",
        ));
    }
    let server = server_info(&state, &res).await?;
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    match jobs.load_file_storage_in(&r, &res.workdir(), &server).await {
        Ok(Some(content)) => {
            parts::update_row(
                &state.db,
                FILE_TABLE,
                id,
                &[
                    ("content", C::Text(Some(enc.encrypt_string(&content)))),
                    ("is_directory", C::Bool(false)),
                ],
                state.now(),
            )
            .await?;
        }
        Ok(None) => {}
        Err(m) => return Ok(toast_error(StatusCode::BAD_REQUEST, m)),
    }
    Ok(ok(json!({"message": "File storage loaded from server."})).into_response())
}

/// `FileStorage::convertToDirectory` and `convertToFile`.
async fn convert_file(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path((kind, uuid, id)): PI,
    Json(body): Json<Value>,
) -> ApiResult {
    let res = find(&state, &ctx, &kind, &uuid).await?;
    if !ctx.is_team_admin() {
        return Ok(unauthorized());
    }
    let Some(r) = file_of(&state, &res, id).await? else {
        return Ok(toast_error(StatusCode::NOT_FOUND, "Storage not found."));
    };
    let to_directory = body.get("to").and_then(Value::as_str) == Some("directory");
    if !to_directory
        && parts::volume_has_backups(&state.db, parts::LOCAL_FILE_VOLUME_MORPH, id).await?
    {
        return Ok(toast_error(
            StatusCode::CONFLICT,
            "Delete this directory backup schedule and its archives before converting it to a file.",
        ));
    }
    if flag(&r, "is_host_file") {
        return Ok(toast_error(
            StatusCode::CONFLICT,
            "Host file mounts are bind-only and cannot be converted.",
        ));
    }
    let server = server_info(&state, &res).await?;
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    let dir = res.workdir();
    if let Err(m) = jobs.delete_file_storage_in(&r, &dir, &server).await {
        return Ok(toast_error(StatusCode::BAD_REQUEST, m));
    }
    let mut values: Vec<(&'static str, C)> = vec![
        ("is_directory", C::Bool(to_directory)),
        ("content", C::Text(None)),
    ];
    let mut changed = vec!["is_directory"];
    if text(&r, "content").is_some() {
        changed.push("content");
    }
    let git_after = if to_directory {
        false
    } else if res.setting_bool("is_preserve_repository_enabled") {
        true
    } else {
        flag(&r, "is_based_on_git")
    };
    if git_after != flag(&r, "is_based_on_git") {
        values.push(("is_based_on_git", C::Bool(git_after)));
        changed.push("is_based_on_git");
    }
    let mut next = r.clone();
    next.insert("is_directory".into(), Value::Bool(to_directory));
    next.insert("content".into(), Value::Null);
    next.insert("is_based_on_git".into(), Value::Bool(git_after));
    if let Err(m) = jobs.save_file_storage_in(&next, &dir, &server).await {
        return Ok(toast_error(StatusCode::BAD_REQUEST, m));
    }
    parts::update_row(&state.db, FILE_TABLE, id, &values, state.now()).await?;
    let operation = if to_directory {
        "converted_to_directory"
    } else {
        "converted_to_file"
    };
    audit(
        &state,
        &ctx,
        &meta,
        &res,
        "updated",
        &next,
        false,
        json!({"changed_fields": changed, "operation": operation}),
    )
    .await;
    Ok(ok(json!({"message": null})).into_response())
}

/// `FileStorage::delete`.
async fn delete_file(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path((kind, uuid, id)): PI,
    Json(body): Json<Value>,
) -> ApiResult {
    let res = find(&state, &ctx, &kind, &uuid).await?;
    if !ctx.is_team_admin() {
        return Ok(unauthorized());
    }
    if !crate::ui::password_confirmed(
        &state,
        ctx.user_id,
        body.get("password").and_then(Value::as_str),
    )
    .await?
    {
        return Err(ApiError::Message(
            StatusCode::UNPROCESSABLE_ENTITY,
            "The provided password is incorrect.",
        ));
    }
    let Some(r) = file_of(&state, &res, id).await? else {
        return Ok(toast_error(StatusCode::NOT_FOUND, "Storage not found."));
    };
    if parts::volume_has_backups(&state.db, parts::LOCAL_FILE_VOLUME_MORPH, id).await? {
        return Ok(toast_error(
            StatusCode::CONFLICT,
            "Delete this directory backup schedule and its archives before deleting the directory.",
        ));
    }
    let dir = res.workdir();
    let mut message = if flag(&r, "is_directory") {
        "Directory deleted."
    } else if flag(&r, "is_host_file") {
        "Host file mount removed."
    } else {
        "File deleted."
    };
    let can_delete = !flag(&r, "is_host_file")
        && !paths::is_outside_resource_directory(text(&r, "fs_path").unwrap_or_default(), &dir);
    let from_server = can_delete
        && body
            .get("selected")
            .and_then(Value::as_array)
            .is_some_and(|a| a.iter().any(|x| x == "permanently_delete"));
    if from_server {
        message = if flag(&r, "is_directory") {
            "Directory deleted from the server."
        } else {
            "File deleted from the server."
        };
        let server = server_info(&state, &res).await?;
        let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
        if let Err(m) = jobs.delete_file_storage_in(&r, &dir, &server).await {
            return Ok(toast_error(StatusCode::BAD_REQUEST, m));
        }
    }
    parts::delete_row(&state.db, FILE_TABLE, id).await?;
    audit(
        &state,
        &ctx,
        &meta,
        &res,
        "deleted",
        &r,
        false,
        json!({"deleted_from_server": from_server}),
    )
    .await;
    Ok(ok(json!({"message": message})).into_response())
}
