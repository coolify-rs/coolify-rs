//! `servers` and `server_settings` (read side). Rows are returned as model
//! records; the API layer applies hidden attributes, accessors and casts that
//! depend on the token (`makeVisible`).

use coolify_domain::TeamId;

use crate::records::{Record, SERVER, SERVER_SETTING, parse_row};
use crate::{Db, DbError, DbResult};

#[derive(Debug, Clone)]
pub struct ServerRow {
    pub id: i64,
    pub uuid: String,
    pub team_id: TeamId,
    pub record: Record,
    /// `$server->settings` (hasOne); `None` when the row is missing.
    pub settings: Option<Record>,
}

fn decode(text: &str, what: &str) -> DbResult<Record> {
    parse_row(text).map_err(|e| DbError::Schema(format!("decoding {what} row: {e}")))
}

fn row(text: &str, settings: Option<&str>) -> DbResult<ServerRow> {
    let record = SERVER.present(decode(text, "server")?);
    Ok(ServerRow {
        id: record
            .get("id")
            .and_then(|v| v.as_i64())
            .unwrap_or_default(),
        uuid: record
            .get("uuid")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        team_id: record
            .get("team_id")
            .and_then(|v| v.as_i64())
            .unwrap_or_default(),
        settings: settings
            .map(|s| decode(s, "server setting").map(|r| SERVER_SETTING.present(r)))
            .transpose()?,
        record,
    })
}

/// `$server->settings`: the first matching row (no ORDER BY, like Eloquent's hasOne).
const SETTINGS: &str =
    "(SELECT row_to_json(ss)::text FROM server_settings ss WHERE ss.server_id = s.id LIMIT 1)";

/// `Server::with(['settings'])->get()` over all teams (`ServerManagerJob`).
pub async fn all(db: &Db) -> DbResult<Vec<ServerRow>> {
    let rows: Vec<(String, Option<String>)> = sqlx::query_as(&format!(
        "SELECT row_to_json(s)::text, {SETTINGS} FROM servers s WHERE s.deleted_at IS NULL"
    ))
    .fetch_all(db.pool())
    .await?;
    rows.iter().map(|(s, st)| row(s, st.as_deref())).collect()
}

/// `ServerSetting::withoutPatchCheckResult()`: the settings subquery without `patch_check_result`,
/// the full update list of the last patch check (often tens of kilobytes), for instance-wide
/// server loops that do not read it. The columns come from the schema discovered at startup.
fn settings_without_patch_check_result(db: &Db) -> String {
    let columns: Vec<String> = db
        .schema()
        .columns
        .get("server_settings")
        .map(|cols| {
            cols.iter()
                .filter(|c| *c != "patch_check_result")
                .map(|c| format!("ss.\"{}\"", c.replace('"', "\"\"")))
                .collect()
        })
        .unwrap_or_default();
    if columns.is_empty() {
        return SETTINGS.to_owned();
    }
    format!(
        "(SELECT row_to_json(x)::text FROM (SELECT {} FROM server_settings ss WHERE ss.server_id = s.id LIMIT 1) x)",
        columns.join(", ")
    )
}

/// `Server::with(['settings' => withoutPatchCheckResult()])->get()` over all teams
/// (`ScheduledJobManager::getServersForCleanupQuery()`).
pub async fn all_without_patch_check_result(db: &Db) -> DbResult<Vec<ServerRow>> {
    let rows: Vec<(String, Option<String>)> = sqlx::query_as(&format!(
        "SELECT row_to_json(s)::text, {} FROM servers s WHERE s.deleted_at IS NULL",
        settings_without_patch_check_result(db)
    ))
    .fetch_all(db.pool())
    .await?;
    rows.iter().map(|(s, st)| row(s, st.as_deref())).collect()
}

/// `ServerManagerJob::getServerIds()`: every server id, in order.
pub async fn ids(db: &Db) -> DbResult<Vec<i64>> {
    Ok(
        sqlx::query_scalar("SELECT id FROM servers WHERE deleted_at IS NULL ORDER BY id")
            .fetch_all(db.pool())
            .await?,
    )
}

/// `Server::with(['settings' => withoutPatchCheckResult()])->whereKey($ids)->orderBy('id')->get()`
/// (`ServerManagerJob`, one batch).
pub async fn batch_without_patch_check_result(db: &Db, ids: &[i64]) -> DbResult<Vec<ServerRow>> {
    let rows: Vec<(String, Option<String>)> = sqlx::query_as(&format!(
        "SELECT row_to_json(s)::text, {} FROM servers s WHERE s.id = ANY($1) AND s.deleted_at IS NULL ORDER BY s.id",
        settings_without_patch_check_result(db)
    ))
    .bind(ids)
    .fetch_all(db.pool())
    .await?;
    rows.iter().map(|(s, st)| row(s, st.as_deref())).collect()
}

/// `Server::where('uuid', $uuid)->first()` without a team scope (`SentinelController`).
pub async fn find_any_team(db: &Db, uuid: &str) -> DbResult<Option<ServerRow>> {
    let r: Option<(String, Option<String>)> = sqlx::query_as(&format!(
        "SELECT row_to_json(s)::text, {SETTINGS} FROM servers s WHERE s.uuid = $1 AND s.deleted_at IS NULL LIMIT 1"
    ))
    .bind(uuid)
    .fetch_optional(db.pool())
    .await?;
    r.as_ref().map(|(s, st)| row(s, st.as_deref())).transpose()
}

/// `Server::whereTeamId($teamId)->get()` (SoftDeletes scope, table order).
pub async fn list(db: &Db, team_id: TeamId) -> DbResult<Vec<ServerRow>> {
    let rows: Vec<(String, Option<String>)> = sqlx::query_as(&format!(
        "SELECT row_to_json(s)::text, {SETTINGS} FROM servers s WHERE s.team_id = $1 AND s.deleted_at IS NULL"
    ))
    .bind(team_id)
    .fetch_all(db.pool())
    .await?;
    rows.iter().map(|(s, st)| row(s, st.as_deref())).collect()
}

/// `Server::ownedByCurrentTeam()`: the team's servers ordered by name.
pub async fn list_by_name(db: &Db, team_id: TeamId) -> DbResult<Vec<ServerRow>> {
    let rows: Vec<(String, Option<String>)> = sqlx::query_as(&format!(
        "SELECT row_to_json(s)::text, {SETTINGS} FROM servers s WHERE s.team_id = $1 AND s.deleted_at IS NULL ORDER BY s.name"
    ))
    .bind(team_id)
    .fetch_all(db.pool())
    .await?;
    rows.iter().map(|(s, st)| row(s, st.as_deref())).collect()
}

/// `$destination->server` (belongsTo; soft-deleted servers excluded).
pub async fn find_by_id(db: &Db, id: i64) -> DbResult<Option<ServerRow>> {
    let r: Option<(String, Option<String>)> = sqlx::query_as(&format!(
        "SELECT row_to_json(s)::text, {SETTINGS} FROM servers s WHERE s.id = $1 AND s.deleted_at IS NULL LIMIT 1"
    ))
    .bind(id)
    .fetch_optional(db.pool())
    .await?;
    r.map(|(s, st)| row(&s, st.as_deref())).transpose()
}

/// UUIDs of every server that is not soft-deleted (all teams).
pub async fn live_uuids(db: &Db) -> DbResult<Vec<String>> {
    Ok(
        sqlx::query_scalar("SELECT uuid FROM servers WHERE deleted_at IS NULL")
            .fetch_all(db.pool())
            .await?,
    )
}

/// `Server::whereTeamId($teamId)->whereUuid($uuid)->first()`.
pub async fn find(db: &Db, team_id: TeamId, uuid: &str) -> DbResult<Option<ServerRow>> {
    let r: Option<(String, Option<String>)> = sqlx::query_as(&format!(
        "SELECT row_to_json(s)::text, {SETTINGS} FROM servers s \
         WHERE s.team_id = $1 AND s.uuid = $2 AND s.deleted_at IS NULL LIMIT 1"
    ))
    .bind(team_id)
    .bind(uuid)
    .fetch_optional(db.pool())
    .await?;
    r.map(|(s, st)| row(&s, st.as_deref())).transpose()
}

/// `NotificationThrottle::wasSent($server, Unreachable::class)`.
pub async fn unreachable_notification_sent(db: &Db, server_id: i64) -> DbResult<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM notification_throttles WHERE notifiable_type = 'App\\Models\\Server' \
         AND notifiable_id = $1 AND notification = 'App\\Notifications\\Server\\Unreachable')",
    )
    .bind(server_id)
    .fetch_one(db.pool())
    .await?)
}

/// `Server::whereIp($ip)->first()`: any team, soft-deleted servers excluded.
pub async fn ip_in_use(db: &Db, ip: &str, except_id: Option<i64>) -> DbResult<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM servers WHERE ip = $1 AND deleted_at IS NULL AND id IS DISTINCT FROM $2)",
    )
    .bind(ip)
    .bind(except_id)
    .fetch_one(db.pool())
    .await?)
}

/// `Server::buildServers($teamId)->exists()` (optionally `whereKeyNot`).
pub async fn has_build_server(db: &Db, team_id: TeamId, except_id: Option<i64>) -> DbResult<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM servers s WHERE s.team_id = $1 AND s.deleted_at IS NULL \
           AND s.id IS DISTINCT FROM $2 \
           AND EXISTS (SELECT 1 FROM server_settings ss WHERE ss.server_id = s.id AND ss.is_reachable \
                 AND ss.is_usable AND NOT ss.is_swarm_worker AND NOT ss.force_disabled) \
           AND NOT EXISTS (SELECT 1 FROM github_runner_configs g WHERE g.server_id = s.id \
                 AND g.is_enabled AND g.is_dedicated) \
           AND EXISTS (SELECT 1 FROM server_settings ss WHERE ss.server_id = s.id \
                 AND (ss.server_role IN ('build') OR (ss.server_role IS NULL AND ss.is_build_server IN (true)))))",
    )
    .bind(team_id)
    .bind(except_id)
    .fetch_one(db.pool())
    .await?)
}

/// `instance_settings` fields `generateSentinelUrl` reads: fqdn, public IPv4, public IPv6.
pub async fn instance_urls(db: &Db) -> DbResult<(Option<String>, Option<String>, Option<String>)> {
    Ok(
        sqlx::query_as("SELECT fqdn, public_ipv4, public_ipv6 FROM instance_settings WHERE id = 0")
            .fetch_optional(db.pool())
            .await?
            .unwrap_or((None, None, None)),
    )
}

/// Everything `POST /servers` writes, already computed by the caller
/// (names sanitized, IP normalized, secrets encrypted with `APP_KEY`).
#[derive(Debug, Clone)]
pub struct NewServer {
    pub uuid: String,
    pub name: String,
    /// `HasSafeStringAttribute`: NULL when the request has none (4.7.5).
    pub description: Option<String>,
    pub ip: String,
    pub port: i64,
    pub user: String,
    pub private_key_id: i64,
    pub team_id: TeamId,
    /// `{"redirect_enabled":true,"type":...,"status":"exited"}` as JSON text
    /// (bound as text: a jsonb round trip would reorder the keys).
    pub proxy: String,
    pub sentinel_token_encrypted: String,
    pub sentinel_custom_url: Option<String>,
    pub docker_uuid: String,
    /// `COOLIFY_SERVER_UUID` and `COOLIFY_SERVER_NAME` shared variables, encrypted.
    pub shared_vars: [(&'static str, String); 2],
    pub server_role: &'static str,
    /// The API always writes the role; the web form (`ByIp`) saves the
    /// settings model, which writes only when the role differs from `both`.
    pub always_write_role: bool,
}

/// `Team::createServerWithinLimit` with the `Server::created` and
/// `ServerSetting::creating` hooks, then the controller's proxy save and role
/// update. Unlike upstream, it all runs in one transaction.
pub async fn create(db: &Db, n: &NewServer, now: chrono::NaiveDateTime) -> DbResult<i64> {
    db.ensure_writable()?;
    let mut tx = db.pool().begin().await?;
    // ensureServerCapacity: lock the team row (self-hosted has no server limit).
    sqlx::query("SELECT id FROM teams WHERE id = $1 FOR UPDATE")
        .bind(n.team_id)
        .execute(&mut *tx)
        .await?;
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO servers (name, description, ip, port, \"user\", private_key_id, team_id, uuid, updated_at, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $9) RETURNING id",
    )
    .bind(&n.name)
    .bind(&n.description)
    .bind(&n.ip)
    .bind(n.port as i32)
    .bind(&n.user)
    .bind(n.private_key_id)
    .bind(n.team_id)
    .bind(&n.uuid)
    .bind(now)
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO server_settings (server_id, sentinel_token, sentinel_custom_url, updated_at, created_at) \
         VALUES ($1, $2, $3, $4, $4)",
    )
    .bind(id)
    .bind(&n.sentinel_token_encrypted)
    .bind(&n.sentinel_custom_url)
    .bind(now)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO standalone_dockers (name, uuid, network, server_id, updated_at, created_at) \
         VALUES ('coolify', $1, 'coolify', $2, $3, $3)",
    )
    .bind(&n.docker_uuid)
    .bind(id)
    .bind(now)
    .execute(&mut *tx)
    .await?;
    for (key, value) in &n.shared_vars {
        sqlx::query(
            "INSERT INTO shared_environment_variables (key, value, type, server_id, team_id, is_literal, updated_at, created_at) \
             VALUES ($1, $2, 'server', $3, $4, true, $5, $5)",
        )
        .bind(key)
        .bind(value)
        .bind(id)
        .bind(n.team_id)
        .bind(now)
        .execute(&mut *tx)
        .await?;
    }
    sqlx::query("UPDATE servers SET proxy = $2::json, updated_at = $3 WHERE id = $1")
        .bind(id)
        .bind(&n.proxy)
        .bind(now)
        .execute(&mut *tx)
        .await?;
    if n.always_write_role || n.server_role != "both" {
        sqlx::query(
            "UPDATE server_settings SET server_role = $2, is_build_server = $3, updated_at = $4 WHERE server_id = $1",
        )
        .bind(id)
        .bind(n.server_role)
        .bind(n.server_role == "build")
        .bind(now)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(id)
}

const DESTINATION_MATCH: &str = "((destination_type = 'App\\Models\\StandaloneDocker' AND destination_id IN \
     (SELECT id FROM standalone_dockers WHERE server_id = $1)) \
  OR (destination_type = 'App\\Models\\SwarmDocker' AND destination_id IN \
     (SELECT id FROM swarm_dockers WHERE server_id = $1)))";

/// Database tables `Server::databases()` reads.
pub(crate) const DATABASE_TABLES: &[&str] = &[
    "standalone_postgresqls",
    "standalone_redis",
    "standalone_mongodbs",
    "standalone_mysqls",
    "standalone_mariadbs",
    "standalone_keydbs",
    "standalone_dragonflies",
    "standalone_clickhouses",
    "standalone_sqlites",
];

/// `$server->definedResources()->count()`: applications (incl. additional
/// destinations), databases and services; trashed rows excluded.
pub async fn resource_count(db: &Db, server_id: i64) -> DbResult<i64> {
    let mut parts = vec![
        format!("SELECT count(*) FROM applications WHERE deleted_at IS NULL AND {DESTINATION_MATCH}"),
        "SELECT count(*) FROM additional_destinations ad JOIN applications a ON a.id = ad.application_id \
         WHERE ad.server_id = $1 AND a.deleted_at IS NULL"
            .to_string(),
        "SELECT count(*) FROM services WHERE server_id = $1 AND deleted_at IS NULL".to_string(),
    ];
    parts.extend(DATABASE_TABLES.iter().map(|t| {
        format!("SELECT count(*) FROM {t} WHERE deleted_at IS NULL AND {DESTINATION_MATCH}")
    }));
    let sql = format!(
        "SELECT ({})::bigint",
        parts
            .iter()
            .map(|p| format!("({p})"))
            .collect::<Vec<_>>()
            .join(" + ")
    );
    Ok(sqlx::query_scalar(&sql)
        .bind(server_id)
        .fetch_one(db.pool())
        .await?)
}

/// `$server->primaryResources()->pluck('name')` (v4.7.5): the resources that deleting the server
/// with "Delete all resources" deletes: the applications whose primary destination is on the
/// server, the databases and the services. An application that has the server only as an
/// additional destination runs on another server, so it is detached instead.
pub async fn primary_resource_names(db: &Db, server_id: i64) -> DbResult<Vec<String>> {
    let mut parts = vec![format!(
        "SELECT 0 AS kind, id, name::text AS name FROM applications WHERE deleted_at IS NULL AND {DESTINATION_MATCH}"
    )];
    parts.extend(DATABASE_TABLES.iter().map(|t| {
        format!(
            "SELECT 1 AS kind, id, name::text AS name FROM {t} WHERE deleted_at IS NULL AND {DESTINATION_MATCH}"
        )
    }));
    parts.push(
        "SELECT 2 AS kind, id, name::text AS name FROM services WHERE server_id = $1 AND deleted_at IS NULL"
            .to_string(),
    );
    let sql = format!(
        "SELECT name FROM ({}) r ORDER BY kind, id",
        parts
            .iter()
            .map(|p| format!("({p})"))
            .collect::<Vec<_>>()
            .join(" UNION ALL ")
    );
    Ok(sqlx::query_scalar(&sql)
        .bind(server_id)
        .fetch_all(db.pool())
        .await?)
}

/// One of `$server->additionalApplications()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdditionalApplication {
    pub id: i64,
    pub name: String,
    /// `destination.server.name`: the server the application keeps running on.
    pub primary_server_name: Option<String>,
}

/// `$server->additionalApplications()` (v4.7.5): the applications that use this server only as
/// an additional destination. Their primary destination is on another server.
pub async fn additional_applications(
    db: &Db,
    server_id: i64,
) -> DbResult<Vec<AdditionalApplication>> {
    let rows: Vec<(i64, String, Option<String>)> = sqlx::query_as(&format!(
        "SELECT a.id::bigint, a.name::text, \
         COALESCE((SELECT s.name::text FROM standalone_dockers sd JOIN servers s ON s.id = sd.server_id \
                   WHERE a.destination_type = 'App\\Models\\StandaloneDocker' AND sd.id = a.destination_id), \
                  (SELECT s.name::text FROM swarm_dockers sw JOIN servers s ON s.id = sw.server_id \
                   WHERE a.destination_type = 'App\\Models\\SwarmDocker' AND sw.id = a.destination_id)) \
         FROM applications a WHERE a.deleted_at IS NULL \
         AND a.id IN (SELECT application_id FROM additional_destinations WHERE server_id = $1) \
         AND a.id NOT IN (SELECT id FROM applications WHERE {DESTINATION_MATCH}) ORDER BY a.id"
    ))
    .bind(server_id)
    .fetch_all(db.pool())
    .await?;
    Ok(rows
        .into_iter()
        .map(|(id, name, primary_server_name)| AdditionalApplication {
            id,
            name,
            primary_server_name,
        })
        .collect())
}

/// The delete of `$server->detachAdditionalApplications()`: removes this server from the
/// applications that use it only as an additional destination.
pub async fn detach_additional_applications(
    db: &Db,
    server_id: i64,
    application_ids: &[i64],
) -> DbResult<()> {
    if application_ids.is_empty() {
        return Ok(());
    }
    db.ensure_writable()?;
    sqlx::query(
        "DELETE FROM additional_destinations WHERE server_id = $1 AND application_id = ANY($2)",
    )
    .bind(server_id)
    .bind(application_ids)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// `Server::sshEndpointInUse()` (v4.7.5): whether a server of any team already uses this SSH
/// endpoint. The port is part of the endpoint, so servers behind one NAT address with different
/// forwarded SSH ports are different servers. `ip` is already normalized.
pub async fn ssh_endpoint_in_use(
    db: &Db,
    ip: &str,
    port: i64,
    except_id: Option<i64>,
) -> DbResult<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM servers WHERE deleted_at IS NULL AND ip = $1 AND port = $2::integer \
         AND ($3::bigint IS NULL OR id <> $3))",
    )
    .bind(ip)
    .bind(port)
    .bind(except_id)
    .fetch_one(db.pool())
    .await?)
}

/// `$server->delete()`: the soft delete.
pub async fn soft_delete(db: &Db, server_id: i64, now: chrono::NaiveDateTime) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query("UPDATE servers SET deleted_at = $2, updated_at = $2 WHERE id = $1")
        .bind(server_id)
        .bind(now)
        .execute(db.pool())
        .await?;
    // `HasStars::bootHasStars()`.
    crate::stars::delete_for(db, crate::stars::SERVER_MORPH, server_id).await?;
    Ok(())
}

/// `forceDelete()` with the `forceDeleting` hook (destinations, runner rows and config, settings,
/// SSL certificates, notification throttles). One transaction.
pub async fn force_delete(db: &Db, server_id: i64) -> DbResult<()> {
    db.ensure_writable()?;
    let mut tx = db.pool().begin().await?;
    for sql in [
        "DELETE FROM standalone_dockers WHERE server_id = $1",
        "DELETE FROM swarm_dockers WHERE server_id = $1",
        "DELETE FROM github_runner_executions WHERE server_id = $1",
        "DELETE FROM github_runner_configs WHERE server_id = $1",
        "DELETE FROM server_settings WHERE server_id = $1",
        "DELETE FROM ssl_certificates WHERE server_id = $1",
        "DELETE FROM notification_throttles WHERE notifiable_type = 'App\\Models\\Server' AND notifiable_id = $1",
        "DELETE FROM servers WHERE id = $1",
    ] {
        sqlx::query(sql).bind(server_id).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(())
}

/// A typed column value for the dynamic updates below.
#[derive(Debug, Clone, PartialEq)]
pub enum ColumnValue {
    Text(Option<String>),
    Int(i64),
    Bool(bool),
    Time(chrono::NaiveDateTime),
}

pub(crate) fn bind_all<'q>(
    mut q: sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments>,
    values: &'q [(&'static str, ColumnValue)],
) -> sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments> {
    for (_, v) in values {
        q = match v {
            ColumnValue::Text(t) => q.bind(t),
            ColumnValue::Int(i) => q.bind(*i),
            ColumnValue::Bool(b) => q.bind(*b),
            ColumnValue::Time(t) => q.bind(*t),
        };
    }
    q
}

pub(crate) fn set_clause(values: &[(&'static str, ColumnValue)], first_param: usize) -> String {
    values
        .iter()
        .enumerate()
        .map(|(i, (col, v))| {
            let cast = match v {
                ColumnValue::Int(_) if *col == "backup_compression_cpu_percentage" => "::smallint",
                ColumnValue::Int(_) if *col != "private_key_id" => "::integer",
                // The JSON columns of `applications`.
                ColumnValue::Text(Some(_))
                    if matches!(*col, "noindex_domains" | "domain_port_overrides") =>
                {
                    "::json"
                }
                _ => "",
            };
            // A NULL is written as a literal: a text-typed NULL parameter does not fit integer columns.
            if matches!(v, ColumnValue::Text(None)) {
                return format!("\"{col}\" = NULL");
            }
            format!("\"{col}\" = ${}{cast}", first_param + i)
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// The SSH address of a server: `ip`, `port` and `server_settings.is_cloudflare_tunnel`.
type SshAddress = Option<(Option<String>, Option<i64>, Option<bool>)>;

/// True when a write can change the address that the pinned SSH host key belongs to. A write
/// that sets the key too (a transfer import) already has the key of the new address.
fn touches_ssh_address(values: &[(&'static str, ColumnValue)]) -> bool {
    values
        .iter()
        .any(|(c, _)| matches!(*c, "ip" | "port" | "is_cloudflare_tunnel"))
        && !values.iter().any(|(c, _)| *c == "ssh_host_key")
}

async fn ssh_address(db: &Db, server_id: i64) -> DbResult<SshAddress> {
    Ok(sqlx::query_as(
        "SELECT s.ip::text, s.port::bigint, (SELECT ss.is_cloudflare_tunnel FROM server_settings ss \
         WHERE ss.server_id = s.id LIMIT 1) FROM servers s WHERE s.id = $1",
    )
    .bind(server_id)
    .fetch_optional(db.pool())
    .await?)
}

/// The address before a write that [`touches_ssh_address`]; `None` when the write does not.
async fn ssh_address_before(
    db: &Db,
    server_id: i64,
    values: &[(&'static str, ColumnValue)],
) -> DbResult<Option<SshAddress>> {
    if !touches_ssh_address(values) {
        return Ok(None);
    }
    Ok(Some(ssh_address(db, server_id).await?))
}

/// The `saved` hook of `Server` (`wasChanged(['ip', 'port'])`) and the `updated` hook of
/// `ServerSetting` (`wasChanged('is_cloudflare_tunnel')`) at v4.7.5:
/// `forgetSshHostKey()` removes the pinned and the pending host key after the address of the
/// server changed, so that the next connection trusts the key of the new address.
async fn forget_ssh_host_key_if_address_changed(
    db: &Db,
    server_id: i64,
    before: Option<SshAddress>,
    now: chrono::NaiveDateTime,
) -> DbResult<()> {
    let Some(before) = before else {
        return Ok(());
    };
    if ssh_address(db, server_id).await? == before {
        return Ok(());
    }
    sqlx::query(
        "UPDATE server_settings SET ssh_host_key = NULL, ssh_host_key_source = NULL, \
         ssh_host_key_trusted_at = NULL, ssh_host_key_pending = NULL, updated_at = $2 \
         WHERE server_id = $1 AND (ssh_host_key IS NOT NULL OR ssh_host_key_pending IS NOT NULL)",
    )
    .bind(server_id)
    .bind(now)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// `$server->update($fields)` after the caller's dirty check: the changed
/// columns plus `updated_at` (nothing when nothing changed).
pub async fn update_columns(
    db: &Db,
    server_id: i64,
    changes: &[(&'static str, ColumnValue)],
    now: chrono::NaiveDateTime,
) -> DbResult<()> {
    if changes.is_empty() {
        return Ok(());
    }
    db.ensure_writable()?;
    let address = ssh_address_before(db, server_id, changes).await?;
    let sql = format!(
        "UPDATE servers SET {}, updated_at = $1 WHERE id = $2",
        set_clause(changes, 3)
    );
    bind_all(sqlx::query(&sql).bind(now).bind(server_id), changes)
        .execute(db.pool())
        .await?;
    forget_ssh_host_key_if_address_changed(db, server_id, address, now).await?;
    Ok(())
}

/// Which row an [`update_if_changed`] targets.
#[derive(Debug, Clone, Copy)]
pub enum Row {
    Server(i64),
    Settings(i64),
}

/// A model `save()` after attribute changes: writes the columns and
/// `updated_at` only when at least one value differs from the stored row
/// (Eloquent's dirty check). The changed columns are written all together,
/// like `update([...])` with several attributes.
pub async fn update_if_changed(
    db: &Db,
    row: Row,
    values: &[(&'static str, ColumnValue)],
    now: chrono::NaiveDateTime,
) -> DbResult<bool> {
    if values.is_empty() {
        return Ok(false);
    }
    db.ensure_writable()?;
    let (table, key) = match row {
        Row::Server(id) => ("servers", id),
        Row::Settings(id) => ("server_settings", id),
    };
    let key_col = if matches!(row, Row::Server(_)) {
        "id"
    } else {
        "server_id"
    };
    let changed = values
        .iter()
        .enumerate()
        .map(|(i, (col, _))| format!("\"{col}\" IS DISTINCT FROM ${}", i + 3))
        .collect::<Vec<_>>()
        .join(" OR ");
    // Both keys are the server id.
    let address = ssh_address_before(db, key, values).await?;
    let sql = format!(
        "UPDATE {table} SET {}, updated_at = $1 WHERE {key_col} = $2 AND ({changed})",
        set_clause(values, 3)
    );
    let done = bind_all(sqlx::query(&sql).bind(now).bind(key), values)
        .execute(db.pool())
        .await?;
    forget_ssh_host_key_if_address_changed(db, key, address, now).await?;
    Ok(done.rows_affected() > 0)
}

/// `$server->settings()->update($values)`: a query-builder update, so it
/// always sets `updated_at` and fires no model events.
pub async fn update_settings(
    db: &Db,
    server_id: i64,
    values: &[(&'static str, ColumnValue)],
    now: chrono::NaiveDateTime,
) -> DbResult<()> {
    db.ensure_writable()?;
    let sets = if values.is_empty() {
        String::new()
    } else {
        format!("{}, ", set_clause(values, 3))
    };
    let address = ssh_address_before(db, server_id, values).await?;
    let sql = format!("UPDATE server_settings SET {sets}updated_at = $1 WHERE server_id = $2");
    bind_all(sqlx::query(&sql).bind(now).bind(server_id), values)
        .execute(db.pool())
        .await?;
    forget_ssh_host_key_if_address_changed(db, server_id, address, now).await?;
    Ok(())
}

/// `$server->hasEnabledGithubRunners()`.
pub async fn has_enabled_runners(db: &Db, server_id: i64) -> DbResult<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM github_runner_configs WHERE server_id = $1 AND is_enabled)",
    )
    .bind(server_id)
    .fetch_one(db.pool())
    .await?)
}

/// `PrivateKey::ownedAndOnlySShKeys()->where('id', '!=', 0)`: `(id, name)`.
pub async fn ssh_keys_for_form(db: &Db, team_id: TeamId) -> DbResult<Vec<(i64, String)>> {
    Ok(sqlx::query_as(
        "SELECT id, name FROM private_keys WHERE team_id = $1 AND is_git_related = false AND id <> 0",
    )
    .bind(team_id)
    .fetch_all(db.pool())
    .await?)
}
