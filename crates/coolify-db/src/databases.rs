//! Standalone databases (`standalone_postgresqls`, ...). SQLite is not ported;
//! it is listed so lookups by UUID and team listings can tell that a resource
//! exists and answer "not ported" for it.

use chrono::NaiveDateTime;
use coolify_domain::{TeamId, new_public_id};

use crate::records::{
    ENVIRONMENT, PROJECT, Record, STANDALONE_DOCKER, STANDALONE_POSTGRESQL, TEAM, parse_row,
};
use crate::servers::{ColumnValue, bind_all, set_clause};
use crate::{Db, DbError, DbResult};

pub const STANDALONE_DOCKER_MORPH: &str = r"App\Models\StandaloneDocker";

/// `STANDALONE_DATABASE_MODELS`, in upstream's order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Postgresql,
    Redis,
    Mongodb,
    Mysql,
    Mariadb,
    Keydb,
    Dragonfly,
    Clickhouse,
    Sqlite,
}

impl Kind {
    pub const ALL: [Kind; 9] = [
        Kind::Postgresql,
        Kind::Redis,
        Kind::Mongodb,
        Kind::Mysql,
        Kind::Mariadb,
        Kind::Keydb,
        Kind::Dragonfly,
        Kind::Clickhouse,
        Kind::Sqlite,
    ];

    pub fn table(self) -> &'static str {
        match self {
            Kind::Postgresql => "standalone_postgresqls",
            Kind::Redis => "standalone_redis",
            Kind::Mongodb => "standalone_mongodbs",
            Kind::Mysql => "standalone_mysqls",
            Kind::Mariadb => "standalone_mariadbs",
            Kind::Keydb => "standalone_keydbs",
            Kind::Dragonfly => "standalone_dragonflies",
            Kind::Clickhouse => "standalone_clickhouses",
            Kind::Sqlite => "standalone_sqlites",
        }
    }

    pub fn morph(self) -> &'static str {
        match self {
            Kind::Postgresql => r"App\Models\StandalonePostgresql",
            Kind::Redis => r"App\Models\StandaloneRedis",
            Kind::Mongodb => r"App\Models\StandaloneMongodb",
            Kind::Mysql => r"App\Models\StandaloneMysql",
            Kind::Mariadb => r"App\Models\StandaloneMariadb",
            Kind::Keydb => r"App\Models\StandaloneKeydb",
            Kind::Dragonfly => r"App\Models\StandaloneDragonfly",
            Kind::Clickhouse => r"App\Models\StandaloneClickhouse",
            Kind::Sqlite => r"App\Models\StandaloneSqlite",
        }
    }

    /// `$database->type()`.
    pub fn type_name(self) -> &'static str {
        match self {
            Kind::Postgresql => "standalone-postgresql",
            Kind::Redis => "standalone-redis",
            Kind::Mongodb => "standalone-mongodb",
            Kind::Mysql => "standalone-mysql",
            Kind::Mariadb => "standalone-mariadb",
            Kind::Keydb => "standalone-keydb",
            Kind::Dragonfly => "standalone-dragonfly",
            Kind::Clickhouse => "standalone-clickhouse",
            Kind::Sqlite => "standalone-sqlite",
        }
    }

    pub fn ported(self) -> bool {
        !matches!(self, Kind::Sqlite)
    }

    /// The ported kinds, in upstream's order.
    pub fn ported_kinds() -> impl Iterator<Item = Kind> {
        Kind::ALL.into_iter().filter(|k| k.ported())
    }
}

/// A database with the relations its API presentation loads.
#[derive(Debug, Clone)]
pub struct DatabaseRow {
    pub kind: Kind,
    pub id: i64,
    pub uuid: String,
    pub team_id: TeamId,
    /// Raw attributes (`row_to_json`), timestamps not yet presented.
    pub raw: Record,
    pub environment: Record,
    pub project: Record,
    pub team: Record,
    /// `standalone_dockers` row when the destination is a standalone Docker.
    pub destination: Option<Record>,
}

impl DatabaseRow {
    pub fn str(&self, key: &str) -> Option<&str> {
        self.raw.get(key).and_then(|v| v.as_str())
    }
    pub fn int(&self, key: &str) -> Option<i64> {
        self.raw.get(key).and_then(|v| v.as_i64())
    }
    pub fn bool(&self, key: &str) -> bool {
        self.raw.get(key).and_then(|v| v.as_bool()).unwrap_or(false)
    }
    pub fn server_id(&self) -> Option<i64> {
        self.destination
            .as_ref()
            .and_then(|d| d.get("server_id"))
            .and_then(|v| v.as_i64())
    }
}

fn decode(text: &str, what: &str) -> DbResult<Record> {
    parse_row(text).map_err(|e| DbError::Schema(format!("decoding {what} row: {e}")))
}

type Joined = (String, String, String, String, Option<String>);

fn select(kind: Kind, filter: &str) -> String {
    format!(
        "SELECT row_to_json(d)::text, row_to_json(e)::text, row_to_json(p)::text, row_to_json(t)::text, \
         (SELECT row_to_json(sd)::text FROM standalone_dockers sd WHERE d.destination_type = '{STANDALONE_DOCKER_MORPH}' \
          AND sd.id = d.destination_id) \
         FROM {table} d JOIN environments e ON e.id = d.environment_id JOIN projects p ON p.id = e.project_id \
         JOIN teams t ON t.id = p.team_id WHERE d.deleted_at IS NULL AND {filter}",
        table = kind.table()
    )
}

fn to_row(kind: Kind, j: &Joined) -> DbResult<DatabaseRow> {
    let raw = decode(&j.0, kind.table())?;
    let environment = ENVIRONMENT.present(decode(&j.1, "environment")?);
    let project = PROJECT.present(decode(&j.2, "project")?);
    let team = TEAM.present(decode(&j.3, "team")?);
    let destination =
        j.4.as_deref()
            .map(|d| decode(d, "destination").map(|r| STANDALONE_DOCKER.present(r)))
            .transpose()?;
    Ok(DatabaseRow {
        kind,
        id: raw.get("id").and_then(|v| v.as_i64()).unwrap_or_default(),
        uuid: raw
            .get("uuid")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_owned(),
        team_id: project
            .get("team_id")
            .and_then(|v| v.as_i64())
            .unwrap_or_default(),
        raw,
        environment,
        project,
        team,
        destination,
    })
}

/// `queryDatabaseByUuidWithinTeam`: the first kind with that UUID, if it belongs to the team.
pub async fn find_in_team(db: &Db, team_id: TeamId, uuid: &str) -> DbResult<Option<DatabaseRow>> {
    for kind in Kind::ALL {
        let r: Option<Joined> = sqlx::query_as(&select(kind, "d.uuid = $1 ORDER BY d.id LIMIT 1"))
            .bind(uuid)
            .fetch_optional(db.pool())
            .await?;
        if let Some(j) = r {
            let row = to_row(kind, &j)?;
            // Upstream checks the first match's team and stops either way.
            return Ok((row.team_id == team_id).then_some(row));
        }
    }
    Ok(None)
}

/// `Project::databases()` for each of the team's projects (table order).
pub async fn list_for_team(db: &Db, team_id: TeamId) -> DbResult<Vec<DatabaseRow>> {
    let projects: Vec<i64> = sqlx::query_scalar("SELECT id FROM projects WHERE team_id = $1")
        .bind(team_id)
        .fetch_all(db.pool())
        .await?;
    let mut out = Vec::new();
    for project in projects {
        for kind in Kind::ALL {
            let rows: Vec<Joined> = sqlx::query_as(&select(kind, "e.project_id = $1"))
                .bind(project)
                .fetch_all(db.pool())
                .await?;
            for j in &rows {
                out.push(to_row(kind, j)?);
            }
        }
    }
    Ok(out)
}

/// `Environment::databases()`.
pub async fn list_for_environment(db: &Db, environment_id: i64) -> DbResult<Vec<DatabaseRow>> {
    let mut out = Vec::new();
    for kind in Kind::ALL {
        let rows: Vec<Joined> = sqlx::query_as(&select(kind, "d.environment_id = $1"))
            .bind(environment_id)
            .fetch_all(db.pool())
            .await?;
        for j in &rows {
            out.push(to_row(kind, j)?);
        }
    }
    Ok(out)
}

/// `Server::databases()`: databases on the server's standalone Docker destinations.
pub async fn list_for_server(db: &Db, server_id: i64) -> DbResult<Vec<DatabaseRow>> {
    let mut out = Vec::new();
    for kind in Kind::ALL {
        let rows: Vec<Joined> = sqlx::query_as(&select(
            kind,
            &format!(
                "d.destination_type = '{STANDALONE_DOCKER_MORPH}' AND d.destination_id IN \
                 (SELECT id FROM standalone_dockers WHERE server_id = $1)"
            ),
        ))
        .bind(server_id)
        .fetch_all(db.pool())
        .await?;
        for j in &rows {
            out.push(to_row(kind, j)?);
        }
    }
    Ok(out)
}

/// `isPublicPortAlreadyUsed` (ported kinds only; other kinds are checked by the caller's 501).
pub async fn public_port_used(db: &Db, server_id: i64, port: i64) -> DbResult<bool> {
    for d in list_for_server(db, server_id).await? {
        if d.bool("is_public") && d.int("public_port") == Some(port) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// `$server->destinations()` (standalone Docker only; Swarm is not ported).
pub async fn destinations(db: &Db, server_id: i64) -> DbResult<Vec<(i64, String)>> {
    Ok(
        sqlx::query_as("SELECT id, uuid FROM standalone_dockers WHERE server_id = $1 ORDER BY id")
            .bind(server_id)
            .fetch_all(db.pool())
            .await?,
    )
}

/// `(uuid, name, network)` of the server's standalone Docker destinations.
pub async fn destination_records(
    db: &Db,
    server_id: i64,
) -> DbResult<Vec<(String, String, String)>> {
    Ok(sqlx::query_as(
        "SELECT uuid, name, network FROM standalone_dockers WHERE server_id = $1 ORDER BY id",
    )
    .bind(server_id)
    .fetch_all(db.pool())
    .await?)
}

/// `find_resource_destination_for_current_team`: `(destination id, server id)`.
pub async fn team_destination(
    db: &Db,
    team_id: TeamId,
    uuid: &str,
) -> DbResult<Option<(i64, i64)>> {
    Ok(sqlx::query_as(
        "SELECT sd.id, s.id FROM standalone_dockers sd JOIN servers s ON s.id = sd.server_id \
         WHERE sd.uuid = $1 AND s.team_id = $2 AND s.deleted_at IS NULL LIMIT 1",
    )
    .bind(uuid)
    .bind(team_id)
    .fetch_optional(db.pool())
    .await?)
}

/// Services of an environment (not ported yet).
pub async fn count_other_resources(db: &Db, environment_id: i64) -> DbResult<i64> {
    Ok(sqlx::query_scalar(
        "SELECT count(*) FROM services WHERE environment_id = $1 AND deleted_at IS NULL",
    )
    .bind(environment_id)
    .fetch_one(db.pool())
    .await?)
}

/// Whether the server has Swarm destinations (not ported).
pub async fn has_swarm_destinations(db: &Db, server_id: i64) -> DbResult<bool> {
    Ok(
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM swarm_dockers WHERE server_id = $1)")
            .bind(server_id)
            .fetch_one(db.pool())
            .await?,
    )
}

/// Fillable values of a new database, as `create_standalone_*` and `fill()` store them.
#[derive(Debug, Default)]
pub struct NewDatabase {
    pub environment_id: i64,
    pub destination_id: i64,
    /// The image when the caller chooses it (PostgreSQL in the UI); otherwise the column default.
    pub image: Option<String>,
    /// Generated or given passwords, already encrypted with `APP_KEY` (`encrypted` casts).
    pub secrets: Vec<(&'static str, String)>,
    /// Other fillable columns from the request, in request order.
    pub fields: Vec<(&'static str, ColumnValue)>,
    pub name: Option<String>,
    /// Variables the create helper adds (Redis: `REDIS_PASSWORD`, `REDIS_USERNAME`).
    pub variables: Vec<crate::environment_variables::NewVariable>,
}

impl Kind {
    /// `create_standalone_*`: the default name is `{prefix}-database-{uuid}`.
    pub fn name_prefix(self) -> &'static str {
        match self {
            Kind::Postgresql => "postgresql",
            Kind::Redis => "redis",
            Kind::Mongodb => "mongodb",
            Kind::Mysql => "mysql",
            Kind::Mariadb => "mariadb",
            Kind::Keydb => "keydb",
            Kind::Dragonfly => "dragonfly",
            Kind::Clickhouse => "clickhouse",
            Kind::Sqlite => "sqlite",
        }
    }

    /// The model's `created` hook: `(volume name, mount path)` in creation order.
    pub fn default_volumes(self, uuid: &str, image: &str) -> Vec<(String, &'static str)> {
        match self {
            Kind::Postgresql => {
                // PostgreSQL 18+ keeps its data one level up.
                let mount = if image_major(image) >= 18 {
                    "/var/lib/postgresql"
                } else {
                    "/var/lib/postgresql/data"
                };
                vec![(format!("postgres-data-{uuid}"), mount)]
            }
            Kind::Redis => vec![(format!("redis-data-{uuid}"), "/data")],
            Kind::Mongodb => vec![
                (format!("mongodb-configdb-{uuid}"), "/data/configdb"),
                (format!("mongodb-db-{uuid}"), "/data/db"),
            ],
            Kind::Mysql => vec![(format!("mysql-data-{uuid}"), "/var/lib/mysql")],
            Kind::Mariadb => vec![(format!("mariadb-data-{uuid}"), "/var/lib/mysql")],
            Kind::Keydb => vec![(format!("keydb-data-{uuid}"), "/data")],
            Kind::Dragonfly => vec![(format!("dragonfly-data-{uuid}"), "/data")],
            Kind::Clickhouse => vec![(format!("clickhouse-data-{uuid}"), "/var/lib/clickhouse")],
            Kind::Sqlite => vec![(format!("sqlite-data-{uuid}"), "/data")],
        }
    }

    /// Encrypted password columns `create_standalone_*` fills with `Str::password(64, symbols: false)`.
    pub fn generated_secrets(self) -> &'static [&'static str] {
        match self {
            Kind::Postgresql => &["postgres_password"],
            Kind::Mongodb => &["mongo_initdb_root_password"],
            Kind::Mysql => &["mysql_root_password", "mysql_password"],
            Kind::Mariadb => &["mariadb_root_password", "mariadb_password"],
            Kind::Keydb => &["keydb_password"],
            Kind::Dragonfly => &["dragonfly_password"],
            Kind::Clickhouse => &["clickhouse_admin_password"],
            Kind::Redis | Kind::Sqlite => &[],
        }
    }

    /// Columns with the `encrypted` cast.
    pub fn encrypted_columns(self) -> &'static [&'static str] {
        self.generated_secrets()
    }
}

/// `create_standalone_*` plus the model's `created` hook (data volumes) and, for
/// Redis, its variables, in one transaction.
pub async fn create(
    db: &Db,
    kind: Kind,
    n: NewDatabase,
    now: NaiveDateTime,
) -> DbResult<(i64, String)> {
    db.ensure_writable()?;
    let uuid = new_public_id();
    let name = n
        .name
        .clone()
        .unwrap_or_else(|| format!("{}-database-{uuid}", kind.name_prefix()));
    let mut cols: Vec<(&'static str, ColumnValue)> = vec![
        ("uuid", ColumnValue::Text(Some(uuid.clone()))),
        ("name", ColumnValue::Text(Some(name))),
    ];
    if let Some(image) = &n.image {
        cols.push(("image", ColumnValue::Text(Some(image.clone()))));
    }
    for (c, v) in &n.secrets {
        cols.push((c, ColumnValue::Text(Some(v.clone()))));
    }
    for (c, v) in &n.fields {
        if *c == "name" || cols.iter().any(|(k, _)| k == c) {
            cols.retain(|(k, _)| k != c);
        }
        cols.push((c, v.clone()));
    }
    let names: Vec<String> = cols.iter().map(|(c, _)| format!("\"{c}\"")).collect();
    let placeholders: Vec<String> = cols
        .iter()
        .enumerate()
        .map(|(i, (c, v))| {
            let cast = match v {
                ColumnValue::Int(_) => "::integer",
                _ if *c == "init_scripts" => "::json",
                _ => "",
            };
            format!("${}{cast}", i + 5)
        })
        .collect();
    let sql = format!(
        "INSERT INTO {} ({}, environment_id, destination_id, destination_type, created_at, updated_at) \
         VALUES ({}, $1, $2, $3, $4, $4) RETURNING id, image",
        kind.table(),
        names.join(", "),
        placeholders.join(", ")
    );
    let mut tx = db.pool().begin().await?;
    let mut q = sqlx::query_as::<_, (i64, String)>(&sql)
        .bind(n.environment_id)
        .bind(n.destination_id)
        .bind(STANDALONE_DOCKER_MORPH)
        .bind(now);
    for (_, v) in &cols {
        q = match v {
            ColumnValue::Text(t) => q.bind(t.clone()),
            ColumnValue::Int(i) => q.bind(*i),
            ColumnValue::Bool(b) => q.bind(*b),
            ColumnValue::Time(t) => q.bind(*t),
        };
    }
    let (id, image) = q.fetch_one(&mut *tx).await?;
    for (name, mount) in kind.default_volumes(&uuid, &image) {
        sqlx::query(
            "INSERT INTO local_persistent_volumes (name, mount_path, host_path, resource_id, resource_type, uuid, created_at, updated_at) \
             VALUES ($1, $2, NULL, $3, $4, $5, $6, $6)",
        )
        .bind(name)
        .bind(mount)
        .bind(id)
        .bind(kind.morph())
        .bind(new_public_id())
        .bind(now)
        .execute(&mut *tx)
        .await?;
    }
    for v in &n.variables {
        crate::environment_variables::insert(&mut tx, kind.morph(), id, v, now).await?;
    }
    tx.commit().await?;
    Ok((id, uuid))
}

/// `preg_match('/:(?:pg)?(\d+)/i', $image)`.
fn image_major(image: &str) -> i64 {
    let lower = image.to_ascii_lowercase();
    let mut rest = lower.as_str();
    while let Some(i) = rest.find(':') {
        let after = &rest[i + 1..];
        let after_pg = after.strip_prefix("pg").unwrap_or(after);
        let digits: String = after_pg
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        if !digits.is_empty() {
            return digits.parse().unwrap_or(i64::MAX);
        }
        let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
        if !digits.is_empty() {
            return digits.parse().unwrap_or(i64::MAX);
        }
        rest = after;
    }
    0
}

/// `$database->update([...])` with Eloquent's dirty check; `status` also sets
/// `last_online_at` (the model's `saving` hook). Returns whether a row changed.
pub async fn update_if_changed(
    db: &Db,
    kind: Kind,
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
        .map(|(i, (col, v))| match v {
            ColumnValue::Text(None) => format!("\"{col}\" IS NOT NULL"),
            ColumnValue::Int(_) => format!("\"{col}\" IS DISTINCT FROM ${}::integer", i + 3),
            _ => format!("\"{col}\" IS DISTINCT FROM ${}", i + 3),
        })
        .collect::<Vec<_>>()
        .join(" OR ");
    let status_touch = if values.iter().any(|(c, _)| *c == "status") {
        // Only when `status` itself changes; the other columns do not move it.
        let pos = values.iter().position(|(c, _)| *c == "status").unwrap_or(0) + 3;
        format!(
            ", last_online_at = CASE WHEN status IS DISTINCT FROM ${pos} THEN $1 ELSE last_online_at END"
        )
    } else {
        String::new()
    };
    let sql = format!(
        "UPDATE {} SET {}{status_touch}, updated_at = $1 WHERE id = $2 AND ({changed})",
        kind.table(),
        set_clause(values, 3)
    );
    let done = bind_all(sqlx::query(&sql).bind(now).bind(id), values)
        .execute(db.pool())
        .await?;
    Ok(done.rows_affected() > 0)
}

/// `$database->update(['last_online_at' => now()])`.
pub async fn touch_online(db: &Db, kind: Kind, id: i64, now: NaiveDateTime) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(&format!(
        "UPDATE {} SET last_online_at = $1, updated_at = $1 WHERE id = $2",
        kind.table()
    ))
    .bind(now)
    .bind(id)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// `$database->delete()` (SoftDeletes).
pub async fn soft_delete(db: &Db, kind: Kind, id: i64, now: NaiveDateTime) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(&format!(
        "UPDATE {} SET deleted_at = $1, updated_at = $1 WHERE id = $2",
        kind.table()
    ))
    .bind(now)
    .bind(id)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// `local_persistent_volumes` of a resource: `(name, mount_path, host_path)` in table order.
pub async fn persistent_volumes(
    db: &Db,
    kind: Kind,
    id: i64,
) -> DbResult<Vec<(String, String, Option<String>)>> {
    Ok(sqlx::query_as(
        "SELECT name, mount_path, host_path FROM local_persistent_volumes WHERE resource_type = $1 AND resource_id = $2",
    )
    .bind(kind.morph())
    .bind(id)
    .fetch_all(db.pool())
    .await?)
}

/// What the start command needs that coolify-rs cannot write yet: file
/// storages, SSL certificates, scheduled backups.
pub async fn unported_parts(db: &Db, kind: Kind, id: i64) -> DbResult<Vec<&'static str>> {
    let mut parts = Vec::new();
    for (table, col_type, col_id, label) in [
        (
            "local_file_volumes",
            "resource_type",
            "resource_id",
            "file storages",
        ),
        (
            "ssl_certificates",
            "resource_type",
            "resource_id",
            "SSL certificates",
        ),
        (
            "scheduled_database_backups",
            "database_type",
            "database_id",
            "scheduled backups",
        ),
    ] {
        let found: bool = sqlx::query_scalar(&format!(
            "SELECT EXISTS (SELECT 1 FROM {table} WHERE {col_type} = $1 AND {col_id} = $2)"
        ))
        .bind(kind.morph())
        .bind(id)
        .fetch_one(db.pool())
        .await?;
        if found {
            parts.push(label);
        }
    }
    Ok(parts)
}

/// `DeleteResourceJob::deleteLocalResource` for a database: the model's
/// `forceDeleting` hook (volumes, backups, variables, tags), then the row.
pub async fn force_delete(db: &Db, kind: Kind, id: i64, delete_volumes: bool) -> DbResult<()> {
    db.ensure_writable()?;
    let mut tx = db.pool().begin().await?;
    if delete_volumes {
        sqlx::query(
            "DELETE FROM local_persistent_volumes WHERE resource_type = $1 AND resource_id = $2",
        )
        .bind(kind.morph())
        .bind(id)
        .execute(&mut *tx)
        .await?;
    }
    for (table, col_type, col_id) in [
        ("local_persistent_volumes", "resource_type", "resource_id"),
        ("scheduled_database_backups", "database_type", "database_id"),
        (
            "environment_variables",
            "resourceable_type",
            "resourceable_id",
        ),
        ("taggables", "taggable_type", "taggable_id"),
    ] {
        sqlx::query(&format!(
            "DELETE FROM {table} WHERE {col_type} = $1 AND {col_id} = $2"
        ))
        .bind(kind.morph())
        .bind(id)
        .execute(&mut *tx)
        .await?;
    }
    sqlx::query(&format!("DELETE FROM {} WHERE id = $1", kind.table()))
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// A start activity (`activity_log` row) as spatie/laravel-activitylog stores it.
pub async fn insert_activity(
    db: &Db,
    kind: Kind,
    id: i64,
    properties: &serde_json::Value,
    now: NaiveDateTime,
) -> DbResult<i64> {
    db.ensure_writable()?;
    Ok(sqlx::query_scalar(
        "INSERT INTO activity_log (log_name, description, subject_type, subject_id, properties, event, created_at, updated_at) \
         VALUES ('default', '[]', $1, $2, $3::jsonb, 'inline', $4, $4) RETURNING id",
    )
    .bind(kind.morph())
    .bind(id)
    .bind(properties.to_string())
    .bind(now)
    .fetch_one(db.pool())
    .await?)
}

/// Merges `properties` and optionally replaces `description` of an activity.
pub async fn update_activity(
    db: &Db,
    activity_id: i64,
    merge: &serde_json::Value,
    description: Option<&str>,
    now: NaiveDateTime,
) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(
        "UPDATE activity_log SET properties = COALESCE(properties, '{}'::jsonb) || $2::jsonb, \
         description = COALESCE($3, description), updated_at = $4 WHERE id = $1",
    )
    .bind(activity_id)
    .bind(merge.to_string())
    .bind(description)
    .bind(now)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// The latest start activity of a database: `(id, properties, description)`.
pub async fn latest_start_activity(
    db: &Db,
    uuid: &str,
) -> DbResult<Option<(i64, serde_json::Value, String)>> {
    let r: Option<(i64, String, String)> = sqlx::query_as(
        "SELECT id, properties::text, description FROM activity_log \
         WHERE properties->>'type_uuid' = $1 AND properties->>'operation' = 'database-start' ORDER BY id DESC LIMIT 1",
    )
    .bind(uuid)
    .fetch_optional(db.pool())
    .await?;
    Ok(r.map(|(id, p, d)| (id, serde_json::from_str(&p).unwrap_or_default(), d)))
}

/// An activity's `(properties, description)`.
pub async fn activity_by_id(db: &Db, id: i64) -> DbResult<Option<(serde_json::Value, String)>> {
    let r: Option<(String, String)> = sqlx::query_as(
        "SELECT COALESCE(properties::text, '{}'), description FROM activity_log WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(db.pool())
    .await?;
    Ok(r.map(|(p, d)| (serde_json::from_str(&p).unwrap_or_default(), d)))
}

/// Servers with at least one ported database on a standalone Docker destination.
pub async fn servers_with_databases(db: &Db) -> DbResult<Vec<i64>> {
    let parts: Vec<String> = Kind::ported_kinds()
        .map(|k| {
            format!(
                "SELECT sd.server_id FROM {} d JOIN standalone_dockers sd ON sd.id = d.destination_id \
                 WHERE d.destination_type = '{STANDALONE_DOCKER_MORPH}' AND d.deleted_at IS NULL",
                k.table()
            )
        })
        .collect();
    Ok(sqlx::query_scalar(&format!(
        "SELECT DISTINCT server_id FROM ({}) s",
        parts.join(" UNION ")
    ))
    .fetch_all(db.pool())
    .await?)
}

/// Presents raw attributes with the model spec (timestamps, hidden columns).
pub fn present(kind: Kind, raw: Record) -> Record {
    use crate::database_specs as specs;
    let spec = match kind {
        Kind::Postgresql => &STANDALONE_POSTGRESQL,
        Kind::Redis => &specs::STANDALONE_REDIS,
        Kind::Mongodb => &specs::STANDALONE_MONGODB,
        Kind::Mysql => &specs::STANDALONE_MYSQL,
        Kind::Mariadb => &specs::STANDALONE_MARIADB,
        Kind::Keydb => &specs::STANDALONE_KEYDB,
        Kind::Dragonfly => &specs::STANDALONE_DRAGONFLY,
        Kind::Clickhouse => &specs::STANDALONE_CLICKHOUSE,
        Kind::Sqlite => return raw,
    };
    spec.present(raw)
}

#[cfg(test)]
mod tests {
    use super::image_major as major;

    #[test]
    fn postgres_major_version() {
        assert_eq!(major("postgres:16-alpine"), 16);
        assert_eq!(major("postgres:18"), 18);
        assert_eq!(major("ghcr.io/x/postgres:pg17"), 17);
        assert_eq!(major("postgres"), 0);
        assert_eq!(major("registry:5000/postgres:18.1"), 5000);
    }
}
