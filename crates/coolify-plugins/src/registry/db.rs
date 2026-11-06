//! The registry's tables in the plugin schema: the last verified index, revocations, and updates waiting
//! for an admin to approve more capabilities.

use chrono::{DateTime, Utc};
use coolify_db::Db;
use serde_json::Value;
use sqlx::Row;

use crate::PluginError;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS coolify_rs_plugins.registry_index (
    id smallint PRIMARY KEY DEFAULT 1 CHECK (id = 1),
    url text NOT NULL,
    index_json bytea NOT NULL,
    signature bytea NOT NULL,
    key_id text NOT NULL,
    generated_at timestamptz NOT NULL,
    fetched_at timestamptz NOT NULL DEFAULT now()
);
CREATE TABLE IF NOT EXISTS coolify_rs_plugins.revoked (
    plugin_id text PRIMARY KEY,
    reason text NOT NULL,
    revoked_at timestamptz NOT NULL DEFAULT now()
);
CREATE TABLE IF NOT EXISTS coolify_rs_plugins.pending_updates (
    plugin_id text PRIMARY KEY REFERENCES coolify_rs_plugins.plugins (id) ON DELETE CASCADE,
    version text NOT NULL,
    manifest jsonb NOT NULL,
    sha256 text NOT NULL,
    url text NOT NULL,
    found_at timestamptz NOT NULL DEFAULT now()
);
";

/// Creates the tables when missing; called from `store::migrate`.
pub async fn migrate(db: &Db) -> Result<(), PluginError> {
    sqlx::raw_sql(SCHEMA).execute(db.pool()).await?;
    Ok(())
}

/// The last index that verified.
#[derive(Debug, Clone)]
pub struct StoredIndex {
    pub url: String,
    pub bytes: Vec<u8>,
    pub signature: Vec<u8>,
    pub key_id: String,
    pub generated_at: DateTime<Utc>,
    pub fetched_at: DateTime<Utc>,
}

pub async fn load_index(db: &Db) -> Result<Option<StoredIndex>, PluginError> {
    let row = sqlx::query(
        "SELECT url, index_json, signature, key_id, generated_at, fetched_at \
         FROM coolify_rs_plugins.registry_index WHERE id = 1",
    )
    .fetch_optional(db.pool())
    .await?;
    row.map(|r| {
        Ok(StoredIndex {
            url: r.try_get("url")?,
            bytes: r.try_get("index_json")?,
            signature: r.try_get("signature")?,
            key_id: r.try_get("key_id")?,
            generated_at: r.try_get("generated_at")?,
            fetched_at: r.try_get("fetched_at")?,
        })
    })
    .transpose()
}

pub async fn save_index(
    db: &Db,
    url: &str,
    bytes: &[u8],
    signature: &[u8],
    key_id: &str,
    generated_at: DateTime<Utc>,
) -> Result<DateTime<Utc>, PluginError> {
    Ok(sqlx::query_scalar(
        "INSERT INTO coolify_rs_plugins.registry_index (id, url, index_json, signature, key_id, generated_at, fetched_at) \
         VALUES (1, $1, $2, $3, $4, $5, now()) \
         ON CONFLICT (id) DO UPDATE SET url = $1, index_json = $2, signature = $3, key_id = $4, \
         generated_at = $5, fetched_at = now() RETURNING fetched_at",
    )
    .bind(url)
    .bind(bytes)
    .bind(signature)
    .bind(key_id)
    .bind(generated_at)
    .fetch_one(db.pool())
    .await?)
}

/// Replaces the revocation list with the index's.
pub async fn set_revoked(db: &Db, revoked: &[(String, String)]) -> Result<(), PluginError> {
    let mut tx = db.pool().begin().await?;
    let ids: Vec<&str> = revoked.iter().map(|(id, _)| id.as_str()).collect();
    sqlx::query("DELETE FROM coolify_rs_plugins.revoked WHERE plugin_id <> ALL($1)")
        .bind(&ids)
        .execute(&mut *tx)
        .await?;
    for (id, reason) in revoked {
        sqlx::query(
            "INSERT INTO coolify_rs_plugins.revoked (plugin_id, reason) VALUES ($1, $2) \
             ON CONFLICT (plugin_id) DO UPDATE SET reason = $2",
        )
        .bind(id)
        .bind(reason)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Why the registry revoked a plugin, if it did.
pub async fn revoked(db: &Db, id: &str) -> Result<Option<String>, PluginError> {
    Ok(
        sqlx::query_scalar("SELECT reason FROM coolify_rs_plugins.revoked WHERE plugin_id = $1")
            .bind(id)
            .fetch_optional(db.pool())
            .await?,
    )
}

/// A newer version that asks for more than the admin granted.
#[derive(Debug, Clone)]
pub struct Pending {
    pub plugin_id: String,
    pub version: String,
    pub manifest: Value,
    pub sha256: String,
    pub url: String,
    pub found_at: DateTime<Utc>,
}

pub async fn pending(db: &Db) -> Result<Vec<Pending>, PluginError> {
    let rows = sqlx::query(
        "SELECT plugin_id, version, manifest, sha256, url, found_at FROM coolify_rs_plugins.pending_updates \
         ORDER BY plugin_id",
    )
    .fetch_all(db.pool())
    .await?;
    rows.iter()
        .map(|r| {
            Ok(Pending {
                plugin_id: r.try_get("plugin_id")?,
                version: r.try_get("version")?,
                manifest: r.try_get("manifest")?,
                sha256: r.try_get("sha256")?,
                url: r.try_get("url")?,
                found_at: r.try_get("found_at")?,
            })
        })
        .collect()
}

/// Records a pending update. Returns false when the same version was already pending.
pub async fn set_pending(
    db: &Db,
    id: &str,
    version: &str,
    manifest: &Value,
    sha256: &str,
    url: &str,
) -> Result<bool, PluginError> {
    let n = sqlx::query(
        "INSERT INTO coolify_rs_plugins.pending_updates (plugin_id, version, manifest, sha256, url) \
         VALUES ($1, $2, $3, $4, $5) \
         ON CONFLICT (plugin_id) DO UPDATE SET version = $2, manifest = $3, sha256 = $4, url = $5, found_at = now() \
         WHERE pending_updates.version <> $2",
    )
    .bind(id)
    .bind(version)
    .bind(manifest)
    .bind(sha256)
    .bind(url)
    .execute(db.pool())
    .await?
    .rows_affected();
    Ok(n > 0)
}

pub async fn clear_pending(db: &Db, id: &str) -> Result<(), PluginError> {
    sqlx::query("DELETE FROM coolify_rs_plugins.pending_updates WHERE plugin_id = $1")
        .bind(id)
        .execute(db.pool())
        .await?;
    Ok(())
}
