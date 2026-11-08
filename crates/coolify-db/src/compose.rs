//! Rows the Docker Compose parsers write (`applicationParser()`, later `serviceParser()`):
//! `environment_variables`, `local_file_volumes`, `local_persistent_volumes` and the compose
//! columns of `applications`. Values arrive already encrypted; the callers implement the model
//! semantics (dirty checks, `created` hooks) and call these in upstream's order.

use chrono::NaiveDateTime;
use coolify_domain::new_public_id;

use crate::{Db, DbResult};

/// A stored environment variable as the parsers read it (value still encrypted).
#[derive(Debug, Clone)]
pub struct EnvRow {
    pub id: i64,
    pub key: String,
    pub value_encrypted: Option<String>,
    pub is_required: bool,
    pub preserve_compose_null: bool,
    pub is_preview: bool,
    pub is_shared: bool,
}

type EnvTuple = (i64, String, Option<String>, bool, bool, bool, bool);

const ENV_COLUMNS: &str =
    "id, key, value, is_required, preserve_compose_null, is_preview, is_shared";

fn env_row(t: EnvTuple) -> EnvRow {
    EnvRow {
        id: t.0,
        key: t.1,
        value_encrypted: t.2,
        is_required: t.3,
        preserve_compose_null: t.4,
        is_preview: t.5,
        is_shared: t.6,
    }
}

/// `$resource->environment_variables()->where('key', $key)->first()` (or the preview relation).
pub async fn env_first(
    db: &Db,
    resource_type: &str,
    resource_id: i64,
    key: &str,
    is_preview: bool,
) -> DbResult<Option<EnvRow>> {
    let row: Option<EnvTuple> = sqlx::query_as(&format!(
        "SELECT {ENV_COLUMNS} FROM environment_variables WHERE resourceable_type = $1 AND resourceable_id = $2 \
         AND is_preview = $3 AND key = $4 LIMIT 1"
    ))
    .bind(resource_type)
    .bind(resource_id)
    .bind(is_preview)
    .bind(key)
    .fetch_optional(db.pool())
    .await?;
    Ok(row.map(env_row))
}

/// `$resource->environment_variables()->get()` or `environment_variables_preview()->get()`
/// (no order; v4.6.0 dropped the `CASE` order of the preview relation).
pub async fn env_list(
    db: &Db,
    resource_type: &str,
    resource_id: i64,
    is_preview: bool,
) -> DbResult<Vec<EnvRow>> {
    let rows: Vec<EnvTuple> = sqlx::query_as(&format!(
        "SELECT {ENV_COLUMNS} FROM environment_variables WHERE resourceable_type = $1 AND resourceable_id = $2 \
         AND is_preview = $3"
    ))
    .bind(resource_type)
    .bind(resource_id)
    .bind(is_preview)
    .fetch_all(db.pool())
    .await?;
    Ok(rows.into_iter().map(env_row).collect())
}

/// The attributes of `EnvironmentVariable::create([...])`.
#[derive(Debug, Clone)]
pub struct NewEnv {
    pub key: String,
    /// `encrypt($value)`, or `None` for a null value.
    pub value_encrypted: Option<String>,
    pub is_preview: bool,
    pub is_required: bool,
    pub is_multiline: bool,
    pub is_literal: bool,
    pub is_shown_once: bool,
    pub is_shared: bool,
    pub comment: Option<String>,
}

/// One `EnvironmentVariable::create()` with the `version` the `created` hook writes.
pub async fn env_insert(
    db: &Db,
    resource_type: &str,
    resource_id: i64,
    v: &NewEnv,
    version: &str,
    now: NaiveDateTime,
) -> DbResult<i64> {
    db.ensure_writable()?;
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO environment_variables (key, value, is_preview, is_required, is_multiline, is_literal, is_shown_once, \
         is_runtime, is_buildtime, is_shared, comment, resourceable_type, resourceable_id, uuid, version, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, true, true, $8, $9, $10, $11, $12, $13, $14, $14) RETURNING id",
    )
    .bind(&v.key)
    .bind(&v.value_encrypted)
    .bind(v.is_preview)
    .bind(v.is_required)
    .bind(v.is_multiline)
    .bind(v.is_literal)
    .bind(v.is_shown_once)
    .bind(v.is_shared)
    .bind(&v.comment)
    .bind(resource_type)
    .bind(resource_id)
    .bind(new_public_id())
    .bind(version)
    .bind(now)
    .fetch_one(db.pool())
    .await?;
    Ok(id)
}

/// The changed columns of an environment variable (`$variable->save()` after the dirty check).
#[derive(Debug, Clone, Default)]
pub struct EnvUpdate {
    /// `Some(value)` when the value changed.
    pub value_encrypted: Option<Option<String>>,
    pub is_shared: Option<bool>,
    pub is_required: Option<bool>,
    pub preserve_compose_null: Option<bool>,
}

/// `UPDATE environment_variables SET <dirty columns>, updated_at`.
pub async fn env_update(db: &Db, id: i64, u: &EnvUpdate, now: NaiveDateTime) -> DbResult<()> {
    if u.value_encrypted.is_none()
        && u.is_shared.is_none()
        && u.is_required.is_none()
        && u.preserve_compose_null.is_none()
    {
        return Ok(());
    }
    db.ensure_writable()?;
    sqlx::query(
        "UPDATE environment_variables SET \
         value = CASE WHEN $2 THEN $3 ELSE value END, \
         is_shared = COALESCE($4, is_shared), is_required = COALESCE($5, is_required), \
         preserve_compose_null = COALESCE($6, preserve_compose_null), updated_at = $7 WHERE id = $1",
    )
    .bind(id)
    .bind(u.value_encrypted.is_some())
    .bind(u.value_encrypted.clone().flatten())
    .bind(u.is_shared)
    .bind(u.is_required)
    .bind(u.preserve_compose_null)
    .bind(now)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// `$resource->environment_variables()->where('key', 'LIKE', $pattern)->update(['value' => null])`.
pub async fn env_null_values_like(
    db: &Db,
    resource_type: &str,
    resource_id: i64,
    pattern: &str,
    now: NaiveDateTime,
) -> DbResult<u64> {
    db.ensure_writable()?;
    let done = sqlx::query(
        "UPDATE environment_variables SET value = NULL, updated_at = $4 WHERE resourceable_type = $1 \
         AND resourceable_id = $2 AND is_preview = false AND key LIKE $3",
    )
    .bind(resource_type)
    .bind(resource_id)
    .bind(pattern)
    .bind(now)
    .execute(db.pool())
    .await?;
    Ok(done.rows_affected())
}

/// A `local_file_volumes` row (content still encrypted).
#[derive(Debug, Clone)]
pub struct FileVolumeRow {
    pub id: i64,
    pub fs_path: String,
    pub mount_path: Option<String>,
    pub content_encrypted: Option<String>,
    pub is_directory: bool,
    pub is_preview_suffix_enabled: bool,
}

/// `$resource->fileStorages()->whereMountPath($path)->first()` / `LocalFileVolume::where(mount_path,
/// resource_id, resource_type)->first()`.
pub async fn file_volume_by_mount(
    db: &Db,
    resource_type: &str,
    resource_id: i64,
    mount_path: &str,
) -> DbResult<Option<FileVolumeRow>> {
    type Row = (i64, String, Option<String>, Option<String>, bool, bool);
    let row: Option<Row> = sqlx::query_as(
        "SELECT id, fs_path, mount_path, content, is_directory, is_preview_suffix_enabled FROM local_file_volumes \
         WHERE resource_type = $1 AND resource_id = $2 AND mount_path = $3 LIMIT 1",
    )
    .bind(resource_type)
    .bind(resource_id)
    .bind(mount_path)
    .fetch_optional(db.pool())
    .await?;
    Ok(row.map(|r| FileVolumeRow {
        id: r.0,
        fs_path: r.1,
        mount_path: r.2,
        content_encrypted: r.3,
        is_directory: r.4,
        is_preview_suffix_enabled: r.5,
    }))
}

/// `LocalFileVolume::updateOrCreate(...)` insert half.
#[allow(clippy::too_many_arguments)]
pub async fn file_volume_insert(
    db: &Db,
    resource_type: &str,
    resource_id: i64,
    fs_path: &str,
    mount_path: &str,
    content_encrypted: Option<&str>,
    is_directory: bool,
    now: NaiveDateTime,
) -> DbResult<i64> {
    db.ensure_writable()?;
    Ok(sqlx::query_scalar(
        "INSERT INTO local_file_volumes (fs_path, mount_path, content, is_directory, resource_id, resource_type, uuid, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $8) RETURNING id",
    )
    .bind(fs_path)
    .bind(mount_path)
    .bind(content_encrypted)
    .bind(is_directory)
    .bind(resource_id)
    .bind(resource_type)
    .bind(new_public_id())
    .bind(now)
    .fetch_one(db.pool())
    .await?)
}

/// `LocalFileVolume::updateOrCreate(...)` update half. `fs_path` and `mount_path` are always
/// dirty (the parser passes `Stringable`s); `content` is `Some` when its plaintext changed.
pub async fn file_volume_update(
    db: &Db,
    id: i64,
    fs_path: &str,
    mount_path: &str,
    content_encrypted: Option<Option<&str>>,
    is_directory: bool,
    now: NaiveDateTime,
) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(
        "UPDATE local_file_volumes SET fs_path = $2, mount_path = $3, content = CASE WHEN $4 THEN $5 ELSE content END, \
         is_directory = $6, updated_at = $7 WHERE id = $1",
    )
    .bind(id)
    .bind(fs_path)
    .bind(mount_path)
    .bind(content_encrypted.is_some())
    .bind(content_encrypted.flatten())
    .bind(is_directory)
    .bind(now)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// `forgetComposeBindStorage($resource, $mountPath)`: deletes the file storages of a mount path
/// that are not host files and have no backup schedule.
pub async fn forget_file_volumes(
    db: &Db,
    resource_type: &str,
    resource_id: i64,
    mount_path: &str,
) -> DbResult<u64> {
    db.ensure_writable()?;
    let done = sqlx::query(
        "DELETE FROM local_file_volumes v WHERE v.resource_type = $1 AND v.resource_id = $2 AND v.mount_path = $3 \
         AND v.is_host_file = false AND NOT EXISTS (SELECT 1 FROM scheduled_volume_backups b \
         WHERE b.backupable_type = 'App\\Models\\LocalFileVolume' AND b.backupable_id = v.id)",
    )
    .bind(resource_type)
    .bind(resource_id)
    .bind(mount_path)
    .execute(db.pool())
    .await?;
    Ok(done.rows_affected())
}

/// `LocalPersistentVolume::updateOrCreate(['name', 'resource_id', 'resource_type'], [..., 'mount_path'])`:
/// returns `ignores_compose_driver_options` of the row. `mount_path` is always dirty (a `Stringable`).
pub async fn persistent_volume_upsert(
    db: &Db,
    resource_type: &str,
    resource_id: i64,
    name: &str,
    mount_path: &str,
    now: NaiveDateTime,
) -> DbResult<bool> {
    db.ensure_writable()?;
    let found: Option<(i64, bool)> = sqlx::query_as(
        "SELECT id, ignores_compose_driver_options FROM local_persistent_volumes WHERE name = $1 AND resource_id = $2 \
         AND resource_type = $3 LIMIT 1",
    )
    .bind(name)
    .bind(resource_id)
    .bind(resource_type)
    .fetch_optional(db.pool())
    .await?;
    match found {
        Some((id, ignores)) => {
            sqlx::query("UPDATE local_persistent_volumes SET mount_path = $2, updated_at = $3 WHERE id = $1")
                .bind(id)
                .bind(mount_path)
                .bind(now)
                .execute(db.pool())
                .await?;
            Ok(ignores)
        }
        None => {
            sqlx::query(
                "INSERT INTO local_persistent_volumes (name, mount_path, resource_id, resource_type, uuid, created_at, updated_at) \
                 VALUES ($1, $2, $3, $4, $5, $6, $6)",
            )
            .bind(name)
            .bind(mount_path)
            .bind(resource_id)
            .bind(resource_type)
            .bind(new_public_id())
            .bind(now)
            .execute(db.pool())
            .await?;
            Ok(false)
        }
    }
}

/// `LocalPersistentVolume::where(resource_type, resource_id, name)->exists()`.
pub async fn persistent_volume_exists(
    db: &Db,
    resource_type: &str,
    resource_id: i64,
    name: &str,
) -> DbResult<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM local_persistent_volumes WHERE resource_type = $1 AND resource_id = $2 AND name = $3)",
    )
    .bind(resource_type)
    .bind(resource_id)
    .bind(name)
    .fetch_one(db.pool())
    .await?)
}

/// The compose columns of an application that `$application->save()` writes when dirty.
#[derive(Debug, Clone, Default)]
pub struct ApplicationComposeUpdate {
    pub fqdn: Option<Option<String>>,
    /// JSON text (the `array` cast) or null.
    pub domain_port_overrides: Option<Option<String>>,
    pub noindex_domains: Option<Option<String>>,
    pub docker_compose_domains: Option<Option<String>>,
    pub docker_compose: Option<Option<String>>,
    pub docker_compose_raw: Option<Option<String>>,
    pub docker_compose_location: Option<Option<String>>,
    pub base_directory: Option<Option<String>>,
}

impl ApplicationComposeUpdate {
    pub fn is_empty(&self) -> bool {
        self.fqdn.is_none()
            && self.domain_port_overrides.is_none()
            && self.noindex_domains.is_none()
            && self.docker_compose_domains.is_none()
            && self.docker_compose.is_none()
            && self.docker_compose_raw.is_none()
            && self.docker_compose_location.is_none()
            && self.base_directory.is_none()
    }
}

/// `UPDATE applications SET <dirty columns>, updated_at`.
pub async fn application_update(
    db: &Db,
    id: i64,
    u: &ApplicationComposeUpdate,
    now: NaiveDateTime,
) -> DbResult<()> {
    if u.is_empty() {
        return Ok(());
    }
    db.ensure_writable()?;
    let mut sets: Vec<String> = Vec::new();
    let mut binds: Vec<Option<String>> = Vec::new();
    let mut add = |col: &str, cast: &str, v: &Option<Option<String>>| {
        if let Some(v) = v {
            binds.push(v.clone());
            sets.push(format!("{col} = ${}{cast}", binds.len() + 2));
        }
    };
    add("fqdn", "", &u.fqdn);
    add("domain_port_overrides", "::json", &u.domain_port_overrides);
    add("noindex_domains", "::json", &u.noindex_domains);
    add("docker_compose_domains", "", &u.docker_compose_domains);
    add("docker_compose", "", &u.docker_compose);
    add("docker_compose_raw", "", &u.docker_compose_raw);
    add("docker_compose_location", "", &u.docker_compose_location);
    add("base_directory", "", &u.base_directory);
    let sql = format!(
        "UPDATE applications SET {}, updated_at = $2 WHERE id = $1",
        sets.join(", ")
    );
    let mut q = sqlx::query(&sql).bind(id).bind(now);
    for b in binds {
        q = q.bind(b);
    }
    q.execute(db.pool()).await?;
    Ok(())
}

/// `SharedEnvironmentVariable::where(type, key, team_id, {type}_id)->first()->value` (encrypted).
pub async fn shared_variable_value(
    db: &Db,
    team_id: i64,
    kind: &str,
    key: &str,
    owner_id: i64,
) -> DbResult<Option<Option<String>>> {
    let column = match kind {
        "team" => "team_id",
        "project" => "project_id",
        "environment" => "environment_id",
        _ => return Ok(None),
    };
    let row: Option<(Option<String>,)> = sqlx::query_as(&format!(
        "SELECT value FROM shared_environment_variables WHERE type = $1 AND key = $2 AND team_id = $3 AND {column} = $4 LIMIT 1"
    ))
    .bind(kind)
    .bind(key)
    .bind(team_id)
    .bind(owner_id)
    .fetch_optional(db.pool())
    .await?;
    Ok(row.map(|r| r.0))
}
