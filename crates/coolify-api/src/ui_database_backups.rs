//! Session endpoints behind the backup pages of a database: `Project/Database/Backup/Index`,
//! `Project/Database/ScheduledBackups`, `Project/Database/CreateScheduledBackup`,
//! `Project/Database/Backup/Execution`, `Project\Database\BackupEdit`,
//! `Project\Database\BackupExecutions` and `Concerns/ManagesDatabaseBackup`, with the
//! `/download/backup/{executionId}` route of routes/web.php.

use axum::Extension;
use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::middleware::from_fn_with_state;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use coolify_db::database_pages as pages;
use coolify_db::database_parts::{self as parts, NewBackup};
use coolify_db::databases::{self as dbs, DatabaseRow, Kind};
use coolify_db::records::Record;
use coolify_db::servers::ColumnValue;
use coolify_jobs::databases::s3::S3Target;
use futures_util::stream;
use serde_json::{Map, Value, json};
use tokio::io::AsyncReadExt as _;

use crate::audit::Meta;
use crate::auth::AuthCtx;
use crate::response::{ApiError, ApiResult, PhpJson, ok};
use crate::state::AppState;
use crate::ui_database_general::{server_of, toasts};

const NOT_FOUND: ApiError = ApiError::Message(StatusCode::NOT_FOUND, "Database not found.");

pub fn routes(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/ui/databases/{uuid}/backups", get(index).post(create))
        .route("/ui/databases/{uuid}/backups/{backup}", get(edit))
        .route("/ui/databases/{uuid}/backups/{backup}/action", post(action))
        .route(
            "/ui/databases/{uuid}/backups/{backup}/delete",
            post(destroy),
        )
        .route(
            "/ui/databases/{uuid}/backups/{backup}/executions",
            get(executions),
        )
        .route(
            "/ui/databases/{uuid}/backups/{backup}/executions/cleanup-failed",
            post(cleanup_failed),
        )
        .route(
            "/ui/databases/{uuid}/backups/{backup}/executions/cleanup-deleted",
            post(cleanup_deleted),
        )
        .route(
            "/ui/databases/{uuid}/backups/{backup}/executions/{execution}/delete",
            post(delete_execution),
        )
        .route("/download/backup/{execution}", get(download))
        .route_layer(from_fn_with_state(state, crate::session::ui_guard))
}

/// `isBackupSolutionAvailable()`.
fn backup_available(kind: Kind) -> bool {
    !matches!(kind, Kind::Redis | Kind::Keydb | Kind::Dragonfly)
}

async fn find(state: &AppState, ctx: &AuthCtx, uuid: &str) -> Result<DatabaseRow, ApiError> {
    dbs::find_in_team(&state.db, ctx.team_id, uuid)
        .await?
        .ok_or(NOT_FOUND)
}

async fn find_backup(
    state: &AppState,
    ctx: &AuthCtx,
    d: &DatabaseRow,
    backup: &str,
) -> Result<Record, ApiError> {
    parts::backup_of(&state.db, ctx.team_id, d.kind.morph(), d.id, backup)
        .await?
        .ok_or(ApiError::Message(StatusCode::NOT_FOUND, "Not found."))
}

/// `$backup->s3Storages` (or `s3`) names, joined.
async fn storage_names(state: &AppState, backup: &Record) -> Result<String, ApiError> {
    let id = parts::id_of(backup);
    let rows = parts::backup_destinations(&state.db, id).await?;
    let mut names: Vec<String> = rows
        .iter()
        .filter_map(|r| parts::str_of(r, "name").map(str::to_owned))
        .collect();
    if names.is_empty()
        && let Some(primary) = backup.get("s3_storage_id").and_then(Value::as_i64)
        && let Some(row) = parts::s3_by_id(&state.db, primary).await?
        && let Some(name) = parts::str_of(&row, "name")
    {
        names.push(name.to_owned());
    }
    Ok(names.join(", "))
}

/// `Backup\Index` and `ScheduledBackups`.
async fn index(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let d = find(&state, &ctx, &uuid).await?;
    if !backup_available(d.kind) {
        return Ok(ok(json!({"unavailable": true})).into_response());
    }
    let rows = pages::scheduled_backups(&state.db, d.kind.morph(), d.id).await?;
    let mut schedules = Vec::with_capacity(rows.len());
    for row in &rows {
        let latest = parts::latest_execution(&state.db, parts::id_of(row)).await?;
        schedules.push(json!({
            "id": row.get("id"),
            "uuid": row.get("uuid"),
            "frequency": row.get("frequency"),
            "enabled": row.get("enabled"),
            "save_s3": row.get("save_s3"),
            "s3_names": storage_names(&state, row).await?,
            "latest_status": latest.as_ref().and_then(|l| l.get("status")),
            "executions_count": row.get("executions_count"),
        }));
    }
    Ok(ok(json!({"schedules": schedules})).into_response())
}

/// `CreateScheduledBackup::submit`.
async fn create(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path(uuid): Path<String>,
    body: Bytes,
) -> ApiResult {
    let d = find(&state, &ctx, &uuid).await?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    if !backup_available(d.kind) {
        return Ok(ok(toasts(&[(
            "error",
            "Scheduled backups are not supported for this database type.",
        )]))
        .into_response());
    }
    let input: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let frequency = input
        .get("frequency")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if frequency.trim().is_empty() {
        let mut errors = Map::new();
        errors.insert(
            "frequency".into(),
            json!(["The frequency field is required."]),
        );
        return Err(ApiError::Validation(errors));
    }
    if !coolify_compat::cron::validate_cron_expression(frequency) {
        return Ok(ok(toasts(&[("error", "Invalid Cron / Human expression.")])).into_response());
    }
    let mut fields: Vec<(&'static str, ColumnValue)> = vec![
        ("enabled", ColumnValue::Bool(true)),
        ("frequency", ColumnValue::Text(Some(frequency.to_owned()))),
        ("save_s3", ColumnValue::Bool(false)),
    ];
    let default_db = match d.kind {
        Kind::Postgresql => Some("postgres_db"),
        Kind::Mysql => Some("mysql_database"),
        Kind::Mariadb => Some("mariadb_database"),
        Kind::Clickhouse => Some("clickhouse_db"),
        _ => None,
    };
    if let Some(column) = default_db {
        fields.push((
            "databases_to_backup",
            ColumnValue::Text(d.str(column).map(str::to_owned)),
        ));
    }
    let (_, backup_uuid) = parts::create_backup(
        &state.db,
        &NewBackup {
            team_id: d.team_id,
            morph: d.kind.morph(),
            database_id: d.id,
            fields,
            numeric_fields: vec![],
        },
        state.now(),
    )
    .await?;
    crate::audit::log(
        &state,
        &ctx,
        &meta,
        "ui.database.backup_schedule_created",
        json!({
            "team_id": d.team_id,
            "database_uuid": d.uuid,
            "database_name": d.str("name"),
            "backup_uuid": backup_uuid,
            "frequency": frequency,
            "save_s3": false,
        }),
        "info",
    )
    .await;
    Ok(ok(json!({"backup_uuid": backup_uuid})).into_response())
}

/// `BackupEdit::syncData()`: the form as the record holds it, with its S3 options.
async fn edit(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((uuid, backup)): Path<(String, String)>,
) -> ApiResult {
    let d = find(&state, &ctx, &uuid).await?;
    let b = find_backup(&state, &ctx, &d, &backup).await?;
    Ok(ok(edit_payload(&state, &d, &b).await?).into_response())
}

async fn edit_payload(state: &AppState, d: &DatabaseRow, b: &Record) -> Result<Value, ApiError> {
    const FIELDS: &[&str] = &[
        "enabled",
        "frequency",
        "database_backup_retention_amount_locally",
        "database_backup_retention_days_locally",
        "database_backup_retention_max_storage_locally",
        "database_backup_retention_amount_s3",
        "database_backup_retention_days_s3",
        "database_backup_retention_max_storage_s3",
        "save_s3",
        "disable_local_backup",
        "databases_to_backup",
        "dump_all",
        "timeout",
        "missing_backup_notification_days",
    ];
    let id = parts::id_of(b);
    let selected = crate::ui_settings::selected_storages(state, id).await?;
    let available = crate::ui_settings::available_storages(state, d.team_id, &selected).await?;
    let mut form = Map::new();
    for k in FIELDS {
        form.insert((*k).to_owned(), b.get(*k).cloned().unwrap_or(Value::Null));
    }
    let ids: Vec<i64> = if selected.is_empty() {
        available.iter().take(1).map(|s| s.0).collect()
    } else {
        selected
    };
    form.insert("s3_storage_ids".into(), json!(ids));
    let timezone = server_of(state, d)
        .await?
        .and_then(|s| {
            s.settings
                .get("server_timezone")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "Instance timezone".to_owned());
    Ok(json!({
        "uuid": b.get("uuid"),
        "database_type": b.get("database_type"),
        "form": form,
        "timezone": timezone,
        "storages": available.iter().map(|(id, name, usable)| json!({"id": id, "name": name, "is_usable": usable})).collect::<Vec<_>>(),
    }))
}

/// `BackupEdit::submit`, `toggleS3`, `toggleEnabled` and `backupNow`, with the trait
/// `Concerns/ManagesDatabaseBackup` (`toggleDatabaseBackup`, `queueDatabaseBackup`).
async fn action(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path((uuid, backup)): Path<(String, String)>,
    body: Bytes,
) -> ApiResult {
    let d = find(&state, &ctx, &uuid).await?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let b = find_backup(&state, &ctx, &d, &backup).await?;
    let id = parts::id_of(&b);
    let input: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let mut form = input
        .get("form")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let name = d.str("name").unwrap_or_default().to_owned();
    match input
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or_default()
    {
        "toggleEnabled" => {
            let on = b.get("enabled") != Some(&json!(true));
            parts::update_backup(
                &state.db,
                id,
                &[("enabled", ColumnValue::Bool(on))],
                &[],
                state.now(),
            )
            .await?;
            crate::audit::log(
                &state,
                &ctx,
                &meta,
                "ui.database.backup_schedule_updated",
                json!({
                    "team_id": d.team_id,
                    "database_uuid": d.uuid,
                    "database_name": name,
                    "backup_uuid": b.get("uuid"),
                    "changed_fields": ["enabled"],
                }),
                "info",
            )
            .await;
            let mut res = toasts(&[(
                "success",
                if on {
                    "Backup enabled."
                } else {
                    "Backup disabled."
                },
            )]);
            let fresh = find_backup(&state, &ctx, &d, &backup).await?;
            res["backup"] = edit_payload(&state, &d, &fresh).await?;
            Ok(ok(res).into_response())
        }
        kind @ ("submit" | "toggleS3") => {
            if kind == "toggleS3" {
                let flag = |k: &str| form.get(k).and_then(Value::as_bool).unwrap_or(false);
                let on = flag("save_s3");
                let selected = crate::ui_settings::selected_storages(&state, id).await?;
                let available =
                    crate::ui_settings::available_storages(&state, d.team_id, &selected).await?;
                let picked = form
                    .get("s3_storage_ids")
                    .and_then(Value::as_array)
                    .is_some_and(|a| {
                        a.iter().any(|x| {
                            x.as_i64()
                                .or_else(|| x.as_str().and_then(|s| s.parse().ok()))
                                .is_some_and(|n| available.iter().any(|s| s.0 == n))
                        })
                    });
                if !on && !picked {
                    return Ok(ok(toasts(&[(
                        "error",
                        "Select a usable S3 storage before enabling S3 backups.",
                    )]))
                    .into_response());
                }
                let keep = !on && flag("disable_local_backup");
                form.insert("save_s3".into(), json!(!on));
                form.insert("disable_local_backup".into(), json!(keep));
            }
            match crate::ui_settings::save_backup(
                &state,
                &ctx,
                &meta,
                &b,
                &form,
                d.team_id,
                Some((&d.uuid, &name)),
            )
            .await?
            {
                Ok(()) => {
                    let mut res = toasts(&[("success", "Backup updated successfully.")]);
                    let fresh = find_backup(&state, &ctx, &d, &backup).await?;
                    res["backup"] = edit_payload(&state, &d, &fresh).await?;
                    Ok(ok(res).into_response())
                }
                Err(message) => {
                    let mut res = toasts(&[("error", &message)]);
                    let fresh = find_backup(&state, &ctx, &d, &backup).await?;
                    res["backup"] = edit_payload(&state, &d, &fresh).await?;
                    Ok(ok(res).into_response())
                }
            }
        }
        "backupNow" => {
            if !d.str("status").unwrap_or_default().starts_with("running") {
                return Ok(ok(toasts(&[(
                    "error",
                    "The database must be running to start a backup.",
                )]))
                .into_response());
            }
            let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
            jobs.spawn_database_backup(id);
            crate::audit::log(
                &state,
                &ctx,
                &meta,
                "ui.database.backup_started",
                json!({
                    "team_id": d.team_id,
                    "database_uuid": d.uuid,
                    "database_name": name,
                    "backup_uuid": b.get("uuid"),
                }),
                "info",
            )
            .await;
            // Upstream's `success` event is lost in the full redirect to the executions page.
            Ok(ok(json!({"redirect_executions": true})).into_response())
        }
        _ => Err(ApiError::Message(StatusCode::NOT_FOUND, "Not found.")),
    }
}

async fn confirmed(state: &AppState, ctx: &AuthCtx, input: &Value) -> Result<bool, ApiError> {
    crate::ui::password_confirmed(
        state,
        ctx.user_id,
        input.get("password").and_then(Value::as_str),
    )
    .await
}

const BAD_PASSWORD: ApiError = ApiError::Message(
    StatusCode::UNPROCESSABLE_ENTITY,
    "The provided password is incorrect.",
);

fn selected(input: &Value, key: &str) -> bool {
    input
        .get("selected")
        .and_then(Value::as_array)
        .is_some_and(|a| a.iter().any(|v| v.as_str() == Some(key)))
}

/// `BackupEdit::delete`.
async fn destroy(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path((uuid, backup)): Path<(String, String)>,
    body: Bytes,
) -> ApiResult {
    let d = find(&state, &ctx, &uuid).await?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let input: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    if !confirmed(&state, &ctx, &input).await? {
        return Err(BAD_PASSWORD);
    }
    let b = find_backup(&state, &ctx, &d, &backup).await?;
    let id = parts::id_of(&b);
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    let executions = parts::executions_of(&state.db, id).await?;
    if selected(&input, "delete_associated_backups_locally")
        && let Some(server) = server_of(&state, &d).await?
    {
        let files: Vec<String> = executions
            .iter()
            .filter_map(|e| parts::str_of(e, "filename"))
            .filter(|f| !f.is_empty())
            .map(str::to_owned)
            .collect();
        jobs.delete_local_backups(&files, &server).await;
    }
    if selected(&input, "delete_associated_backups_s3") {
        for e in &executions {
            if let Err(message) = jobs.delete_s3_copies(e).await {
                return Ok(ok(toasts(&[(
                    "error",
                    &format!("Failed to delete backup: {message}"),
                )]))
                .into_response());
            }
        }
    }
    parts::delete_backup(&state.db, id).await?;
    crate::audit::log(
        &state,
        &ctx,
        &meta,
        "ui.database.backup_schedule_deleted",
        json!({
            "team_id": d.team_id,
            "database_uuid": d.uuid,
            "database_name": d.str("name"),
            "backup_uuid": b.get("uuid"),
        }),
        "info",
    )
    .await;
    Ok(ok(json!({"deleted": true})).into_response())
}

/// `redactBackupExecutionMessage`, for the admins that open this page.
async fn secrets(state: &AppState, d: &DatabaseRow) -> Vec<String> {
    let Ok(c) = crate::routes::databases::credentials(state, d).await else {
        return vec![];
    };
    let mut out: Vec<String> = d
        .kind
        .encrypted_columns()
        .iter()
        .map(|column| c.secret(column))
        .filter(|s| !s.is_empty())
        .collect();
    out.extend(
        c.variables
            .iter()
            .filter(|v| v.is_shown_once)
            .filter_map(|v| v.resolved.clone())
            .filter(|v| !v.is_empty()),
    );
    out
}

/// `BackupExecutions`: one page, each row with `s3Replicas`.
async fn executions(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((uuid, backup)): Path<(String, String)>,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> ApiResult {
    let d = find(&state, &ctx, &uuid).await?;
    let b = find_backup(&state, &ctx, &d, &backup).await?;
    let skip = q
        .get("skip")
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(0)
        .max(0);
    let (rows, total) = pages::executions_page(&state.db, parts::id_of(&b), skip, 10).await?;
    let secrets = secrets(&state, &d).await;
    let locked: Vec<&str> = secrets.iter().map(String::as_str).collect();
    let mut list = Vec::with_capacity(rows.len());
    for row in rows {
        let replicas = pages::execution_replicas(&state.db, parts::id_of(&row)).await?;
        let mut r = row;
        if let Some(m) = r.get("message").and_then(Value::as_str) {
            let safe = coolify_compat::redact::redact_text(m, &[], &locked);
            r.insert("message".into(), json!(safe));
        }
        r.insert("replicas".into(), json!(replicas));
        list.push(Value::Object(r));
    }
    Ok(ok(json!({
        "executions": list,
        "total": total,
        "server_timezone": server_of(&state, &d).await?.and_then(|s| s.settings.get("server_timezone").and_then(Value::as_str).map(str::to_owned)),
        "s3_downloads": false,
    }))
    .into_response())
}

/// `BackupExecutions::cleanupFailed`.
async fn cleanup_failed(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((uuid, backup)): Path<(String, String)>,
) -> ApiResult {
    let d = find(&state, &ctx, &uuid).await?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let b = find_backup(&state, &ctx, &d, &backup).await?;
    pages::delete_failed_executions(&state.db, parts::id_of(&b)).await?;
    Ok(ok(toasts(&[("success", "Failed backups cleaned up.")])).into_response())
}

/// `BackupExecutions::cleanupDeleted`.
async fn cleanup_deleted(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((uuid, backup)): Path<(String, String)>,
    body: Bytes,
) -> ApiResult {
    let d = find(&state, &ctx, &uuid).await?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let input: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    if !confirmed(&state, &ctx, &input).await? {
        return Err(BAD_PASSWORD);
    }
    let b = find_backup(&state, &ctx, &d, &backup).await?;
    let count = pages::delete_local_deleted_executions(&state.db, parts::id_of(&b)).await?;
    if count > 0 {
        let message = format!("Cleaned up {count} backup entries deleted from local storage.");
        Ok(ok(toasts(&[("success", &message)])).into_response())
    } else {
        Ok(ok(toasts(&[(
            "info",
            "No backup entries found that are deleted from local storage.",
        )]))
        .into_response())
    }
}

/// `BackupExecutions::deleteBackup`.
async fn delete_execution(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((uuid, backup, execution)): Path<(String, String, i64)>,
    body: Bytes,
) -> ApiResult {
    let d = find(&state, &ctx, &uuid).await?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let input: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    if !confirmed(&state, &ctx, &input).await? {
        return Err(BAD_PASSWORD);
    }
    let b = find_backup(&state, &ctx, &d, &backup).await?;
    let rows = parts::executions_of(&state.db, parts::id_of(&b)).await?;
    let Some(row) = rows.iter().find(|e| parts::id_of(e) == execution) else {
        return Ok(ok(toasts(&[("error", "Backup execution not found.")])).into_response());
    };
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    if let Some(file) = parts::str_of(row, "filename").filter(|f| !f.is_empty())
        && !parts::bool_of(row, "local_storage_deleted")
    {
        let Some(server) = server_of(&state, &d).await? else {
            return Ok(ok(toasts(&[(
                "error",
                "Failed to delete backup: The backup server is unavailable.",
            )]))
            .into_response());
        };
        jobs.delete_local_backups(&[file.to_owned()], &server).await;
    }
    if selected(&input, "delete_backup_s3")
        && let Err(message) = jobs.delete_s3_copies(row).await
    {
        return Ok(ok(toasts(&[(
            "error",
            &format!("Failed to delete backup: {message}"),
        )]))
        .into_response());
    }
    parts::delete_execution(&state.db, execution).await?;
    Ok(ok(toasts(&[("success", "Backup deleted.")])).into_response())
}

/// `Route::get('/download/backup/{executionId}')`.
async fn download(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(execution): Path<String>,
) -> Response {
    match download_inner(&state, &ctx, &execution).await {
        Ok(r) => r,
        Err(_) => message(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to download backup.",
        ),
    }
}

fn message(code: StatusCode, text: &str) -> Response {
    PhpJson(code, json!({"message": text})).into_response()
}

async fn download_inner(
    state: &AppState,
    ctx: &AuthCtx,
    execution: &str,
) -> Result<Response, ApiError> {
    if !ctx.is_team_admin() {
        return Ok(message(
            StatusCode::FORBIDDEN,
            "Only team admins/owners can download backups.",
        ));
    }
    let Some(id) = execution.parse::<i64>().ok() else {
        return Err(ApiError::Internal);
    };
    let Some((row, backup)) = pages::execution_by_id(&state.db, id).await? else {
        return Err(ApiError::Internal);
    };
    let morph = parts::str_of(&backup, "database_type").unwrap_or_default();
    let database_id = backup
        .get("database_id")
        .and_then(Value::as_i64)
        .unwrap_or(-1);
    let Some(kind) = Kind::ALL.into_iter().find(|k| k.morph() == morph) else {
        return Ok(message(StatusCode::NOT_FOUND, "Team not found."));
    };
    let database = dbs::find_of_kind(&state.db, kind, database_id).await?;
    let team_id = database.as_ref().map(|d| d.team_id);
    if ctx.team_id != 0 {
        let Some(team) = team_id else {
            return Ok(message(StatusCode::NOT_FOUND, "Team not found."));
        };
        if team != ctx.team_id {
            return Ok(message(StatusCode::FORBIDDEN, "Permission denied."));
        }
    }
    let filename = parts::str_of(&row, "filename")
        .unwrap_or_default()
        .to_owned();
    if !parts::bool_of(&row, "local_storage_deleted")
        && !filename.is_empty()
        && let Some(d) = &database
        && let Some(server) = server_of(state, d).await?
    {
        let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
        match jobs.read_server_file(&server, &filename).await {
            Ok(Some(read)) => {
                let base = filename.rsplit('/').next().unwrap_or(&filename).to_owned();
                let stream = stream::unfold(read, |mut read| async move {
                    let mut buf = vec![0u8; 64 * 1024];
                    match read.stdout.read(&mut buf).await {
                        Ok(0) | Err(_) => None,
                        Ok(n) => {
                            buf.truncate(n);
                            Some((Ok::<_, std::io::Error>(Bytes::from(buf)), read))
                        }
                    }
                });
                return Ok((
                    StatusCode::OK,
                    [
                        (header::CONTENT_TYPE, "application/octet-stream".to_owned()),
                        (
                            header::CONTENT_DISPOSITION,
                            format!("attachment; filename=\"{base}\""),
                        ),
                    ],
                    Body::from_stream(stream),
                )
                    .into_response());
            }
            Ok(None) => {}
            Err(_) => return Err(ApiError::Internal),
        }
    }
    if let Some(team) = team_id
        && !filename.is_empty()
    {
        let primary = backup.get("s3_storage_id").and_then(Value::as_i64);
        let mut storages =
            pages::live_replica_storages(&state.db, parts::id_of(&row), team).await?;
        storages.sort_by_key(|s| i32::from(Some(parts::id_of(s)) != primary));
        if let Some(storage) = storages.first()
            && let Some(enc) = state.encrypter.as_ref()
        {
            let target = S3Target::from_row(enc, storage).map_err(|_| ApiError::Internal)?;
            // `FileDownloadStorageService::LINK_TTL_MINUTES`.
            let url = target
                .presigned_get(filename.trim_start_matches('/'), 15 * 60)
                .map_err(|_| ApiError::Internal)?;
            return Ok(Redirect::to(&url).into_response());
        }
    }
    if parts::bool_of(&backup, "disable_local_backup") && parts::bool_of(&backup, "save_s3") {
        return Ok(message(
            StatusCode::NOT_FOUND,
            "Backup not available locally, but available on S3.",
        ));
    }
    Ok(message(
        StatusCode::NOT_FOUND,
        "Backup not found locally on the server.",
    ))
}
