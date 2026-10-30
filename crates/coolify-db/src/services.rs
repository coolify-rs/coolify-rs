//! `services`, `service_applications`, `service_databases` and the rows that hang off them
//! (`app/Models/{Service,ServiceApplication,ServiceDatabase}.php`).

use chrono::NaiveDateTime;
use coolify_domain::{TeamId, new_public_id};
use serde_json::Value;

use crate::records::{ENVIRONMENT, ModelSpec, PROJECT, Record, TEAM, parse_row};
use crate::servers::{ColumnValue, bind_all, set_clause};
use crate::{Db, DbError, DbResult};

pub const MORPH: &str = r"App\Models\Service";
pub const APPLICATION_MORPH: &str = r"App\Models\ServiceApplication";
pub const DATABASE_MORPH: &str = r"App\Models\ServiceDatabase";

/// The attributes `Service::toArray()` serializes (`$hidden` is applied by the caller, which may
/// `makeVisible`).
pub static SERVICE: ModelSpec = ModelSpec {
    table: "services",
    known: &[
        "id",
        "uuid",
        "name",
        "environment_id",
        "created_at",
        "updated_at",
        "server_id",
        "description",
        "docker_compose_raw",
        "docker_compose",
        "destination_type",
        "destination_id",
        "deleted_at",
        "connect_to_docker_network",
        "config_hash",
        "service_type",
        "is_container_label_escape_enabled",
        "compose_parsing_version",
        "redact_all_env_values_in_logs",
        "is_maintenance_enabled",
        "maintenance_page",
        "include_coolify_default_variables",
    ],
    hidden: &[],
    timestamps: &["created_at", "updated_at", "deleted_at"],
    datetimes: &["created_at", "updated_at", "deleted_at"],
};

pub static SERVICE_APPLICATION: ModelSpec = ModelSpec {
    table: "service_applications",
    known: &[
        "id",
        "uuid",
        "name",
        "human_name",
        "description",
        "fqdn",
        "ports",
        "exposes",
        "status",
        "service_id",
        "created_at",
        "updated_at",
        "exclude_from_status",
        "required_fqdn",
        "image",
        "is_log_drain_enabled",
        "is_include_timestamps",
        "deleted_at",
        "is_gzip_enabled",
        "is_stripprefix_enabled",
        "last_online_at",
        "is_migrated",
        "noindex_domains",
        "redirect",
        "is_force_https_enabled",
        "restart_count",
        "max_restart_count",
        "restart_limit_reached",
        "last_restart_at",
        "last_restart_type",
        "traefik_certificate_resolver",
        "is_permanent_redirect_enabled",
        "domain_dns_statuses",
        "domain_port_overrides",
    ],
    hidden: &["domain_dns_statuses", "domain_port_overrides"],
    timestamps: &[
        "created_at",
        "updated_at",
        "deleted_at",
        "last_online_at",
        "last_restart_at",
    ],
    datetimes: &["created_at", "updated_at", "deleted_at"],
};

pub static SERVICE_DATABASE: ModelSpec = ModelSpec {
    table: "service_databases",
    known: &[
        "id",
        "uuid",
        "name",
        "human_name",
        "description",
        "ports",
        "exposes",
        "status",
        "service_id",
        "created_at",
        "updated_at",
        "exclude_from_status",
        "image",
        "public_port",
        "is_public",
        "is_log_drain_enabled",
        "is_include_timestamps",
        "deleted_at",
        "is_gzip_enabled",
        "is_stripprefix_enabled",
        "last_online_at",
        "is_migrated",
        "custom_type",
        "public_port_timeout",
    ],
    hidden: &[],
    timestamps: &["created_at", "updated_at", "deleted_at", "last_online_at"],
    datetimes: &["created_at", "updated_at", "deleted_at"],
};

/// A service with the relations its parser, actions and presentation read.
#[derive(Debug, Clone)]
pub struct ServiceRow {
    pub id: i64,
    pub uuid: String,
    pub team_id: TeamId,
    pub raw: Record,
    pub environment: Record,
    pub project: Record,
    pub team: Record,
}

impl ServiceRow {
    pub fn str(&self, key: &str) -> Option<&str> {
        self.raw.get(key).and_then(Value::as_str)
    }
    pub fn int(&self, key: &str) -> Option<i64> {
        self.raw.get(key).and_then(Value::as_i64)
    }
    pub fn bool(&self, key: &str) -> bool {
        self.raw.get(key).and_then(Value::as_bool).unwrap_or(false)
    }
    pub fn server_id(&self) -> Option<i64> {
        self.int("server_id")
    }
    pub fn name(&self) -> &str {
        self.str("name").unwrap_or_default()
    }
    /// `(int) $service->compose_parsing_version`.
    pub fn parsing_version(&self) -> i64 {
        self.str("compose_parsing_version")
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(0)
    }
}

fn decode(text: &str, what: &str) -> DbResult<Record> {
    parse_row(text).map_err(|e| DbError::Schema(format!("decoding {what} row: {e}")))
}

type Joined = (String, String, String, String);

fn select(filter: &str) -> String {
    format!(
        "SELECT row_to_json(s)::text, row_to_json(e)::text, row_to_json(p)::text, row_to_json(t)::text \
         FROM services s JOIN environments e ON e.id = s.environment_id JOIN projects p ON p.id = e.project_id \
         JOIN teams t ON t.id = p.team_id WHERE s.deleted_at IS NULL AND {filter}"
    )
}

fn to_row(j: &Joined) -> DbResult<ServiceRow> {
    let raw = decode(&j.0, "service")?;
    let environment = ENVIRONMENT.present(decode(&j.1, "environment")?);
    let project = PROJECT.present(decode(&j.2, "project")?);
    let team = TEAM.present(decode(&j.3, "team")?);
    Ok(ServiceRow {
        id: raw.get("id").and_then(Value::as_i64).unwrap_or_default(),
        uuid: raw
            .get("uuid")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        team_id: project
            .get("team_id")
            .and_then(Value::as_i64)
            .unwrap_or_default(),
        raw,
        environment,
        project,
        team,
    })
}

/// `Service::whereRelation('environment.project.team', 'id', $teamId)->whereUuid($uuid)->first()`.
pub async fn find_in_team(db: &Db, team_id: TeamId, uuid: &str) -> DbResult<Option<ServiceRow>> {
    let r: Option<Joined> = sqlx::query_as(&select(
        "s.uuid = $1 AND p.team_id = $2 ORDER BY s.id LIMIT 1",
    ))
    .bind(uuid)
    .bind(team_id)
    .fetch_optional(db.pool())
    .await?;
    r.as_ref().map(to_row).transpose()
}

pub async fn find_by_id(db: &Db, id: i64) -> DbResult<Option<ServiceRow>> {
    let r: Option<Joined> = sqlx::query_as(&select("s.id = $1"))
        .bind(id)
        .fetch_optional(db.pool())
        .await?;
    r.as_ref().map(to_row).transpose()
}

/// `$project->services()->get()` for every project of the team, in `Project::where(team_id)` order.
pub async fn list_for_team(db: &Db, team_id: TeamId) -> DbResult<Vec<ServiceRow>> {
    let projects: Vec<(i64,)> = sqlx::query_as("SELECT id FROM projects WHERE team_id = $1")
        .bind(team_id)
        .fetch_all(db.pool())
        .await?;
    let mut out = Vec::new();
    for (project,) in projects {
        let rows: Vec<Joined> = sqlx::query_as(&select("e.project_id = $1"))
            .bind(project)
            .fetch_all(db.pool())
            .await?;
        for r in &rows {
            out.push(to_row(r)?);
        }
    }
    Ok(out)
}

/// `Server::services()`.
pub async fn list_for_server(db: &Db, server_id: i64) -> DbResult<Vec<ServiceRow>> {
    let rows: Vec<Joined> = sqlx::query_as(&select("s.server_id = $1 ORDER BY s.id"))
        .bind(server_id)
        .fetch_all(db.pool())
        .await?;
    rows.iter().map(to_row).collect()
}

/// Servers with at least one service (the status poller).
pub async fn servers_with_services(db: &Db) -> DbResult<Vec<i64>> {
    let rows: Vec<(i64,)> = sqlx::query_as(
        "SELECT DISTINCT server_id FROM services WHERE deleted_at IS NULL AND server_id IS NOT NULL",
    )
    .fetch_all(db.pool())
    .await?;
    Ok(rows.into_iter().map(|r| r.0).collect())
}

/// The attributes `new Service([...])->save()` writes.
#[derive(Debug, Clone, Default)]
pub struct NewService {
    pub uuid: Option<String>,
    pub name: String,
    pub description: Option<String>,
    pub docker_compose_raw: String,
    pub environment_id: i64,
    pub service_type: Option<String>,
    pub server_id: i64,
    pub destination_id: i64,
    pub destination_type: String,
    pub connect_to_docker_network: Option<bool>,
    pub is_container_label_escape_enabled: Option<bool>,
    pub redact_all_env_values_in_logs: Option<bool>,
}

/// `$service->save()` for a new model, then the `created` hook (`compose_parsing_version`).
pub async fn create(
    db: &Db,
    n: &NewService,
    parser_version: &str,
    now: NaiveDateTime,
) -> DbResult<(i64, String)> {
    db.ensure_writable()?;
    let uuid = n.uuid.clone().unwrap_or_else(new_public_id);
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO services (uuid, name, description, docker_compose_raw, environment_id, service_type, server_id, \
         destination_id, destination_type, connect_to_docker_network, is_container_label_escape_enabled, \
         redact_all_env_values_in_logs, compose_parsing_version, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, COALESCE($10, false), COALESCE($11, true), COALESCE($12, true), $13, $14, $14) \
         RETURNING id",
    )
    .bind(&uuid)
    .bind(&n.name)
    .bind(&n.description)
    .bind(&n.docker_compose_raw)
    .bind(n.environment_id)
    .bind(&n.service_type)
    .bind(n.server_id)
    .bind(n.destination_id)
    .bind(&n.destination_type)
    .bind(n.connect_to_docker_network)
    .bind(n.is_container_label_escape_enabled)
    .bind(n.redact_all_env_values_in_logs)
    .bind(parser_version)
    .bind(now)
    .fetch_one(db.pool())
    .await?;
    Ok((id, uuid))
}

/// `$service->save()` after the caller changed attributes: only when a value differs.
pub async fn update(
    db: &Db,
    id: i64,
    values: &[(&'static str, ColumnValue)],
    now: NaiveDateTime,
) -> DbResult<bool> {
    update_row(db, "services", id, values, now).await
}

async fn update_row(
    db: &Db,
    table: &str,
    id: i64,
    values: &[(&'static str, ColumnValue)],
    now: NaiveDateTime,
) -> DbResult<bool> {
    if values.is_empty() {
        return Ok(false);
    }
    db.ensure_writable()?;
    let changed = values
        .iter()
        .enumerate()
        .map(|(i, (col, v))| {
            if matches!(v, ColumnValue::Text(None)) {
                format!("\"{col}\" IS NOT NULL")
            } else {
                let cast = match v {
                    ColumnValue::Int(_) => "::integer",
                    _ => "",
                };
                format!("\"{col}\" IS DISTINCT FROM ${}{cast}", i + 3)
            }
        })
        .collect::<Vec<_>>()
        .join(" OR ");
    let sql = format!(
        "UPDATE {table} SET {}, updated_at = $1 WHERE id = $2 AND ({changed})",
        set_clause(values, 3)
    );
    let done = bind_all(sqlx::query(&sql).bind(now).bind(id), values)
        .execute(db.pool())
        .await?;
    Ok(done.rows_affected() > 0)
}

/// The `SoftDeletes` delete of `$service->delete()`.
pub async fn soft_delete(db: &Db, id: i64, now: NaiveDateTime) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query("UPDATE services SET deleted_at = $2, updated_at = $2 WHERE id = $1")
        .bind(id)
        .bind(now)
        .execute(db.pool())
        .await?;
    Ok(())
}

/// A trashed service (the delete job loads it `withTrashed`).
pub async fn find_trashed(db: &Db, id: i64) -> DbResult<Option<ServiceRow>> {
    let r: Option<Joined> = sqlx::query_as(
        "SELECT row_to_json(s)::text, row_to_json(e)::text, row_to_json(p)::text, row_to_json(t)::text \
         FROM services s JOIN environments e ON e.id = s.environment_id JOIN projects p ON p.id = e.project_id \
         JOIN teams t ON t.id = p.team_id WHERE s.id = $1",
    )
    .bind(id)
    .fetch_optional(db.pool())
    .await?;
    r.as_ref().map(to_row).transpose()
}

/// `DeleteService::deleteLocal()`: child rows and their storages, tasks, variables, tags, the row.
pub async fn force_delete(db: &Db, id: i64) -> DbResult<()> {
    db.ensure_writable()?;
    let mut tx = db.pool().begin().await?;
    for (table, morph) in [
        ("service_applications", APPLICATION_MORPH),
        ("service_databases", DATABASE_MORPH),
    ] {
        let children: Vec<(i64,)> =
            sqlx::query_as(&format!("SELECT id FROM {table} WHERE service_id = $1"))
                .bind(id)
                .fetch_all(&mut *tx)
                .await?;
        for (child,) in children {
            for storage in ["local_persistent_volumes", "local_file_volumes"] {
                sqlx::query(&format!(
                    "DELETE FROM {storage} WHERE resource_type = $1 AND resource_id = $2"
                ))
                .bind(morph)
                .bind(child)
                .execute(&mut *tx)
                .await?;
            }
            if morph == DATABASE_MORPH {
                sqlx::query(
                    "DELETE FROM scheduled_database_backups WHERE database_type = $1 AND database_id = $2",
                )
                .bind(morph)
                .bind(child)
                .execute(&mut *tx)
                .await?;
            }
        }
        sqlx::query(&format!("DELETE FROM {table} WHERE service_id = $1"))
            .bind(id)
            .execute(&mut *tx)
            .await?;
    }
    sqlx::query("DELETE FROM scheduled_tasks WHERE service_id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "DELETE FROM environment_variables WHERE resourceable_type = $1 AND resourceable_id = $2",
    )
    .bind(MORPH)
    .bind(id)
    .execute(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM taggables WHERE taggable_type = $1 AND taggable_id = $2")
        .bind(MORPH)
        .bind(id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM services WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

// --- service applications and databases -----------------------------------------------------

/// Which child table a row lives in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubKind {
    Application,
    Database,
}

impl SubKind {
    pub fn table(self) -> &'static str {
        match self {
            SubKind::Application => "service_applications",
            SubKind::Database => "service_databases",
        }
    }
    pub fn morph(self) -> &'static str {
        match self {
            SubKind::Application => APPLICATION_MORPH,
            SubKind::Database => DATABASE_MORPH,
        }
    }
    /// The `coolify.service.subType` label and the storage API's `resource_type`.
    pub fn sub_type(self) -> &'static str {
        match self {
            SubKind::Application => "application",
            SubKind::Database => "database",
        }
    }
}

/// A `service_applications` or `service_databases` row.
#[derive(Debug, Clone)]
pub struct SubRow {
    pub kind: SubKind,
    pub id: i64,
    pub uuid: String,
    pub name: String,
    pub raw: Record,
}

impl SubRow {
    pub fn str(&self, key: &str) -> Option<&str> {
        self.raw.get(key).and_then(Value::as_str)
    }
    pub fn int(&self, key: &str) -> Option<i64> {
        self.raw.get(key).and_then(Value::as_i64)
    }
    pub fn bool(&self, key: &str) -> bool {
        self.raw.get(key).and_then(Value::as_bool).unwrap_or(false)
    }
    pub fn bool_or(&self, key: &str, default: bool) -> bool {
        self.raw
            .get(key)
            .and_then(Value::as_bool)
            .unwrap_or(default)
    }
    pub fn service_id(&self) -> i64 {
        self.int("service_id").unwrap_or_default()
    }
    pub fn is_application(&self) -> bool {
        self.kind == SubKind::Application
    }
}

fn sub_row(kind: SubKind, text: &str) -> DbResult<SubRow> {
    let raw = decode(text, kind.table())?;
    Ok(SubRow {
        kind,
        id: raw.get("id").and_then(Value::as_i64).unwrap_or_default(),
        uuid: raw
            .get("uuid")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        name: raw
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        raw,
    })
}

/// `$service->applications()->get()` / `databases()->get()` (table order).
pub async fn subs(db: &Db, kind: SubKind, service_id: i64) -> DbResult<Vec<SubRow>> {
    let rows: Vec<(String,)> = sqlx::query_as(&format!(
        "SELECT row_to_json(x)::text FROM {} x WHERE x.service_id = $1 AND x.deleted_at IS NULL ORDER BY x.id",
        kind.table()
    ))
    .bind(service_id)
    .fetch_all(db.pool())
    .await?;
    rows.iter().map(|r| sub_row(kind, &r.0)).collect()
}

/// `Sub::where('name', $name)->where('service_id', $id)->first()`.
pub async fn sub_by_name(
    db: &Db,
    kind: SubKind,
    service_id: i64,
    name: &str,
) -> DbResult<Option<SubRow>> {
    let row: Option<(String,)> = sqlx::query_as(&format!(
        "SELECT row_to_json(x)::text FROM {} x WHERE x.service_id = $1 AND x.name = $2 AND x.deleted_at IS NULL \
         LIMIT 1",
        kind.table()
    ))
    .bind(service_id)
    .bind(name)
    .fetch_optional(db.pool())
    .await?;
    row.map(|r| sub_row(kind, &r.0)).transpose()
}

/// `$service->applications()->where('uuid', $uuid)->first()` (or `databases()`).
pub async fn sub_by_uuid(
    db: &Db,
    kind: SubKind,
    service_id: i64,
    uuid: &str,
) -> DbResult<Option<SubRow>> {
    let row: Option<(String,)> = sqlx::query_as(&format!(
        "SELECT row_to_json(x)::text FROM {} x WHERE x.service_id = $1 AND x.uuid = $2 AND x.deleted_at IS NULL \
         LIMIT 1",
        kind.table()
    ))
    .bind(service_id)
    .bind(uuid)
    .fetch_optional(db.pool())
    .await?;
    row.map(|r| sub_row(kind, &r.0)).transpose()
}

/// `Sub::create(['name', 'service_id'])`: the model defaults come from the columns.
pub async fn sub_create(
    db: &Db,
    kind: SubKind,
    service_id: i64,
    name: &str,
    now: NaiveDateTime,
) -> DbResult<SubRow> {
    db.ensure_writable()?;
    let row: (String,) = sqlx::query_as(&format!(
        "INSERT INTO {t} AS x (name, service_id, uuid, created_at, updated_at) VALUES ($1, $2, $3, $4, $4) \
         RETURNING row_to_json(x)::text",
        t = kind.table()
    ))
    .bind(name)
    .bind(service_id)
    .bind(new_public_id())
    .bind(now)
    .fetch_one(db.pool())
    .await?;
    sub_row(kind, &row.0)
}

/// A sub-resource's `save()` / `update()` when the values differ. `last_online_at` follows a
/// changed `status` (the `saving` hooks).
pub async fn sub_update(
    db: &Db,
    kind: SubKind,
    id: i64,
    values: &[(&'static str, ColumnValue)],
    now: NaiveDateTime,
) -> DbResult<bool> {
    update_row(db, kind.table(), id, values, now).await
}

/// `$sub->image = $image; $sub->save()`: a `Stringable` attribute is always dirty.
pub async fn sub_write_image(
    db: &Db,
    kind: SubKind,
    id: i64,
    image: &str,
    now: NaiveDateTime,
) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(&format!(
        "UPDATE {} SET image = $2, updated_at = $3 WHERE id = $1",
        kind.table()
    ))
    .bind(id)
    .bind(image)
    .bind(now)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// Writes a JSON column of a sub-resource (`noindex_domains`, `domain_port_overrides`).
pub async fn sub_set_json(
    db: &Db,
    kind: SubKind,
    id: i64,
    column: &'static str,
    json: Option<&str>,
    now: NaiveDateTime,
) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(&format!(
        "UPDATE {} SET \"{column}\" = $2::json, updated_at = $3 WHERE id = $1",
        kind.table()
    ))
    .bind(id)
    .bind(json)
    .bind(now)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// `HasRestartLimit::resetRestartLimit()`.
pub async fn reset_restart_limit(db: &Db, id: i64, now: NaiveDateTime) -> DbResult<()> {
    sub_update(
        db,
        SubKind::Application,
        id,
        &[
            ("restart_count", ColumnValue::Int(0)),
            ("restart_limit_reached", ColumnValue::Bool(false)),
            ("last_restart_at", ColumnValue::Text(None)),
            ("last_restart_type", ColumnValue::Text(None)),
        ],
        now,
    )
    .await?;
    Ok(())
}

/// `$sub->update(['status' => $status])` with the `saving` hook.
pub async fn sub_set_status(
    db: &Db,
    kind: SubKind,
    id: i64,
    status: &str,
    now: NaiveDateTime,
) -> DbResult<bool> {
    db.ensure_writable()?;
    let done = sqlx::query(&format!(
        "UPDATE {} SET status = $2, last_online_at = $3, updated_at = $3 WHERE id = $1 AND status IS DISTINCT FROM $2",
        kind.table()
    ))
    .bind(id)
    .bind(status)
    .bind(now)
    .execute(db.pool())
    .await?;
    Ok(done.rows_affected() > 0)
}

/// `$sub->last_online_at = now(); $sub->save()` of `GetContainersStatus`.
pub async fn sub_touch_online(db: &Db, kind: SubKind, id: i64, now: NaiveDateTime) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(&format!(
        "UPDATE {} SET last_online_at = $2, updated_at = $2 WHERE id = $1",
        kind.table()
    ))
    .bind(id)
    .bind(now)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// `data_get($service, 'destination.network')`; `coolify` when the destination is gone.
pub async fn destination_network(db: &Db, service: &ServiceRow) -> DbResult<String> {
    let id = service.int("destination_id").unwrap_or_default();
    let table = match service.str("destination_type") {
        Some(t) if t.ends_with("SwarmDocker") => "swarm_dockers",
        _ => "standalone_dockers",
    };
    let row: Option<(String,)> =
        sqlx::query_as(&format!("SELECT network FROM {table} WHERE id = $1"))
            .bind(id)
            .fetch_optional(db.pool())
            .await?;
    Ok(row.map_or_else(|| "coolify".to_owned(), |r| r.0))
}

/// `$service->destination` as `StandaloneDocker::toArray()`.
pub async fn destination_record(db: &Db, service: &ServiceRow) -> DbResult<Option<Record>> {
    let id = service.int("destination_id").unwrap_or_default();
    let swarm = service
        .str("destination_type")
        .is_some_and(|t| t.ends_with("SwarmDocker"));
    let table = if swarm {
        "swarm_dockers"
    } else {
        "standalone_dockers"
    };
    let row: Option<(String,)> = sqlx::query_as(&format!(
        "SELECT row_to_json(d)::text FROM {table} d WHERE d.id = $1"
    ))
    .bind(id)
    .fetch_optional(db.pool())
    .await?;
    row.map(|r| {
        decode(&r.0, table).map(|d| {
            if swarm {
                d
            } else {
                crate::records::STANDALONE_DOCKER.present(d)
            }
        })
    })
    .transpose()
}

// --- storages --------------------------------------------------------------------------------

/// One `local_file_volumes` row by id.
pub async fn file_storage_by_id(db: &Db, id: i64) -> DbResult<Option<Record>> {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT row_to_json(v)::text FROM local_file_volumes v WHERE v.id = $1")
            .bind(id)
            .fetch_optional(db.pool())
            .await?;
    row.map(|r| decode(&r.0, "local_file_volumes")).transpose()
}

/// `$sub->persistentStorages` as raw rows (table order).
pub async fn persistent_storages(db: &Db, kind: SubKind, id: i64) -> DbResult<Vec<Record>> {
    rows_of(
        db,
        "SELECT row_to_json(v)::text FROM local_persistent_volumes v WHERE v.resource_type = $1 AND v.resource_id = $2",
        kind.morph(),
        id,
    )
    .await
}

/// `$sub->fileStorages` as raw rows (table order).
pub async fn file_storages(db: &Db, kind: SubKind, id: i64) -> DbResult<Vec<Record>> {
    rows_of(
        db,
        "SELECT row_to_json(v)::text FROM local_file_volumes v WHERE v.resource_type = $1 AND v.resource_id = $2",
        kind.morph(),
        id,
    )
    .await
}

async fn rows_of(db: &Db, sql: &str, morph: &str, id: i64) -> DbResult<Vec<Record>> {
    let rows: Vec<(String,)> = sqlx::query_as(sql)
        .bind(morph)
        .bind(id)
        .fetch_all(db.pool())
        .await?;
    rows.iter()
        .map(|r| decode(&r.0, "storage"))
        .collect::<DbResult<Vec<_>>>()
}

/// `LocalPersistentVolume::create([...])`.
pub async fn persistent_storage_create(
    db: &Db,
    morph: &str,
    resource_id: i64,
    name: &str,
    mount_path: &str,
    now: NaiveDateTime,
) -> DbResult<Record> {
    db.ensure_writable()?;
    let row: (String,) = sqlx::query_as(
        "INSERT INTO local_persistent_volumes AS v (name, mount_path, resource_id, resource_type, uuid, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $6) RETURNING row_to_json(v)::text",
    )
    .bind(name)
    .bind(mount_path)
    .bind(resource_id)
    .bind(morph)
    .bind(new_public_id())
    .bind(now)
    .fetch_one(db.pool())
    .await?;
    decode(&row.0, "local_persistent_volumes")
}

/// The attributes of `LocalFileVolume::create([...])`.
#[derive(Debug, Clone, Default)]
pub struct NewFileStorage {
    pub fs_path: String,
    pub mount_path: String,
    pub content_encrypted: Option<String>,
    pub is_directory: bool,
    pub is_host_file: bool,
}

pub async fn file_storage_create(
    db: &Db,
    morph: &str,
    resource_id: i64,
    n: &NewFileStorage,
    now: NaiveDateTime,
) -> DbResult<Record> {
    db.ensure_writable()?;
    let row: (String,) = sqlx::query_as(
        "INSERT INTO local_file_volumes AS v (fs_path, mount_path, content, is_directory, is_host_file, resource_id, resource_type, uuid, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $9) RETURNING row_to_json(v)::text",
    )
    .bind(&n.fs_path)
    .bind(&n.mount_path)
    .bind(&n.content_encrypted)
    .bind(n.is_directory)
    .bind(n.is_host_file)
    .bind(resource_id)
    .bind(morph)
    .bind(new_public_id())
    .bind(now)
    .fetch_one(db.pool())
    .await?;
    decode(&row.0, "local_file_volumes")
}

/// `$storage->save()` of a persistent or file storage when values differ, returns the row.
pub async fn storage_update(
    db: &Db,
    table: &'static str,
    id: i64,
    values: &[(&'static str, ColumnValue)],
    now: NaiveDateTime,
) -> DbResult<Option<Record>> {
    update_row(db, table, id, values, now).await?;
    let row: Option<(String,)> = sqlx::query_as(&format!(
        "SELECT row_to_json(v)::text FROM {table} v WHERE v.id = $1"
    ))
    .bind(id)
    .fetch_optional(db.pool())
    .await?;
    row.map(|r| decode(&r.0, table)).transpose()
}

/// `$storage->delete()`.
pub async fn storage_delete(db: &Db, table: &'static str, id: i64) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(&format!("DELETE FROM {table} WHERE id = $1"))
        .bind(id)
        .execute(db.pool())
        .await?;
    Ok(())
}

/// `$storage->scheduledBackups()->exists()`.
pub async fn storage_has_backups(db: &Db, morph: &str, id: i64) -> DbResult<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM scheduled_volume_backups WHERE backupable_type = $1 AND backupable_id = $2)",
    )
    .bind(morph)
    .bind(id)
    .fetch_one(db.pool())
    .await?)
}

// --- environment variables -------------------------------------------------------------------

/// The `delete()` of the corrupted keys at the start of `serviceParser()`.
pub async fn env_delete_corrupted(db: &Db, service_id: i64) -> DbResult<u64> {
    db.ensure_writable()?;
    let done = sqlx::query(
        "DELETE FROM environment_variables WHERE resourceable_type = $1 AND resourceable_id = $2 \
         AND (key LIKE '$%' OR key LIKE '%}')",
    )
    .bind(MORPH)
    .bind(service_id)
    .execute(db.pool())
    .await?;
    Ok(done.rows_affected())
}

/// `updateOrCreate(..., ['comment' => ...])`: the comment half of the dirty check.
pub async fn env_set_comment(
    db: &Db,
    id: i64,
    comment: Option<&str>,
    now: NaiveDateTime,
) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(
        "UPDATE environment_variables SET comment = $2, updated_at = $3 WHERE id = $1 AND comment IS DISTINCT FROM $2",
    )
    .bind(id)
    .bind(comment)
    .bind(now)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// `$service->environment_variables()->where('key', $key)->delete()`.
pub async fn env_delete_key(db: &Db, service_id: i64, key: &str) -> DbResult<u64> {
    db.ensure_writable()?;
    let done = sqlx::query(
        "DELETE FROM environment_variables WHERE resourceable_type = $1 AND resourceable_id = $2 AND key = $3",
    )
    .bind(MORPH)
    .bind(service_id)
    .bind(key)
    .execute(db.pool())
    .await?;
    Ok(done.rows_affected())
}

/// `$resource->environment_variables()->where('key', 'LIKE', $key.'_%')->pluck('key')`.
pub async fn env_keys_like(db: &Db, service_id: i64, prefix: &str) -> DbResult<Vec<String>> {
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT key FROM environment_variables WHERE resourceable_type = $1 AND resourceable_id = $2 AND key LIKE $3",
    )
    .bind(MORPH)
    .bind(service_id)
    .bind(format!("{prefix}_%"))
    .fetch_all(db.pool())
    .await?;
    Ok(rows.into_iter().map(|r| r.0).collect())
}

/// `$service->environment_variables` as raw rows (table order).
pub async fn env_rows(db: &Db, service_id: i64) -> DbResult<Vec<Record>> {
    rows_of(
        db,
        "SELECT row_to_json(v)::text FROM environment_variables v WHERE v.resourceable_type = $1 AND v.resourceable_id = $2 ORDER BY v.id",
        MORPH,
        service_id,
    )
    .await
}

/// One variable of the service by key.
pub async fn env_row_by_key(db: &Db, service_id: i64, key: &str) -> DbResult<Option<Record>> {
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT row_to_json(v)::text FROM environment_variables v WHERE v.resourceable_type = $1 AND v.resourceable_id = $2 \
         AND v.key = $3 LIMIT 1",
    )
    .bind(MORPH)
    .bind(service_id)
    .bind(key)
    .fetch_optional(db.pool())
    .await?;
    row.map(|r| decode(&r.0, "environment_variables"))
        .transpose()
}

/// One variable of the service by uuid.
pub async fn env_row_by_uuid(db: &Db, service_id: i64, uuid: &str) -> DbResult<Option<Record>> {
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT row_to_json(v)::text FROM environment_variables v WHERE v.resourceable_type = $1 AND v.resourceable_id = $2 \
         AND v.uuid = $3 LIMIT 1",
    )
    .bind(MORPH)
    .bind(service_id)
    .bind(uuid)
    .fetch_optional(db.pool())
    .await?;
    row.map(|r| decode(&r.0, "environment_variables"))
        .transpose()
}

/// `$env->save()` of the API with the columns that changed.
pub async fn env_update_columns(
    db: &Db,
    id: i64,
    values: &[(&'static str, ColumnValue)],
    now: NaiveDateTime,
) -> DbResult<bool> {
    update_row(db, "environment_variables", id, values, now).await
}

/// `$env->forceDelete()`.
pub async fn env_delete(db: &Db, id: i64) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query("DELETE FROM environment_variables WHERE id = $1")
        .bind(id)
        .execute(db.pool())
        .await?;
    Ok(())
}

// --- tags -------------------------------------------------------------------------------------

/// `$service->tags` (id, uuid, name, team_id, timestamps; pivot added by the caller's serializer).
pub async fn tags_of(db: &Db, service_id: i64) -> DbResult<Vec<Record>> {
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT row_to_json(t)::text FROM tags t JOIN taggables p ON p.tag_id = t.id \
         WHERE p.taggable_type = $1 AND p.taggable_id = $2",
    )
    .bind(MORPH)
    .bind(service_id)
    .fetch_all(db.pool())
    .await?;
    rows.iter().map(|r| decode(&r.0, "tags")).collect()
}

/// `Tag::query()->createOrFirst(['team_id', 'name'])` then `syncWithoutDetaching([$tag->id])`.
pub async fn attach_tag(
    db: &Db,
    team_id: TeamId,
    service_id: i64,
    name: &str,
    now: NaiveDateTime,
) -> DbResult<()> {
    db.ensure_writable()?;
    let found: Option<(i64,)> =
        sqlx::query_as("SELECT id FROM tags WHERE team_id = $1 AND name = $2 LIMIT 1")
            .bind(team_id)
            .bind(name)
            .fetch_optional(db.pool())
            .await?;
    let tag_id = match found {
        Some((id,)) => id,
        None => {
            sqlx::query_scalar(
                "INSERT INTO tags (uuid, name, team_id, created_at, updated_at) VALUES ($1, $2, $3, $4, $4) RETURNING id",
            )
            .bind(new_public_id())
            .bind(name)
            .bind(team_id)
            .bind(now)
            .fetch_one(db.pool())
            .await?
        }
    };
    sqlx::query(
        "INSERT INTO taggables (tag_id, taggable_id, taggable_type) SELECT $1, $2, $3 \
         WHERE NOT EXISTS (SELECT 1 FROM taggables WHERE tag_id = $1 AND taggable_id = $2 AND taggable_type = $3)",
    )
    .bind(tag_id)
    .bind(service_id)
    .bind(MORPH)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// `Tag::where('team_id', $teamId)->where('uuid', $uuid)->first()->id`.
pub async fn tag_id(db: &Db, team_id: TeamId, uuid: &str) -> DbResult<Option<i64>> {
    let row: Option<(i64,)> =
        sqlx::query_as("SELECT id FROM tags WHERE team_id = $1 AND uuid = $2 LIMIT 1")
            .bind(team_id)
            .bind(uuid)
            .fetch_optional(db.pool())
            .await?;
    Ok(row.map(|r| r.0))
}

/// `$service->tags()->whereKey($id)->exists()`.
pub async fn has_tag(db: &Db, service_id: i64, tag_id: i64) -> DbResult<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM taggables WHERE taggable_type = $1 AND taggable_id = $2 AND tag_id = $3)",
    )
    .bind(MORPH)
    .bind(service_id)
    .bind(tag_id)
    .fetch_one(db.pool())
    .await?)
}

/// `$service->tags()->detach($id)`.
pub async fn detach_tag(db: &Db, service_id: i64, tag_id: i64) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(
        "DELETE FROM taggables WHERE taggable_type = $1 AND taggable_id = $2 AND tag_id = $3",
    )
    .bind(MORPH)
    .bind(service_id)
    .bind(tag_id)
    .execute(db.pool())
    .await?;
    Ok(())
}

// --- activities -------------------------------------------------------------------------------

/// `Activity::where('properties->type_uuid', $uuid)` in progress or queued: `status` cancelled.
pub async fn cancel_running_activities(
    db: &Db,
    type_uuid: &str,
    now: NaiveDateTime,
) -> DbResult<u64> {
    db.ensure_writable()?;
    let done = sqlx::query(
        "UPDATE activity_log SET properties = properties || '{\"status\":\"cancelled\"}'::jsonb, updated_at = $2 \
         WHERE properties->>'type_uuid' = $1 AND properties->>'status' IN ('in_progress', 'queued')",
    )
    .bind(type_uuid)
    .bind(now)
    .execute(db.pool())
    .await?;
    Ok(done.rows_affected())
}

/// `ResourceStartActivity::latestRunning($uuid)`: the latest activity of the uuid is queued (fresh
/// for 600 seconds) or in progress (fresh for 900 seconds).
pub async fn has_running_start(db: &Db, type_uuid: &str) -> DbResult<bool> {
    Ok(sqlx::query_scalar(
        "SELECT COALESCE((SELECT (a.properties->>'status' = 'queued' AND a.updated_at > (now() AT TIME ZONE 'UTC') - interval '600 seconds') \
         OR (a.properties->>'status' = 'in_progress' AND a.updated_at > (now() AT TIME ZONE 'UTC') - interval '900 seconds') \
         FROM activity_log a WHERE a.properties->>'type_uuid' = $1 ORDER BY a.created_at DESC, a.id DESC LIMIT 1), false)",
    )
    .bind(type_uuid)
    .fetch_one(db.pool())
    .await?)
}

/// `activity()->withProperties($properties)->event('inline')->log('[]')` without a subject.
pub async fn activity_insert(db: &Db, properties: &Value, now: NaiveDateTime) -> DbResult<i64> {
    db.ensure_writable()?;
    Ok(sqlx::query_scalar(
        "INSERT INTO activity_log (log_name, description, properties, event, created_at, updated_at) \
         VALUES ('default', '[]', $1::jsonb, 'inline', $2, $2) RETURNING id",
    )
    .bind(properties.to_string())
    .bind(now)
    .fetch_one(db.pool())
    .await?)
}

/// The latest activity of a service: `(status, log entries)`.
pub async fn latest_activity(db: &Db, type_uuid: &str) -> DbResult<Option<(Value, String)>> {
    let row: Option<(Value, String)> = sqlx::query_as(
        "SELECT properties, description FROM activity_log WHERE properties->>'type_uuid' = $1 \
         ORDER BY created_at DESC, id DESC LIMIT 1",
    )
    .bind(type_uuid)
    .fetch_optional(db.pool())
    .await?;
    Ok(row)
}

// --- domains ----------------------------------------------------------------------------------

/// `ServiceApplication::ownedByCurrentTeamAPI($teamId)` with `fqdn`: `(uuid, service name, fqdn)`.
pub async fn application_fqdns_of_team(
    db: &Db,
    team_id: TeamId,
) -> DbResult<Vec<(String, String, Option<String>)>> {
    Ok(sqlx::query_as(
        "SELECT a.uuid, s.name, a.fqdn FROM service_applications a JOIN services s ON s.id = a.service_id \
         JOIN environments e ON e.id = s.environment_id JOIN projects p ON p.id = e.project_id \
         WHERE p.team_id = $1 AND a.deleted_at IS NULL AND s.deleted_at IS NULL ORDER BY a.name",
    )
    .bind(team_id)
    .fetch_all(db.pool())
    .await?)
}

// --- clone ------------------------------------------------------------------------------------

/// `$source->replicate(['id', 'created_at', 'updated_at'])->fill($overrides)->save()`.
#[allow(clippy::too_many_arguments)]
pub async fn replicate(
    db: &Db,
    source_id: i64,
    uuid: &str,
    name: &str,
    destination_id: i64,
    destination_type: &str,
    server_id: i64,
    now: NaiveDateTime,
) -> DbResult<i64> {
    db.ensure_writable()?;
    Ok(sqlx::query_scalar(
        "INSERT INTO services (uuid, name, environment_id, server_id, description, docker_compose_raw, docker_compose, \
         destination_type, destination_id, connect_to_docker_network, config_hash, service_type, is_container_label_escape_enabled, \
         compose_parsing_version, redact_all_env_values_in_logs, is_maintenance_enabled, maintenance_page, include_coolify_default_variables, \
         created_at, updated_at) \
         SELECT $2, $3, environment_id, $4, description, docker_compose_raw, docker_compose, $5, $6, connect_to_docker_network, config_hash, \
         service_type, is_container_label_escape_enabled, '5', redact_all_env_values_in_logs, is_maintenance_enabled, maintenance_page, \
         include_coolify_default_variables, $7, $7 FROM services WHERE id = $1 RETURNING id",
    )
    .bind(source_id)
    .bind(uuid)
    .bind(name)
    .bind(server_id)
    .bind(destination_type)
    .bind(destination_id)
    .bind(now)
    .fetch_one(db.pool())
    .await?)
}

/// The tags, scheduled tasks and variables `clone_service()` copies.
pub async fn replicate_children(
    db: &Db,
    source_id: i64,
    new_id: i64,
    team_id: TeamId,
    now: NaiveDateTime,
) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(
        "INSERT INTO taggables (tag_id, taggable_id, taggable_type) \
         SELECT tag_id, $2, taggable_type FROM taggables WHERE taggable_type = $3 AND taggable_id = $1",
    )
    .bind(source_id)
    .bind(new_id)
    .bind(MORPH)
    .execute(db.pool())
    .await?;
    let tasks: Vec<(i64,)> =
        sqlx::query_as("SELECT id FROM scheduled_tasks WHERE service_id = $1 ORDER BY name ASC")
            .bind(source_id)
            .fetch_all(db.pool())
            .await?;
    for (task,) in tasks {
        sqlx::query(
            "INSERT INTO scheduled_tasks (uuid, enabled, name, command, frequency, container, timeout, team_id, service_id, created_at, updated_at) \
             SELECT $2, enabled, name, command, frequency, container, timeout, $3, $4, $5, $5 FROM scheduled_tasks WHERE id = $1",
        )
        .bind(task)
        .bind(new_public_id())
        .bind(team_id)
        .bind(new_id)
        .bind(now)
        .execute(db.pool())
        .await?;
    }
    sqlx::query(
        "INSERT INTO environment_variables (key, value, is_preview, is_required, is_multiline, is_literal, is_shown_once, is_runtime, \
         is_buildtime, is_shared, comment, resourceable_type, resourceable_id, uuid, version, preserve_compose_null, uses_legacy_escaping, \"order\", created_at, updated_at) \
         SELECT key, value, is_preview, is_required, is_multiline, is_literal, is_shown_once, is_runtime, is_buildtime, is_shared, comment, \
         resourceable_type, $2, uuid, version, preserve_compose_null, uses_legacy_escaping, \"order\", $4, $4 \
         FROM environment_variables WHERE resourceable_type = $3 AND resourceable_id = $1 ORDER BY id",
    )
    .bind(source_id)
    .bind(new_id)
    .bind(MORPH)
    .bind(now)
    .execute(db.pool())
    .await?;
    Ok(())
}

// --- scheduled tasks --------------------------------------------------------------------------

/// `$service->scheduled_tasks` (`orderBy('name', 'asc')`).
pub async fn scheduled_tasks(db: &Db, service_id: i64) -> DbResult<Vec<Record>> {
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT row_to_json(t)::text FROM scheduled_tasks t WHERE t.service_id = $1 ORDER BY t.name ASC",
    )
    .bind(service_id)
    .fetch_all(db.pool())
    .await?;
    rows.iter()
        .map(|r| decode(&r.0, "scheduled_tasks"))
        .collect()
}

/// `$service->scheduled_tasks()->where('uuid', $uuid)->first()`.
pub async fn scheduled_task(db: &Db, service_id: i64, uuid: &str) -> DbResult<Option<Record>> {
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT row_to_json(t)::text FROM scheduled_tasks t WHERE t.service_id = $1 AND t.uuid = $2 ORDER BY t.name ASC LIMIT 1",
    )
    .bind(service_id)
    .bind(uuid)
    .fetch_optional(db.pool())
    .await?;
    row.map(|r| decode(&r.0, "scheduled_tasks")).transpose()
}

/// `$task->save()` for a new task of a service.
#[allow(clippy::too_many_arguments)]
pub async fn scheduled_task_create(
    db: &Db,
    service_id: i64,
    team_id: TeamId,
    name: &str,
    command: &str,
    frequency: &str,
    container: Option<&str>,
    timeout: i64,
    enabled: bool,
    now: NaiveDateTime,
) -> DbResult<Record> {
    db.ensure_writable()?;
    let row: (String,) = sqlx::query_as(
        "INSERT INTO scheduled_tasks AS t (uuid, enabled, name, command, frequency, container, timeout, team_id, service_id, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $10) RETURNING row_to_json(t)::text",
    )
    .bind(new_public_id())
    .bind(enabled)
    .bind(name)
    .bind(command)
    .bind(frequency)
    .bind(container)
    .bind(i32::try_from(timeout).unwrap_or(300))
    .bind(team_id)
    .bind(service_id)
    .bind(now)
    .fetch_one(db.pool())
    .await?;
    decode(&row.0, "scheduled_tasks")
}

/// `$task->update($request->only($allowedFields))`.
pub async fn scheduled_task_update(
    db: &Db,
    id: i64,
    values: &[(&'static str, ColumnValue)],
    now: NaiveDateTime,
) -> DbResult<Option<Record>> {
    storage_update(db, "scheduled_tasks", id, values, now).await
}

/// `$task->delete()`.
pub async fn scheduled_task_delete(db: &Db, id: i64) -> DbResult<()> {
    storage_delete(db, "scheduled_tasks", id).await
}

/// `$task->executions()->get()` (`orderBy('created_at', 'desc')`).
pub async fn scheduled_task_executions(db: &Db, task_id: i64) -> DbResult<Vec<Record>> {
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT row_to_json(x)::text FROM scheduled_task_executions x WHERE x.scheduled_task_id = $1 ORDER BY x.created_at DESC",
    )
    .bind(task_id)
    .fetch_all(db.pool())
    .await?;
    rows.iter()
        .map(|r| decode(&r.0, "scheduled_task_executions"))
        .collect()
}

/// `ScheduledTaskExecution::forceCreate([...])`.
pub async fn scheduled_task_execution_start(
    db: &Db,
    task_id: i64,
    now: NaiveDateTime,
) -> DbResult<i64> {
    db.ensure_writable()?;
    Ok(sqlx::query_scalar(
        "INSERT INTO scheduled_task_executions (uuid, scheduled_task_id, started_at, retry_count, created_at, updated_at) \
         VALUES ($1, $2, $3, 0, $3, $3) RETURNING id",
    )
    .bind(new_public_id())
    .bind(task_id)
    .bind(now)
    .fetch_one(db.pool())
    .await?)
}

/// The final `update()`s of a task execution: `status`, `message`, `finished_at`, `duration`.
pub async fn scheduled_task_execution_finish(
    db: &Db,
    id: i64,
    status: &str,
    message: &str,
    duration: f64,
    now: NaiveDateTime,
) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(
        "UPDATE scheduled_task_executions SET status = $2, message = $3, finished_at = $4, \
         duration = round($5::numeric, 2), updated_at = $4 WHERE id = $1",
    )
    .bind(id)
    .bind(status)
    .bind(message)
    .bind(now)
    .bind(duration.to_string())
    .execute(db.pool())
    .await?;
    Ok(())
}

// --- move -------------------------------------------------------------------------------------

/// `Environment::ownedByCurrentTeamAPI($teamId)->whereUuid($uuid)->first()`: `(id, project uuid)`.
pub async fn environment_of_team(
    db: &Db,
    team_id: TeamId,
    uuid: &str,
) -> DbResult<Option<(i64, String)>> {
    Ok(sqlx::query_as(
        "SELECT e.id, p.uuid FROM environments e JOIN projects p ON p.id = e.project_id WHERE p.team_id = $1 AND e.uuid = $2 LIMIT 1",
    )
    .bind(team_id)
    .bind(uuid)
    .fetch_optional(db.pool())
    .await?)
}

/// `$project->environments()->where('name', $name)->first()`, then by uuid.
pub async fn project_environment(
    db: &Db,
    project_id: i64,
    name: Option<&str>,
    uuid: Option<&str>,
) -> DbResult<Option<i64>> {
    let by_name: Option<(i64,)> =
        sqlx::query_as("SELECT id FROM environments WHERE project_id = $1 AND name = $2 LIMIT 1")
            .bind(project_id)
            .bind(name)
            .fetch_optional(db.pool())
            .await?;
    if let Some((id,)) = by_name {
        return Ok(Some(id));
    }
    let by_uuid: Option<(i64,)> =
        sqlx::query_as("SELECT id FROM environments WHERE project_id = $1 AND uuid = $2 LIMIT 1")
            .bind(project_id)
            .bind(uuid)
            .fetch_optional(db.pool())
            .await?;
    Ok(by_uuid.map(|r| r.0))
}

/// A team's destination by uuid: `(id, morph, server_id)` (`StandaloneDocker`, then `SwarmDocker`).
pub async fn team_destination_by_uuid(
    db: &Db,
    team_id: TeamId,
    uuid: &str,
) -> DbResult<Option<(i64, &'static str, i64)>> {
    for (table, morph) in [
        ("standalone_dockers", r"App\Models\StandaloneDocker"),
        ("swarm_dockers", r"App\Models\SwarmDocker"),
    ] {
        let row: Option<(i64, i64)> = sqlx::query_as(&format!(
            "SELECT d.id, d.server_id FROM {table} d JOIN servers s ON s.id = d.server_id WHERE s.team_id = $1 AND d.uuid = $2 LIMIT 1"
        ))
        .bind(team_id)
        .bind(uuid)
        .fetch_optional(db.pool())
        .await?;
        if let Some((id, server_id)) = row {
            return Ok(Some((id, morph, server_id)));
        }
    }
    Ok(None)
}

/// Whether another public service database of the server uses the port.
pub async fn public_port_used(
    db: &Db,
    server_id: i64,
    port: i64,
    except_id: i64,
) -> DbResult<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM service_databases d JOIN services s ON s.id = d.service_id \
         WHERE s.server_id = $1 AND d.public_port = $2 AND d.is_public AND d.id <> $3 AND d.deleted_at IS NULL)",
    )
    .bind(server_id)
    .bind(i32::try_from(port).unwrap_or(0))
    .bind(except_id)
    .fetch_one(db.pool())
    .await?)
}
