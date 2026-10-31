//! Queries of the canvas services (`App\Services\EnvironmentCanvas`, `ProjectCanvas`,
//! `ServerCanvas`, `SharedVariableCanvas`, `TagCanvas`). The queries have no `ORDER BY` where
//! Eloquent has none, so rows come back in the same order as upstream.

use std::collections::HashMap;

use chrono::NaiveDateTime;
use coolify_domain::new_public_id;
use serde_json::{Value, json};

use crate::records::{Record, parse_row};
use crate::{Db, DbError, DbResult};

fn decode(t: &str) -> DbResult<Record> {
    parse_row(t).map_err(|e| DbError::Schema(format!("decoding canvas row: {e}")))
}

async fn rows(db: &Db, sql: &str, ids: &[i64]) -> DbResult<Vec<Record>> {
    let texts: Vec<String> = sqlx::query_scalar(sql)
        .bind(ids)
        .fetch_all(db.pool())
        .await?;
    texts.iter().map(|t| decode(t)).collect()
}

/// `EnvironmentCanvas::DATABASE_RELATIONS`: table, morph class, `$database->type()` without
/// `standalone-`.
pub const DATABASE_TABLES: [(&str, &str, &str); 9] = [
    (
        "standalone_postgresqls",
        "App\\Models\\StandalonePostgresql",
        "postgresql",
    ),
    ("standalone_redis", "App\\Models\\StandaloneRedis", "redis"),
    (
        "standalone_mongodbs",
        "App\\Models\\StandaloneMongodb",
        "mongodb",
    ),
    ("standalone_mysqls", "App\\Models\\StandaloneMysql", "mysql"),
    (
        "standalone_mariadbs",
        "App\\Models\\StandaloneMariadb",
        "mariadb",
    ),
    ("standalone_keydbs", "App\\Models\\StandaloneKeydb", "keydb"),
    (
        "standalone_dragonflies",
        "App\\Models\\StandaloneDragonfly",
        "dragonfly",
    ),
    (
        "standalone_clickhouses",
        "App\\Models\\StandaloneClickhouse",
        "clickhouse",
    ),
    (
        "standalone_sqlites",
        "App\\Models\\StandaloneSqlite",
        "sqlite",
    ),
];

pub const APPLICATION_MORPH: &str = "App\\Models\\Application";
pub const SERVICE_MORPH: &str = "App\\Models\\Service";

/// One top-level resource of a canvas.
#[derive(Debug, Clone)]
pub struct Resource {
    pub morph: &'static str,
    /// Engine of a standalone database, `None` for applications and services.
    pub engine: Option<&'static str>,
    pub row: Record,
}

impl Resource {
    pub fn str(&self, key: &str) -> Option<&str> {
        self.row.get(key).and_then(Value::as_str)
    }
    pub fn id(&self) -> i64 {
        self.row.get("id").and_then(Value::as_i64).unwrap_or(0)
    }
    pub fn uuid(&self) -> &str {
        self.str("uuid").unwrap_or_default()
    }
    pub fn name(&self) -> &str {
        self.str("name").unwrap_or_default()
    }
    pub fn environment_id(&self) -> i64 {
        self.row
            .get("environment_id")
            .and_then(Value::as_i64)
            .unwrap_or(0)
    }
}

/// `ServerCanvas::loadResources`: applications, services, then every database relation.
pub async fn resources(db: &Db, environment_ids: &[i64]) -> DbResult<Vec<Resource>> {
    let mut out = Vec::new();
    for (table, morph) in [
        ("applications", APPLICATION_MORPH),
        ("services", SERVICE_MORPH),
    ] {
        let sql = format!(
            "SELECT row_to_json(t)::text FROM {table} t WHERE t.environment_id = ANY($1) AND t.deleted_at IS NULL"
        );
        for row in rows(db, &sql, environment_ids).await? {
            out.push(Resource {
                morph,
                engine: None,
                row,
            });
        }
    }
    for (table, morph, engine) in DATABASE_TABLES {
        let sql = format!(
            "SELECT row_to_json(t)::text FROM {table} t WHERE t.environment_id = ANY($1) AND t.deleted_at IS NULL"
        );
        for row in rows(db, &sql, environment_ids).await? {
            out.push(Resource {
                morph,
                engine: Some(engine),
                row,
            });
        }
    }
    Ok(out)
}

/// A server with its `server_settings` under `settings`, by id. Soft-deleted servers are left out.
pub async fn servers_by_id(db: &Db, ids: &[i64]) -> DbResult<HashMap<i64, Record>> {
    let list = rows(
        db,
        "SELECT (to_jsonb(s) || jsonb_build_object('settings', to_jsonb(ss)))::text FROM servers s \
         LEFT JOIN server_settings ss ON ss.server_id = s.id WHERE s.id = ANY($1) AND s.deleted_at IS NULL",
        ids,
    )
    .await?;
    Ok(list
        .into_iter()
        .filter_map(|r| r.get("id").and_then(Value::as_i64).map(|id| (id, r)))
        .collect())
}

/// `Server::query()->whereTeamId($teamId)->with('settings')->orderBy('name')->get()`.
pub async fn team_servers(db: &Db, team_id: i64) -> DbResult<Vec<Record>> {
    rows(
        db,
        "SELECT (to_jsonb(s) || jsonb_build_object('settings', to_jsonb(ss)))::text FROM servers s \
         LEFT JOIN server_settings ss ON ss.server_id = s.id WHERE s.team_id = ANY($1) AND s.deleted_at IS NULL ORDER BY s.name",
        &[team_id],
    )
    .await
}

/// `Server::query()->whereTeamId($teamId)->whereUuid($uuid)->with('settings')->first()`.
pub async fn server_by_uuid(db: &Db, team_id: i64, uuid: &str) -> DbResult<Option<Record>> {
    let text: Option<String> = sqlx::query_scalar(
        "SELECT (to_jsonb(s) || jsonb_build_object('settings', to_jsonb(ss)))::text FROM servers s \
         LEFT JOIN server_settings ss ON ss.server_id = s.id WHERE s.team_id = $1 AND s.uuid = $2 AND s.deleted_at IS NULL LIMIT 1",
    )
    .bind(team_id)
    .bind(uuid)
    .fetch_optional(db.pool())
    .await?;
    text.as_deref().map(decode).transpose()
}

/// `server_id` per (`destination_type`, id).
pub async fn destination_servers(db: &Db) -> DbResult<HashMap<(String, i64), i64>> {
    let list: Vec<(String, i64, i64)> = sqlx::query_as(
        "SELECT 'App\\Models\\StandaloneDocker', id, server_id FROM standalone_dockers \
         UNION ALL SELECT 'App\\Models\\SwarmDocker', id, server_id FROM swarm_dockers",
    )
    .fetch_all(db.pool())
    .await?;
    Ok(list.into_iter().map(|(t, i, s)| ((t, i), s)).collect())
}

/// `$swarmDocker->server_id`.
pub async fn swarm_server_id(db: &Db, destination_id: i64) -> DbResult<Option<i64>> {
    Ok(
        sqlx::query_scalar("SELECT server_id FROM swarm_dockers WHERE id = $1")
            .bind(destination_id)
            .fetch_optional(db.pool())
            .await?,
    )
}

/// `$application->additional_servers` per application: server id and pivot status.
pub async fn additional_servers(
    db: &Db,
    application_ids: &[i64],
) -> DbResult<HashMap<i64, Vec<(i64, Option<String>)>>> {
    let list: Vec<(i64, i64, Option<String>)> = sqlx::query_as(
        "SELECT a.application_id, s.id, a.status FROM servers s INNER JOIN additional_destinations a ON s.id = a.server_id \
         WHERE a.application_id = ANY($1) AND s.deleted_at IS NULL",
    )
    .bind(application_ids)
    .fetch_all(db.pool())
    .await?;
    let mut out: HashMap<i64, Vec<(i64, Option<String>)>> = HashMap::new();
    for (application, server, status) in list {
        out.entry(application).or_default().push((server, status));
    }
    Ok(out)
}

/// `$service->applications` or `$service->databases` per service.
pub async fn service_children(
    db: &Db,
    table: &str,
    service_ids: &[i64],
) -> DbResult<HashMap<i64, Vec<Record>>> {
    let sql = format!(
        "SELECT row_to_json(t)::text FROM {table} t WHERE t.service_id = ANY($1) AND t.deleted_at IS NULL"
    );
    let mut out: HashMap<i64, Vec<Record>> = HashMap::new();
    for row in rows(db, &sql, service_ids).await? {
        let service = row.get("service_id").and_then(Value::as_i64).unwrap_or(0);
        out.entry(service).or_default().push(row);
    }
    Ok(out)
}

/// `EnvironmentCanvas::startingServiceUuids`: the latest activity of the service is a queued or
/// running start that is not stale (`ResourceStartActivity::isRunning`).
pub async fn starting_services(db: &Db, uuids: &[String]) -> DbResult<Vec<String>> {
    if uuids.is_empty() {
        return Ok(Vec::new());
    }
    Ok(sqlx::query_scalar(
        "SELECT l.type_uuid FROM (\
           SELECT DISTINCT ON (a.properties->>'type_uuid') a.properties->>'type_uuid' AS type_uuid, a.properties->>'status' AS status, a.updated_at \
           FROM activity_log a WHERE a.properties->>'type_uuid' = ANY($1) \
             AND a.updated_at >= (now() AT TIME ZONE 'UTC') - interval '900 seconds' \
           ORDER BY a.properties->>'type_uuid', a.created_at DESC, a.id DESC) l \
         WHERE (l.status = 'queued' AND l.updated_at > (now() AT TIME ZONE 'UTC') - interval '600 seconds') \
            OR (l.status = 'in_progress' AND l.updated_at > (now() AT TIME ZONE 'UTC') - interval '900 seconds')",
    )
    .bind(uuids)
    .fetch_all(db.pool())
    .await?)
}

/// `$environment->resourceLinks()` or `ResourceLink::query()->whereIn('environment_id', ...)`.
pub async fn links(db: &Db, environment_ids: &[i64]) -> DbResult<Vec<Record>> {
    rows(
        db,
        "SELECT row_to_json(t)::text FROM resource_links t WHERE t.environment_id = ANY($1) ORDER BY t.id",
        environment_ids,
    )
    .await
}

/// `Project::query()->where('team_id', $teamId)->orderBy('name')`.
pub async fn team_projects(db: &Db, team_id: i64) -> DbResult<Vec<Record>> {
    rows(
        db,
        "SELECT row_to_json(t)::text FROM projects t WHERE t.team_id = ANY($1) ORDER BY t.name",
        &[team_id],
    )
    .await
}

pub async fn project_by_uuid(db: &Db, team_id: i64, uuid: &str) -> DbResult<Option<Record>> {
    let text: Option<String> = sqlx::query_scalar(
        "SELECT row_to_json(t)::text FROM projects t WHERE t.team_id = $1 AND t.uuid = $2 LIMIT 1",
    )
    .bind(team_id)
    .bind(uuid)
    .fetch_optional(db.pool())
    .await?;
    text.as_deref().map(decode).transpose()
}

/// The environments of the projects of a team (`whereHas('project', ...)`).
pub async fn team_environments(db: &Db, team_id: i64) -> DbResult<Vec<Record>> {
    rows(
        db,
        "SELECT row_to_json(t)::text FROM environments t WHERE EXISTS (SELECT 1 FROM projects p WHERE p.id = t.project_id AND p.team_id = ANY($1))",
        &[team_id],
    )
    .await
}

/// `$project->environments()->orderBy('id')`, or `whereIn('project_id', ...)`.
pub async fn project_environments(db: &Db, project_ids: &[i64]) -> DbResult<Vec<Record>> {
    rows(
        db,
        "SELECT row_to_json(t)::text FROM environments t WHERE t.project_id = ANY($1) ORDER BY t.id",
        project_ids,
    )
    .await
}

pub async fn projects_by_id(db: &Db, ids: &[i64]) -> DbResult<HashMap<i64, Record>> {
    let list = rows(
        db,
        "SELECT row_to_json(t)::text FROM projects t WHERE t.id = ANY($1)",
        ids,
    )
    .await?;
    Ok(list
        .into_iter()
        .filter_map(|r| r.get("id").and_then(Value::as_i64).map(|id| (id, r)))
        .collect())
}

/// `$project->environments()->whereName($v)->first() ?? ...->whereUuid($v)->first()`.
pub async fn find_environment(
    db: &Db,
    project_id: i64,
    name_or_uuid: &str,
) -> DbResult<Option<Record>> {
    for column in ["name", "uuid"] {
        let sql = format!(
            "SELECT row_to_json(t)::text FROM environments t WHERE t.project_id = $1 AND t.{column} = $2 LIMIT 1"
        );
        let text: Option<String> = sqlx::query_scalar(&sql)
            .bind(project_id)
            .bind(name_or_uuid)
            .fetch_optional(db.pool())
            .await?;
        if let Some(t) = text {
            return decode(&t).map(Some);
        }
    }
    Ok(None)
}

/// `Environment::query()->where('uuid', $uuid)->whereHas('project', team)`.
pub async fn environment_in_team(db: &Db, team_id: i64, uuid: &str) -> DbResult<Option<Record>> {
    let text: Option<String> = sqlx::query_scalar(
        "SELECT row_to_json(t)::text FROM environments t WHERE t.uuid = $2 AND EXISTS (SELECT 1 FROM projects p WHERE p.id = t.project_id AND p.team_id = $1) LIMIT 1",
    )
    .bind(team_id)
    .bind(uuid)
    .fetch_optional(db.pool())
    .await?;
    text.as_deref().map(decode).transpose()
}

/// The transaction of `EnvironmentCanvasController::updatePositions`: merges the positions into
/// the locked row, keeps only the current cards, and returns what was saved.
pub async fn merge_positions(
    db: &Db,
    environment_id: i64,
    merge: impl FnOnce(Option<Value>) -> Value,
    now: NaiveDateTime,
) -> DbResult<Value> {
    db.ensure_writable()?;
    let mut tx = db.pool().begin().await?;
    let current: Option<String> = sqlx::query_scalar(
        "SELECT canvas_positions::text FROM environments WHERE id = $1 FOR UPDATE",
    )
    .bind(environment_id)
    .fetch_one(&mut *tx)
    .await?;
    let merged = merge(current.and_then(|t| serde_json::from_str(&t).ok()));
    sqlx::query(
        "UPDATE environments SET canvas_positions = $1::json, updated_at = $2 WHERE id = $3",
    )
    .bind(merged.to_string())
    .bind(now)
    .bind(environment_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(merged)
}

/// `$project->canvas_position = ...; save()` or the same on a server.
pub async fn save_zone_position(
    db: &Db,
    table: &'static str,
    id: i64,
    position: &Value,
    now: NaiveDateTime,
) -> DbResult<()> {
    db.ensure_writable()?;
    let sql =
        format!("UPDATE {table} SET canvas_position = $1::json, updated_at = $2 WHERE id = $3");
    sqlx::query(&sql)
        .bind(position.to_string())
        .bind(now)
        .bind(id)
        .execute(db.pool())
        .await?;
    Ok(())
}

pub struct LinkEnds<'a> {
    pub environment_id: i64,
    pub source_type: &'a str,
    pub source_id: i64,
    pub target_type: &'a str,
    pub target_id: i64,
}

pub async fn link_exists(db: &Db, l: &LinkEnds<'_>) -> DbResult<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM resource_links WHERE environment_id = $1 AND source_type = $2 AND source_id = $3 AND target_type = $4 AND target_id = $5)",
    )
    .bind(l.environment_id)
    .bind(l.source_type)
    .bind(l.source_id)
    .bind(l.target_type)
    .bind(l.target_id)
    .fetch_one(db.pool())
    .await?)
}

/// `ResourceLink::create`: `None` when the unique index refuses a duplicate.
pub async fn create_link(
    db: &Db,
    l: &LinkEnds<'_>,
    label: Option<&str>,
    created_by: i64,
    now: NaiveDateTime,
) -> DbResult<Option<String>> {
    db.ensure_writable()?;
    Ok(sqlx::query_scalar(
        "INSERT INTO resource_links (uuid, environment_id, source_type, source_id, target_type, target_id, label, created_by, created_at, updated_at) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$9) ON CONFLICT ON CONSTRAINT resource_links_unique_direction DO NOTHING RETURNING uuid",
    )
    .bind(new_public_id())
    .bind(l.environment_id)
    .bind(l.source_type)
    .bind(l.source_id)
    .bind(l.target_type)
    .bind(l.target_id)
    .bind(label)
    .bind(created_by)
    .bind(now)
    .fetch_optional(db.pool())
    .await?)
}

/// `$environment->resourceLinks()->whereUuid($uuid)->first()`.
pub async fn find_link(db: &Db, environment_id: i64, uuid: &str) -> DbResult<Option<Record>> {
    let text: Option<String> = sqlx::query_scalar(
        "SELECT row_to_json(t)::text FROM resource_links t WHERE t.environment_id = $1 AND t.uuid = $2 LIMIT 1",
    )
    .bind(environment_id)
    .bind(uuid)
    .fetch_optional(db.pool())
    .await?;
    text.as_deref().map(decode).transpose()
}

pub async fn update_link_label(
    db: &Db,
    id: i64,
    label: Option<&str>,
    now: NaiveDateTime,
) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query("UPDATE resource_links SET label = $1, updated_at = $2 WHERE id = $3")
        .bind(label)
        .bind(now)
        .bind(id)
        .execute(db.pool())
        .await?;
    Ok(())
}

pub async fn delete_link(db: &Db, id: i64) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query("DELETE FROM resource_links WHERE id = $1")
        .bind(id)
        .execute(db.pool())
        .await?;
    Ok(())
}

/// `taggables` per (morph, resource id): the tag ids of each resource.
pub async fn tag_ids(db: &Db, team_tag_ids: &[i64]) -> DbResult<HashMap<(String, i64), Vec<i64>>> {
    let list: Vec<(String, i64, i64)> = sqlx::query_as(
        "SELECT taggable_type, taggable_id, tag_id FROM taggables WHERE tag_id = ANY($1)",
    )
    .bind(team_tag_ids)
    .fetch_all(db.pool())
    .await?;
    let mut out: HashMap<(String, i64), Vec<i64>> = HashMap::new();
    for (morph, id, tag) in list {
        out.entry((morph, id)).or_default().push(tag);
    }
    Ok(out)
}

/// `$owner->environment_variables()->orderBy('key')`: the shared variables of one scope.
pub async fn shared_of_owner(
    db: &Db,
    scope: &str,
    owner_id: i64,
    team_id: i64,
) -> DbResult<Vec<Record>> {
    let (filter, id) = match scope {
        "team" => ("t.type = 'team' AND t.team_id = $1", team_id),
        "project" => ("t.type = 'project' AND t.project_id = $1", owner_id),
        "environment" => ("t.type = 'environment' AND t.environment_id = $1", owner_id),
        _ => ("t.type = 'server' AND t.server_id = $1", owner_id),
    };
    let sql = format!(
        "SELECT (to_jsonb(t) - 'value')::text FROM shared_environment_variables t WHERE {filter} ORDER BY t.key"
    );
    let texts: Vec<String> = sqlx::query_scalar(&sql)
        .bind(id)
        .fetch_all(db.pool())
        .await?;
    texts.iter().map(|t| decode(t)).collect()
}

/// `SharedVariableCanvas::resourceVariables`, or every row of the resources with `include_preview`.
pub async fn resource_variables(
    db: &Db,
    resources: &[(&str, i64)],
    include_preview: bool,
) -> DbResult<Vec<Record>> {
    if resources.is_empty() {
        return Ok(Vec::new());
    }
    let morphs: Vec<String> = resources.iter().map(|(m, _)| (*m).to_owned()).collect();
    let ids: Vec<i64> = resources.iter().map(|(_, i)| *i).collect();
    let preview = if include_preview {
        ""
    } else {
        "t.is_preview = false AND "
    };
    let sql = format!(
        "SELECT row_to_json(t)::text FROM environment_variables t WHERE {preview}\
         (t.resourceable_type, t.resourceable_id) IN (SELECT * FROM unnest($1::text[], $2::bigint[])) ORDER BY t.key"
    );
    let texts: Vec<String> = sqlx::query_scalar(&sql)
        .bind(&morphs)
        .bind(&ids)
        .fetch_all(db.pool())
        .await?;
    texts.iter().map(|t| decode(t)).collect()
}

/// The columns `ServerCanvas::dashboardPayload` reads, active first when `active`.
pub async fn dashboard_deployments(
    db: &Db,
    server_ids: &[i64],
    active: bool,
) -> DbResult<Vec<Record>> {
    const COLUMNS: &str = "t.deployment_uuid, t.application_id, t.application_name, t.status, t.deployment_url, t.pull_request_id, t.server_name, t.created_at, t.finished_at";
    let sql = if active {
        format!(
            "SELECT row_to_json(r)::text FROM (SELECT {COLUMNS} FROM application_deployment_queues t \
             WHERE t.server_id = ANY($1) AND t.status IN ('in_progress', 'queued') ORDER BY t.status, t.id) r"
        )
    } else {
        format!(
            "SELECT row_to_json(r)::text FROM (SELECT {COLUMNS} FROM application_deployment_queues t \
             WHERE t.server_id = ANY($1) AND t.status NOT IN ('in_progress', 'queued') ORDER BY t.id DESC LIMIT 8) r"
        )
    };
    rows(db, &sql, server_ids).await
}

/// `Application::whereIn('id', ...)->pluck('uuid', 'id')`.
pub async fn application_uuids(db: &Db, ids: &[i64]) -> DbResult<HashMap<i64, String>> {
    let list: Vec<(i64, String)> = sqlx::query_as(
        "SELECT id, uuid FROM applications WHERE id = ANY($1) AND deleted_at IS NULL",
    )
    .bind(ids)
    .fetch_all(db.pool())
    .await?;
    Ok(list.into_iter().collect())
}

/// `{uuid, name}` of a project, for page shells.
pub fn brief(r: &Record) -> Value {
    json!({"uuid": r.get("uuid"), "name": r.get("name")})
}

/// The icon columns of `ProjectIconStorageService`: `None` removes the icon.
pub async fn set_project_icon(
    db: &Db,
    id: i64,
    path: Option<&str>,
    now: NaiveDateTime,
) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(
        "UPDATE projects SET icon_path = $1, icon_storage_type = CASE WHEN $1::text IS NULL THEN NULL ELSE 'local' END, \
         icon_s3_storage_id = NULL, updated_at = $2 WHERE id = $3",
    )
    .bind(path)
    .bind(now)
    .bind(id)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// `project_icon_url($project)` per uuid, for the projects of a team that have an icon.
pub async fn project_icon_urls(db: &Db, team_id: i64) -> DbResult<HashMap<String, String>> {
    let list: Vec<(String, i64)> = sqlx::query_as(
        "SELECT uuid, COALESCE(EXTRACT(EPOCH FROM updated_at)::bigint, 0) FROM projects WHERE team_id = $1 AND icon_path IS NOT NULL",
    )
    .bind(team_id)
    .fetch_all(db.pool())
    .await?;
    Ok(list
        .into_iter()
        .map(|(uuid, at)| {
            let url = format!("/project/{uuid}/icon?v={at}");
            (uuid, url)
        })
        .collect())
}

/// `$team->subscription`.
pub async fn team_subscription(db: &Db, team_id: i64) -> DbResult<Option<Record>> {
    let text: Option<String> = sqlx::query_scalar(
        "SELECT row_to_json(t)::text FROM subscriptions t WHERE t.team_id = $1 ORDER BY t.id LIMIT 1",
    )
    .bind(team_id)
    .fetch_optional(db.pool())
    .await?;
    text.as_deref().map(decode).transpose()
}

/// `$subscription->update([...])`. Column names are the caller's literals.
pub async fn update_subscription(db: &Db, team_id: i64, values: &[(&str, Value)]) -> DbResult<()> {
    db.ensure_writable()?;
    for (column, value) in values {
        if !column.bytes().all(|b| b.is_ascii_lowercase() || b == b'_') {
            return Err(DbError::Schema(format!("bad column {column}")));
        }
        let sql = format!(
            "UPDATE subscriptions SET {column} = (SELECT {column} FROM jsonb_to_record(jsonb_build_object('{column}', $1::jsonb)) AS x({column} {kind})), updated_at = now() AT TIME ZONE 'UTC' WHERE team_id = $2",
            kind = match value {
                Value::Bool(_) => "boolean",
                _ if column.ends_with("_at") => "timestamp",
                _ => "text",
            }
        );
        sqlx::query(&sql)
            .bind(value.to_string())
            .bind(team_id)
            .execute(db.pool())
            .await?;
    }
    Ok(())
}

/// `Subscription::firstOrNew(['team_id' => ...])`, then `stripe_customer_id` and `save()`.
pub async fn assign_customer(db: &Db, team_id: i64, customer: &str) -> DbResult<()> {
    db.ensure_writable()?;
    let updated = sqlx::query(
        "UPDATE subscriptions SET stripe_customer_id = $1, updated_at = now() AT TIME ZONE 'UTC' WHERE team_id = $2",
    )
    .bind(customer)
    .bind(team_id)
    .execute(db.pool())
    .await?;
    if updated.rows_affected() == 0 {
        sqlx::query(
            "INSERT INTO subscriptions (team_id, stripe_customer_id, created_at, updated_at) \
             VALUES ($1, $2, now() AT TIME ZONE 'UTC', now() AT TIME ZONE 'UTC')",
        )
        .bind(team_id)
        .bind(customer)
        .execute(db.pool())
        .await?;
    }
    Ok(())
}

pub async fn set_server_limit(db: &Db, team_id: i64, limit: i64) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query("UPDATE teams SET custom_server_limit = $1, updated_at = now() AT TIME ZONE 'UTC' WHERE id = $2")
        .bind(limit)
        .bind(team_id)
        .execute(db.pool())
        .await?;
    Ok(())
}

/// `Team::subscriptionEnded`: every server of the team stops being usable.
pub async fn subscription_ended(db: &Db, team_id: i64) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(
        "UPDATE subscriptions SET stripe_subscription_id = NULL, stripe_cancel_at_period_end = false, stripe_invoice_paid = false, \
         stripe_trial_already_ended = false, stripe_past_due = false, updated_at = now() AT TIME ZONE 'UTC' WHERE team_id = $1",
    )
    .bind(team_id)
    .execute(db.pool())
    .await?;
    sqlx::query(
        "UPDATE server_settings SET is_usable = false, is_reachable = false WHERE server_id IN (SELECT id FROM servers WHERE team_id = $1)",
    )
    .bind(team_id)
    .execute(db.pool())
    .await?;
    sqlx::query("UPDATE servers SET unreachable_count = 3 WHERE team_id = $1")
        .bind(team_id)
        .execute(db.pool())
        .await?;
    Ok(())
}

pub struct TeamLimits {
    pub name: String,
    pub custom_server_limit: Option<i64>,
    pub servers: i64,
    /// `managedServers()`: not transferred away.
    pub managed_servers: i64,
}

pub async fn team_limits(db: &Db, team_id: i64) -> DbResult<TeamLimits> {
    let (name, custom_server_limit, servers, managed_servers): (String, Option<i64>, i64, i64) = sqlx::query_as(
        "SELECT t.name, t.custom_server_limit::bigint, \
           (SELECT count(*) FROM servers s WHERE s.team_id = t.id AND s.deleted_at IS NULL), \
           (SELECT count(*) FROM servers s WHERE s.team_id = t.id AND s.deleted_at IS NULL \
              AND (s.server_metadata->'transfer'->>'status' IS NULL OR s.server_metadata->'transfer'->>'status' != 'transferred')) \
         FROM teams t WHERE t.id = $1",
    )
    .bind(team_id)
    .fetch_one(db.pool())
    .await?;
    Ok(TeamLimits {
        name,
        custom_server_limit,
        servers,
        managed_servers,
    })
}

/// `Team::whereRelation('subscription', 'stripe_invoice_paid', ...)->count()`: active, inactive.
pub async fn subscriber_counts(db: &Db) -> DbResult<(i64, i64)> {
    Ok(sqlx::query_as(
        "SELECT (SELECT count(*) FROM teams t WHERE EXISTS (SELECT 1 FROM subscriptions s WHERE s.team_id = t.id AND s.stripe_invoice_paid = true)), \
                (SELECT count(*) FROM teams t WHERE EXISTS (SELECT 1 FROM subscriptions s WHERE s.team_id = t.id AND s.stripe_invoice_paid = false))",
    )
    .fetch_one(db.pool())
    .await?)
}

/// `Admin\Index::submitSearch`, with the subscription badge of the view.
pub async fn search_users(db: &Db, search: &str) -> DbResult<Vec<Value>> {
    let list: Vec<(i64, String, String, bool)> = sqlx::query_as(
        "SELECT u.id, u.name, u.email, EXISTS (SELECT 1 FROM team_user tu JOIN subscriptions s ON s.team_id = tu.team_id \
           WHERE tu.user_id = u.id AND s.stripe_invoice_paid = true) \
         FROM users u WHERE u.name LIKE $1 OR u.email LIKE $1",
    )
    .bind(format!("%{search}%"))
    .fetch_all(db.pool())
    .await?;
    Ok(list
        .into_iter()
        .map(|(id, name, email, active)| json!({"id": id, "name": name, "email": email, "active": active}))
        .collect())
}

/// `Subscription::where('stripe_customer_id', $customerId)->first()`.
pub async fn subscription_by_customer(db: &Db, customer: &str) -> DbResult<Option<Record>> {
    let text: Option<String> = sqlx::query_scalar(
        "SELECT row_to_json(t)::text FROM subscriptions t WHERE t.stripe_customer_id = $1 ORDER BY t.id LIMIT 1",
    )
    .bind(customer)
    .fetch_optional(db.pool())
    .await?;
    text.as_deref().map(decode).transpose()
}

/// `Subscription::updateOrCreate(['team_id' => ...], [...])` or `firstOrCreate` with `overwrite` false.
pub async fn store_subscription(
    db: &Db,
    team_id: i64,
    subscription_id: Option<&str>,
    customer: Option<&str>,
    paid: bool,
    overwrite: bool,
) -> DbResult<()> {
    db.ensure_writable()?;
    if team_subscription(db, team_id).await?.is_some() {
        if overwrite {
            sqlx::query(
                "UPDATE subscriptions SET stripe_subscription_id = $1, stripe_customer_id = $2, stripe_invoice_paid = $3, \
                 stripe_past_due = false, updated_at = now() AT TIME ZONE 'UTC' WHERE team_id = $4",
            )
            .bind(subscription_id)
            .bind(customer)
            .bind(paid)
            .bind(team_id)
            .execute(db.pool())
            .await?;
        }
        return Ok(());
    }
    sqlx::query(
        "INSERT INTO subscriptions (team_id, stripe_subscription_id, stripe_customer_id, stripe_invoice_paid, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, now() AT TIME ZONE 'UTC', now() AT TIME ZONE 'UTC')",
    )
    .bind(team_id)
    .bind(subscription_id)
    .bind(customer)
    .bind(paid)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// `$team->members->where('id', $userId)->first()->isAdmin()`.
pub async fn is_team_admin(db: &Db, team_id: i64, user_id: i64) -> DbResult<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM team_user WHERE team_id = $1 AND user_id = $2 AND role IN ('admin', 'owner'))",
    )
    .bind(team_id)
    .bind(user_id)
    .fetch_one(db.pool())
    .await?)
}
