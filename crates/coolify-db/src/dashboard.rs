//! Read models for the web UI dashboard (upstream `App\Livewire\Dashboard`).

use std::collections::HashMap;

use coolify_domain::TeamId;

use crate::projects::RESOURCE_TABLES;
use crate::{Db, DbResult};

/// `(id, uuid, name, description, created_at)` of a project or environment row.
type NamedRow = (i64, String, String, Option<String>, Option<String>);

#[derive(Debug, Clone)]
pub struct DashProject {
    pub id: i64,
    pub uuid: String,
    pub name: String,
    pub description: Option<String>,
    /// `created_at` as `YYYY-MM-DD HH:MM:SS` (UTC, like the column).
    pub created_at: Option<String>,
    /// `(uuid, name)` in the order Eloquent loads them (no ORDER BY).
    pub environments: Vec<(String, String)>,
    /// Sum of the eleven `withCount` relations (trashed rows excluded).
    pub resources: i64,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct DashServer {
    pub uuid: String,
    pub name: String,
    pub description: Option<String>,
    pub force_disabled: bool,
    pub is_reachable: bool,
    pub is_usable: bool,
    pub proxy_type: Option<String>,
    pub proxy_status: Option<String>,
    pub is_metrics_enabled: bool,
    pub is_traffic_analytics_enabled: bool,
}

/// `Project::ownedByCurrentTeam()->with('environments')->withCount([...])`.
pub async fn projects(db: &Db, team_id: TeamId) -> DbResult<Vec<DashProject>> {
    let rows: Vec<NamedRow> = sqlx::query_as(
        "SELECT id, uuid, name, description, to_char(created_at, 'YYYY-MM-DD HH24:MI:SS') \
         FROM projects WHERE team_id = $1 ORDER BY LOWER(name)",
    )
    .bind(team_id)
    .fetch_all(db.pool())
    .await?;
    let ids: Vec<i64> = rows.iter().map(|r| r.0).collect();

    let envs: Vec<(i64, String, String)> = sqlx::query_as(
        "SELECT project_id, uuid, name FROM environments WHERE project_id = ANY($1)",
    )
    .bind(&ids)
    .fetch_all(db.pool())
    .await?;

    let union = RESOURCE_TABLES
        .iter()
        .map(|t| format!("SELECT environment_id FROM {t} WHERE deleted_at IS NULL"))
        .collect::<Vec<_>>()
        .join(" UNION ALL ");
    let counts: Vec<(i64, i64)> = sqlx::query_as(&format!(
        "SELECT e.project_id, count(*) FROM ({union}) r \
         JOIN environments e ON e.id = r.environment_id \
         WHERE e.project_id = ANY($1) GROUP BY e.project_id"
    ))
    .bind(&ids)
    .fetch_all(db.pool())
    .await?;
    let counts: HashMap<i64, i64> = counts.into_iter().collect();

    Ok(rows
        .into_iter()
        .map(|(id, uuid, name, description, created_at)| DashProject {
            environments: envs
                .iter()
                .filter(|e| e.0 == id)
                .map(|e| (e.1.clone(), e.2.clone()))
                .collect(),
            resources: counts.get(&id).copied().unwrap_or(0),
            id,
            uuid,
            name,
            description,
            created_at,
        })
        .collect())
}

/// `Server::ownedByCurrentTeam()` with the settings the dashboard reads.
pub async fn servers(db: &Db, team_id: TeamId) -> DbResult<Vec<DashServer>> {
    Ok(sqlx::query_as(
        "SELECT s.uuid, s.name, s.description, \
                COALESCE(ss.force_disabled, false) AS force_disabled, \
                COALESCE(ss.is_reachable, false) AS is_reachable, \
                COALESCE(ss.is_usable, false) AS is_usable, \
                s.proxy->>'type' AS proxy_type, s.proxy->>'status' AS proxy_status, \
                COALESCE(ss.is_metrics_enabled, false) AS is_metrics_enabled, \
                COALESCE(ss.is_traffic_analytics_enabled, false) AS is_traffic_analytics_enabled \
         FROM servers s LEFT JOIN server_settings ss ON ss.server_id = s.id \
         WHERE s.team_id = $1 AND s.deleted_at IS NULL ORDER BY s.name",
    )
    .bind(team_id)
    .fetch_all(db.pool())
    .await?)
}

/// `PrivateKey::ownedByCurrentTeam()->count()`.
pub async fn private_key_count(db: &Db, team_id: TeamId) -> DbResult<i64> {
    Ok(
        sqlx::query_scalar("SELECT count(*) FROM private_keys WHERE team_id = $1")
            .bind(team_id)
            .fetch_one(db.pool())
            .await?,
    )
}

#[derive(Debug, Clone)]
pub struct EnvSummary {
    pub uuid: String,
    pub name: String,
    pub description: Option<String>,
    pub created_at: Option<String>,
    pub resources: i64,
}

/// A project's environments with resource counts, in table order (no
/// ORDER BY, like `$project->environments()->get()`).
pub async fn environments(db: &Db, project_id: i64) -> DbResult<Vec<EnvSummary>> {
    let rows: Vec<NamedRow> = sqlx::query_as(
        "SELECT id, uuid, name, description, to_char(created_at, 'YYYY-MM-DD HH24:MI:SS.US') \
         FROM environments WHERE project_id = $1",
    )
    .bind(project_id)
    .fetch_all(db.pool())
    .await?;
    let union = RESOURCE_TABLES
        .iter()
        .map(|t| format!("SELECT environment_id FROM {t} WHERE deleted_at IS NULL"))
        .collect::<Vec<_>>()
        .join(" UNION ALL ");
    let counts: Vec<(i64, i64)> = sqlx::query_as(&format!(
        "SELECT r.environment_id, count(*) FROM ({union}) r \
         JOIN environments e ON e.id = r.environment_id \
         WHERE e.project_id = $1 GROUP BY r.environment_id"
    ))
    .bind(project_id)
    .fetch_all(db.pool())
    .await?;
    let counts: HashMap<i64, i64> = counts.into_iter().collect();
    Ok(rows
        .into_iter()
        .map(|(id, uuid, name, description, created_at)| EnvSummary {
            resources: counts.get(&id).copied().unwrap_or(0),
            uuid,
            name,
            description,
            created_at,
        })
        .collect())
}

/// `$team->projects()->get()`: the team's projects in table order.
pub async fn project_names(db: &Db, team_id: TeamId) -> DbResult<Vec<(String, String)>> {
    Ok(
        sqlx::query_as("SELECT uuid, name FROM projects WHERE team_id = $1")
            .bind(team_id)
            .fetch_all(db.pool())
            .await?,
    )
}
