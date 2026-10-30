//! `local_persistent_volumes` and `local_file_volumes` of any resource (morph type + id).
//! `local_file_volumes.content` uses the `encrypted` cast; callers encrypt and decrypt it.

use chrono::NaiveDateTime;
use coolify_domain::new_public_id;

use crate::records::{Record, parse_row};
use crate::servers::{ColumnValue, bind_all};
use crate::{Db, DbError, DbResult};

/// Which table a storage row is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageKind {
    Persistent,
    File,
}

impl StorageKind {
    fn table(self) -> &'static str {
        match self {
            Self::Persistent => "local_persistent_volumes",
            Self::File => "local_file_volumes",
        }
    }
}

fn decode(t: &str) -> DbResult<Record> {
    parse_row(t).map_err(|e| DbError::Schema(format!("decoding storage row: {e}")))
}

/// `$resource->persistentStorages` / `fileStorages`, `->sortBy('id')`.
pub async fn list(
    db: &Db,
    kind: StorageKind,
    morph: &str,
    resource_id: i64,
) -> DbResult<Vec<Record>> {
    let table = kind.table();
    let rows: Vec<String> = sqlx::query_scalar(&format!(
        "SELECT row_to_json(s)::text FROM {table} s WHERE resource_type = $1 AND resource_id = $2 ORDER BY id"
    ))
    .bind(morph)
    .bind(resource_id)
    .fetch_all(db.pool())
    .await?;
    rows.iter().map(|t| decode(t)).collect()
}

/// `LocalPersistentVolume::create([...])`: the row.
pub async fn create_persistent(
    db: &Db,
    morph: &str,
    resource_id: i64,
    name: &str,
    mount_path: &str,
    now: NaiveDateTime,
) -> DbResult<Record> {
    db.ensure_writable()?;
    let row: String = sqlx::query_scalar(
        "INSERT INTO local_persistent_volumes (uuid, name, mount_path, resource_id, resource_type, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $6) RETURNING row_to_json(local_persistent_volumes)::text",
    )
    .bind(new_public_id())
    .bind(name)
    .bind(mount_path)
    .bind(resource_id)
    .bind(morph)
    .bind(now)
    .fetch_one(db.pool())
    .await?;
    decode(&row)
}

/// The attributes `LocalFileVolume::create([...])` gets.
#[derive(Debug, Clone, Default)]
pub struct NewFileVolume {
    pub fs_path: String,
    pub mount_path: String,
    pub content_encrypted: Option<String>,
    pub is_directory: bool,
    pub is_host_file: bool,
}

/// `LocalFileVolume::create([...])`: the row. The model's `created` hook (the server write) is the
/// caller's.
pub async fn create_file(
    db: &Db,
    morph: &str,
    resource_id: i64,
    v: &NewFileVolume,
    now: NaiveDateTime,
) -> DbResult<Record> {
    db.ensure_writable()?;
    let row: String = sqlx::query_scalar(
        "INSERT INTO local_file_volumes (uuid, fs_path, mount_path, content, is_directory, is_host_file, resource_id, resource_type, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $9) RETURNING row_to_json(local_file_volumes)::text",
    )
    .bind(new_public_id())
    .bind(&v.fs_path)
    .bind(&v.mount_path)
    .bind(&v.content_encrypted)
    .bind(v.is_directory)
    .bind(v.is_host_file)
    .bind(resource_id)
    .bind(morph)
    .bind(now)
    .fetch_one(db.pool())
    .await?;
    decode(&row)
}

/// `$storage->save()` with the dirty columns; without any, nothing is written. Returns the row.
pub async fn save_dirty(
    db: &Db,
    kind: StorageKind,
    id: i64,
    dirty: &[(&'static str, ColumnValue)],
    now: NaiveDateTime,
) -> DbResult<Record> {
    let table = kind.table();
    if !dirty.is_empty() {
        db.ensure_writable()?;
        let set = dirty
            .iter()
            .enumerate()
            .map(|(i, (c, _))| format!("\"{c}\" = ${}", i + 3))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!("UPDATE {table} SET {set}, updated_at = $1 WHERE id = $2");
        bind_all(sqlx::query(&sql).bind(now).bind(id), dirty)
            .execute(db.pool())
            .await?;
    }
    let row: String = sqlx::query_scalar(&format!(
        "SELECT row_to_json(s)::text FROM {table} s WHERE id = $1"
    ))
    .bind(id)
    .fetch_one(db.pool())
    .await?;
    decode(&row)
}

/// `$storage->delete()`.
pub async fn delete(db: &Db, kind: StorageKind, id: i64) -> DbResult<()> {
    db.ensure_writable()?;
    let table = kind.table();
    sqlx::query(&format!("DELETE FROM {table} WHERE id = $1"))
        .bind(id)
        .execute(db.pool())
        .await?;
    Ok(())
}

/// `$storage->scheduledBackups()->exists()`.
pub async fn has_scheduled_backups(db: &Db, kind: StorageKind, id: i64) -> DbResult<bool> {
    let morph = match kind {
        StorageKind::Persistent => r"App\Models\LocalPersistentVolume",
        StorageKind::File => r"App\Models\LocalFileVolume",
    };
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM scheduled_volume_backups WHERE backupable_type = $1 AND backupable_id = $2)",
    )
    .bind(morph)
    .bind(id)
    .fetch_one(db.pool())
    .await?)
}

/// `$application->additional_servers` (ids).
pub async fn additional_server_ids(db: &Db, application_id: i64) -> DbResult<Vec<i64>> {
    Ok(sqlx::query_scalar(
        "SELECT server_id FROM additional_destinations WHERE application_id = $1",
    )
    .bind(application_id)
    .fetch_all(db.pool())
    .await?)
}
