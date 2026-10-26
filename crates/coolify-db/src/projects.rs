//! Projects and environments.
//!
//! Writes reproduce upstream model behaviour:
//! * `HasSafeStringAttribute` runs `strip_tags` on name/description when they
//!   are assigned (so an assigned `null` becomes `""`);
//! * `Project::created` inserts a `project_settings` row and a `production`
//!   environment;
//! * `Project::deleting` deletes its environments and settings.
//!
//! Unlike upstream, multi-row writes run in one transaction.

use chrono::NaiveDateTime;
use coolify_domain::naming::strip_tags;
use coolify_domain::{TeamId, new_public_id};

use crate::records::{ENVIRONMENT, PROJECT, Record, parse_row};
use crate::{Db, DbError, DbResult};

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ProjectRef {
    pub id: i64,
    pub uuid: String,
    pub team_id: TeamId,
    pub name: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct EnvironmentRef {
    pub id: i64,
    pub uuid: String,
    pub project_id: i64,
    pub name: String,
    pub description: Option<String>,
}

/// A field present in a PATCH body: `Some(None)` is an explicit `null`.
pub type Patch<T> = Option<Option<T>>;

/// Resource tables counted by `Project::isEmpty` / `Environment::isEmpty`.
/// All of them soft-delete, and Eloquent's counts exclude trashed rows.
const RESOURCE_TABLES: &[&str] = &[
    "applications",
    "standalone_redis",
    "standalone_postgresqls",
    "standalone_mysqls",
    "standalone_keydbs",
    "standalone_dragonflies",
    "standalone_clickhouses",
    "standalone_mariadbs",
    "standalone_mongodbs",
    "standalone_sqlites",
    "services",
];

fn decode(r: &str, what: &str) -> DbResult<Record> {
    parse_row(r).map_err(|e| DbError::Schema(format!("decoding {what} row: {e}")))
}

pub async fn list(db: &Db, team_id: TeamId) -> DbResult<Vec<Record>> {
    // Upstream selects exactly these columns in this order.
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT row_to_json(p)::text FROM (SELECT id, name, description, uuid FROM projects \
         WHERE team_id = $1 ORDER BY id) p",
    )
    .bind(team_id)
    .fetch_all(db.pool())
    .await?;
    rows.iter().map(|r| decode(r, "project")).collect()
}

pub async fn find(db: &Db, team_id: TeamId, uuid: &str) -> DbResult<Option<ProjectRef>> {
    Ok(sqlx::query_as(
        "SELECT id, uuid, team_id, name, description FROM projects WHERE team_id = $1 AND uuid = $2 \
         ORDER BY id LIMIT 1",
    )
    .bind(team_id)
    .bind(uuid)
    .fetch_optional(db.pool())
    .await?)
}

pub async fn record(db: &Db, project_id: i64) -> DbResult<Record> {
    let row: String =
        sqlx::query_scalar("SELECT row_to_json(p)::text FROM projects p WHERE p.id = $1")
            .bind(project_id)
            .fetch_one(db.pool())
            .await?;
    Ok(PROJECT.present(decode(&row, "project")?))
}

pub async fn environment_records(db: &Db, project_id: i64) -> DbResult<Vec<Record>> {
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT row_to_json(e)::text FROM environments e WHERE e.project_id = $1 ORDER BY e.id",
    )
    .bind(project_id)
    .fetch_all(db.pool())
    .await?;
    rows.iter()
        .map(|r| decode(r, "environment").map(|raw| ENVIRONMENT.present(raw)))
        .collect()
}

pub async fn environment_briefs(db: &Db, project_id: i64) -> DbResult<Vec<Record>> {
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT row_to_json(e)::text FROM (SELECT id, name, uuid FROM environments \
         WHERE project_id = $1 ORDER BY id) e",
    )
    .bind(project_id)
    .fetch_all(db.pool())
    .await?;
    rows.iter().map(|r| decode(r, "environment")).collect()
}

pub async fn environment_record(db: &Db, env_id: i64) -> DbResult<Record> {
    let row: String =
        sqlx::query_scalar("SELECT row_to_json(e)::text FROM environments e WHERE e.id = $1")
            .bind(env_id)
            .fetch_one(db.pool())
            .await?;
    Ok(ENVIRONMENT.present(decode(&row, "environment")?))
}

/// Upstream looks the environment up by name first, then by uuid.
pub async fn find_environment(
    db: &Db,
    project_id: i64,
    name_or_uuid: &str,
) -> DbResult<Option<EnvironmentRef>> {
    for column in ["name", "uuid"] {
        let env: Option<EnvironmentRef> = sqlx::query_as(&format!(
            "SELECT id, uuid, project_id, name, description FROM environments \
             WHERE project_id = $1 AND {column} = $2 ORDER BY id LIMIT 1"
        ))
        .bind(project_id)
        .bind(name_or_uuid)
        .fetch_optional(db.pool())
        .await?;
        if env.is_some() {
            return Ok(env);
        }
    }
    Ok(None)
}

pub async fn environment_name_taken(
    db: &Db,
    project_id: i64,
    name: &str,
    except_id: Option<i64>,
) -> DbResult<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM environments WHERE project_id = $1 AND name = $2 \
         AND ($3::bigint IS NULL OR id <> $3))",
    )
    .bind(project_id)
    .bind(name)
    .bind(except_id)
    .fetch_one(db.pool())
    .await?)
}

async fn count_resources(db: &Db, scope_sql: &str, id: i64) -> DbResult<i64> {
    let mut total = 0i64;
    for table in RESOURCE_TABLES {
        let n: i64 = sqlx::query_scalar(&format!(
            "SELECT count(*) FROM {table} WHERE deleted_at IS NULL AND environment_id IN ({scope_sql})"
        ))
        .bind(id)
        .fetch_one(db.pool())
        .await?;
        total += n;
    }
    Ok(total)
}

pub async fn project_is_empty(db: &Db, project_id: i64) -> DbResult<bool> {
    Ok(count_resources(
        db,
        "SELECT id FROM environments WHERE project_id = $1",
        project_id,
    )
    .await?
        == 0)
}

pub async fn environment_is_empty(db: &Db, env_id: i64) -> DbResult<bool> {
    Ok(count_resources(db, "SELECT $1::bigint", env_id).await? == 0)
}

/// `Project::create` + its `created` hook.
pub async fn create(
    db: &Db,
    team_id: TeamId,
    name: Option<&str>,
    description: Option<&str>,
    now: NaiveDateTime,
) -> DbResult<ProjectRef> {
    db.ensure_writable()?;
    let mut tx = db.pool().begin().await?;
    let project: ProjectRef = sqlx::query_as(
        "INSERT INTO projects (uuid, name, description, team_id, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $5) RETURNING id, uuid, team_id, name, description",
    )
    .bind(new_public_id())
    .bind(strip_tags(name))
    .bind(strip_tags(description))
    .bind(team_id)
    .bind(now)
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO project_settings (project_id, created_at, updated_at) VALUES ($1, $2, $2)",
    )
    .bind(project.id)
    .bind(now)
    .execute(&mut *tx)
    .await?;
    // `Environment::create` without a description: the mutator never runs, so NULL.
    sqlx::query(
        "INSERT INTO environments (name, project_id, uuid, created_at, updated_at) VALUES ('production', $1, $2, $3, $3)",
    )
    .bind(project.id)
    .bind(new_public_id())
    .bind(now)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(project)
}

/// `$project->update($request->only(['name','description']))`.
pub async fn update(
    db: &Db,
    project: &ProjectRef,
    name: Patch<&str>,
    description: Patch<&str>,
    now: NaiveDateTime,
) -> DbResult<ProjectRef> {
    db.ensure_writable()?;
    let new_name = name.map(strip_tags).unwrap_or_else(|| project.name.clone());
    let new_desc = match description {
        Some(d) => Some(strip_tags(d)),
        None => project.description.clone(),
    };
    let dirty = new_name != project.name || new_desc != project.description;
    if !dirty {
        return Ok(project.clone());
    }
    Ok(sqlx::query_as(
        "UPDATE projects SET name = $2, description = $3, updated_at = $4 WHERE id = $1 \
         RETURNING id, uuid, team_id, name, description",
    )
    .bind(project.id)
    .bind(new_name)
    .bind(new_desc)
    .bind(now)
    .fetch_one(db.pool())
    .await?)
}

/// `$project->delete()` with its `deleting` hook (environments, settings).
pub async fn delete(db: &Db, project_id: i64) -> DbResult<()> {
    db.ensure_writable()?;
    let mut tx = db.pool().begin().await?;
    sqlx::query("DELETE FROM environments WHERE project_id = $1")
        .bind(project_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM project_settings WHERE project_id = $1")
        .bind(project_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM projects WHERE id = $1")
        .bind(project_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

#[derive(Debug)]
pub enum CreateEnvError {
    NameTaken,
    Db(DbError),
}

impl From<sqlx::Error> for CreateEnvError {
    fn from(e: sqlx::Error) -> Self {
        // The (name, project_id) unique index turns a race into a conflict
        // instead of upstream's 500.
        if let sqlx::Error::Database(d) = &e
            && d.is_unique_violation()
        {
            return Self::NameTaken;
        }
        Self::Db(e.into())
    }
}

pub async fn create_environment(
    db: &Db,
    project_id: i64,
    name: &str,
    now: NaiveDateTime,
) -> Result<EnvironmentRef, CreateEnvError> {
    db.ensure_writable().map_err(CreateEnvError::Db)?;
    Ok(sqlx::query_as(
        "INSERT INTO environments (name, project_id, uuid, created_at, updated_at) VALUES ($1, $2, $3, $4, $4) \
         RETURNING id, uuid, project_id, name, description",
    )
    .bind(strip_tags(Some(name)))
    .bind(project_id)
    .bind(new_public_id())
    .bind(now)
    .fetch_one(db.pool())
    .await?)
}

pub async fn update_environment(
    db: &Db,
    env: &EnvironmentRef,
    name: Patch<&str>,
    description: Patch<&str>,
    now: NaiveDateTime,
) -> Result<EnvironmentRef, CreateEnvError> {
    db.ensure_writable().map_err(CreateEnvError::Db)?;
    let new_name = name.map(strip_tags).unwrap_or_else(|| env.name.clone());
    let new_desc = match description {
        Some(d) => Some(strip_tags(d)),
        None => env.description.clone(),
    };
    if new_name == env.name && new_desc == env.description {
        return Ok(env.clone());
    }
    Ok(sqlx::query_as(
        "UPDATE environments SET name = $2, description = $3, updated_at = $4 WHERE id = $1 \
         RETURNING id, uuid, project_id, name, description",
    )
    .bind(env.id)
    .bind(new_name)
    .bind(new_desc)
    .bind(now)
    .fetch_one(db.pool())
    .await?)
}

pub async fn delete_environment(db: &Db, env_id: i64) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query("DELETE FROM environments WHERE id = $1")
        .bind(env_id)
        .execute(db.pool())
        .await?;
    Ok(())
}
