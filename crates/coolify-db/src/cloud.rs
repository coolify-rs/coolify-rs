//! `cloud_provider_tokens` and `cloud_init_scripts`.

use chrono::NaiveDateTime;
use coolify_domain::TeamId;

use crate::records::{Record, parse_row};
use crate::{Db, DbError, DbResult};

fn decode(text: &str) -> DbResult<Record> {
    parse_row(text).map_err(|e| DbError::Schema(format!("decoding cloud row: {e}")))
}

/// `CloudProviderToken::whereTeamId(...)->withCount('servers')`: table order, with `servers_count`.
pub async fn list_tokens(db: &Db, team_id: TeamId) -> DbResult<Vec<Record>> {
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT (row_to_json(t)::jsonb || jsonb_build_object('servers_count', \
           (SELECT count(*) FROM servers s WHERE s.cloud_provider_token_id = t.id AND s.deleted_at IS NULL)))::text \
         FROM cloud_provider_tokens t WHERE t.team_id = $1",
    )
    .bind(team_id)
    .fetch_all(db.pool())
    .await?;
    rows.iter().map(|t| decode(t)).collect()
}

pub async fn find_token(db: &Db, team_id: TeamId, uuid: &str) -> DbResult<Option<Record>> {
    let row: Option<String> = sqlx::query_scalar(
        "SELECT (row_to_json(t)::jsonb || jsonb_build_object('servers_count', \
           (SELECT count(*) FROM servers s WHERE s.cloud_provider_token_id = t.id AND s.deleted_at IS NULL)))::text \
         FROM cloud_provider_tokens t WHERE t.team_id = $1 AND t.uuid = $2 LIMIT 1",
    )
    .bind(team_id)
    .bind(uuid)
    .fetch_optional(db.pool())
    .await?;
    row.as_deref().map(decode).transpose()
}

pub async fn find_token_by_id(db: &Db, id: i64) -> DbResult<Option<Record>> {
    let row: Option<String> = sqlx::query_scalar(
        "SELECT row_to_json(t)::text FROM cloud_provider_tokens t WHERE t.id = $1",
    )
    .bind(id)
    .fetch_optional(db.pool())
    .await?;
    row.as_deref().map(decode).transpose()
}

pub async fn create_token(
    db: &Db,
    team_id: TeamId,
    uuid: &str,
    provider: &str,
    token_encrypted: &str,
    name: &str,
    now: NaiveDateTime,
) -> DbResult<i64> {
    db.ensure_writable()?;
    Ok(sqlx::query_scalar(
        "INSERT INTO cloud_provider_tokens (team_id, provider, token, name, uuid, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $6) RETURNING id",
    )
    .bind(team_id)
    .bind(provider)
    .bind(token_encrypted)
    .bind(name)
    .bind(uuid)
    .bind(now)
    .fetch_one(db.pool())
    .await?)
}

/// The `description` of `CloudProviderToken::create()` in `CloudProviderTokenForm::addToken`.
pub async fn set_token_description(db: &Db, id: i64, description: &str) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query("UPDATE cloud_provider_tokens SET description = $1 WHERE id = $2")
        .bind(description)
        .bind(id)
        .execute(db.pool())
        .await?;
    Ok(())
}

pub async fn update_token_name(db: &Db, id: i64, name: &str, now: NaiveDateTime) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(
        "UPDATE cloud_provider_tokens SET name = $1, updated_at = $2 WHERE id = $3 AND name IS DISTINCT FROM $1",
    )
    .bind(name)
    .bind(now)
    .bind(id)
    .execute(db.pool())
    .await?;
    Ok(())
}

pub async fn delete_token(db: &Db, id: i64) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query("DELETE FROM cloud_provider_tokens WHERE id = $1")
        .bind(id)
        .execute(db.pool())
        .await?;
    Ok(())
}

/// `CloudInitScript::where('team_id', ...)->orderByDesc('created_at')`.
pub async fn list_scripts(db: &Db, team_id: TeamId) -> DbResult<Vec<Record>> {
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT row_to_json(c)::text FROM cloud_init_scripts c WHERE c.team_id = $1 ORDER BY c.created_at DESC",
    )
    .bind(team_id)
    .fetch_all(db.pool())
    .await?;
    rows.iter().map(|t| decode(t)).collect()
}

pub async fn find_script(db: &Db, team_id: TeamId, uuid: &str) -> DbResult<Option<Record>> {
    let row: Option<String> = sqlx::query_scalar(
        "SELECT row_to_json(c)::text FROM cloud_init_scripts c WHERE c.team_id = $1 AND c.uuid = $2 LIMIT 1",
    )
    .bind(team_id)
    .bind(uuid)
    .fetch_optional(db.pool())
    .await?;
    row.as_deref().map(decode).transpose()
}

pub async fn create_script(
    db: &Db,
    team_id: TeamId,
    uuid: &str,
    name: &str,
    script_encrypted: &str,
    now: NaiveDateTime,
) -> DbResult<i64> {
    db.ensure_writable()?;
    Ok(sqlx::query_scalar(
        "INSERT INTO cloud_init_scripts (team_id, name, script, uuid, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $5) RETURNING id",
    )
    .bind(team_id)
    .bind(name)
    .bind(script_encrypted)
    .bind(uuid)
    .bind(now)
    .fetch_one(db.pool())
    .await?)
}

pub async fn update_script(
    db: &Db,
    id: i64,
    name: Option<&str>,
    script_encrypted: Option<&str>,
    now: NaiveDateTime,
) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(
        "UPDATE cloud_init_scripts SET name = COALESCE($1, name), script = COALESCE($2, script), updated_at = $3 WHERE id = $4",
    )
    .bind(name)
    .bind(script_encrypted)
    .bind(now)
    .bind(id)
    .execute(db.pool())
    .await?;
    Ok(())
}

pub async fn delete_script(db: &Db, id: i64) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query("DELETE FROM cloud_init_scripts WHERE id = $1")
        .bind(id)
        .execute(db.pool())
        .await?;
    Ok(())
}
