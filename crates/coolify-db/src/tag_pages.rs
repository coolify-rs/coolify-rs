//! Queries behind the `Tags\Show` page and tag deploys.

use crate::{Db, DbResult};
use serde_json::Value;

/// `Tag::ownedByCurrentTeam()->withCount(['applications', 'services'])->get()->unique('name')`.
pub async fn tags_with_counts(db: &Db, team_id: i64) -> DbResult<Vec<Value>> {
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT json_build_object('id', t.id, 'uuid', t.uuid, 'name', t.name, \
           'applications_count', (SELECT count(*) FROM taggables x WHERE x.tag_id = t.id AND x.taggable_type = 'App\\Models\\Application'), \
           'services_count', (SELECT count(*) FROM taggables x WHERE x.tag_id = t.id AND x.taggable_type = 'App\\Models\\Service'))::text \
         FROM tags t WHERE t.team_id = $1 ORDER BY t.name",
    )
    .bind(team_id)
    .fetch_all(db.pool())
    .await?;
    Ok(rows
        .iter()
        .filter_map(|r| serde_json::from_str(r).ok())
        .collect())
}

/// `$tag->applications()->get()` with what the cards show.
pub async fn tag_applications(db: &Db, tag_id: i64) -> DbResult<Vec<Value>> {
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT json_build_object('id', a.id, 'uuid', a.uuid, 'name', a.name, 'description', a.description, \
           'build_pack', a.build_pack, 'source_type', a.source_type, 'fqdn', a.fqdn, \
           'is_http_basic_auth_enabled', a.is_http_basic_auth_enabled, \
           'project', p.name, 'project_uuid', p.uuid, 'environment', e.name, 'environment_uuid', e.uuid)::text \
         FROM taggables x JOIN applications a ON a.id = x.taggable_id \
         JOIN environments e ON e.id = a.environment_id JOIN projects p ON p.id = e.project_id \
         WHERE x.tag_id = $1 AND x.taggable_type = 'App\\Models\\Application' AND a.deleted_at IS NULL ORDER BY a.id",
    )
    .bind(tag_id)
    .fetch_all(db.pool())
    .await?;
    Ok(rows
        .iter()
        .filter_map(|r| serde_json::from_str(r).ok())
        .collect())
}

/// `$tag->services()->get()` with what the cards show.
pub async fn tag_services(db: &Db, tag_id: i64) -> DbResult<Vec<Value>> {
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT json_build_object('id', s.id, 'uuid', s.uuid, 'name', s.name, 'description', s.description, \
           'project', p.name, 'project_uuid', p.uuid, 'environment', e.name, 'environment_uuid', e.uuid)::text \
         FROM taggables x JOIN services s ON s.id = x.taggable_id \
         JOIN environments e ON e.id = s.environment_id JOIN projects p ON p.id = e.project_id \
         WHERE x.tag_id = $1 AND x.taggable_type = 'App\\Models\\Service' AND s.deleted_at IS NULL ORDER BY s.id",
    )
    .bind(tag_id)
    .fetch_all(db.pool())
    .await?;
    Ok(rows
        .iter()
        .filter_map(|r| serde_json::from_str(r).ok())
        .collect())
}

/// `getDeployments`: queued and running deployments of the given applications, oldest first.
pub async fn active_deployments(db: &Db, application_ids: &[i64]) -> DbResult<Vec<Value>> {
    let ids: Vec<String> = application_ids.iter().map(ToString::to_string).collect();
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT json_build_object('id', q.id, 'application_name', q.application_name, \
           'deployment_url', q.deployment_url, 'server_name', q.server_name, 'status', q.status)::text \
         FROM application_deployment_queues q \
         WHERE q.status IN ('in_progress', 'queued') AND q.application_id = ANY($1) ORDER BY q.id",
    )
    .bind(&ids)
    .fetch_all(db.pool())
    .await?;
    Ok(rows
        .iter()
        .filter_map(|r| serde_json::from_str(r).ok())
        .collect())
}

/// A resource the tag can be connected to.
#[derive(Debug, Clone)]
pub struct Connectable {
    pub uuid: String,
    pub name: String,
    pub project: String,
    pub environment: String,
    pub morph: String,
    pub id: i64,
    pub tagged: bool,
}

/// `TagCanvas::resources($teamId)`: applications, services and standalone databases of the team,
/// ordered by lower-cased name.
pub async fn connectable(db: &Db, team_id: i64, tag_id: i64) -> DbResult<Vec<Connectable>> {
    let mut union: Vec<String> = vec![
        "SELECT a.id, a.uuid, a.name, a.environment_id, 'App\\Models\\Application' AS morph FROM applications a WHERE a.deleted_at IS NULL".to_owned(),
        "SELECT s.id, s.uuid, s.name, s.environment_id, 'App\\Models\\Service' AS morph FROM services s WHERE s.deleted_at IS NULL".to_owned(),
    ];
    for (table, class) in [
        ("standalone_postgresqls", "StandalonePostgresql"),
        ("standalone_redis", "StandaloneRedis"),
        ("standalone_mongodbs", "StandaloneMongodb"),
        ("standalone_mysqls", "StandaloneMysql"),
        ("standalone_mariadbs", "StandaloneMariadb"),
        ("standalone_keydbs", "StandaloneKeydb"),
        ("standalone_dragonflies", "StandaloneDragonfly"),
        ("standalone_clickhouses", "StandaloneClickhouse"),
        ("standalone_sqlites", "StandaloneSqlite"),
    ] {
        union.push(format!(
            "SELECT d.id, d.uuid, d.name, d.environment_id, 'App\\Models\\{class}' AS morph FROM {table} d WHERE d.deleted_at IS NULL"
        ));
    }
    let sql = format!(
        "SELECT r.id, r.uuid, r.name, r.morph, p.name, e.name, \
           EXISTS (SELECT 1 FROM taggables x WHERE x.tag_id = $2 AND x.taggable_id = r.id AND x.taggable_type = r.morph) \
         FROM ({}) r JOIN environments e ON e.id = r.environment_id JOIN projects p ON p.id = e.project_id \
         WHERE p.team_id = $1 ORDER BY lower(r.name)",
        union.join(" UNION ALL ")
    );
    type Row = (i64, String, String, String, String, String, bool);
    let rows: Vec<Row> = sqlx::query_as(&sql)
        .bind(team_id)
        .bind(tag_id)
        .fetch_all(db.pool())
        .await?;
    Ok(rows
        .into_iter()
        .map(
            |(id, uuid, name, morph, project, environment, tagged)| Connectable {
                uuid,
                name,
                project,
                environment,
                morph,
                id,
                tagged,
            },
        )
        .collect())
}

/// `$resource->tags()->attach($tagId)`.
pub async fn attach(db: &Db, tag_id: i64, morph: &str, id: i64) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query("INSERT INTO taggables (tag_id, taggable_id, taggable_type) VALUES ($1, $2, $3)")
        .bind(tag_id)
        .bind(id)
        .bind(morph)
        .execute(db.pool())
        .await?;
    Ok(())
}

/// `$resource->tags()->detach($tagId)`.
pub async fn detach(db: &Db, tag_id: i64, morph: &str, id: i64) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(
        "DELETE FROM taggables WHERE tag_id = $1 AND taggable_id = $2 AND taggable_type = $3",
    )
    .bind(tag_id)
    .bind(id)
    .bind(morph)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// A resource variable with the owners its `{{scope.KEY}}` references resolve against.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ResourceVariable {
    pub value: Option<String>,
    pub resource_uuid: String,
    pub environment_id: i64,
    pub project_id: i64,
    pub server_id: Option<i64>,
}

/// `SharedVariableCanvas::resourceVariables(resources('team', $team))`.
pub async fn resource_variables(db: &Db, team_id: i64) -> DbResult<Vec<ResourceVariable>> {
    let mut union: Vec<String> = vec![
        "SELECT a.id, a.uuid, a.environment_id, 'App\\Models\\Application' AS morph, a.destination_id, a.destination_type, NULL::bigint AS service_server FROM applications a WHERE a.deleted_at IS NULL".to_owned(),
        "SELECT s.id, s.uuid, s.environment_id, 'App\\Models\\Service' AS morph, s.destination_id, s.destination_type, s.server_id AS service_server FROM services s WHERE s.deleted_at IS NULL".to_owned(),
    ];
    for (table, class) in [
        ("standalone_postgresqls", "StandalonePostgresql"),
        ("standalone_redis", "StandaloneRedis"),
        ("standalone_mongodbs", "StandaloneMongodb"),
        ("standalone_mysqls", "StandaloneMysql"),
        ("standalone_mariadbs", "StandaloneMariadb"),
        ("standalone_keydbs", "StandaloneKeydb"),
        ("standalone_dragonflies", "StandaloneDragonfly"),
        ("standalone_clickhouses", "StandaloneClickhouse"),
        ("standalone_sqlites", "StandaloneSqlite"),
    ] {
        union.push(format!(
            "SELECT d.id, d.uuid, d.environment_id, 'App\\Models\\{class}' AS morph, d.destination_id, d.destination_type, NULL::bigint AS service_server FROM {table} d WHERE d.deleted_at IS NULL"
        ));
    }
    let sql = format!(
        "SELECT v.value, r.uuid AS resource_uuid, r.environment_id, e.project_id, \
           COALESCE(r.service_server, sd.server_id, sw.server_id) AS server_id \
         FROM ({}) r JOIN environments e ON e.id = r.environment_id JOIN projects p ON p.id = e.project_id \
         JOIN environment_variables v ON v.resourceable_id = r.id AND v.resourceable_type = r.morph AND v.is_preview = false \
         LEFT JOIN standalone_dockers sd ON r.destination_type = 'App\\Models\\StandaloneDocker' AND sd.id = r.destination_id \
         LEFT JOIN swarm_dockers sw ON r.destination_type = 'App\\Models\\SwarmDocker' AND sw.id = r.destination_id \
         WHERE p.team_id = $1 ORDER BY v.key",
        union.join(" UNION ALL ")
    );
    Ok(sqlx::query_as(&sql)
        .bind(team_id)
        .fetch_all(db.pool())
        .await?)
}
