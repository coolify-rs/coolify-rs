//! `shared_environment_variables` for every scope, and `tags`.

use chrono::NaiveDateTime;

use crate::records::{Record, parse_row};
use crate::{Db, DbError, DbResult};

/// The scope of a shared variable: its `type` and the owning row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Team,
    Project(i64),
    Environment(i64),
    Server(i64),
}

impl Scope {
    pub fn kind(self) -> &'static str {
        match self {
            Self::Team => "team",
            Self::Project(_) => "project",
            Self::Environment(_) => "environment",
            Self::Server(_) => "server",
        }
    }

    /// `findEnvInScope` compares only the scope's own column.
    fn sql(self) -> String {
        match self {
            Self::Team => String::new(),
            Self::Project(p) => format!(" AND project_id = {p}"),
            Self::Environment(e) => format!(" AND environment_id = {e}"),
            Self::Server(s) => format!(" AND server_id = {s}"),
        }
    }

    /// `keyExistsInScope` also requires the other owner columns to be NULL.
    fn exact_sql(self) -> String {
        let (p, e, s) = self.columns();
        let col = |c: &str, v: Option<i64>| match v {
            Some(v) => format!(" AND {c} = {v}"),
            None => format!(" AND {c} IS NULL"),
        };
        format!(
            "{}{}{}",
            col("project_id", p),
            col("environment_id", e),
            col("server_id", s)
        )
    }

    /// (project_id, environment_id, server_id) columns.
    pub fn columns(self) -> (Option<i64>, Option<i64>, Option<i64>) {
        match self {
            Self::Team => (None, None, None),
            Self::Project(p) => (Some(p), None, None),
            Self::Environment(e) => (None, Some(e), None),
            Self::Server(s) => (None, None, Some(s)),
        }
    }
}

fn decode(text: &str) -> DbResult<Record> {
    parse_row(text).map_err(|e| DbError::Schema(format!("decoding shared variable: {e}")))
}

/// `listEnvs`: `ownedByCurrentTeamAPI`, the type and scope, ordered by id.
pub async fn list(db: &Db, team_id: i64, scope: Scope) -> DbResult<Vec<Record>> {
    let sql = format!(
        "SELECT row_to_json(e)::text FROM shared_environment_variables e \
         WHERE team_id = $1 AND type = $2{} ORDER BY id",
        scope.sql()
    );
    let rows: Vec<String> = sqlx::query_scalar(&sql)
        .bind(team_id)
        .bind(scope.kind())
        .fetch_all(db.pool())
        .await?;
    rows.iter().map(|t| decode(t)).collect()
}

/// `findEnvInScope`.
pub async fn find(db: &Db, team_id: i64, scope: Scope, id: i64) -> DbResult<Option<Record>> {
    let sql = format!(
        "SELECT row_to_json(e)::text FROM shared_environment_variables e \
         WHERE team_id = $1 AND type = $2 AND id = $3{} LIMIT 1",
        scope.sql()
    );
    let row: Option<String> = sqlx::query_scalar(&sql)
        .bind(team_id)
        .bind(scope.kind())
        .bind(id)
        .fetch_optional(db.pool())
        .await?;
    row.as_deref().map(decode).transpose()
}

/// `keyExistsInScope`.
pub async fn key_exists(
    db: &Db,
    team_id: i64,
    scope: Scope,
    key: &str,
    except: Option<i64>,
) -> DbResult<bool> {
    let sql = format!(
        "SELECT EXISTS (SELECT 1 FROM shared_environment_variables WHERE team_id = $1 AND type = $2 AND key = $3{} \
         AND ($4::bigint IS NULL OR id <> $4))",
        scope.exact_sql()
    );
    Ok(sqlx::query_scalar(&sql)
        .bind(team_id)
        .bind(scope.kind())
        .bind(key)
        .bind(except)
        .fetch_one(db.pool())
        .await?)
}

/// `SharedEnvironmentVariable::create`.
#[derive(Debug)]
pub struct NewShared<'a> {
    pub team_id: i64,
    pub key: &'a str,
    pub value_encrypted: Option<&'a str>,
    pub is_literal: bool,
    pub is_multiline: bool,
    pub is_shown_once: bool,
    pub comment: Option<&'a str>,
}

pub async fn create(db: &Db, scope: Scope, n: &NewShared<'_>, now: NaiveDateTime) -> DbResult<i64> {
    db.ensure_writable()?;
    let (p, e, s) = scope.columns();
    Ok(sqlx::query_scalar(
        "INSERT INTO shared_environment_variables (key, value, is_literal, is_multiline, is_shown_once, comment, \
         type, team_id, project_id, environment_id, server_id, updated_at, created_at) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$12) RETURNING id",
    )
    .bind(n.key)
    .bind(n.value_encrypted)
    .bind(n.is_literal)
    .bind(n.is_multiline)
    .bind(n.is_shown_once)
    .bind(n.comment)
    .bind(scope.kind())
    .bind(n.team_id)
    .bind(p)
    .bind(e)
    .bind(s)
    .bind(now)
    .fetch_one(db.pool())
    .await?)
}

// ── tags ──────────────────────────────────────────────────────────────

fn tag_decode(text: &str) -> DbResult<Record> {
    parse_row(text).map_err(|e| DbError::Schema(format!("decoding tag: {e}")))
}

/// `Tag::where('team_id', ...)->orderBy('name')`.
pub async fn tags(db: &Db, team_id: i64) -> DbResult<Vec<Record>> {
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT row_to_json(t)::text FROM tags t WHERE team_id = $1 ORDER BY name",
    )
    .bind(team_id)
    .fetch_all(db.pool())
    .await?;
    rows.iter().map(|t| tag_decode(t)).collect()
}

pub async fn find_tag(db: &Db, team_id: i64, uuid: &str) -> DbResult<Option<Record>> {
    let row: Option<String> = sqlx::query_scalar(
        "SELECT row_to_json(t)::text FROM tags t WHERE team_id = $1 AND uuid = $2 LIMIT 1",
    )
    .bind(team_id)
    .bind(uuid)
    .fetch_optional(db.pool())
    .await?;
    row.as_deref().map(tag_decode).transpose()
}

pub async fn tag_name_exists(
    db: &Db,
    team_id: i64,
    name: &str,
    except: Option<i64>,
) -> DbResult<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM tags WHERE team_id = $1 AND name = $2 AND ($3::bigint IS NULL OR id <> $3))",
    )
    .bind(team_id)
    .bind(name)
    .bind(except)
    .fetch_one(db.pool())
    .await?)
}

/// `Tag::create`: `None` when the unique constraint refused it.
pub async fn create_tag(
    db: &Db,
    team_id: i64,
    uuid: &str,
    name: &str,
    now: NaiveDateTime,
) -> DbResult<Option<Record>> {
    db.ensure_writable()?;
    let row: Option<String> = sqlx::query_scalar(
        "INSERT INTO tags (uuid, name, team_id, created_at, updated_at) VALUES ($1,$2,$3,$4,$4) \
         ON CONFLICT DO NOTHING RETURNING row_to_json(tags)::text",
    )
    .bind(uuid)
    .bind(name)
    .bind(team_id)
    .bind(now)
    .fetch_optional(db.pool())
    .await?;
    row.as_deref().map(tag_decode).transpose()
}

pub async fn rename_tag(
    db: &Db,
    id: i64,
    name: &str,
    now: NaiveDateTime,
) -> DbResult<Option<Record>> {
    db.ensure_writable()?;
    let row: Option<String> = sqlx::query_scalar(
        "UPDATE tags SET name = $2, updated_at = $3 WHERE id = $1 RETURNING row_to_json(tags)::text",
    )
    .bind(id)
    .bind(name)
    .bind(now)
    .fetch_optional(db.pool())
    .await?;
    row.as_deref().map(tag_decode).transpose()
}

pub async fn delete_tag(db: &Db, id: i64) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query("DELETE FROM tags WHERE id = $1")
        .bind(id)
        .execute(db.pool())
        .await?;
    Ok(())
}
