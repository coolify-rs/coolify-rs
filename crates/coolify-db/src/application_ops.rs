//! Rows behind the application routes that are not plain field updates: `application_previews`,
//! `additional_destinations`, destinations and environments of a team, and `clone_application()`.

use chrono::NaiveDateTime;
use coolify_domain::new_public_id;
use serde_json::Value;

use crate::applications::MORPH;
use crate::records::{Record, parse_row};
use crate::servers::{ColumnValue, bind_all};
use crate::{Db, DbError, DbResult};

/// The morph type of `$destination` when it is a Swarm destination.
pub const SWARM_DOCKER_MORPH: &str = "App\\Models\\SwarmDocker";
/// The morph type of `$preview->persistentStorages()`.
pub const PREVIEW_MORPH: &str = "App\\Models\\ApplicationPreview";

fn decode(t: &str) -> DbResult<Record> {
    parse_row(t).map_err(|e| DbError::Schema(format!("decoding row: {e}")))
}

// ---------------------------------------------------------------------------
// Previews

/// `$application->previews()->get()`: newest pull request first, without soft deleted rows.
pub async fn previews(db: &Db, application_id: i64) -> DbResult<Vec<Record>> {
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT row_to_json(p)::text FROM application_previews p \
         WHERE application_id = $1 AND deleted_at IS NULL ORDER BY pull_request_id DESC",
    )
    .bind(application_id)
    .fetch_all(db.pool())
    .await?;
    rows.iter().map(|t| decode(t)).collect()
}

/// `ApplicationPreview::where('application_id', ...)->where('pull_request_id', ...)->first()`.
pub async fn preview(
    db: &Db,
    application_id: i64,
    pull_request_id: i64,
) -> DbResult<Option<Record>> {
    let row: Option<String> = sqlx::query_scalar(
        "SELECT row_to_json(p)::text FROM application_previews p \
         WHERE application_id = $1 AND pull_request_id = $2 AND deleted_at IS NULL LIMIT 1",
    )
    .bind(application_id)
    .bind(pull_request_id as i32)
    .fetch_optional(db.pool())
    .await?;
    row.as_deref().map(decode).transpose()
}

/// The commit of the latest other deployment of a pull request that has one.
pub async fn latest_pull_request_commit(
    db: &Db,
    application_id: i64,
    pull_request_id: i64,
    except_queue_id: i64,
) -> DbResult<Option<String>> {
    Ok(sqlx::query_scalar(
        "SELECT commit FROM application_deployment_queues \
         WHERE application_id = $1::text AND pull_request_id = $2 AND id <> $3 AND commit NOT IN ('HEAD', '') \
         ORDER BY id DESC LIMIT 1",
    )
    .bind(application_id)
    .bind(pull_request_id as i32)
    .bind(except_queue_id)
    .fetch_optional(db.pool())
    .await?)
}

/// `$preview->resetRestartLimit()`.
pub async fn reset_preview_restart_limit(db: &Db, id: i64, now: NaiveDateTime) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(
        "UPDATE application_previews SET restart_count = 0, restart_limit_reached = false, \
         last_restart_at = NULL, last_restart_type = NULL, updated_at = $2 WHERE id = $1 \
         AND (restart_count <> 0 OR restart_limit_reached OR last_restart_at IS NOT NULL OR last_restart_type IS NOT NULL)",
    )
    .bind(id)
    .bind(now)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// The soft deleted row too (`DeleteResourceJob` holds the model it was given).
pub async fn preview_by_id(db: &Db, id: i64) -> DbResult<Option<Record>> {
    let row: Option<String> =
        sqlx::query_scalar("SELECT row_to_json(p)::text FROM application_previews p WHERE id = $1")
            .bind(id)
            .fetch_optional(db.pool())
            .await?;
    row.as_deref().map(decode).transpose()
}

/// What `ApplicationPreview::create([...])` writes.
#[derive(Debug, Clone)]
pub struct NewPreview {
    pub application_id: i64,
    pub pull_request_id: i64,
    pub pull_request_html_url: String,
    pub git_type: Option<String>,
    pub docker_compose_domains: Option<String>,
    pub docker_registry_image_tag: Option<String>,
}

/// `ApplicationPreview::create([...])`: the row.
pub async fn create_preview(db: &Db, n: &NewPreview, now: NaiveDateTime) -> DbResult<Record> {
    db.ensure_writable()?;
    let row: String = sqlx::query_scalar(
        "INSERT INTO application_previews (uuid, application_id, pull_request_id, pull_request_html_url, git_type, \
         docker_compose_domains, docker_registry_image_tag, max_restart_count, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, 0, $8, $8) RETURNING row_to_json(application_previews)::text",
    )
    .bind(new_public_id())
    .bind(n.application_id)
    .bind(n.pull_request_id as i32)
    .bind(&n.pull_request_html_url)
    .bind(&n.git_type)
    .bind(&n.docker_compose_domains)
    .bind(&n.docker_registry_image_tag)
    .bind(now)
    .fetch_one(db.pool())
    .await?;
    decode(&row)
}

/// `$preview->save()` after the dirty check: only changed columns count, `updated_at` follows them.
pub async fn update_preview(
    db: &Db,
    id: i64,
    values: &[(&'static str, ColumnValue)],
    now: NaiveDateTime,
) -> DbResult<bool> {
    if values.is_empty() {
        return Ok(false);
    }
    db.ensure_writable()?;
    let is_json = |c: &str| matches!(c, "domain_port_overrides" | "domain_dns_statuses");
    let set = values
        .iter()
        .enumerate()
        .map(|(i, (c, v))| match v {
            ColumnValue::Text(None) => format!("\"{c}\" = NULL"),
            _ if is_json(c) => format!("\"{c}\" = ${}::json", i + 3),
            _ => format!("\"{c}\" = ${}", i + 3),
        })
        .collect::<Vec<_>>()
        .join(", ");
    let changed = values
        .iter()
        .enumerate()
        .map(|(i, (c, v))| match v {
            ColumnValue::Text(None) => format!("\"{c}\" IS NOT NULL"),
            _ if is_json(c) => format!("\"{c}\"::text IS DISTINCT FROM ${}", i + 3),
            _ => format!("\"{c}\" IS DISTINCT FROM ${}", i + 3),
        })
        .collect::<Vec<_>>()
        .join(" OR ");
    let sql = format!(
        "UPDATE application_previews SET {set}, updated_at = $1 WHERE id = $2 AND ({changed})"
    );
    let done = bind_all(sqlx::query(&sql).bind(now).bind(id), values)
        .execute(db.pool())
        .await?;
    Ok(done.rows_affected() > 0)
}

/// The domains of the other previews of the team that mention one of the hosts
/// (`ApplicationPreview::query()->whereIn('application_id', ...)->whereKeyNot(...)`).
pub async fn other_preview_domains(
    db: &Db,
    team_id: i64,
    except_id: i64,
    hosts: &[String],
) -> DbResult<Vec<(String, i64, String)>> {
    if hosts.is_empty() {
        return Ok(Vec::new());
    }
    let patterns: Vec<String> = hosts.iter().map(|h| format!("%{h}%")).collect();
    Ok(sqlx::query_as::<_, (String, i32, String)>(
        "SELECT pr.uuid, pr.pull_request_id, pr.fqdn FROM application_previews pr \
         JOIN applications a ON a.id = pr.application_id \
         JOIN environments e ON e.id = a.environment_id \
         JOIN projects p ON p.id = e.project_id \
         WHERE p.team_id = $1 AND pr.id <> $2 AND pr.fqdn IS NOT NULL AND pr.deleted_at IS NULL AND a.deleted_at IS NULL \
         AND pr.fqdn LIKE ANY($3) ORDER BY pr.id",
    )
    .bind(team_id)
    .bind(except_id)
    .bind(&patterns)
    .fetch_all(db.pool())
    .await?
    .into_iter()
    .map(|(u, p, f)| (u, i64::from(p), f))
    .collect())
}

/// `$preview->delete()` (soft delete).
pub async fn soft_delete_preview(db: &Db, id: i64, now: NaiveDateTime) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query("UPDATE application_previews SET deleted_at = $2, updated_at = $2 WHERE id = $1")
        .bind(id)
        .bind(now)
        .execute(db.pool())
        .await?;
    Ok(())
}

/// The end of `forceDeleting` and `forceDelete()`: the volume rows of the preview, the Compose
/// volume rows the parser stored on the application under `compose_volumes`, and the row.
pub async fn force_delete_preview(
    db: &Db,
    id: i64,
    application_id: i64,
    compose_volumes: &[String],
) -> DbResult<()> {
    db.ensure_writable()?;
    let mut tx = db.pool().begin().await?;
    sqlx::query(
        "DELETE FROM local_persistent_volumes WHERE resource_type = $1 AND resource_id = $2 AND name = ANY($3)",
    )
    .bind(MORPH)
    .bind(application_id)
    .bind(compose_volumes)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "DELETE FROM local_persistent_volumes WHERE resource_type = $1 AND resource_id = $2",
    )
    .bind(PREVIEW_MORPH)
    .bind(id)
    .execute(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM application_previews WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// The latest finished production deployments with a real commit, as `GetRollbackImages` lists
/// them: `(commit, max(updated_at))`, newest first.
pub async fn finished_commits(
    db: &Db,
    application_id: i64,
    limit: i64,
) -> DbResult<Vec<(String, String)>> {
    Ok(sqlx::query_as(
        "SELECT commit, to_char(max(updated_at), 'YYYY-MM-DD HH24:MI:SS') AS deployed_at FROM application_deployment_queues \
         WHERE application_id = $1 AND status = 'finished' AND pull_request_id = 0 AND commit IS NOT NULL AND commit <> 'HEAD' \
         GROUP BY commit ORDER BY max(updated_at) DESC LIMIT $2",
    )
    .bind(application_id.to_string())
    .bind(limit)
    .fetch_all(db.pool())
    .await?)
}

// ---------------------------------------------------------------------------
// Destinations and environments of a team

/// A row of `standalone_dockers` or `swarm_dockers`, with its server.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct DestinationRow {
    pub id: i64,
    pub uuid: String,
    pub name: String,
    pub network: Option<String>,
    pub server_id: i64,
    pub server_uuid: String,
}

fn destination_select(table: &str, condition: &str) -> String {
    format!(
        "SELECT d.id, d.uuid, d.name, d.network, d.server_id, s.uuid AS server_uuid FROM {table} d \
         JOIN servers s ON s.id = d.server_id WHERE {condition}"
    )
}

/// `StandaloneDocker::ownedByCurrentTeamAPI($teamId)->where('uuid', $uuid)->first()`.
pub async fn standalone_docker_in_team(
    db: &Db,
    team_id: i64,
    uuid: &str,
) -> DbResult<Option<DestinationRow>> {
    Ok(sqlx::query_as(&destination_select(
        "standalone_dockers",
        "d.uuid = $1 AND s.team_id = $2 AND s.deleted_at IS NULL LIMIT 1",
    ))
    .bind(uuid)
    .bind(team_id)
    .fetch_optional(db.pool())
    .await?)
}

/// `SwarmDocker::ownedByCurrentTeamAPI($teamId)->where('uuid', $uuid)->first()`.
pub async fn swarm_docker_in_team(
    db: &Db,
    team_id: i64,
    uuid: &str,
) -> DbResult<Option<DestinationRow>> {
    Ok(sqlx::query_as(&destination_select(
        "swarm_dockers",
        "d.uuid = $1 AND s.team_id = $2 AND s.deleted_at IS NULL LIMIT 1",
    ))
    .bind(uuid)
    .bind(team_id)
    .fetch_optional(db.pool())
    .await?)
}

/// `$application->destination`, from its morph columns.
pub async fn destination_by_morph(
    db: &Db,
    destination_type: &str,
    destination_id: i64,
) -> DbResult<Option<DestinationRow>> {
    let table = match destination_type {
        SWARM_DOCKER_MORPH => "swarm_dockers",
        crate::databases::STANDALONE_DOCKER_MORPH => "standalone_dockers",
        _ => return Ok(None),
    };
    Ok(sqlx::query_as(&destination_select(table, "d.id = $1"))
        .bind(destination_id)
        .fetch_optional(db.pool())
        .await?)
}

/// `$server->destinations()`: the standalone Docker networks, then the Swarm ones:
/// `(id, uuid, morph class)`.
pub async fn all_destinations(
    db: &Db,
    server_id: i64,
) -> DbResult<Vec<(i64, String, &'static str)>> {
    let mut out: Vec<(i64, String, &'static str)> = Vec::new();
    for (table, morph) in [
        (
            "standalone_dockers",
            crate::databases::STANDALONE_DOCKER_MORPH,
        ),
        ("swarm_dockers", SWARM_DOCKER_MORPH),
    ] {
        let rows: Vec<(i64, String)> = sqlx::query_as(&format!(
            "SELECT id, uuid::text FROM {table} WHERE server_id = $1 ORDER BY id"
        ))
        .bind(server_id)
        .fetch_all(db.pool())
        .await?;
        out.extend(rows.into_iter().map(|(id, uuid)| (id, uuid, morph)));
    }
    Ok(out)
}

/// `$application->additional_networks`, with the pivot's `server_id`.
pub async fn additional_networks(db: &Db, application_id: i64) -> DbResult<Vec<DestinationRow>> {
    Ok(sqlx::query_as(
        "SELECT d.id, d.uuid, d.name, d.network, ad.server_id, s.uuid AS server_uuid \
         FROM additional_destinations ad JOIN standalone_dockers d ON d.id = ad.standalone_docker_id \
         JOIN servers s ON s.id = d.server_id WHERE ad.application_id = $1 ORDER BY ad.id",
    )
    .bind(application_id)
    .fetch_all(db.pool())
    .await?)
}

/// `$application->additional_networks()->attach($destination->id, ['server_id' => ...])`.
pub async fn attach_destination(
    db: &Db,
    application_id: i64,
    standalone_docker_id: i64,
    server_id: i64,
) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(
        "INSERT INTO additional_destinations (application_id, standalone_docker_id, server_id) VALUES ($1, $2, $3)",
    )
    .bind(application_id)
    .bind(standalone_docker_id)
    .bind(server_id)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// `$application->additional_networks()->wherePivot('server_id', ...)->detach($destination->id)`.
pub async fn detach_destination(
    db: &Db,
    application_id: i64,
    standalone_docker_id: i64,
    server_id: i64,
) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(
        "DELETE FROM additional_destinations WHERE application_id = $1 AND standalone_docker_id = $2 AND server_id = $3",
    )
    .bind(application_id)
    .bind(standalone_docker_id)
    .bind(server_id)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// `Environment::ownedByCurrentTeamAPI($teamId)->whereUuid($uuid)->first()`:
/// `(id, uuid, project uuid)`.
pub async fn environment_in_team(
    db: &Db,
    team_id: i64,
    uuid: &str,
) -> DbResult<Option<(i64, String, String)>> {
    Ok(sqlx::query_as(
        "SELECT e.id, e.uuid, p.uuid FROM environments e JOIN projects p ON p.id = e.project_id \
         WHERE p.team_id = $1 AND e.uuid = $2 ORDER BY e.name LIMIT 1",
    )
    .bind(team_id)
    .bind(uuid)
    .fetch_optional(db.pool())
    .await?)
}

/// The volumes `forceDeleting` removes for a preview: no host path, preview suffix on.
pub async fn preview_suffixed_volumes(db: &Db, application_id: i64) -> DbResult<Vec<String>> {
    Ok(sqlx::query_scalar(
        "SELECT name FROM local_persistent_volumes WHERE resource_type = $1 AND resource_id = $2 \
         AND is_preview_suffix_enabled = true AND (host_path IS NULL OR btrim(host_path) = '') ORDER BY id",
    )
    .bind(MORPH)
    .bind(application_id)
    .fetch_all(db.pool())
    .await?)
}

/// `$application->persistentStorages()->exists()`.
pub async fn has_persistent_storage(db: &Db, application_id: i64) -> DbResult<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM local_persistent_volumes WHERE resource_type = $1 AND resource_id = $2)",
    )
    .bind(MORPH)
    .bind(application_id)
    .fetch_one(db.pool())
    .await?)
}

// ---------------------------------------------------------------------------
// clone_application()

/// The columns of a table without the given ones (`replicate($except)` copies all others).
async fn columns_without(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    table: &str,
    except: &[&str],
) -> DbResult<Vec<String>> {
    let except: Vec<String> = except.iter().map(|c| (*c).to_owned()).collect();
    Ok(sqlx::query_scalar(
        "SELECT column_name::text FROM information_schema.columns \
         WHERE table_schema = current_schema() AND table_name = $1 AND column_name::text <> ALL($2) ORDER BY ordinal_position",
    )
    .bind(table)
    .bind(&except)
    .fetch_all(&mut **tx)
    .await?)
}

fn quoted(columns: &[String]) -> String {
    columns
        .iter()
        .map(|c| format!("\"{c}\""))
        .collect::<Vec<_>>()
        .join(", ")
}

/// A copy of one row: every column but `drop`, with the `set` values. The timestamps are `now`.
async fn replicate(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    table: &str,
    source_id: i64,
    drop: &[&str],
    set: &[(&'static str, ColumnValue)],
    now: NaiveDateTime,
) -> DbResult<i64> {
    let mut except: Vec<&str> = vec!["id", "created_at", "updated_at"];
    except.extend_from_slice(drop);
    except.extend(set.iter().map(|(c, _)| *c));
    let copied = columns_without(tx, table, &except).await?;
    let json_columns = ["domain_port_overrides", "noindex_domains"];
    let placeholders: Vec<String> = set
        .iter()
        .enumerate()
        .map(|(i, (c, v))| match v {
            ColumnValue::Text(None) => "NULL".to_owned(),
            ColumnValue::Text(Some(_)) if json_columns.contains(c) => format!("${}::json", i + 3),
            ColumnValue::Int(_) if *c == "application_id" || *c == "team_id" => {
                format!("${}::bigint", i + 3)
            }
            _ => format!("${}", i + 3),
        })
        .collect();
    let set_names: Vec<String> = set.iter().map(|(c, _)| format!("\"{c}\"")).collect();
    let mut targets = quoted(&copied);
    let mut sources = targets.clone();
    if !set.is_empty() {
        targets = format!("{targets}, {}", set_names.join(", "));
        sources = format!("{sources}, {}", placeholders.join(", "));
    }
    let sql = format!(
        "INSERT INTO {table} ({targets}, created_at, updated_at) \
         SELECT {sources}, $2, $2 FROM {table} WHERE id = $1 RETURNING id"
    );
    let row = bind_all(sqlx::query(&sql).bind(source_id).bind(now), set)
        .fetch_one(&mut **tx)
        .await?;
    Ok(sqlx::Row::get(&row, 0))
}

/// What `clone_application()` decided before it writes.
#[derive(Debug, Clone)]
pub struct CloneApplication {
    pub source_id: i64,
    pub source_uuid: String,
    pub new_uuid: String,
    pub server_team_id: i64,
    /// The `applications` columns the `fill()` and the `saving` hook set.
    pub columns: Vec<(&'static str, ColumnValue)>,
    /// `NIXPACKS_NODE_VERSION` of the `created` hook: `(value, preview value, version)`, encrypted.
    pub nixpacks_default: Option<(String, String, String)>,
}

/// The name of a cloned volume (`clone_application()`).
pub fn cloned_volume_name(
    name: &str,
    is_sqlite: bool,
    source_uuid: &str,
    new_uuid: &str,
) -> String {
    if is_sqlite {
        format!("{new_uuid}-{name}")
    } else if name.starts_with(source_uuid) {
        name.replace(source_uuid, new_uuid)
    } else {
        format!(
            "{new_uuid}-{}",
            name.rsplit_once('-').map_or(name, |(_, last)| last)
        )
    }
}

/// The rows `clone_application()` creates, in one transaction: application, settings, tags,
/// scheduled tasks, previews (without domains; the caller generates them), persistent volumes,
/// file storages, variables and the secret manager link. Returns the new application's id, the
/// ids of its previews and `(source volume name, new volume id, new volume name)`.
pub async fn clone_application(
    db: &Db,
    c: &CloneApplication,
    now: NaiveDateTime,
) -> DbResult<(i64, Vec<i64>, Vec<(String, i64, String)>)> {
    db.ensure_writable()?;
    let mut tx = db.pool().begin().await?;
    let new_id = replicate(&mut tx, "applications", c.source_id, &[], &c.columns, now).await?;

    // Settings.
    let settings: Option<i64> =
        sqlx::query_scalar("SELECT id FROM application_settings WHERE application_id = $1 LIMIT 1")
            .bind(c.source_id)
            .fetch_optional(&mut *tx)
            .await?;
    if let Some(id) = settings {
        replicate(
            &mut tx,
            "application_settings",
            id,
            &[],
            &[("application_id", ColumnValue::Int(new_id))],
            now,
        )
        .await?;
    }

    // Tags.
    sqlx::query(
        "INSERT INTO taggables (tag_id, taggable_id, taggable_type) \
         SELECT tag_id, $2, taggable_type FROM taggables WHERE taggable_type = $3 AND taggable_id = $1",
    )
    .bind(c.source_id)
    .bind(new_id)
    .bind(MORPH)
    .execute(&mut *tx)
    .await?;

    // Scheduled tasks.
    let tasks: Vec<i64> = sqlx::query_scalar(
        "SELECT id FROM scheduled_tasks WHERE application_id = $1 ORDER BY name ASC",
    )
    .bind(c.source_id)
    .fetch_all(&mut *tx)
    .await?;
    for id in tasks {
        replicate(
            &mut tx,
            "scheduled_tasks",
            id,
            &[],
            &[
                ("uuid", ColumnValue::Text(Some(new_public_id()))),
                ("application_id", ColumnValue::Int(new_id)),
                ("team_id", ColumnValue::Int(c.server_team_id)),
            ],
            now,
        )
        .await?;
    }

    // Previews; `saving` sets `last_online_at` because `status` is dirty.
    let previews: Vec<i64> = sqlx::query_scalar(
        "SELECT id FROM application_previews WHERE application_id = $1 AND deleted_at IS NULL ORDER BY pull_request_id DESC",
    )
    .bind(c.source_id)
    .fetch_all(&mut *tx)
    .await?;
    let mut new_previews = Vec::new();
    for id in previews {
        new_previews.push(
            replicate(
                &mut tx,
                "application_previews",
                id,
                &[],
                &[
                    ("uuid", ColumnValue::Text(Some(new_public_id()))),
                    ("application_id", ColumnValue::Int(new_id)),
                    ("status", ColumnValue::Text(Some("exited".into()))),
                    ("fqdn", ColumnValue::Text(None)),
                    ("docker_compose_domains", ColumnValue::Text(None)),
                    ("last_online_at", ColumnValue::Time(now)),
                ],
                now,
            )
            .await?,
        );
    }

    // Persistent volumes.
    let volumes: Vec<(i64, String, Option<i64>)> = sqlx::query_as(
        "SELECT id, name, standalone_sqlite_id FROM local_persistent_volumes \
         WHERE resource_type = $1 AND resource_id = $2 ORDER BY id",
    )
    .bind(MORPH)
    .bind(c.source_id)
    .fetch_all(&mut *tx)
    .await?;
    let mut new_volumes = Vec::new();
    for (id, name, sqlite) in volumes {
        let new_name = cloned_volume_name(&name, sqlite.is_some(), &c.source_uuid, &c.new_uuid);
        let new_volume = replicate(
            &mut tx,
            "local_persistent_volumes",
            id,
            &[],
            &[
                ("uuid", ColumnValue::Text(Some(new_public_id()))),
                ("name", ColumnValue::Text(Some(new_name.clone()))),
                ("resource_id", ColumnValue::Int(new_id)),
                ("standalone_sqlite_id", ColumnValue::Text(None)),
            ],
            now,
        )
        .await?;
        new_volumes.push((name, new_volume, new_name));
    }

    // File storages keep the uuid and `fs_path` of the source: `replicate()` only leaves out the
    // id and the timestamps, and the `creating` hook only fills an empty uuid.
    let files: Vec<i64> = sqlx::query_scalar(
        "SELECT id FROM local_file_volumes WHERE resource_type = $1 AND resource_id = $2 ORDER BY id",
    )
    .bind(MORPH)
    .bind(c.source_id)
    .fetch_all(&mut *tx)
    .await?;
    for id in files {
        replicate(
            &mut tx,
            "local_file_volumes",
            id,
            &[],
            &[("resource_id", ColumnValue::Int(new_id))],
            now,
        )
        .await?;
    }

    // The default the `created` hook adds, dropped again when the source has that key.
    let source_keys: Vec<String> = sqlx::query_scalar(
        "SELECT key FROM environment_variables WHERE resourceable_type = $1 AND resourceable_id = $2",
    )
    .bind(MORPH)
    .bind(c.source_id)
    .fetch_all(&mut *tx)
    .await?;
    if let Some((value, preview_value, version)) = &c.nixpacks_default
        && !source_keys.iter().any(|k| k == "NIXPACKS_NODE_VERSION")
    {
        for (v, is_preview) in [(value, false), (preview_value, true)] {
            sqlx::query(
                "INSERT INTO environment_variables (key, value, is_multiline, is_literal, is_buildtime, is_runtime, is_preview, \
                 is_required, is_shown_once, comment, resourceable_type, resourceable_id, is_shared, uuid, version, created_at, updated_at) \
                 VALUES ('NIXPACKS_NODE_VERSION', $1, false, false, true, false, $2, false, false, NULL, $3, $4, false, $5, $6, $7, $7)",
            )
            .bind(v)
            .bind(is_preview)
            .bind(MORPH)
            .bind(new_id)
            .bind(new_public_id())
            .bind(version)
            .bind(now)
            .execute(&mut *tx)
            .await?;
        }
    }

    // Production variables (table order), then preview ones (their relation's order). They keep
    // their uuid: `withoutEvents` skips the hook, and `replicate()` copied it.
    let production: Vec<i64> = sqlx::query_scalar(
        "SELECT id FROM environment_variables WHERE resourceable_type = $1 AND resourceable_id = $2 AND is_preview = false ORDER BY id",
    )
    .bind(MORPH)
    .bind(c.source_id)
    .fetch_all(&mut *tx)
    .await?;
    for id in production {
        replicate(
            &mut tx,
            "environment_variables",
            id,
            &[],
            &[("resourceable_id", ColumnValue::Int(new_id))],
            now,
        )
        .await?;
    }

    // cloneSecretManagerLinkTo().
    sqlx::query(
        "INSERT INTO secret_manager_links (uuid, resourceable_type, resourceable_id, integration_token_id, settings, created_at, updated_at) \
         SELECT $1, resourceable_type, $2, integration_token_id, settings, $3, $3 FROM secret_manager_links \
         WHERE resourceable_type = $4 AND resourceable_id = $5",
    )
    .bind(new_public_id())
    .bind(new_id)
    .bind(now)
    .bind(MORPH)
    .bind(c.source_id)
    .execute(&mut *tx)
    .await?;

    let preview_vars: Vec<i64> = sqlx::query_scalar(
        // `environment_variables_preview` has no order since v4.6.0.
        "SELECT id FROM environment_variables WHERE resourceable_type = $1 AND resourceable_id = $2 AND is_preview = true",
    )
    .bind(MORPH)
    .bind(c.source_id)
    .fetch_all(&mut *tx)
    .await?;
    for id in preview_vars {
        replicate(
            &mut tx,
            "environment_variables",
            id,
            &[],
            &[("resourceable_id", ColumnValue::Int(new_id))],
            now,
        )
        .await?;
    }

    tx.commit().await?;
    Ok((new_id, new_previews, new_volumes))
}

/// The file storage rows of the application (`fs_path` etc.), for the save job.
pub async fn file_storage_rows(db: &Db, application_id: i64) -> DbResult<Vec<Record>> {
    crate::storages::list(
        db,
        crate::storages::StorageKind::File,
        MORPH,
        application_id,
    )
    .await
}

/// A JSON column of a row as a value (`row_to_json` gives it parsed).
pub fn json_column(r: &Record, key: &str) -> Option<Value> {
    match r.get(key) {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => serde_json::from_str(s).ok(),
        Some(v) => Some(v.clone()),
    }
}

/// `VolumeCloneJob::composeVolumeLabels()`'s project: the `--project-name` of the volume's
/// resource, which is the service UUID for a service application or database, else the resource
/// UUID. `None` when the resource (or its service) is gone.
pub async fn compose_project_of_volume(db: &Db, volume_id: i64) -> DbResult<Option<String>> {
    let owner: Option<(Option<String>, Option<i64>)> = sqlx::query_as(
        "SELECT resource_type, resource_id FROM local_persistent_volumes WHERE id = $1",
    )
    .bind(volume_id)
    .fetch_optional(db.pool())
    .await?;
    let Some((Some(morph), Some(id))) = owner else {
        return Ok(None);
    };
    let sql = match morph.as_str() {
        r"App\Models\ServiceApplication" | r"App\Models\ServiceDatabase" => {
            let table = if morph.ends_with("ServiceApplication") {
                "service_applications"
            } else {
                "service_databases"
            };
            format!(
                "SELECT s.uuid FROM {table} r JOIN services s ON s.id = r.service_id \
                 WHERE r.id = $1 AND r.deleted_at IS NULL AND s.deleted_at IS NULL"
            )
        }
        r"App\Models\Application" => {
            "SELECT uuid FROM applications WHERE id = $1 AND deleted_at IS NULL".to_owned()
        }
        other => match crate::databases::Kind::ALL
            .iter()
            .find(|k| k.morph() == other)
        {
            Some(kind) => format!(
                "SELECT uuid FROM {} WHERE id = $1 AND deleted_at IS NULL",
                kind.table()
            ),
            None => return Ok(None),
        },
    };
    let uuid: Option<String> = sqlx::query_scalar(&sql)
        .bind(id)
        .fetch_optional(db.pool())
        .await?;
    Ok(uuid.filter(|u| !u.trim().is_empty()))
}

/// The `ignores_compose_driver_options` flag `VolumeCloneJob` sets on the new volume.
pub async fn set_ignores_driver_options(db: &Db, volume_id: i64) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(
        "UPDATE local_persistent_volumes SET ignores_compose_driver_options = true WHERE id = $1",
    )
    .bind(volume_id)
    .execute(db.pool())
    .await?;
    Ok(())
}
