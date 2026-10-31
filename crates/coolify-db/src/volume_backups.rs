//! `ScheduledVolumeBackup`, its executions and S3 copies (`scheduled_volume_backups`,
//! `scheduled_volume_backup_executions`, `volume_backup_s3_replicas`,
//! `scheduled_volume_backup_s3_storage`).

use chrono::NaiveDateTime;
use coolify_domain::{TeamId, new_public_id};
use serde_json::Value;

use crate::records::Record;
use crate::{Db, DbResult};

pub const PERSISTENT_MORPH: &str = r"App\Models\LocalPersistentVolume";
pub const FILE_MORPH: &str = r"App\Models\LocalFileVolume";

fn decode(texts: Vec<String>) -> Vec<Record> {
    texts
        .iter()
        .filter_map(|t| serde_json::from_str::<Record>(t).ok())
        .collect()
}

/// `findResource`: the `(morph, id)` pairs whose storages count, or `None` when not in the team.
pub async fn application_resource(
    db: &Db,
    team_id: TeamId,
    uuid: &str,
) -> DbResult<Option<Vec<(String, i64)>>> {
    let id: Option<i64> = sqlx::query_scalar(
        "SELECT a.id FROM applications a JOIN environments e ON e.id = a.environment_id JOIN projects p ON p.id = e.project_id
         WHERE a.uuid = $1 AND p.team_id = $2 AND a.deleted_at IS NULL",
    )
    .bind(uuid)
    .bind(team_id)
    .fetch_optional(db.pool())
    .await?;
    Ok(id.map(|id| vec![(r"App\Models\Application".to_owned(), id)]))
}

/// `$resource->applications->concat($resource->databases)` of a service of the team.
pub async fn service_resource(
    db: &Db,
    team_id: TeamId,
    uuid: &str,
) -> DbResult<Option<Vec<(String, i64)>>> {
    let id: Option<i64> = sqlx::query_scalar(
        "SELECT s.id FROM services s JOIN environments e ON e.id = s.environment_id JOIN projects p ON p.id = e.project_id
         WHERE s.uuid = $1 AND p.team_id = $2 AND s.deleted_at IS NULL",
    )
    .bind(uuid)
    .bind(team_id)
    .fetch_optional(db.pool())
    .await?;
    let Some(id) = id else {
        return Ok(None);
    };
    let mut out: Vec<(String, i64)> = Vec::new();
    let apps: Vec<i64> =
        sqlx::query_scalar("SELECT id FROM service_applications WHERE service_id = $1 ORDER BY id")
            .bind(id)
            .fetch_all(db.pool())
            .await?;
    out.extend(
        apps.into_iter()
            .map(|a| (r"App\Models\ServiceApplication".to_owned(), a)),
    );
    let dbs: Vec<i64> =
        sqlx::query_scalar("SELECT id FROM service_databases WHERE service_id = $1 ORDER BY id")
            .bind(id)
            .fetch_all(db.pool())
            .await?;
    out.extend(
        dbs.into_iter()
            .map(|a| (r"App\Models\ServiceDatabase".to_owned(), a)),
    );
    Ok(Some(out))
}

/// A storage a schedule can point at.
#[derive(Debug, Clone)]
pub struct BackupTarget {
    /// `LocalPersistentVolume` or `LocalFileVolume`.
    pub morph: &'static str,
    pub row: Record,
}

/// `findStorage`.
pub async fn find_storage(
    db: &Db,
    resources: &[(String, i64)],
    storage_uuid: &str,
) -> DbResult<Option<BackupTarget>> {
    for (morph, id) in resources {
        let text: Option<String> = sqlx::query_scalar(
            "SELECT row_to_json(v)::text FROM local_persistent_volumes v WHERE v.resource_type = $1 AND v.resource_id = $2 AND v.uuid = $3 ORDER BY v.id LIMIT 1",
        )
        .bind(morph)
        .bind(id)
        .bind(storage_uuid)
        .fetch_optional(db.pool())
        .await?;
        if let Some(row) = text.and_then(|t| serde_json::from_str(&t).ok()) {
            return Ok(Some(BackupTarget {
                morph: PERSISTENT_MORPH,
                row,
            }));
        }
        let text: Option<String> = sqlx::query_scalar(
            "SELECT row_to_json(v)::text FROM local_file_volumes v WHERE v.resource_type = $1 AND v.resource_id = $2 AND v.uuid = $3 ORDER BY v.id LIMIT 1",
        )
        .bind(morph)
        .bind(id)
        .bind(storage_uuid)
        .fetch_optional(db.pool())
        .await?;
        if let Some(row) = text.and_then(|t| serde_json::from_str(&t).ok()) {
            return Ok(Some(BackupTarget {
                morph: FILE_MORPH,
                row,
            }));
        }
    }
    Ok(None)
}

/// `$backup->backupable`.
pub async fn target_of(db: &Db, backup: &Record) -> DbResult<Option<BackupTarget>> {
    let id = backup
        .get("backupable_id")
        .and_then(Value::as_i64)
        .unwrap_or(-1);
    let (morph, table) =
        if backup.get("backupable_type").and_then(Value::as_str) == Some(FILE_MORPH) {
            (FILE_MORPH, "local_file_volumes")
        } else {
            (PERSISTENT_MORPH, "local_persistent_volumes")
        };
    let text: Option<String> = sqlx::query_scalar(&format!(
        "SELECT row_to_json(v)::text FROM {table} v WHERE v.id = $1"
    ))
    .bind(id)
    .fetch_optional(db.pool())
    .await?;
    Ok(text
        .and_then(|t| serde_json::from_str(&t).ok())
        .map(|row| BackupTarget { morph, row }))
}

/// `$backup->targetResource()?->name` and the path of `resourceUrl()`: a service's own link for its parts.
pub async fn resource_link(
    db: &Db,
    morph: &str,
    id: i64,
) -> DbResult<(Option<String>, Option<String>)> {
    let (table, segment, service) = match morph {
        r"App\Models\ServiceApplication" => ("service_applications", "service", true),
        r"App\Models\ServiceDatabase" => ("service_databases", "service", true),
        r"App\Models\Application" => ("applications", "application", false),
        r"App\Models\StandalonePostgresql" => ("standalone_postgresqls", "database", false),
        r"App\Models\StandaloneRedis" => ("standalone_redis", "database", false),
        r"App\Models\StandaloneMongodb" => ("standalone_mongodbs", "database", false),
        r"App\Models\StandaloneMysql" => ("standalone_mysqls", "database", false),
        r"App\Models\StandaloneMariadb" => ("standalone_mariadbs", "database", false),
        r"App\Models\StandaloneKeydb" => ("standalone_keydbs", "database", false),
        r"App\Models\StandaloneDragonfly" => ("standalone_dragonflies", "database", false),
        r"App\Models\StandaloneClickhouse" => ("standalone_clickhouses", "database", false),
        r"App\Models\StandaloneSqlite" => ("standalone_sqlites", "database", false),
        _ => return Ok((None, None)),
    };
    let query = if service {
        format!(
            "SELECT x.name::text, p.uuid::text, e.uuid::text, s.uuid::text FROM {table} x
             JOIN services s ON s.id = x.service_id JOIN environments e ON e.id = s.environment_id JOIN projects p ON p.id = e.project_id WHERE x.id = $1"
        )
    } else {
        format!(
            "SELECT x.name::text, p.uuid::text, e.uuid::text, x.uuid::text FROM {table} x
             JOIN environments e ON e.id = x.environment_id JOIN projects p ON p.id = e.project_id WHERE x.id = $1"
        )
    };
    let row: Option<(Option<String>, String, String, String)> = sqlx::query_as(&query)
        .bind(id)
        .fetch_optional(db.pool())
        .await?;
    Ok(match row {
        Some((name, project, environment, uuid)) => (
            name,
            Some(format!(
                "/project/{project}/environment/{environment}/{segment}/{uuid}"
            )),
        ),
        None => (None, None),
    })
}

/// `$backup->server()`: the server id and, for an application, its uuid (`workdir()`).
pub async fn resource_server(db: &Db, morph: &str, id: i64) -> DbResult<Option<i64>> {
    let query = match morph {
        r"App\Models\ServiceApplication" => {
            "SELECT s.server_id FROM services s JOIN service_applications x ON x.service_id = s.id WHERE x.id = $1"
                .to_owned()
        }
        r"App\Models\ServiceDatabase" => {
            "SELECT s.server_id FROM services s JOIN service_databases x ON x.service_id = s.id WHERE x.id = $1"
                .to_owned()
        }
        _ => {
            let table = match morph {
                r"App\Models\Application" => "applications",
                r"App\Models\StandalonePostgresql" => "standalone_postgresqls",
                r"App\Models\StandaloneRedis" => "standalone_redis",
                r"App\Models\StandaloneMongodb" => "standalone_mongodbs",
                r"App\Models\StandaloneMysql" => "standalone_mysqls",
                r"App\Models\StandaloneMariadb" => "standalone_mariadbs",
                r"App\Models\StandaloneKeydb" => "standalone_keydbs",
                r"App\Models\StandaloneDragonfly" => "standalone_dragonflies",
                r"App\Models\StandaloneClickhouse" => "standalone_clickhouses",
                r"App\Models\StandaloneSqlite" => "standalone_sqlites",
                _ => return Ok(None),
            };
            format!(
                "SELECT coalesce(d.server_id, w.server_id) FROM {table} x
                 LEFT JOIN standalone_dockers d ON d.id = x.destination_id AND x.destination_type = 'App\\Models\\StandaloneDocker'
                 LEFT JOIN swarm_dockers w ON w.id = x.destination_id AND x.destination_type = 'App\\Models\\SwarmDocker'
                 WHERE x.id = $1"
            )
        }
    };
    Ok(sqlx::query_scalar(&query)
        .bind(id)
        .fetch_optional(db.pool())
        .await?)
}

pub async fn application_uuid(db: &Db, id: i64) -> DbResult<Option<String>> {
    Ok(
        sqlx::query_scalar("SELECT uuid FROM applications WHERE id = $1")
            .bind(id)
            .fetch_optional(db.pool())
            .await?,
    )
}

pub async fn team_name(db: &Db, id: i64) -> DbResult<Option<String>> {
    Ok(sqlx::query_scalar("SELECT name FROM teams WHERE id = $1")
        .bind(id)
        .fetch_optional(db.pool())
        .await?)
}

/// `$storage->scheduledBackups()->first()`.
pub async fn schedule_of(db: &Db, morph: &str, target_id: i64) -> DbResult<Option<Record>> {
    let text: Option<String> = sqlx::query_scalar(
        "SELECT row_to_json(b)::text FROM scheduled_volume_backups b WHERE b.backupable_type = $1 AND b.backupable_id = $2 ORDER BY b.id LIMIT 1",
    )
    .bind(morph)
    .bind(target_id)
    .fetch_optional(db.pool())
    .await?;
    Ok(text.and_then(|t| serde_json::from_str(&t).ok()))
}

pub async fn schedule_by_id(db: &Db, id: i64) -> DbResult<Option<Record>> {
    let text: Option<String> = sqlx::query_scalar(
        "SELECT row_to_json(b)::text FROM scheduled_volume_backups b WHERE b.id = $1",
    )
    .bind(id)
    .fetch_optional(db.pool())
    .await?;
    Ok(text.and_then(|t| serde_json::from_str(&t).ok()))
}

/// The attributes `persistSchedule` writes. `None` keeps the column (or its default on create).
#[derive(Debug, Clone)]
pub struct ScheduleValues {
    pub team_id: TeamId,
    pub frequency: String,
    pub enabled: bool,
    pub save_s3: bool,
    pub disable_local_backup: bool,
    pub stop_during_backup: bool,
    pub s3_storage_id: Option<i64>,
    pub retention_amount_locally: i64,
    pub retention_days_locally: i64,
    pub retention_max_storage_locally: f64,
    pub retention_amount_s3: i64,
    pub retention_days_s3: i64,
    pub retention_max_storage_s3: f64,
    pub timeout: Option<i64>,
    pub missing_backup_notification_days: Option<i64>,
}

/// `updateOrCreate([], $attributes)`: `(id, uuid, created)`.
pub async fn upsert_schedule(
    db: &Db,
    morph: &str,
    target_id: i64,
    v: &ScheduleValues,
    now: NaiveDateTime,
) -> DbResult<(i64, String, bool)> {
    db.ensure_writable()?;
    let existing = schedule_of(db, morph, target_id).await?;
    if let Some(row) = existing {
        let id = row.get("id").and_then(Value::as_i64).unwrap_or_default();
        sqlx::query(
            "UPDATE scheduled_volume_backups SET team_id=$1, frequency=$2, enabled=$3, save_s3=$4, disable_local_backup=$5, stop_during_backup=$6, s3_storage_id=$7,
               retention_amount_locally=$8, retention_days_locally=$9, retention_max_storage_locally=$10::float8::numeric, retention_amount_s3=$11, retention_days_s3=$12, retention_max_storage_s3=$13::float8::numeric,
               timeout=coalesce($14, timeout), missing_backup_notification_days=coalesce($15, missing_backup_notification_days), updated_at=$16 WHERE id=$17",
        )
        .bind(v.team_id)
        .bind(&v.frequency)
        .bind(v.enabled)
        .bind(v.save_s3)
        .bind(v.disable_local_backup)
        .bind(v.stop_during_backup)
        .bind(v.s3_storage_id)
        .bind(v.retention_amount_locally as i32)
        .bind(v.retention_days_locally as i32)
        .bind(v.retention_max_storage_locally)
        .bind(v.retention_amount_s3 as i32)
        .bind(v.retention_days_s3 as i32)
        .bind(v.retention_max_storage_s3)
        .bind(v.timeout.map(|t| t as i32))
        .bind(v.missing_backup_notification_days.map(|t| t as i32))
        .bind(now)
        .bind(id)
        .execute(db.pool())
        .await?;
        let uuid = row
            .get("uuid")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        return Ok((id, uuid, false));
    }
    let uuid = new_public_id();
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO scheduled_volume_backups (uuid, backupable_type, backupable_id, team_id, frequency, enabled, save_s3, disable_local_backup, stop_during_backup, s3_storage_id,
           retention_amount_locally, retention_days_locally, retention_max_storage_locally, retention_amount_s3, retention_days_s3, retention_max_storage_s3,
           timeout, missing_backup_notification_days, created_at, updated_at)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13::float8::numeric,$14,$15,$16::float8::numeric,coalesce($17,36000),coalesce($18,0),$19,$19) RETURNING id",
    )
    .bind(&uuid)
    .bind(morph)
    .bind(target_id)
    .bind(v.team_id)
    .bind(&v.frequency)
    .bind(v.enabled)
    .bind(v.save_s3)
    .bind(v.disable_local_backup)
    .bind(v.stop_during_backup)
    .bind(v.s3_storage_id)
    .bind(v.retention_amount_locally as i32)
    .bind(v.retention_days_locally as i32)
    .bind(v.retention_max_storage_locally)
    .bind(v.retention_amount_s3 as i32)
    .bind(v.retention_days_s3 as i32)
    .bind(v.retention_max_storage_s3)
    .bind(v.timeout.map(|t| t as i32))
    .bind(v.missing_backup_notification_days.map(|t| t as i32))
    .bind(now)
    .fetch_one(db.pool())
    .await?;
    Ok((id, uuid, true))
}

/// `syncS3Storages`.
pub async fn sync_storages(db: &Db, id: i64, ids: &[i64], now: NaiveDateTime) -> DbResult<()> {
    db.ensure_writable()?;
    let mut tx = db.pool().begin().await?;
    sqlx::query(
        "DELETE FROM scheduled_volume_backup_s3_storage WHERE scheduled_volume_backup_id = $1 AND NOT (s3_storage_id = ANY($2))",
    )
    .bind(id)
    .bind(ids)
    .execute(&mut *tx)
    .await?;
    for s in ids {
        sqlx::query(
            "INSERT INTO scheduled_volume_backup_s3_storage (scheduled_volume_backup_id, s3_storage_id, created_at, updated_at)
             SELECT $1, $2, $3, $3 WHERE NOT EXISTS (SELECT 1 FROM scheduled_volume_backup_s3_storage WHERE scheduled_volume_backup_id = $1 AND s3_storage_id = $2)",
        )
        .bind(id)
        .bind(s)
        .bind(now)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// The usable S3 storages of a team with these uuids (`validateUpsertRequest`).
pub async fn usable_storages(db: &Db, team_id: TeamId, uuids: &[String]) -> DbResult<Vec<Record>> {
    let texts: Vec<String> = sqlx::query_scalar(
        "SELECT row_to_json(s)::text FROM s3_storages s WHERE s.team_id = $1 AND s.is_usable = true AND s.uuid = ANY($2) ORDER BY s.id",
    )
    .bind(team_id)
    .bind(uuids)
    .fetch_all(db.pool())
    .await?;
    Ok(decode(texts))
}

/// `selectedS3Storages()`: the primary first, or the legacy `s3` when none is linked.
pub async fn selected_storages(db: &Db, backup: &Record) -> DbResult<Vec<Record>> {
    let id = backup.get("id").and_then(Value::as_i64).unwrap_or_default();
    let primary = backup.get("s3_storage_id").and_then(Value::as_i64);
    let texts: Vec<String> = sqlx::query_scalar(
        "SELECT row_to_json(s)::text FROM s3_storages s JOIN scheduled_volume_backup_s3_storage p ON p.s3_storage_id = s.id WHERE p.scheduled_volume_backup_id = $1 ORDER BY s.id",
    )
    .bind(id)
    .fetch_all(db.pool())
    .await?;
    let mut rows = decode(texts);
    rows.sort_by_key(|r| i32::from(r.get("id").and_then(Value::as_i64) != primary));
    if rows.is_empty()
        && let Some(p) = primary
        && let Some(row) = crate::database_parts::s3_by_id(db, p).await?
    {
        rows.push(row);
    }
    Ok(rows)
}

pub async fn has_active_executions(db: &Db, backup_id: i64) -> DbResult<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM scheduled_volume_backup_executions WHERE scheduled_volume_backup_id = $1 AND (status = 'running' OR stop_recovery_pending OR s3_cleanup_pending))",
    )
    .bind(backup_id)
    .fetch_one(db.pool())
    .await?)
}

pub async fn has_running_execution(db: &Db, backup_id: i64) -> DbResult<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM scheduled_volume_backup_executions WHERE scheduled_volume_backup_id = $1 AND status = 'running')",
    )
    .bind(backup_id)
    .fetch_one(db.pool())
    .await?)
}

/// A new execution: `(id, uuid)`.
pub async fn create_execution(
    db: &Db,
    backup_id: i64,
    s3_storage_id: Option<i64>,
    now: NaiveDateTime,
) -> DbResult<(i64, String)> {
    db.ensure_writable()?;
    let uuid = new_public_id();
    let id = sqlx::query_scalar(
        "INSERT INTO scheduled_volume_backup_executions (uuid, scheduled_volume_backup_id, s3_storage_id, created_at, updated_at) VALUES ($1, $2, $3, $4, $4) RETURNING id",
    )
    .bind(&uuid)
    .bind(backup_id)
    .bind(s3_storage_id)
    .bind(now)
    .fetch_one(db.pool())
    .await?;
    Ok((id, uuid))
}

pub async fn create_replica(
    db: &Db,
    execution_id: i64,
    s3_storage_id: i64,
    now: NaiveDateTime,
) -> DbResult<()> {
    sqlx::query(
        "INSERT INTO volume_backup_s3_replicas (execution_id, s3_storage_id, created_at, updated_at) VALUES ($1, $2, $3, $3)",
    )
    .bind(execution_id)
    .bind(s3_storage_id)
    .bind(now)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// One typed column change of an execution.
#[derive(Debug, Clone)]
pub enum ExecutionField {
    Filename(Option<String>),
    Status(&'static str),
    Message(Option<String>),
    Size(i64),
    LocalDeleted(bool),
    StopPending(bool),
    CleanupPending(bool),
    ClearStopContainers,
    Finished,
}

/// `recordSuccess` (`None`) and `recordFailure` of `VolumeBackupRecoveryJob`, and the first update of
/// `retryRecovery` (`Some((None, false))`, which keeps the last attempt).
pub async fn set_recovery_state(
    db: &Db,
    id: i64,
    outcome: Option<(Option<&str>, bool)>,
    now: NaiveDateTime,
) -> DbResult<()> {
    db.ensure_writable()?;
    match outcome {
        Some((None, needs_attention)) => {
            sqlx::query(
                "UPDATE scheduled_volume_backup_executions SET recovery_needs_attention = $1, updated_at = $2 WHERE id = $3",
            )
            .bind(needs_attention)
            .bind(now)
            .bind(id)
            .execute(db.pool())
            .await?;
        }
        Some((Some(category), needs_attention)) => {
            sqlx::query(
                "UPDATE scheduled_volume_backup_executions SET recovery_last_attempt_at = $1, recovery_error = $2, recovery_needs_attention = $3, updated_at = $1 WHERE id = $4",
            )
            .bind(now)
            .bind(category)
            .bind(needs_attention)
            .bind(id)
            .execute(db.pool())
            .await?;
        }
        None => {
            sqlx::query(
                "UPDATE scheduled_volume_backup_executions SET recovery_last_attempt_at = NULL, recovery_error = NULL, recovery_needs_attention = false, updated_at = $1 WHERE id = $2",
            )
            .bind(now)
            .bind(id)
            .execute(db.pool())
            .await?;
        }
    }
    Ok(())
}

pub async fn update_execution(
    db: &Db,
    id: i64,
    fields: &[ExecutionField],
    now: NaiveDateTime,
) -> DbResult<()> {
    db.ensure_writable()?;
    for f in fields {
        let q = match f {
            ExecutionField::Filename(v) => sqlx::query(
                "UPDATE scheduled_volume_backup_executions SET filename = $1, updated_at = $2 WHERE id = $3",
            )
            .bind(v.clone()),
            ExecutionField::Status(v) => sqlx::query(
                "UPDATE scheduled_volume_backup_executions SET status = $1, updated_at = $2 WHERE id = $3",
            )
            .bind(*v),
            ExecutionField::Message(v) => sqlx::query(
                "UPDATE scheduled_volume_backup_executions SET message = $1, updated_at = $2 WHERE id = $3",
            )
            .bind(v.clone()),
            ExecutionField::Size(v) => sqlx::query(
                "UPDATE scheduled_volume_backup_executions SET size = $1, updated_at = $2 WHERE id = $3",
            )
            .bind(*v),
            ExecutionField::LocalDeleted(v) => sqlx::query(
                "UPDATE scheduled_volume_backup_executions SET local_storage_deleted = $1, updated_at = $2 WHERE id = $3",
            )
            .bind(*v),
            ExecutionField::StopPending(v) => sqlx::query(
                "UPDATE scheduled_volume_backup_executions SET stop_recovery_pending = $1, updated_at = $2 WHERE id = $3",
            )
            .bind(*v),
            ExecutionField::CleanupPending(v) => sqlx::query(
                "UPDATE scheduled_volume_backup_executions SET s3_cleanup_pending = $1, updated_at = $2 WHERE id = $3",
            )
            .bind(*v),
            ExecutionField::ClearStopContainers => sqlx::query(
                "UPDATE scheduled_volume_backup_executions SET stop_container_ids = NULL, updated_at = $1 WHERE id = $2",
            ),
            ExecutionField::Finished => sqlx::query(
                "UPDATE scheduled_volume_backup_executions SET finished_at = $1, updated_at = $1 WHERE id = $2",
            ),
        };
        q.bind(now).bind(id).execute(db.pool()).await?;
    }
    Ok(())
}

/// `$backup->executions()` (latest first), for retention and deletion.
pub async fn executions_of(db: &Db, backup_id: i64) -> DbResult<Vec<Record>> {
    let texts: Vec<String> = sqlx::query_scalar(
        "SELECT row_to_json(e)::text FROM scheduled_volume_backup_executions e WHERE e.scheduled_volume_backup_id = $1 ORDER BY e.created_at DESC, e.id DESC",
    )
    .bind(backup_id)
    .fetch_all(db.pool())
    .await?;
    Ok(decode(texts))
}

/// The copies of an execution: `(replica id, storage id, uploaded, deleted)`.
pub async fn replicas_of(
    db: &Db,
    execution_id: i64,
) -> DbResult<Vec<(i64, Option<i64>, Option<bool>, bool)>> {
    Ok(sqlx::query_as(
        "SELECT id, s3_storage_id, s3_uploaded, s3_storage_deleted FROM volume_backup_s3_replicas WHERE execution_id = $1 ORDER BY id",
    )
    .bind(execution_id)
    .fetch_all(db.pool())
    .await?)
}

pub async fn set_replica_result(
    db: &Db,
    replica_id: i64,
    uploaded: bool,
    message: Option<&str>,
    now: NaiveDateTime,
) -> DbResult<()> {
    sqlx::query(
        "UPDATE volume_backup_s3_replicas SET s3_uploaded = $1, message = $2, updated_at = $3 WHERE id = $4",
    )
    .bind(uploaded)
    .bind(message)
    .bind(now)
    .bind(replica_id)
    .execute(db.pool())
    .await?;
    Ok(())
}

pub async fn set_all_replicas_uploaded(
    db: &Db,
    execution_id: i64,
    now: NaiveDateTime,
) -> DbResult<()> {
    sqlx::query(
        "UPDATE volume_backup_s3_replicas SET s3_uploaded = true, updated_at = $1 WHERE execution_id = $2",
    )
    .bind(now)
    .bind(execution_id)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// `->whereNull('s3_uploaded')->update(['s3_uploaded' => false])`.
pub async fn fail_open_replicas(db: &Db, execution_id: i64, now: NaiveDateTime) -> DbResult<()> {
    sqlx::query(
        "UPDATE volume_backup_s3_replicas SET s3_uploaded = false, updated_at = $1 WHERE execution_id = $2 AND s3_uploaded IS NULL",
    )
    .bind(now)
    .bind(execution_id)
    .execute(db.pool())
    .await?;
    Ok(())
}

pub async fn mark_replica_deleted(db: &Db, replica_id: i64, now: NaiveDateTime) -> DbResult<()> {
    sqlx::query(
        "UPDATE volume_backup_s3_replicas SET s3_storage_deleted = true, updated_at = $1 WHERE id = $2",
    )
    .bind(now)
    .bind(replica_id)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// `refreshS3Summary`: uploaded when every copy uploaded, deleted when every uploaded copy is deleted.
pub async fn refresh_summary(db: &Db, execution_id: i64, now: NaiveDateTime) -> DbResult<()> {
    let replicas = replicas_of(db, execution_id).await?;
    if replicas.is_empty() {
        return Ok(());
    }
    let all = replicas.iter().all(|r| r.2 == Some(true));
    let uploaded: Vec<_> = replicas.iter().filter(|r| r.2 == Some(true)).collect();
    let deleted = !uploaded.is_empty() && uploaded.iter().all(|r| r.3);
    sqlx::query(
        "UPDATE scheduled_volume_backup_executions SET s3_uploaded = $1, s3_storage_deleted = $2, updated_at = $3 WHERE id = $4",
    )
    .bind(all)
    .bind(deleted)
    .bind(now)
    .bind(execution_id)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// `->whereKey(...)->update(['local_storage_deleted' => true])`.
pub async fn mark_local_deleted(db: &Db, ids: &[i64], now: NaiveDateTime) -> DbResult<()> {
    sqlx::query(
        "UPDATE scheduled_volume_backup_executions SET local_storage_deleted = true, updated_at = $1 WHERE id = ANY($2)",
    )
    .bind(now)
    .bind(ids)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// The last step of `removeExpiredBackups`: rows without any copy left.
pub async fn delete_exhausted(db: &Db, backup_id: i64) -> DbResult<()> {
    sqlx::query(
        "DELETE FROM scheduled_volume_backup_executions e WHERE e.scheduled_volume_backup_id = $1 AND e.local_storage_deleted AND NOT e.stop_recovery_pending AND NOT e.s3_cleanup_pending
         AND NOT EXISTS (SELECT 1 FROM volume_backup_s3_replicas r WHERE r.execution_id = e.id AND r.s3_uploaded = true AND r.s3_storage_deleted = false)",
    )
    .bind(backup_id)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// `$target->scheduledBackups()->firstOrNew()` with `Create::submit`'s fill: `(uuid, created, changed fields)`.
pub async fn set_frequency(
    db: &Db,
    morph: &str,
    target_id: i64,
    team_id: TeamId,
    frequency: &str,
    now: NaiveDateTime,
) -> DbResult<(String, bool, Vec<&'static str>)> {
    db.ensure_writable()?;
    if let Some(row) = schedule_of(db, morph, target_id).await? {
        let id = row.get("id").and_then(Value::as_i64).unwrap_or_default();
        let uuid = row
            .get("uuid")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let mut changed = Vec::new();
        if row.get("frequency").and_then(Value::as_str) != Some(frequency) {
            changed.push("frequency");
        }
        if row.get("enabled").and_then(Value::as_bool) != Some(true) {
            changed.push("enabled");
        }
        if changed.is_empty() {
            return Ok((uuid, false, changed));
        }
        sqlx::query(
            "UPDATE scheduled_volume_backups SET frequency = $1, enabled = true, team_id = $2, updated_at = $3 WHERE id = $4",
        )
        .bind(frequency)
        .bind(team_id)
        .bind(now)
        .bind(id)
        .execute(db.pool())
        .await?;
        return Ok((uuid, false, changed));
    }
    let uuid = new_public_id();
    sqlx::query(
        "INSERT INTO scheduled_volume_backups (uuid, team_id, backupable_type, backupable_id, frequency, enabled, created_at, updated_at)
         VALUES ($1, $2, $3, $4, $5, true, $6, $6)",
    )
    .bind(&uuid)
    .bind(team_id)
    .bind(morph)
    .bind(target_id)
    .bind(frequency)
    .bind(now)
    .execute(db.pool())
    .await?;
    Ok((uuid, true, Vec::new()))
}

/// `toggleEnabled`.
pub async fn set_enabled(db: &Db, id: i64, enabled: bool, now: NaiveDateTime) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query("UPDATE scheduled_volume_backups SET enabled = $1, updated_at = $2 WHERE id = $3")
        .bind(enabled)
        .bind(now)
        .bind(id)
        .execute(db.pool())
        .await?;
    Ok(())
}

/// `$backup->executions()->paginate()`: the page and the total count, newest first.
pub async fn executions_page(
    db: &Db,
    backup_id: i64,
    skip: i64,
    take: i64,
) -> DbResult<(Vec<Record>, i64)> {
    let total: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM scheduled_volume_backup_executions WHERE scheduled_volume_backup_id = $1",
    )
    .bind(backup_id)
    .fetch_one(db.pool())
    .await?;
    let texts: Vec<String> = sqlx::query_scalar(
        "SELECT row_to_json(e)::text FROM scheduled_volume_backup_executions e WHERE e.scheduled_volume_backup_id = $1 ORDER BY e.created_at DESC, e.id DESC LIMIT $2 OFFSET $3",
    )
    .bind(backup_id)
    .bind(take)
    .bind(skip)
    .fetch_all(db.pool())
    .await?;
    Ok((decode(texts), total))
}

/// `s3Replicas.s3`: the copies of an execution with the storage's name.
pub async fn replica_rows(db: &Db, execution_id: i64) -> DbResult<Vec<Record>> {
    let texts: Vec<String> = sqlx::query_scalar(
        "SELECT row_to_json(x)::text FROM (SELECT r.id, r.s3_uploaded, r.s3_storage_deleted, r.message, s.name AS s3_name FROM volume_backup_s3_replicas r LEFT JOIN s3_storages s ON s.id = r.s3_storage_id WHERE r.execution_id = $1 ORDER BY r.id) x",
    )
    .bind(execution_id)
    .fetch_all(db.pool())
    .await?;
    Ok(decode(texts))
}

/// `cleanupFailed`.
pub async fn delete_failed_executions(db: &Db, backup_id: i64) -> DbResult<u64> {
    db.ensure_writable()?;
    Ok(sqlx::query(
        "DELETE FROM scheduled_volume_backup_executions WHERE scheduled_volume_backup_id = $1 AND status = 'failed' AND stop_recovery_pending = false AND s3_cleanup_pending = false AND (filename IS NULL OR local_storage_deleted = true)",
    )
    .bind(backup_id)
    .execute(db.pool())
    .await?
    .rows_affected())
}

/// `cleanupDeleted`.
pub async fn delete_local_deleted_executions(db: &Db, backup_id: i64) -> DbResult<u64> {
    db.ensure_writable()?;
    Ok(sqlx::query(
        "DELETE FROM scheduled_volume_backup_executions e WHERE e.scheduled_volume_backup_id = $1 AND e.local_storage_deleted = true AND e.stop_recovery_pending = false AND e.s3_cleanup_pending = false
         AND (e.s3_storage_deleted = true OR e.s3_uploaded IS NULL OR e.s3_uploaded = false)
         AND NOT EXISTS (SELECT 1 FROM volume_backup_s3_replicas r WHERE r.execution_id = e.id AND r.s3_uploaded = true AND r.s3_storage_deleted = false)",
    )
    .bind(backup_id)
    .execute(db.pool())
    .await?
    .rows_affected())
}

pub async fn delete_execution(db: &Db, id: i64) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query("DELETE FROM scheduled_volume_backup_executions WHERE id = $1")
        .bind(id)
        .execute(db.pool())
        .await?;
    Ok(())
}

pub async fn delete_schedule(db: &Db, id: i64) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query("DELETE FROM scheduled_volume_backups WHERE id = $1")
        .bind(id)
        .execute(db.pool())
        .await?;
    Ok(())
}

/// An execution by id, with its schedule.
pub async fn execution_by_id(db: &Db, id: i64) -> DbResult<Option<(Record, Record)>> {
    let row: Option<(String, String)> = sqlx::query_as(
        "SELECT row_to_json(e)::text, row_to_json(b)::text FROM scheduled_volume_backup_executions e JOIN scheduled_volume_backups b ON b.id = e.scheduled_volume_backup_id WHERE e.id = $1",
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

/// The usable storages of a team that hold a live copy of an execution (`s3DownloadUrl`).
pub async fn live_storages(db: &Db, execution_id: i64, team_id: i64) -> DbResult<Vec<Record>> {
    let texts: Vec<String> = sqlx::query_scalar(
        "SELECT row_to_json(s)::text FROM volume_backup_s3_replicas r JOIN s3_storages s ON s.id = r.s3_storage_id
         WHERE r.execution_id = $1 AND r.s3_uploaded = true AND r.s3_storage_deleted = false AND s.team_id = $2 AND s.is_usable = true ORDER BY r.id",
    )
    .bind(execution_id)
    .bind(team_id)
    .fetch_all(db.pool())
    .await?;
    Ok(decode(texts))
}
