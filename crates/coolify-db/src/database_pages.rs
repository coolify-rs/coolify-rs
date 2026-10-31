//! Queries behind the database pages of the web UI (`Project\Database\*` Livewire components)
//! that the REST API has no use for.

use chrono::NaiveDateTime;
use serde_json::Value;

use crate::database_parts::{self as parts};
use crate::databases::Kind;
use crate::records::Record;
use crate::{Db, DbResult};
use coolify_domain::TeamId;

fn decode_all(texts: Vec<String>) -> Vec<Record> {
    texts
        .iter()
        .filter_map(|t| serde_json::from_str::<Record>(t).ok())
        .collect()
}

/// `$this->database->init_scripts = ...; save()`: a `json` column, so it cannot go through the
/// text binds of `update_if_changed`.
pub async fn save_init_scripts(
    db: &Db,
    id: i64,
    scripts: &Value,
    now: NaiveDateTime,
) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(
        "UPDATE standalone_postgresqls SET init_scripts = $1::json, updated_at = $2 WHERE id = $3",
    )
    .bind(scripts.to_string())
    .bind(now)
    .bind(id)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// A row of `connectedVolumes` with the application it mounts into.
#[derive(Debug, sqlx::FromRow)]
pub struct SqliteConnection {
    pub id: i64,
    pub uuid: Option<String>,
    pub name: String,
    pub mount_path: String,
    pub host_path: Option<String>,
    pub resource_id: Option<i64>,
    pub application_uuid: Option<String>,
    pub application_name: Option<String>,
    pub environment_uuid: Option<String>,
    pub project_uuid: Option<String>,
}

const CONNECTION_SELECT: &str = "SELECT v.id, v.uuid, v.name, v.mount_path, v.host_path, v.resource_id, a.uuid AS application_uuid, a.name AS application_name, e.uuid AS environment_uuid, p.uuid AS project_uuid
     FROM local_persistent_volumes v
     LEFT JOIN applications a ON a.id = v.resource_id AND v.resource_type = 'App\\Models\\Application'
     LEFT JOIN environments e ON e.id = a.environment_id
     LEFT JOIN projects p ON p.id = e.project_id
     WHERE v.standalone_sqlite_id = $1";

/// `$database->connectedVolumes()->with('resource')->orderBy('id')->get()`.
pub async fn sqlite_connections(db: &Db, sqlite_id: i64) -> DbResult<Vec<SqliteConnection>> {
    Ok(
        sqlx::query_as(&format!("{CONNECTION_SELECT} ORDER BY v.id"))
            .bind(sqlite_id)
            .fetch_all(db.pool())
            .await?,
    )
}

pub async fn sqlite_connection(
    db: &Db,
    sqlite_id: i64,
    volume_id: i64,
) -> DbResult<Option<SqliteConnection>> {
    Ok(
        sqlx::query_as(&format!("{CONNECTION_SELECT} AND v.id = $2"))
            .bind(sqlite_id)
            .bind(volume_id)
            .fetch_optional(db.pool())
            .await?,
    )
}

/// An application a SQLite database can be mounted into.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ConnectableApplication {
    pub id: i64,
    pub uuid: String,
    pub name: String,
    pub build_pack: String,
    pub docker_compose_raw: Option<String>,
    pub project_name: Option<String>,
    pub environment_name: Option<String>,
    pub project_uuid: Option<String>,
    pub environment_uuid: Option<String>,
    pub has_additional_servers: bool,
}

/// `Application::ownedByCurrentTeamAPI($teamId)` on the given server (`connectableApplications`).
pub async fn connectable_applications(
    db: &Db,
    team_id: TeamId,
    server_id: i64,
) -> DbResult<Vec<ConnectableApplication>> {
    Ok(sqlx::query_as(
        "SELECT a.id, a.uuid, a.name, a.build_pack, a.docker_compose_raw, p.name AS project_name, e.name AS environment_name,
                p.uuid AS project_uuid, e.uuid AS environment_uuid,
                EXISTS (SELECT 1 FROM additional_destinations ad WHERE ad.application_id = a.id) AS has_additional_servers
         FROM applications a
         JOIN environments e ON e.id = a.environment_id
         JOIN projects p ON p.id = e.project_id
         JOIN standalone_dockers d ON d.id = a.destination_id AND a.destination_type = 'App\\Models\\StandaloneDocker'
         WHERE p.team_id = $1 AND d.server_id = $2 AND a.deleted_at IS NULL
         ORDER BY a.name",
    )
    .bind(team_id)
    .bind(server_id)
    .fetch_all(db.pool())
    .await?)
}

/// `$application->persistentStorages()->where('standalone_sqlite_id', ...)->exists()`.
pub async fn application_mounts_sqlite(
    db: &Db,
    application_id: i64,
    sqlite_id: i64,
) -> DbResult<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM local_persistent_volumes WHERE resource_type = 'App\\Models\\Application' AND resource_id = $1 AND standalone_sqlite_id = $2)",
    )
    .bind(application_id)
    .bind(sqlite_id)
    .fetch_one(db.pool())
    .await?)
}

/// `LocalPersistentVolume::create` of `ConnectApplication::connect`.
pub async fn create_sqlite_connection(
    db: &Db,
    uuid: &str,
    name: &str,
    mount_path: &str,
    sqlite_id: i64,
    application_id: i64,
    now: NaiveDateTime,
) -> DbResult<i64> {
    db.ensure_writable()?;
    Ok(sqlx::query_scalar(
        "INSERT INTO local_persistent_volumes (uuid, name, mount_path, host_path, standalone_sqlite_id, resource_id, resource_type, is_preview_suffix_enabled, created_at, updated_at)
         VALUES ($1, $2, $3, NULL, $4, $5, 'App\\Models\\Application', false, $6, $6) RETURNING id",
    )
    .bind(uuid)
    .bind(name)
    .bind(mount_path)
    .bind(sqlite_id)
    .bind(application_id)
    .bind(now)
    .fetch_one(db.pool())
    .await?)
}

pub async fn delete_volume(db: &Db, id: i64) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query("DELETE FROM local_persistent_volumes WHERE id = $1")
        .bind(id)
        .execute(db.pool())
        .await?;
    Ok(())
}

/// `keepAnonymousDataVolume`.
pub async fn keep_anonymous_volume(
    db: &Db,
    kind: Kind,
    id: i64,
    uuid: &str,
    name: &str,
    data_directory: &str,
    now: NaiveDateTime,
) -> DbResult<()> {
    db.ensure_writable()?;
    let updated = sqlx::query(
        "UPDATE local_persistent_volumes SET name = $1, updated_at = $2 WHERE id = (SELECT id FROM local_persistent_volumes WHERE resource_type = $3 AND resource_id = $4 AND mount_path = $5 ORDER BY id LIMIT 1)",
    )
    .bind(name)
    .bind(now)
    .bind(kind.morph())
    .bind(id)
    .bind(data_directory)
    .execute(db.pool())
    .await?
    .rows_affected();
    if updated == 0 {
        sqlx::query(
            "INSERT INTO local_persistent_volumes (uuid, name, mount_path, resource_id, resource_type, created_at, updated_at) VALUES ($1, $2, $3, $4, $5, $6, $6)",
        )
        .bind(uuid)
        .bind(name)
        .bind(data_directory)
        .bind(id)
        .bind(kind.morph())
        .bind(now)
        .execute(db.pool())
        .await?;
    }
    Ok(())
}

// ---- Backups ----

/// `$database->scheduledBackups` with `withCount('executions')`, in table order.
pub async fn scheduled_backups(db: &Db, morph: &str, id: i64) -> DbResult<Vec<Record>> {
    let texts: Vec<String> = sqlx::query_scalar(
        "SELECT row_to_json(b)::text FROM (SELECT s.*, (SELECT count(*) FROM scheduled_database_backup_executions e WHERE e.scheduled_database_backup_id = s.id) AS executions_count
         FROM scheduled_database_backups s WHERE s.database_type = $1 AND s.database_id = $2 ORDER BY s.id) b",
    )
    .bind(morph)
    .bind(id)
    .fetch_all(db.pool())
    .await?;
    Ok(decode_all(texts))
}

/// `executionsPaginated($skip, $take)`: the page and the total count.
pub async fn executions_page(
    db: &Db,
    backup_id: i64,
    skip: i64,
    take: i64,
) -> DbResult<(Vec<Record>, i64)> {
    let total: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM scheduled_database_backup_executions WHERE scheduled_database_backup_id = $1",
    )
    .bind(backup_id)
    .fetch_one(db.pool())
    .await?;
    let texts: Vec<String> = sqlx::query_scalar(
        "SELECT row_to_json(e)::text FROM scheduled_database_backup_executions e WHERE e.scheduled_database_backup_id = $1 ORDER BY e.created_at DESC, e.id DESC OFFSET $2 LIMIT $3",
    )
    .bind(backup_id)
    .bind(skip)
    .bind(take)
    .fetch_all(db.pool())
    .await?;
    Ok((decode_all(texts), total))
}

/// `$execution->s3Replicas` with the storage name.
pub async fn execution_replicas(db: &Db, execution_id: i64) -> DbResult<Vec<Record>> {
    let texts: Vec<String> = sqlx::query_scalar(
        "SELECT row_to_json(r)::text FROM (SELECT p.*, s.name AS s3_name FROM database_backup_s3_replicas p LEFT JOIN s3_storages s ON s.id = p.s3_storage_id WHERE p.execution_id = $1 ORDER BY p.id) r",
    )
    .bind(execution_id)
    .fetch_all(db.pool())
    .await?;
    Ok(decode_all(texts))
}

/// `$backup->executions()->where('status', 'failed')->delete()` (replicas go with them).
pub async fn delete_failed_executions(db: &Db, backup_id: i64) -> DbResult<u64> {
    db.ensure_writable()?;
    Ok(sqlx::query(
        "DELETE FROM scheduled_database_backup_executions WHERE scheduled_database_backup_id = $1 AND status = 'failed'",
    )
    .bind(backup_id)
    .execute(db.pool())
    .await?
    .rows_affected())
}

/// `$backup->executions()->where('local_storage_deleted', true)->delete()`.
pub async fn delete_local_deleted_executions(db: &Db, backup_id: i64) -> DbResult<u64> {
    db.ensure_writable()?;
    Ok(sqlx::query(
        "DELETE FROM scheduled_database_backup_executions WHERE scheduled_database_backup_id = $1 AND local_storage_deleted = true",
    )
    .bind(backup_id)
    .execute(db.pool())
    .await?
    .rows_affected())
}

/// An execution by id, with its schedule (`ScheduledDatabaseBackupExecution::where('id', $id)`).
pub async fn execution_by_id(db: &Db, id: i64) -> DbResult<Option<(Record, Record)>> {
    let row: Option<(String, String)> = sqlx::query_as(
        "SELECT row_to_json(e)::text, row_to_json(b)::text FROM scheduled_database_backup_executions e JOIN scheduled_database_backups b ON b.id = e.scheduled_database_backup_id WHERE e.id = $1",
    )
    .bind(id)
    .fetch_optional(db.pool())
    .await?;
    Ok(row.and_then(|(e, b)| {
        Some((
            serde_json::from_str(&e).ok()?,
            serde_json::from_str(&b).ok()?,
        ))
    }))
}

/// `$execution->selectedS3Storages` as `s3DownloadUrl` walks them: live uploaded copies first.
pub async fn live_replica_storages(
    db: &Db,
    execution_id: i64,
    team_id: TeamId,
) -> DbResult<Vec<Record>> {
    let ids = parts::live_replicas(db, execution_id).await?;
    let mut out = Vec::new();
    for (_, storage) in ids {
        if let Some(id) = storage
            && let Some(row) = parts::s3_by_id(db, id).await?
            && row.get("team_id").and_then(Value::as_i64) == Some(team_id)
        {
            out.push(row);
        }
    }
    Ok(out)
}

/// The usable S3 storages of a team, plus those already selected (`availableS3StorageIds`).
pub async fn available_s3_storages(
    db: &Db,
    team_id: i64,
    selected: &[i64],
) -> DbResult<Vec<Record>> {
    let texts: Vec<String> = sqlx::query_scalar(
        "SELECT row_to_json(s)::text FROM s3_storages s WHERE s.team_id = $1 AND (s.is_usable = true OR s.id = ANY($2)) ORDER BY s.name",
    )
    .bind(team_id)
    .bind(selected)
    .fetch_all(db.pool())
    .await?;
    Ok(decode_all(texts))
}
