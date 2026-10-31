//! `Project\Shared\FileBrowser`: files inside the running container of a resource.
//!
//! Upstream keeps the browsing state in the Livewire component; here the page sends it back and
//! `guard()` checks it on every request. Downloads to an S3 file storage
//! (`FileDownloadStorageService`) are not ported: the file is always streamed from the server.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::{Multipart, Path, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use coolify_db::servers;
use coolify_db::services::{self as svc, SubKind};
use coolify_jobs::Jobs;
use coolify_jobs::file_browser::{
    ContainerFiles, ListError, MAX_DOWNLOAD_SIZE, guess_language, is_valid_container_name,
};
use coolify_jobs::server::ServerInfo;
use coolify_remote::Target;
use serde_json::{Value, json};

use super::{Class, Res, find};
use crate::auth::AuthCtx;
use crate::response::{ApiError, ApiResult, ok};
use crate::state::AppState;

/// `FileBrowserDownloadController::TOKEN_TTL_SECONDS`.
const TOKEN_TTL: Duration = Duration::from_secs(60);

struct Download {
    created: Instant,
    user_id: i64,
    team_id: i64,
    kind: String,
    uuid: String,
    container: String,
    path: String,
    name: String,
    is_dir: bool,
    use_helper: bool,
}

/// `Cache::put(FileBrowserDownloadController::cacheKey($token), ...)`.
static DOWNLOADS: LazyLock<Mutex<HashMap<String, Download>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/ui/resources/{kind}/{uuid}/files/load", post(load))
        .route("/ui/resources/{kind}/{uuid}/files/list", post(list))
        .route("/ui/resources/{kind}/{uuid}/files/action", post(action))
        .route("/ui/resources/{kind}/{uuid}/files/upload", post(upload))
        .route("/ui/resources/{kind}/{uuid}/files/download", post(download))
        .route("/project/files/download/{token}", get(stream))
}

fn message_error(message: impl Into<String>) -> Response {
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        Json(json!({"message": message.into()})),
    )
        .into_response()
}

async fn files_resource(
    state: &AppState,
    ctx: &AuthCtx,
    kind: &str,
    uuid: &str,
) -> Result<Res, ApiError> {
    let res = find(state, ctx, kind, uuid).await?;
    if !matches!(
        res.class,
        Class::Application | Class::Service | Class::Database
    ) {
        return Err(super::NOT_FOUND);
    }
    // `$this->authorize('canAccessTerminal')`.
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    Ok(res)
}

/// `$this->resource->isRunning()`.
async fn is_running(state: &AppState, res: &Res) -> Result<bool, ApiError> {
    Ok(match res.class {
        Class::Application => res.str("status").is_some_and(|s| s.starts_with("running")),
        Class::Service => {
            let s = svc::find_in_team(&state.db, res.team_id, &res.uuid)
                .await?
                .ok_or(super::NOT_FOUND)?;
            crate::routes::services::status_of(state, &s)
                .await?
                .is_some_and(|status| status.contains("running"))
        }
        _ => res.str("status").is_some_and(|s| s.contains("running")),
    })
}

/// The connection of `server()`.
async fn target(state: &AppState, res: &Res) -> Result<(Jobs, Target), ApiError> {
    let jobs = state.jobs.clone().ok_or(ApiError::NotPorted)?;
    let id = res.server_id.ok_or(ApiError::NotPorted)?;
    let row = servers::find_by_id(&state.db, id)
        .await?
        .ok_or(ApiError::NotPorted)?;
    let t = ServerInfo::from_row(&row)
        .target(&jobs)
        .await
        .map_err(|e| {
            tracing::warn!(error = %e, "file browser: no connection to the server");
            ApiError::Message(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Could not connect to the server.",
            )
        })?;
    Ok((jobs, t))
}

/// `resolveContainers()`.
async fn containers(
    state: &AppState,
    res: &Res,
    jobs: &Jobs,
    t: &Target,
) -> Result<Vec<String>, ApiError> {
    let names: Vec<String> = match res.class {
        Class::Service => {
            let mut names = Vec::new();
            for kind in [SubKind::Application, SubKind::Database] {
                for sub in svc::subs(&state.db, kind, res.id).await? {
                    let status = sub
                        .raw
                        .get("status")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    if status.contains("running") {
                        names.push(format!("{}-{}", sub.name, res.uuid));
                    }
                }
            }
            names
        }
        Class::Application => jobs
            .application_containers(t, &res.uuid, false)
            .await
            .map_err(|e| {
                tracing::warn!(error = %e, "file browser: containers could not be read");
                ApiError::Message(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Could not read the containers of this resource.",
                )
            })?
            .iter()
            .filter(|c| c.get("State").and_then(Value::as_str) == Some("running"))
            .filter_map(|c| c.get("Names").and_then(Value::as_str).map(str::to_owned))
            .collect(),
        _ => vec![res.uuid.clone()],
    };
    Ok(names
        .into_iter()
        .filter(|n| is_valid_container_name(n))
        .collect())
}

/// `childPath()`.
fn child_path(current: &str, name: &str) -> Result<String, String> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains('/')
        || name.contains('\\')
        || name.contains('\0')
    {
        return Err("Invalid file name.".to_owned());
    }
    coolify_jobs::compose_parser::validate::validate_shell_safe_path(name, "file name")?;
    Ok(format!("{}/{name}", current.trim_end_matches('/')))
}

/// `loadEntries()`: entries, helper flag and the unavailable state.
async fn entries_json(
    jobs: &Jobs,
    t: &Target,
    container: &str,
    path: &str,
    use_helper: bool,
) -> Value {
    let mut helper = use_helper;
    let mut unavailable = false;
    let mut helper_error: Option<String> = None;
    let mut files = ContainerFiles::new(jobs, t, container, helper);
    let mut listed = files.list(path).await;
    if let Err(ListError::ShellUnavailable(_)) = &listed
        && !helper
    {
        helper = true;
        files = ContainerFiles::new(jobs, t, container, true);
        listed = files.list(path).await;
    }
    let entries = match listed {
        Ok(e) => e,
        Err(ListError::ShellUnavailable(message)) => {
            tracing::warn!(container, error = %message, "File browser helper container failed");
            unavailable = true;
            let pull = [
                "Unable to find image",
                "failed to resolve reference",
                "pull access denied",
                "manifest unknown",
            ];
            let lower = message.to_lowercase();
            if pull.iter().any(|p| lower.contains(&p.to_lowercase())) {
                helper_error = Some(format!(
                    "The server could not pull the Coolify helper image ({}). Check that the server can reach the registry.",
                    coolify_jobs::file_browser::helper_image_name(jobs)
                ));
            }
            Vec::new()
        }
    };
    json!({
        "current_path": path,
        "use_helper": helper,
        "shell_unavailable": unavailable,
        "helper_error": helper_error,
        "entries": entries.iter().map(|e| json!({
            "name": e.name,
            "type": e.kind,
            "size": e.size,
            "mtime": e.mtime,
            "perms": e.perms,
            "owner": e.owner,
            "group": e.group,
        })).collect::<Vec<_>>(),
    })
}

/// `mount()`: what the page needs before `loadFiles`, then `loadFiles()`.
async fn load(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid)): Path<(String, String)>,
) -> ApiResult {
    let res = files_resource(&state, &ctx, &kind, &uuid).await?;
    if !is_running(&state, &res).await? {
        return Ok(ok(json!({"container_running": false})).into_response());
    }
    let (jobs, t) = target(&state, &res).await?;
    let available = containers(&state, &res, &jobs, &t).await?;
    let Some(container) = available.first().cloned() else {
        return Ok(ok(json!({"container_running": false})).into_response());
    };
    let root = ContainerFiles::new(&jobs, &t, &container, false)
        .default_root()
        .await;
    let mut body = entries_json(&jobs, &t, &container, &root, false).await;
    body["container_running"] = json!(true);
    body["available_containers"] = json!(available);
    body["container"] = json!(container);
    Ok(ok(body).into_response())
}

/// `guard()`: `Err` with the toast message when the container is gone.
async fn guard(
    state: &AppState,
    res: &Res,
    container: &str,
) -> Result<Result<(Jobs, Target), &'static str>, ApiError> {
    if !is_running(state, res).await? {
        return Ok(Err("Container is not running."));
    }
    let (jobs, t) = target(state, res).await?;
    let available = containers(state, res, &jobs, &t).await?;
    if !available.iter().any(|c| c == container) {
        return Ok(Err("Container is not running."));
    }
    Ok(Ok((jobs, t)))
}

fn text<'a>(body: &'a Value, key: &str) -> &'a str {
    body.get(key).and_then(Value::as_str).unwrap_or_default()
}

/// `selectContainer`, `open`, `goTo` and `refresh`.
async fn list(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> ApiResult {
    let res = files_resource(&state, &ctx, &kind, &uuid).await?;
    let container = text(&body, "container");
    let (jobs, t) = match guard(&state, &res, container).await? {
        Ok(v) => v,
        Err(m) => return Ok(message_error(m)),
    };
    let use_helper = body
        .get("use_helper")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let current = text(&body, "path");
    let target_path = match text(&body, "op") {
        "select" => {
            ContainerFiles::new(&jobs, &t, container, false)
                .default_root()
                .await
        }
        "open" => match child_path(current, text(&body, "name")) {
            Ok(p) => p,
            Err(e) => return Ok(message_error(e)),
        },
        "go" => {
            let to = text(&body, "to");
            if !to.starts_with('/') {
                return Ok(ok(json!({"unchanged": true})).into_response());
            }
            if to == "/" {
                "/".to_owned()
            } else {
                to.trim_end_matches('/').to_owned()
            }
        }
        _ => current.to_owned(),
    };
    let helper = use_helper && text(&body, "op") != "select";
    Ok(ok(entries_json(&jobs, &t, container, &target_path, helper).await).into_response())
}

/// `createDirectory`, `createFile`, `chmodEntry`, `renameEntry`, `deleteEntry`, `openEditor`, `saveEditor`.
async fn action(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> ApiResult {
    let res = files_resource(&state, &ctx, &kind, &uuid).await?;
    let container = text(&body, "container");
    let (jobs, t) = match guard(&state, &res, container).await? {
        Ok(v) => v,
        Err(m) => return Ok(message_error(m)),
    };
    let use_helper = body
        .get("use_helper")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let current = text(&body, "path");
    let name = text(&body, "name");
    let mut files = ContainerFiles::new(&jobs, &t, container, use_helper);
    let op = text(&body, "op");
    if op == "save" {
        let path = text(&body, "editing_path");
        return Ok(match files.write(path, text(&body, "content")).await {
            Ok(()) => {
                let mut r = entries_json(&jobs, &t, container, current, use_helper).await;
                r["message"] = json!("Saved.");
                ok(r).into_response()
            }
            Err(e) => message_error(e),
        });
    }
    let child = match child_path(current, name) {
        Ok(p) => p,
        Err(e) => return Ok(message_error(e)),
    };
    if op == "edit" {
        return Ok(match files.read(&child).await {
            Ok(Ok(content)) => ok(json!({
                "content": content,
                "language": guess_language(name),
                "editing_path": child,
            }))
            .into_response(),
            // `openEditor`: any `RuntimeException` of `read()`.
            Ok(Err(())) | Err(_) => message_error(
                "Can't edit this file - it's binary or larger than 5 MB. Download it instead.",
            ),
        });
    }
    let (result, message) = match op {
        "create_directory" => (files.make_directory(&child).await, "Folder created."),
        "create_file" => (files.create_file(&child).await, "File created."),
        "chmod" => (
            files.chmod(&child, text(&body, "mode")).await,
            "Permissions updated.",
        ),
        "rename" => {
            let to = match child_path(current, text(&body, "new_name")) {
                Ok(p) => p,
                Err(e) => return Ok(message_error(e)),
            };
            (files.rename(&child, &to).await, "Renamed.")
        }
        "delete" => (files.delete(&child).await, "Deleted."),
        _ => return Err(super::NOT_FOUND),
    };
    Ok(match result {
        Ok(()) => {
            let mut r = entries_json(&jobs, &t, container, current, use_helper).await;
            r["message"] = json!(message);
            ok(r).into_response()
        }
        Err(e) => message_error(e),
    })
}

/// `sanitizeUploadName()`.
fn upload_name(name: &str) -> Result<String, String> {
    let replaced = name.replace('\\', "/");
    let base = replaced.rsplit('/').next().unwrap_or_default();
    if base.is_empty() || base.chars().all(|c| c == '.') {
        return Err("Invalid file name.".to_owned());
    }
    Ok(base.to_owned())
}

/// `uploadFile()`.
async fn upload(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid)): Path<(String, String)>,
    mut multipart: Multipart,
) -> ApiResult {
    let res = files_resource(&state, &ctx, &kind, &uuid).await?;
    let mut fields: HashMap<String, String> = HashMap::new();
    let local =
        std::env::temp_dir().join(format!("coolify-files-{}", coolify_domain::new_public_id()));
    let mut original = None;
    let mut written: u64 = 0;
    while let Ok(Some(mut field)) = multipart.next_field().await {
        let name = field.name().unwrap_or_default().to_owned();
        if name == "file" {
            original = field.file_name().map(str::to_owned);
            let mut f = std::fs::File::create(&local).map_err(|_| ApiError::Internal)?;
            while let Ok(Some(chunk)) = field.chunk().await {
                use std::io::Write as _;
                written += chunk.len() as u64;
                f.write_all(&chunk).map_err(|_| ApiError::Internal)?;
            }
        } else if let Ok(text) = field.text().await {
            fields.insert(name, text);
        }
    }
    let cleanup = || {
        let _ = std::fs::remove_file(&local);
    };
    // `'upload' => 'required|file|max:1048576'`.
    let Some(original) = original else {
        return Ok(message_error("The upload field is required."));
    };
    if written > 1_048_576 * 1024 {
        cleanup();
        return Ok(message_error(
            "The upload field must not be greater than 1048576 kilobytes.",
        ));
    }
    let container = fields.get("container").cloned().unwrap_or_default();
    let (jobs, t) = match guard(&state, &res, &container).await? {
        Ok(v) => v,
        Err(m) => {
            cleanup();
            return Ok(message_error(m));
        }
    };
    let current = fields.get("path").cloned().unwrap_or_default();
    let use_helper = fields.get("use_helper").is_some_and(|v| v == "true");
    let result = match upload_name(&original).and_then(|n| child_path(&current, &n)) {
        Ok(dest) => {
            ContainerFiles::new(&jobs, &t, &container, use_helper)
                .upload(&local, &dest)
                .await
        }
        Err(e) => Err(e),
    };
    cleanup();
    Ok(match result {
        Ok(()) => {
            let mut r = entries_json(&jobs, &t, &container, &current, use_helper).await;
            r["message"] = json!("Uploaded.");
            ok(r).into_response()
        }
        Err(e) => message_error(e),
    })
}

/// `formatBytes()` for the size message.
fn one_gigabyte() -> &'static str {
    "1 GB"
}

/// `download($name)`: the link for one download.
async fn download(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> ApiResult {
    let res = files_resource(&state, &ctx, &kind, &uuid).await?;
    let container = text(&body, "container");
    let (jobs, t) = match guard(&state, &res, container).await? {
        Ok(v) => v,
        Err(m) => return Ok(message_error(m)),
    };
    let use_helper = body
        .get("use_helper")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let name = text(&body, "name");
    let path = match child_path(text(&body, "path"), name) {
        Ok(p) => p,
        Err(e) => return Ok(message_error(e)),
    };
    let mut files = ContainerFiles::new(&jobs, &t, container, use_helper);
    let (is_dir, size) = match files.download_info(&path).await {
        Ok(v) => v,
        Err(e) => return Ok(message_error(e)),
    };
    if size > MAX_DOWNLOAD_SIZE {
        return Ok(message_error(format!(
            "{} is larger than {} and cannot be downloaded here. Use the terminal or a volume backup instead.",
            if is_dir { "This folder" } else { "This file" },
            one_gigabyte()
        )));
    }
    let token = crate::ui_applications::random_str(64);
    let entry = Download {
        created: Instant::now(),
        user_id: ctx.user_id,
        team_id: ctx.team_id,
        kind,
        uuid,
        container: container.to_owned(),
        path,
        name: if is_dir {
            format!("{name}.tar.gz")
        } else {
            name.to_owned()
        },
        is_dir,
        use_helper,
    };
    if let Ok(mut map) = DOWNLOADS.lock() {
        map.retain(|_, d| d.created.elapsed() < TOKEN_TTL);
        map.insert(token.clone(), entry);
    }
    Ok(ok(json!({"url": format!("/project/files/download/{token}")})).into_response())
}

/// `FileBrowserDownloadController`.
async fn stream(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(token): Path<String>,
) -> Result<Response, ApiError> {
    let not_found = ApiError::Abort(StatusCode::NOT_FOUND, "");
    let entry = DOWNLOADS.lock().ok().and_then(|mut m| m.remove(&token));
    let Some(entry) = entry.filter(|d| d.created.elapsed() < TOKEN_TTL) else {
        return Err(not_found);
    };
    if entry.user_id != ctx.user_id {
        return Err(not_found);
    }
    if entry.team_id != ctx.team_id {
        return Err(ApiError::Unauthorized);
    }
    let res = files_resource(&state, &ctx, &entry.kind, &entry.uuid).await?;
    let (jobs, t) = target(&state, &res).await?;
    let mut files = ContainerFiles::new(&jobs, &t, &entry.container, entry.use_helper);
    let read = files
        .open_download(&entry.path, entry.is_dir)
        .await
        .map_err(|e| {
            tracing::warn!(error = %e, "file browser: download could not start");
            ApiError::Internal
        })?;
    // The read stays alive in the stream's state: dropping it ends the ssh process.
    let body = Body::from_stream(futures_util::stream::unfold(read, |mut read| async move {
        use tokio::io::AsyncReadExt as _;
        let mut buf = vec![0u8; 64 * 1024];
        match read.stdout.read(&mut buf).await {
            Ok(0) | Err(_) => None,
            Ok(n) => {
                buf.truncate(n);
                Some((Ok::<_, std::io::Error>(axum::body::Bytes::from(buf)), read))
            }
        }
    }));
    Response::builder()
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{}\"", entry.name.replace('"', "")),
        )
        .header("X-Accel-Buffering", "no")
        .body(body)
        .map_err(|_| ApiError::Internal)
}
