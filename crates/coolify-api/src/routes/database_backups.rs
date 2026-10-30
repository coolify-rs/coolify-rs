//! `DatabasesController`: scheduled backups and their executions.

use std::sync::LazyLock;

use axum::Extension;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::IntoResponse;
use coolify_db::database_parts::{
    self as parts, BACKUP_EXECUTION, NewBackup, SCHEDULED_DATABASE_BACKUP,
};
use coolify_db::databases::{self as dbs, DatabaseRow, Kind};
use coolify_db::records::Record;
use coolify_db::servers::{self, ColumnValue};
use coolify_domain::Ability;
use regex::Regex;
use serde_json::{Map, Value, json};

use crate::auth::AuthCtx;
use crate::input::{Input, lenient_json, query_param, validated_json};
use crate::response::{ApiError, ApiResult, PhpJson, created, ok};
use crate::routes::databases::{NOT_FOUND, input_bool};
use crate::serialize::php_truthy;
use crate::state::AppState;
use crate::validation::{CUSTOM_API_MESSAGES, Field, Rule, validate};

const BACKUP_FIELDS: &[&str] = &[
    "save_s3",
    "enabled",
    "dump_all",
    "frequency",
    "databases_to_backup",
    "database_backup_retention_amount_locally",
    "database_backup_retention_days_locally",
    "database_backup_retention_max_storage_locally",
    "database_backup_retention_amount_s3",
    "database_backup_retention_days_s3",
    "database_backup_retention_max_storage_s3",
    "s3_storage_uuid",
    "s3_storage_uuids",
    "timeout",
    "missing_backup_notification_days",
    "disable_local_backup",
];
const FLOATS: &[&str] = &[
    "database_backup_retention_max_storage_locally",
    "database_backup_retention_max_storage_s3",
];
const INTS: &[&str] = &[
    "database_backup_retention_amount_locally",
    "database_backup_retention_days_locally",
    "database_backup_retention_amount_s3",
    "database_backup_retention_days_s3",
    "timeout",
    "missing_backup_notification_days",
];
const BOOLS: &[&str] = &["save_s3", "enabled", "dump_all", "disable_local_backup"];

const BACKUP_NOW_REASON: &str = "Missing required permissions: deploy. A backup that runs now copies the database on the server, so it needs a token with the deploy permission.";

fn message_403(ctx: &AuthCtx, message: &'static str) -> Result<(), ApiError> {
    if ctx.abilities.can(Ability::Deploy) || ctx.abilities.can(Ability::Root) {
        Ok(())
    } else {
        Err(ApiError::Message(StatusCode::FORBIDDEN, message))
    }
}

fn field(name: &'static str, rules: Vec<Rule>) -> Field {
    Field { name, rules }
}

/// The rules of `create_backup` and `update_backup`.
fn backup_rules(create: bool) -> Vec<Field> {
    let frequency = if create {
        vec![Rule::Required, Rule::String]
    } else {
        vec![Rule::String]
    };
    vec![
        field("frequency", frequency),
        field("enabled", vec![Rule::Boolean]),
        field("save_s3", vec![Rule::Boolean]),
        field("disable_local_backup", vec![Rule::Boolean]),
        field("dump_all", vec![Rule::Boolean]),
        field("backup_now", vec![Rule::Boolean, Rule::Nullable]),
        field("s3_storage_uuid", vec![Rule::String, Rule::Nullable]),
        field("s3_storage_uuids", vec![Rule::Array]),
        field("databases_to_backup", vec![Rule::String, Rule::Nullable]),
        field(
            "database_backup_retention_amount_locally",
            vec![Rule::Integer, Rule::MinNum(0)],
        ),
        field(
            "database_backup_retention_days_locally",
            vec![Rule::Integer, Rule::MinNum(0)],
        ),
        field(
            "database_backup_retention_max_storage_locally",
            vec![Rule::Numeric, Rule::MinNum(0)],
        ),
        field(
            "database_backup_retention_amount_s3",
            vec![Rule::Integer, Rule::MinNum(0)],
        ),
        field(
            "database_backup_retention_days_s3",
            vec![Rule::Integer, Rule::MinNum(0)],
        ),
        field(
            "database_backup_retention_max_storage_s3",
            vec![Rule::Numeric, Rule::MinNum(0)],
        ),
        field(
            "timeout",
            vec![Rule::Integer, Rule::MinNum(60), Rule::MaxNum(36000)],
        ),
        field(
            "missing_backup_notification_days",
            vec![Rule::Integer, Rule::MinNum(0), Rule::MaxNum(365)],
        ),
    ]
}

/// The rule pass of a backup request: the keys of the input are allowed here (extra fields are
/// reported later), plus `exists:s3_storages,uuid` and `s3_storage_uuids.*`.
async fn validate_backup_input(
    state: &AppState,
    input: &Input,
    create: bool,
) -> Result<(), ApiError> {
    let keys: Vec<&str> = input.keys().map(String::as_str).collect();
    let mut errors = match validate(input, &backup_rules(create), CUSTOM_API_MESSAGES, &keys) {
        Ok(()) => Map::new(),
        Err(ApiError::Validation(m)) => m,
        Err(other) => return Err(other),
    };
    if let Some(uuid) = input.str("s3_storage_uuid")
        && !errors.contains_key("s3_storage_uuid")
        && !parts::s3_uuid_exists(&state.db, uuid).await?
    {
        errors.insert(
            "s3_storage_uuid".into(),
            json!(["The selected s3 storage uuid is invalid."]),
        );
    }
    if let Some(Value::Array(list)) = input.get("s3_storage_uuids") {
        for (i, item) in list.iter().enumerate() {
            let key = format!("s3_storage_uuids.{i}");
            if !item.is_string() {
                errors.insert(
                    key,
                    json!([format!("The s3_storage_uuids.{i} field must be a string.")]),
                );
            } else if list.iter().filter(|o| *o == item).count() > 1 {
                errors.insert(
                    key,
                    json!([format!(
                        "The s3_storage_uuids.{i} field has a duplicate value."
                    )]),
                );
            }
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(ApiError::Validation(errors))
    }
}

/// `requestedBackupS3Storages`: the destinations, primary first; `None` when the request selects none.
async fn requested_storages(
    state: &AppState,
    ctx: &AuthCtx,
    input: &Input,
) -> Result<Option<Vec<Record>>, ApiError> {
    let has_list = input.has("s3_storage_uuids");
    if !has_list && !input.filled("s3_storage_uuid") {
        return Ok(None);
    }
    let raw: Vec<String> = if has_list {
        match input.get("s3_storage_uuids") {
            Some(Value::Array(a)) => a
                .iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect(),
            _ => Vec::new(),
        }
    } else {
        vec![input.str("s3_storage_uuid").unwrap_or_default().to_owned()]
    };
    let mut uuids: Vec<String> = Vec::new();
    for u in raw {
        if !uuids.contains(&u) {
            uuids.push(u);
        }
    }
    let primary = input
        .str("s3_storage_uuid")
        .map(str::to_owned)
        .or_else(|| uuids.first().cloned());
    let error_field = if has_list {
        "s3_storage_uuids"
    } else {
        "s3_storage_uuid"
    };
    if primary
        .as_deref()
        .is_some_and(|p| !p.is_empty() && !uuids.iter().any(|u| u == p))
    {
        let mut m = Map::new();
        m.insert(
            "s3_storage_uuid".into(),
            json!(["The s3_storage_uuid must be one of the s3_storage_uuids."]),
        );
        return Err(ApiError::Validation(m));
    }
    let mut rows = Vec::new();
    for u in &uuids {
        if let Some(r) = parts::s3_find(&state.db, ctx.team_id, u).await? {
            rows.push(r);
        }
    }
    if rows.len() != uuids.len() {
        let mut m = Map::new();
        m.insert(
            error_field.into(),
            json!(["The selected S3 storage is invalid for this team."]),
        );
        return Err(ApiError::Validation(m));
    }
    rows.sort_by_key(|r| i32::from(parts::str_of(r, "uuid") != primary.as_deref()));
    Ok(Some(rows))
}

fn extra_fields_error(input: &Input) -> Result<(), ApiError> {
    let mut errors = Map::new();
    for key in input.keys() {
        if !BACKUP_FIELDS.contains(&key.as_str()) && key != "backup_now" {
            errors.insert(key.clone(), json!(["This field is not allowed."]));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(ApiError::Validation(errors))
    }
}

/// `validate_cron_expression`: `dragonmantank/cron-expression` plus `VALID_CRON_STRINGS`.
pub fn valid_cron(expression: &str) -> bool {
    const NAMED: &[&str] = &[
        "every_minute",
        "hourly",
        "daily",
        "weekly",
        "monthly",
        "yearly",
        "@hourly",
        "@daily",
        "@weekly",
        "@monthly",
        "@yearly",
    ];
    const ALIASES: &[&str] = &[
        "@yearly",
        "@annually",
        "@monthly",
        "@weekly",
        "@daily",
        "@midnight",
        "@hourly",
    ];
    if expression.is_empty() || expression == "0" {
        return false;
    }
    if NAMED.contains(&expression) || ALIASES.contains(&expression) {
        return true;
    }
    let parts: Vec<&str> = expression.split_whitespace().collect();
    if parts.len() != 5 {
        return false;
    }
    static MONTHS: &[&str] = &[
        "JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC",
    ];
    static DAYS: &[&str] = &["SUN", "MON", "TUE", "WED", "THU", "FRI", "SAT"];
    cron_field(parts[0], 0, 59, &[])
        && cron_field(parts[1], 0, 23, &[])
        && cron_field_extra(parts[2], 1, 31, &[], "LW?")
        && cron_field(parts[3], 1, 12, MONTHS)
        && cron_field_extra(parts[4], 0, 7, DAYS, "L#?")
}

fn cron_field(field: &str, min: i64, max: i64, names: &[&str]) -> bool {
    cron_field_extra(field, min, max, names, "")
}

fn cron_field_extra(field: &str, min: i64, max: i64, names: &[&str], special: &str) -> bool {
    let value = |v: &str| -> Option<i64> {
        if let Ok(n) = v.parse::<i64>() {
            return (n >= min && n <= max).then_some(n);
        }
        names
            .iter()
            .position(|n| n.eq_ignore_ascii_case(v))
            .map(|i| i as i64 + min)
    };
    field.split(',').all(|part| {
        if part.is_empty() {
            return false;
        }
        if part == "?" && special.contains('?') {
            return true;
        }
        if !special.is_empty() && part.chars().any(|c| "LW#".contains(c)) {
            static SPECIAL: LazyLock<Regex> = LazyLock::new(|| {
                Regex::new(r"^(L|LW|\d{1,2}W|\d?L|\d#[1-5])$").unwrap_or_else(|_| unreachable!())
            });
            return SPECIAL.is_match(part);
        }
        let (range, step) = match part.split_once('/') {
            Some((r, s)) => (r, Some(s)),
            None => (part, None),
        };
        if let Some(s) = step
            && !s.parse::<i64>().is_ok_and(|n| n > 0 && n <= max)
        {
            return false;
        }
        if range == "*" {
            return true;
        }
        match range.split_once('-') {
            Some((a, b)) => matches!((value(a), value(b)), (Some(x), Some(y)) if x <= y),
            None => value(range).is_some(),
        }
    })
}

/// `validateDatabasesBackupInput`.
fn validate_databases_input(input: &str) -> Result<(), String> {
    use coolify_jobs::compose_parser::validate::validate_shell_safe_path as safe;
    for entry in input.split('|') {
        let entry = entry.trim();
        if entry.is_empty() || entry == "all" || entry == "*" {
            continue;
        }
        if let Some((database, collections)) = entry.split_once(':') {
            safe(database, "database name")?;
            for c in collections.split(',') {
                let c = c.trim();
                if !c.is_empty() {
                    safe(c, "collection name")?;
                }
            }
        } else {
            for db in entry.split(',') {
                let db = db.trim();
                if !db.is_empty() && db != "all" && db != "*" {
                    safe(db, "database name")?;
                }
            }
        }
    }
    Ok(())
}

fn number(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

type Columns = (Vec<(&'static str, ColumnValue)>, Vec<(&'static str, f64)>);

/// `$request->only($backupConfigFields)` minus the S3 UUIDs.
fn backup_columns(input: &Input) -> Columns {
    let mut values: Vec<(&'static str, ColumnValue)> = Vec::new();
    let mut numeric: Vec<(&'static str, f64)> = Vec::new();
    for key in BACKUP_FIELDS {
        let Some(v) = input.get(key) else { continue };
        if *key == "s3_storage_uuid" || *key == "s3_storage_uuids" {
            continue;
        }
        if BOOLS.contains(key) {
            values.push((
                key,
                ColumnValue::Bool(match v {
                    Value::Bool(b) => *b,
                    Value::Number(n) => n.as_i64() == Some(1),
                    Value::String(s) => s == "1",
                    _ => false,
                }),
            ));
        } else if INTS.contains(key) {
            if let Some(n) = number(v) {
                values.push((key, ColumnValue::Int(n as i64)));
            }
        } else if FLOATS.contains(key) {
            if let Some(n) = number(v) {
                numeric.push((key, n));
            }
        } else {
            values.push((key, ColumnValue::Text(v.as_str().map(str::to_owned))));
        }
    }
    (values, numeric)
}

/// `$database->isBackupSolutionAvailable()`.
fn backup_available(kind: Kind) -> bool {
    matches!(
        kind,
        Kind::Postgresql
            | Kind::Mysql
            | Kind::Mariadb
            | Kind::Mongodb
            | Kind::Clickhouse
            | Kind::Sqlite
    )
}

/// `POST /databases/{uuid}/backups` (`create_backup`).
pub async fn create(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let input = validated_json(&headers, &uri, &body)?;
    validate_backup_input(&state, &input, true).await?;
    let d = dbs::find_in_team(&state.db, ctx.team_id, &uuid)
        .await?
        .ok_or(NOT_FOUND)?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let backup_now = input.get("backup_now").is_some_and(php_truthy);
    if backup_now {
        message_403(&ctx, BACKUP_NOW_REASON)?;
    }
    if !backup_available(d.kind) {
        return Err(ApiError::Message(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Scheduled backups are not supported for this database type.",
        ));
    }
    if !valid_cron(input.str("frequency").unwrap_or_default()) {
        let mut m = Map::new();
        m.insert(
            "frequency".into(),
            json!(["Invalid cron expression or frequency format."]),
        );
        return Err(ApiError::Validation(m));
    }
    let storages = requested_storages(&state, &ctx, &input).await?;
    if input_bool(&input, "save_s3") && storages.as_ref().is_none_or(Vec::is_empty) {
        let mut m = Map::new();
        m.insert(
            "s3_storage_uuid".into(),
            json!([
                "The s3_storage_uuid or s3_storage_uuids field is required when save_s3 is true."
            ]),
        );
        return Err(ApiError::Validation(m));
    }
    if input_bool(&input, "disable_local_backup") && !input_bool(&input, "save_s3") {
        let mut m = Map::new();
        m.insert(
            "disable_local_backup".into(),
            json!(["Local backups can only be disabled when save_s3 is true."]),
        );
        return Err(ApiError::Validation(m));
    }
    extra_fields_error(&input)?;
    let (mut values, numeric) = backup_columns(&input);
    if !input.get("databases_to_backup").is_some_and(php_truthy) {
        let default = match d.kind {
            Kind::Postgresql => d.str("postgres_db"),
            Kind::Mysql => d.str("mysql_database"),
            Kind::Mariadb => d.str("mariadb_database"),
            Kind::Clickhouse => d.str("clickhouse_db"),
            _ => None,
        };
        values.retain(|(k, _)| *k != "databases_to_backup");
        if let Some(default) = default {
            values.push((
                "databases_to_backup",
                ColumnValue::Text(Some(default.to_owned())),
            ));
        } else if input.has("databases_to_backup") {
            values.push(("databases_to_backup", ColumnValue::Text(None)));
        }
    }
    if let Some((_, ColumnValue::Text(Some(text)))) =
        values.iter().find(|(k, _)| *k == "databases_to_backup")
        && !text.is_empty()
        && text != "0"
        && let Err(e) = validate_databases_input(text)
    {
        let mut m = Map::new();
        m.insert("databases_to_backup".into(), json!([e]));
        return Err(ApiError::Validation(m));
    }
    if !values.iter().any(|(k, _)| *k == "enabled") {
        values.push(("enabled", ColumnValue::Bool(true)));
    }
    let now = state.now();
    let (id, backup_uuid) = parts::create_backup(
        &state.db,
        &NewBackup {
            team_id: ctx.team_id,
            morph: d.kind.morph(),
            database_id: d.id,
            fields: values,
            numeric_fields: numeric,
        },
        now,
    )
    .await?;
    if let Some(rows) = &storages {
        let ids: Vec<i64> = rows.iter().map(parts::id_of).collect();
        parts::sync_backup_storages(&state.db, id, None, &ids, now).await?;
    }
    if backup_now {
        let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
        jobs.spawn_database_backup(id);
    }
    tracing::info!(target: "audit", event = "api.database.backup_created", team_id = ctx.team_id, database_uuid = %d.uuid, backup_uuid = %backup_uuid, "backup configuration created");
    Ok(created(json!({
        "uuid": backup_uuid,
        "message": "Backup configuration created successfully.",
    }))
    .into_response())
}

/// `PATCH /databases/{uuid}/backups/{scheduled_backup_uuid}` (`update_backup`).
pub async fn update(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((uuid, backup_uuid)): Path<(String, String)>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let input = validated_json(&headers, &uri, &body)?;
    validate_backup_input(&state, &input, false).await?;
    let d = dbs::find_in_team(&state.db, ctx.team_id, &uuid)
        .await?
        .ok_or(NOT_FOUND)?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let backup_now = input.get("backup_now").is_some_and(php_truthy);
    if backup_now {
        message_403(&ctx, BACKUP_NOW_REASON)?;
    }
    if input.filled("frequency") && !valid_cron(input.str("frequency").unwrap_or_default()) {
        let mut m = Map::new();
        m.insert(
            "frequency".into(),
            json!(["Invalid cron expression or frequency format."]),
        );
        return Err(ApiError::Validation(m));
    }
    let storages = requested_storages(&state, &ctx, &input).await?;
    let row = parts::backup_of(&state.db, ctx.team_id, d.kind.morph(), d.id, &backup_uuid)
        .await?
        .ok_or(ApiError::Message(
            StatusCode::NOT_FOUND,
            "Backup config not found.",
        ))?;
    let id = parts::id_of(&row);
    let save_s3 = if input.has("save_s3") {
        input_bool(&input, "save_s3")
    } else {
        parts::bool_of(&row, "save_s3")
    };
    if (input_bool(&input, "save_s3") && storages.is_none())
        || (save_s3 && storages.as_ref().is_some_and(Vec::is_empty))
    {
        let mut m = Map::new();
        m.insert(
            "s3_storage_uuid".into(),
            json!([
                "The s3_storage_uuid or s3_storage_uuids field is required when save_s3 is true."
            ]),
        );
        return Err(ApiError::Validation(m));
    }
    extra_fields_error(&input)?;
    if input_bool(&input, "disable_local_backup") && !save_s3 {
        let mut m = Map::new();
        m.insert(
            "disable_local_backup".into(),
            json!(["Local backups can only be disabled when save_s3 is true."]),
        );
        return Err(ApiError::Validation(m));
    }
    let (mut values, numeric) = backup_columns(&input);
    if !save_s3 {
        values.retain(|(k, _)| *k != "disable_local_backup");
        values.push(("disable_local_backup", ColumnValue::Bool(false)));
    }
    if let Some((_, ColumnValue::Text(Some(text)))) =
        values.iter().find(|(k, _)| *k == "databases_to_backup")
        && !text.is_empty()
        && text != "0"
        && let Err(e) = validate_databases_input(text)
    {
        let mut m = Map::new();
        m.insert("databases_to_backup".into(), json!([e]));
        return Err(ApiError::Validation(m));
    }
    let now = state.now();
    let current_primary = row.get("s3_storage_id").and_then(Value::as_i64);
    let new_primary = storages.as_ref().map(|rows| rows.first().map(parts::id_of));
    if let Some(p) = new_primary {
        values.push((
            "s3_storage_id",
            match p {
                Some(i) => ColumnValue::Int(i),
                None => ColumnValue::Text(None),
            },
        ));
    }
    let mut storage_ids: Option<Vec<i64>> = storages
        .as_ref()
        .map(|rows| rows.iter().map(parts::id_of).collect());
    // An older client sends only s3_storage_uuid: when it is already a destination, the others stay.
    if let Some(ids) = &storage_ids
        && !input.has("s3_storage_uuids")
    {
        let mut current: Vec<i64> = parts::backup_destinations(&state.db, id)
            .await?
            .iter()
            .map(parts::id_of)
            .collect();
        if current.is_empty()
            && let Some(p) = current_primary
        {
            current.push(p);
        }
        if ids.first().is_some_and(|f| current.contains(f)) {
            storage_ids = Some(current);
        }
    }
    parts::update_backup(&state.db, id, &values, &numeric, now).await?;
    if let Some(ids) = storage_ids {
        let primary = new_primary.flatten().or(current_primary);
        parts::sync_backup_storages(&state.db, id, primary, &ids, now).await?;
    }
    if backup_now {
        let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
        jobs.spawn_database_backup(id);
    }
    tracing::info!(target: "audit", event = "api.database.backup_updated", team_id = ctx.team_id, backup_uuid = %backup_uuid, "backup configuration updated");
    Ok(ok(json!({"message": "Database backup configuration updated"})).into_response())
}

/// The secrets of a database for `redactBackupExecutionMessage`.
async fn message_secrets(state: &AppState, d: &DatabaseRow) -> Result<Vec<String>, ApiError> {
    let c = crate::routes::databases::credentials(state, d).await?;
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
    Ok(out)
}

/// `redactBackupExecutionMessage`.
fn redact_message(ctx: &AuthCtx, message: Option<&str>, secrets: &[String]) -> Value {
    match message {
        Some(m) if ctx.can_read_sensitive => {
            let locked: Vec<&str> = secrets.iter().map(String::as_str).collect();
            Value::String(coolify_compat::redact::redact_text(m, &[], &locked))
        }
        _ => Value::Null,
    }
}

/// An execution as the model serializes it (`size` integer, the rest cast).
fn present_execution(ctx: &AuthCtx, row: &Record, secrets: &[String]) -> Value {
    let mut r = BACKUP_EXECUTION.present(row.clone());
    let size = row
        .get("size")
        .and_then(Value::as_str)
        .map(|s| Value::from(s.trim().parse::<i64>().unwrap_or(0)))
        .unwrap_or(Value::Null);
    r.insert("size".into(), size);
    r.insert(
        "message".into(),
        redact_message(ctx, parts::str_of(row, "message"), secrets),
    );
    Value::Object(r)
}

/// `withBackupS3StorageUuids`: the destinations' UUIDs, primary first.
async fn with_storage_uuids(state: &AppState, row: &Record) -> Result<Record, ApiError> {
    let mut r = SCHEDULED_DATABASE_BACKUP.present(row.clone());
    for f in FLOATS {
        if let Some(v) = row.get(*f).and_then(number) {
            r.insert((*f).into(), json!(v));
        }
    }
    let id = parts::id_of(row);
    let primary = row.get("s3_storage_id").and_then(Value::as_i64);
    let mut rows = parts::backup_destinations(&state.db, id).await?;
    rows.sort_by_key(|x| i32::from(Some(parts::id_of(x)) != primary));
    let primary_row = match primary {
        Some(p) => parts::s3_by_id(&state.db, p).await?,
        None => None,
    };
    if rows.is_empty()
        && let Some(p) = &primary_row
    {
        rows.push(p.clone());
    }
    r.insert(
        "s3_storage_uuid".into(),
        primary_row
            .as_ref()
            .and_then(|p| parts::str_of(p, "uuid"))
            .map_or(Value::Null, |u| Value::String(u.to_owned())),
    );
    r.insert(
        "s3_storage_uuids".into(),
        Value::Array(
            rows.iter()
                .filter_map(|x| parts::str_of(x, "uuid").map(|u| Value::String(u.to_owned())))
                .collect(),
        ),
    );
    Ok(r)
}

/// `backup_configs` of `GET /databases`: with `latest_log`.
pub(crate) async fn configs_with_latest_log(
    state: &AppState,
    ctx: &AuthCtx,
    d: &DatabaseRow,
) -> Result<Value, ApiError> {
    let rows = parts::backups_of(&state.db, ctx.team_id, d.kind.morph(), d.id).await?;
    if rows.is_empty() {
        return Ok(json!([]));
    }
    let secrets = message_secrets(state, d).await.unwrap_or_default();
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        let mut r = with_storage_uuids(state, row).await?;
        let latest = parts::latest_execution(&state.db, parts::id_of(row)).await?;
        r.insert(
            "latest_log".into(),
            latest.map_or(Value::Null, |e| present_execution(ctx, &e, &secrets)),
        );
        out.push(Value::Object(r));
    }
    Ok(Value::Array(out))
}

/// `GET /databases/{uuid}/backups` (`database_backup_details_uuid`).
pub async fn details(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let d = dbs::find_in_team(&state.db, ctx.team_id, &uuid)
        .await?
        .ok_or(NOT_FOUND)?;
    let rows = parts::backups_of(&state.db, ctx.team_id, d.kind.morph(), d.id).await?;
    let secrets = message_secrets(&state, &d).await.unwrap_or_default();
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        let mut r = with_storage_uuids(&state, row).await?;
        let executions = parts::executions_of(&state.db, parts::id_of(row)).await?;
        r.insert(
            "executions".into(),
            Value::Array(
                executions
                    .iter()
                    .map(|e| present_execution(&ctx, e, &secrets))
                    .collect(),
            ),
        );
        out.push(Value::Object(r));
    }
    Ok(ok(Value::Array(out)).into_response())
}

async fn find_backup(
    state: &AppState,
    ctx: &AuthCtx,
    d: &DatabaseRow,
    backup_uuid: &str,
) -> Result<Record, ApiError> {
    parts::backup_of(&state.db, ctx.team_id, d.kind.morph(), d.id, backup_uuid)
        .await?
        .ok_or(ApiError::Message(
            StatusCode::NOT_FOUND,
            "Backup configuration not found.",
        ))
}

/// `GET /databases/{uuid}/backups/{scheduled_backup_uuid}/executions` (`list_backup_executions`).
pub async fn executions(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((uuid, backup_uuid)): Path<(String, String)>,
) -> ApiResult {
    let d = dbs::find_in_team(&state.db, ctx.team_id, &uuid)
        .await?
        .ok_or(NOT_FOUND)?;
    let row = find_backup(&state, &ctx, &d, &backup_uuid).await?;
    let secrets = message_secrets(&state, &d).await.unwrap_or_default();
    let list = parts::executions_of(&state.db, parts::id_of(&row)).await?;
    let items: Vec<Value> = list
        .iter()
        .map(|e| {
            let size = e
                .get("size")
                .and_then(Value::as_str)
                .map(|s| Value::from(s.trim().parse::<i64>().unwrap_or(0)))
                .unwrap_or(Value::Null);
            let created_at = parts::str_of(e, "created_at")
                .map(|c| format!("{c}+00:00"))
                .map_or(Value::Null, Value::String);
            let mut m = Map::new();
            m.insert("uuid".into(), e.get("uuid").cloned().unwrap_or(Value::Null));
            m.insert(
                "filename".into(),
                e.get("filename").cloned().unwrap_or(Value::Null),
            );
            m.insert("size".into(), size);
            m.insert("created_at".into(), created_at);
            m.insert(
                "message".into(),
                redact_message(&ctx, parts::str_of(e, "message"), &secrets),
            );
            m.insert(
                "status".into(),
                e.get("status").cloned().unwrap_or(Value::Null),
            );
            Value::Object(m)
        })
        .collect();
    Ok(PhpJson(StatusCode::OK, json!({"executions": items})).into_response())
}

fn wants_s3_delete(uri: &Uri, input: &Input) -> bool {
    if let Some(v) = query_param(uri, "delete_s3") {
        return matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "on" | "yes");
    }
    input_bool(input, "delete_s3")
}

/// `DELETE /databases/{uuid}/backups/{scheduled_backup_uuid}` (`delete_backup_by_uuid`).
pub async fn delete(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((uuid, backup_uuid)): Path<(String, String)>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let d = dbs::find_in_team(&state.db, ctx.team_id, &uuid)
        .await?
        .ok_or(NOT_FOUND)?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let row = find_backup(&state, &ctx, &d, &backup_uuid).await?;
    let delete_s3 = wants_s3_delete(&uri, &lenient_json(&headers, &uri, &body));
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    let failed = ApiError::Message(
        StatusCode::INTERNAL_SERVER_ERROR,
        "Failed to delete backup.",
    );
    let server = server_info(&state, &d).await?;
    let id = parts::id_of(&row);
    let executions = parts::executions_of(&state.db, id).await?;
    for e in &executions {
        if let Some(file) = parts::str_of(e, "filename").filter(|f| !f.is_empty() && *f != "0")
            && let Some(server) = &server
        {
            jobs.delete_local_backups(&[file.to_owned()], server).await;
        }
        if delete_s3 && jobs.delete_s3_copies(e).await.is_err() {
            return Err(failed);
        }
        parts::delete_execution(&state.db, parts::id_of(e)).await?;
    }
    parts::delete_backup(&state.db, id).await?;
    tracing::info!(target: "audit", event = "api.database.backup_deleted", team_id = ctx.team_id, database_uuid = %d.uuid, backup_uuid = %backup_uuid, delete_s3, "backup configuration deleted");
    Ok(ok(json!({"message": "Backup configuration and all executions deleted."})).into_response())
}

async fn server_info(
    state: &AppState,
    d: &DatabaseRow,
) -> Result<Option<coolify_jobs::server::ServerInfo>, ApiError> {
    let Some(id) = d.server_id() else {
        return Ok(None);
    };
    Ok(servers::find_by_id(&state.db, id)
        .await?
        .map(|r| coolify_jobs::server::ServerInfo::from_row(&r)))
}

/// `DELETE /databases/{uuid}/backups/{scheduled_backup_uuid}/executions/{execution_uuid}`.
pub async fn delete_execution(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((uuid, backup_uuid, execution_uuid)): Path<(String, String, String)>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let d = dbs::find_in_team(&state.db, ctx.team_id, &uuid)
        .await?
        .ok_or(NOT_FOUND)?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let row = find_backup(&state, &ctx, &d, &backup_uuid).await?;
    let execution = parts::execution_of(&state.db, parts::id_of(&row), &execution_uuid)
        .await?
        .ok_or(ApiError::Message(
            StatusCode::NOT_FOUND,
            "Backup execution not found.",
        ))?;
    let delete_s3 = wants_s3_delete(&uri, &lenient_json(&headers, &uri, &body));
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    let server = server_info(&state, &d).await?;
    if let Some(file) = parts::str_of(&execution, "filename").filter(|f| !f.is_empty() && *f != "0")
        && let Some(server) = &server
    {
        jobs.delete_local_backups(&[file.to_owned()], server).await;
    }
    if delete_s3 && jobs.delete_s3_copies(&execution).await.is_err() {
        return Err(ApiError::Message(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to delete backup execution.",
        ));
    }
    parts::delete_execution(&state.db, parts::id_of(&execution)).await?;
    tracing::info!(target: "audit", event = "api.database.backup_execution_deleted", team_id = ctx.team_id, database_uuid = %d.uuid, backup_uuid = %backup_uuid, execution_uuid = %execution_uuid, delete_s3, "backup execution deleted");
    Ok(ok(json!({"message": "Backup execution deleted."})).into_response())
}

#[cfg(test)]
mod tests {
    use super::valid_cron;

    #[test]
    fn cron_expressions() {
        for good in [
            "* * * * *",
            "0 0 * * *",
            "*/5 1-3 * JAN MON",
            "@daily",
            "daily",
            "0 0 L * *",
            "0 0 * * 5#2",
        ] {
            assert!(valid_cron(good), "{good}");
        }
        for bad in ["", "* * *", "60 * * * *", "a b c d e", "* * * * * *"] {
            assert!(!valid_cron(bad), "{bad}");
        }
    }
}
