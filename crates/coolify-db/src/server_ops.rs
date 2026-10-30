//! Rows that server jobs and the server sub-resources read and write:
//! the `proxy` attribute, `docker_cleanup_executions`, `ssl_certificates` and
//! server shared variables.

use chrono::NaiveDateTime;
use serde_json::Value;

use crate::records::{Record, laravel_datetime_from, parse_row};
use crate::{Db, DbError, DbResult};

/// `$server->save()` after changing the schemaless `proxy` attribute.
pub async fn save_proxy(
    db: &Db,
    server_id: i64,
    proxy: Option<&str>,
    now: NaiveDateTime,
) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query("UPDATE servers SET proxy = $1::json, updated_at = $2 WHERE id = $3")
        .bind(proxy)
        .bind(now)
        .bind(server_id)
        .execute(db.pool())
        .await?;
    Ok(())
}

/// `$server->save()` for a JSON column (`traefik_outdated_info`, `server_metadata`).
pub async fn save_json_column(
    db: &Db,
    server_id: i64,
    column: &'static str,
    value: Option<&str>,
    now: NaiveDateTime,
) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(&format!(
        "UPDATE servers SET \"{column}\" = $1::json, updated_at = $2 WHERE id = $3"
    ))
    .bind(value)
    .bind(now)
    .bind(server_id)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// `$settings->save()` for a JSON column (`patch_check_result`).
pub async fn save_settings_json(
    db: &Db,
    server_id: i64,
    column: &'static str,
    value: Option<&str>,
    now: NaiveDateTime,
) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(&format!(
        "UPDATE server_settings SET \"{column}\" = $1::json, updated_at = $2 WHERE server_id = $3"
    ))
    .bind(value)
    .bind(now)
    .bind(server_id)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// A `remote_process` activity with the server as subject.
pub async fn insert_server_activity(
    db: &Db,
    server_id: i64,
    properties: &Value,
    now: NaiveDateTime,
) -> DbResult<i64> {
    db.ensure_writable()?;
    Ok(sqlx::query_scalar(
        "INSERT INTO activity_log (log_name, description, subject_type, subject_id, properties, event, created_at, updated_at) \
         VALUES ('default', '[]', 'App\\Models\\Server', $1, $2::jsonb, 'inline', $3, $3) RETURNING id",
    )
    .bind(server_id)
    .bind(properties.to_string())
    .bind(now)
    .fetch_one(db.pool())
    .await?)
}

/// `$server->save()` for `sentinel_updated_at`.
pub async fn save_sentinel_updated_at(
    db: &Db,
    server_id: i64,
    updated_at: NaiveDateTime,
    now: NaiveDateTime,
) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query("UPDATE servers SET sentinel_updated_at = $1, updated_at = $2 WHERE id = $3")
        .bind(updated_at)
        .bind(now)
        .bind(server_id)
        .execute(db.pool())
        .await?;
    Ok(())
}

/// `$server->save()` for `sentinel_waiting_since`.
pub async fn save_sentinel_waiting(
    db: &Db,
    server_id: i64,
    waiting_since: Option<NaiveDateTime>,
    now: NaiveDateTime,
) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query("UPDATE servers SET sentinel_waiting_since = $1, updated_at = $2 WHERE id = $3")
        .bind(waiting_since)
        .bind(now)
        .bind(server_id)
        .execute(db.pool())
        .await?;
    Ok(())
}

/// The sentinel heartbeat of a push (`sentinel_waiting_since = null` plus `sentinelHeartbeat()`).
pub async fn sentinel_heartbeat(db: &Db, server_id: i64, now: NaiveDateTime) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(
        "UPDATE servers SET sentinel_updated_at = $1, sentinel_waiting_since = NULL, updated_at = $1 WHERE id = $2",
    )
    .bind(now)
    .bind(server_id)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// `DockerCleanupExecution::create(['server_id' => ...])`.
pub async fn create_cleanup_execution(
    db: &Db,
    uuid: &str,
    server_id: i64,
    now: NaiveDateTime,
) -> DbResult<i64> {
    db.ensure_writable()?;
    Ok(sqlx::query_scalar(
        "INSERT INTO docker_cleanup_executions (server_id, uuid, created_at, updated_at) \
         VALUES ($1, $2, $3, $3) RETURNING id",
    )
    .bind(server_id)
    .bind(uuid)
    .bind(now)
    .fetch_one(db.pool())
    .await?)
}

/// What `$execution_log->update([...])` changes.
#[derive(Debug, Default)]
pub struct CleanupUpdate<'a> {
    pub status: Option<&'a str>,
    pub message: Option<&'a str>,
    pub cleanup_log: Option<&'a str>,
    pub finished_at: Option<NaiveDateTime>,
}

pub async fn update_cleanup_execution(
    db: &Db,
    id: i64,
    u: &CleanupUpdate<'_>,
    now: NaiveDateTime,
) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(
        "UPDATE docker_cleanup_executions SET \
           status = COALESCE($1, status), \
           message = COALESCE($2, message), \
           cleanup_log = COALESCE($3::json, cleanup_log), \
           finished_at = COALESCE($4, finished_at), \
           updated_at = $5 WHERE id = $6",
    )
    .bind(u.status)
    .bind(u.message)
    .bind(u.cleanup_log)
    .bind(u.finished_at)
    .bind(now)
    .bind(id)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// `$server->dockerCleanupExecutions()->orderBy('created_at', 'desc')->take(20)`, as the API maps them.
pub async fn cleanup_executions(db: &Db, server_id: i64) -> DbResult<Vec<Value>> {
    type Row = (
        String,
        String,
        Option<String>,
        Option<NaiveDateTime>,
        Option<NaiveDateTime>,
        Option<NaiveDateTime>,
    );
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT uuid, status, message, finished_at, created_at, updated_at FROM docker_cleanup_executions \
         WHERE server_id = $1 ORDER BY created_at DESC LIMIT 20",
    )
    .bind(server_id)
    .fetch_all(db.pool())
    .await?;
    let time = |t: Option<NaiveDateTime>| {
        t.map_or(Value::Null, |t| Value::String(laravel_datetime_from(t)))
    };
    // `finished_at` has no cast on the model: it is the raw column text.
    Ok(rows
        .into_iter()
        .map(|(uuid, status, message, finished, created, updated)| {
            let mut m = serde_json::Map::new();
            m.insert("uuid".into(), Value::String(uuid));
            m.insert("status".into(), Value::String(status));
            m.insert("message".into(), message.map_or(Value::Null, Value::String));
            m.insert(
                "finished_at".into(),
                finished.map_or(Value::Null, |t| {
                    Value::String(t.format("%Y-%m-%d %H:%M:%S").to_string())
                }),
            );
            m.insert("created_at".into(), time(created));
            m.insert("updated_at".into(), time(updated));
            Value::Object(m)
        })
        .collect())
}

/// `CleanupDocker::activeDeployment`: the deployment uuid.
pub async fn active_deployment(
    db: &Db,
    server_id: i64,
    timeout_seconds: i64,
    now: NaiveDateTime,
) -> DbResult<Option<String>> {
    Ok(sqlx::query_scalar(
        "SELECT deployment_uuid FROM application_deployment_queues \
         WHERE (server_id::bigint = $1 OR build_server_id::bigint = $1) AND status IN ('queued', 'in_progress') \
           AND updated_at >= $2 LIMIT 1",
    )
    .bind(server_id)
    .bind(now - chrono::Duration::seconds(timeout_seconds))
    .fetch_optional(db.pool())
    .await?)
}

/// Application ids with a queued or running deployment (`cleanupApplicationImages`).
pub async fn deploying_applications(
    db: &Db,
    timeout_seconds: i64,
    now: NaiveDateTime,
) -> DbResult<Vec<String>> {
    Ok(sqlx::query_scalar(
        "SELECT application_id::text FROM application_deployment_queues \
         WHERE status IN ('queued', 'in_progress') AND updated_at >= $1",
    )
    .bind(now - chrono::Duration::seconds(timeout_seconds))
    .fetch_all(db.pool())
    .await?)
}

/// `$server->sslCertificates()->where('is_ca_certificate', true)`: the certificate (encrypted casts on the model).
pub async fn ca_certificate(db: &Db, server_id: i64) -> DbResult<Option<(i64, String)>> {
    Ok(sqlx::query_as(
        "SELECT id, ssl_certificate FROM ssl_certificates \
         WHERE server_id = $1 AND is_ca_certificate = true LIMIT 1",
    )
    .bind(server_id)
    .fetch_optional(db.pool())
    .await?)
}

/// `SslCertificate::create` after deleting the previous certificate of the scope.
#[derive(Debug)]
pub struct NewCertificate<'a> {
    pub certificate: &'a str,
    pub private_key: &'a str,
    pub server_id: i64,
    pub common_name: &'a str,
    pub subject_alternative_names: &'a str,
    pub valid_until: NaiveDateTime,
    pub is_ca_certificate: bool,
}

pub async fn replace_server_certificate(
    db: &Db,
    n: &NewCertificate<'_>,
    now: NaiveDateTime,
) -> DbResult<i64> {
    db.ensure_writable()?;
    let mut tx = db.pool().begin().await?;
    sqlx::query(
        "DELETE FROM ssl_certificates WHERE resource_type IS NULL AND resource_id IS NULL AND server_id = $1",
    )
    .bind(n.server_id)
    .execute(&mut *tx)
    .await?;
    let id = sqlx::query_scalar(
        "INSERT INTO ssl_certificates (ssl_certificate, ssl_private_key, server_id, common_name, \
           subject_alternative_names, valid_until, is_ca_certificate, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5::json, $6, $7, $8, $8) RETURNING id",
    )
    .bind(n.certificate)
    .bind(n.private_key)
    .bind(n.server_id)
    .bind(n.common_name)
    .bind(n.subject_alternative_names)
    .bind(n.valid_until)
    .bind(n.is_ca_certificate)
    .bind(now)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(id)
}

fn decode_all(rows: &[String], what: &str) -> DbResult<Vec<Record>> {
    rows.iter()
        .map(|t| parse_row(t).map_err(|e| DbError::Schema(format!("decoding {what}: {e}"))))
        .collect()
}

/// `SharedEnvironmentVariable` rows of a server, ordered by id.
pub async fn server_envs(db: &Db, team_id: i64, server_id: i64) -> DbResult<Vec<Record>> {
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT row_to_json(e)::text FROM shared_environment_variables e \
         WHERE team_id = $1 AND type = 'server' AND server_id = $2 ORDER BY id",
    )
    .bind(team_id)
    .bind(server_id)
    .fetch_all(db.pool())
    .await?;
    decode_all(&rows, "shared variable")
}

pub async fn find_server_env(
    db: &Db,
    team_id: i64,
    server_id: i64,
    env_id: i64,
) -> DbResult<Option<Record>> {
    let row: Option<String> = sqlx::query_scalar(
        "SELECT row_to_json(e)::text FROM shared_environment_variables e \
         WHERE team_id = $1 AND type = 'server' AND id = $2 AND server_id = $3 LIMIT 1",
    )
    .bind(team_id)
    .bind(env_id)
    .bind(server_id)
    .fetch_optional(db.pool())
    .await?;
    row.as_deref()
        .map(|t| parse_row(t).map_err(|e| DbError::Schema(format!("decoding shared env: {e}"))))
        .transpose()
}

pub async fn server_env_key_exists(
    db: &Db,
    team_id: i64,
    server_id: i64,
    key: &str,
    except: Option<i64>,
) -> DbResult<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM shared_environment_variables WHERE team_id = $1 AND type = 'server' \
           AND key = $2 AND project_id IS NULL AND environment_id IS NULL AND server_id = $3 \
           AND ($4::bigint IS NULL OR id <> $4))",
    )
    .bind(team_id)
    .bind(key)
    .bind(server_id)
    .bind(except)
    .fetch_one(db.pool())
    .await?)
}

#[derive(Debug)]
pub struct NewServerEnv<'a> {
    pub team_id: i64,
    pub server_id: i64,
    pub key: &'a str,
    /// The encrypted value.
    pub value: &'a str,
    pub is_literal: bool,
    pub is_multiline: bool,
    pub is_shown_once: bool,
    pub comment: Option<&'a str>,
}

pub async fn create_server_env(db: &Db, n: &NewServerEnv<'_>, now: NaiveDateTime) -> DbResult<i64> {
    db.ensure_writable()?;
    Ok(sqlx::query_scalar(
        "INSERT INTO shared_environment_variables (key, value, is_literal, is_multiline, is_shown_once, comment, \
           type, team_id, server_id, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, 'server', $7, $8, $9, $9) RETURNING id",
    )
    .bind(n.key)
    .bind(n.value)
    .bind(n.is_literal)
    .bind(n.is_multiline)
    .bind(n.is_shown_once)
    .bind(n.comment)
    .bind(n.team_id)
    .bind(n.server_id)
    .bind(now)
    .fetch_one(db.pool())
    .await?)
}

pub async fn delete_shared_env(db: &Db, id: i64) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query("DELETE FROM shared_environment_variables WHERE id = $1")
        .bind(id)
        .execute(db.pool())
        .await?;
    Ok(())
}

/// `$env->save()`: the dirty columns and `updated_at`.
pub async fn update_shared_env(
    db: &Db,
    id: i64,
    values: &[(&'static str, crate::servers::ColumnValue)],
    now: NaiveDateTime,
) -> DbResult<()> {
    if values.is_empty() {
        return Ok(());
    }
    db.ensure_writable()?;
    let sql = format!(
        "UPDATE shared_environment_variables SET {}, updated_at = $1 WHERE id = $2",
        crate::servers::set_clause(values, 3)
    );
    crate::servers::bind_all(sqlx::query(&sql).bind(now).bind(id), values)
        .execute(db.pool())
        .await?;
    Ok(())
}

/// Instance settings the server jobs need.
#[derive(Debug, Clone, Default, sqlx::FromRow)]
pub struct InstanceServerSettings {
    pub fqdn: Option<String>,
    pub public_ipv4: Option<String>,
    pub public_ipv6: Option<String>,
    pub docker_registry_url: Option<String>,
    pub helper_version: Option<String>,
    pub is_dashboard_force_https_enabled: Option<bool>,
    pub instance_timezone: Option<String>,
}

pub async fn instance_server_settings(db: &Db) -> DbResult<InstanceServerSettings> {
    Ok(sqlx::query_as(
        "SELECT fqdn, public_ipv4, public_ipv6, docker_registry_url, helper_version, \
           is_dashboard_force_https_enabled, instance_timezone FROM instance_settings WHERE id = 0",
    )
    .fetch_optional(db.pool())
    .await?
    .unwrap_or_default())
}

/// `webhook_allowed_internal_hosts` and `webhook_allow_localhost` (`SafeWebhookUrl`).
pub async fn webhook_policy(db: &Db) -> DbResult<(Option<String>, bool)> {
    let r: Option<(Option<String>, Option<bool>)> = sqlx::query_as(
        "SELECT webhook_allowed_internal_hosts::text, webhook_allow_localhost FROM instance_settings WHERE id = 0",
    )
    .fetch_optional(db.pool())
    .await?;
    Ok(r.map(|(h, l)| (h, l.unwrap_or(false))).unwrap_or_default())
}

/// `$server->services()`.
pub async fn services_on_server(db: &Db, server_id: i64) -> DbResult<Vec<Record>> {
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT row_to_json(s)::text FROM services s WHERE s.server_id = $1 AND s.deleted_at IS NULL",
    )
    .bind(server_id)
    .fetch_all(db.pool())
    .await?;
    decode_all(&rows, "service")
}

pub async fn team_has_services(db: &Db, team_id: i64) -> DbResult<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM services s JOIN environments e ON e.id = s.environment_id \
           JOIN projects p ON p.id = e.project_id WHERE p.team_id = $1 AND s.deleted_at IS NULL)",
    )
    .bind(team_id)
    .fetch_one(db.pool())
    .await?)
}

/// `Application` columns the Docker cleanup and proxy jobs read: applications on a server.
pub async fn applications_for_cleanup(
    db: &Db,
    server_id: i64,
) -> DbResult<Vec<(i64, String, Option<String>, Option<i64>, Option<String>)>> {
    Ok(sqlx::query_as(
        "SELECT a.id, a.uuid, a.docker_registry_image_name, s.docker_images_to_keep::bigint, a.build_pack \
         FROM applications a LEFT JOIN application_settings s ON s.application_id = a.id \
         WHERE a.deleted_at IS NULL AND ( \
           (a.destination_type = 'App\\Models\\StandaloneDocker' AND a.destination_id IN (SELECT id FROM standalone_dockers WHERE server_id = $1)) \
           OR (a.destination_type = 'App\\Models\\SwarmDocker' AND a.destination_id IN (SELECT id FROM swarm_dockers WHERE server_id = $1)) \
           OR a.id IN (SELECT application_id FROM additional_destinations WHERE server_id = $1))",
    )
    .bind(server_id)
    .fetch_all(db.pool())
    .await?)
}

/// `environment.project_id`.
pub async fn environment_project(db: &Db, environment_id: i64) -> DbResult<Option<i64>> {
    Ok(
        sqlx::query_scalar("SELECT project_id FROM environments WHERE id = $1")
            .bind(environment_id)
            .fetch_optional(db.pool())
            .await?,
    )
}
