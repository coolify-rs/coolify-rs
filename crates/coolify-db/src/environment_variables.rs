//! `environment_variables` of resources and `shared_environment_variables`.
//!
//! `environment_variables.value` is stored with PHP's serializing `encrypt()`
//! (the model's value mutator); shared variables use the `encrypted` cast
//! (`encryptString`). Callers encrypt and decrypt with the instance `APP_KEY`.

use chrono::NaiveDateTime;
use coolify_domain::new_public_id;
use serde_json::Value;

use crate::records::{Record, parse_row};
use crate::{Db, DbError, DbResult};

/// A variable a create helper adds (`EnvironmentVariable::create([...])`).
#[derive(Debug, Clone)]
pub struct NewVariable {
    pub key: String,
    /// `encrypt($value)` (serialized).
    pub value_encrypted: Option<String>,
    pub is_shared: bool,
    /// `config('constants.coolify.version')`, written by the model's `created` hook.
    pub version: String,
}

/// A stored variable (value still encrypted).
#[derive(Clone)]
pub struct Variable {
    pub id: i64,
    pub key: String,
    pub value_encrypted: Option<String>,
    pub is_literal: bool,
    pub is_multiline: bool,
    pub is_shared: bool,
    pub is_shown_once: bool,
    pub is_runtime: bool,
    pub is_preview: bool,
    pub uses_legacy_escaping: bool,
    pub record: Record,
}

impl std::fmt::Debug for Variable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Variable")
            .field("id", &self.id)
            .field("key", &self.key)
            .finish_non_exhaustive()
    }
}

pub(crate) async fn insert(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    resource_type: &str,
    resource_id: i64,
    v: &NewVariable,
    now: NaiveDateTime,
) -> DbResult<()> {
    sqlx::query(
        "INSERT INTO environment_variables (key, value, resourceable_type, resourceable_id, is_shared, uuid, version, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $8)",
    )
    .bind(&v.key)
    .bind(&v.value_encrypted)
    .bind(resource_type)
    .bind(resource_id)
    .bind(v.is_shared)
    .bind(new_public_id())
    .bind(&v.version)
    .bind(now)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

fn flag(r: &Record, k: &str) -> bool {
    r.get(k).and_then(Value::as_bool).unwrap_or(false)
}

/// `$resource->environment_variables` (morphMany, table order).
pub async fn for_resource(
    db: &Db,
    resource_type: &str,
    resource_id: i64,
) -> DbResult<Vec<Variable>> {
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT row_to_json(v)::text FROM environment_variables v WHERE resourceable_type = $1 AND resourceable_id = $2",
    )
    .bind(resource_type)
    .bind(resource_id)
    .fetch_all(db.pool())
    .await?;
    rows.iter()
        .map(|t| {
            let r = parse_row(t)
                .map_err(|e| DbError::Schema(format!("decoding environment variable row: {e}")))?;
            Ok(Variable {
                id: r.get("id").and_then(Value::as_i64).unwrap_or_default(),
                key: r
                    .get("key")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                value_encrypted: r.get("value").and_then(Value::as_str).map(str::to_owned),
                is_literal: flag(&r, "is_literal"),
                is_multiline: flag(&r, "is_multiline"),
                is_shared: flag(&r, "is_shared"),
                is_shown_once: flag(&r, "is_shown_once"),
                is_runtime: flag(&r, "is_runtime"),
                is_preview: flag(&r, "is_preview"),
                uses_legacy_escaping: flag(&r, "uses_legacy_escaping"),
                record: r,
            })
        })
        .collect()
}

/// `SharedEnvironmentVariable::where(type, key, team_id, {type}_id)->first()`: `(value, is_shown_once)`,
/// the value still encrypted (`encryptString`).
pub async fn shared(
    db: &Db,
    team_id: i64,
    kind: &str,
    key: &str,
    owner_id: i64,
) -> DbResult<Option<(Option<String>, bool)>> {
    let column = match kind {
        "team" => "team_id",
        "project" => "project_id",
        "environment" => "environment_id",
        "server" => "server_id",
        _ => return Ok(None),
    };
    Ok(sqlx::query_as(&format!(
        "SELECT value, is_shown_once FROM shared_environment_variables WHERE type = $1 AND key = $2 AND team_id = $3 AND {column} = $4 LIMIT 1"
    ))
    .bind(kind)
    .bind(key)
    .bind(team_id)
    .bind(owner_id)
    .fetch_optional(db.pool())
    .await?)
}

/// `$variable->update(['value' => ...])` (the Redis start keeps `REDIS_USERNAME` in sync).
pub async fn update_value(
    db: &Db,
    id: i64,
    value_encrypted: Option<&str>,
    now: NaiveDateTime,
) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query("UPDATE environment_variables SET value = $1, updated_at = $2 WHERE id = $3")
        .bind(value_encrypted)
        .bind(now)
        .bind(id)
        .execute(db.pool())
        .await?;
    Ok(())
}

/// Whether the team has a shown-once shared variable with one of these `(type, key)` pairs
/// (`isLockedForLogs`).
pub async fn references_locked_shared(
    db: &Db,
    team_id: i64,
    references: &[(String, String)],
) -> DbResult<bool> {
    for (kind, key) in references {
        let found: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM shared_environment_variables WHERE team_id = $1 AND key = $2 AND type = $3 AND is_shown_once)",
        )
        .bind(team_id)
        .bind(key)
        .bind(kind)
        .fetch_one(db.pool())
        .await?;
        if found {
            return Ok(true);
        }
    }
    Ok(false)
}

/// `$resource->runtime_environment_variables()->updateOrCreate(['key' => $key], ['value' => ...])`:
/// updates the first variable with that key or creates it. `is_shared` is what the model's
/// `saving` hook derives from the value.
#[allow(clippy::too_many_arguments)]
pub async fn upsert(
    db: &Db,
    resource_type: &str,
    resource_id: i64,
    key: &str,
    value_encrypted: Option<&str>,
    is_shared: bool,
    version: &str,
    now: NaiveDateTime,
) -> DbResult<()> {
    db.ensure_writable()?;
    let id: Option<i64> = sqlx::query_scalar(
        "SELECT id FROM environment_variables WHERE resourceable_type = $1 AND resourceable_id = $2 AND key = $3 ORDER BY id LIMIT 1",
    )
    .bind(resource_type)
    .bind(resource_id)
    .bind(key)
    .fetch_optional(db.pool())
    .await?;
    match id {
        Some(id) => {
            sqlx::query("UPDATE environment_variables SET value = $1, is_shared = $2, updated_at = $3 WHERE id = $4")
                .bind(value_encrypted)
                .bind(is_shared)
                .bind(now)
                .bind(id)
                .execute(db.pool())
                .await?;
        }
        None => {
            let mut tx = db.pool().begin().await?;
            insert(
                &mut tx,
                resource_type,
                resource_id,
                &NewVariable {
                    key: key.to_owned(),
                    value_encrypted: value_encrypted.map(str::to_owned),
                    is_shared,
                    version: version.to_owned(),
                },
                now,
            )
            .await?;
            tx.commit().await?;
        }
    }
    Ok(())
}
