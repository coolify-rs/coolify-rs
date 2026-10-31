//! Rows behind `App\Livewire\Project\Shared\Destination`, `Maintenance`, `ResourceOperations` and
//! the delete job's view of an application: additional servers, own volumes and volume backups.

use chrono::NaiveDateTime;
use coolify_domain::new_public_id;
use sqlx::Row as _;

use crate::applications::MORPH;
use crate::records::{Record, parse_row};
use crate::servers::{ColumnValue, bind_all};
use crate::{Db, DbError, DbResult};

fn decode(t: &str) -> DbResult<Record> {
    parse_row(t).map_err(|e| DbError::Schema(format!("decoding row: {e}")))
}

/// `$application->additional_servers`: the server ids.
pub async fn additional_server_ids(db: &Db, application_id: i64) -> DbResult<Vec<i64>> {
    Ok(sqlx::query_scalar(
        "SELECT server_id FROM additional_destinations WHERE application_id = $1 ORDER BY id",
    )
    .bind(application_id)
    .fetch_all(db.pool())
    .await?)
}

/// `$application->persistentStorages()` without `isSharedWithAnotherResource()`.
pub async fn own_volume_names(db: &Db, application_id: i64) -> DbResult<Vec<String>> {
    Ok(sqlx::query_scalar(
        "SELECT name FROM local_persistent_volumes WHERE resource_type = $1 AND resource_id = $2 \
         AND standalone_sqlite_id IS NULL ORDER BY id",
    )
    .bind(MORPH)
    .bind(application_id)
    .fetch_all(db.pool())
    .await?)
}

/// `volumeBackupSchedules()` of `DeleteResourceJob` for an application.
pub async fn volume_backup_schedules(db: &Db, application_id: i64) -> DbResult<Vec<Record>> {
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT row_to_json(b)::text FROM scheduled_volume_backups b WHERE \
         (b.backupable_type = 'App\\Models\\LocalPersistentVolume' AND b.backupable_id IN \
            (SELECT id FROM local_persistent_volumes WHERE resource_type = $1 AND resource_id = $2)) \
         OR (b.backupable_type = 'App\\Models\\LocalFileVolume' AND b.backupable_id IN \
            (SELECT id FROM local_file_volumes WHERE resource_type = $1 AND resource_id = $2)) ORDER BY b.id",
    )
    .bind(MORPH)
    .bind(application_id)
    .fetch_all(db.pool())
    .await?;
    rows.iter().map(|t| decode(t)).collect()
}

/// One row of `$resource->additional_networks` with what the page shows.
#[derive(Debug, Clone)]
pub struct AdditionalNetwork {
    pub network_id: i64,
    pub network_name: String,
    pub network: String,
    pub server_id: i64,
    pub server_uuid: String,
    pub server_name: String,
    pub status: String,
}

/// `$resource->additional_networks` with `server`.
pub async fn additional_networks(db: &Db, application_id: i64) -> DbResult<Vec<AdditionalNetwork>> {
    let rows = sqlx::query(
        "SELECT d.id, d.name, d.network, s.id AS server_id, s.uuid, s.name AS server_name, a.status \
         FROM additional_destinations a JOIN standalone_dockers d ON d.id = a.standalone_docker_id \
         JOIN servers s ON s.id = d.server_id WHERE a.application_id = $1 ORDER BY a.id",
    )
    .bind(application_id)
    .fetch_all(db.pool())
    .await?;
    Ok(rows
        .iter()
        .map(|r| AdditionalNetwork {
            network_id: r.get(0),
            network_name: r.get(1),
            network: r.get(2),
            server_id: r.get(3),
            server_uuid: r.get(4),
            server_name: r.get(5),
            status: r.get(6),
        })
        .collect())
}

/// `syncWithoutDetaching([$network->id => ['server_id' => $server->id]])`.
pub async fn attach_network(
    db: &Db,
    application_id: i64,
    network_id: i64,
    server_id: i64,
    now: NaiveDateTime,
) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(
        "INSERT INTO additional_destinations (application_id, server_id, standalone_docker_id, created_at, updated_at) \
         SELECT $1, $2, $3, $4, $4 WHERE NOT EXISTS (SELECT 1 FROM additional_destinations \
         WHERE application_id = $1 AND standalone_docker_id = $3)",
    )
    .bind(application_id)
    .bind(server_id)
    .bind(network_id)
    .bind(now)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// `additional_networks()->wherePivot('server_id', $serverId)->detach($networkId)`.
pub async fn detach_network(
    db: &Db,
    application_id: i64,
    network_id: i64,
    server_id: i64,
) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(
        "DELETE FROM additional_destinations WHERE application_id = $1 AND server_id = $2 AND standalone_docker_id = $3",
    )
    .bind(application_id)
    .bind(server_id)
    .bind(network_id)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// `promote()`: swaps the main destination with an additional one, in one transaction.
pub async fn promote_network(
    db: &Db,
    application_id: i64,
    network_id: i64,
    server_id: i64,
    main_network_id: i64,
    main_server_id: i64,
    now: NaiveDateTime,
) -> DbResult<()> {
    db.ensure_writable()?;
    let mut tx = db.pool().begin().await?;
    sqlx::query(
        "UPDATE applications SET destination_id = $2, destination_type = 'App\\Models\\StandaloneDocker', updated_at = $3 WHERE id = $1",
    )
    .bind(application_id)
    .bind(network_id)
    .bind(now)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "DELETE FROM additional_destinations WHERE application_id = $1 AND server_id = $2 AND standalone_docker_id = $3",
    )
    .bind(application_id)
    .bind(server_id)
    .bind(network_id)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO additional_destinations (application_id, server_id, standalone_docker_id) VALUES ($1, $2, $3)",
    )
    .bind(application_id)
    .bind(main_server_id)
    .bind(main_network_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Does the application have persistent volumes (`persistentStorages()->count() > 0`).
pub async fn has_persistent_storage(db: &Db, application_id: i64) -> DbResult<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM local_persistent_volumes WHERE resource_type = $1 AND resource_id = $2)",
    )
    .bind(MORPH)
    .bind(application_id)
    .fetch_one(db.pool())
    .await?)
}

/// `Project::query()->where('team_id', $teamId)->orderByRaw('LOWER(name)')`: `(id, name)`.
pub async fn team_projects(db: &Db, team_id: i64) -> DbResult<Vec<(i64, Option<String>)>> {
    Ok(
        sqlx::query_as("SELECT id, name FROM projects WHERE team_id = $1 ORDER BY LOWER(name)")
            .bind(team_id)
            .fetch_all(db.pool())
            .await?,
    )
}

/// `$project->environments`: `(id, name)`.
pub async fn project_environments(db: &Db, project_id: i64) -> DbResult<Vec<(i64, String)>> {
    Ok(
        sqlx::query_as("SELECT id, name FROM environments WHERE project_id = $1 ORDER BY id")
            .bind(project_id)
            .fetch_all(db.pool())
            .await?,
    )
}

/// `findResourceTeamEnvironment()`: `(environment uuid, project uuid)`.
pub async fn team_environment(
    db: &Db,
    team_id: i64,
    environment_id: i64,
) -> DbResult<Option<(String, String)>> {
    Ok(sqlx::query_as(
        "SELECT e.uuid, p.uuid FROM environments e JOIN projects p ON p.id = e.project_id WHERE e.id = $1 AND p.team_id = $2",
    )
    .bind(environment_id)
    .bind(team_id)
    .fetch_optional(db.pool())
    .await?)
}

/// `$resource->destination->uuid`; `table` is one of the code's own resource tables.
pub async fn destination_uuid(db: &Db, table: &str, id: i64) -> DbResult<Option<String>> {
    Ok(sqlx::query_scalar(&format!(
        "SELECT COALESCE(sd.uuid, sw.uuid) FROM {table} r \
         LEFT JOIN standalone_dockers sd ON r.destination_type = 'App\\Models\\StandaloneDocker' AND sd.id = r.destination_id \
         LEFT JOIN swarm_dockers sw ON r.destination_type = 'App\\Models\\SwarmDocker' AND sw.id = r.destination_id \
         WHERE r.id = $1"
    ))
    .bind(id)
    .fetch_optional(db.pool())
    .await?
    .flatten())
}

/// `$resource->fill(['environment_id' => $id])->save()`.
pub async fn set_environment(
    db: &Db,
    table: &str,
    id: i64,
    environment_id: i64,
    now: NaiveDateTime,
) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(&format!(
        "UPDATE {table} SET environment_id = $2, updated_at = $3 WHERE id = $1 AND environment_id <> $2"
    ))
    .bind(id)
    .bind(environment_id)
    .bind(now)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// The `environment_id` a new clone was filled with; its timestamps stay those of the insert.
pub async fn set_environment_only(
    db: &Db,
    table: &str,
    id: i64,
    environment_id: i64,
) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(&format!(
        "UPDATE {table} SET environment_id = $2 WHERE id = $1"
    ))
    .bind(id)
    .bind(environment_id)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// `cloneSecretManagerLinkTo()`.
pub async fn clone_secret_link(
    db: &Db,
    morph: &str,
    from: i64,
    to: i64,
    now: NaiveDateTime,
) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(
        "INSERT INTO secret_manager_links (uuid, resourceable_type, resourceable_id, integration_token_id, settings, created_at, updated_at) \
         SELECT $1, resourceable_type, $2, integration_token_id, settings, $3, $3 FROM secret_manager_links \
         WHERE resourceable_type = $4 AND resourceable_id = $5",
    )
    .bind(new_public_id())
    .bind(to)
    .bind(now)
    .bind(morph)
    .bind(from)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// `Project::query()->where('team_id', $teamId)->where('name', $name)->first()`.
pub async fn team_project_named(db: &Db, team_id: i64, name: &str) -> DbResult<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM projects WHERE team_id = $1 AND name = $2)",
    )
    .bind(team_id)
    .bind(name)
    .fetch_one(db.pool())
    .await?)
}

/// `$project->environments()->where('name', $name)->first()`.
pub async fn environment_named(db: &Db, project_id: i64, name: &str) -> DbResult<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM environments WHERE project_id = $1 AND name = $2)",
    )
    .bind(project_id)
    .bind(name)
    .fetch_one(db.pool())
    .await?)
}

/// `$project->environments()->create(['name' => $name, 'uuid' => new_public_id()])`: `(id, uuid)`.
pub async fn create_environment(
    db: &Db,
    project_id: i64,
    name: &str,
    now: NaiveDateTime,
) -> DbResult<(i64, String)> {
    db.ensure_writable()?;
    Ok(sqlx::query_as(
        "INSERT INTO environments (name, project_id, uuid, created_at, updated_at) VALUES ($1, $2, $3, $4, $4) RETURNING id, uuid",
    )
    .bind(name)
    .bind(project_id)
    .bind(new_public_id())
    .bind(now)
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
        "SELECT id FROM environment_variables WHERE resourceable_type = $1 AND resourceable_id = $2 AND is_preview = true \
         ORDER BY CASE WHEN is_required = true THEN 1 WHEN LOWER(key) LIKE 'service_%' THEN 2 ELSE 3 END, LOWER(key) ASC",
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
