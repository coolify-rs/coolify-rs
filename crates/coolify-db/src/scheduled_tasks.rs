//! `scheduled_tasks` and `scheduled_task_executions` (`ScheduledTask`, `ScheduledTaskExecution`).
//! A task belongs to an application or a service, see [`Owner`].

use chrono::NaiveDateTime;
use coolify_domain::new_public_id;
use serde_json::Value;

use crate::records::{Record, parse_row};
use crate::{Db, DbError, DbResult};

/// `$application->scheduled_tasks()` or `$service->scheduled_tasks()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Owner {
    Application(i64),
    Service(i64),
}

impl Owner {
    fn column_and_id(self) -> (&'static str, i64) {
        match self {
            Owner::Application(id) => ("application_id", id),
            Owner::Service(id) => ("service_id", id),
        }
    }
}

fn decode(t: &str) -> DbResult<Record> {
    parse_row(t).map_err(|e| DbError::Schema(format!("decoding row: {e}")))
}

/// `$resource->scheduled_tasks`: by name.
pub async fn list(db: &Db, owner: Owner) -> DbResult<Vec<Record>> {
    let (column, id) = owner.column_and_id();
    let rows: Vec<String> = sqlx::query_scalar(&format!(
        "SELECT row_to_json(t)::text FROM scheduled_tasks t WHERE {column} = $1 ORDER BY name ASC"
    ))
    .bind(id)
    .fetch_all(db.pool())
    .await?;
    rows.iter().map(|t| decode(t)).collect()
}

/// `$resource->scheduled_tasks()->where('uuid', $uuid)->first()`.
pub async fn find(db: &Db, owner: Owner, uuid: &str) -> DbResult<Option<Record>> {
    let (column, id) = owner.column_and_id();
    let row: Option<String> = sqlx::query_scalar(&format!(
        "SELECT row_to_json(t)::text FROM scheduled_tasks t WHERE {column} = $1 AND uuid = $2 \
         ORDER BY name ASC LIMIT 1"
    ))
    .bind(id)
    .bind(uuid)
    .fetch_optional(db.pool())
    .await?;
    row.map(|t| decode(&t)).transpose()
}

/// `$task->save()` of a new task.
#[derive(Debug)]
pub struct NewTask<'a> {
    pub name: &'a str,
    pub command: &'a str,
    pub frequency: &'a str,
    pub container: Option<&'a str>,
    pub timeout: i64,
    pub enabled: bool,
    pub team_id: i64,
}

pub async fn create(
    db: &Db,
    owner: Owner,
    task: &NewTask<'_>,
    now: NaiveDateTime,
) -> DbResult<Record> {
    db.ensure_writable()?;
    let (column, id) = owner.column_and_id();
    let row: String = sqlx::query_scalar(&format!(
        "INSERT INTO scheduled_tasks (uuid, name, command, frequency, container, timeout, enabled, team_id, {column}, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $10) RETURNING row_to_json(scheduled_tasks)::text"
    ))
    .bind(new_public_id())
    .bind(task.name)
    .bind(task.command)
    .bind(task.frequency)
    .bind(task.container)
    .bind(task.timeout)
    .bind(task.enabled)
    .bind(task.team_id)
    .bind(id)
    .bind(now)
    .fetch_one(db.pool())
    .await?;
    decode(&row)
}

/// What `$task->update()` changes; `None` leaves a field alone.
#[derive(Debug, Default)]
pub struct TaskChanges {
    pub name: Option<String>,
    pub command: Option<String>,
    pub frequency: Option<String>,
    pub container: Option<Option<String>>,
    pub timeout: Option<i64>,
    pub enabled: Option<bool>,
}

/// `$task->update(...)`: writes only when an attribute changed, and then touches `updated_at`.
pub async fn update(
    db: &Db,
    task: &Record,
    changes: &TaskChanges,
    now: NaiveDateTime,
) -> DbResult<Record> {
    db.ensure_writable()?;
    let id = task.get("id").and_then(Value::as_i64).unwrap_or_default();
    let text = |k: &str| task.get(k).and_then(Value::as_str);
    let dirty = changes
        .name
        .as_deref()
        .is_some_and(|v| Some(v) != text("name"))
        || changes
            .command
            .as_deref()
            .is_some_and(|v| Some(v) != text("command"))
        || changes
            .frequency
            .as_deref()
            .is_some_and(|v| Some(v) != text("frequency"))
        || changes
            .container
            .as_ref()
            .is_some_and(|v| v.as_deref() != text("container"))
        || changes
            .timeout
            .is_some_and(|v| Some(v) != task.get("timeout").and_then(Value::as_i64))
        || changes
            .enabled
            .is_some_and(|v| Some(v) != task.get("enabled").and_then(Value::as_bool));
    if !dirty {
        return Ok(task.clone());
    }
    let row: String = sqlx::query_scalar(
        "UPDATE scheduled_tasks SET name = COALESCE($2, name), command = COALESCE($3, command), \
         frequency = COALESCE($4, frequency), container = CASE WHEN $5 THEN $6 ELSE container END, \
         timeout = COALESCE($7, timeout), enabled = COALESCE($8, enabled), updated_at = $9 \
         WHERE id = $1 RETURNING row_to_json(scheduled_tasks)::text",
    )
    .bind(id)
    .bind(changes.name.as_deref())
    .bind(changes.command.as_deref())
    .bind(changes.frequency.as_deref())
    .bind(changes.container.is_some())
    .bind(changes.container.clone().flatten())
    .bind(changes.timeout)
    .bind(changes.enabled)
    .bind(now)
    .fetch_one(db.pool())
    .await?;
    decode(&row)
}

/// `$task->delete()`: executions stay, there is no foreign key and no model event.
pub async fn delete(db: &Db, id: i64) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query("DELETE FROM scheduled_tasks WHERE id = $1")
        .bind(id)
        .execute(db.pool())
        .await?;
    Ok(())
}

/// `$task->executions()->get()`: last execution first.
pub async fn executions(db: &Db, task_id: i64) -> DbResult<Vec<Record>> {
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT row_to_json(e)::text FROM scheduled_task_executions e \
         WHERE scheduled_task_id = $1 ORDER BY created_at DESC",
    )
    .bind(task_id)
    .fetch_all(db.pool())
    .await?;
    rows.iter().map(|t| decode(t)).collect()
}

/// A task with what the scheduler and `ScheduledTaskJob` read through its relations.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct TaskContext {
    pub id: i64,
    pub uuid: String,
    pub name: String,
    pub command: String,
    pub frequency: String,
    pub container: Option<String>,
    pub timeout: i32,
    pub team_id: i64,
    /// `$task->application`: `None` when missing or soft deleted.
    pub application_id: Option<i64>,
    pub application_uuid: Option<String>,
    pub application_status: Option<String>,
    pub application_environment_id: Option<i64>,
    /// `$task->service`: `None` when missing or soft deleted.
    pub service_id: Option<i64>,
    pub service_uuid: Option<String>,
    pub service_environment_id: Option<i64>,
    /// `$task->server()`: the server of the resource's destination.
    pub server_id: Option<i64>,
}

const CONTEXT: &str = "SELECT t.id, t.uuid, t.name, t.command, t.frequency, t.container, t.timeout, t.team_id, \
    a.id AS application_id, a.uuid AS application_uuid, a.status AS application_status, a.environment_id AS application_environment_id, \
    sv.id AS service_id, sv.uuid AS service_uuid, sv.environment_id AS service_environment_id, \
    CASE WHEN a.id IS NOT NULL THEN COALESCE(ad.server_id, asw.server_id) \
         WHEN sv.id IS NOT NULL THEN COALESCE(sd.server_id, ssw.server_id) END AS server_id \
    FROM scheduled_tasks t \
    LEFT JOIN applications a ON a.id = t.application_id AND a.deleted_at IS NULL \
    LEFT JOIN services sv ON sv.id = t.service_id AND sv.deleted_at IS NULL \
    LEFT JOIN standalone_dockers ad ON a.destination_type = 'App\\Models\\StandaloneDocker' AND ad.id = a.destination_id \
    LEFT JOIN swarm_dockers asw ON a.destination_type = 'App\\Models\\SwarmDocker' AND asw.id = a.destination_id \
    LEFT JOIN standalone_dockers sd ON sv.destination_type = 'App\\Models\\StandaloneDocker' AND sd.id = sv.destination_id \
    LEFT JOIN swarm_dockers ssw ON sv.destination_type = 'App\\Models\\SwarmDocker' AND ssw.id = sv.destination_id";

/// `ScheduledTask::where('enabled', true)->orderBy('id')`, with the relations.
pub async fn enabled(db: &Db) -> DbResult<Vec<TaskContext>> {
    Ok(
        sqlx::query_as(&format!("{CONTEXT} WHERE t.enabled = true ORDER BY t.id"))
            .fetch_all(db.pool())
            .await?,
    )
}

/// `ScheduledTask::find($id)`, with the relations.
pub async fn context(db: &Db, id: i64) -> DbResult<Option<TaskContext>> {
    Ok(sqlx::query_as(&format!("{CONTEXT} WHERE t.id = $1"))
        .bind(id)
        .fetch_optional(db.pool())
        .await?)
}

/// `$service->applications()->get()` then `$service->databases()->get()`: `(name, status, exclude_from_status)`.
pub async fn service_resources(
    db: &Db,
    service_id: i64,
) -> DbResult<Vec<(String, Option<String>, bool)>> {
    let mut rows: Vec<(String, Option<String>, bool)> = sqlx::query_as(
        "SELECT name, status, exclude_from_status FROM service_applications \
         WHERE service_id = $1 AND deleted_at IS NULL",
    )
    .bind(service_id)
    .fetch_all(db.pool())
    .await?;
    let databases: Vec<(String, Option<String>, bool)> = sqlx::query_as(
        "SELECT name, status, exclude_from_status FROM service_databases \
         WHERE service_id = $1 AND deleted_at IS NULL",
    )
    .bind(service_id)
    .fetch_all(db.pool())
    .await?;
    rows.extend(databases);
    Ok(rows)
}

/// `Team::findOrFail($task->team_id)`.
pub async fn team_exists(db: &Db, team_id: i64) -> DbResult<bool> {
    Ok(
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM teams WHERE id = $1)")
            .bind(team_id)
            .fetch_one(db.pool())
            .await?,
    )
}

/// What `logRedactionSecretsFor($service)` reads: environment, project, `redactsAllEnvValuesInLogs()`.
pub async fn service_log_context(
    db: &Db,
    service_id: i64,
) -> DbResult<Option<(Option<i64>, Option<i64>, bool)>> {
    Ok(sqlx::query_as(
        "SELECT s.environment_id, e.project_id, COALESCE(s.redact_all_env_values_in_logs, true) \
         FROM services s LEFT JOIN environments e ON e.id = s.environment_id WHERE s.id = $1",
    )
    .bind(service_id)
    .fetch_optional(db.pool())
    .await?)
}

/// `ScheduledTaskExecution::forceCreate(...)`: a running execution. Returns its id.
pub async fn start_execution(
    db: &Db,
    uuid: &str,
    task_id: i64,
    retry_count: i32,
    started_at: NaiveDateTime,
) -> DbResult<i64> {
    db.ensure_writable()?;
    let (column, value, host) =
        crate::worker_host::insert_parts(db.schema(), "scheduled_task_executions", 5);
    let sql = format!(
        "INSERT INTO scheduled_task_executions (uuid, scheduled_task_id, started_at, retry_count, created_at, updated_at{column}) \
         VALUES ($1, $2, $3, $4, $3, $3{value}) RETURNING id"
    );
    let mut query = sqlx::query_scalar(&sql)
        .bind(uuid)
        .bind(task_id)
        .bind(started_at)
        .bind(retry_count);
    if let Some(host) = host {
        query = query.bind(host);
    }
    Ok(query.fetch_one(db.pool()).await?)
}

/// `$this->task_log->update(['status' => ..., 'message' => ...])`.
pub async fn set_execution_result(
    db: &Db,
    id: i64,
    status: &str,
    message: &str,
    now: NaiveDateTime,
) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(
        "UPDATE scheduled_task_executions SET status = $2, message = $3, updated_at = $4 WHERE id = $1",
    )
    .bind(id)
    .bind(status)
    .bind(message)
    .bind(now)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// `$this->task_log->update(['finished_at' => ..., 'duration' => ...])`.
pub async fn finish_execution(
    db: &Db,
    id: i64,
    finished_at: NaiveDateTime,
    duration: f64,
) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(
        "UPDATE scheduled_task_executions SET finished_at = $2, duration = round($3::numeric, 2), updated_at = $2 WHERE id = $1",
    )
    .bind(id)
    .bind(finished_at)
    .bind(duration)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// The update of `ScheduledTaskJob::failed()`, for an execution that has not finished.
pub async fn fail_unfinished_execution(
    db: &Db,
    uuid: &str,
    message: &str,
    now: NaiveDateTime,
) -> DbResult<bool> {
    db.ensure_writable()?;
    let done = sqlx::query(
        "UPDATE scheduled_task_executions SET status = 'failed', message = $2, finished_at = $3, updated_at = $3 \
         WHERE uuid = $1 AND finished_at IS NULL",
    )
    .bind(uuid)
    .bind(message)
    .bind(now)
    .execute(db.pool())
    .await?;
    Ok(done.rows_affected() > 0)
}

/// A run that a stopped worker left behind. Upstream notices when Redis hands the job out again;
/// here tasks live in the process, so startup does it. Returns how many.
pub async fn fail_interrupted_executions(
    db: &Db,
    message: &str,
    now: NaiveDateTime,
) -> DbResult<u64> {
    db.ensure_writable()?;
    let done = sqlx::query(
        "UPDATE scheduled_task_executions SET status = 'failed', message = $1, finished_at = $2, updated_at = $2 \
         WHERE status = 'running' AND finished_at IS NULL",
    )
    .bind(message)
    .bind(now)
    .execute(db.pool())
    .await?;
    Ok(done.rows_affected())
}

/// `ScheduledTaskExecution::create(...)` of `reportMissedOccurrence()`.
pub async fn record_missed_execution(
    db: &Db,
    task_id: i64,
    message: &str,
    now: NaiveDateTime,
) -> DbResult<()> {
    db.ensure_writable()?;
    let (column, value, host) =
        crate::worker_host::insert_parts(db.schema(), "scheduled_task_executions", 5);
    let sql = format!(
        "INSERT INTO scheduled_task_executions (uuid, scheduled_task_id, status, message, retry_count, finished_at, created_at, updated_at{column}) \
         VALUES ($1, $2, 'failed', $3, 0, $4, $4, $4{value})"
    );
    let mut query = sqlx::query(&sql)
        .bind(new_public_id())
        .bind(task_id)
        .bind(message)
        .bind(now);
    if let Some(host) = host {
        query = query.bind(host);
    }
    query.execute(db.pool()).await?;
    Ok(())
}
