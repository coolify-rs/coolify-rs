//! Reads behind upstream `Services\TrafficResource` and `Livewire\Analytics::teamResources`.

use coolify_domain::TeamId;

use crate::{Db, DbResult};

const STANDALONE_DOCKER: &str = "App\\Models\\StandaloneDocker";
const SWARM_DOCKER: &str = "App\\Models\\SwarmDocker";

/// An Application or a Service stack of a team.
#[derive(Debug, Clone)]
pub struct TrafficResourceRow {
    pub id: i64,
    pub uuid: String,
    pub name: String,
    pub is_service: bool,
    pub server_uuid: Option<String>,
    pub environment_uuid: Option<String>,
    pub project_uuid: Option<String>,
    pub project_name: Option<String>,
    pub fqdn: Option<String>,
    pub docker_compose_domains: Option<String>,
    /// `$service->applications`: name and fqdn.
    pub service_applications: Vec<(String, Option<String>)>,
}

type Row = (
    i64,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

fn server_of_destination(alias: &str) -> String {
    format!(
        "(SELECT sv.uuid FROM servers sv WHERE sv.deleted_at IS NULL AND sv.id = CASE \
         WHEN {alias}.destination_type = '{STANDALONE_DOCKER}' THEN (SELECT sd.server_id FROM standalone_dockers sd WHERE sd.id = {alias}.destination_id) \
         WHEN {alias}.destination_type = '{SWARM_DOCKER}' THEN (SELECT sw.server_id FROM swarm_dockers sw WHERE sw.id = {alias}.destination_id) END)"
    )
}

/// `Application::ownedByCurrentTeam()` then `Service::ownedByCurrentTeam()`, each ordered by name,
/// with the relations `TrafficResource` reads.
pub async fn team_resources(db: &Db, team_id: TeamId) -> DbResult<Vec<TrafficResourceRow>> {
    let applications: Vec<Row> = sqlx::query_as(&format!(
        "SELECT a.id, a.uuid, a.name, {server}, e.uuid, p.uuid, p.name, a.fqdn, a.docker_compose_domains \
         FROM applications a JOIN environments e ON e.id = a.environment_id JOIN projects p ON p.id = e.project_id \
         WHERE a.deleted_at IS NULL AND p.team_id = $1 ORDER BY a.name",
        server = server_of_destination("a"),
    ))
    .bind(team_id)
    .fetch_all(db.pool())
    .await?;
    let services: Vec<Row> = sqlx::query_as(&format!(
        "SELECT s.id, s.uuid, s.name, COALESCE((SELECT sv.uuid FROM servers sv WHERE sv.deleted_at IS NULL AND sv.id = s.server_id), {server}), \
         e.uuid, p.uuid, p.name, NULL::text, NULL::text \
         FROM services s JOIN environments e ON e.id = s.environment_id JOIN projects p ON p.id = e.project_id \
         WHERE s.deleted_at IS NULL AND p.team_id = $1 ORDER BY s.name",
        server = server_of_destination("s"),
    ))
    .bind(team_id)
    .fetch_all(db.pool())
    .await?;
    let mut out = Vec::with_capacity(applications.len() + services.len());
    let row = |r: Row, is_service: bool| TrafficResourceRow {
        id: r.0,
        uuid: r.1,
        name: r.2.unwrap_or_default(),
        is_service,
        server_uuid: r.3,
        environment_uuid: r.4,
        project_uuid: r.5,
        project_name: r.6,
        fqdn: r.7,
        docker_compose_domains: r.8,
        service_applications: Vec::new(),
    };
    out.extend(applications.into_iter().map(|r| row(r, false)));
    for service in services {
        let mut resource = row(service, true);
        resource.service_applications = sqlx::query_as(
            "SELECT COALESCE(name, ''), fqdn FROM service_applications WHERE service_id = $1 AND deleted_at IS NULL ORDER BY id",
        )
        .bind(resource.id)
        .fetch_all(db.pool())
        .await?;
        out.push(resource);
    }
    Ok(out)
}

/// `$application->previews()->where('pull_request_id', ...)->first()`: fqdn and compose domains.
pub async fn preview_domains(
    db: &Db,
    application_id: i64,
    pull_request_id: i64,
) -> DbResult<Option<(Option<String>, Option<String>)>> {
    Ok(sqlx::query_as(
        "SELECT fqdn, docker_compose_domains FROM application_previews \
         WHERE application_id = $1 AND pull_request_id = $2 AND deleted_at IS NULL LIMIT 1",
    )
    .bind(application_id)
    .bind(pull_request_id)
    .fetch_optional(db.pool())
    .await?)
}
