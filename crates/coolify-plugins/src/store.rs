//! The `coolify_rs_plugins` schema (ADR 0010, section 5). Coolify's migrations and coolify-rs's schema guard
//! only look at `public`, so this schema is invisible to both; dropping it removes every trace of plugins.
//! Nothing here references Coolify's tables, so deleting a team in Coolify is never blocked by a plugin.

use coolify_db::Db;
use serde_json::Value;
use sqlx::Row;

use crate::PluginError;
use crate::manifest::{Capabilities, Manifest};

const SCHEMA: &str = "
CREATE SCHEMA IF NOT EXISTS coolify_rs_plugins;
CREATE TABLE IF NOT EXISTS coolify_rs_plugins.plugins (
    id text PRIMARY KEY,
    version text NOT NULL,
    manifest jsonb NOT NULL,
    granted jsonb NOT NULL,
    sha256 text NOT NULL,
    component bytea NOT NULL,
    origin text NOT NULL,
    verified boolean NOT NULL DEFAULT false,
    enabled boolean NOT NULL DEFAULT true,
    disabled_reason text,
    installed_by bigint,
    installed_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE TABLE IF NOT EXISTS coolify_rs_plugins.team_plugins (
    plugin_id text NOT NULL REFERENCES coolify_rs_plugins.plugins (id) ON DELETE CASCADE,
    team_id bigint NOT NULL,
    enabled_by bigint,
    enabled_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (plugin_id, team_id)
);
CREATE TABLE IF NOT EXISTS coolify_rs_plugins.storage (
    plugin_id text NOT NULL REFERENCES coolify_rs_plugins.plugins (id) ON DELETE CASCADE,
    team_id bigint NOT NULL,
    key text NOT NULL,
    value bytea NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (plugin_id, team_id, key)
);
CREATE TABLE IF NOT EXISTS coolify_rs_plugins.audit (
    id bigserial PRIMARY KEY,
    at timestamptz NOT NULL DEFAULT now(),
    plugin_id text NOT NULL,
    team_id bigint,
    user_id bigint,
    action text NOT NULL,
    detail jsonb NOT NULL DEFAULT '{}'
);
";

/// Creates the schema when missing.
pub async fn migrate(db: &Db) -> Result<(), PluginError> {
    sqlx::raw_sql(SCHEMA).execute(db.pool()).await?;
    crate::registry::db::migrate(db).await
}

/// An installed plugin.
#[derive(Debug, Clone)]
pub struct Installed {
    pub id: String,
    pub version: String,
    pub manifest: Manifest,
    /// What an admin granted; never more than the manifest asked for when it was granted.
    pub granted: Capabilities,
    pub sha256: String,
    pub origin: String,
    pub verified: bool,
    pub enabled: bool,
    pub disabled_reason: Option<String>,
}

fn installed(row: &sqlx::postgres::PgRow) -> Result<Installed, PluginError> {
    let manifest: Value = row.try_get("manifest")?;
    let granted: Value = row.try_get("granted")?;
    Ok(Installed {
        id: row.try_get("id")?,
        version: row.try_get("version")?,
        manifest: serde_json::from_value(manifest)
            .map_err(|e| PluginError::Manifest(e.to_string()))?,
        granted: serde_json::from_value(granted)
            .map_err(|e| PluginError::Manifest(e.to_string()))?,
        sha256: row.try_get("sha256")?,
        origin: row.try_get("origin")?,
        verified: row.try_get("verified")?,
        enabled: row.try_get("enabled")?,
        disabled_reason: row.try_get("disabled_reason")?,
    })
}

const COLUMNS: &str =
    "id, version, manifest, granted, sha256, origin, verified, enabled, disabled_reason";

pub async fn list(db: &Db) -> Result<Vec<Installed>, PluginError> {
    let rows = sqlx::query(&format!(
        "SELECT {COLUMNS} FROM coolify_rs_plugins.plugins ORDER BY id"
    ))
    .fetch_all(db.pool())
    .await?;
    rows.iter().map(installed).collect()
}

pub async fn find(db: &Db, id: &str) -> Result<Option<Installed>, PluginError> {
    let row = sqlx::query(&format!(
        "SELECT {COLUMNS} FROM coolify_rs_plugins.plugins WHERE id = $1"
    ))
    .bind(id)
    .fetch_optional(db.pool())
    .await?;
    row.as_ref().map(installed).transpose()
}

pub async fn component(db: &Db, id: &str) -> Result<Option<Vec<u8>>, PluginError> {
    Ok(
        sqlx::query_scalar("SELECT component FROM coolify_rs_plugins.plugins WHERE id = $1")
            .bind(id)
            .fetch_optional(db.pool())
            .await?,
    )
}

#[derive(Debug)]
pub struct NewPlugin<'a> {
    pub manifest: &'a Manifest,
    pub granted: &'a Capabilities,
    pub sha256: &'a str,
    pub component: &'a [u8],
    pub origin: &'a str,
    pub verified: bool,
    pub user_id: Option<i64>,
}

/// Installs or replaces a plugin. A replacement keeps per-team enablement and storage.
pub async fn upsert(db: &Db, p: NewPlugin<'_>) -> Result<(), PluginError> {
    let manifest = serde_json::to_value(p.manifest).unwrap_or_default();
    let granted = serde_json::to_value(p.granted).unwrap_or_default();
    sqlx::query(
        "INSERT INTO coolify_rs_plugins.plugins \
         (id, version, manifest, granted, sha256, component, origin, verified, installed_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) \
         ON CONFLICT (id) DO UPDATE SET version = $2, manifest = $3, granted = $4, sha256 = $5, \
         component = $6, origin = $7, verified = $8, enabled = true, disabled_reason = NULL, updated_at = now()",
    )
    .bind(&p.manifest.id)
    .bind(&p.manifest.version)
    .bind(manifest)
    .bind(granted)
    .bind(p.sha256)
    .bind(p.component)
    .bind(p.origin)
    .bind(p.verified)
    .bind(p.user_id)
    .execute(db.pool())
    .await?;
    Ok(())
}

pub async fn delete(db: &Db, id: &str) -> Result<bool, PluginError> {
    let n = sqlx::query("DELETE FROM coolify_rs_plugins.plugins WHERE id = $1")
        .bind(id)
        .execute(db.pool())
        .await?
        .rows_affected();
    Ok(n > 0)
}

/// Instance-wide on or off; `reason` says why when switched off automatically.
pub async fn set_enabled(
    db: &Db,
    id: &str,
    enabled: bool,
    reason: Option<&str>,
) -> Result<(), PluginError> {
    sqlx::query(
        "UPDATE coolify_rs_plugins.plugins SET enabled = $2, disabled_reason = $3, updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(enabled)
    .bind(reason)
    .execute(db.pool())
    .await?;
    Ok(())
}

pub async fn set_team(
    db: &Db,
    id: &str,
    team_id: i64,
    enabled: bool,
    user_id: Option<i64>,
) -> Result<(), PluginError> {
    if enabled {
        sqlx::query(
            "INSERT INTO coolify_rs_plugins.team_plugins (plugin_id, team_id, enabled_by) VALUES ($1, $2, $3) \
             ON CONFLICT DO NOTHING",
        )
        .bind(id)
        .bind(team_id)
        .bind(user_id)
        .execute(db.pool())
        .await?;
    } else {
        sqlx::query(
            "DELETE FROM coolify_rs_plugins.team_plugins WHERE plugin_id = $1 AND team_id = $2",
        )
        .bind(id)
        .bind(team_id)
        .execute(db.pool())
        .await?;
    }
    Ok(())
}

pub async fn enabled_for_team(db: &Db, id: &str, team_id: i64) -> Result<bool, PluginError> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM coolify_rs_plugins.team_plugins WHERE plugin_id = $1 AND team_id = $2)",
    )
    .bind(id)
    .bind(team_id)
    .fetch_one(db.pool())
    .await?)
}

/// Plugins enabled instance-wide and for the team.
pub async fn ids_for_team(db: &Db, team_id: i64) -> Result<Vec<String>, PluginError> {
    Ok(sqlx::query_scalar(
        "SELECT p.id FROM coolify_rs_plugins.plugins p JOIN coolify_rs_plugins.team_plugins t ON t.plugin_id = p.id \
         WHERE t.team_id = $1 AND p.enabled ORDER BY p.id",
    )
    .bind(team_id)
    .fetch_all(db.pool())
    .await?)
}

pub async fn audit(
    db: &Db,
    plugin_id: &str,
    team_id: Option<i64>,
    user_id: Option<i64>,
    action: &str,
    detail: Value,
) -> Result<(), PluginError> {
    sqlx::query(
        "INSERT INTO coolify_rs_plugins.audit (plugin_id, team_id, user_id, action, detail) VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(plugin_id)
    .bind(team_id)
    .bind(user_id)
    .bind(action)
    .bind(detail)
    .execute(db.pool())
    .await?;
    Ok(())
}

// Key-value storage of one plugin and team.

pub async fn kv_get(
    db: &Db,
    plugin: &str,
    team: i64,
    key: &str,
) -> Result<Option<Vec<u8>>, PluginError> {
    Ok(sqlx::query_scalar(
        "SELECT value FROM coolify_rs_plugins.storage WHERE plugin_id = $1 AND team_id = $2 AND key = $3",
    )
    .bind(plugin)
    .bind(team)
    .bind(key)
    .fetch_optional(db.pool())
    .await?)
}

/// Writes a value unless the team's total for the plugin would pass `quota` bytes.
pub async fn kv_set(
    db: &Db,
    plugin: &str,
    team: i64,
    key: &str,
    value: &[u8],
    quota: i64,
) -> Result<bool, PluginError> {
    let mut tx = db.pool().begin().await?;
    // Serialize writers of this plugin and team so two writes can't both pass the quota check.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, $2))")
        .bind(plugin)
        .bind(team)
        .execute(&mut *tx)
        .await?;
    let used: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(octet_length(key) + octet_length(value)), 0)::bigint FROM coolify_rs_plugins.storage \
         WHERE plugin_id = $1 AND team_id = $2 AND key <> $3",
    )
    .bind(plugin)
    .bind(team)
    .bind(key)
    .fetch_one(&mut *tx)
    .await?;
    if used + (key.len() + value.len()) as i64 > quota {
        return Ok(false);
    }
    sqlx::query(
        "INSERT INTO coolify_rs_plugins.storage (plugin_id, team_id, key, value) VALUES ($1, $2, $3, $4) \
         ON CONFLICT (plugin_id, team_id, key) DO UPDATE SET value = $4, updated_at = now()",
    )
    .bind(plugin)
    .bind(team)
    .bind(key)
    .bind(value)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(true)
}

pub async fn kv_delete(db: &Db, plugin: &str, team: i64, key: &str) -> Result<(), PluginError> {
    sqlx::query(
        "DELETE FROM coolify_rs_plugins.storage WHERE plugin_id = $1 AND team_id = $2 AND key = $3",
    )
    .bind(plugin)
    .bind(team)
    .bind(key)
    .execute(db.pool())
    .await?;
    Ok(())
}

pub async fn kv_keys(
    db: &Db,
    plugin: &str,
    team: i64,
    prefix: &str,
) -> Result<Vec<String>, PluginError> {
    // `starts_with` instead of LIKE, so `%` and `_` in the prefix match literally.
    Ok(sqlx::query_scalar(
        "SELECT key FROM coolify_rs_plugins.storage WHERE plugin_id = $1 AND team_id = $2 \
         AND starts_with(key, $3) ORDER BY key LIMIT 1000",
    )
    .bind(plugin)
    .bind(team)
    .bind(prefix)
    .fetch_all(db.pool())
    .await?)
}
