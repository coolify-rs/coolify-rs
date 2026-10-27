//! `private_keys`. The `private_key` column holds a Laravel-encrypted value
//! (`encrypted` cast); callers encrypt/decrypt with the instance `APP_KEY`.

use chrono::NaiveDateTime;
use coolify_domain::naming::strip_tags;
use coolify_domain::{TeamId, new_public_id};

use crate::records::{PRIVATE_KEY, Record, parse_row};
use crate::{Db, DbError, DbResult};

/// A key row with its ciphertext (never log `private_key_encrypted`).
pub struct KeyRow {
    pub id: i64,
    pub uuid: String,
    pub team_id: TeamId,
    pub name: String,
    pub description: Option<String>,
    pub private_key_encrypted: String,
    /// Model attributes as `toArray()` renders them (private_key hidden).
    pub record: Record,
}

impl std::fmt::Debug for KeyRow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KeyRow")
            .field("id", &self.id)
            .field("uuid", &self.uuid)
            .finish_non_exhaustive()
    }
}

fn to_row(text: &str) -> DbResult<KeyRow> {
    let raw =
        parse_row(text).map_err(|e| DbError::Schema(format!("decoding private key row: {e}")))?;
    let get_str = |k: &str| raw.get(k).and_then(|v| v.as_str()).map(str::to_string);
    let row = KeyRow {
        id: raw.get("id").and_then(|v| v.as_i64()).unwrap_or_default(),
        uuid: get_str("uuid").unwrap_or_default(),
        team_id: raw
            .get("team_id")
            .and_then(|v| v.as_i64())
            .unwrap_or_default(),
        name: get_str("name").unwrap_or_default(),
        description: get_str("description"),
        private_key_encrypted: get_str("private_key").unwrap_or_default(),
        record: Record::new(),
    };
    Ok(KeyRow {
        record: PRIVATE_KEY.present(raw),
        ..row
    })
}

pub async fn list(db: &Db, team_id: TeamId) -> DbResult<Vec<KeyRow>> {
    let rows: Vec<String> =
        sqlx::query_scalar("SELECT row_to_json(k)::text FROM private_keys k WHERE k.team_id = $1")
            .bind(team_id)
            .fetch_all(db.pool())
            .await?;
    rows.iter().map(|r| to_row(r)).collect()
}

pub async fn find(db: &Db, team_id: TeamId, uuid: &str) -> DbResult<Option<KeyRow>> {
    let row: Option<String> = sqlx::query_scalar(
        "SELECT row_to_json(k)::text FROM private_keys k WHERE k.team_id = $1 AND k.uuid = $2 LIMIT 1",
    )
    .bind(team_id)
    .bind(uuid)
    .fetch_optional(db.pool())
    .await?;
    row.as_deref().map(to_row).transpose()
}

/// `PrivateKey::fingerprintExists`: always scoped to one team.
pub async fn fingerprint_exists(
    db: &Db,
    team_id: TeamId,
    fingerprint: &str,
    exclude_id: Option<i64>,
) -> DbResult<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM private_keys WHERE team_id = $1 AND fingerprint = $2 \
         AND ($3::bigint IS NULL OR id <> $3))",
    )
    .bind(team_id)
    .bind(fingerprint)
    .bind(exclude_id)
    .fetch_one(db.pool())
    .await?)
}

#[derive(Debug)]
pub struct NewKey<'a> {
    pub team_id: TeamId,
    pub name: &'a str,
    pub description: &'a str,
    pub private_key_encrypted: &'a str,
    pub fingerprint: &'a str,
}

pub async fn create(db: &Db, k: NewKey<'_>, now: NaiveDateTime) -> DbResult<String> {
    db.ensure_writable()?;
    let uuid = new_public_id();
    sqlx::query(
        "INSERT INTO private_keys (uuid, name, description, private_key, is_git_related, team_id, fingerprint, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, false, $5, $6, $7, $7)",
    )
    .bind(&uuid)
    .bind(strip_tags(Some(k.name)))
    .bind(strip_tags(Some(k.description)))
    .bind(k.private_key_encrypted)
    .bind(k.team_id)
    .bind(k.fingerprint)
    .bind(now)
    .execute(db.pool())
    .await?;
    Ok(uuid)
}

/// Fields present in an update; the key is re-encrypted by the caller.
#[derive(Debug, Default)]
pub struct KeyUpdate {
    pub name: Option<String>,
    pub description: Option<String>,
    pub private_key_encrypted: Option<String>,
    /// Always recomputed by the model's `saving` hook.
    pub fingerprint: Option<String>,
}

pub async fn update(db: &Db, id: i64, u: KeyUpdate, now: NaiveDateTime) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(
        "UPDATE private_keys SET \
           name = COALESCE($2, name), description = COALESCE($3, description), \
           private_key = COALESCE($4, private_key), fingerprint = COALESCE($5, fingerprint), updated_at = $6 \
         WHERE id = $1",
    )
    .bind(id)
    .bind(u.name.map(|n| strip_tags(Some(&n))))
    .bind(u.description.map(|d| strip_tags(Some(&d))))
    .bind(u.private_key_encrypted)
    .bind(u.fingerprint)
    .bind(now)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// `PrivateKey::isInUse`: servers, applications (both soft-deleting), Git apps.
pub async fn in_use(db: &Db, id: i64) -> DbResult<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM servers WHERE private_key_id = $1 AND deleted_at IS NULL) \
             OR EXISTS (SELECT 1 FROM applications WHERE private_key_id = $1 AND deleted_at IS NULL) \
             OR EXISTS (SELECT 1 FROM github_apps WHERE private_key_id = $1) \
             OR EXISTS (SELECT 1 FROM gitlab_apps WHERE private_key_id = $1) \
             OR EXISTS (SELECT 1 FROM gitea_apps WHERE private_key_id = $1)",
    )
    .bind(id)
    .fetch_one(db.pool())
    .await?)
}

pub async fn delete(db: &Db, id: i64) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query("DELETE FROM private_keys WHERE id = $1")
        .bind(id)
        .execute(db.pool())
        .await?;
    Ok(())
}
