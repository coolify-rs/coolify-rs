//! Queries behind `Project\Shared\EnvironmentVariable\All` and `Show`: the rows of the variables
//! table of a resource and the model writes of `EnvironmentVariable`.

use chrono::NaiveDateTime;
use coolify_domain::new_public_id;
use serde_json::Value;

use crate::records::{Record, parse_row};
use crate::{Db, DbError, DbResult};

fn decode(text: &str) -> DbResult<Record> {
    parse_row(text).map_err(|e| DbError::Schema(format!("decoding environment variable row: {e}")))
}

/// What `managedEnvironmentVariablesQuery` filters and sorts by.
#[derive(Debug, Default)]
pub struct ManagedQuery<'a> {
    pub is_preview: bool,
    pub hidden_keys: &'a [String],
    pub missing_required_ids: &'a [i64],
    pub search: &'a str,
    pub filters: &'a [String],
    pub services_filtered: bool,
    pub sort: &'a str,
    pub alphabetical: bool,
}

const MAGIC: &str =
    "(key LIKE 'SERVICE_FQDN%' OR key LIKE 'SERVICE_URL%' OR key LIKE 'SERVICE_NAME%')";

/// `managedEnvironmentVariablesQuery($isPreview)->get()`.
pub async fn managed(db: &Db, morph: &str, id: i64, q: &ManagedQuery<'_>) -> DbResult<Vec<Record>> {
    let mut wheres = vec![
        "resourceable_type = $1".to_owned(),
        "resourceable_id = $2".to_owned(),
        "is_preview = $3".to_owned(),
        "NOT (key = ANY($4))".to_owned(),
    ];
    if q.services_filtered {
        wheres.push("1 = 0".into());
    }
    let has = |f: &str| q.filters.iter().any(|x| x == f);
    let search = q.search.trim();
    let needle = format!(
        "%{}%",
        search
            .to_lowercase()
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_")
    );
    if !search.is_empty() {
        wheres.push("LOWER(key) LIKE $6 ESCAPE '\\'".into());
    }
    if has("managed") {
        wheres.push(MAGIC.into());
    } else if has("user") {
        wheres.push(format!("NOT {MAGIC}"));
    }
    for f in ["buildtime", "runtime", "multiline", "literal"] {
        if has(f) {
            wheres.push(format!("is_{f} = true"));
        }
    }
    let tail = match q.sort {
        "name_asc" => "key",
        "name_desc" => "key DESC",
        _ if q.alphabetical => "key",
        _ => "\"order\", id",
    };
    let sql = format!(
        "SELECT row_to_json(v)::text FROM environment_variables v WHERE {} \
         ORDER BY CASE WHEN id = ANY($5) THEN 0 ELSE 1 END, CASE WHEN {MAGIC} THEN 0 ELSE 1 END, {tail}",
        wheres.join(" AND ")
    );
    let mut query = sqlx::query_scalar::<_, String>(&sql)
        .bind(morph)
        .bind(id)
        .bind(q.is_preview)
        .bind(q.hidden_keys)
        .bind(q.missing_required_ids);
    if !search.is_empty() {
        query = query.bind(needle);
    }
    let rows = query.fetch_all(db.pool()).await?;
    rows.iter().map(|t| decode(t)).collect()
}

/// `$resource->environment_variables()` or `environment_variables_preview()`, in the order of
/// `getEnvironmentVariables` (required without a value first, then key or `order`).
pub async fn scoped(
    db: &Db,
    morph: &str,
    id: i64,
    is_preview: bool,
    alphabetical: bool,
) -> DbResult<Vec<Record>> {
    let tail = if alphabetical { "key" } else { "\"order\"" };
    let rows: Vec<String> = sqlx::query_scalar(&format!(
        "SELECT row_to_json(v)::text FROM environment_variables v WHERE resourceable_type = $1 AND resourceable_id = $2 AND is_preview = $3 \
         ORDER BY CASE WHEN is_required = true AND (value IS NULL OR value = '') THEN 0 ELSE 1 END, {tail}"
    ))
    .bind(morph)
    .bind(id)
    .bind(is_preview)
    .fetch_all(db.pool())
    .await?;
    rows.iter().map(|t| decode(t)).collect()
}

pub async fn find(db: &Db, morph: &str, resource_id: i64, id: i64) -> DbResult<Option<Record>> {
    let row: Option<String> = sqlx::query_scalar(
        "SELECT row_to_json(v)::text FROM environment_variables v WHERE resourceable_type = $1 AND resourceable_id = $2 AND id = $3",
    )
    .bind(morph)
    .bind(resource_id)
    .bind(id)
    .fetch_optional(db.pool())
    .await?;
    row.map(|t| decode(&t)).transpose()
}

pub async fn by_key(
    db: &Db,
    morph: &str,
    resource_id: i64,
    is_preview: bool,
    key: &str,
) -> DbResult<Option<Record>> {
    let row: Option<String> = sqlx::query_scalar(
        "SELECT row_to_json(v)::text FROM environment_variables v WHERE resourceable_type = $1 AND resourceable_id = $2 AND is_preview = $3 AND key = $4 ORDER BY id LIMIT 1",
    )
    .bind(morph)
    .bind(resource_id)
    .bind(is_preview)
    .bind(key)
    .fetch_optional(db.pool())
    .await?;
    row.map(|t| decode(&t)).transpose()
}

/// `$resource->environment_variables()->max('order')`.
pub async fn max_order(db: &Db, morph: &str, resource_id: i64) -> DbResult<i64> {
    let max: Option<i32> = sqlx::query_scalar(
        "SELECT MAX(\"order\") FROM environment_variables WHERE resourceable_type = $1 AND resourceable_id = $2 AND is_preview = false",
    )
    .bind(morph)
    .bind(resource_id)
    .fetch_one(db.pool())
    .await?;
    Ok(i64::from(max.unwrap_or(0)))
}

/// One new `environment_variables` row.
#[derive(Debug, Clone)]
pub struct NewEnv<'a> {
    pub morph: &'a str,
    pub resource_id: i64,
    pub key: &'a str,
    pub value_encrypted: Option<&'a str>,
    pub comment: Option<&'a str>,
    pub is_multiline: bool,
    pub is_literal: bool,
    pub is_runtime: bool,
    pub is_buildtime: bool,
    pub is_required: bool,
    pub is_shown_once: bool,
    pub is_preview: bool,
    pub is_shared: bool,
    pub order: Option<i64>,
    pub version: &'a str,
}

pub async fn create(db: &Db, n: &NewEnv<'_>, now: NaiveDateTime) -> DbResult<Record> {
    db.ensure_writable()?;
    let text: String = sqlx::query_scalar(
        "INSERT INTO environment_variables (key, value, comment, is_multiline, is_literal, is_runtime, is_buildtime, is_required, is_shown_once, is_preview, is_shared, \"order\", resourceable_type, resourceable_id, uuid, version, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12::integer, $13, $14, $15, $16, $17, $17) RETURNING row_to_json(environment_variables)::text",
    )
    .bind(n.key)
    .bind(n.value_encrypted)
    .bind(n.comment)
    .bind(n.is_multiline)
    .bind(n.is_literal)
    .bind(n.is_runtime)
    .bind(n.is_buildtime)
    .bind(n.is_required)
    .bind(n.is_shown_once)
    .bind(n.is_preview)
    .bind(n.is_shared)
    .bind(n.order)
    .bind(n.morph)
    .bind(n.resource_id)
    .bind(new_public_id())
    .bind(n.version)
    .bind(now)
    .fetch_one(db.pool())
    .await?;
    decode(&text)
}

/// `$env->order = $n; $env->save();`.
pub async fn set_order(db: &Db, id: i64, order: i64, now: NaiveDateTime) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(
        "UPDATE environment_variables SET \"order\" = $1::integer, updated_at = $2 WHERE id = $3 AND \"order\" IS DISTINCT FROM $1::integer",
    )
    .bind(order)
    .bind(now)
    .bind(id)
    .execute(db.pool())
    .await?;
    Ok(())
}

pub async fn delete_ids(db: &Db, ids: &[i64]) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query("DELETE FROM environment_variables WHERE id = ANY($1)")
        .bind(ids)
        .execute(db.pool())
        .await?;
    Ok(())
}

/// The keys of a shared variable scope (`$team->environment_variables()->pluck('key')`).
pub async fn shared_keys(
    db: &Db,
    team_id: i64,
    kind: &str,
    owner: Option<(&str, i64)>,
) -> DbResult<Vec<String>> {
    let sql = match owner {
        Some((column, _)) => format!(
            "SELECT key FROM shared_environment_variables WHERE team_id = $1 AND type = $2 AND {column} = $3 ORDER BY id"
        ),
        None => "SELECT key FROM shared_environment_variables WHERE team_id = $1 AND type = $2 ORDER BY id".to_owned(),
    };
    let mut q = sqlx::query_scalar::<_, String>(&sql)
        .bind(team_id)
        .bind(kind);
    if let Some((_, id)) = owner {
        q = q.bind(id);
    }
    Ok(q.fetch_all(db.pool()).await?)
}

/// `$resource->secretManagerLink` exists, and how many secret manager tokens the team has
/// (`IntegrationToken::SECRET_MANAGER_PROVIDERS` with the `secrets` capability).
pub async fn secret_manager_state(
    db: &Db,
    morph: &str,
    resource_id: i64,
    team_id: i64,
) -> DbResult<(bool, i64)> {
    let linked: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM secret_manager_links WHERE resourceable_type = $1 AND resourceable_id = $2)",
    )
    .bind(morph)
    .bind(resource_id)
    .fetch_one(db.pool())
    .await?;
    let tokens: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM integration_tokens WHERE team_id = $1 AND provider IN ('doppler', 'infisical', 'vault') AND capabilities::jsonb ? 'secrets'",
    )
    .bind(team_id)
    .fetch_one(db.pool())
    .await?;
    Ok((linked, tokens))
}

/// The service of a `service_applications` or `service_databases` row, by uuid.
pub async fn sub_service_id(db: &Db, table: &str, uuid: &str) -> DbResult<Option<i64>> {
    Ok(sqlx::query_scalar(&format!(
        "SELECT service_id FROM {table} WHERE uuid = $1 AND deleted_at IS NULL LIMIT 1"
    ))
    .bind(uuid)
    .fetch_optional(db.pool())
    .await?)
}

pub fn flag(r: &Record, k: &str) -> bool {
    r.get(k).and_then(Value::as_bool).unwrap_or(false)
}
