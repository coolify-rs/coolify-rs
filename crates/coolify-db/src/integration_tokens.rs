//! `integration_tokens`.

use chrono::NaiveDateTime;

use crate::records::{Record, parse_row};
use crate::{Db, DbError, DbResult};

/// `IntegrationToken::query()->create([...])`.
#[derive(Debug)]
pub struct NewToken<'a> {
    pub uuid: &'a str,
    pub team_id: i64,
    pub provider: &'a str,
    pub name: &'a str,
    pub token_encrypted: &'a str,
    pub capabilities: &'a str,
    pub metadata: Option<&'a str>,
}

pub async fn create(db: &Db, n: &NewToken<'_>, now: NaiveDateTime) -> DbResult<i64> {
    db.ensure_writable()?;
    Ok(sqlx::query_scalar(
        "INSERT INTO integration_tokens (uuid, team_id, provider, name, token, capabilities, metadata, created_at, updated_at) \
         VALUES ($1,$2,$3,$4,$5,$6::json,$7::json,$8,$8) RETURNING id",
    )
    .bind(n.uuid)
    .bind(n.team_id)
    .bind(n.provider)
    .bind(n.name)
    .bind(n.token_encrypted)
    .bind(n.capabilities)
    .bind(n.metadata)
    .bind(now)
    .fetch_one(db.pool())
    .await?)
}

fn decode(text: &str) -> DbResult<Record> {
    parse_row(text).map_err(|e| DbError::Schema(format!("decoding integration token: {e}")))
}

/// Every token of the team, newest last.
pub async fn list(db: &Db, team_id: i64) -> DbResult<Vec<Record>> {
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT row_to_json(t)::text FROM integration_tokens t WHERE team_id = $1 ORDER BY id",
    )
    .bind(team_id)
    .fetch_all(db.pool())
    .await?;
    rows.iter().map(|t| decode(t)).collect()
}

pub async fn find(db: &Db, team_id: i64, uuid: &str) -> DbResult<Option<Record>> {
    let row: Option<String> = sqlx::query_scalar(
        "SELECT row_to_json(t)::text FROM integration_tokens t WHERE team_id = $1 AND uuid = $2",
    )
    .bind(team_id)
    .bind(uuid)
    .fetch_optional(db.pool())
    .await?;
    row.as_deref().map(decode).transpose()
}

pub async fn delete(db: &Db, id: i64) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query("DELETE FROM integration_tokens WHERE id = $1")
        .bind(id)
        .execute(db.pool())
        .await?;
    Ok(())
}
