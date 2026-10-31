//! `applications`, their `application_settings` and the deployment queue
//! (`application_deployment_queues`). Only standalone Docker destinations are
//! ported; the API answers "not ported" for the rest.

use chrono::NaiveDateTime;
use coolify_domain::{TeamId, new_public_id};
use serde_json::Value;

use crate::application_ops::SWARM_DOCKER_MORPH;
use crate::databases::STANDALONE_DOCKER_MORPH;
use crate::records::{ENVIRONMENT, PROJECT, Record, STANDALONE_DOCKER, TEAM, parse_row};
use crate::servers::{ColumnValue, bind_all, set_clause};
use crate::{Db, DbError, DbResult};

pub const MORPH: &str = r"App\Models\Application";

/// An application with the relations its presentation and its jobs read.
#[derive(Debug, Clone)]
pub struct ApplicationRow {
    pub id: i64,
    pub uuid: String,
    pub team_id: TeamId,
    /// Raw attributes (`row_to_json`).
    pub raw: Record,
    /// The `application_settings` row (raw).
    pub settings: Record,
    pub environment: Record,
    pub project: Record,
    pub team: Record,
    /// `standalone_dockers` row when the destination is a standalone Docker.
    pub destination: Option<Record>,
}

impl ApplicationRow {
    pub fn str(&self, key: &str) -> Option<&str> {
        self.raw.get(key).and_then(Value::as_str)
    }
    pub fn int(&self, key: &str) -> Option<i64> {
        self.raw.get(key).and_then(Value::as_i64)
    }
    pub fn bool(&self, key: &str) -> bool {
        self.raw.get(key).and_then(Value::as_bool).unwrap_or(false)
    }
    pub fn setting_bool(&self, key: &str) -> bool {
        self.settings
            .get(key)
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }
    pub fn setting_int(&self, key: &str) -> Option<i64> {
        self.settings.get(key).and_then(Value::as_i64)
    }
    pub fn setting_str(&self, key: &str) -> Option<&str> {
        self.settings.get(key).and_then(Value::as_str)
    }
    pub fn server_id(&self) -> Option<i64> {
        self.destination
            .as_ref()
            .and_then(|d| d.get("server_id"))
            .and_then(Value::as_i64)
    }
    pub fn network(&self) -> &str {
        self.destination
            .as_ref()
            .and_then(|d| d.get("network"))
            .and_then(Value::as_str)
            .unwrap_or("coolify")
    }
}

fn decode(text: &str, what: &str) -> DbResult<Record> {
    parse_row(text).map_err(|e| DbError::Schema(format!("decoding {what} row: {e}")))
}

type Joined = (
    String,
    Option<String>,
    String,
    String,
    String,
    Option<String>,
);

fn select(filter: &str) -> String {
    format!(
        "SELECT row_to_json(a)::text, (SELECT row_to_json(s)::text FROM application_settings s WHERE s.application_id = a.id ORDER BY s.id LIMIT 1), \
         row_to_json(e)::text, row_to_json(p)::text, row_to_json(t)::text, \
         COALESCE((SELECT row_to_json(sd)::text FROM standalone_dockers sd WHERE a.destination_type = '{STANDALONE_DOCKER_MORPH}' AND sd.id = a.destination_id), \
         (SELECT row_to_json(sw)::text FROM swarm_dockers sw WHERE a.destination_type = '{SWARM_DOCKER_MORPH}' AND sw.id = a.destination_id)) \
         FROM applications a JOIN environments e ON e.id = a.environment_id JOIN projects p ON p.id = e.project_id \
         JOIN teams t ON t.id = p.team_id WHERE a.deleted_at IS NULL AND {filter}"
    )
}

fn to_row(j: &Joined) -> DbResult<ApplicationRow> {
    let raw = decode(&j.0, "application")?;
    let settings =
        j.1.as_deref()
            .map(|s| decode(s, "application settings"))
            .transpose()?
            .unwrap_or_default();
    let environment = ENVIRONMENT.present(decode(&j.2, "environment")?);
    let project = PROJECT.present(decode(&j.3, "project")?);
    let team = TEAM.present(decode(&j.4, "team")?);
    let destination =
        j.5.as_deref()
            .map(|d| decode(d, "destination").map(|r| STANDALONE_DOCKER.present(r)))
            .transpose()?;
    Ok(ApplicationRow {
        id: raw.get("id").and_then(Value::as_i64).unwrap_or_default(),
        uuid: raw
            .get("uuid")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        team_id: project
            .get("team_id")
            .and_then(Value::as_i64)
            .unwrap_or_default(),
        raw,
        settings,
        environment,
        project,
        team,
        destination,
    })
}

/// `Application::ownedByCurrentTeamAPI($team)->where('uuid', ...)`.
pub async fn find_in_team(
    db: &Db,
    team_id: TeamId,
    uuid: &str,
) -> DbResult<Option<ApplicationRow>> {
    let r: Option<Joined> = sqlx::query_as(&select(
        "a.uuid = $1 AND p.team_id = $2 ORDER BY a.id LIMIT 1",
    ))
    .bind(uuid)
    .bind(team_id)
    .fetch_optional(db.pool())
    .await?;
    r.as_ref().map(to_row).transpose()
}

pub async fn find_by_id(db: &Db, id: i64) -> DbResult<Option<ApplicationRow>> {
    let r: Option<Joined> = sqlx::query_as(&select("a.id = $1"))
        .bind(id)
        .fetch_optional(db.pool())
        .await?;
    r.as_ref().map(to_row).transpose()
}

/// `Application::ownedByCurrentTeamAPI($team)->get()`: ordered by name.
pub async fn list_for_team(db: &Db, team_id: TeamId) -> DbResult<Vec<ApplicationRow>> {
    let rows: Vec<Joined> = sqlx::query_as(&select("p.team_id = $1 ORDER BY a.name"))
        .bind(team_id)
        .fetch_all(db.pool())
        .await?;
    rows.iter().map(to_row).collect()
}

/// `Environment::applications()` (table order).
pub async fn list_for_environment(db: &Db, environment_id: i64) -> DbResult<Vec<ApplicationRow>> {
    let rows: Vec<Joined> = sqlx::query_as(&select("a.environment_id = $1 ORDER BY a.id"))
        .bind(environment_id)
        .fetch_all(db.pool())
        .await?;
    rows.iter().map(to_row).collect()
}

/// `Server::applications()`: applications on the server's standalone Docker destinations.
pub async fn list_for_server(db: &Db, server_id: i64) -> DbResult<Vec<ApplicationRow>> {
    let rows: Vec<Joined> = sqlx::query_as(&select(&format!(
        "((a.destination_type = '{STANDALONE_DOCKER_MORPH}' AND a.destination_id IN \
         (SELECT id FROM standalone_dockers WHERE server_id = $1)) \
         OR (a.destination_type = '{SWARM_DOCKER_MORPH}' AND a.destination_id IN \
         (SELECT id FROM swarm_dockers WHERE server_id = $1)) \
         OR a.id IN (SELECT application_id FROM additional_destinations WHERE server_id = $1)) ORDER BY a.id"
    )))
    .bind(server_id)
    .fetch_all(db.pool())
    .await?;
    rows.iter().map(to_row).collect()
}

/// Whether the application has pieces coolify-rs cannot deploy yet (additional servers).
pub async fn unported_parts(db: &Db, id: i64) -> DbResult<Vec<&'static str>> {
    let mut parts = Vec::new();
    for (sql, label) in [(
        "SELECT EXISTS (SELECT 1 FROM additional_destinations WHERE application_id = $1)",
        "additional servers",
    )] {
        let found: bool = sqlx::query_scalar(sql)
            .bind(id)
            .fetch_one(db.pool())
            .await?;
        if found {
            parts.push(label);
        }
    }
    Ok(parts)
}

/// What `create_application` writes: the `applications` columns (in `fill()` order),
/// encrypted webhook secrets, and the `application_settings` overrides.
#[derive(Debug, Default)]
pub struct NewApplication {
    pub uuid: String,
    pub environment_id: i64,
    pub destination_id: i64,
    /// `destination_type`: the morph class of `destination_id`.
    pub destination_type: &'static str,
    pub columns: Vec<(&'static str, ColumnValue)>,
    pub settings: Vec<(&'static str, ColumnValue)>,
}

/// `$application->save()` + the `created` hook (settings row, `compose_parsing_version = '5'`).
pub async fn create(db: &Db, n: NewApplication, now: NaiveDateTime) -> DbResult<i64> {
    db.ensure_writable()?;
    let mut cols: Vec<(&'static str, ColumnValue)> =
        vec![("uuid", ColumnValue::Text(Some(n.uuid.clone())))];
    for (c, v) in n.columns {
        cols.retain(|(k, _)| *k != c);
        cols.push((c, v));
    }
    cols.retain(|(k, _)| *k != "compose_parsing_version");
    cols.push((
        "compose_parsing_version",
        ColumnValue::Text(Some("5".into())),
    ));
    let names: Vec<String> = cols.iter().map(|(c, _)| format!("\"{c}\"")).collect();
    let placeholders: Vec<String> = cols
        .iter()
        .enumerate()
        .map(|(i, (c, v))| {
            // A NULL is written as a literal: a text-typed NULL parameter does not fit other columns.
            if matches!(v, ColumnValue::Text(None)) {
                return "NULL".to_owned();
            }
            let cast = match v {
                ColumnValue::Int(_) => "::integer",
                ColumnValue::Text(_)
                    if matches!(*c, "noindex_domains" | "domain_port_overrides") =>
                {
                    "::json"
                }
                _ => "",
            };
            format!("${}{cast}", i + 5)
        })
        .collect();
    let sql = format!(
        "INSERT INTO applications ({}, environment_id, destination_id, destination_type, created_at, updated_at) \
         VALUES ({}, $1, $2, $3, $4, $4) RETURNING id",
        names.join(", "),
        placeholders.join(", ")
    );
    let mut tx = db.pool().begin().await?;
    let id: i64 = bind_all(
        sqlx::query(&sql)
            .bind(n.environment_id)
            .bind(n.destination_id)
            .bind(n.destination_type)
            .bind(now),
        &cols,
    )
    .fetch_one(&mut *tx)
    .await
    .map(|r| sqlx::Row::get(&r, 0))?;
    let mut settings = n.settings;
    settings.push(("application_id", ColumnValue::Int(id)));
    let names: Vec<String> = settings.iter().map(|(c, _)| format!("\"{c}\"")).collect();
    let placeholders: Vec<String> = (0..settings.len())
        .map(|i| {
            if matches!(settings[i].1, ColumnValue::Text(None)) {
                return "NULL".to_owned();
            }
            let cast = if matches!(settings[i].1, ColumnValue::Int(_)) {
                "::bigint"
            } else {
                ""
            };
            format!("${}{cast}", i + 2)
        })
        .collect();
    let sql = format!(
        "INSERT INTO application_settings ({}, created_at, updated_at) VALUES ({}, $1, $1)",
        names.join(", "),
        placeholders.join(", ")
    );
    bind_all(sqlx::query(&sql).bind(now), &settings)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(id)
}

/// `$application->update($values)` after the caller's dirty check: changed columns plus
/// `updated_at`, and `last_online_at` when `status` changes (the `saving` hook).
pub async fn update_if_changed(
    db: &Db,
    id: i64,
    values: &[(&'static str, ColumnValue)],
    now: NaiveDateTime,
) -> DbResult<bool> {
    if values.is_empty() {
        return Ok(false);
    }
    db.ensure_writable()?;
    let changed = values
        .iter()
        .enumerate()
        .map(|(i, (col, v))| match v {
            ColumnValue::Text(None) => format!("\"{col}\" IS NOT NULL"),
            ColumnValue::Int(_) => format!("\"{col}\" IS DISTINCT FROM ${}::integer", i + 3),
            _ if matches!(*col, "noindex_domains" | "domain_port_overrides") => {
                format!("\"{col}\"::text IS DISTINCT FROM ${}", i + 3)
            }
            _ => format!("\"{col}\" IS DISTINCT FROM ${}", i + 3),
        })
        .collect::<Vec<_>>()
        .join(" OR ");
    let status_touch = values.iter().position(|(c, _)| *c == "status").map_or(String::new(), |pos| {
        format!(", last_online_at = CASE WHEN status IS DISTINCT FROM ${} THEN $1 ELSE last_online_at END", pos + 3)
    });
    let sql = format!(
        "UPDATE applications SET {}{status_touch}, updated_at = $1 WHERE id = $2 AND ({changed})",
        set_clause(values, 3)
    );
    let done = bind_all(sqlx::query(&sql).bind(now).bind(id), values)
        .execute(db.pool())
        .await?;
    Ok(done.rows_affected() > 0)
}

/// `$application->settings->update($values)` after the dirty check.
pub async fn update_settings(
    db: &Db,
    application_id: i64,
    values: &[(&'static str, ColumnValue)],
    now: NaiveDateTime,
) -> DbResult<bool> {
    if values.is_empty() {
        return Ok(false);
    }
    db.ensure_writable()?;
    let changed = values
        .iter()
        .enumerate()
        .map(|(i, (col, v))| match v {
            ColumnValue::Text(None) => format!("\"{col}\" IS NOT NULL"),
            ColumnValue::Int(_) => format!("\"{col}\" IS DISTINCT FROM ${}::integer", i + 3),
            _ => format!("\"{col}\" IS DISTINCT FROM ${}", i + 3),
        })
        .collect::<Vec<_>>()
        .join(" OR ");
    let sql = format!(
        "UPDATE application_settings SET {}, updated_at = $1 WHERE application_id = $2 AND ({changed})",
        set_clause(values, 3)
    );
    let done = bind_all(sqlx::query(&sql).bind(now).bind(application_id), values)
        .execute(db.pool())
        .await?;
    Ok(done.rows_affected() > 0)
}

/// `$application->delete()` (soft delete).
pub async fn soft_delete(db: &Db, id: i64, now: NaiveDateTime) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query("UPDATE applications SET deleted_at = $1, updated_at = $1 WHERE id = $2")
        .bind(now)
        .bind(id)
        .execute(db.pool())
        .await?;
    Ok(())
}

/// `forceDelete()` with the model's `forceDeleting` hook: fqdn cleared, settings, storages,
/// variables, scheduled tasks, tags, previews (soft deleted) and all deployment queue rows.
pub async fn force_delete(db: &Db, id: i64, now: NaiveDateTime) -> DbResult<()> {
    db.ensure_writable()?;
    let mut tx = db.pool().begin().await?;
    // deleteLocalResource(): file storages go with the row.
    sqlx::query("DELETE FROM local_file_volumes WHERE resource_type = $1 AND resource_id = $2")
        .bind(MORPH)
        .bind(id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM application_settings WHERE application_id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE applications SET fqdn = NULL, updated_at = $2 WHERE id = $1")
        .bind(id)
        .bind(now)
        .execute(&mut *tx)
        .await?;
    for (table, col_type, col_id) in [
        ("local_persistent_volumes", "resource_type", "resource_id"),
        (
            "environment_variables",
            "resourceable_type",
            "resourceable_id",
        ),
        ("taggables", "taggable_type", "taggable_id"),
        (
            "secret_manager_links",
            "resourceable_type",
            "resourceable_id",
        ),
    ] {
        sqlx::query(&format!(
            "DELETE FROM {table} WHERE {col_type} = $1 AND {col_id} = $2"
        ))
        .bind(MORPH)
        .bind(id)
        .execute(&mut *tx)
        .await?;
    }
    sqlx::query("DELETE FROM scheduled_tasks WHERE application_id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE application_previews SET deleted_at = $2 WHERE application_id = $1 AND deleted_at IS NULL")
        .bind(id)
        .bind(now)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM application_deployment_queues WHERE application_id = $1")
        .bind(id.to_string())
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM applications WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// The application's persistent volumes: `(name, mount_path, host_path)` in table order.
pub async fn persistent_volumes(
    db: &Db,
    id: i64,
) -> DbResult<Vec<(String, String, Option<String>)>> {
    Ok(sqlx::query_as(
        "SELECT name, mount_path, host_path FROM local_persistent_volumes WHERE resource_type = $1 AND resource_id = $2",
    )
    .bind(MORPH)
    .bind(id)
    .fetch_all(db.pool())
    .await?)
}

/// `CleanupDocker::activeDeployment`: a queued or running deployment that is not stuck.
pub async fn server_has_active_deployment(db: &Db, server_id: i64) -> DbResult<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM application_deployment_queues q LEFT JOIN server_settings s ON s.server_id = $1 \
         WHERE (q.server_id = $1 OR q.build_server_id = $1) AND q.status IN ('queued', 'in_progress') \
         AND q.updated_at >= (now() AT TIME ZONE 'UTC') - make_interval(secs => COALESCE(s.dynamic_timeout, 3600)))",
    )
    .bind(server_id)
    .fetch_one(db.pool())
    .await?)
}

/// `$app->docker_registry_image_name ?? $app->uuid` of a server's applications.
pub async fn server_image_repositories(db: &Db, server_id: i64) -> DbResult<Vec<String>> {
    Ok(sqlx::query_scalar(&format!(
        "SELECT DISTINCT COALESCE(a.docker_registry_image_name, a.uuid) FROM applications a JOIN standalone_dockers sd ON sd.id = a.destination_id \
         WHERE a.destination_type = '{STANDALONE_DOCKER_MORPH}' AND a.deleted_at IS NULL AND sd.server_id = $1"
    ))
    .bind(server_id)
    .fetch_all(db.pool())
    .await?)
}

/// `is_preview_suffix_enabled` of the persistent volumes, keyed by name.
pub async fn preview_suffix_volumes(db: &Db, id: i64) -> DbResult<Vec<(String, bool)>> {
    Ok(sqlx::query_as(
        "SELECT name, COALESCE(is_preview_suffix_enabled, true) FROM local_persistent_volumes WHERE resource_type = $1 AND resource_id = $2",
    )
    .bind(MORPH)
    .bind(id)
    .fetch_all(db.pool())
    .await?)
}

/// Servers with at least one application on a standalone Docker destination.
pub async fn servers_with_applications(db: &Db) -> DbResult<Vec<i64>> {
    Ok(sqlx::query_scalar(&format!(
        "SELECT DISTINCT sd.server_id FROM applications a JOIN standalone_dockers sd ON sd.id = a.destination_id \
         WHERE a.destination_type = '{STANDALONE_DOCKER_MORPH}' AND a.deleted_at IS NULL"
    ))
    .fetch_all(db.pool())
    .await?)
}

/// A row of [`domain_rows_of_team`]: `(uuid, name, fqdn, build_pack, docker_compose_domains)`.
pub type DomainRow = (
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
);

/// The columns `checkIfDomainIsAlreadyUsedViaAPI` loads of the team's applications, by name.
pub async fn domain_rows_of_team(db: &Db, team_id: TeamId) -> DbResult<Vec<DomainRow>> {
    Ok(sqlx::query_as(
        "SELECT a.uuid, a.name, a.fqdn, a.build_pack, a.docker_compose_domains FROM applications a \
         JOIN environments e ON e.id = a.environment_id JOIN projects p ON p.id = e.project_id \
         WHERE p.team_id = $1 AND a.deleted_at IS NULL ORDER BY a.name",
    )
    .bind(team_id)
    .fetch_all(db.pool())
    .await?)
}

/// `ServiceApplication::ownedByCurrentTeamAPI`, by name: `(uuid, fqdn, service name)`.
pub async fn service_domain_rows_of_team(
    db: &Db,
    team_id: TeamId,
) -> DbResult<Vec<(String, Option<String>, String)>> {
    Ok(sqlx::query_as(
        "SELECT sa.uuid, sa.fqdn, s.name FROM service_applications sa JOIN services s ON s.id = sa.service_id \
         JOIN environments e ON e.id = s.environment_id JOIN projects p ON p.id = e.project_id \
         WHERE p.team_id = $1 AND sa.deleted_at IS NULL ORDER BY sa.name",
    )
    .bind(team_id)
    .fetch_all(db.pool())
    .await?)
}

/// `instanceSettings()->fqdn`.
pub async fn instance_fqdn(db: &Db) -> DbResult<Option<String>> {
    Ok(
        sqlx::query_scalar("SELECT fqdn FROM instance_settings WHERE id = 0")
            .fetch_optional(db.pool())
            .await?
            .flatten(),
    )
}

/// Whether a `morphTo` source exists (`data_get($application, 'source')`).
pub async fn source_exists(db: &Db, source_type: &str, source_id: i64) -> DbResult<bool> {
    let table = match source_type {
        r"App\Models\GithubApp" => "github_apps",
        r"App\Models\GitlabApp" => "gitlab_apps",
        r"App\Models\BitbucketApp" => "bitbucket_apps",
        r"App\Models\GiteaApp" => "gitea_apps",
        _ => return Ok(false),
    };
    Ok(sqlx::query_scalar(&format!(
        "SELECT EXISTS (SELECT 1 FROM {table} WHERE id = $1)"
    ))
    .bind(source_id)
    .fetch_one(db.pool())
    .await?)
}

/// The `saving` hook when the build pack leaves `dockercompose`: `SERVICE_FQDN_*` and
/// `SERVICE_URL_*` variables go, preview copies included.
pub async fn delete_service_url_variables(db: &Db, id: i64) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(
        "DELETE FROM environment_variables WHERE resourceable_type = $1 AND resourceable_id = $2 \
         AND (key LIKE 'SERVICE_FQDN_%' OR key LIKE 'SERVICE_URL_%')",
    )
    .bind(MORPH)
    .bind(id)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// The fqdns of the team's other applications (`checkIfDomainIsAlreadyUsedViaAPI`).
pub async fn fqdns_of_team(db: &Db, team_id: TeamId) -> DbResult<Vec<(String, String, String)>> {
    Ok(sqlx::query_as(
        "SELECT a.uuid, a.name, a.fqdn FROM applications a JOIN environments e ON e.id = a.environment_id \
         JOIN projects p ON p.id = e.project_id WHERE p.team_id = $1 AND a.deleted_at IS NULL AND a.fqdn IS NOT NULL ORDER BY a.id",
    )
    .bind(team_id)
    .fetch_all(db.pool())
    .await?)
}

/// `GithubApp::where('uuid', $uuid)->where(team_id = $teamId OR is_system_wide)->first()`
/// (raw attributes).
pub async fn github_app_for_team(db: &Db, team_id: TeamId, uuid: &str) -> DbResult<Option<Record>> {
    let row: Option<String> = sqlx::query_scalar(
        "SELECT row_to_json(g)::text FROM github_apps g WHERE g.uuid = $1 AND (g.team_id = $2 OR g.is_system_wide) LIMIT 1",
    )
    .bind(uuid)
    .bind(team_id)
    .fetch_optional(db.pool())
    .await?;
    row.map(|t| parse_row(&t).map_err(|e| DbError::Schema(format!("decoding github app: {e}"))))
        .transpose()
}

// ---------------------------------------------------------------------------
// Deployment queue

/// A row of `application_deployment_queues`.
#[derive(Debug, Clone)]
pub struct Deployment {
    pub id: i64,
    pub uuid: String,
    pub application_id: i64,
    pub status: String,
    pub raw: Record,
}

impl Deployment {
    pub fn str(&self, key: &str) -> Option<&str> {
        self.raw.get(key).and_then(Value::as_str)
    }
    pub fn bool(&self, key: &str) -> bool {
        self.raw.get(key).and_then(Value::as_bool).unwrap_or(false)
    }
    pub fn int(&self, key: &str) -> Option<i64> {
        self.raw.get(key).and_then(Value::as_i64)
    }
}

fn deployment(text: &str) -> DbResult<Deployment> {
    let raw = decode(text, "application deployment queue")?;
    Ok(Deployment {
        id: raw.get("id").and_then(Value::as_i64).unwrap_or_default(),
        uuid: raw
            .get("deployment_uuid")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        application_id: raw
            .get("application_id")
            .and_then(Value::as_str)
            .and_then(|s| s.parse().ok())
            .unwrap_or_default(),
        status: raw
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        raw,
    })
}

/// A new queue row (`ApplicationDeploymentQueue::create([...])`).
#[derive(Debug, Clone)]
pub struct NewDeployment {
    pub application_id: i64,
    pub application_name: String,
    pub server_id: i64,
    pub server_name: String,
    pub destination_id: i64,
    pub deployment_uuid: String,
    pub deployment_url: String,
    pub pull_request_id: i64,
    pub force_rebuild: bool,
    pub commit: String,
    pub status: String,
    pub is_webhook: bool,
    pub is_api: bool,
    pub restart_only: bool,
    pub rollback: bool,
    pub only_this_server: bool,
    pub git_type: Option<String>,
    pub commit_message: Option<String>,
    pub docker_registry_image_tag: Option<String>,
    pub build_server_id: Option<i64>,
    pub parent_deployment_uuid: Option<String>,
}

/// `$server->destinations()->where('id', $id)->first()->network`.
pub async fn destination_network(
    db: &Db,
    server_id: i64,
    destination_id: i64,
) -> DbResult<Option<String>> {
    Ok(sqlx::query_scalar(
        "SELECT network FROM standalone_dockers WHERE server_id = $1 AND id = $2 \
         UNION ALL SELECT network FROM swarm_dockers WHERE server_id = $1 AND id = $2 LIMIT 1",
    )
    .bind(server_id)
    .bind(destination_id)
    .fetch_optional(db.pool())
    .await?)
}

/// `additional_servers()->updateExistingPivot($server->id, ['status' => $status])`.
pub async fn set_additional_status(
    db: &Db,
    application_id: i64,
    server_id: i64,
    status: &str,
    now: NaiveDateTime,
) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(
        "UPDATE additional_destinations SET status = $3, updated_at = $4 WHERE application_id = $1 AND server_id = $2 AND status IS DISTINCT FROM $3",
    )
    .bind(application_id)
    .bind(server_id)
    .bind(status)
    .bind(now)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// `$this->additional_servers->pluck('pivot.status')`.
pub async fn additional_statuses(db: &Db, application_id: i64) -> DbResult<Vec<String>> {
    Ok(sqlx::query_scalar(
        "SELECT status FROM additional_destinations WHERE application_id = $1 ORDER BY id",
    )
    .bind(application_id)
    .fetch_all(db.pool())
    .await?)
}

/// `Server::buildServers($teamId)`: id of each usable build server of the team.
pub async fn build_server_ids(db: &Db, team_id: i64, except: i64) -> DbResult<Vec<i64>> {
    Ok(sqlx::query_scalar(
        "SELECT s.id FROM servers s JOIN server_settings st ON st.server_id = s.id \
         WHERE s.team_id = $1 AND s.id <> $2 AND s.deleted_at IS NULL AND st.is_reachable AND st.is_usable \
         AND NOT st.is_swarm_worker AND NOT st.force_disabled \
         AND COALESCE(st.server_role, CASE WHEN st.is_build_server THEN 'build' ELSE 'all' END) = 'build' \
         AND NOT EXISTS (SELECT 1 FROM github_runner_configs g WHERE g.server_id = s.id AND g.is_enabled)",
    )
    .bind(team_id)
    .bind(except)
    .fetch_all(db.pool())
    .await?)
}

/// `$team->is_build_server_fallback_enabled`.
pub async fn build_server_fallback_enabled(db: &Db, team_id: i64) -> DbResult<bool> {
    Ok(sqlx::query_scalar::<_, Option<bool>>(
        "SELECT is_build_server_fallback_enabled FROM teams WHERE id = $1",
    )
    .bind(team_id)
    .fetch_optional(db.pool())
    .await?
    .flatten()
    .unwrap_or(false))
}

/// `parent_deployment_uuid` children: `(deployment_uuid, status)`.
pub async fn child_deployments(db: &Db, parent: &str) -> DbResult<Vec<(String, String)>> {
    Ok(sqlx::query_as(
        "SELECT deployment_uuid, status FROM application_deployment_queues WHERE parent_deployment_uuid = $1 ORDER BY id",
    )
    .bind(parent)
    .fetch_all(db.pool())
    .await?)
}

/// `ApplicationDeploymentQueue::create(...)`; `id` of the new row.
pub async fn insert_deployment(db: &Db, n: &NewDeployment, now: NaiveDateTime) -> DbResult<i64> {
    db.ensure_writable()?;
    Ok(sqlx::query_scalar(
        "INSERT INTO application_deployment_queues (application_id, deployment_uuid, pull_request_id, force_rebuild, commit, status, \
         is_webhook, created_at, updated_at, restart_only, git_type, server_id, application_name, server_name, deployment_url, \
         destination_id, only_this_server, rollback, commit_message, is_api, build_server_id, docker_registry_image_tag, parent_deployment_uuid) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20, $21, $22) RETURNING id",
    )
    .bind(n.application_id.to_string())
    .bind(&n.deployment_uuid)
    .bind(n.pull_request_id as i32)
    .bind(n.force_rebuild)
    .bind(&n.commit)
    .bind(&n.status)
    .bind(n.is_webhook)
    .bind(now)
    .bind(n.restart_only)
    .bind(&n.git_type)
    .bind(n.server_id as i32)
    .bind(&n.application_name)
    .bind(&n.server_name)
    .bind(&n.deployment_url)
    .bind(n.destination_id.to_string())
    .bind(n.only_this_server)
    .bind(n.rollback)
    .bind(&n.commit_message)
    .bind(n.is_api)
    .bind(n.build_server_id.map(|v| v as i32))
    .bind(&n.docker_registry_image_tag)
    .bind(&n.parent_deployment_uuid)
    .fetch_one(db.pool())
    .await?)
}

pub async fn deployment_by_id(db: &Db, id: i64) -> DbResult<Option<Deployment>> {
    let t: Option<String> = sqlx::query_scalar(
        "SELECT row_to_json(q)::text FROM application_deployment_queues q WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(db.pool())
    .await?;
    t.as_deref().map(deployment).transpose()
}

pub async fn deployment_by_uuid(db: &Db, uuid: &str) -> DbResult<Option<Deployment>> {
    let t: Option<String> = sqlx::query_scalar(
        "SELECT row_to_json(q)::text FROM application_deployment_queues q WHERE deployment_uuid = $1 ORDER BY id LIMIT 1",
    )
    .bind(uuid)
    .fetch_optional(db.pool())
    .await?;
    t.as_deref().map(deployment).transpose()
}

/// Deployments of an application, newest first.
pub async fn deployments_for_application(
    db: &Db,
    application_id: i64,
    skip: i64,
    take: i64,
) -> DbResult<(Vec<Deployment>, i64)> {
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT row_to_json(q)::text FROM application_deployment_queues q WHERE application_id = $1 \
         ORDER BY created_at DESC, id DESC OFFSET $2 LIMIT $3",
    )
    .bind(application_id.to_string())
    .bind(skip)
    .bind(take)
    .fetch_all(db.pool())
    .await?;
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM application_deployment_queues WHERE application_id = $1",
    )
    .bind(application_id.to_string())
    .fetch_one(db.pool())
    .await?;
    Ok((
        rows.iter()
            .map(|t| deployment(t))
            .collect::<DbResult<_>>()?,
        count,
    ))
}

/// Queued and in-progress deployments of the team's servers.
pub async fn running_for_servers(db: &Db, server_ids: &[i64]) -> DbResult<Vec<Deployment>> {
    let ids: Vec<i32> = server_ids.iter().map(|v| *v as i32).collect();
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT row_to_json(q)::text FROM application_deployment_queues q WHERE server_id = ANY($1) \
         AND status IN ('in_progress', 'queued') ORDER BY id",
    )
    .bind(&ids)
    .fetch_all(db.pool())
    .await?;
    rows.iter().map(|t| deployment(t)).collect()
}

/// Rows matching `status IN (...)` for an application (optionally only one pull request).
pub async fn deployments_with_status(
    db: &Db,
    application_id: i64,
    statuses: &[&str],
) -> DbResult<Vec<Deployment>> {
    let st: Vec<String> = statuses.iter().map(|s| (*s).to_owned()).collect();
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT row_to_json(q)::text FROM application_deployment_queues q WHERE application_id = $1 AND status = ANY($2) ORDER BY id",
    )
    .bind(application_id.to_string())
    .bind(&st)
    .fetch_all(db.pool())
    .await?;
    rows.iter().map(|t| deployment(t)).collect()
}

/// Count of `in_progress` deployments on a server (`concurrent_builds` check).
pub async fn in_progress_on_server(db: &Db, server_id: i64) -> DbResult<i64> {
    Ok(sqlx::query_scalar(
        "SELECT count(*) FROM application_deployment_queues WHERE server_id = $1 AND status = 'in_progress'",
    )
    .bind(server_id as i32)
    .fetch_one(db.pool())
    .await?)
}

/// Oldest `queued` deployments of a server.
pub async fn queued_on_server(db: &Db, server_id: i64) -> DbResult<Vec<Deployment>> {
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT row_to_json(q)::text FROM application_deployment_queues q WHERE server_id = $1 AND status = 'queued' ORDER BY created_at, id",
    )
    .bind(server_id as i32)
    .fetch_all(db.pool())
    .await?;
    rows.iter().map(|t| deployment(t)).collect()
}

/// The newest `finished` deployment of an application for a pull request
/// (`get_last_successful_deployment()` with pull request 0).
pub async fn last_finished_deployment(
    db: &Db,
    application_id: i64,
    pull_request_id: i64,
) -> DbResult<Option<Deployment>> {
    let t: Option<String> = sqlx::query_scalar(
        "SELECT row_to_json(q)::text FROM application_deployment_queues q WHERE application_id = $1 \
         AND status = 'finished' AND pull_request_id = $2 ORDER BY created_at DESC, id DESC LIMIT 1",
    )
    .bind(application_id.to_string())
    .bind(pull_request_id as i32)
    .fetch_optional(db.pool())
    .await?;
    t.as_deref().map(deployment).transpose()
}

/// The newest other `finished` deployment of the pull request that stored a configuration
/// snapshot (`markDeploymentConfigurationApplied()`'s `$previousDeployment`).
pub async fn last_finished_deployment_with_snapshot(
    db: &Db,
    application_id: i64,
    pull_request_id: i64,
    except_id: i64,
) -> DbResult<Option<Deployment>> {
    let t: Option<String> = sqlx::query_scalar(
        "SELECT row_to_json(q)::text FROM application_deployment_queues q WHERE application_id = $1 \
         AND status = 'finished' AND pull_request_id = $2 AND id != $3 AND configuration_snapshot IS NOT NULL \
         ORDER BY created_at DESC, id DESC LIMIT 1",
    )
    .bind(application_id.to_string())
    .bind(pull_request_id as i32)
    .bind(except_id)
    .fetch_optional(db.pool())
    .await?;
    t.as_deref().map(deployment).transpose()
}

/// `$queue->update($values)` (always writes, bumps `updated_at`).
pub async fn update_deployment(
    db: &Db,
    id: i64,
    values: &[(&'static str, ColumnValue)],
    now: NaiveDateTime,
) -> DbResult<()> {
    if values.is_empty() {
        return Ok(());
    }
    db.ensure_writable()?;
    let sql = format!(
        "UPDATE application_deployment_queues SET {}, updated_at = $1 WHERE id = $2",
        set_clause(values, 3)
    );
    bind_all(sqlx::query(&sql).bind(now).bind(id), values)
        .execute(db.pool())
        .await?;
    Ok(())
}

/// Moves a deployment to `status` unless it is already finished, failed or cancelled.
/// Returns the status it had.
pub async fn transition_status(
    db: &Db,
    id: i64,
    status: &str,
    now: NaiveDateTime,
) -> DbResult<Option<String>> {
    db.ensure_writable()?;
    let mut tx = db.pool().begin().await?;
    let current: Option<String> = sqlx::query_scalar(
        "SELECT status FROM application_deployment_queues WHERE id = $1 FOR UPDATE",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;
    if matches!(
        current.as_deref(),
        Some("finished" | "failed" | "cancelled-by-user")
    ) || current.is_none()
    {
        tx.commit().await?;
        return Ok(current);
    }
    sqlx::query(
        "UPDATE application_deployment_queues SET status = $1, updated_at = $2 WHERE id = $3",
    )
    .bind(status)
    .bind(now)
    .bind(id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(current)
}

/// One `logs` entry, before `order` is assigned.
#[derive(Debug, Clone)]
pub struct LogEntry {
    pub command: Option<String>,
    pub output: String,
    pub kind: &'static str,
    pub timestamp: String,
    pub hidden: bool,
    pub batch: u64,
}

/// `ApplicationDeploymentQueue::addLogEntry`: appends under a row lock and stores the
/// list with PHP's `json_encode` (escaped slashes and `\uXXXX`).
pub async fn append_log(
    db: &Db,
    id: i64,
    entries: &[LogEntry],
    _now: NaiveDateTime,
) -> DbResult<()> {
    if entries.is_empty() {
        return Ok(());
    }
    db.ensure_writable()?;
    let mut tx = db.pool().begin().await?;
    let current: Option<Option<String>> = sqlx::query_scalar(
        "SELECT logs FROM application_deployment_queues WHERE id = $1 FOR UPDATE",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(current) = current else {
        return Ok(());
    };
    let mut logs: Vec<Value> = match current.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        None => Vec::new(),
        Some(text) => match serde_json::from_str::<Value>(text) {
            Ok(Value::Array(v)) => v,
            // Upstream drops the entry when the stored logs are not valid JSON.
            _ => return Ok(()),
        },
    };
    for e in entries {
        let mut m = serde_json::Map::new();
        m.insert(
            "command".into(),
            e.command.clone().map_or(Value::Null, Value::String),
        );
        m.insert("output".into(), Value::String(e.output.clone()));
        m.insert("type".into(), Value::String(e.kind.into()));
        m.insert("timestamp".into(), Value::String(e.timestamp.clone()));
        m.insert("hidden".into(), Value::Bool(e.hidden));
        m.insert("batch".into(), Value::from(e.batch));
        m.insert("order".into(), Value::from(logs.len() + 1));
        logs.push(Value::Object(m));
    }
    let text = coolify_compat::php_json::encode(&Value::Array(logs));
    // A raw query: `updated_at` is not touched.
    sqlx::query("UPDATE application_deployment_queues SET logs = $1 WHERE id = $2")
        .bind(text)
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// `new_public_id()` for a new deployment.
pub fn new_deployment_uuid() -> String {
    new_public_id()
}

/// `(id, name, mount_path, host_path)` of the persistent volumes, by id.
pub async fn volume_rows(db: &Db, id: i64) -> DbResult<Vec<(i64, String, String, Option<String>)>> {
    Ok(sqlx::query_as(
        "SELECT id, name, mount_path, host_path FROM local_persistent_volumes WHERE resource_type = $1 AND resource_id = $2 ORDER BY id",
    )
    .bind(MORPH)
    .bind(id)
    .fetch_all(db.pool())
    .await?)
}

/// `(id, fs_path, mount_path, is_directory)` of the file mounts, by id.
pub async fn file_volume_rows(
    db: &Db,
    id: i64,
) -> DbResult<Vec<(i64, String, Option<String>, bool)>> {
    Ok(sqlx::query_as(
        "SELECT id, fs_path, mount_path, COALESCE(is_directory, false) FROM local_file_volumes WHERE resource_type = $1 AND resource_id = $2 ORDER BY id",
    )
    .bind(MORPH)
    .bind(id)
    .fetch_all(db.pool())
    .await?)
}
