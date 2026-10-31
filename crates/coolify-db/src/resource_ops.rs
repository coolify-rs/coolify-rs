//! Rows behind `App\Livewire\Project\Shared\Destination`, `Maintenance`, `ResourceOperations` and
//! the delete job's view of an application: additional servers, own volumes and volume backups.

use chrono::NaiveDateTime;
use coolify_domain::new_public_id;
use sqlx::Row as _;

use crate::applications::MORPH;
use crate::records::{Record, parse_row};
use crate::{Db, DbError, DbResult};

fn decode(t: &str) -> DbResult<Record> {
    parse_row(t).map_err(|e| DbError::Schema(format!("decoding row: {e}")))
}

/// `$application->additional_servers`: the server ids.
pub async fn additional_server_ids(db: &Db, application_id: i64) -> DbResult<Vec<i64>> {
    Ok(sqlx::query_scalar(
        "SELECT server_id FROM additional_destinations WHERE application_id = $1 ORDER BY id",
    )
    .bind(application_id)
    .fetch_all(db.pool())
    .await?)
}

/// `$application->persistentStorages()` without `isSharedWithAnotherResource()`.
pub async fn own_volume_names(db: &Db, application_id: i64) -> DbResult<Vec<String>> {
    Ok(sqlx::query_scalar(
        "SELECT name FROM local_persistent_volumes WHERE resource_type = $1 AND resource_id = $2 \
         AND standalone_sqlite_id IS NULL ORDER BY id",
    )
    .bind(MORPH)
    .bind(application_id)
    .fetch_all(db.pool())
    .await?)
}

/// `volumeBackupSchedules()` of `DeleteResourceJob` for an application.
pub async fn volume_backup_schedules(db: &Db, application_id: i64) -> DbResult<Vec<Record>> {
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT row_to_json(b)::text FROM scheduled_volume_backups b WHERE \
         (b.backupable_type = 'App\\Models\\LocalPersistentVolume' AND b.backupable_id IN \
            (SELECT id FROM local_persistent_volumes WHERE resource_type = $1 AND resource_id = $2)) \
         OR (b.backupable_type = 'App\\Models\\LocalFileVolume' AND b.backupable_id IN \
            (SELECT id FROM local_file_volumes WHERE resource_type = $1 AND resource_id = $2)) ORDER BY b.id",
    )
    .bind(MORPH)
    .bind(application_id)
    .fetch_all(db.pool())
    .await?;
    rows.iter().map(|t| decode(t)).collect()
}

/// One row of `$resource->additional_networks` with what the page shows.
#[derive(Debug, Clone)]
pub struct AdditionalNetwork {
    pub network_id: i64,
    pub network_name: String,
    pub network: String,
    pub server_id: i64,
    pub server_uuid: String,
    pub server_name: String,
    pub status: String,
}

/// `$resource->additional_networks` with `server`.
pub async fn additional_networks(db: &Db, application_id: i64) -> DbResult<Vec<AdditionalNetwork>> {
    let rows = sqlx::query(
        "SELECT d.id, d.name, d.network, s.id AS server_id, s.uuid, s.name AS server_name, a.status \
         FROM additional_destinations a JOIN standalone_dockers d ON d.id = a.standalone_docker_id \
         JOIN servers s ON s.id = d.server_id WHERE a.application_id = $1 ORDER BY a.id",
    )
    .bind(application_id)
    .fetch_all(db.pool())
    .await?;
    Ok(rows
        .iter()
        .map(|r| AdditionalNetwork {
            network_id: r.get(0),
            network_name: r.get(1),
            network: r.get(2),
            server_id: r.get(3),
            server_uuid: r.get(4),
            server_name: r.get(5),
            status: r.get(6),
        })
        .collect())
}

/// `syncWithoutDetaching([$network->id => ['server_id' => $server->id]])`.
pub async fn attach_network(
    db: &Db,
    application_id: i64,
    network_id: i64,
    server_id: i64,
    now: NaiveDateTime,
) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(
        "INSERT INTO additional_destinations (application_id, server_id, standalone_docker_id, created_at, updated_at) \
         SELECT $1, $2, $3, $4, $4 WHERE NOT EXISTS (SELECT 1 FROM additional_destinations \
         WHERE application_id = $1 AND standalone_docker_id = $3)",
    )
    .bind(application_id)
    .bind(server_id)
    .bind(network_id)
    .bind(now)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// `additional_networks()->wherePivot('server_id', $serverId)->detach($networkId)`.
pub async fn detach_network(
    db: &Db,
    application_id: i64,
    network_id: i64,
    server_id: i64,
) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(
        "DELETE FROM additional_destinations WHERE application_id = $1 AND server_id = $2 AND standalone_docker_id = $3",
    )
    .bind(application_id)
    .bind(server_id)
    .bind(network_id)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// `promote()`: swaps the main destination with an additional one, in one transaction.
pub async fn promote_network(
    db: &Db,
    application_id: i64,
    network_id: i64,
    server_id: i64,
    main_network_id: i64,
    main_server_id: i64,
    now: NaiveDateTime,
) -> DbResult<()> {
    db.ensure_writable()?;
    let mut tx = db.pool().begin().await?;
    sqlx::query(
        "UPDATE applications SET destination_id = $2, destination_type = 'App\\Models\\StandaloneDocker', updated_at = $3 WHERE id = $1",
    )
    .bind(application_id)
    .bind(network_id)
    .bind(now)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "DELETE FROM additional_destinations WHERE application_id = $1 AND server_id = $2 AND standalone_docker_id = $3",
    )
    .bind(application_id)
    .bind(server_id)
    .bind(network_id)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO additional_destinations (application_id, server_id, standalone_docker_id) VALUES ($1, $2, $3)",
    )
    .bind(application_id)
    .bind(main_server_id)
    .bind(main_network_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Does the application have persistent volumes (`persistentStorages()->count() > 0`).
pub async fn has_persistent_storage(db: &Db, application_id: i64) -> DbResult<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM local_persistent_volumes WHERE resource_type = $1 AND resource_id = $2)",
    )
    .bind(MORPH)
    .bind(application_id)
    .fetch_one(db.pool())
    .await?)
}

/// `Project::query()->where('team_id', $teamId)->orderByRaw('LOWER(name)')`: `(id, name)`.
pub async fn team_projects(db: &Db, team_id: i64) -> DbResult<Vec<(i64, Option<String>)>> {
    Ok(
        sqlx::query_as("SELECT id, name FROM projects WHERE team_id = $1 ORDER BY LOWER(name)")
            .bind(team_id)
            .fetch_all(db.pool())
            .await?,
    )
}

/// `$project->environments`: `(id, name)`.
pub async fn project_environments(db: &Db, project_id: i64) -> DbResult<Vec<(i64, String)>> {
    Ok(
        sqlx::query_as("SELECT id, name FROM environments WHERE project_id = $1 ORDER BY id")
            .bind(project_id)
            .fetch_all(db.pool())
            .await?,
    )
}

/// `findResourceTeamEnvironment()`: `(environment uuid, project uuid)`.
pub async fn team_environment(
    db: &Db,
    team_id: i64,
    environment_id: i64,
) -> DbResult<Option<(String, String)>> {
    Ok(sqlx::query_as(
        "SELECT e.uuid, p.uuid FROM environments e JOIN projects p ON p.id = e.project_id WHERE e.id = $1 AND p.team_id = $2",
    )
    .bind(environment_id)
    .bind(team_id)
    .fetch_optional(db.pool())
    .await?)
}

/// `$resource->destination->uuid`; `table` is one of the code's own resource tables.
pub async fn destination_uuid(db: &Db, table: &str, id: i64) -> DbResult<Option<String>> {
    Ok(sqlx::query_scalar(&format!(
        "SELECT COALESCE(sd.uuid, sw.uuid) FROM {table} r \
         LEFT JOIN standalone_dockers sd ON r.destination_type = 'App\\Models\\StandaloneDocker' AND sd.id = r.destination_id \
         LEFT JOIN swarm_dockers sw ON r.destination_type = 'App\\Models\\SwarmDocker' AND sw.id = r.destination_id \
         WHERE r.id = $1"
    ))
    .bind(id)
    .fetch_optional(db.pool())
    .await?
    .flatten())
}

/// `$resource->fill(['environment_id' => $id])->save()`.
pub async fn set_environment(
    db: &Db,
    table: &str,
    id: i64,
    environment_id: i64,
    now: NaiveDateTime,
) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(&format!(
        "UPDATE {table} SET environment_id = $2, updated_at = $3 WHERE id = $1 AND environment_id <> $2"
    ))
    .bind(id)
    .bind(environment_id)
    .bind(now)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// The `environment_id` a new clone was filled with; its timestamps stay those of the insert.
pub async fn set_environment_only(
    db: &Db,
    table: &str,
    id: i64,
    environment_id: i64,
) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(&format!(
        "UPDATE {table} SET environment_id = $2 WHERE id = $1"
    ))
    .bind(id)
    .bind(environment_id)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// `cloneSecretManagerLinkTo()`.
pub async fn clone_secret_link(
    db: &Db,
    morph: &str,
    from: i64,
    to: i64,
    now: NaiveDateTime,
) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(
        "INSERT INTO secret_manager_links (uuid, resourceable_type, resourceable_id, integration_token_id, settings, created_at, updated_at) \
         SELECT $1, resourceable_type, $2, integration_token_id, settings, $3, $3 FROM secret_manager_links \
         WHERE resourceable_type = $4 AND resourceable_id = $5",
    )
    .bind(new_public_id())
    .bind(to)
    .bind(now)
    .bind(morph)
    .bind(from)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// `Project::query()->where('team_id', $teamId)->where('name', $name)->first()`.
pub async fn team_project_named(db: &Db, team_id: i64, name: &str) -> DbResult<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM projects WHERE team_id = $1 AND name = $2)",
    )
    .bind(team_id)
    .bind(name)
    .fetch_one(db.pool())
    .await?)
}

/// `$project->environments()->where('name', $name)->first()`.
pub async fn environment_named(db: &Db, project_id: i64, name: &str) -> DbResult<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM environments WHERE project_id = $1 AND name = $2)",
    )
    .bind(project_id)
    .bind(name)
    .fetch_one(db.pool())
    .await?)
}

/// `$project->environments()->create(['name' => $name, 'uuid' => new_public_id()])`: `(id, uuid)`.
pub async fn create_environment(
    db: &Db,
    project_id: i64,
    name: &str,
    now: NaiveDateTime,
) -> DbResult<(i64, String)> {
    db.ensure_writable()?;
    Ok(sqlx::query_as(
        "INSERT INTO environments (name, project_id, uuid, created_at, updated_at) VALUES ($1, $2, $3, $4, $4) RETURNING id, uuid",
    )
    .bind(name)
    .bind(project_id)
    .bind(new_public_id())
    .bind(now)
    .fetch_one(db.pool())
    .await?)
}

/// `Server::maintenanceResources()`: what `setupMaintenancePages` writes.
#[derive(Debug, Clone)]
pub struct MaintenanceResource {
    pub uuid: String,
    pub page: Option<String>,
    /// `maintenanceDomains()`: `(url, force_https)`.
    pub domains: Vec<(String, bool)>,
}

fn domains_of_list(fqdn: Option<&str>, force: bool) -> Vec<(String, bool)> {
    fqdn.unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .map(|d| (d.to_owned(), force))
        .collect()
}

/// Applications and services of the server with maintenance mode on.
pub async fn maintenance_resources(db: &Db, server_id: i64) -> DbResult<Vec<MaintenanceResource>> {
    type AppRow = (
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        String,
        Option<bool>,
    );
    let rows: Vec<AppRow> = sqlx::query_as(
        "SELECT a.uuid, a.maintenance_page, a.fqdn, a.docker_compose_domains, a.build_pack, st.is_force_https_enabled \
         FROM applications a LEFT JOIN application_settings st ON st.application_id = a.id \
         WHERE a.deleted_at IS NULL AND a.is_maintenance_enabled = true AND ( \
           (a.destination_type = 'App\\Models\\StandaloneDocker' AND a.destination_id IN (SELECT id FROM standalone_dockers WHERE server_id = $1)) \
           OR (a.destination_type = 'App\\Models\\SwarmDocker' AND a.destination_id IN (SELECT id FROM swarm_dockers WHERE server_id = $1)) \
           OR a.id IN (SELECT application_id FROM additional_destinations WHERE server_id = $1)) ORDER BY a.id",
    )
    .bind(server_id)
    .fetch_all(db.pool())
    .await?;
    let mut out = Vec::new();
    for (uuid, page, fqdn, compose, build_pack, force) in rows {
        let force = force.unwrap_or(false);
        let domains = if build_pack == "dockercompose" {
            let parsed: serde_json::Value = compose
                .as_deref()
                .and_then(|c| serde_json::from_str(c).ok())
                .unwrap_or(serde_json::Value::Null);
            let mut list = Vec::new();
            if let Some(services) = parsed.as_object() {
                for service in services.values() {
                    list.extend(domains_of_list(
                        service.get("domain").and_then(serde_json::Value::as_str),
                        force,
                    ));
                }
            }
            list
        } else {
            domains_of_list(fqdn.as_deref(), force)
        };
        out.push(MaintenanceResource {
            uuid,
            page,
            domains,
        });
    }
    let services: Vec<(i64, String, Option<String>)> = sqlx::query_as(
        "SELECT id, uuid, maintenance_page FROM services WHERE deleted_at IS NULL AND is_maintenance_enabled = true AND server_id = $1 ORDER BY id",
    )
    .bind(server_id)
    .fetch_all(db.pool())
    .await?;
    for (id, uuid, page) in services {
        let subs: Vec<(Option<String>, bool)> = sqlx::query_as(
            "SELECT fqdn, is_force_https_enabled FROM service_applications WHERE service_id = $1 ORDER BY id",
        )
        .bind(id)
        .fetch_all(db.pool())
        .await?;
        out.push(MaintenanceResource {
            uuid,
            page,
            domains: subs
                .iter()
                .flat_map(|(f, force)| domains_of_list(f.as_deref(), *force))
                .collect(),
        });
    }
    Ok(out)
}

/// `$resource->fill(['is_maintenance_enabled' => ..., 'maintenance_page' => ...])->save()`.
pub async fn set_maintenance(
    db: &Db,
    table: &str,
    id: i64,
    enabled: Option<bool>,
    page: Option<Option<&str>>,
    now: NaiveDateTime,
) -> DbResult<()> {
    db.ensure_writable()?;
    if let Some(enabled) = enabled {
        sqlx::query(&format!(
            "UPDATE {table} SET is_maintenance_enabled = $2, updated_at = $3 WHERE id = $1 AND is_maintenance_enabled <> $2"
        ))
        .bind(id)
        .bind(enabled)
        .bind(now)
        .execute(db.pool())
        .await?;
    }
    if let Some(page) = page {
        sqlx::query(&format!(
            "UPDATE {table} SET maintenance_page = $2, updated_at = $3 WHERE id = $1 AND maintenance_page IS DISTINCT FROM $2"
        ))
        .bind(id)
        .bind(page)
        .bind(now)
        .execute(db.pool())
        .await?;
    }
    Ok(())
}

/// The primary destination of a resource: `(id, uuid, name, network, server id, server uuid, server name)`.
pub async fn destination_info(
    db: &Db,
    table: &str,
    id: i64,
) -> DbResult<Option<(i64, String, String, String, i64, String, String)>> {
    Ok(sqlx::query_as(&format!(
        "SELECT d.id, d.uuid, d.name, d.network, s.id, s.uuid, s.name FROM {table} r \
         JOIN (SELECT id, uuid, name, network, server_id, 'App\\Models\\StandaloneDocker' AS morph FROM standalone_dockers \
               UNION ALL SELECT id, uuid, name, network, server_id, 'App\\Models\\SwarmDocker' FROM swarm_dockers) d \
           ON d.morph = r.destination_type AND d.id = r.destination_id \
         JOIN servers s ON s.id = d.server_id WHERE r.id = $1"
    ))
    .bind(id)
    .fetch_optional(db.pool())
    .await?)
}

/// One row of `$networks`: `(id, name, server id, server name)`.
pub async fn team_standalone_networks(
    db: &Db,
    team_id: i64,
) -> DbResult<Vec<(i64, String, i64, String)>> {
    Ok(sqlx::query_as(
        "SELECT d.id, d.name, s.id, s.name FROM servers s \
         JOIN server_settings st ON st.server_id = s.id \
         JOIN standalone_dockers d ON d.server_id = s.id \
         WHERE s.team_id = $1 AND s.deleted_at IS NULL AND st.is_reachable = true AND st.is_usable = true \
           AND st.is_swarm_worker = false AND st.force_disabled = false \
           AND COALESCE(st.server_role, CASE WHEN st.is_build_server THEN 'build' ELSE 'server' END) <> 'build' \
         ORDER BY s.name, d.id",
    )
    .bind(team_id)
    .fetch_all(db.pool())
    .await?)
}

/// `StandaloneDocker::query()->where('server_id', $serverId)->findOrFail($id)`.
pub async fn standalone_on_server(db: &Db, network_id: i64, server_id: i64) -> DbResult<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM standalone_dockers WHERE id = $1 AND server_id = $2)",
    )
    .bind(network_id)
    .bind(server_id)
    .fetch_one(db.pool())
    .await?)
}
