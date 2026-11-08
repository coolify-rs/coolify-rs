//! `GithubRunnerConfig`, `GithubRunnerExecution` and the columns of `GithubApp` the runners page reads.

use chrono::NaiveDateTime;
use serde_json::Value;

use crate::{Db, DbResult};

fn parse(rows: Vec<String>) -> Vec<Value> {
    rows.iter()
        .filter_map(|r| serde_json::from_str(r).ok())
        .collect()
}

/// `GithubRunners::githubApps`: the organization Apps of a team, without their secrets.
pub async fn organization_apps(db: &Db, team_id: i64) -> DbResult<Vec<Value>> {
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT row_to_json(t)::text FROM (SELECT id, uuid, name, organization, html_url, actions, \
         organization_self_hosted_runners, webhook_events FROM github_apps \
         WHERE team_id = $1 AND app_id IS NOT NULL AND installation_id IS NOT NULL \
         AND organization IS NOT NULL AND organization <> '' ORDER BY name) t",
    )
    .bind(team_id)
    .fetch_all(db.pool())
    .await?;
    Ok(parse(rows))
}

/// `$server->githubRunnerConfig()->first()`.
pub async fn config(db: &Db, server_id: i64) -> DbResult<Option<Value>> {
    let row: Option<String> = sqlx::query_scalar(
        "SELECT row_to_json(t)::text FROM (SELECT * FROM github_runner_configs WHERE server_id = $1 LIMIT 1) t",
    )
    .bind(server_id)
    .fetch_optional(db.pool())
    .await?;
    Ok(row.and_then(|r| serde_json::from_str(&r).ok()))
}

/// What `GithubRunners::submit` fills.
#[derive(Debug, Clone)]
pub struct ConfigFields {
    pub github_app_id: i64,
    pub labels: String,
    pub max_runners: i64,
    pub docker_mode: String,
    pub runner_image: Option<String>,
    pub cpu_limit: Option<String>,
    pub memory_limit: Option<String>,
    pub capacity_wait_timeout: i64,
    pub idle_timeout: i64,
    pub job_timeout: i64,
    pub is_dedicated: bool,
    pub allow_pull_requests: bool,
}

/// `GithubRunnerConfig::firstOrNew(...)->fill(...)->save()`: `updated_at` moves only for a change.
pub async fn save_config(
    db: &Db,
    uuid: &str,
    server_id: i64,
    f: &ConfigFields,
    now: NaiveDateTime,
) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(
        "INSERT INTO github_runner_configs (uuid, server_id, github_app_id, labels, max_runners, docker_mode, runner_image, \
         cpu_limit, memory_limit, capacity_wait_timeout, idle_timeout, job_timeout, is_dedicated, allow_pull_requests, created_at, updated_at) \
         VALUES ($1, $2, $3, $4::json, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $15) \
         ON CONFLICT (server_id) DO UPDATE SET github_app_id = EXCLUDED.github_app_id, labels = EXCLUDED.labels, \
         max_runners = EXCLUDED.max_runners, docker_mode = EXCLUDED.docker_mode, runner_image = EXCLUDED.runner_image, \
         cpu_limit = EXCLUDED.cpu_limit, memory_limit = EXCLUDED.memory_limit, capacity_wait_timeout = EXCLUDED.capacity_wait_timeout, \
         idle_timeout = EXCLUDED.idle_timeout, job_timeout = EXCLUDED.job_timeout, is_dedicated = EXCLUDED.is_dedicated, \
         allow_pull_requests = EXCLUDED.allow_pull_requests, updated_at = EXCLUDED.updated_at \
         WHERE (github_runner_configs.github_app_id, github_runner_configs.labels::text, github_runner_configs.max_runners, \
         github_runner_configs.docker_mode, github_runner_configs.runner_image, github_runner_configs.cpu_limit, github_runner_configs.memory_limit, \
         github_runner_configs.capacity_wait_timeout, github_runner_configs.idle_timeout, github_runner_configs.job_timeout, \
         github_runner_configs.is_dedicated, github_runner_configs.allow_pull_requests) IS DISTINCT FROM \
         (EXCLUDED.github_app_id, EXCLUDED.labels::text, EXCLUDED.max_runners, EXCLUDED.docker_mode, EXCLUDED.runner_image, EXCLUDED.cpu_limit, \
         EXCLUDED.memory_limit, EXCLUDED.capacity_wait_timeout, EXCLUDED.idle_timeout, EXCLUDED.job_timeout, EXCLUDED.is_dedicated, EXCLUDED.allow_pull_requests)",
    )
    .bind(uuid)
    .bind(server_id)
    .bind(f.github_app_id)
    .bind(&f.labels)
    .bind(f.max_runners as i16)
    .bind(&f.docker_mode)
    .bind(&f.runner_image)
    .bind(&f.cpu_limit)
    .bind(&f.memory_limit)
    .bind(f.capacity_wait_timeout as i32)
    .bind(f.idle_timeout as i32)
    .bind(f.job_timeout as i32)
    .bind(f.is_dedicated)
    .bind(f.allow_pull_requests)
    .bind(now)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// `$config->update(['is_enabled' => ...])`.
pub async fn set_enabled(
    db: &Db,
    server_id: i64,
    enabled: bool,
    now: NaiveDateTime,
) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(
        "UPDATE github_runner_configs SET is_enabled = $1, updated_at = $2 WHERE server_id = $3",
    )
    .bind(enabled)
    .bind(now)
    .bind(server_id)
    .execute(db.pool())
    .await?;
    Ok(())
}

const EXECUTION_SCOPE: &str = "(server_id = $1 OR ($2::bigint IS NOT NULL AND server_id IS NULL AND status = 'queued' AND github_app_id = $2))";

/// `GithubRunnerExecutions::executions`.
pub async fn executions(db: &Db, server_id: i64, app_id: Option<i64>) -> DbResult<Vec<Value>> {
    let rows: Vec<String> = sqlx::query_scalar(&format!(
        "SELECT row_to_json(t)::text FROM (SELECT id, uuid, status, server_id, runner_id, repository_full_name, workflow_name, job_name, \
         conclusion, error_message, workflow_job_html_url, created_at, started_at, completed_at FROM github_runner_executions \
         WHERE {EXECUTION_SCOPE} ORDER BY id DESC LIMIT 25) t"
    ))
    .bind(server_id)
    .bind(app_id)
    .fetch_all(db.pool())
    .await?;
    Ok(parse(rows))
}

/// `GithubRunnerExecutions::query()->whereKey($id)`: `(uuid, status, server_id)`.
pub async fn execution(
    db: &Db,
    server_id: i64,
    app_id: Option<i64>,
    id: i64,
) -> DbResult<Option<(String, String, Option<i64>)>> {
    Ok(sqlx::query_as(&format!(
        "SELECT uuid, status, server_id FROM github_runner_executions WHERE {EXECUTION_SCOPE} AND id = $3"
    ))
    .bind(server_id)
    .bind(app_id)
    .bind(id)
    .fetch_optional(db.pool())
    .await?)
}

/// The executions of a config in the `idle` state: `(id, uuid)`.
pub async fn idle_executions(db: &Db, config_id: i64) -> DbResult<Vec<(i64, String)>> {
    Ok(sqlx::query_as(
        "SELECT id, uuid FROM github_runner_executions WHERE github_runner_config_id = $1 AND status = 'idle'",
    )
    .bind(config_id)
    .fetch_all(db.pool())
    .await?)
}

/// `GithubRunnerExecution::finish`.
pub async fn finish_execution(
    db: &Db,
    id: i64,
    status: &str,
    error: &str,
    now: NaiveDateTime,
) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(
        "UPDATE github_runner_executions SET status = $1, error_message = $2, completed_at = COALESCE(completed_at, $3), updated_at = $3 WHERE id = $4",
    )
    .bind(status)
    .bind(error)
    .bind(now)
    .bind(id)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// A `github_runner_configs` row as the provisioning reads it.
#[derive(Debug, Clone)]
pub struct RunnerConfig {
    pub id: i64,
    pub server_id: i64,
    pub github_app_id: i64,
    pub docker_mode: String,
    pub cpu_limit: Option<String>,
    pub memory_limit: Option<String>,
    pub runner_image: Option<String>,
    pub labels: Vec<String>,
    pub allow_pull_requests: bool,
    pub max_runners: i64,
    pub idle_timeout: i64,
    pub job_timeout: i64,
}

fn strings(v: Option<&Value>) -> Vec<String> {
    v.and_then(|x| match x {
        Value::String(s) => serde_json::from_str::<Value>(s).ok(),
        other => Some(other.clone()),
    })
    .and_then(|x| x.as_array().cloned())
    .unwrap_or_default()
    .iter()
    .filter_map(|x| x.as_str().map(str::to_owned))
    .collect()
}

fn config_of(v: &Value) -> RunnerConfig {
    let int = |k: &str| v.get(k).and_then(Value::as_i64).unwrap_or(0);
    let text = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_owned);
    RunnerConfig {
        id: int("id"),
        server_id: int("server_id"),
        github_app_id: int("github_app_id"),
        docker_mode: text("docker_mode").unwrap_or_default(),
        cpu_limit: text("cpu_limit"),
        memory_limit: text("memory_limit"),
        runner_image: text("runner_image"),
        labels: strings(v.get("labels")),
        allow_pull_requests: v
            .get("allow_pull_requests")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        max_runners: int("max_runners"),
        idle_timeout: int("idle_timeout"),
        job_timeout: int("job_timeout"),
    }
}

pub async fn config_by_id(db: &Db, id: i64) -> DbResult<Option<RunnerConfig>> {
    let row: Option<String> = sqlx::query_scalar(
        "SELECT row_to_json(c)::text FROM github_runner_configs c WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(db.pool())
    .await?;
    Ok(row
        .and_then(|r| serde_json::from_str(&r).ok())
        .map(|v| config_of(&v)))
}

/// `$githubApp->runnerConfigs()->where('is_enabled', true)`, by id.
pub async fn enabled_configs(db: &Db, github_app_id: i64) -> DbResult<Vec<RunnerConfig>> {
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT row_to_json(c)::text FROM github_runner_configs c WHERE github_app_id = $1 AND is_enabled = true ORDER BY id",
    )
    .bind(github_app_id)
    .fetch_all(db.pool())
    .await?;
    Ok(parse(rows).iter().map(config_of).collect())
}

/// `GithubRunnerStatus::occupying()`.
const OCCUPYING: &str = "('provisioning', 'idle', 'running')";

/// `occupiedRunnerCount`.
pub async fn occupied_count(db: &Db, config_id: i64) -> DbResult<i64> {
    Ok(sqlx::query_scalar(&format!(
        "SELECT count(*) FROM github_runner_executions WHERE github_runner_config_id = $1 AND status IN {OCCUPYING}"
    ))
    .bind(config_id)
    .fetch_one(db.pool())
    .await?)
}

/// A `github_runner_executions` row.
#[derive(Debug, Clone)]
pub struct Execution {
    pub id: i64,
    pub uuid: String,
    pub github_app_id: i64,
    pub github_runner_config_id: Option<i64>,
    pub server_id: Option<i64>,
    pub status: String,
    pub trigger_workflow_job_id: Option<i64>,
    pub runner_id: Option<i64>,
    pub labels: Vec<String>,
    pub is_pull_request: bool,
    pub provision_attempts: i64,
    pub ready_at: Option<NaiveDateTime>,
    pub started_at: Option<NaiveDateTime>,
    pub updated_at: NaiveDateTime,
}

fn time_of(v: &Value, key: &str) -> Option<NaiveDateTime> {
    let t = v.get(key)?.as_str()?;
    NaiveDateTime::parse_from_str(t, "%Y-%m-%dT%H:%M:%S").ok()
}

fn execution_of(v: &Value) -> Execution {
    let int = |k: &str| v.get(k).and_then(Value::as_i64);
    Execution {
        id: int("id").unwrap_or(0),
        uuid: v
            .get("uuid")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        github_app_id: int("github_app_id").unwrap_or(0),
        github_runner_config_id: int("github_runner_config_id"),
        server_id: int("server_id"),
        status: v
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        trigger_workflow_job_id: int("trigger_workflow_job_id"),
        runner_id: int("runner_id"),
        labels: strings(v.get("labels")),
        is_pull_request: v
            .get("is_pull_request")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        provision_attempts: int("provision_attempts").unwrap_or(0),
        ready_at: time_of(v, "ready_at"),
        started_at: time_of(v, "started_at"),
        updated_at: time_of(v, "updated_at").unwrap_or_default(),
    }
}

pub async fn execution_by_id(db: &Db, id: i64) -> DbResult<Option<Execution>> {
    let row: Option<String> = sqlx::query_scalar(
        "SELECT row_to_json(e)::text FROM github_runner_executions e WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(db.pool())
    .await?;
    Ok(row
        .and_then(|r| serde_json::from_str(&r).ok())
        .map(|v| execution_of(&v)))
}

/// The execution of a job of an App (`trigger_workflow_job_id`).
pub async fn by_trigger(db: &Db, app_id: i64, job_id: i64) -> DbResult<Option<Execution>> {
    let row: Option<String> = sqlx::query_scalar(
        "SELECT row_to_json(e)::text FROM github_runner_executions e WHERE github_app_id = $1 AND trigger_workflow_job_id = $2",
    )
    .bind(app_id)
    .bind(job_id)
    .fetch_optional(db.pool())
    .await?;
    Ok(row
        .and_then(|r| serde_json::from_str(&r).ok())
        .map(|v| execution_of(&v)))
}

/// The execution of a runner of an App.
pub async fn by_runner_name(db: &Db, app_id: i64, name: &str) -> DbResult<Option<Execution>> {
    let row: Option<String> = sqlx::query_scalar(
        "SELECT row_to_json(e)::text FROM github_runner_executions e WHERE github_app_id = $1 AND runner_name = $2 ORDER BY id LIMIT 1",
    )
    .bind(app_id)
    .bind(name)
    .fetch_optional(db.pool())
    .await?;
    Ok(row
        .and_then(|r| serde_json::from_str(&r).ok())
        .map(|v| execution_of(&v)))
}

/// The columns `array_filter`'s `$jobDetails` keeps.
#[derive(Debug, Clone, Default)]
pub struct JobDetails {
    pub workflow_job_id: i64,
    pub workflow_job_html_url: Option<String>,
    pub workflow_name: Option<String>,
    pub job_name: Option<String>,
    pub repository_full_name: Option<String>,
}

/// `createOrFirst`: `(id, was created)`.
pub async fn create_or_first(
    db: &Db,
    app_id: i64,
    job_id: i64,
    d: &JobDetails,
    labels: &[String],
    is_pull_request: bool,
    now: NaiveDateTime,
) -> DbResult<(i64, bool)> {
    db.ensure_writable()?;
    if let Some(e) = by_trigger(db, app_id, job_id).await? {
        return Ok((e.id, false));
    }
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO github_runner_executions (uuid, github_app_id, trigger_workflow_job_id, workflow_job_id, workflow_job_html_url, workflow_name, job_name, repository_full_name, labels, is_pull_request, status, queued_at, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9::json, $10, 'queued', $11, $11, $11) RETURNING id",
    )
    .bind(coolify_domain::new_public_id())
    .bind(app_id)
    .bind(job_id)
    .bind(d.workflow_job_id)
    .bind(&d.workflow_job_html_url)
    .bind(&d.workflow_name)
    .bind(&d.job_name)
    .bind(&d.repository_full_name)
    .bind(serde_json::to_string(labels).unwrap_or_else(|_| "[]".into()))
    .bind(is_pull_request)
    .bind(now)
    .fetch_one(db.pool())
    .await?;
    Ok((id, true))
}

/// `reserveServer`: moves a queued execution to provisioning. `false` when another job took it.
pub async fn reserve_execution(
    db: &Db,
    id: i64,
    config_id: i64,
    server_id: i64,
    name: &str,
    now: NaiveDateTime,
) -> DbResult<bool> {
    db.ensure_writable()?;
    let r = sqlx::query(
        "UPDATE github_runner_executions SET github_runner_config_id = $2, server_id = $3, status = 'provisioning', runner_name = $4, provision_attempts = provision_attempts + 1, updated_at = $5 WHERE id = $1 AND status = 'queued'",
    )
    .bind(id)
    .bind(config_id)
    .bind(server_id)
    .bind(name)
    .bind(now)
    .execute(db.pool())
    .await?;
    Ok(r.rows_affected() > 0)
}

pub async fn set_runner_id(db: &Db, id: i64, runner_id: i64, now: NaiveDateTime) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(
        "UPDATE github_runner_executions SET runner_id = $2, updated_at = $3 WHERE id = $1",
    )
    .bind(id)
    .bind(runner_id)
    .bind(now)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// The end of `ProvisionGithubRunnerJob::handle`.
pub async fn mark_ready(db: &Db, id: i64, now: NaiveDateTime) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(&format!(
        "UPDATE github_runner_executions SET ready_at = $2, updated_at = $2 WHERE id = $1 AND status IN {OCCUPYING}"
    ))
    .bind(id)
    .bind(now)
    .execute(db.pool())
    .await?;
    sqlx::query("UPDATE github_runner_executions SET status = 'idle', updated_at = $2 WHERE id = $1 AND status = 'provisioning'")
        .bind(id)
        .bind(now)
        .execute(db.pool())
        .await?;
    Ok(())
}

/// The last failed attempt.
pub async fn fail_provisioning(db: &Db, id: i64, error: &str, now: NaiveDateTime) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query("UPDATE github_runner_executions SET status = 'failed', error_message = $2, completed_at = $3, updated_at = $3 WHERE id = $1 AND status = 'provisioning'")
        .bind(id)
        .bind(error)
        .bind(now)
        .execute(db.pool())
        .await?;
    Ok(())
}

/// A failed attempt with attempts left: `false` when the execution changed meanwhile.
pub async fn requeue_provisioning(
    db: &Db,
    id: i64,
    error: &str,
    now: NaiveDateTime,
) -> DbResult<bool> {
    db.ensure_writable()?;
    let r = sqlx::query(
        "UPDATE github_runner_executions SET status = 'queued', github_runner_config_id = NULL, server_id = NULL, runner_name = NULL, runner_id = NULL, ready_at = NULL, error_message = $2, updated_at = $3 WHERE id = $1 AND status = 'provisioning'",
    )
    .bind(id)
    .bind(error)
    .bind(now)
    .execute(db.pool())
    .await?;
    Ok(r.rows_affected() > 0)
}

/// `$githubApp->update(['runner_group_id' => ...])`.
pub async fn set_runner_group(
    db: &Db,
    app_id: i64,
    group: i64,
    now: NaiveDateTime,
) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query("UPDATE github_apps SET runner_group_id = $2, updated_at = $3 WHERE id = $1")
        .bind(app_id)
        .bind(group)
        .bind(now)
        .execute(db.pool())
        .await?;
    Ok(())
}

/// `dispatchQueued`.
pub async fn queued_ids(db: &Db, app_id: i64, limit: i64) -> DbResult<Vec<i64>> {
    Ok(sqlx::query_scalar(
        "SELECT id FROM github_runner_executions WHERE github_app_id = $1 AND status = 'queued' ORDER BY id LIMIT $2",
    )
    .bind(app_id)
    .bind(limit)
    .fetch_all(db.pool())
    .await?)
}

/// The servers `ReconcileGithubRunnersJob` looks at (`serversToReconcile()`): the server ids come
/// from one UNION, since an OR of two EXISTS subqueries makes Postgres plan a per-server scan.
pub async fn reconcile_server_ids(db: &Db) -> DbResult<Vec<i64>> {
    Ok(sqlx::query_scalar(&format!(
        "SELECT s.id FROM servers s WHERE s.id IN (SELECT server_id FROM github_runner_configs WHERE is_enabled = true \
         UNION SELECT server_id FROM github_runner_executions WHERE status IN {OCCUPYING}) ORDER BY s.id"
    ))
    .fetch_all(db.pool())
    .await?)
}

/// `removeRunnersOfDeletedServers`, the runners to deregister: `(github_app_id, runner_id)`.
pub async fn occupying_without_server(db: &Db) -> DbResult<Vec<(i64, Option<i64>)>> {
    Ok(sqlx::query_as(&format!(
        "SELECT github_app_id, runner_id FROM github_runner_executions WHERE server_id IS NULL AND status IN {OCCUPYING}"
    ))
    .fetch_all(db.pool())
    .await?)
}

pub async fn delete_occupying_without_server(db: &Db) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(&format!(
        "DELETE FROM github_runner_executions WHERE server_id IS NULL AND status IN {OCCUPYING}"
    ))
    .execute(db.pool())
    .await?;
    Ok(())
}

/// `reconcileQueued`: per App, `(id, queued_at ?? created_at)`.
pub async fn queued_by_app(db: &Db) -> DbResult<Vec<(i64, Vec<(i64, NaiveDateTime)>)>> {
    let rows: Vec<(i64, i64, NaiveDateTime)> = sqlx::query_as(
        "SELECT github_app_id, id, COALESCE(queued_at, created_at) FROM github_runner_executions WHERE status = 'queued' ORDER BY github_app_id, id",
    )
    .fetch_all(db.pool())
    .await?;
    let mut out: Vec<(i64, Vec<(i64, NaiveDateTime)>)> = Vec::new();
    for (app, id, at) in rows {
        match out.last_mut() {
            Some((a, list)) if *a == app => list.push((id, at)),
            _ => out.push((app, vec![(id, at)])),
        }
    }
    Ok(out)
}

/// `->max('capacity_wait_timeout') ?? 0`.
pub async fn max_capacity_wait(db: &Db, app_id: i64) -> DbResult<i64> {
    let v: Option<i64> = sqlx::query_scalar(
        "SELECT max(capacity_wait_timeout)::bigint FROM github_runner_configs WHERE github_app_id = $1 AND is_enabled = true",
    )
    .bind(app_id)
    .fetch_one(db.pool())
    .await?;
    Ok(v.unwrap_or(0))
}

pub async fn occupying_on_server(db: &Db, server_id: i64) -> DbResult<Vec<Execution>> {
    let rows: Vec<String> = sqlx::query_scalar(&format!(
        "SELECT row_to_json(e)::text FROM github_runner_executions e WHERE server_id = $1 AND status IN {OCCUPYING} ORDER BY id"
    ))
    .bind(server_id)
    .fetch_all(db.pool())
    .await?;
    Ok(parse(rows).iter().map(execution_of).collect())
}

/// `assignJob`: running with the job details; `trigger` takes the job over from another execution.
pub async fn mark_running(
    db: &Db,
    id: i64,
    job_id: i64,
    trigger: Option<i64>,
    d: &JobDetails,
    now: NaiveDateTime,
) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(
        "UPDATE github_runner_executions SET status = 'running', started_at = $3, workflow_job_id = $2, trigger_workflow_job_id = COALESCE($4, trigger_workflow_job_id), \
         workflow_job_html_url = COALESCE($5, workflow_job_html_url), workflow_name = COALESCE($6, workflow_name), job_name = COALESCE($7, job_name), repository_full_name = COALESCE($8, repository_full_name), updated_at = $3 WHERE id = $1",
    )
    .bind(id)
    .bind(job_id)
    .bind(now)
    .bind(trigger)
    .bind(&d.workflow_job_html_url)
    .bind(&d.workflow_name)
    .bind(&d.job_name)
    .bind(&d.repository_full_name)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// `$execution->update([...$jobDetails, 'conclusion' => ...])`.
pub async fn set_conclusion(
    db: &Db,
    id: i64,
    d: &JobDetails,
    conclusion: Option<&str>,
    now: NaiveDateTime,
) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(
        "UPDATE github_runner_executions SET workflow_job_id = $2, workflow_job_html_url = COALESCE($3, workflow_job_html_url), workflow_name = COALESCE($4, workflow_name), job_name = COALESCE($5, job_name), repository_full_name = COALESCE($6, repository_full_name), conclusion = $7, updated_at = $8 WHERE id = $1",
    )
    .bind(id)
    .bind(d.workflow_job_id)
    .bind(&d.workflow_job_html_url)
    .bind(&d.workflow_name)
    .bind(&d.job_name)
    .bind(&d.repository_full_name)
    .bind(conclusion)
    .bind(now)
    .execute(db.pool())
    .await?;
    Ok(())
}

pub async fn delete_execution(db: &Db, id: i64) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query("DELETE FROM github_runner_executions WHERE id = $1")
        .bind(id)
        .execute(db.pool())
        .await?;
    Ok(())
}

/// `assignJob`: the queued execution for the original job. The old row is read from `from`.
pub async fn create_replacement(db: &Db, from: &Execution, now: NaiveDateTime) -> DbResult<i64> {
    db.ensure_writable()?;
    Ok(sqlx::query_scalar(
        "INSERT INTO github_runner_executions (uuid, github_app_id, trigger_workflow_job_id, workflow_job_id, labels, is_pull_request, provision_attempts, status, queued_at, created_at, updated_at) \
         VALUES ($1, $2, $3, $3, $4::json, $5, $6, 'queued', $7, $7, $7) RETURNING id",
    )
    .bind(coolify_domain::new_public_id())
    .bind(from.github_app_id)
    .bind(from.trigger_workflow_job_id)
    .bind(serde_json::to_string(&from.labels).unwrap_or_else(|_| "[]".into()))
    .bind(from.is_pull_request)
    .bind(from.provision_attempts as i16)
    .bind(now)
    .fetch_one(db.pool())
    .await?)
}

/// `finish($status)` without a message: `error_message` becomes null.
pub async fn clear_error(db: &Db, id: i64) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query("UPDATE github_runner_executions SET error_message = NULL WHERE id = $1")
        .bind(id)
        .execute(db.pool())
        .await?;
    Ok(())
}
