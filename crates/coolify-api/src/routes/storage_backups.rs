//! `VolumeBackupsController`: `PUT`, `DELETE` and `POST .../run` on
//! `/{applications,databases,services}/{uuid}/storages/{storage_uuid}/backups`.

use axum::Extension;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::IntoResponse;
use coolify_db::database_parts as parts;
use coolify_db::databases as dbs;
use coolify_db::servers;
use coolify_db::volume_backups::{self as vb, BackupTarget, FILE_MORPH, ScheduleValues};
use coolify_domain::Ability;
use coolify_jobs::server::ServerInfo;
use serde_json::{Map, Value, json};

use crate::auth::AuthCtx;
use crate::input::{Input, validated_json};
use crate::response::{ApiError, ApiResult, PhpJson};
use crate::state::AppState;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Resource {
    Application,
    Database,
    Service,
}

impl Resource {
    fn name(self) -> &'static str {
        match self {
            Self::Application => "application",
            Self::Database => "database",
            Self::Service => "service",
        }
    }
    fn not_found(self) -> &'static str {
        match self {
            Self::Application => "Application not found.",
            Self::Database => "Database not found.",
            Self::Service => "Service not found.",
        }
    }
}

fn answer(code: StatusCode, value: Value) -> axum::response::Response {
    PhpJson(code, value).into_response()
}

/// `findResource`.
async fn find_resource(
    state: &AppState,
    ctx: &AuthCtx,
    kind: Resource,
    uuid: &str,
) -> Result<Option<Vec<(String, i64)>>, ApiError> {
    Ok(match kind {
        Resource::Application => vb::application_resource(&state.db, ctx.team_id, uuid).await?,
        Resource::Service => vb::service_resource(&state.db, ctx.team_id, uuid).await?,
        Resource::Database => dbs::find_in_team(&state.db, ctx.team_id, uuid)
            .await?
            .map(|d| vec![(d.kind.morph().to_owned(), d.id)]),
    })
}

/// `$request->boolean()`: `FILTER_VALIDATE_BOOLEAN`.
fn boolean(v: Option<&Value>) -> bool {
    match v {
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64() == Some(1.0),
        Some(Value::String(s)) => {
            ["1", "true", "on", "yes"].contains(&s.to_ascii_lowercase().as_str())
        }
        _ => false,
    }
}

fn add(errors: &mut Map<String, Value>, key: &str, message: String) {
    let list = errors
        .entry(key.to_owned())
        .or_insert_with(|| Value::Array(vec![]));
    if let Value::Array(a) = list {
        a.push(Value::String(message));
    }
}

const ALLOWED: &[&str] = &[
    "frequency",
    "enabled",
    "save_s3",
    "disable_local_backup",
    "stop_during_backup",
    "s3_storage_uuid",
    "s3_storage_uuids",
    "retention_amount_locally",
    "retention_days_locally",
    "retention_max_storage_locally",
    "retention_amount_s3",
    "retention_days_s3",
    "retention_max_storage_s3",
    "timeout",
    "missing_backup_notification_days",
];

fn is_boolean(v: &Value) -> bool {
    matches!(v, Value::Bool(_))
        || matches!(v.as_i64(), Some(0 | 1))
        || matches!(v.as_str(), Some("0" | "1"))
}

fn as_integer(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n
            .as_i64()
            .or_else(|| n.as_f64().filter(|f| f.fract() == 0.0).map(|f| f as i64)),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn as_numeric(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// `validateUpsertRequest`.
async fn validate_upsert(
    state: &AppState,
    ctx: &AuthCtx,
    input: &Input,
    target: &BackupTarget,
) -> Result<(Map<String, Value>, Vec<coolify_db::records::Record>, bool), ApiError> {
    let mut errors = Map::new();
    let present = |k: &str| input.get(k).filter(|v| !v.is_null());
    match input.get("frequency") {
        None | Some(Value::Null) => add(
            &mut errors,
            "frequency",
            "The frequency field is required.".into(),
        ),
        Some(Value::String(s)) if s.is_empty() => add(
            &mut errors,
            "frequency",
            "The frequency field is required.".into(),
        ),
        Some(Value::String(s)) if s.chars().count() > 255 => add(
            &mut errors,
            "frequency",
            "The frequency field must not be greater than 255 characters.".into(),
        ),
        Some(Value::String(_)) => {}
        Some(_) => add(
            &mut errors,
            "frequency",
            "The frequency field must be a string.".into(),
        ),
    }
    for k in [
        "enabled",
        "save_s3",
        "disable_local_backup",
        "stop_during_backup",
    ] {
        if present(k).is_some_and(|v| !is_boolean(v)) {
            add(
                &mut errors,
                k,
                format!("The {} field must be true or false.", k.replace('_', " ")),
            );
        }
    }
    if present("s3_storage_uuid").is_some_and(|v| !v.is_string()) {
        add(
            &mut errors,
            "s3_storage_uuid",
            "The s3 storage uuid field must be a string.".into(),
        );
    }
    if let Some(list) = present("s3_storage_uuids") {
        match list.as_array() {
            None => add(
                &mut errors,
                "s3_storage_uuids",
                "The s3 storage uuids field must be an array.".into(),
            ),
            Some(items) => {
                for (i, item) in items.iter().enumerate() {
                    let key = format!("s3_storage_uuids.{i}");
                    if !item.is_string() {
                        add(
                            &mut errors,
                            &key,
                            format!("The s3_storage_uuids.{i} field must be a string."),
                        );
                    }
                    if items.iter().filter(|x| *x == item).count() > 1 {
                        add(
                            &mut errors,
                            &key,
                            format!("The s3_storage_uuids.{i} field has a duplicate value."),
                        );
                    }
                }
            }
        }
    }
    for (k, min, max) in [
        ("retention_amount_locally", 0, 10_000),
        ("retention_days_locally", 0, 2_147_483_647),
        ("retention_amount_s3", 0, 10_000),
        ("retention_days_s3", 0, 2_147_483_647),
        ("timeout", 60, 36_000),
        ("missing_backup_notification_days", 0, 365),
    ] {
        let Some(v) = present(k) else { continue };
        let name = k.replace('_', " ");
        match as_integer(v) {
            None => add(
                &mut errors,
                k,
                format!("The {name} field must be an integer."),
            ),
            Some(n) if n < min => add(
                &mut errors,
                k,
                format!("The {name} field must be at least {min}."),
            ),
            Some(n) if n > max => add(
                &mut errors,
                k,
                format!("The {name} field must not be greater than {max}."),
            ),
            _ => {}
        }
    }
    for k in ["retention_max_storage_locally", "retention_max_storage_s3"] {
        let Some(v) = present(k) else { continue };
        let name = k.replace('_', " ");
        match as_numeric(v) {
            None => add(
                &mut errors,
                k,
                format!("The {name} field must be a number."),
            ),
            Some(n) if n < 0.0 => add(
                &mut errors,
                k,
                format!("The {name} field must be at least 0."),
            ),
            Some(n) if n > 9_999_999_999.0 => add(
                &mut errors,
                k,
                format!("The {name} field must not be greater than 9999999999."),
            ),
            _ => {}
        }
    }
    for key in input.keys() {
        if !ALLOWED.contains(&key.as_str()) {
            add(&mut errors, key, "This field is not allowed.".into());
        }
    }
    if !errors.contains_key("frequency")
        && !coolify_compat::cron::validate_cron_expression(
            input.str("frequency").unwrap_or_default(),
        )
    {
        add(
            &mut errors,
            "frequency",
            "The frequency must be a valid cron or human expression.".into(),
        );
    }
    let save_s3 = boolean(input.get("save_s3"));
    if boolean(input.get("disable_local_backup")) && !save_s3 {
        add(
            &mut errors,
            "disable_local_backup",
            "Local backups can only be disabled when S3 backups are enabled.".into(),
        );
    }
    let mut storages = Vec::new();
    let has_s3_error = errors
        .keys()
        .any(|k| k == "s3_storage_uuid" || k.starts_with("s3_storage_uuids"));
    if save_s3 && !has_s3_error {
        let has_list = input.has("s3_storage_uuids");
        let uuids: Vec<String> = if has_list {
            input
                .get("s3_storage_uuids")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default()
        } else {
            input
                .str("s3_storage_uuid")
                .filter(|s| !s.is_empty())
                .map(|s| vec![s.to_owned()])
                .unwrap_or_default()
        };
        let primary = input
            .str("s3_storage_uuid")
            .map(str::to_owned)
            .or_else(|| uuids.first().cloned());
        storages = vb::usable_storages(&state.db, ctx.team_id, &uuids).await?;
        storages.sort_by_key(|s| i32::from(parts::str_of(s, "uuid") != primary.as_deref()));
        if has_list
            && primary
                .as_deref()
                .is_some_and(|p| !p.is_empty() && !uuids.iter().any(|u| u == p))
        {
            add(
                &mut errors,
                "s3_storage_uuid",
                "The s3_storage_uuid must be one of the s3_storage_uuids.".into(),
            );
        } else if uuids.is_empty() || storages.len() != uuids.len() {
            add(
                &mut errors,
                if has_list {
                    "s3_storage_uuids"
                } else {
                    "s3_storage_uuid"
                },
                "Select a usable S3 storage owned by your team.".into(),
            );
        }
    }
    if target.morph == FILE_MORPH
        && (target.row.get("is_directory").and_then(Value::as_bool) != Some(true)
            || target.row.get("is_host_file").and_then(Value::as_bool) == Some(true))
    {
        add(
            &mut errors,
            "storage_uuid",
            "Only directory file storages can be backed up.".into(),
        );
    }
    Ok((errors, storages, save_s3))
}

fn float_json(v: f64) -> Value {
    if v.fract() == 0.0 {
        json!(v as i64)
    } else {
        json!(v)
    }
}

#[allow(clippy::too_many_arguments)]
async fn upsert(
    state: AppState,
    ctx: AuthCtx,
    kind: Resource,
    uuid: String,
    storage_uuid: String,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let input = validated_json(&headers, &uri, &body)?;
    let Some(resources) = find_resource(&state, &ctx, kind, &uuid).await? else {
        return Ok(answer(
            StatusCode::NOT_FOUND,
            json!({"message": kind.not_found()}),
        ));
    };
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    if boolean(input.get("stop_during_backup"))
        && !ctx.abilities.can(Ability::Deploy)
        && !ctx.abilities.can(Ability::Root)
    {
        return Ok(answer(
            StatusCode::FORBIDDEN,
            json!({"message": "Missing required permissions: deploy. A backup that stops the containers needs a token with the deploy permission."}),
        ));
    }
    let Some(target) = vb::find_storage(&state.db, &resources, &storage_uuid).await? else {
        return Ok(answer(
            StatusCode::NOT_FOUND,
            json!({"message": "Storage not found."}),
        ));
    };
    let (errors, storages, save_s3) = validate_upsert(&state, &ctx, &input, &target).await?;
    if !errors.is_empty() {
        return Ok(answer(
            StatusCode::UNPROCESSABLE_ENTITY,
            json!({"message": "Validation failed.", "errors": errors}),
        ));
    }
    let int = |k: &str, default: i64| input.get(k).and_then(as_integer).unwrap_or(default);
    let num = |k: &str| input.get(k).and_then(as_numeric).unwrap_or(0.0);
    let values = ScheduleValues {
        team_id: ctx.team_id,
        frequency: input.str("frequency").unwrap_or_default().to_owned(),
        enabled: if input.has("enabled") {
            boolean(input.get("enabled"))
        } else {
            true
        },
        save_s3,
        disable_local_backup: save_s3 && boolean(input.get("disable_local_backup")),
        stop_during_backup: boolean(input.get("stop_during_backup")),
        s3_storage_id: storages.first().map(parts::id_of),
        retention_amount_locally: int("retention_amount_locally", 7),
        retention_days_locally: int("retention_days_locally", 0),
        retention_max_storage_locally: num("retention_max_storage_locally"),
        retention_amount_s3: int("retention_amount_s3", 7),
        retention_days_s3: int("retention_days_s3", 0),
        retention_max_storage_s3: num("retention_max_storage_s3"),
        timeout: input.has("timeout").then(|| int("timeout", 36000)),
        missing_backup_notification_days: input
            .has("missing_backup_notification_days")
            .then(|| int("missing_backup_notification_days", 0)),
    };
    let target_id = parts::id_of(&target.row);
    let now = state.now();
    let (id, backup_uuid, created) =
        vb::upsert_schedule(&state.db, target.morph, target_id, &values, now).await?;
    let ids: Vec<i64> = storages.iter().map(parts::id_of).collect();
    vb::sync_storages(&state.db, id, &ids, now).await?;
    let storage_row_uuid = target.row.get("uuid").cloned().unwrap_or(Value::Null);
    tracing::info!(target: "audit", event = "api.volume_backup.schedule_set", team_id = ctx.team_id, resource_type = kind.name(), resource_uuid = %uuid, backup_uuid = %backup_uuid);
    let row = vb::schedule_by_id(&state.db, id).await?.unwrap_or_default();
    let code = if created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok(answer(
        code,
        json!({
            "uuid": backup_uuid,
            "message": if created { "Storage backup schedule created." } else { "Storage backup schedule updated." },
            "storage_uuid": storage_row_uuid,
            "storage_type": if target.morph == FILE_MORPH { "directory" } else { "persistent" },
            "frequency": values.frequency,
            "enabled": values.enabled,
            "save_s3": values.save_s3,
            "disable_local_backup": values.disable_local_backup,
            "stop_during_backup": values.stop_during_backup,
            "s3_storage_uuid": storages.first().and_then(|s| s.get("uuid")),
            "s3_storage_uuids": storages.iter().filter_map(|s| s.get("uuid")).collect::<Vec<_>>(),
            "retention_amount_locally": values.retention_amount_locally,
            "retention_days_locally": values.retention_days_locally,
            "retention_max_storage_locally": float_json(values.retention_max_storage_locally),
            "retention_amount_s3": values.retention_amount_s3,
            "retention_days_s3": values.retention_days_s3,
            "retention_max_storage_s3": float_json(values.retention_max_storage_s3),
            // The model is not refreshed: a new schedule has no `timeout` attribute until it is set.
            "timeout": if created && values.timeout.is_none() { Value::Null } else { row.get("timeout").cloned().unwrap_or(Value::Null) },
            "missing_backup_notification_days": row.get("missing_backup_notification_days").cloned().unwrap_or(json!(0)),
        }),
    ))
}

/// `destroy` and `run` share their lookups, which differ only in the first 404's text.
async fn schedule(
    state: &AppState,
    ctx: &AuthCtx,
    kind: Resource,
    uuid: &str,
    storage_uuid: &str,
    not_found: &'static str,
) -> Result<Result<(BackupTarget, coolify_db::records::Record), axum::response::Response>, ApiError>
{
    let Some(resources) = find_resource(state, ctx, kind, uuid).await? else {
        return Ok(Err(answer(
            StatusCode::NOT_FOUND,
            json!({"message": not_found}),
        )));
    };
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let Some(target) = vb::find_storage(&state.db, &resources, storage_uuid).await? else {
        return Ok(Err(answer(
            StatusCode::NOT_FOUND,
            json!({"message": "Storage not found."}),
        )));
    };
    let Some(row) = vb::schedule_of(&state.db, target.morph, parts::id_of(&target.row)).await?
    else {
        return Ok(Err(answer(
            StatusCode::NOT_FOUND,
            json!({"message": "Storage backup schedule not found."}),
        )));
    };
    Ok(Ok((target, row)))
}

async fn destroy(
    state: AppState,
    ctx: AuthCtx,
    kind: Resource,
    uuid: String,
    storage_uuid: String,
) -> ApiResult {
    let (target, row) = match schedule(
        &state,
        &ctx,
        kind,
        &uuid,
        &storage_uuid,
        "Resource not found.",
    )
    .await?
    {
        Ok(v) => v,
        Err(r) => return Ok(r),
    };
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    let server_id = vb::resource_server(
        &state.db,
        parts::str_of(&target.row, "resource_type").unwrap_or_default(),
        target
            .row
            .get("resource_id")
            .and_then(Value::as_i64)
            .unwrap_or(-1),
    )
    .await?;
    let server = match server_id {
        Some(id) => servers::find_by_id(&state.db, id)
            .await?
            .map(|r| ServerInfo::from_row(&r)),
        None => None,
    };
    if let Err(message) = jobs
        .delete_volume_schedule(&row, server.as_ref(), true, true)
        .await
    {
        return Ok(answer(StatusCode::CONFLICT, json!({"message": message})));
    }
    tracing::info!(target: "audit", event = "api.volume_backup.schedule_deleted", team_id = ctx.team_id, resource_type = kind.name(), resource_uuid = %uuid, backup_uuid = ?row.get("uuid"));
    Ok(answer(
        StatusCode::OK,
        json!({"message": "Storage backup schedule and archives deleted."}),
    ))
}

async fn run(
    state: AppState,
    ctx: AuthCtx,
    kind: Resource,
    uuid: String,
    storage_uuid: String,
) -> ApiResult {
    let (_, row) =
        match schedule(&state, &ctx, kind, &uuid, &storage_uuid, kind.not_found()).await? {
            Ok(v) => v,
            Err(r) => return Ok(r),
        };
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    jobs.spawn_volume_backup(parts::id_of(&row));
    tracing::info!(target: "audit", event = "api.volume_backup.run", team_id = ctx.team_id, resource_type = kind.name(), resource_uuid = %uuid, backup_uuid = ?row.get("uuid"));
    Ok(answer(
        StatusCode::OK,
        json!({"message": "Storage backup queued.", "uuid": row.get("uuid")}),
    ))
}

macro_rules! handlers {
    ($put:ident, $del:ident, $post:ident, $kind:expr) => {
        pub async fn $put(
            State(state): State<AppState>,
            Extension(ctx): Extension<AuthCtx>,
            Path((uuid, storage)): Path<(String, String)>,
            headers: HeaderMap,
            uri: Uri,
            body: Bytes,
        ) -> ApiResult {
            upsert(state, ctx, $kind, uuid, storage, headers, uri, body).await
        }
        pub async fn $del(
            State(state): State<AppState>,
            Extension(ctx): Extension<AuthCtx>,
            Path((uuid, storage)): Path<(String, String)>,
        ) -> ApiResult {
            destroy(state, ctx, $kind, uuid, storage).await
        }
        pub async fn $post(
            State(state): State<AppState>,
            Extension(ctx): Extension<AuthCtx>,
            Path((uuid, storage)): Path<(String, String)>,
        ) -> ApiResult {
            run(state, ctx, $kind, uuid, storage).await
        }
    };
}

handlers!(
    put_database,
    delete_database,
    run_database,
    Resource::Database
);
handlers!(put_service, delete_service, run_service, Resource::Service);
handlers!(
    put_application,
    delete_application,
    run_application,
    Resource::Application
);
