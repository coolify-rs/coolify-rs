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

/// `EnvironmentVariable::GENERATED_MAGIC_VARIABLE_COMMANDS`: magic variable commands
/// (`SERVICE_<COMMAND>_<NAME>`) whose value `generateEnvValue()` generates.
pub const GENERATED_MAGIC_VARIABLE_COMMANDS: [&str; 19] = [
    "PASSWORD",
    "PASSWORD_64",
    "PASSWORDWITHSYMBOLS",
    "PASSWORDWITHSYMBOLS_64",
    "BASE64",
    "BASE64_32",
    "BASE64_64",
    "BASE64_128",
    "REALBASE64",
    "REALBASE64_32",
    "REALBASE64_64",
    "REALBASE64_128",
    "HEX_32",
    "HEX_64",
    "HEX_128",
    "USER",
    "LOWERCASEUSER",
    "SUPABASEANON",
    "SUPABASESERVICE",
];

/// `parseCommandFromMagicEnvVariable($key)->value()`.
pub fn parse_command_from_magic_env_variable(key: &str) -> String {
    // `Str::after()` / `Str::before()` / `Str::beforeLast()`: the whole string when not found.
    let after = |s: &str, search: &str| {
        s.split_once(search)
            .map_or_else(|| s.to_owned(), |(_, a)| a.to_owned())
    };
    let before = |s: &str, search: &str| {
        s.split_once(search)
            .map_or_else(|| s.to_owned(), |(b, _)| b.to_owned())
    };
    let before_last = |s: &str, search: &str| {
        s.rfind(search)
            .map_or_else(|| s.to_owned(), |p| s[..p].to_owned())
    };
    let count = key.matches('_').count();
    let fqdn_or_url = key.starts_with("SERVICE_FQDN") || key.starts_with("SERVICE_URL");
    match count {
        2 => before_last(&after(key, "SERVICE_"), "_"),
        3 if fqdn_or_url => before(&after(key, "SERVICE_"), "_"),
        3 => before_last(&after(key, "SERVICE_"), "_"),
        _ => String::new(),
    }
}

/// `EnvironmentVariable::isGeneratedMagicVariable()`.
pub fn is_generated_magic_variable(key: &str) -> bool {
    key.starts_with("SERVICE_")
        && GENERATED_MAGIC_VARIABLE_COMMANDS
            .contains(&parse_command_from_magic_env_variable(key).as_str())
}

/// The check of the `created` hook of `EnvironmentVariable` before it copies a new production
/// variable of an application to the preview: `Application::find($id)` exists and is not a
/// `dockercompose` application with a generated magic variable (a compose preview generates its
/// own magic secrets on the first pull request parse).
pub async fn application_copies_to_preview<'e, E>(
    executor: E,
    application_id: i64,
    key: &str,
) -> DbResult<bool>
where
    E: sqlx::PgExecutor<'e>,
{
    let build_pack: Option<String> = sqlx::query_scalar(
        "SELECT build_pack FROM applications WHERE id = $1 AND deleted_at IS NULL",
    )
    .bind(application_id)
    .fetch_optional(executor)
    .await?;
    Ok(build_pack.is_some_and(|b| !(b == "dockercompose" && is_generated_magic_variable(key))))
}

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

/// `EnvironmentVariable::create([... 'is_buildtime' => true, 'is_runtime' => false ...])` on an
/// application (the `created` hook of a Nixpacks application) and the preview copy the
/// variable's own `created` hook adds (`is_preview = true`, same flags, value re-encrypted).
/// `version` is what both `created` hooks then write.
#[allow(clippy::too_many_arguments)]
pub async fn create_for_application(
    db: &Db,
    application_id: i64,
    key: &str,
    value_encrypted: &str,
    preview_value_encrypted: &str,
    version: &str,
    now: NaiveDateTime,
) -> DbResult<()> {
    db.ensure_writable()?;
    let mut tx = db.pool().begin().await?;
    for (value, is_preview) in [(value_encrypted, false), (preview_value_encrypted, true)] {
        sqlx::query(
            "INSERT INTO environment_variables (key, value, is_multiline, is_literal, is_buildtime, is_runtime, is_preview, \
             is_required, is_shown_once, comment, resourceable_type, resourceable_id, is_shared, uuid, version, created_at, updated_at) \
             VALUES ($1, $2, false, false, true, false, $3, false, false, NULL, $4, $5, false, $6, $7, $8, $8)",
        )
        .bind(key)
        .bind(value)
        .bind(is_preview)
        .bind(crate::applications::MORPH)
        .bind(application_id)
        .bind(new_public_id())
        .bind(version)
        .bind(now)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
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
        "SELECT row_to_json(v)::text FROM environment_variables v WHERE resourceable_type = $1 AND resourceable_id = $2 ORDER BY id",
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

// ---------------------------------------------------------------------------
// The resource API (`envs`, `create_env`, `update_env_by_uuid`, `create_bulk_envs`,
// `delete_env_by_uuid` of the application, database and service controllers).

/// The attributes `$resource->environment_variables()->create([...])` passes. `value_encrypted`
/// is `encrypt($value)`; `is_shared` is what the `saving` hook derives from the value.
#[derive(Clone)]
pub struct NewResourceVariable {
    pub key: String,
    pub value_encrypted: Option<String>,
    pub is_preview: bool,
    pub is_literal: bool,
    pub is_multiline: bool,
    pub is_shown_once: bool,
    pub is_runtime: bool,
    pub is_buildtime: bool,
    pub comment: Option<String>,
    pub is_shared: bool,
}

impl std::fmt::Debug for NewResourceVariable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NewResourceVariable")
            .field("key", &self.key)
            .field("is_preview", &self.is_preview)
            .finish_non_exhaustive()
    }
}

fn decode_row(t: &str) -> DbResult<Record> {
    parse_row(t).map_err(|e| DbError::Schema(format!("decoding environment variable row: {e}")))
}

async fn insert_resource_variable(
    db: &Db,
    resource_type: &str,
    resource_id: i64,
    v: &NewResourceVariable,
    version: &str,
    now: NaiveDateTime,
) -> DbResult<Record> {
    let row: String = sqlx::query_scalar(
        "INSERT INTO environment_variables (key, value, is_preview, is_literal, is_multiline, is_shown_once, is_runtime, is_buildtime, \
         comment, resourceable_type, resourceable_id, is_shared, uuid, version, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $15) RETURNING row_to_json(environment_variables)::text",
    )
    .bind(&v.key)
    .bind(&v.value_encrypted)
    .bind(v.is_preview)
    .bind(v.is_literal)
    .bind(v.is_multiline)
    .bind(v.is_shown_once)
    .bind(v.is_runtime)
    .bind(v.is_buildtime)
    .bind(&v.comment)
    .bind(resource_type)
    .bind(resource_id)
    .bind(v.is_shared)
    .bind(new_public_id())
    .bind(version)
    .bind(now)
    .fetch_one(db.pool())
    .await?;
    decode_row(&row)
}

/// `EnvironmentVariable::create(...)` with the model's `created` hook: a non-preview variable of
/// an application also gets a preview copy unless one with the key exists (or
/// [`application_copies_to_preview`] says no). `preview_value_encrypted` is the copy's own
/// `encrypt($value)`. Returns the row of the created variable.
pub async fn create_on_resource(
    db: &Db,
    (resource_type, resource_id): (&str, i64),
    v: &NewResourceVariable,
    preview_value_encrypted: Option<&str>,
    version: &str,
    now: NaiveDateTime,
) -> DbResult<Record> {
    db.ensure_writable()?;
    let row = insert_resource_variable(db, resource_type, resource_id, v, version, now).await?;
    if resource_type == crate::applications::MORPH
        && !v.is_preview
        && first_by_key(db, resource_type, resource_id, &v.key, true)
            .await?
            .is_none()
        && application_copies_to_preview(db.pool(), resource_id, &v.key).await?
    {
        let copy = NewResourceVariable {
            is_preview: true,
            value_encrypted: preview_value_encrypted.map(str::to_owned),
            ..v.clone()
        };
        insert_resource_variable(db, resource_type, resource_id, &copy, version, now).await?;
    }
    Ok(row)
}

/// `$resource->environment_variables` / `environment_variables_preview`, `->sortBy('id')`.
pub async fn rows_for_resource(
    db: &Db,
    resource_type: &str,
    resource_id: i64,
    is_preview: bool,
) -> DbResult<Vec<Record>> {
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT row_to_json(v)::text FROM environment_variables v \
         WHERE resourceable_type = $1 AND resourceable_id = $2 AND is_preview = $3 ORDER BY id",
    )
    .bind(resource_type)
    .bind(resource_id)
    .bind(is_preview)
    .fetch_all(db.pool())
    .await?;
    rows.iter().map(|t| decode_row(t)).collect()
}

/// `$resource->environment_variables[_preview]()->where('key', $key)->first()`.
pub async fn first_by_key(
    db: &Db,
    resource_type: &str,
    resource_id: i64,
    key: &str,
    is_preview: bool,
) -> DbResult<Option<Record>> {
    let row: Option<String> = sqlx::query_scalar(
        "SELECT row_to_json(v)::text FROM environment_variables v \
         WHERE resourceable_type = $1 AND resourceable_id = $2 AND key = $3 AND is_preview = $4 LIMIT 1",
    )
    .bind(resource_type)
    .bind(resource_id)
    .bind(key)
    .bind(is_preview)
    .fetch_optional(db.pool())
    .await?;
    row.map(|t| decode_row(&t)).transpose()
}

/// `EnvironmentVariable::where('uuid', ...)->where('resourceable_type', ...)->where('resourceable_id', ...)->first()`:
/// `(id, key)`.
pub async fn id_by_uuid(
    db: &Db,
    resource_type: &str,
    resource_id: i64,
    uuid: &str,
) -> DbResult<Option<(i64, String)>> {
    Ok(sqlx::query_as(
        "SELECT id, key FROM environment_variables WHERE uuid = $1 AND resourceable_type = $2 AND resourceable_id = $3 LIMIT 1",
    )
    .bind(uuid)
    .bind(resource_type)
    .bind(resource_id)
    .fetch_optional(db.pool())
    .await?)
}

/// `$env->save()` with the dirty columns; without any, nothing is written. Returns the row.
pub async fn save_dirty(
    db: &Db,
    id: i64,
    dirty: &[(&'static str, crate::servers::ColumnValue)],
    now: NaiveDateTime,
) -> DbResult<Record> {
    if !dirty.is_empty() {
        db.ensure_writable()?;
        let set = dirty
            .iter()
            .enumerate()
            .map(|(i, (c, _))| format!("\"{c}\" = ${}", i + 3))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!("UPDATE environment_variables SET {set}, updated_at = $1 WHERE id = $2");
        crate::servers::bind_all(sqlx::query(&sql).bind(now).bind(id), dirty)
            .execute(db.pool())
            .await?;
    }
    let row: String = sqlx::query_scalar(
        "SELECT row_to_json(v)::text FROM environment_variables v WHERE id = $1",
    )
    .bind(id)
    .fetch_one(db.pool())
    .await?;
    decode_row(&row)
}

/// `$env->forceDelete()`.
pub async fn force_delete(db: &Db, id: i64) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query("DELETE FROM environment_variables WHERE id = $1")
        .bind(id)
        .execute(db.pool())
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_magic_variables() {
        for key in [
            "SERVICE_PASSWORD_DB",
            "SERVICE_PASSWORD_64_DB",
            "SERVICE_PASSWORDWITHSYMBOLS_APP",
            "SERVICE_BASE64_64_WEB",
            "SERVICE_REALBASE64_APP",
            "SERVICE_HEX_32_WEB",
            "SERVICE_USER_ADMIN",
            "SERVICE_LOWERCASEUSER_BACK",
            "SERVICE_SUPABASEANON_KONG",
        ] {
            assert!(is_generated_magic_variable(key), "{key}");
        }
        for key in [
            "SERVICE_URL_WEB",
            "SERVICE_FQDN_WEB_3000",
            "SERVICE_URL_MY_APP_80",
            "SERVICE_NAME_DB",
            "PASSWORD_DB",
            "SERVICE_PASSWORD",
            "SERVICE_PASSWORD_MY_LONG_NAME",
        ] {
            assert!(!is_generated_magic_variable(key), "{key}");
        }
        assert_eq!(
            parse_command_from_magic_env_variable("SERVICE_FQDN_API_8080"),
            "FQDN"
        );
        assert_eq!(
            parse_command_from_magic_env_variable("SERVICE_HEX_64_X"),
            "HEX_64"
        );
    }
}
