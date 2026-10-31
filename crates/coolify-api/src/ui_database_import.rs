//! Session endpoints behind the Import Backup page of a database: `Project/Database/Import` and
//! `Project/Database/ImportForm`. Uploads use the API route (`POST /databases/{uuid}/imports/uploads`
//! through the session bridge) with Dropzone's chunk fields; this module holds the checks and the
//! password-confirmed start.

use std::sync::LazyLock;

use axum::Extension;
use axum::Router;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::middleware::from_fn_with_state;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use coolify_db::database_parts as parts;
use coolify_db::databases::{self as dbs, DatabaseRow, Kind};
use coolify_jobs::databases::import::{self, ImportSource};
use coolify_jobs::databases::s3::{S3Target, valid_bucket_name};
use regex::Regex;
use serde_json::{Value, json};

use crate::audit::Meta;
use crate::auth::AuthCtx;
use crate::response::{ApiError, ApiResult, ok};
use crate::state::AppState;
use crate::ui_database_general::{server_of, toasts};

const NOT_FOUND: ApiError = ApiError::Message(StatusCode::NOT_FOUND, "Database not found.");

pub fn routes(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/ui/databases/{uuid}/import", get(form).post(run_import))
        .route("/ui/databases/{uuid}/import/command", post(command))
        .route("/ui/databases/{uuid}/import/check-file", post(check_file))
        .route("/ui/databases/{uuid}/import/check-s3", post(check_s3))
        .route_layer(from_fn_with_state(state, crate::session::ui_guard))
}

fn toast(kind: &str, message: &str) -> Response {
    ok(toasts(&[(kind, message)])).into_response()
}

async fn find(state: &AppState, ctx: &AuthCtx, uuid: &str) -> Result<DatabaseRow, ApiError> {
    dbs::find_in_team(&state.db, ctx.team_id, uuid)
        .await?
        .ok_or(NOT_FOUND)
}

/// `Import::isUnsupportedResource`.
fn unsupported(kind: Kind) -> bool {
    matches!(
        kind,
        Kind::Redis | Kind::Keydb | Kind::Dragonfly | Kind::Clickhouse
    )
}

fn source_from(input: &Value, kind: &str) -> ImportSource {
    let flag = |k: &str| input.get(k).and_then(Value::as_bool).unwrap_or(false);
    let text = |k: &str| input.get(k).and_then(Value::as_str).map(str::to_owned);
    ImportSource {
        kind: kind.to_owned(),
        upload_id: text("upload_id"),
        path: text("path"),
        s3_storage_uuid: text("s3_storage_uuid"),
        dump_all: flag("dump_all"),
        replace_existing: flag("replace_existing"),
        keep_owners: flag("keep_owners"),
        sqlite_database: text("sqlite_database"),
        restore_mysql_users: flag("restore_mysql_users"),
    }
}

/// `refreshRestoreCommandText`.
fn restore_command(d: &DatabaseRow, input: &Value) -> String {
    let source = source_from(input, "upload");
    let file = source
        .sqlite_database
        .clone()
        .or_else(|| sqlite_files(d).first().cloned());
    import::build_restore_command(d, "<temp_backup_file>", &source, file.as_deref())
}

/// `StandaloneSqlite::databaseFiles`.
fn sqlite_files(d: &DatabaseRow) -> Vec<String> {
    d.str("sqlite_databases")
        .unwrap_or_default()
        .split(',')
        .map(|f| f.trim().to_owned())
        .filter(|f| !f.is_empty())
        .collect()
}

/// `Import::mount` and `ImportForm::mount`.
async fn form(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let d = find(&state, &ctx, &uuid).await?;
    let storages = parts::s3_list(&state.db, d.team_id).await?;
    let storages: Vec<Value> = storages
        .iter()
        .filter(|s| parts::bool_of(s, "is_usable"))
        .map(|s| {
            json!({
                "id": s.get("id"),
                "uuid": s.get("uuid"),
                "name": s.get("name"),
                "description": s.get("description"),
            })
        })
        .collect();
    let command = if unsupported(d.kind) {
        String::new()
    } else {
        restore_command(&d, &json!({}))
    };
    Ok(ok(json!({
        "unsupported": unsupported(d.kind),
        "status": d.str("status").unwrap_or_default(),
        "db_type": d.kind.type_name(),
        "storages": storages,
        "sqlite_files": if d.kind == Kind::Sqlite { sqlite_files(&d) } else { vec![] },
        "restore_command": command,
    }))
    .into_response())
}

/// The `updatedDumpAll` family of `ImportForm`: shows the script the import runs.
async fn command(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    body: Bytes,
) -> ApiResult {
    let d = find(&state, &ctx, &uuid).await?;
    let input: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let text = if unsupported(d.kind) {
        String::new()
    } else {
        restore_command(&d, &input)
    };
    Ok(ok(json!({"restore_command": text})).into_response())
}

const DANGEROUS: &[&str] = &[
    "..", "$(", "`", "|", ";", "&", ">", "<", "\n", "\r", "\0", "'", "\"", "\\",
];
static SERVER_PATH: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-zA-Z0-9.\-_/\s]+\n?\z").unwrap_or_else(|_| unreachable!()));
static S3_PATH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[a-zA-Z0-9.\-_/\s+@=]+\n?\z").unwrap_or_else(|_| unreachable!())
});

/// `ImportForm::validateServerPath`.
fn valid_server_path(path: &str) -> bool {
    path.starts_with('/')
        && !DANGEROUS.iter().any(|p| path.contains(p))
        && SERVER_PATH.is_match(path)
}

/// `ImportForm::validateS3Path`.
fn valid_s3_path(path: &str) -> bool {
    !path.is_empty() && !DANGEROUS.iter().any(|p| path.contains(p)) && S3_PATH.is_match(path)
}

/// `ImportForm::checkFile`.
async fn check_file(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    body: Bytes,
) -> ApiResult {
    let d = find(&state, &ctx, &uuid).await?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let input: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let path = input
        .get("path")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if path.is_empty() {
        return Ok(ok(json!({"toasts": []})).into_response());
    }
    if !valid_server_path(path) {
        return Ok(toast(
            "error",
            "Invalid file path. Path must be absolute and contain only safe characters (alphanumerics, dots, dashes, underscores, slashes).",
        ));
    }
    let Some(server) = server_of(&state, &d).await? else {
        return Ok(toast("error", "Server not found. Please refresh the page."));
    };
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    match jobs.server_file_exists(&server, path).await {
        Ok(true) => {
            let mut res = toasts(&[("success", "The file exists.")]);
            res["exists"] = json!(true);
            Ok(ok(res).into_response())
        }
        Ok(false) => Ok(toast(
            "error",
            "The file does not exist or has been deleted.",
        )),
        Err(e) => Ok(toast("error", &e)),
    }
}

/// `formatBytes`.
pub(crate) fn format_bytes(size: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    if size == 0 {
        return "0 B".to_owned();
    }
    let i = (size as f64).log(1024.0).floor() as usize;
    let i = i.min(UNITS.len() - 1);
    let value = size as f64 / 1024f64.powi(i as i32);
    let rounded = (value * 100.0).round() / 100.0;
    let text = if rounded.fract() == 0.0 {
        format!("{}", rounded as i64)
    } else {
        format!("{rounded}")
    };
    format!("{text} {}", UNITS[i])
}

/// `ImportForm::checkS3File`.
async fn check_s3(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    body: Bytes,
) -> ApiResult {
    let d = find(&state, &ctx, &uuid).await?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let input: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let Some(storage_id) = input.get("s3_storage_id").and_then(Value::as_i64) else {
        return Ok(toast("error", "Please select an S3 storage."));
    };
    let path = input
        .get("path")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    if path.is_empty() {
        return Ok(toast("error", "Please provide an S3 path."));
    }
    let clean = path.trim_start_matches('/');
    if !valid_s3_path(clean) {
        return Ok(toast(
            "error",
            "Invalid S3 path. Path must contain only safe characters (alphanumerics, dots, dashes, underscores, slashes).",
        ));
    }
    let Some(row) = parts::s3_by_id(&state.db, storage_id)
        .await?
        .filter(|s| s.get("team_id").and_then(Value::as_i64) == Some(d.team_id))
    else {
        return Ok(toast("error", "S3 storage not found."));
    };
    if !valid_bucket_name(parts::str_of(&row, "bucket").unwrap_or_default()) {
        return Ok(toast(
            "error",
            "Invalid S3 bucket name. Bucket name must contain only letters, numbers, dots, and dashes, and must start and end with a letter or number.",
        ));
    }
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let target = match S3Target::from_row(enc, &row) {
        Ok(t) => t,
        Err(e) => return Ok(toast("error", &e)),
    };
    // `$s3Storage->testConnection()`.
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    if let Err(e) = jobs.check_s3_connection(&row).await {
        return Ok(toast("error", &e));
    }
    match target.object_size(clean).await {
        Ok(Some(size)) => {
            let message = format!("File found in S3. Size: {}", format_bytes(size));
            let mut res = toasts(&[("success", &message)]);
            res["size"] = json!(size);
            res["size_text"] = json!(format_bytes(size));
            res["s3_storage_uuid"] = row.get("uuid").cloned().unwrap_or(Value::Null);
            Ok(ok(res).into_response())
        }
        Ok(None) => Ok(toast(
            "error",
            "File not found in S3. Please check the path.",
        )),
        Err(e) => Ok(toast("error", &e)),
    }
}

/// `ImportForm::runImport` and `restoreFromS3`, with the password of the confirmation.
async fn run_import(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path(uuid): Path<String>,
    body: Bytes,
) -> ApiResult {
    let d = find(&state, &ctx, &uuid).await?;
    let input: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    if !crate::ui::password_confirmed(
        &state,
        ctx.user_id,
        input.get("password").and_then(Value::as_str),
    )
    .await?
    {
        return Err(ApiError::Message(
            StatusCode::UNPROCESSABLE_ENTITY,
            "The provided password is incorrect.",
        ));
    }
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let kind = input
        .get("source")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !matches!(kind, "upload" | "server" | "s3") {
        return Ok(toast("error", "Please select a file to import."));
    }
    if server_of(&state, &d).await?.is_none() {
        return Ok(toast("error", "Server not found. Please refresh the page."));
    }
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    let source = source_from(&input, kind);
    let replace = source.replace_existing;
    let data_dir = crate::routes::database_more::import_data_dir(&state);
    let id = match jobs
        .start_database_import(&d, source, d.team_id, &data_dir)
        .await
    {
        Ok((id, _)) => id,
        Err(e) => return Ok(toast("error", &e.message)),
    };
    let (event, context) = if kind == "s3" {
        (
            "ui.database.restore_started",
            json!({
                "team_id": d.team_id,
                "database_uuid": d.uuid,
                "database_name": d.str("name"),
                "source": "s3",
                "replace_existing": replace,
                "storage_id": input.get("s3_storage_id"),
            }),
        )
    } else {
        (
            "ui.database.import_started",
            json!({
                "team_id": d.team_id,
                "database_uuid": d.uuid,
                "database_name": d.str("name"),
                "source": "file",
                "replace_existing": replace,
            }),
        )
    };
    crate::audit::log(&state, &ctx, &meta, event, context, "info").await;
    let mut res = if kind == "s3" {
        toasts(&[(
            "info",
            "Restoring database from S3. Progress will be shown in the activity monitor...",
        )])
    } else {
        json!({"toasts": []})
    };
    res["activity_id"] = json!(id);
    Ok(ok(res).into_response())
}
