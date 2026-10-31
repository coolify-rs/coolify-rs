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
