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
    /// `use_shell`: run the command with `sh -c` in the container.
    pub use_shell: bool,
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
        "INSERT INTO scheduled_tasks (uuid, name, command, frequency, container, timeout, enabled, team_id, {column}, created_at, updated_at, use_shell) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $10, $11) RETURNING row_to_json(scheduled_tasks)::text"
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
    .bind(task.use_shell)
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
    pub use_shell: Option<bool>,
    pub enabled: Option<bool>,
}

/// The morph class of `ScheduledTask` and the class of its failure notification, the key of the
/// throttle that `TaskFailed` claims.
const TASK_MODEL: &str = "App\\Models\\ScheduledTask";
const TASK_FAILED_NOTIFICATION: &str = "App\\Notifications\\ScheduledTask\\TaskFailed";

/// `NotificationThrottle::release($task, TaskFailed::class)`: true when a record was removed.
pub async fn release_failure_throttle(db: &Db, task_id: i64) -> DbResult<bool> {
    db.ensure_writable()?;
    let done = sqlx::query(
        "DELETE FROM notification_throttles WHERE notifiable_type = $1 AND notifiable_id = $2 AND notification = $3",
    )
    .bind(TASK_MODEL)
    .bind(task_id)
    .bind(TASK_FAILED_NOTIFICATION)
    .execute(db.pool())
    .await?;
    Ok(done.rows_affected() > 0)
}

/// `$task->update(...)`: writes only when an attribute changed, and then touches `updated_at`.
/// The `saving` and `saved` hooks of `ScheduledTask`: a task that is enabled again starts a new
/// failure streak, and its next failure notifies at once.
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
            .use_shell
            .is_some_and(|v| Some(v) != task.get("use_shell").and_then(Value::as_bool))
        || changes
            .enabled
            .is_some_and(|v| Some(v) != task.get("enabled").and_then(Value::as_bool));
    if !dirty {
        return Ok(task.clone());
    }
    // `$task->isDirty('enabled') && $task->enabled`.
    let enabled_again =
        changes.enabled == Some(true) && task.get("enabled").and_then(Value::as_bool) != Some(true);
    let row: String = sqlx::query_scalar(
        "UPDATE scheduled_tasks SET name = COALESCE($2, name), command = COALESCE($3, command), \
         frequency = COALESCE($4, frequency), container = CASE WHEN $5 THEN $6 ELSE container END, \
         timeout = COALESCE($7, timeout), enabled = COALESCE($8, enabled), updated_at = $9, \
         use_shell = COALESCE($10, use_shell), \
         consecutive_failures = CASE WHEN $11 THEN 0 ELSE consecutive_failures END, \
         failing_since = CASE WHEN $11 THEN NULL ELSE failing_since END, \
         disabled_reason = CASE WHEN $11 THEN NULL ELSE disabled_reason END \
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
    .bind(changes.use_shell)
    .bind(enabled_again)
    .fetch_one(db.pool())
    .await?;
    if enabled_again {
        release_failure_throttle(db, id).await?;
    }
    decode(&row)
}

/// `ScheduledTask::AUTO_DISABLE_AFTER_FAILURES`: a task is disabled automatically only when it
/// failed this many times in a row...
pub const AUTO_DISABLE_AFTER_FAILURES: i32 = 10;
/// `ScheduledTask::AUTO_DISABLE_AFTER_HOURS`: ...and its first failure is this old, so a short
/// outage does not disable a task that runs often.
pub const AUTO_DISABLE_AFTER_HOURS: i64 = 24;

/// What `ScheduledTask::recordFailure()` left in the row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailureStreak {
    /// This failure disabled the task.
    pub disabled: bool,
    pub consecutive_failures: i32,
    pub failing_since: Option<NaiveDateTime>,
    pub disabled_reason: Option<String>,
}

/// The decision of `recordFailure()` after the failure was counted: the task is disabled when it
/// is enabled, failed too often, and for too long.
pub fn should_auto_disable(
    enabled: bool,
    consecutive_failures: i32,
    failing_since: NaiveDateTime,
    now: NaiveDateTime,
) -> bool {
    enabled
        && consecutive_failures >= AUTO_DISABLE_AFTER_FAILURES
        && failing_since <= now - chrono::Duration::hours(AUTO_DISABLE_AFTER_HOURS)
}

/// The `disabled_reason` that `recordFailure()` writes.
pub fn auto_disabled_reason(consecutive_failures: i32, failing_since: NaiveDateTime) -> String {
    format!(
        "Disabled automatically after {consecutive_failures} failed runs in a row since {} UTC.",
        failing_since.format("%Y-%m-%d %H:%M:%S")
    )
}

/// `ScheduledTask::recordFailure()`: counts a failed run, and disables the task when it failed
/// too often for too long. `None` when the task no longer exists.
pub async fn record_failure(
    db: &Db,
    id: i64,
    now: NaiveDateTime,
) -> DbResult<Option<FailureStreak>> {
    db.ensure_writable()?;
    let row: Option<(bool, i32, Option<NaiveDateTime>, Option<String>)> = sqlx::query_as(
        "UPDATE scheduled_tasks SET consecutive_failures = consecutive_failures + 1, \
         failing_since = COALESCE(failing_since, $2), updated_at = $2 \
         WHERE id = $1 RETURNING enabled, consecutive_failures, failing_since, disabled_reason",
    )
    .bind(id)
    .bind(now)
    .fetch_optional(db.pool())
    .await?;
    let Some((enabled, consecutive_failures, failing_since, disabled_reason)) = row else {
        return Ok(None);
    };
    let since = failing_since.unwrap_or(now);
    if !should_auto_disable(enabled, consecutive_failures, since, now) {
        return Ok(Some(FailureStreak {
            disabled: false,
            consecutive_failures,
            failing_since,
            disabled_reason,
        }));
    }
    let reason = auto_disabled_reason(consecutive_failures, since);
    sqlx::query("UPDATE scheduled_tasks SET enabled = false, disabled_reason = $2 WHERE id = $1")
        .bind(id)
        .bind(&reason)
        .execute(db.pool())
        .await?;
    Ok(Some(FailureStreak {
        disabled: true,
        consecutive_failures,
        failing_since,
        disabled_reason: Some(reason),
    }))
}

/// `ScheduledTask::recordSuccess()`: ends the failure streak after a successful run. True when
/// the team was notified about the failure, so it must be told that the task works again.
pub async fn record_success(db: &Db, id: i64, now: NaiveDateTime) -> DbResult<bool> {
    db.ensure_writable()?;
    sqlx::query(
        "UPDATE scheduled_tasks SET consecutive_failures = 0, failing_since = NULL, updated_at = $2 \
         WHERE id = $1 AND (consecutive_failures > 0 OR failing_since IS NOT NULL)",
    )
    .bind(id)
    .bind(now)
    .execute(db.pool())
    .await?;
    release_failure_throttle(db, id).await
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
    /// `use_shell`: run the command with `sh -c` in the container.
    pub use_shell: bool,
    /// The number of failed runs since the last successful run.
    pub consecutive_failures: i32,
    /// The first failed run since the last successful run.
    pub failing_since: Option<NaiveDateTime>,
    /// Why Coolify disabled the task automatically.
    pub disabled_reason: Option<String>,
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
    COALESCE(t.use_shell, true) AS use_shell, t.consecutive_failures, t.failing_since, t.disabled_reason, \
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
/// Parts that are no longer in the compose file (`is_removed_from_compose`) are left out: the
/// service status ignores them (v4.7).
pub async fn service_resources(
    db: &Db,
    service_id: i64,
) -> DbResult<Vec<(String, Option<String>, bool)>> {
    let mut rows: Vec<(String, Option<String>, bool)> = sqlx::query_as(
        "SELECT name, status, exclude_from_status FROM service_applications \
         WHERE service_id = $1 AND deleted_at IS NULL AND NOT is_removed_from_compose",
    )
    .bind(service_id)
    .fetch_all(db.pool())
    .await?;
    let databases: Vec<(String, Option<String>, bool)> = sqlx::query_as(
        "SELECT name, status, exclude_from_status FROM service_databases \
         WHERE service_id = $1 AND deleted_at IS NULL AND NOT is_removed_from_compose",
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

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M:%S").unwrap()
    }

    #[test]
    fn a_task_is_disabled_only_after_many_failures_over_a_long_time() {
        let now = at("2026-11-10 12:00:00");
        let long_ago = at("2026-11-09 12:00:00");
        let recently = at("2026-11-09 12:00:01");
        assert!(should_auto_disable(true, 10, long_ago, now));
        assert!(should_auto_disable(
            true,
            25,
            at("2026-11-01 00:00:00"),
            now
        ));
        // A task that runs often fails ten times in a short outage.
        assert!(!should_auto_disable(true, 10, recently, now));
        assert!(!should_auto_disable(
            true,
            500,
            at("2026-11-10 11:00:00"),
            now
        ));
        // A task that runs once a day needs ten failures.
        assert!(!should_auto_disable(
            true,
            9,
            at("2026-11-01 00:00:00"),
            now
        ));
        // A task that is already off is not disabled again.
        assert!(!should_auto_disable(false, 10, long_ago, now));
    }

    #[test]
    fn the_reason_names_the_streak() {
        assert_eq!(
            auto_disabled_reason(12, at("2026-11-08 03:04:05")),
            "Disabled automatically after 12 failed runs in a row since 2026-11-08 03:04:05 UTC."
        );
    }
}
