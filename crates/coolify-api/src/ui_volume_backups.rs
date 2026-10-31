//! Session endpoints behind the storage backup pages: `Project/Application/Backup/{Index,Create,Show}`,
//! `Project/Service/VolumeBackup/{Index,Create,Show}`, `Project/Shared/Storages/VolumeBackups` and
//! `Project/Service/BackupExecutions`. `{kind}` is `application` or `service`.

use axum::Extension;
use axum::Router;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::middleware::from_fn_with_state;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use coolify_db::database_parts as parts;
use coolify_db::databases as dbs;
use coolify_db::services::{self as svc, SubKind};
use coolify_db::volume_backups::{self as vb, BackupTarget, FILE_MORPH, ScheduleValues};
use serde_json::{Map, Value, json};
use std::collections::HashMap;

use crate::audit::Meta;
use crate::auth::AuthCtx;
use crate::response::{ApiError, ApiResult, ok};
use crate::state::AppState;
use crate::ui_shared::{self, Res};

const SCHEDULE_NOT_FOUND: ApiError =
    ApiError::Message(StatusCode::NOT_FOUND, "Backup schedule not found.");
const INCORRECT_PASSWORD: ApiError = ApiError::Message(
    StatusCode::UNPROCESSABLE_ENTITY,
    "The provided password is incorrect.",
);

pub fn routes(state: AppState) -> Router<AppState> {
    let base = "/ui/volume-backups/{kind}/{uuid}";
    Router::new()
        .route(base, get(index).post(create))
        .route(&format!("{base}/{{backup}}"), get(show))
        .route(&format!("{base}/{{backup}}/action"), post(action))
        .route(&format!("{base}/{{backup}}/delete"), post(destroy))
        .route(&format!("{base}/{{backup}}/executions"), get(executions))
        .route(
            &format!("{base}/{{backup}}/executions/cleanup-failed"),
            post(cleanup_failed),
        )
        .route(
            &format!("{base}/{{backup}}/executions/cleanup-deleted"),
            post(cleanup_deleted),
        )
        .route(
            &format!("{base}/{{backup}}/executions/{{execution}}/retry-recovery"),
            post(retry_recovery),
        )
        .route(
            &format!("{base}/{{backup}}/executions/{{execution}}/delete"),
            post(delete_execution),
        )
        .route("/ui/services/{uuid}/backup-now", post(service_backup_now))
        .route(
            "/ui/services/{uuid}/backup-executions",
            get(service_executions),
        )
        .route(
            "/ui/services/{uuid}/import-databases",
            get(service_import_databases),
        )
        .route_layer(from_fn_with_state(state, crate::session::ui_guard))
}

fn toasts(list: &[(&str, &str)]) -> Value {
    json!({"toasts": list.iter().map(|(k, m)| json!({"type": k, "message": m})).collect::<Vec<_>>()})
}

/// Only `application` and `service` pages have storage backups.
async fn resource(
    state: &AppState,
    ctx: &AuthCtx,
    kind: &str,
    uuid: &str,
) -> Result<Res, ApiError> {
    if kind != "application" && kind != "service" {
        return Err(ApiError::RouteNotFound);
    }
    ui_shared::find(state, ctx, kind, uuid).await
}

fn str_of<'a>(r: &'a coolify_db::records::Record, k: &str) -> &'a str {
    r.get(k).and_then(Value::as_str).unwrap_or_default()
}

/// `$resource->persistentStorages` and `fileStorages` of the application, or of every part of the service.
async fn resource_parts(
    state: &AppState,
    res: &Res,
) -> Result<Vec<(String, i64, String)>, ApiError> {
    if res.is_service() {
        let mut out = Vec::new();
        for s in svc::subs(&state.db, SubKind::Application, res.id).await? {
            out.push((
                SubKind::Application.morph().to_owned(),
                s.id,
                s.name.clone(),
            ));
        }
        for s in svc::subs(&state.db, SubKind::Database, res.id).await? {
            out.push((SubKind::Database.morph().to_owned(), s.id, s.name.clone()));
        }
        Ok(out)
    } else {
        Ok(vec![(res.morph.clone(), res.id, res.name.clone())])
    }
}

/// `str($name)->headline()`.
fn headline(name: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut word = String::new();
    let mut previous_lower = false;
    for c in name.chars() {
        if matches!(c, '-' | '_' | ' ') {
            if !word.is_empty() {
                out.push(std::mem::take(&mut word));
            }
            previous_lower = false;
            continue;
        }
        if c.is_uppercase() && previous_lower {
            out.push(std::mem::take(&mut word));
        }
        previous_lower = c.is_lowercase();
        word.push(c);
    }
    if !word.is_empty() {
        out.push(word);
    }
    out.iter()
        .map(|w| {
            let mut chars = w.chars();
            match chars.next() {
                Some(f) => f.to_uppercase().chain(chars).collect::<String>(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// `availableTargets()`: `(key, label, schedule frequency)`.
async fn available_targets(state: &AppState, res: &Res) -> Result<Vec<Value>, ApiError> {
    let db = &state.db;
    let mut out = Vec::new();
    let service_uuid = res.is_service().then(|| format!("{}_", res.uuid));
    for (morph, id, name) in resource_parts(state, res).await? {
        let label = if res.is_service() {
            headline(&name)
        } else {
            String::new()
        };
        let mut volumes = parts::persistent_storages(db, &morph, id).await?;
        volumes.sort_by(|a, b| str_of(a, "name").cmp(str_of(b, "name")));
        for v in volumes {
            let id = parts::id_of(&v);
            let full = str_of(&v, "name");
            // `str($volume->name)->after($service->uuid.'_')`
            let shown = match &service_uuid {
                Some(prefix) => full.split_once(prefix.as_str()).map_or(full, |(_, b)| b),
                None => full,
            };
            let schedule = vb::schedule_of(db, vb::PERSISTENT_MORPH, id).await?;
            out.push(json!({
                "key": format!("volume:{id}"),
                "type": if res.is_service() { label.clone() } else { "Volume".to_owned() },
                "name": shown,
                "frequency": schedule.as_ref().map(|s| str_of(s, "frequency").to_owned()),
            }));
        }
        let mut files = parts::file_storages(db, &morph, id).await?;
        files.retain(|f| {
            f.get("is_directory").and_then(Value::as_bool) == Some(true)
                && f.get("is_host_file").and_then(Value::as_bool) != Some(true)
        });
        files.sort_by(|a, b| str_of(a, "fs_path").cmp(str_of(b, "fs_path")));
        for f in files {
            let id = parts::id_of(&f);
            let schedule = vb::schedule_of(db, FILE_MORPH, id).await?;
            out.push(json!({
                "key": format!("directory:{id}"),
                "type": if res.is_service() { label.clone() } else { "Directory".to_owned() },
                "name": if res.is_service() { format!("{} (directory)", str_of(&f, "fs_path")) } else { str_of(&f, "fs_path").to_owned() },
                "frequency": schedule.as_ref().map(|s| str_of(s, "frequency").to_owned()),
            }));
        }
    }
    Ok(out)
}

/// `targetType()` and `targetName()`.
fn target_labels(target: Option<&BackupTarget>) -> (&'static str, String) {
    match target {
        Some(t) if t.morph == FILE_MORPH => ("Directory", str_of(&t.row, "fs_path").to_owned()),
        Some(t) => ("Volume", str_of(&t.row, "name").to_owned()),
        None => ("Volume", "Unknown storage".to_owned()),
    }
}

/// `forApplication` and `forService`: the schedules of the resource's volumes and directories.
async fn schedules_of(
    state: &AppState,
    res: &Res,
) -> Result<Vec<coolify_db::records::Record>, ApiError> {
    let db = &state.db;
    let mut out = Vec::new();
    for (morph, id, _) in resource_parts(state, res).await? {
        for v in parts::persistent_storages(db, &morph, id).await? {
            if let Some(s) = vb::schedule_of(db, vb::PERSISTENT_MORPH, parts::id_of(&v)).await? {
                out.push(s);
            }
        }
        for f in parts::file_storages(db, &morph, id).await? {
            if f.get("is_directory").and_then(Value::as_bool) == Some(true)
                && f.get("is_host_file").and_then(Value::as_bool) != Some(true)
                && let Some(s) = vb::schedule_of(db, FILE_MORPH, parts::id_of(&f)).await?
            {
                out.push(s);
            }
        }
    }
    // `->latest()`
    out.sort_by(|a, b| {
        str_of(b, "created_at")
            .cmp(str_of(a, "created_at"))
            .then_with(|| parts::id_of(b).cmp(&parts::id_of(a)))
    });
    Ok(out)
}

/// `$backup->s3Storages`, else `$backup->s3`, with the team check of the service page.
async fn s3_rows(
    state: &AppState,
    backup: &coolify_db::records::Record,
    pivot: &str,
    pivot_key: &str,
) -> Result<Vec<Value>, ApiError> {
    let rows: Vec<(String, String, i64)> = sqlx::query_as(&format!(
        "SELECT s.name::text, s.bucket::text, s.team_id FROM {pivot} p JOIN s3_storages s ON s.id = p.s3_storage_id WHERE p.{pivot_key} = $1 ORDER BY s.name"
    ))
    .bind(parts::id_of(backup))
    .fetch_all(state.db.pool())
    .await
    .map_err(coolify_db::DbError::from)?;
    let mut rows = rows;
    if rows.is_empty()
        && let Some(id) = backup.get("s3_storage_id").and_then(Value::as_i64)
        && let Some(r) = parts::s3_by_id(&state.db, id).await?
    {
        rows.push((
            str_of(&r, "name").to_owned(),
            str_of(&r, "bucket").to_owned(),
            r.get("team_id").and_then(Value::as_i64).unwrap_or(-1),
        ));
    }
    Ok(rows
        .into_iter()
        .map(|(name, bucket, team_id)| json!({"name": name, "bucket": bucket, "team_id": team_id}))
        .collect())
}

/// The `render()` of `Application/Backup/Index` and `Service/VolumeBackup/Index`.
async fn index(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid)): Path<(String, String)>,
) -> ApiResult {
    let res = resource(&state, &ctx, &kind, &uuid).await?;
    let db = &state.db;
    let mut backups = Vec::new();
    for b in schedules_of(&state, &res).await? {
        let target = vb::target_of(db, &b).await?;
        let (target_type, target_name) = target_labels(target.as_ref());
        let latest: Option<(Option<String>, Option<String>, i64)> = sqlx::query_as(
            "SELECT status::text, to_char(finished_at, 'YYYY-MM-DD\"T\"HH24:MI:SS'), (SELECT count(*) FROM scheduled_volume_backup_executions c WHERE c.scheduled_volume_backup_id = e.scheduled_volume_backup_id)
             FROM scheduled_volume_backup_executions e WHERE e.scheduled_volume_backup_id = $1 ORDER BY e.created_at DESC, e.id DESC LIMIT 1",
        )
        .bind(parts::id_of(&b))
        .fetch_optional(db.pool())
        .await
        .map_err(coolify_db::DbError::from)?;
        let (status, finished, count) = latest.unwrap_or((None, None, 0));
        backups.push(json!({
            "id": parts::id_of(&b),
            "uuid": b.get("uuid"),
            "target_type": target_type,
            "target_name": target_name,
            "frequency": b.get("frequency"),
            "enabled": b.get("enabled"),
            "save_s3": b.get("save_s3"),
            "s3": s3_rows(&state, &b, "scheduled_volume_backup_s3_storage", "scheduled_volume_backup_id").await?,
            "latest_status": status,
            "latest_finished_at": finished,
            "executions_count": count,
            "created_at": b.get("created_at"),
        }));
    }
    let mut database_backups = Vec::new();
    let mut database_targets = Vec::new();
    let mut databases = Vec::new();
    if res.is_service() {
        for s in svc::subs(db, SubKind::Database, res.id).await? {
            let label = s
                .str("human_name")
                .filter(|h| !h.is_empty())
                .unwrap_or(s.name.as_str())
                .to_owned();
            databases.push(json!({
                "uuid": s.uuid,
                "backup_available": dbs::service_backup_available(s.str("image").unwrap_or_default(), s.str("custom_type")),
                "is_migrated": s.bool("is_migrated"),
            }));
            if dbs::service_backup_available(
                s.str("image").unwrap_or_default(),
                s.str("custom_type"),
            ) {
                database_targets.push(json!({"uuid": s.uuid, "name": label}));
            }
            for b in
                coolify_db::database_pages::scheduled_backups(db, dbs::SERVICE_DATABASE_MORPH, s.id)
                    .await?
            {
                let latest: Option<(Option<String>, Option<String>)> = sqlx::query_as(
                    "SELECT status::text, to_char(finished_at, 'YYYY-MM-DD\"T\"HH24:MI:SS') FROM scheduled_database_backup_executions e WHERE e.scheduled_database_backup_id = $1 ORDER BY e.created_at DESC, e.id DESC LIMIT 1",
                )
                .bind(parts::id_of(&b))
                .fetch_optional(db.pool())
                .await
                .map_err(coolify_db::DbError::from)?;
                let (status, finished) = latest.unwrap_or((None, None));
                database_backups.push(json!({
                    "id": parts::id_of(&b),
                    "uuid": b.get("uuid"),
                    "database_uuid": s.uuid,
                    "target_name": label,
                    "database_status": s.str("status"),
                    "frequency": b.get("frequency"),
                    "enabled": b.get("enabled"),
                    "save_s3": b.get("save_s3"),
                    "s3": s3_rows(&state, &b, "scheduled_database_backup_s3_storage", "scheduled_database_backup_id").await?,
                    "latest_status": status,
                    "latest_finished_at": finished,
                    "executions_count": b.get("executions_count"),
                    "created_at": b.get("created_at"),
                }));
            }
        }
        // `->latest()`
        database_backups.sort_by(|a, b| {
            let key = |v: &Value| {
                v.get("created_at")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned()
            };
            key(b).cmp(&key(a))
        });
    }
    Ok(ok(json!({
        "can_update": ctx.is_team_admin(),
        "team_id": res.team_id,
        "name": res.name,
        "backups": backups,
        "database_backups": database_backups,
        "database_targets": database_targets,
        "databases": databases,
        "targets": available_targets(&state, &res).await?,
    }))
    .into_response())
}

/// `selectedTarget()`: the target key must point at a volume or directory of this resource.
async fn selected_target(
    state: &AppState,
    res: &Res,
    key: &str,
) -> Result<Option<BackupTarget>, ApiError> {
    let Some((kind, id)) = key.split_once(':') else {
        return Ok(None);
    };
    let Ok(id) = id.parse::<i64>() else {
        return Ok(None);
    };
    let parts_of = resource_parts(state, res).await?;
    let db = &state.db;
    for (morph, part, _) in parts_of {
        if kind == "volume" {
            let row = parts::persistent_storages(db, &morph, part)
                .await?
                .into_iter()
                .find(|v| parts::id_of(v) == id);
            if let Some(row) = row {
                return Ok(Some(BackupTarget {
                    morph: vb::PERSISTENT_MORPH,
                    row,
                }));
            }
        } else if kind == "directory" {
            let row = parts::file_storages(db, &morph, part)
                .await?
                .into_iter()
                .find(|f| {
                    parts::id_of(f) == id
                        && f.get("is_directory").and_then(Value::as_bool) == Some(true)
                        && f.get("is_host_file").and_then(Value::as_bool) != Some(true)
                });
            if let Some(row) = row {
                return Ok(Some(BackupTarget {
                    morph: FILE_MORPH,
                    row,
                }));
            }
        }
    }
    Ok(None)
}

fn field_error(field: &str, message: &str) -> ApiError {
    let mut errors = Map::new();
    errors.insert(field.to_owned(), json!([message]));
    ApiError::Validation(errors)
}

/// `Application/Backup/Create::submit` and `Service/VolumeBackup/Create::submit`.
async fn create(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path((kind, uuid)): Path<(String, String)>,
    body: Bytes,
) -> ApiResult {
    let res = resource(&state, &ctx, &kind, &uuid).await?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let input: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let key = input
        .get("target_key")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let frequency = input
        .get("frequency")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let mut errors = Map::new();
    if key.is_empty() {
        errors.insert(
            "targetKey".into(),
            json!(["The target key field is required."]),
        );
    } else if !valid_target_key(key) {
        errors.insert(
            "targetKey".into(),
            json!(["The target key field format is invalid."]),
        );
    }
    if frequency.trim().is_empty() {
        errors.insert(
            "frequency".into(),
            json!(["The frequency field is required."]),
        );
    }
    if !errors.is_empty() {
        return Err(ApiError::Validation(errors));
    }
    let owner = if res.is_service() {
        "service"
    } else {
        "application"
    };
    let Some(target) = selected_target(&state, &res, key).await? else {
        return Err(field_error(
            "targetKey",
            &format!("Select a volume or directory owned by this {owner}."),
        ));
    };
    if !coolify_compat::cron::validate_cron_expression(frequency) {
        return Err(field_error(
            "frequency",
            "The frequency must be a valid cron or human expression.",
        ));
    }
    let target_id = parts::id_of(&target.row);
    let now = state.now();
    let (backup_uuid, created, changed) = vb::set_frequency(
        &state.db,
        target.morph,
        target_id,
        res.team_id,
        frequency,
        now,
    )
    .await?;
    if created || !changed.is_empty() {
        let mut details = Map::new();
        details.insert("team_id".into(), json!(res.team_id));
        details.insert("resource_type".into(), json!(owner));
        details.insert("resource_uuid".into(), json!(res.uuid));
        details.insert("resource_name".into(), json!(res.name));
        details.insert("storage_uuid".into(), json!(target.row.get("uuid")));
        details.insert("backup_uuid".into(), json!(backup_uuid));
        details.insert("created".into(), json!(created));
        if !created {
            details.insert("changed_fields".into(), json!(changed));
        }
        crate::audit::log(
            &state,
            &ctx,
            &meta,
            "ui.volume_backup.schedule_set",
            Value::Object(details),
            "info",
        )
        .await;
    }
    let mut out = toasts(&[(
        "success",
        if created {
            "Scheduled storage backup created."
        } else {
            "Scheduled storage backup updated."
        },
    )]);
    out["backup_uuid"] = json!(backup_uuid);
    Ok(ok(out).into_response())
}

/// `regex:/^(volume|directory):[1-9][0-9]*$/`.
fn valid_target_key(key: &str) -> bool {
    let Some((kind, id)) = key.split_once(':') else {
        return false;
    };
    matches!(kind, "volume" | "directory")
        && id.starts_with(|c: char| ('1'..='9').contains(&c))
        && id.chars().all(|c| c.is_ascii_digit())
}

/// A schedule of the resource by uuid.
async fn find_schedule(
    state: &AppState,
    res: &Res,
    backup: &str,
) -> Result<coolify_db::records::Record, ApiError> {
    schedules_of(state, res)
        .await?
        .into_iter()
        .find(|b| str_of(b, "uuid") == backup)
        .ok_or(SCHEDULE_NOT_FOUND)
}

/// Usable storages of the team plus the current destinations: `selectableS3Storages()`.
async fn selectable_storages(
    state: &AppState,
    team_id: i64,
    selected: &[i64],
) -> Result<Vec<(i64, String, bool)>, ApiError> {
    let rows: Vec<(i64, String, bool)> = sqlx::query_as(
        "SELECT id, name::text, is_usable FROM s3_storages WHERE team_id = $1 AND (is_usable = true OR id = ANY($2)) ORDER BY name",
    )
    .bind(team_id)
    .bind(selected)
    .fetch_all(state.db.pool())
    .await
    .map_err(coolify_db::DbError::from)?;
    Ok(rows)
}

async fn selected_ids(
    state: &AppState,
    backup: &coolify_db::records::Record,
) -> Result<Vec<i64>, ApiError> {
    Ok(vb::selected_storages(&state.db, backup)
        .await?
        .iter()
        .map(parts::id_of)
        .collect())
}

/// `VolumeBackups::mount()`.
async fn payload(
    state: &AppState,
    res: &Res,
    b: &coolify_db::records::Record,
) -> Result<Value, ApiError> {
    let target = vb::target_of(&state.db, b).await?;
    let (target_type, target_name) = target_labels(target.as_ref());
    let selected = selected_ids(state, b).await?;
    let available = selectable_storages(state, res.team_id, &selected).await?;
    let ids: Vec<i64> = if selected.is_empty() {
        available.iter().take(1).map(|s| s.0).collect()
    } else {
        selected
    };
    let timezone = match res.server_id {
        Some(id) => coolify_db::servers::find_by_id(&state.db, id)
            .await?
            .map(|r| coolify_jobs::server::ServerInfo::from_row(&r))
            .and_then(|s| {
                s.settings
                    .get("server_timezone")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            }),
        None => None,
    };
    Ok(json!({
        "uuid": b.get("uuid"),
        "target_type": target_type,
        "target_name": target_name,
        "timezone": timezone.unwrap_or_else(|| "Instance timezone".to_owned()),
        "form": {
            "frequency": b.get("frequency"),
            "enabled": b.get("enabled"),
            "save_s3": b.get("save_s3"),
            "disable_local_backup": b.get("disable_local_backup"),
            "stop_during_backup": b.get("stop_during_backup"),
            "s3_storage_ids": ids,
            "retention_amount_locally": b.get("retention_amount_locally"),
            "retention_days_locally": b.get("retention_days_locally"),
            "retention_max_storage_locally": b.get("retention_max_storage_locally"),
            "retention_amount_s3": b.get("retention_amount_s3"),
            "retention_days_s3": b.get("retention_days_s3"),
            "retention_max_storage_s3": b.get("retention_max_storage_s3"),
            "timeout": b.get("timeout"),
            "missing_backup_notification_days": b.get("missing_backup_notification_days"),
        },
        "storages": available.iter().map(|(id, name, usable)| json!({"id": id, "name": name, "is_usable": usable})).collect::<Vec<_>>(),
    }))
}

async fn show(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid, backup)): Path<(String, String, String)>,
) -> ApiResult {
    let res = resource(&state, &ctx, &kind, &uuid).await?;
    let b = find_schedule(&state, &res, &backup).await?;
    Ok(ok(payload(&state, &res, &b).await?).into_response())
}

/// The form's values, read as `validateSettings()` checks them.
struct Settings {
    values: ScheduleValues,
    storage_ids: Vec<i64>,
}

fn number_field(form: &Map<String, Value>, key: &str) -> Option<f64> {
    match form.get(key)? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// `$this->validate()` with `rules()`, `validate_cron_expression` and the S3 checks.
async fn validate_settings(
    state: &AppState,
    res: &Res,
    current: &[i64],
    form: &Map<String, Value>,
) -> Result<Settings, ApiError> {
    let mut errors = Map::new();
    let mut add = |k: &str, m: String| {
        errors.insert(k.to_owned(), json!([m]));
    };
    let frequency = form
        .get("frequency")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if frequency.trim().is_empty() {
        add("frequency", "The frequency field is required.".to_owned());
    }
    let integer = |key: &str,
                   label: &str,
                   min: i64,
                   max: Option<i64>,
                   add: &mut dyn FnMut(&str, String)|
     -> i64 {
        match number_field(form, key) {
            None if matches!(form.get(key), None | Some(Value::Null))
                || form.get(key).and_then(Value::as_str) == Some("") =>
            {
                add(key, format!("The {label} field is required."));
                0
            }
            Some(v) if v.fract() == 0.0 => {
                let v = v as i64;
                if v < min {
                    add(key, format!("The {label} field must be at least {min}."));
                } else if let Some(max) = max.filter(|m| v > *m) {
                    add(
                        key,
                        format!("The {label} field must not be greater than {max}."),
                    );
                }
                v
            }
            _ => {
                add(key, format!("The {label} field must be an integer."));
                0
            }
        }
    };
    let numeric = |key: &str, label: &str, add: &mut dyn FnMut(&str, String)| -> f64 {
        match number_field(form, key) {
            None if matches!(form.get(key), None | Some(Value::Null))
                || form.get(key).and_then(Value::as_str) == Some("") =>
            {
                add(key, format!("The {label} field is required."));
                0.0
            }
            Some(v) if v >= 0.0 => v,
            Some(_) => {
                add(key, format!("The {label} field must be at least 0."));
                0.0
            }
            None => {
                add(key, format!("The {label} field must be a number."));
                0.0
            }
        }
    };
    let retention_amount_locally = integer(
        "retention_amount_locally",
        "retention amount locally",
        0,
        Some(10000),
        &mut add,
    );
    let retention_days_locally = integer(
        "retention_days_locally",
        "retention days locally",
        0,
        None,
        &mut add,
    );
    let retention_max_storage_locally = numeric(
        "retention_max_storage_locally",
        "retention max storage locally",
        &mut add,
    );
    let retention_amount_s3 = integer(
        "retention_amount_s3",
        "retention amount s3",
        0,
        Some(10000),
        &mut add,
    );
    let retention_days_s3 = integer("retention_days_s3", "retention days s3", 0, None, &mut add);
    let retention_max_storage_s3 = numeric(
        "retention_max_storage_s3",
        "retention max storage s3",
        &mut add,
    );
    let timeout = integer("timeout", "timeout", 60, Some(36000), &mut add);
    let missing = integer(
        "missing_backup_notification_days",
        "missing backup notification days",
        0,
        Some(365),
        &mut add,
    );
    if !errors.is_empty() {
        return Err(ApiError::Validation(errors));
    }
    if !coolify_compat::cron::validate_cron_expression(frequency) {
        return Err(field_error(
            "frequency",
            "The frequency must be a valid cron or human expression.",
        ));
    }
    let boolean = |key: &str| form.get(key).and_then(Value::as_bool).unwrap_or(false);
    let storage_ids = submitted_storages(state, res, current, form).await?;
    let save_s3 = boolean("save_s3");
    if save_s3 && storage_ids.is_empty() {
        return Err(field_error(
            "s3StorageIds",
            "Select at least one usable S3 storage owned by your team.",
        ));
    }
    Ok(Settings {
        values: ScheduleValues {
            team_id: res.team_id,
            frequency: frequency.to_owned(),
            enabled: boolean("enabled"),
            save_s3,
            disable_local_backup: save_s3 && boolean("disable_local_backup"),
            stop_during_backup: boolean("stop_during_backup"),
            s3_storage_id: storage_ids.first().copied(),
            retention_amount_locally,
            retention_days_locally,
            retention_max_storage_locally,
            retention_amount_s3,
            retention_days_s3,
            retention_max_storage_s3,
            timeout: Some(timeout),
            missing_backup_notification_days: Some(missing),
        },
        storage_ids,
    })
}

/// `selectedS3StorageIds()`: the submitted destinations that are selectable, in the order submitted.
async fn submitted_storages(
    state: &AppState,
    res: &Res,
    current: &[i64],
    form: &Map<String, Value>,
) -> Result<Vec<i64>, ApiError> {
    let selectable = selectable_storages(state, res.team_id, current).await?;
    let mut out: Vec<i64> = Vec::new();
    for v in form
        .get("s3_storage_ids")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let id = v
            .as_i64()
            .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
            .unwrap_or(0);
        if id != 0 && !out.contains(&id) && selectable.iter().any(|s| s.0 == id) {
            out.push(id);
        }
    }
    Ok(out)
}

/// `persistBackup()` for an existing schedule, with the audit of changed fields.
async fn persist(
    state: &AppState,
    ctx: &AuthCtx,
    meta: &Meta,
    res: &Res,
    b: &coolify_db::records::Record,
    target: &BackupTarget,
    settings: &Settings,
) -> Result<coolify_db::records::Record, ApiError> {
    let now = state.now();
    let v = &settings.values;
    let target_id = parts::id_of(&target.row);
    let id = parts::id_of(b);
    let previous = selected_ids(state, b).await?;
    let mut changed: Vec<&str> = Vec::new();
    let differs = |k: &str, value: Value| -> bool {
        match b.get(k) {
            Some(Value::Number(n)) => n.as_f64() != value.as_f64(),
            Some(Value::String(s)) if value.is_number() => s.parse::<f64>().ok() != value.as_f64(),
            Some(o) => *o != value,
            None => !value.is_null(),
        }
    };
    for (k, value) in [
        ("frequency", json!(v.frequency)),
        ("enabled", json!(v.enabled)),
        ("save_s3", json!(v.save_s3)),
        ("disable_local_backup", json!(v.disable_local_backup)),
        ("stop_during_backup", json!(v.stop_during_backup)),
        (
            "retention_amount_locally",
            json!(v.retention_amount_locally),
        ),
        ("retention_days_locally", json!(v.retention_days_locally)),
        (
            "retention_max_storage_locally",
            json!(v.retention_max_storage_locally),
        ),
        ("retention_amount_s3", json!(v.retention_amount_s3)),
        ("retention_days_s3", json!(v.retention_days_s3)),
        (
            "retention_max_storage_s3",
            json!(v.retention_max_storage_s3),
        ),
        ("timeout", json!(v.timeout)),
        (
            "missing_backup_notification_days",
            json!(v.missing_backup_notification_days),
        ),
    ] {
        if differs(k, value) {
            changed.push(k);
        }
    }
    vb::upsert_schedule(&state.db, target.morph, target_id, v, now).await?;
    vb::sync_storages(&state.db, id, &settings.storage_ids, now).await?;
    let mut before = previous;
    before.sort_unstable();
    let mut after = settings.storage_ids.clone();
    after.sort_unstable();
    if before != after {
        changed.push("s3_storages");
    }
    if !changed.is_empty() {
        audit_schedule_set(state, ctx, meta, res, target, str_of(b, "uuid"), &changed).await;
    }
    Ok(vb::schedule_by_id(&state.db, id).await?.unwrap_or_default())
}

async fn audit_schedule_set(
    state: &AppState,
    ctx: &AuthCtx,
    meta: &Meta,
    res: &Res,
    target: &BackupTarget,
    backup: &str,
    changed: &[&str],
) {
    crate::audit::log(
        state,
        ctx,
        meta,
        "ui.volume_backup.schedule_set",
        json!({
            "team_id": res.team_id,
            "resource_type": if res.is_service() { "service" } else { "application" },
            "resource_uuid": res.uuid,
            "resource_name": res.name,
            "storage_uuid": target.row.get("uuid"),
            "backup_uuid": backup,
            "created": false,
            "changed_fields": changed,
        }),
        "info",
    )
    .await;
}

/// `save`, `updatedS3StorageIds`, `toggleS3`, `toggleEnabled` and `backupNow`.
async fn action(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path((kind, uuid, backup)): Path<(String, String, String)>,
    body: Bytes,
) -> ApiResult {
    let res = resource(&state, &ctx, &kind, &uuid).await?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let b = find_schedule(&state, &res, &backup).await?;
    let target = vb::target_of(&state.db, &b)
        .await?
        .ok_or(SCHEDULE_NOT_FOUND)?;
    let input: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let mut form = input
        .get("form")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let current = selected_ids(&state, &b).await?;
    let verb = input
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let toast: (&str, &str) = match verb {
        "save" => {
            let settings = validate_settings(&state, &res, &current, &form).await?;
            persist(&state, &ctx, &meta, &res, &b, &target, &settings).await?;
            ("success", "Storage backup schedule saved.")
        }
        "s3Storages" => {
            let settings = validate_settings(&state, &res, &current, &form).await?;
            persist(&state, &ctx, &meta, &res, &b, &target, &settings).await?;
            ("success", "S3 storages updated.")
        }
        "toggleS3" => {
            let save = form
                .get("save_s3")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if !save
                && submitted_storages(&state, &res, &current, &form)
                    .await?
                    .is_empty()
            {
                (
                    "error",
                    "Select a usable S3 storage before enabling S3 backups.",
                )
            } else {
                form.insert("save_s3".into(), json!(!save));
                let settings = validate_settings(&state, &res, &current, &form).await?;
                persist(&state, &ctx, &meta, &res, &b, &target, &settings).await?;
                (
                    "success",
                    if save {
                        "S3 backups disabled."
                    } else {
                        "S3 backups enabled."
                    },
                )
            }
        }
        "toggleEnabled" => {
            let enabled = b.get("enabled").and_then(Value::as_bool).unwrap_or(false);
            vb::set_enabled(&state.db, parts::id_of(&b), !enabled, state.now()).await?;
            audit_schedule_set(&state, &ctx, &meta, &res, &target, &backup, &["enabled"]).await;
            (
                "success",
                if enabled {
                    "Storage backups disabled."
                } else {
                    "Storage backups enabled."
                },
            )
        }
        "backupNow" => {
            let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
            jobs.spawn_volume_backup(parts::id_of(&b));
            crate::audit::log(
                &state,
                &ctx,
                &meta,
                "ui.volume_backup.started",
                json!({
                    "team_id": res.team_id,
                    "resource_uuid": res.uuid,
                    "resource_name": res.name,
                    "backup_uuid": backup,
                }),
                "info",
            )
            .await;
            let mut out = toasts(&[("success", "Storage backup queued.")]);
            out["redirect_executions"] = json!(true);
            return Ok(ok(out).into_response());
        }
        _ => return Err(ApiError::RouteNotFound),
    };
    let fresh = vb::schedule_by_id(&state.db, parts::id_of(&b))
        .await?
        .unwrap_or_default();
    let mut out = toasts(&[toast]);
    out["backup"] = payload(&state, &res, &fresh).await?;
    out["ok"] = json!(toast.0 == "success");
    Ok(ok(out).into_response())
}

async fn confirmed(state: &AppState, ctx: &AuthCtx, input: &Value) -> Result<bool, ApiError> {
    crate::ui::password_confirmed(
        state,
        ctx.user_id,
        input.get("password").and_then(Value::as_str),
    )
    .await
}

fn has_selected(input: &Value, key: &str) -> bool {
    input
        .get("selected")
        .and_then(Value::as_array)
        .is_some_and(|l| l.iter().any(|v| v.as_str() == Some(key)))
}

async fn server_of(
    state: &AppState,
    res: &Res,
) -> Result<Option<coolify_jobs::server::ServerInfo>, ApiError> {
    Ok(match res.server_id {
        Some(id) => coolify_db::servers::find_by_id(&state.db, id)
            .await?
            .map(|r| coolify_jobs::server::ServerInfo::from_row(&r)),
        None => None,
    })
}

/// `VolumeBackups::delete` with `DeleteScheduledVolumeBackup`.
async fn destroy(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path((kind, uuid, backup)): Path<(String, String, String)>,
    body: Bytes,
) -> ApiResult {
    let res = resource(&state, &ctx, &kind, &uuid).await?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let input: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    if !confirmed(&state, &ctx, &input).await? {
        return Err(INCORRECT_PASSWORD);
    }
    let b = find_schedule(&state, &res, &backup).await?;
    let target = vb::target_of(&state.db, &b).await?;
    let delete_local = has_selected(&input, "delete_associated_backups_locally");
    let delete_s3 = has_selected(&input, "delete_associated_backups_s3");
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    let server = server_of(&state, &res).await?;
    let result = jobs
        .delete_volume_schedule(&b, server.as_ref(), delete_local, delete_s3)
        .await;
    if let Err(e) = result {
        return Ok(ok(toasts(&[(
            "error",
            &format!("Could not delete the backup schedule: {e}"),
        )]))
        .into_response());
    }
    crate::audit::log(
        &state,
        &ctx,
        &meta,
        "ui.volume_backup.schedule_deleted",
        json!({
            "team_id": res.team_id,
            "resource_type": if res.is_service() { "service" } else { "application" },
            "resource_uuid": res.uuid,
            "resource_name": res.name,
            "storage_uuid": target.as_ref().and_then(|t| t.row.get("uuid")),
            "backup_uuid": backup,
            "delete_local_archives": delete_local,
            "delete_s3_archives": delete_s3,
        }),
        "info",
    )
    .await;
    let mut out = toasts(&[("success", "Storage backup schedule deleted.")]);
    out["deleted"] = json!(true);
    Ok(ok(out).into_response())
}

async fn executions(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid, backup)): Path<(String, String, String)>,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult {
    let res = resource(&state, &ctx, &kind, &uuid).await?;
    let b = find_schedule(&state, &res, &backup).await?;
    let per_page = q
        .get("per_page")
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(10)
        .clamp(1, 100);
    let page = q
        .get("page")
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(1)
        .max(1);
    let (rows, total) =
        vb::executions_page(&state.db, parts::id_of(&b), (page - 1) * per_page, per_page).await?;
    let mut list = Vec::with_capacity(rows.len());
    for mut row in rows {
        let replicas = vb::replica_rows(&state.db, parts::id_of(&row)).await?;
        row.insert("replicas".into(), json!(replicas));
        list.push(Value::Object(row));
    }
    let timezone = server_of(&state, &res).await?.and_then(|s| {
        s.settings
            .get("server_timezone")
            .and_then(Value::as_str)
            .map(str::to_owned)
    });
    Ok(ok(json!({
        "executions": list,
        "total": total,
        "page": page,
        "per_page": per_page,
        "server_timezone": timezone,
        "s3_downloads": crate::ui_database_backups::s3_downloads_enabled(&state).await,
    }))
    .into_response())
}

/// `cleanupFailed`.
async fn cleanup_failed(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid, backup)): Path<(String, String, String)>,
) -> ApiResult {
    let res = resource(&state, &ctx, &kind, &uuid).await?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let b = find_schedule(&state, &res, &backup).await?;
    let deleted = vb::delete_failed_executions(&state.db, parts::id_of(&b)).await?;
    Ok(ok(toasts(&[if deleted > 0 {
        ("success", "Failed backup entries cleaned up.")
    } else {
        ("info", "No safely removable failed backup entries found.")
    }]))
    .into_response())
}

/// `cleanupDeleted`.
async fn cleanup_deleted(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid, backup)): Path<(String, String, String)>,
    body: Bytes,
) -> ApiResult {
    let res = resource(&state, &ctx, &kind, &uuid).await?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let input: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    if !confirmed(&state, &ctx, &input).await? {
        return Err(INCORRECT_PASSWORD);
    }
    let b = find_schedule(&state, &res, &backup).await?;
    let deleted = vb::delete_local_deleted_executions(&state.db, parts::id_of(&b)).await?;
    let cleaned = format!("Cleaned up {deleted} deleted backup entries.");
    Ok(ok(toasts(&[if deleted > 0 {
        ("success", cleaned.as_str())
    } else {
        ("info", "No deleted backup entries found.")
    }]))
    .into_response())
}

/// An execution of the schedule by id.
async fn find_execution(
    state: &AppState,
    b: &coolify_db::records::Record,
    id: &str,
) -> Result<Option<coolify_db::records::Record>, ApiError> {
    let Ok(id) = id.parse::<i64>() else {
        return Ok(None);
    };
    Ok(vb::execution_by_id(&state.db, id)
        .await?
        .filter(|(_, schedule)| parts::id_of(schedule) == parts::id_of(b))
        .map(|(e, _)| e))
}

/// `retryRecovery`.
async fn retry_recovery(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path((kind, uuid, backup, execution)): Path<(String, String, String, String)>,
) -> ApiResult {
    let res = resource(&state, &ctx, &kind, &uuid).await?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let b = find_schedule(&state, &res, &backup).await?;
    let pending = find_execution(&state, &b, &execution).await?.filter(|e| {
        let flag = |k: &str| e.get(k).and_then(Value::as_bool) == Some(true);
        flag("stop_recovery_pending") || flag("s3_cleanup_pending")
    });
    let Some(row) = pending else {
        return Ok(ok(toasts(&[(
            "error",
            "No pending recovery found for this backup execution.",
        )]))
        .into_response());
    };
    let id = parts::id_of(&row);
    vb::set_recovery_state(&state.db, id, Some((None, false)), state.now()).await?;
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?.clone();
    tokio::spawn(async move {
        if let Err(e) = jobs.retry_volume_recovery(id).await {
            tracing::warn!(execution = id, error = %e, "volume backup recovery failed");
        }
    });
    crate::audit::log(
        &state,
        &ctx,
        &meta,
        "ui.volume_backup.recovery_retried",
        json!({
            "team_id": res.team_id,
            "resource_uuid": res.uuid,
            "backup_uuid": backup,
            "execution_uuid": row.get("uuid"),
        }),
        "info",
    )
    .await;
    Ok(ok(toasts(&[("success", "Backup recovery queued.")])).into_response())
}

/// `deleteBackup`.
async fn delete_execution(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid, backup, execution)): Path<(String, String, String, String)>,
    body: Bytes,
) -> ApiResult {
    let res = resource(&state, &ctx, &kind, &uuid).await?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let input: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    if !confirmed(&state, &ctx, &input).await? {
        return Err(INCORRECT_PASSWORD);
    }
    let b = find_schedule(&state, &res, &backup).await?;
    let Some(row) = find_execution(&state, &b, &execution).await? else {
        return Ok(ok(toasts(&[("error", "Backup execution not found.")])).into_response());
    };
    let flag = |k: &str| row.get(k).and_then(Value::as_bool) == Some(true);
    if str_of(&row, "status") == "running"
        || flag("stop_recovery_pending")
        || flag("s3_cleanup_pending")
    {
        return Ok(ok(toasts(&[(
            "error",
            "Wait for the backup and recovery operations to finish before deleting it.",
        )]))
        .into_response());
    }
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    let filename = str_of(&row, "filename");
    if !flag("local_storage_deleted") && !filename.is_empty() {
        let Some(server) = server_of(&state, &res).await? else {
            return Ok(ok(toasts(&[(
                "error",
                "Failed to delete backup: The server is unavailable.",
            )]))
            .into_response());
        };
        jobs.delete_local_backups(&[filename.to_owned()], &server)
            .await;
    }
    let id = parts::id_of(&row);
    if has_selected(&input, "delete_backup_s3") {
        jobs.delete_volume_s3_copies(id, filename).await.ok();
    }
    vb::delete_execution(&state.db, id).await?;
    let mut out = toasts(&[("success", "Backup deleted.")]);
    out["deleted"] = json!(true);
    Ok(ok(out).into_response())
}

/// `Service/VolumeBackup/Index::backupNow`.
async fn service_backup_now(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    body: Bytes,
) -> ApiResult {
    let res = resource(&state, &ctx, "service", &uuid).await?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let input: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let backup = input
        .get("backup")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    match input
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default()
    {
        "database" => {
            for s in svc::subs(&state.db, SubKind::Database, res.id).await? {
                let rows = coolify_db::database_pages::scheduled_backups(
                    &state.db,
                    dbs::SERVICE_DATABASE_MORPH,
                    s.id,
                )
                .await?;
                let Some(b) = rows.iter().find(|b| str_of(b, "uuid") == backup) else {
                    continue;
                };
                if !s.str("status").unwrap_or_default().starts_with("running") {
                    return Ok(ok(toasts(&[(
                        "error",
                        "The database must be running to start a backup.",
                    )]))
                    .into_response());
                }
                jobs.spawn_database_backup(parts::id_of(b));
                return Ok(ok(toasts(&[("success", "Backup queued.")])).into_response());
            }
            Err(SCHEDULE_NOT_FOUND)
        }
        "storage" => {
            let b = find_schedule(&state, &res, backup).await?;
            jobs.spawn_volume_backup(parts::id_of(&b));
            Ok(ok(toasts(&[("success", "Backup queued.")])).into_response())
        }
        _ => Err(ApiError::RouteNotFound),
    }
}

/// `Service/BackupExecutions::render` and `formatExecutions`.
async fn service_executions(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult {
    let res = resource(&state, &ctx, "service", &uuid).await?;
    let db = &state.db;
    let per_page = q
        .get("per_page")
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(10)
        .clamp(1, 100);
    let mut page = q
        .get("page")
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(1)
        .max(1);
    let schedules = schedules_of(&state, &res).await?;
    let volume_ids: Vec<i64> = schedules.iter().map(parts::id_of).collect();
    let mut database_ids: Vec<i64> = Vec::new();
    let mut database_owner: HashMap<i64, (String, coolify_db::records::Record)> = HashMap::new();
    for s in svc::subs(db, SubKind::Database, res.id).await? {
        let label = s
            .str("human_name")
            .filter(|h| !h.is_empty())
            .unwrap_or(s.name.as_str())
            .to_owned();
        for b in
            coolify_db::database_pages::scheduled_backups(db, dbs::SERVICE_DATABASE_MORPH, s.id)
                .await?
        {
            database_ids.push(parts::id_of(&b));
            database_owner.insert(parts::id_of(&b), (label.clone(), b));
        }
    }
    let total: i64 = sqlx::query_scalar(
        "SELECT (SELECT count(*) FROM scheduled_database_backup_executions WHERE scheduled_database_backup_id = ANY($1)) + (SELECT count(*) FROM scheduled_volume_backup_executions WHERE scheduled_volume_backup_id = ANY($2))",
    )
    .bind(&database_ids)
    .bind(&volume_ids)
    .fetch_one(db.pool())
    .await
    .map_err(coolify_db::DbError::from)?;
    let last_page = ((total + per_page - 1) / per_page).max(1);
    page = page.min(last_page);
    let rows: Vec<(i64, String)> = sqlx::query_as(
        "SELECT id, type FROM (
           SELECT id, created_at, 'database' AS type FROM scheduled_database_backup_executions WHERE scheduled_database_backup_id = ANY($1)
           UNION ALL
           SELECT id, created_at, 'storage' AS type FROM scheduled_volume_backup_executions WHERE scheduled_volume_backup_id = ANY($2)
         ) x ORDER BY created_at DESC, id DESC, type ASC LIMIT $3 OFFSET $4",
    )
    .bind(&database_ids)
    .bind(&volume_ids)
    .bind(per_page)
    .bind((page - 1) * per_page)
    .fetch_all(db.pool())
    .await
    .map_err(coolify_db::DbError::from)?;
    let mut list = Vec::with_capacity(rows.len());
    for (id, kind) in rows {
        let is_database = kind == "database";
        let table = if is_database {
            "scheduled_database_backup_executions"
        } else {
            "scheduled_volume_backup_executions"
        };
        let text: Option<String> = sqlx::query_scalar(&format!(
            "SELECT row_to_json(e)::text FROM {table} e WHERE e.id = $1"
        ))
        .bind(id)
        .fetch_optional(db.pool())
        .await
        .map_err(coolify_db::DbError::from)?;
        let Some(row) =
            text.and_then(|t| serde_json::from_str::<coolify_db::records::Record>(&t).ok())
        else {
            continue;
        };
        let status = str_of(&row, "status").to_owned();
        let (target, type_label, schedule, storage_label) = if is_database {
            let schedule_id = row
                .get("scheduled_database_backup_id")
                .and_then(Value::as_i64)
                .unwrap_or(-1);
            let (label, b) = database_owner
                .get(&schedule_id)
                .cloned()
                .unwrap_or_default();
            let save = b.get("save_s3").and_then(Value::as_bool) == Some(true);
            let names = s3_rows(
                &state,
                &b,
                "scheduled_database_backup_s3_storage",
                "scheduled_database_backup_id",
            )
            .await?;
            let text = storages_text(&names, res.team_id);
            (
                label,
                "Database".to_owned(),
                str_of(&b, "frequency").to_owned(),
                if save {
                    text
                } else {
                    "Not configured".to_owned()
                },
            )
        } else {
            let schedule_id = row
                .get("scheduled_volume_backup_id")
                .and_then(Value::as_i64)
                .unwrap_or(-1);
            let b = schedules
                .iter()
                .find(|s| parts::id_of(s) == schedule_id)
                .cloned()
                .unwrap_or_default();
            let target = vb::target_of(db, &b).await?;
            let (t, n) = target_labels(target.as_ref());
            let s3_id = row.get("s3_storage_id").and_then(Value::as_i64);
            let uploaded = row.get("s3_uploaded").and_then(Value::as_bool) == Some(true);
            let deleted = row.get("s3_storage_deleted").and_then(Value::as_bool) == Some(true);
            let label = if s3_id.is_none() && !uploaded && !deleted {
                "No destination recorded".to_owned()
            } else {
                let mut names = Vec::new();
                if let Some(id) = s3_id
                    && let Some(r) = parts::s3_by_id(db, id).await?
                {
                    names.push(json!({"name": r.get("name"), "bucket": r.get("bucket"), "team_id": r.get("team_id")}));
                }
                storages_text(&names, res.team_id)
            };
            (n, t.to_owned(), str_of(&b, "frequency").to_owned(), label)
        };
        // `isDownloadable()`
        let downloadable = status == "success"
            && (row.get("local_storage_deleted").and_then(Value::as_bool) != Some(true)
                || row.get("s3_uploaded").and_then(Value::as_bool) == Some(true)
                    && row.get("s3_storage_deleted").and_then(Value::as_bool) != Some(true));
        list.push(json!({
            "id": format!("{kind}:{id}"),
            "uuid": row.get("uuid"),
            "target": target,
            "type": type_label,
            "schedule": schedule,
            "s3_tooltip": format!("{}{storage_label}", if is_database { "Current schedule S3 storage: " } else { "S3 storage: " }),
            "status": status,
            "started_at": row.get("created_at"),
            "size": row.get("size"),
            "message": row.get("message"),
            "filename": row.get("filename"),
            "download_url": downloadable.then(|| format!("/download/{}/{id}", if is_database { "backup" } else { "volume-backup" })),
        }));
    }
    let timezone = server_of(&state, &res).await?.and_then(|s| {
        s.settings
            .get("server_timezone")
            .and_then(Value::as_str)
            .map(str::to_owned)
    });
    Ok(ok(json!({
        "executions": list,
        "total": total,
        "page": page,
        "per_page": per_page,
        "last_page": last_page,
        "server_timezone": timezone,
        "s3_downloads": crate::ui_database_backups::s3_downloads_enabled(&state).await,
    }))
    .into_response())
}

/// The storages of the service's team, as `name (bucket: x)`.
fn storages_text(rows: &[Value], team_id: i64) -> String {
    let names: Vec<String> = rows
        .iter()
        .filter(|r| r.get("team_id").and_then(Value::as_i64) == Some(team_id))
        .map(|r| {
            format!(
                "{} (bucket: {})",
                r.get("name").and_then(Value::as_str).unwrap_or_default(),
                r.get("bucket").and_then(Value::as_str).unwrap_or_default()
            )
        })
        .collect();
    if names.is_empty() {
        "Unavailable".to_owned()
    } else {
        names.join(", ")
    }
}

/// `Service/ImportBackup::mount`: the databases that take an import (`supportsImport`).
async fn service_import_databases(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let res = resource(&state, &ctx, "service", &uuid).await?;
    // `can.update.resource` and `$this->authorize('update', $this->service)`.
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let mut out = Vec::new();
    for s in svc::subs(&state.db, SubKind::Database, res.id).await? {
        let t =
            dbs::service_database_type(s.str("image").unwrap_or_default(), s.str("custom_type"));
        if ["mysql", "mariadb", "postgres", "mongo"]
            .iter()
            .any(|k| t.contains(k))
        {
            out.push(json!({
                "uuid": s.uuid,
                "name": s.str("human_name").filter(|h| !h.is_empty()).unwrap_or(s.name.as_str()),
            }));
        }
    }
    Ok(ok(json!({"databases": out})).into_response())
}
