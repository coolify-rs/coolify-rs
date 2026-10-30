//! What belongs to a standalone database besides its row: tags, file and
//! persistent storages, S3 storages, scheduled backups with their executions
//! and S3 replicas, SSL certificates. Rows are read with `row_to_json`, like
//! the other record modules, and presented with the specs below.

use chrono::NaiveDateTime;
use coolify_domain::{TeamId, new_public_id};
use serde_json::Value;

use crate::records::{ModelSpec, Record, parse_row};
use crate::servers::ColumnValue;
use crate::{Db, DbError, DbResult};

/// `$hidden`: `key`, `secret`.
pub static S3_STORAGE: ModelSpec = ModelSpec {
    table: "s3_storages",
    known: &[
        "id",
        "uuid",
        "name",
        "description",
        "region",
        "key",
        "secret",
        "bucket",
        "endpoint",
        "team_id",
        "created_at",
        "updated_at",
        "is_usable",
        "unusable_email_sent",
    ],
    hidden: &[],
    timestamps: &["created_at", "updated_at"],
    datetimes: &["created_at", "updated_at"],
};

pub static SCHEDULED_DATABASE_BACKUP: ModelSpec = ModelSpec {
    table: "scheduled_database_backups",
    known: &[
        "id",
        "description",
        "uuid",
        "enabled",
        "save_s3",
        "frequency",
        "database_backup_retention_amount_locally",
        "database_type",
        "database_id",
        "s3_storage_id",
        "team_id",
        "created_at",
        "updated_at",
        "databases_to_backup",
        "dump_all",
        "database_backup_retention_days_locally",
        "database_backup_retention_max_storage_locally",
        "database_backup_retention_amount_s3",
        "database_backup_retention_days_s3",
        "database_backup_retention_max_storage_s3",
        "timeout",
        "disable_local_backup",
        "missing_backup_notification_days",
        "last_execution_at",
    ],
    hidden: &[],
    timestamps: &["created_at", "updated_at", "last_execution_at"],
    datetimes: &["created_at", "updated_at", "last_execution_at"],
};

pub static BACKUP_EXECUTION: ModelSpec = ModelSpec {
    table: "scheduled_database_backup_executions",
    known: &[
        "id",
        "uuid",
        "status",
        "message",
        "size",
        "filename",
        "scheduled_database_backup_id",
        "created_at",
        "updated_at",
        "database_name",
        "finished_at",
        "local_storage_deleted",
        "s3_storage_deleted",
        "s3_uploaded",
    ],
    hidden: &[],
    timestamps: &["created_at", "updated_at", "finished_at"],
    datetimes: &["created_at", "updated_at", "finished_at"],
};

/// `$hidden`: `content` (the API shows it to `read:sensitive` tokens).
pub static LOCAL_FILE_VOLUME: ModelSpec = ModelSpec {
    table: "local_file_volumes",
    known: &[
        "id",
        "uuid",
        "fs_path",
        "mount_path",
        "content",
        "resource_type",
        "resource_id",
        "created_at",
        "updated_at",
        "is_directory",
        "chown",
        "chmod",
        "is_based_on_git",
        "is_preview_suffix_enabled",
        "is_host_file",
    ],
    hidden: &[],
    timestamps: &["created_at", "updated_at"],
    datetimes: &["created_at", "updated_at"],
};

pub static LOCAL_PERSISTENT_VOLUME: ModelSpec = ModelSpec {
    table: "local_persistent_volumes",
    known: &[
        "id",
        "name",
        "mount_path",
        "host_path",
        "container_id",
        "resource_type",
        "resource_id",
        "created_at",
        "updated_at",
        "is_preview_suffix_enabled",
        "uuid",
        "standalone_sqlite_id",
        "ignores_compose_driver_options",
    ],
    hidden: &[],
    timestamps: &["created_at", "updated_at"],
    datetimes: &["created_at", "updated_at"],
};

fn decode(text: &str, what: &str) -> DbResult<Record> {
    parse_row(text).map_err(|e| DbError::Schema(format!("decoding {what} row: {e}")))
}

fn rows(texts: Vec<String>, what: &str) -> DbResult<Vec<Record>> {
    texts.iter().map(|t| decode(t, what)).collect()
}

pub fn id_of(r: &Record) -> i64 {
    r.get("id").and_then(Value::as_i64).unwrap_or_default()
}

pub fn str_of<'a>(r: &'a Record, k: &str) -> Option<&'a str> {
    r.get(k).and_then(Value::as_str)
}

pub fn bool_of(r: &Record, k: &str) -> bool {
    r.get(k).and_then(Value::as_bool).unwrap_or(false)
}

/// One `UPDATE {table} SET ...` with `updated_at`, for columns that changed.
pub async fn update_row(
    db: &Db,
    table: &str,
    id: i64,
    values: &[(&'static str, ColumnValue)],
    now: NaiveDateTime,
) -> DbResult<()> {
    if values.is_empty() {
        return Ok(());
    }
    db.ensure_writable()?;
    let sets = crate::servers::set_clause(values, 3);
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
    let sql = format!("UPDATE {table} SET {sets}, updated_at = $1 WHERE id = $2 AND ({changed})");
    crate::servers::bind_all(sqlx::query(&sql).bind(now).bind(id), values)
        .execute(db.pool())
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Tags
// ---------------------------------------------------------------------------

/// `$resource->tags`: `(id, uuid, name, created_at, updated_at)`.
pub async fn tags_of(db: &Db, morph: &str, id: i64) -> DbResult<Vec<Record>> {
    let texts: Vec<String> = sqlx::query_scalar(
        "SELECT row_to_json(t)::text FROM tags t JOIN taggables x ON x.tag_id = t.id \
         WHERE x.taggable_type = $1 AND x.taggable_id = $2 ORDER BY t.id",
    )
    .bind(morph)
    .bind(id)
    .fetch_all(db.pool())
    .await?;
    rows(texts, "tag")
}

/// `Tag::where('team_id', ...)->where('uuid', ...)->first()`: the id.
pub async fn tag_id(db: &Db, team_id: TeamId, uuid: &str) -> DbResult<Option<i64>> {
    Ok(
        sqlx::query_scalar("SELECT id FROM tags WHERE team_id = $1 AND uuid = $2 LIMIT 1")
            .bind(team_id)
            .bind(uuid)
            .fetch_optional(db.pool())
            .await?,
    )
}

/// `attachTagsToResource`: `Tag::createOrFirst` + `syncWithoutDetaching`.
pub async fn attach_tags(
    db: &Db,
    team_id: TeamId,
    morph: &str,
    id: i64,
    names: &[String],
    now: NaiveDateTime,
) -> DbResult<()> {
    db.ensure_writable()?;
    for name in names {
        let found: Option<i64> =
            sqlx::query_scalar("SELECT id FROM tags WHERE team_id = $1 AND name = $2 LIMIT 1")
                .bind(team_id)
                .bind(name)
                .fetch_optional(db.pool())
                .await?;
        let tag = match found {
            Some(t) => t,
            None => {
                sqlx::query_scalar(
                    "INSERT INTO tags (uuid, name, team_id, created_at, updated_at) VALUES ($1, $2, $3, $4, $4) RETURNING id",
                )
                .bind(new_public_id())
                .bind(name)
                .bind(team_id)
                .bind(now)
                .fetch_one(db.pool())
                .await?
            }
        };
        let linked: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM taggables WHERE tag_id = $1 AND taggable_id = $2 AND taggable_type = $3)",
        )
        .bind(tag)
        .bind(id)
        .bind(morph)
        .fetch_one(db.pool())
        .await?;
        if !linked {
            link_tag(db, tag, morph, id).await?;
        }
    }
    Ok(())
}

async fn link_tag(db: &Db, tag: i64, morph: &str, id: i64) -> DbResult<()> {
    sqlx::query("INSERT INTO taggables (tag_id, taggable_id, taggable_type) VALUES ($1, $2, $3)")
        .bind(tag)
        .bind(id)
        .bind(morph)
        .execute(db.pool())
        .await?;
    Ok(())
}

pub async fn has_tag(db: &Db, morph: &str, id: i64, tag: i64) -> DbResult<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM taggables WHERE tag_id = $1 AND taggable_id = $2 AND taggable_type = $3)",
    )
    .bind(tag)
    .bind(id)
    .bind(morph)
    .fetch_one(db.pool())
    .await?)
}

pub async fn detach_tag(db: &Db, morph: &str, id: i64, tag: i64) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(
        "DELETE FROM taggables WHERE tag_id = $1 AND taggable_id = $2 AND taggable_type = $3",
    )
    .bind(tag)
    .bind(id)
    .bind(morph)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// `$newDatabase->tags()->attach($tag->id)` for every tag of the source.
pub async fn copy_tags(db: &Db, morph: &str, from: i64, to: i64) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(
        "INSERT INTO taggables (tag_id, taggable_id, taggable_type) \
         SELECT tag_id, $1, taggable_type FROM taggables WHERE taggable_type = $2 AND taggable_id = $3",
    )
    .bind(to)
    .bind(morph)
    .bind(from)
    .execute(db.pool())
    .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Storages
// ---------------------------------------------------------------------------

async fn list_for(db: &Db, table: &str, morph: &str, id: i64) -> DbResult<Vec<Record>> {
    let texts: Vec<String> = sqlx::query_scalar(&format!(
        "SELECT row_to_json(t)::text FROM {table} t WHERE resource_type = $1 AND resource_id = $2 ORDER BY id"
    ))
    .bind(morph)
    .bind(id)
    .fetch_all(db.pool())
    .await?;
    rows(texts, table)
}

/// `$database->persistentStorages` by id.
pub async fn persistent_storages(db: &Db, morph: &str, id: i64) -> DbResult<Vec<Record>> {
    list_for(db, "local_persistent_volumes", morph, id).await
}

/// `$database->fileStorages` by id (`content` still encrypted).
pub async fn file_storages(db: &Db, morph: &str, id: i64) -> DbResult<Vec<Record>> {
    list_for(db, "local_file_volumes", morph, id).await
}

/// `LocalPersistentVolume::create([...])`. Returns the new row.
pub async fn create_persistent_storage(
    db: &Db,
    morph: &str,
    resource_id: i64,
    name: &str,
    mount_path: &str,
    now: NaiveDateTime,
) -> DbResult<Record> {
    db.ensure_writable()?;
    let text: String = sqlx::query_scalar(
        "INSERT INTO local_persistent_volumes (name, mount_path, resource_id, resource_type, uuid, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $6) RETURNING row_to_json(local_persistent_volumes)::text",
    )
    .bind(name)
    .bind(mount_path)
    .bind(resource_id)
    .bind(morph)
    .bind(new_public_id())
    .bind(now)
    .fetch_one(db.pool())
    .await?;
    decode(&text, "local_persistent_volumes")
}

/// A new `local_file_volumes` row (`content_encrypted` with the `encrypted` cast).
#[derive(Debug, Default)]
pub struct NewFileVolume {
    pub fs_path: String,
    pub mount_path: String,
    pub content_encrypted: Option<String>,
    pub is_directory: bool,
    pub is_host_file: bool,
    pub chmod: Option<String>,
}

pub async fn create_file_storage(
    db: &Db,
    morph: &str,
    resource_id: i64,
    n: &NewFileVolume,
    now: NaiveDateTime,
) -> DbResult<Record> {
    db.ensure_writable()?;
    let text: String = sqlx::query_scalar(
        "INSERT INTO local_file_volumes (uuid, fs_path, mount_path, content, is_directory, is_host_file, chmod, resource_id, resource_type, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $10) RETURNING row_to_json(local_file_volumes)::text",
    )
    .bind(new_public_id())
    .bind(&n.fs_path)
    .bind(&n.mount_path)
    .bind(&n.content_encrypted)
    .bind(n.is_directory)
    .bind(n.is_host_file)
    .bind(&n.chmod)
    .bind(resource_id)
    .bind(morph)
    .bind(now)
    .fetch_one(db.pool())
    .await?;
    decode(&text, "local_file_volumes")
}

pub async fn find_row(db: &Db, table: &str, id: i64) -> DbResult<Option<Record>> {
    let text: Option<String> = sqlx::query_scalar(&format!(
        "SELECT row_to_json(t)::text FROM {table} t WHERE id = $1"
    ))
    .bind(id)
    .fetch_optional(db.pool())
    .await?;
    text.map(|t| decode(&t, table)).transpose()
}

pub async fn delete_row(db: &Db, table: &str, id: i64) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(&format!("DELETE FROM {table} WHERE id = $1"))
        .bind(id)
        .execute(db.pool())
        .await?;
    Ok(())
}

/// `$volume->scheduledBackups()->exists()`.
pub async fn volume_has_backups(db: &Db, morph: &str, id: i64) -> DbResult<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM scheduled_volume_backups WHERE backupable_type = $1 AND backupable_id = $2)",
    )
    .bind(morph)
    .bind(id)
    .fetch_one(db.pool())
    .await?)
}

pub const LOCAL_FILE_VOLUME_MORPH: &str = r"App\Models\LocalFileVolume";
pub const LOCAL_PERSISTENT_VOLUME_MORPH: &str = r"App\Models\LocalPersistentVolume";

/// Whether another resource's volume entry was connected from this SQLite database
/// (`isSharedWithAnotherResource`, `connectedVolumes`).
pub async fn sqlite_volume_connected(db: &Db, sqlite_id: i64, name: &str) -> DbResult<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM local_persistent_volumes WHERE standalone_sqlite_id = $1 AND name = $2)",
    )
    .bind(sqlite_id)
    .bind(name)
    .fetch_one(db.pool())
    .await?)
}

/// `connectedVolumes`: the names of the applications that mount a volume of this SQLite database.
pub async fn sqlite_connected_applications(db: &Db, sqlite_id: i64) -> DbResult<Vec<String>> {
    Ok(sqlx::query_scalar(
        "SELECT a.name FROM local_persistent_volumes v JOIN applications a ON a.id = v.resource_id \
         WHERE v.standalone_sqlite_id = $1 AND v.resource_type = 'App\\Models\\Application' AND a.deleted_at IS NULL ORDER BY v.id",
    )
    .bind(sqlite_id)
    .fetch_all(db.pool())
    .await?)
}

// ---------------------------------------------------------------------------
// S3 storages
// ---------------------------------------------------------------------------

/// `S3Storage::ownedByCurrentTeamAPI($teamId)`: ordered by name.
pub async fn s3_list(db: &Db, team_id: TeamId) -> DbResult<Vec<Record>> {
    let texts: Vec<String> = sqlx::query_scalar(
        "SELECT row_to_json(s)::text FROM s3_storages s WHERE team_id = $1 ORDER BY name",
    )
    .bind(team_id)
    .fetch_all(db.pool())
    .await?;
    rows(texts, "s3_storages")
}

pub async fn s3_find(db: &Db, team_id: TeamId, uuid: &str) -> DbResult<Option<Record>> {
    let text: Option<String> = sqlx::query_scalar(
        "SELECT row_to_json(s)::text FROM s3_storages s WHERE team_id = $1 AND uuid = $2 ORDER BY name LIMIT 1",
    )
    .bind(team_id)
    .bind(uuid)
    .fetch_optional(db.pool())
    .await?;
    text.map(|t| decode(&t, "s3_storages")).transpose()
}

pub async fn s3_by_id(db: &Db, id: i64) -> DbResult<Option<Record>> {
    find_row(db, "s3_storages", id).await
}

/// `exists:s3_storages,uuid`.
pub async fn s3_uuid_exists(db: &Db, uuid: &str) -> DbResult<bool> {
    Ok(
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM s3_storages WHERE uuid = $1)")
            .bind(uuid)
            .fetch_one(db.pool())
            .await?,
    )
}

#[derive(Debug)]
pub struct NewS3Storage<'a> {
    pub team_id: TeamId,
    pub name: &'a str,
    pub description: Option<&'a str>,
    pub endpoint: Option<&'a str>,
    pub bucket: &'a str,
    pub region: &'a str,
    pub key_encrypted: &'a str,
    pub secret_encrypted: &'a str,
    pub is_usable: bool,
}

pub async fn s3_create(db: &Db, n: &NewS3Storage<'_>, now: NaiveDateTime) -> DbResult<String> {
    db.ensure_writable()?;
    let uuid = new_public_id();
    sqlx::query(
        "INSERT INTO s3_storages (uuid, name, description, region, key, secret, bucket, endpoint, team_id, is_usable, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $11)",
    )
    .bind(&uuid)
    .bind(n.name)
    .bind(n.description)
    .bind(n.region)
    .bind(n.key_encrypted)
    .bind(n.secret_encrypted)
    .bind(n.bucket)
    .bind(n.endpoint)
    .bind(n.team_id)
    .bind(n.is_usable)
    .bind(now)
    .execute(db.pool())
    .await?;
    Ok(uuid)
}

/// `$storage->save()` after `testConnection`: the usable flags only.
pub async fn s3_set_usable(db: &Db, id: i64, usable: bool, now: NaiveDateTime) -> DbResult<()> {
    let mut values = vec![("is_usable", ColumnValue::Bool(usable))];
    if usable {
        values.push(("unusable_email_sent", ColumnValue::Bool(false)));
    }
    update_row(db, "s3_storages", id, &values, now).await
}

/// `S3Storage::deleting` + delete: `detachFromBackups`, then the row.
pub async fn s3_delete(db: &Db, id: i64, now: NaiveDateTime) -> DbResult<()> {
    db.ensure_writable()?;
    let mut tx = db.pool().begin().await?;
    // Schedules that use the storage move their primary destination, or stop S3 backups.
    let schedules: Vec<(i64, Option<i64>, bool)> = sqlx::query_as(
        "SELECT b.id, b.s3_storage_id, b.save_s3 FROM scheduled_database_backups b WHERE b.s3_storage_id = $1 \
         OR EXISTS (SELECT 1 FROM scheduled_database_backup_s3_storage p WHERE p.scheduled_database_backup_id = b.id AND p.s3_storage_id = $1) ORDER BY b.id",
    )
    .bind(id)
    .fetch_all(&mut *tx)
    .await?;
    for (backup, primary, save_s3) in schedules {
        sqlx::query("DELETE FROM scheduled_database_backup_s3_storage WHERE scheduled_database_backup_id = $1 AND s3_storage_id = $2")
            .bind(backup)
            .bind(id)
            .execute(&mut *tx)
            .await?;
        let remaining: Vec<i64> = sqlx::query_scalar(
            "SELECT s3_storage_id FROM scheduled_database_backup_s3_storage WHERE scheduled_database_backup_id = $1 ORDER BY id",
        )
        .bind(backup)
        .fetch_all(&mut *tx)
        .await?;
        let new_primary = primary
            .filter(|p| remaining.contains(p))
            .or_else(|| remaining.first().copied());
        let save = save_s3 && new_primary.is_some();
        sqlx::query(
            "UPDATE scheduled_database_backups SET s3_storage_id = $1, save_s3 = $2, updated_at = $3 WHERE id = $4",
        )
        .bind(new_primary)
        .bind(save)
        .bind(now)
        .bind(backup)
        .execute(&mut *tx)
        .await?;
    }
    let executions: Vec<i64> = sqlx::query_scalar(
        "SELECT DISTINCT execution_id FROM database_backup_s3_replicas WHERE s3_storage_id = $1 AND NOT s3_storage_deleted",
    )
    .bind(id)
    .fetch_all(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE database_backup_s3_replicas SET s3_storage_deleted = true, updated_at = $1 WHERE s3_storage_id = $2 AND NOT s3_storage_deleted",
    )
    .bind(now)
    .bind(id)
    .execute(&mut *tx)
    .await?;
    for execution in executions {
        refresh_s3_summary_tx(&mut tx, execution, now).await?;
    }
    sqlx::query(
        "UPDATE volume_backup_s3_replicas SET s3_storage_deleted = true, updated_at = $1 WHERE s3_storage_id = $2 AND NOT s3_storage_deleted",
    )
    .bind(now)
    .bind(id)
    .execute(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM s3_storages WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Scheduled database backups
// ---------------------------------------------------------------------------

/// `ScheduledDatabaseBackup::ownedByCurrentTeamAPI($teamId)` of one database, latest first.
pub async fn backups_of(db: &Db, team_id: TeamId, morph: &str, id: i64) -> DbResult<Vec<Record>> {
    let texts: Vec<String> = sqlx::query_scalar(
        "SELECT row_to_json(b)::text FROM scheduled_database_backups b WHERE team_id = $1 AND database_type = $2 AND database_id = $3 ORDER BY created_at DESC",
    )
    .bind(team_id)
    .bind(morph)
    .bind(id)
    .fetch_all(db.pool())
    .await?;
    rows(texts, "scheduled_database_backups")
}

pub async fn backup_of(
    db: &Db,
    team_id: TeamId,
    morph: &str,
    id: i64,
    uuid: &str,
) -> DbResult<Option<Record>> {
    let text: Option<String> = sqlx::query_scalar(
        "SELECT row_to_json(b)::text FROM scheduled_database_backups b WHERE team_id = $1 AND database_type = $2 AND database_id = $3 AND uuid = $4 ORDER BY created_at DESC LIMIT 1",
    )
    .bind(team_id)
    .bind(morph)
    .bind(id)
    .bind(uuid)
    .fetch_optional(db.pool())
    .await?;
    text.map(|t| decode(&t, "scheduled_database_backups"))
        .transpose()
}

/// `selectedS3Storages` and the UUID lists of a schedule: rows of its destinations, by name.
pub async fn backup_destinations(db: &Db, backup_id: i64) -> DbResult<Vec<Record>> {
    let texts: Vec<String> = sqlx::query_scalar(
        "SELECT row_to_json(s)::text FROM s3_storages s JOIN scheduled_database_backup_s3_storage p ON p.s3_storage_id = s.id \
         WHERE p.scheduled_database_backup_id = $1 ORDER BY s.name",
    )
    .bind(backup_id)
    .fetch_all(db.pool())
    .await?;
    rows(texts, "s3_storages")
}

/// A new `scheduled_database_backups` row: only the fields of the request are given.
#[derive(Debug)]
pub struct NewBackup {
    pub team_id: TeamId,
    pub morph: &'static str,
    pub database_id: i64,
    pub fields: Vec<(&'static str, ColumnValue)>,
    pub numeric_fields: Vec<(&'static str, f64)>,
}

pub async fn create_backup(db: &Db, n: &NewBackup, now: NaiveDateTime) -> DbResult<(i64, String)> {
    db.ensure_writable()?;
    let uuid = new_public_id();
    let mut names: Vec<String> = n.fields.iter().map(|(c, _)| format!("\"{c}\"")).collect();
    let mut placeholders: Vec<String> = n
        .fields
        .iter()
        .enumerate()
        .map(|(i, (_, v))| match v {
            ColumnValue::Int(_) => format!("${}::bigint", i + 6),
            _ => format!("${}", i + 6),
        })
        .collect();
    for (i, (c, _)) in n.numeric_fields.iter().enumerate() {
        names.push(format!("\"{c}\""));
        placeholders.push(format!("${}::float8", n.fields.len() + 6 + i));
    }
    let (extra_names, extra_values) = if names.is_empty() {
        (String::new(), String::new())
    } else {
        (
            format!(", {}", names.join(", ")),
            format!(", {}", placeholders.join(", ")),
        )
    };
    let sql = format!(
        "INSERT INTO scheduled_database_backups (uuid, team_id, database_type, database_id, created_at, updated_at{extra_names}) \
         VALUES ($1, $2, $3, $4, $5, $5{extra_values}) RETURNING id"
    );
    let mut q = sqlx::query_scalar::<_, i64>(&sql)
        .bind(&uuid)
        .bind(n.team_id)
        .bind(n.morph)
        .bind(n.database_id)
        .bind(now);
    for (_, v) in &n.fields {
        q = match v {
            ColumnValue::Text(t) => q.bind(t.clone()),
            ColumnValue::Int(i) => q.bind(*i),
            ColumnValue::Bool(b) => q.bind(*b),
            ColumnValue::Time(t) => q.bind(*t),
        };
    }
    for (_, f) in &n.numeric_fields {
        q = q.bind(*f);
    }
    let id = q.fetch_one(db.pool()).await?;
    Ok((id, uuid))
}

/// `$backup->update([...])`: `numeric` holds the float columns.
pub async fn update_backup(
    db: &Db,
    id: i64,
    values: &[(&'static str, ColumnValue)],
    numeric: &[(&'static str, f64)],
    now: NaiveDateTime,
) -> DbResult<()> {
    update_row(db, "scheduled_database_backups", id, values, now).await?;
    for (c, f) in numeric {
        sqlx::query(&format!(
            "UPDATE scheduled_database_backups SET \"{c}\" = $1, updated_at = $2 WHERE id = $3 AND \"{c}\" IS DISTINCT FROM $1::float8::numeric"
        ))
        .bind(*f)
        .bind(now)
        .bind(id)
        .execute(db.pool())
        .await?;
    }
    Ok(())
}

/// `syncS3Storages`: replaces the destinations; the primary stays when still selected.
pub async fn sync_backup_storages(
    db: &Db,
    backup_id: i64,
    current_primary: Option<i64>,
    ids: &[i64],
    now: NaiveDateTime,
) -> DbResult<()> {
    db.ensure_writable()?;
    let mut tx = db.pool().begin().await?;
    sqlx::query(
        "DELETE FROM scheduled_database_backup_s3_storage WHERE scheduled_database_backup_id = $1 AND NOT (s3_storage_id = ANY($2))",
    )
    .bind(backup_id)
    .bind(ids)
    .execute(&mut *tx)
    .await?;
    for id in ids {
        sqlx::query(
            "INSERT INTO scheduled_database_backup_s3_storage (scheduled_database_backup_id, s3_storage_id, created_at, updated_at) \
             SELECT $1, $2, $3, $3 WHERE NOT EXISTS (SELECT 1 FROM scheduled_database_backup_s3_storage WHERE scheduled_database_backup_id = $1 AND s3_storage_id = $2)",
        )
        .bind(backup_id)
        .bind(id)
        .bind(now)
        .execute(&mut *tx)
        .await?;
    }
    let primary = current_primary
        .filter(|p| ids.contains(p))
        .or_else(|| ids.first().copied());
    sqlx::query(
        "UPDATE scheduled_database_backups SET s3_storage_id = $1, updated_at = CASE WHEN s3_storage_id IS DISTINCT FROM $1 THEN $2 ELSE updated_at END WHERE id = $3",
    )
    .bind(primary)
    .bind(now)
    .bind(backup_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

/// `$backup->executions()`: last execution first.
pub async fn executions_of(db: &Db, backup_id: i64) -> DbResult<Vec<Record>> {
    let texts: Vec<String> = sqlx::query_scalar(
        "SELECT row_to_json(e)::text FROM scheduled_database_backup_executions e WHERE scheduled_database_backup_id = $1 ORDER BY created_at DESC",
    )
    .bind(backup_id)
    .fetch_all(db.pool())
    .await?;
    rows(texts, "scheduled_database_backup_executions")
}

/// `latest_log`: the newest execution.
pub async fn latest_execution(db: &Db, backup_id: i64) -> DbResult<Option<Record>> {
    let text: Option<String> = sqlx::query_scalar(
        "SELECT row_to_json(e)::text FROM scheduled_database_backup_executions e WHERE scheduled_database_backup_id = $1 ORDER BY created_at DESC, id DESC LIMIT 1",
    )
    .bind(backup_id)
    .fetch_optional(db.pool())
    .await?;
    text.map(|t| decode(&t, "scheduled_database_backup_executions"))
        .transpose()
}

pub async fn execution_of(db: &Db, backup_id: i64, uuid: &str) -> DbResult<Option<Record>> {
    let text: Option<String> = sqlx::query_scalar(
        "SELECT row_to_json(e)::text FROM scheduled_database_backup_executions e WHERE scheduled_database_backup_id = $1 AND uuid = $2 ORDER BY created_at DESC LIMIT 1",
    )
    .bind(backup_id)
    .bind(uuid)
    .fetch_optional(db.pool())
    .await?;
    text.map(|t| decode(&t, "scheduled_database_backup_executions"))
        .transpose()
}

pub async fn execution_uuid_exists(db: &Db, uuid: &str) -> DbResult<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM scheduled_database_backup_executions WHERE uuid = $1)",
    )
    .bind(uuid)
    .fetch_one(db.pool())
    .await?)
}

/// `ScheduledDatabaseBackupExecution::create([...])` and the `created` hook that sets
/// `last_execution_at` on the schedule.
#[allow(clippy::too_many_arguments)]
pub async fn create_execution(
    db: &Db,
    backup_id: i64,
    uuid: &str,
    database_name: Option<&str>,
    filename: Option<&str>,
    status: Option<&str>,
    message: Option<&str>,
    now: NaiveDateTime,
) -> DbResult<i64> {
    db.ensure_writable()?;
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO scheduled_database_backup_executions (uuid, scheduled_database_backup_id, database_name, filename, status, message, local_storage_deleted, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, COALESCE($5, 'running'), $6, false, $7, $7) RETURNING id",
    )
    .bind(uuid)
    .bind(backup_id)
    .bind(database_name)
    .bind(filename)
    .bind(status)
    .bind(message)
    .bind(now)
    .fetch_one(db.pool())
    .await?;
    sqlx::query("UPDATE scheduled_database_backups SET last_execution_at = $1, updated_at = $1 WHERE id = $2")
        .bind(now)
        .bind(backup_id)
        .execute(db.pool())
        .await?;
    Ok(id)
}

pub async fn update_execution(
    db: &Db,
    id: i64,
    values: &[(&'static str, ColumnValue)],
    now: NaiveDateTime,
) -> DbResult<()> {
    update_row(db, "scheduled_database_backup_executions", id, values, now).await
}

/// `markStaleExecutionsAsFailed`.
pub async fn fail_stale_executions(
    db: &Db,
    backup_id: i64,
    timeout_seconds: i64,
    now: NaiveDateTime,
) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(
        "UPDATE scheduled_database_backup_executions SET status = 'failed', message = 'Marked as failed - backup execution exceeded maximum allowed time', finished_at = $1, updated_at = $1 \
         WHERE scheduled_database_backup_id = $2 AND status = 'running' AND created_at < $1 - make_interval(secs => $3)",
    )
    .bind(now)
    .bind(backup_id)
    .bind(timeout_seconds as f64)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// `$backup_log->s3Replicas()->create([...])`.
pub async fn create_replica(
    db: &Db,
    execution_id: i64,
    s3_storage_id: i64,
    uploaded: bool,
    message: Option<&str>,
    now: NaiveDateTime,
) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(
        "INSERT INTO database_backup_s3_replicas (execution_id, s3_storage_id, s3_uploaded, message, created_at, updated_at) VALUES ($1, $2, $3, $4, $5, $5)",
    )
    .bind(execution_id)
    .bind(s3_storage_id)
    .bind(uploaded)
    .bind(message)
    .bind(now)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// `(replica id, execution id, s3 storage id)` of the live copies of an execution.
pub async fn live_replicas(db: &Db, execution_id: i64) -> DbResult<Vec<(i64, Option<i64>)>> {
    Ok(sqlx::query_as(
        "SELECT id, s3_storage_id FROM database_backup_s3_replicas WHERE execution_id = $1 AND s3_uploaded = true AND NOT s3_storage_deleted ORDER BY id",
    )
    .bind(execution_id)
    .fetch_all(db.pool())
    .await?)
}

pub async fn mark_replica_deleted(db: &Db, replica_id: i64, now: NaiveDateTime) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(
        "UPDATE database_backup_s3_replicas SET s3_storage_deleted = true, updated_at = $1 WHERE id = $2",
    )
    .bind(now)
    .bind(replica_id)
    .execute(db.pool())
    .await?;
    Ok(())
}

async fn refresh_s3_summary_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    execution_id: i64,
    now: NaiveDateTime,
) -> DbResult<()> {
    let replicas: Vec<(Option<bool>, bool)> = sqlx::query_as(
        "SELECT s3_uploaded, s3_storage_deleted FROM database_backup_s3_replicas WHERE execution_id = $1",
    )
    .bind(execution_id)
    .fetch_all(&mut **tx)
    .await?;
    if replicas.is_empty() {
        return Ok(());
    }
    let all_uploaded = replicas.iter().all(|(u, _)| *u == Some(true));
    let uploaded: Vec<_> = replicas.iter().filter(|(u, _)| *u == Some(true)).collect();
    let deleted = !uploaded.is_empty() && uploaded.iter().all(|(_, d)| *d);
    sqlx::query(
        "UPDATE scheduled_database_backup_executions SET s3_uploaded = $1, s3_storage_deleted = $2, updated_at = $3 \
         WHERE id = $4 AND (s3_uploaded IS DISTINCT FROM $1 OR s3_storage_deleted IS DISTINCT FROM $2)",
    )
    .bind(all_uploaded)
    .bind(deleted)
    .bind(now)
    .bind(execution_id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// `refreshS3Summary`.
pub async fn refresh_s3_summary(db: &Db, execution_id: i64, now: NaiveDateTime) -> DbResult<()> {
    db.ensure_writable()?;
    let mut tx = db.pool().begin().await?;
    refresh_s3_summary_tx(&mut tx, execution_id, now).await?;
    tx.commit().await?;
    Ok(())
}

/// Live S3 replicas of a schedule's successful executions: `(replica id, execution id, storage id)`.
pub async fn schedule_live_replicas(
    db: &Db,
    backup_id: i64,
) -> DbResult<Vec<(i64, i64, Option<i64>)>> {
    Ok(sqlx::query_as(
        "SELECT r.id, r.execution_id, r.s3_storage_id FROM database_backup_s3_replicas r \
         JOIN scheduled_database_backup_executions e ON e.id = r.execution_id \
         WHERE e.scheduled_database_backup_id = $1 AND e.status = 'success' AND r.s3_uploaded = true AND NOT r.s3_storage_deleted ORDER BY r.id",
    )
    .bind(backup_id)
    .fetch_all(db.pool())
    .await?)
}

/// `deleteOldBackupsLocally`'s candidates: successful executions with a local file, newest first.
/// With `only_uploaded` (`disable_local_backup`) only those with a live S3 copy.
pub async fn successful_local_executions(
    db: &Db,
    backup_id: i64,
    only_uploaded: bool,
) -> DbResult<Vec<Record>> {
    let filter = if only_uploaded {
        " AND EXISTS (SELECT 1 FROM database_backup_s3_replicas r WHERE r.execution_id = e.id AND r.s3_uploaded = true AND NOT r.s3_storage_deleted)"
    } else {
        ""
    };
    let texts: Vec<String> = sqlx::query_scalar(&format!(
        "SELECT row_to_json(e)::text FROM scheduled_database_backup_executions e \
         WHERE scheduled_database_backup_id = $1 AND status = 'success' AND NOT local_storage_deleted{filter} ORDER BY created_at DESC, id DESC"
    ))
    .bind(backup_id)
    .fetch_all(db.pool())
    .await?;
    rows(texts, "scheduled_database_backup_executions")
}

pub async fn executions_by_ids(db: &Db, backup_id: i64, ids: &[i64]) -> DbResult<Vec<Record>> {
    let texts: Vec<String> = sqlx::query_scalar(
        "SELECT row_to_json(e)::text FROM scheduled_database_backup_executions e \
         WHERE scheduled_database_backup_id = $1 AND id = ANY($2) ORDER BY created_at DESC, id DESC",
    )
    .bind(backup_id)
    .bind(ids)
    .fetch_all(db.pool())
    .await?;
    rows(texts, "scheduled_database_backup_executions")
}

pub async fn mark_local_deleted(
    db: &Db,
    backup_id: i64,
    ids: &[i64],
    now: NaiveDateTime,
) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(
        "UPDATE scheduled_database_backup_executions SET local_storage_deleted = true, updated_at = $1 WHERE scheduled_database_backup_id = $2 AND id = ANY($3)",
    )
    .bind(now)
    .bind(backup_id)
    .bind(ids)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// `->where('local_storage_deleted', true)->withoutLiveS3Copies()->delete()`.
pub async fn delete_exhausted_executions(db: &Db, backup_id: i64) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(
        "DELETE FROM scheduled_database_backup_executions e WHERE scheduled_database_backup_id = $1 AND local_storage_deleted \
         AND NOT EXISTS (SELECT 1 FROM database_backup_s3_replicas r WHERE r.execution_id = e.id AND r.s3_uploaded = true AND NOT r.s3_storage_deleted)",
    )
    .bind(backup_id)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// `$execution->delete()`.
pub async fn delete_execution(db: &Db, id: i64) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query("DELETE FROM scheduled_database_backup_executions WHERE id = $1")
        .bind(id)
        .execute(db.pool())
        .await?;
    Ok(())
}

/// `$backup->delete()`.
pub async fn delete_backup(db: &Db, id: i64) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query("DELETE FROM scheduled_database_backups WHERE id = $1")
        .bind(id)
        .execute(db.pool())
        .await?;
    Ok(())
}

/// Every enabled schedule (the scheduler's `ScheduledJobManager` input): `(uuid, frequency, team id)`.
pub async fn enabled_backups(db: &Db) -> DbResult<Vec<(i64, String, String)>> {
    Ok(sqlx::query_as(
        "SELECT id, uuid, frequency FROM scheduled_database_backups WHERE enabled ORDER BY id",
    )
    .fetch_all(db.pool())
    .await?)
}

pub async fn backup_by_id(db: &Db, id: i64) -> DbResult<Option<Record>> {
    find_row(db, "scheduled_database_backups", id).await
}

/// `(kind table morph, database id)` lookup for a schedule is `database_type` + `database_id`.
pub async fn copy_backups_to(
    db: &Db,
    morph: &str,
    from: i64,
    to: i64,
    team_id: TeamId,
    now: NaiveDateTime,
) -> DbResult<()> {
    db.ensure_writable()?;
    let schedules: Vec<i64> = sqlx::query_scalar(
        "SELECT id FROM scheduled_database_backups WHERE database_type = $1 AND database_id = $2 ORDER BY id",
    )
    .bind(morph)
    .bind(from)
    .fetch_all(db.pool())
    .await?;
    for backup in schedules {
        let new_id: i64 = sqlx::query_scalar(
            "INSERT INTO scheduled_database_backups (description, uuid, enabled, save_s3, frequency, database_backup_retention_amount_locally, database_type, database_id, s3_storage_id, team_id, created_at, updated_at, databases_to_backup, dump_all, database_backup_retention_days_locally, database_backup_retention_max_storage_locally, database_backup_retention_amount_s3, database_backup_retention_days_s3, database_backup_retention_max_storage_s3, timeout, disable_local_backup, missing_backup_notification_days) \
             SELECT description, $1, enabled, save_s3, frequency, database_backup_retention_amount_locally, $2, $3, s3_storage_id, $4, $5, $5, databases_to_backup, dump_all, database_backup_retention_days_locally, database_backup_retention_max_storage_locally, database_backup_retention_amount_s3, database_backup_retention_days_s3, database_backup_retention_max_storage_s3, timeout, disable_local_backup, missing_backup_notification_days \
             FROM scheduled_database_backups WHERE id = $6 RETURNING id",
        )
        .bind(new_public_id())
        .bind(morph)
        .bind(to)
        .bind(team_id)
        .bind(now)
        .bind(backup)
        .fetch_one(db.pool())
        .await?;
        sqlx::query(
            "INSERT INTO scheduled_database_backup_s3_storage (scheduled_database_backup_id, s3_storage_id, created_at, updated_at) \
             SELECT $1, p.s3_storage_id, $2, $2 FROM scheduled_database_backup_s3_storage p JOIN s3_storages s ON s.id = p.s3_storage_id \
             WHERE p.scheduled_database_backup_id = $3 AND s.team_id = $4",
        )
        .bind(new_id)
        .bind(now)
        .bind(backup)
        .bind(team_id)
        .execute(db.pool())
        .await?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Clone
// ---------------------------------------------------------------------------

/// The columns of a standalone database table except the ones `replicate()` skips.
async fn replicable_columns(db: &Db, table: &str, skip: &[&str]) -> DbResult<Vec<String>> {
    let cols: Vec<String> = sqlx::query_scalar(
        "SELECT column_name::text FROM information_schema.columns WHERE table_name = $1 AND table_schema = current_schema() ORDER BY ordinal_position",
    )
    .bind(table)
    .fetch_all(db.pool())
    .await?;
    Ok(cols
        .into_iter()
        .filter(|c| !skip.contains(&c.as_str()))
        .collect())
}

/// `$database->replicate([...])->fill([...])->save()`: the new row's `(id, uuid)`.
#[allow(clippy::too_many_arguments)]
pub async fn replicate_database(
    db: &Db,
    table: &str,
    id: i64,
    uuid: &str,
    name: &str,
    destination_id: i64,
    destination_morph: &str,
    now: NaiveDateTime,
) -> DbResult<i64> {
    db.ensure_writable()?;
    let cols = replicable_columns(
        db,
        table,
        &[
            "id",
            "uuid",
            "name",
            "status",
            "started_at",
            "destination_id",
            "destination_type",
            "created_at",
            "updated_at",
            // `status` is set, so the model's saving hook moves it.
            "last_online_at",
        ],
    )
    .await?;
    let list = cols
        .iter()
        .map(|c| format!("\"{c}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "INSERT INTO {table} (uuid, name, status, started_at, destination_id, destination_type, created_at, updated_at, last_online_at, {list}) \
         SELECT $1, $2, 'exited:unhealthy', NULL, $3, $4, $5, $5, $5, {list} FROM {table} WHERE id = $6 RETURNING id"
    );
    Ok(sqlx::query_scalar(&sql)
        .bind(uuid)
        .bind(name)
        .bind(destination_id)
        .bind(destination_morph)
        .bind(now)
        .bind(id)
        .fetch_one(db.pool())
        .await?)
}

/// `$newDatabase->persistentStorages()->delete()`: the row hook created default volumes.
/// `$volume->replicate(...)->fill(['name' => ..., 'resource_id' => ...])->save()`.
pub async fn replicate_persistent_volume(
    db: &Db,
    id: i64,
    name: &str,
    resource_id: i64,
    now: NaiveDateTime,
) -> DbResult<()> {
    db.ensure_writable()?;
    let cols = replicable_columns(
        db,
        "local_persistent_volumes",
        &[
            "id",
            "uuid",
            "name",
            "resource_id",
            "created_at",
            "updated_at",
            "ignores_compose_driver_options",
        ],
    )
    .await?;
    let list = cols
        .iter()
        .map(|c| format!("\"{c}\""))
        .collect::<Vec<_>>()
        .join(", ");
    sqlx::query(&format!(
        "INSERT INTO local_persistent_volumes (uuid, name, resource_id, created_at, updated_at, ignores_compose_driver_options, {list}) \
         SELECT $1, $2, $3, $4, $4, false, {list} FROM local_persistent_volumes WHERE id = $5"
    ))
    .bind(new_public_id())
    .bind(name)
    .bind(resource_id)
    .bind(now)
    .bind(id)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// `$storage->replicate(['id', 'created_at', 'updated_at'])`: the copy gets a new UUID from `BaseModel`.
pub async fn replicate_file_volume(
    db: &Db,
    id: i64,
    resource_id: i64,
    now: NaiveDateTime,
) -> DbResult<Record> {
    db.ensure_writable()?;
    let cols = replicable_columns(
        db,
        "local_file_volumes",
        &["id", "uuid", "resource_id", "created_at", "updated_at"],
    )
    .await?;
    let list = cols
        .iter()
        .map(|c| format!("\"{c}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let text: String = sqlx::query_scalar(&format!(
        "INSERT INTO local_file_volumes (uuid, resource_id, created_at, updated_at, {list}) \
         SELECT $1, $2, $3, $3, {list} FROM local_file_volumes WHERE id = $4 RETURNING row_to_json(local_file_volumes)::text"
    ))
    .bind(new_public_id())
    .bind(resource_id)
    .bind(now)
    .bind(id)
    .fetch_one(db.pool())
    .await?;
    decode(&text, "local_file_volumes")
}

/// `$environmentVariable->replicate(...)->fill([...])->save()` for every variable.
pub async fn replicate_variables(
    db: &Db,
    morph: &str,
    from: i64,
    to: i64,
    now: NaiveDateTime,
) -> DbResult<()> {
    db.ensure_writable()?;
    let cols = replicable_columns(
        db,
        "environment_variables",
        &["id", "uuid", "resourceable_id", "created_at", "updated_at"],
    )
    .await?;
    let list = cols
        .iter()
        .map(|c| format!("\"{c}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let ids: Vec<i64> = sqlx::query_scalar(
        "SELECT id FROM environment_variables WHERE resourceable_type = $1 AND resourceable_id = $2 ORDER BY id",
    )
    .bind(morph)
    .bind(from)
    .fetch_all(db.pool())
    .await?;
    for id in ids {
        sqlx::query(&format!(
            "INSERT INTO environment_variables (uuid, resourceable_id, created_at, updated_at, {list}) \
             SELECT $1, $2, $3, $3, {list} FROM environment_variables WHERE id = $4"
        ))
        .bind(new_public_id())
        .bind(to)
        .bind(now)
        .bind(id)
        .execute(db.pool())
        .await?;
    }
    Ok(())
}

/// `StandaloneDocker::ownedByCurrentTeamAPI($teamId)->where('uuid', ...)`: `(destination id, server id)`.
pub async fn team_standalone_destination(
    db: &Db,
    team_id: TeamId,
    uuid: &str,
) -> DbResult<Option<(i64, i64)>> {
    Ok(sqlx::query_as(
        "SELECT sd.id, s.id FROM standalone_dockers sd JOIN servers s ON s.id = sd.server_id \
         WHERE sd.uuid = $1 AND s.team_id = $2 LIMIT 1",
    )
    .bind(uuid)
    .bind(team_id)
    .fetch_optional(db.pool())
    .await?)
}

/// `SwarmDocker::ownedByCurrentTeamAPI($teamId)->where('uuid', ...)` exists.
pub async fn team_swarm_destination_exists(db: &Db, team_id: TeamId, uuid: &str) -> DbResult<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM swarm_dockers sd JOIN servers s ON s.id = sd.server_id WHERE sd.uuid = $1 AND s.team_id = $2)",
    )
    .bind(uuid)
    .bind(team_id)
    .fetch_one(db.pool())
    .await?)
}

// ---------------------------------------------------------------------------
// SSL certificates
// ---------------------------------------------------------------------------

/// A row of `ssl_certificates`, values still encrypted (`encrypted` casts).
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct SslCertificate {
    pub id: i64,
    pub ssl_certificate: String,
    pub ssl_private_key: String,
    pub common_name: String,
    pub valid_until: NaiveDateTime,
}

/// `$server->sslCertificates()->where('is_ca_certificate', true)->first()`.
pub async fn ca_certificate(db: &Db, server_id: i64) -> DbResult<Option<SslCertificate>> {
    Ok(sqlx::query_as(
        "SELECT id, ssl_certificate, ssl_private_key, common_name, valid_until FROM ssl_certificates WHERE server_id = $1 AND is_ca_certificate ORDER BY id LIMIT 1",
    )
    .bind(server_id)
    .fetch_optional(db.pool())
    .await?)
}

/// `$database->sslCertificates()->first()`.
pub async fn resource_certificate(
    db: &Db,
    morph: &str,
    id: i64,
) -> DbResult<Option<SslCertificate>> {
    Ok(sqlx::query_as(
        "SELECT id, ssl_certificate, ssl_private_key, common_name, valid_until FROM ssl_certificates WHERE resource_type = $1 AND resource_id = $2 ORDER BY id LIMIT 1",
    )
    .bind(morph)
    .bind(id)
    .fetch_optional(db.pool())
    .await?)
}

#[derive(Debug)]
pub struct NewCertificate<'a> {
    pub certificate_encrypted: &'a str,
    pub private_key_encrypted: &'a str,
    pub resource_type: Option<&'a str>,
    pub resource_id: Option<i64>,
    pub server_id: i64,
    pub configuration_dir: Option<&'a str>,
    pub mount_path: Option<&'a str>,
    pub valid_until: NaiveDateTime,
    pub is_ca_certificate: bool,
    pub common_name: &'a str,
    pub subject_alternative_names: &'a [String],
}

/// `SslHelper::generateSslCertificate`'s row: earlier rows of the resource on the server go first.
pub async fn store_certificate(
    db: &Db,
    n: &NewCertificate<'_>,
    now: NaiveDateTime,
) -> DbResult<i64> {
    db.ensure_writable()?;
    let mut tx = db.pool().begin().await?;
    sqlx::query(
        "DELETE FROM ssl_certificates WHERE resource_type IS NOT DISTINCT FROM $1 AND resource_id IS NOT DISTINCT FROM $2 AND server_id = $3",
    )
    .bind(n.resource_type)
    .bind(n.resource_id)
    .bind(n.server_id)
    .execute(&mut *tx)
    .await?;
    let names = serde_json::to_string(n.subject_alternative_names).unwrap_or_else(|_| "[]".into());
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO ssl_certificates (ssl_certificate, ssl_private_key, resource_type, resource_id, server_id, configuration_dir, mount_path, valid_until, is_ca_certificate, common_name, subject_alternative_names, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11::json, $12, $12) RETURNING id",
    )
    .bind(n.certificate_encrypted)
    .bind(n.private_key_encrypted)
    .bind(n.resource_type)
    .bind(n.resource_id)
    .bind(n.server_id)
    .bind(n.configuration_dir)
    .bind(n.mount_path)
    .bind(n.valid_until)
    .bind(n.is_ca_certificate)
    .bind(n.common_name)
    .bind(names)
    .bind(now)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(id)
}

/// `$database->sslCertificates()->delete()`.
pub async fn delete_resource_certificates(db: &Db, morph: &str, id: i64) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query("DELETE FROM ssl_certificates WHERE resource_type = $1 AND resource_id = $2")
        .bind(morph)
        .bind(id)
        .execute(db.pool())
        .await?;
    Ok(())
}

/// Deletes the file storages of a resource with these mount paths
/// (`->filter(in_array($storage->mount_path, ...))->each->delete()`).
pub async fn delete_file_storages_at(
    db: &Db,
    morph: &str,
    id: i64,
    mount_paths: &[String],
) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(
        "DELETE FROM local_file_volumes WHERE resource_type = $1 AND resource_id = $2 AND mount_path = ANY($3)",
    )
    .bind(morph)
    .bind(id)
    .bind(mount_paths)
    .execute(db.pool())
    .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Variables
// ---------------------------------------------------------------------------

/// A row of `environment_variables` of a resource, by key or uuid.
pub async fn variable_by_key(db: &Db, morph: &str, id: i64, key: &str) -> DbResult<Option<Record>> {
    let text: Option<String> = sqlx::query_scalar(
        "SELECT row_to_json(v)::text FROM environment_variables v WHERE resourceable_type = $1 AND resourceable_id = $2 AND key = $3 ORDER BY id LIMIT 1",
    )
    .bind(morph)
    .bind(id)
    .bind(key)
    .fetch_optional(db.pool())
    .await?;
    text.map(|t| decode(&t, "environment_variables"))
        .transpose()
}

pub async fn variable_by_uuid(
    db: &Db,
    morph: &str,
    id: i64,
    uuid: &str,
) -> DbResult<Option<Record>> {
    let text: Option<String> = sqlx::query_scalar(
        "SELECT row_to_json(v)::text FROM environment_variables v WHERE resourceable_type = $1 AND resourceable_id = $2 AND uuid = $3 ORDER BY id LIMIT 1",
    )
    .bind(morph)
    .bind(id)
    .bind(uuid)
    .fetch_optional(db.pool())
    .await?;
    text.map(|t| decode(&t, "environment_variables"))
        .transpose()
}

/// `$database->environment_variables()->create([...])` plus the `created` hook's `version`.
#[allow(clippy::too_many_arguments)]
pub async fn create_variable(
    db: &Db,
    morph: &str,
    id: i64,
    key: &str,
    value_encrypted: Option<&str>,
    is_literal: bool,
    is_multiline: bool,
    is_shown_once: bool,
    comment: Option<&str>,
    is_shared: bool,
    version: &str,
    now: NaiveDateTime,
) -> DbResult<Record> {
    db.ensure_writable()?;
    let text: String = sqlx::query_scalar(
        "INSERT INTO environment_variables (key, value, is_literal, is_multiline, is_shown_once, comment, resourceable_type, resourceable_id, is_shared, is_runtime, is_buildtime, uuid, version, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, true, true, $10, $11, $12, $12) RETURNING row_to_json(environment_variables)::text",
    )
    .bind(key)
    .bind(value_encrypted)
    .bind(is_literal)
    .bind(is_multiline)
    .bind(is_shown_once)
    .bind(comment)
    .bind(morph)
    .bind(id)
    .bind(is_shared)
    .bind(new_public_id())
    .bind(version)
    .bind(now)
    .fetch_one(db.pool())
    .await?;
    decode(&text, "environment_variables")
}

/// `$env->forceDelete()`.
pub async fn delete_variable(db: &Db, id: i64) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query("DELETE FROM environment_variables WHERE id = $1")
        .bind(id)
        .execute(db.pool())
        .await?;
    Ok(())
}
