//! Image vulnerability scanning (upstream v4.7.0+): `image_scans`, `image_sboms`,
//! `image_scan_policies`, `image_scan_ignores`, and the scan columns of `server_settings`.
//!
//! Ports the models `ImageScan`, `ImageSbom`, `ImageScanPolicy`, `ImageScanIgnore` and the trait
//! `HasImageScans`. Submodules hold the queries of one part of the engine: [`notifications`]
//! (`ImageScanNotificationState`), [`host`] (`ServerHostScan`), [`candidates`] (version checks and
//! image updates).
//!
//! A scan belongs to a *scan owner*: an application, a service application, a service database or
//! a standalone database ([`ScanOwner`]). A service shows the scans of its applications and
//! databases and owns none itself.

pub mod candidates;
pub mod gate;
pub mod host;
pub mod jobs;
pub mod notifications;
pub mod update;
pub mod views;

use chrono::NaiveDateTime;
use coolify_domain::new_public_id;
use serde_json::Value;

use crate::databases::Kind;
use crate::records::{Record, parse_row};
use crate::{Db, DbError, DbResult};

pub const APPLICATION_MORPH: &str = r"App\Models\Application";
pub const SERVICE_MORPH: &str = r"App\Models\Service";
pub const SERVICE_APPLICATION_MORPH: &str = r"App\Models\ServiceApplication";
pub const SERVICE_DATABASE_MORPH: &str = r"App\Models\ServiceDatabase";
pub const TEAM_MORPH: &str = r"App\Models\Team";
pub const SERVER_MORPH: &str = r"App\Models\Server";

/// `ImageScanStatus`.
pub const STATUS_QUEUED: &str = "queued";
pub const STATUS_RUNNING: &str = "running";
pub const STATUS_COMPLETED: &str = "completed";
pub const STATUS_FAILED: &str = "failed";

/// `ApplicationDeploymentStatus::FINISHED`.
const DEPLOYMENT_FINISHED: &str = "finished";

// ---------------------------------------------------------------------------------------------
// Scan owners
// ---------------------------------------------------------------------------------------------

/// The table and the static morph class of a resource type that can own scans or be a resource
/// page (`Service`). `None` for any other class.
pub fn resource_table(morph: &str) -> Option<(&'static str, &'static str)> {
    match morph {
        APPLICATION_MORPH => Some(("applications", APPLICATION_MORPH)),
        SERVICE_MORPH => Some(("services", SERVICE_MORPH)),
        SERVICE_APPLICATION_MORPH => Some(("service_applications", SERVICE_APPLICATION_MORPH)),
        SERVICE_DATABASE_MORPH => Some(("service_databases", SERVICE_DATABASE_MORPH)),
        other => Kind::ALL
            .into_iter()
            .find(|k| k.morph() == other)
            .map(|k| (k.table(), k.morph())),
    }
}

/// A resource as the scan engine reads it: an application, a service, a service application, a
/// service database or a standalone database, with its team and its main server.
#[derive(Debug, Clone)]
pub struct ScanOwner {
    /// `$resource->getMorphClass()`.
    pub morph: &'static str,
    /// `$resource->getKey()`.
    pub id: i64,
    pub uuid: String,
    /// `$resource->team()?->id`.
    pub team_id: Option<i64>,
    /// `ImageScanPolicyResolver::mainServer($resource)`: the destination server, or the server of
    /// a service. `None` when the server is gone.
    pub main_server_id: Option<i64>,
    /// The service of a service application or service database.
    pub service_id: Option<i64>,
    /// The whole row (`row_to_json`), for attributes that have no accessor here.
    pub record: Record,
    /// `$application->settings` (`application_settings`); `None` for other resources.
    pub settings: Option<Record>,
}

impl ScanOwner {
    /// `ResourceScans::key()`: unique across resource types.
    pub fn key(&self) -> String {
        format!("{}:{}", self.morph, self.id)
    }

    pub fn is_application(&self) -> bool {
        self.morph == APPLICATION_MORPH
    }

    pub fn is_service(&self) -> bool {
        self.morph == SERVICE_MORPH
    }

    /// A service application or a service database.
    pub fn is_service_part(&self) -> bool {
        self.morph == SERVICE_APPLICATION_MORPH || self.morph == SERVICE_DATABASE_MORPH
    }

    /// `in_array(HasImageScans::class, class_uses_recursive($resource))`: every type but `Service`.
    pub fn has_image_scans(&self) -> bool {
        !self.is_service()
    }

    /// A string attribute of the row; `None` for null and for other types.
    pub fn str(&self, key: &str) -> Option<&str> {
        self.record.get(key).and_then(Value::as_str)
    }

    /// `(string) $resource->getAttribute($key)`.
    pub fn string(&self, key: &str) -> String {
        match self.record.get(key) {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Number(n)) => n.to_string(),
            Some(Value::Bool(true)) => "1".to_owned(),
            _ => String::new(),
        }
    }

    pub fn name(&self) -> String {
        self.string("name")
    }

    /// `$application->build_pack`; `None` for other resources.
    pub fn build_pack(&self) -> Option<&str> {
        self.is_application()
            .then(|| self.str("build_pack"))
            .flatten()
    }

    /// `trim((string) $owner->getAttribute('image'))`.
    pub fn image(&self) -> String {
        self.string("image").trim().to_owned()
    }

    /// `$owner->getAttribute('exclude_from_status')` as a boolean.
    pub fn exclude_from_status(&self) -> bool {
        self.record
            .get("exclude_from_status")
            .and_then(Value::as_bool)
            == Some(true)
    }

    /// `(bool) $application->settings?->is_image_scan_enabled`. The column defaults to true.
    pub fn is_image_scan_enabled(&self) -> bool {
        self.settings
            .as_ref()
            .and_then(|s| s.get("is_image_scan_enabled"))
            .and_then(Value::as_bool)
            == Some(true)
    }

    /// `$application->settings?->image_builds_by_team`.
    pub fn image_builds_by_team(&self) -> Option<bool> {
        self.settings
            .as_ref()
            .and_then(|s| s.get("image_builds_by_team"))
            .and_then(Value::as_bool)
    }
}

/// The destination server of `x`, when it still exists.
fn destination_server(x: &str) -> String {
    format!(
        "(SELECT srv.id FROM servers srv WHERE srv.deleted_at IS NULL AND srv.id = CASE {x}.destination_type \
         WHEN 'App\\Models\\StandaloneDocker' THEN (SELECT d.server_id FROM standalone_dockers d WHERE d.id = {x}.destination_id) \
         WHEN 'App\\Models\\SwarmDocker' THEN (SELECT d.server_id FROM swarm_dockers d WHERE d.id = {x}.destination_id) END)"
    )
}

/// `SELECT record, team_id, server_id, service_id, settings FROM …` for one resource table, with
/// the alias `t` for the resource. The caller adds the `WHERE`.
fn owner_select(table: &str) -> String {
    match table {
        "applications" => format!(
            "SELECT row_to_json(t)::text, p.team_id, {server}, NULL::bigint, \
             (SELECT row_to_json(st)::text FROM application_settings st WHERE st.application_id = t.id LIMIT 1) \
             FROM applications t \
             LEFT JOIN environments e ON e.id = t.environment_id \
             LEFT JOIN projects p ON p.id = e.project_id",
            server = destination_server("t")
        ),
        "services" => format!(
            "SELECT row_to_json(t)::text, p.team_id, \
             COALESCE({server}, (SELECT srv.id FROM servers srv WHERE srv.deleted_at IS NULL AND srv.id = t.server_id)), \
             NULL::bigint, NULL::text \
             FROM services t \
             LEFT JOIN environments e ON e.id = t.environment_id \
             LEFT JOIN projects p ON p.id = e.project_id",
            server = destination_server("t")
        ),
        "service_applications" | "service_databases" => format!(
            "SELECT row_to_json(t)::text, p.team_id, \
             COALESCE({server}, (SELECT srv.id FROM servers srv WHERE srv.deleted_at IS NULL AND srv.id = sv.server_id)), \
             t.service_id::bigint, NULL::text \
             FROM {table} t \
             LEFT JOIN services sv ON sv.id = t.service_id AND sv.deleted_at IS NULL \
             LEFT JOIN environments e ON e.id = sv.environment_id \
             LEFT JOIN projects p ON p.id = e.project_id",
            server = destination_server("sv")
        ),
        // The instance database (`coolify-db`, id 0) has no environment and belongs to the root team.
        "standalone_postgresqls" => format!(
            "SELECT row_to_json(t)::text, CASE WHEN t.id = 0 THEN 0 ELSE p.team_id END, {server}, NULL::bigint, NULL::text \
             FROM standalone_postgresqls t \
             LEFT JOIN environments e ON e.id = t.environment_id \
             LEFT JOIN projects p ON p.id = e.project_id",
            server = destination_server("t")
        ),
        _ => format!(
            "SELECT row_to_json(t)::text, p.team_id, {server}, NULL::bigint, NULL::text \
             FROM {table} t \
             LEFT JOIN environments e ON e.id = t.environment_id \
             LEFT JOIN projects p ON p.id = e.project_id",
            server = destination_server("t")
        ),
    }
}

type OwnerTuple = (
    String,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<String>,
);

fn owner_from(morph: &'static str, tuple: OwnerTuple) -> DbResult<ScanOwner> {
    let (text, team_id, main_server_id, service_id, settings) = tuple;
    let record =
        parse_row(&text).map_err(|e| DbError::Schema(format!("decoding {morph} row: {e}")))?;
    let settings = settings
        .map(|s| parse_row(&s))
        .transpose()
        .map_err(|e| DbError::Schema(format!("decoding application settings row: {e}")))?;
    Ok(ScanOwner {
        morph,
        id: record.get("id").and_then(Value::as_i64).unwrap_or_default(),
        uuid: record
            .get("uuid")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        team_id,
        main_server_id,
        service_id,
        record,
        settings,
    })
}

/// The resource of a morph relation (`$scan->resource`, `$ignore->resource`): `None` for an
/// unknown class and for a resource that is gone or soft-deleted.
pub async fn find_owner(db: &Db, morph: &str, id: i64) -> DbResult<Option<ScanOwner>> {
    let Some((table, morph)) = resource_table(morph) else {
        return Ok(None);
    };
    let row: Option<OwnerTuple> = sqlx::query_as(&format!(
        "{} WHERE t.id = $1 AND t.deleted_at IS NULL",
        owner_select(table)
    ))
    .bind(id)
    .fetch_optional(db.pool())
    .await?;
    row.map(|r| owner_from(morph, r)).transpose()
}

async fn find_owner_by_uuid_in(
    db: &Db,
    table: &'static str,
    morph: &'static str,
    uuid: &str,
) -> DbResult<Option<ScanOwner>> {
    let row: Option<OwnerTuple> = sqlx::query_as(&format!(
        "{} WHERE t.uuid = $1 AND t.deleted_at IS NULL LIMIT 1",
        owner_select(table)
    ))
    .bind(uuid)
    .fetch_optional(db.pool())
    .await?;
    row.map(|r| owner_from(morph, r)).transpose()
}

/// `ResourceScans::find($type, $uuid, $teamIds)`: the resource of a resource page by type
/// (`application`, `service`, `database`) and uuid, only in the given teams.
pub async fn find_resource(
    db: &Db,
    kind: &str,
    uuid: &str,
    team_ids: &[i64],
) -> DbResult<Option<ScanOwner>> {
    let found = match kind {
        "application" => find_owner_by_uuid_in(db, "applications", APPLICATION_MORPH, uuid).await?,
        "service" => find_owner_by_uuid_in(db, "services", SERVICE_MORPH, uuid).await?,
        "database" => {
            let mut found = None;
            for k in Kind::ALL {
                found = find_owner_by_uuid_in(db, k.table(), k.morph(), uuid).await?;
                if found.is_some() {
                    break;
                }
            }
            found
        }
        _ => None,
    };
    Ok(found.filter(|r| r.team_id.is_some_and(|t| team_ids.contains(&t))))
}

/// `$service->applications()->get()->concat($service->databases()->get())`.
pub async fn service_owners(db: &Db, service_id: i64) -> DbResult<Vec<ScanOwner>> {
    let mut out = Vec::new();
    for (table, morph) in [
        ("service_applications", SERVICE_APPLICATION_MORPH),
        ("service_databases", SERVICE_DATABASE_MORPH),
    ] {
        let rows: Vec<OwnerTuple> = sqlx::query_as(&format!(
            "{} WHERE t.service_id = $1 AND t.deleted_at IS NULL",
            owner_select(table)
        ))
        .bind(service_id)
        .fetch_all(db.pool())
        .await?;
        for r in rows {
            out.push(owner_from(morph, r)?);
        }
    }
    Ok(out)
}

/// `ResourceScans::owners($resource)`: the applications and databases of a service, else the
/// resource itself.
pub async fn owners(db: &Db, resource: &ScanOwner) -> DbResult<Vec<ScanOwner>> {
    if resource.is_service() {
        service_owners(db, resource.id).await
    } else {
        Ok(vec![resource.clone()])
    }
}

/// The service (`$owner->service`) of a service application or database.
pub async fn owner_service(db: &Db, owner: &ScanOwner) -> DbResult<Option<ScanOwner>> {
    match owner.service_id {
        Some(id) if owner.is_service_part() => find_owner(db, SERVICE_MORPH, id).await,
        _ => Ok(None),
    }
}

/// Every resource of one type whose main server is `server_id` (the applications, service
/// applications, service databases and standalone databases that run on a server).
pub async fn owners_on_server(db: &Db, server_id: i64) -> DbResult<Vec<ScanOwner>> {
    let mut tables: Vec<(&'static str, &'static str)> = vec![
        ("applications", APPLICATION_MORPH),
        ("service_applications", SERVICE_APPLICATION_MORPH),
        ("service_databases", SERVICE_DATABASE_MORPH),
    ];
    tables.extend(Kind::ALL.into_iter().map(|k| (k.table(), k.morph())));
    let mut out = Vec::new();
    for (table, morph) in tables {
        let select = owner_select(table);
        let rows: Vec<OwnerTuple> = sqlx::query_as(&format!(
            "SELECT * FROM ({select} WHERE t.deleted_at IS NULL) o(record, team_id, server_id, service_id, settings) \
             WHERE o.server_id = $1"
        ))
        .bind(server_id)
        .fetch_all(db.pool())
        .await?;
        for r in rows {
            out.push(owner_from(morph, r)?);
        }
    }
    Ok(out)
}

/// `HasImageScans::bootHasImageScans()`: the scans, the scan rules, the ignores and the
/// notification state of a resource go away with it. Call it where a scan owner is deleted
/// (soft or forced).
pub async fn delete_resource_data(db: &Db, morph: &str, id: i64) -> DbResult<()> {
    db.ensure_writable()?;
    for sql in [
        "DELETE FROM image_scans WHERE resource_type = $1 AND resource_id = $2",
        "DELETE FROM image_scan_policies WHERE scope_type = $1 AND scope_id = $2",
        "DELETE FROM image_scan_ignores WHERE resource_type = $1 AND resource_id = $2",
        "DELETE FROM image_scan_notification_states WHERE resource_type = $1 AND resource_id = $2",
    ] {
        sqlx::query(sql)
            .bind(morph)
            .bind(id)
            .execute(db.pool())
            .await?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Server settings
// ---------------------------------------------------------------------------------------------

/// The image scanning columns of `server_settings`.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct ServerScanSettings {
    pub is_image_scan_enabled: bool,
    /// `image_scan_memory_mb` (default 1024).
    pub image_scan_memory_mb: i64,
    /// `image_scan_syft_timeout` in seconds (default 600).
    pub image_scan_syft_timeout: i64,
    /// `image_scan_grype_timeout` in seconds (default 600).
    pub image_scan_grype_timeout: i64,
    /// `image_scan_cpus` (default 1).
    pub image_scan_cpus: f64,
    pub image_candidate_scans_today: i64,
    /// `image_candidate_scans_date` as `YYYY-MM-DD`.
    pub image_candidate_scans_date: Option<String>,
}

impl Default for ServerScanSettings {
    /// The column defaults (`ServerSetting::DEFAULT_IMAGE_SCAN_*`), with scanning off.
    fn default() -> Self {
        Self {
            is_image_scan_enabled: false,
            image_scan_memory_mb: 1024,
            image_scan_syft_timeout: 600,
            image_scan_grype_timeout: 600,
            image_scan_cpus: 1.0,
            image_candidate_scans_today: 0,
            image_candidate_scans_date: None,
        }
    }
}

/// `$server->settings` as the scanner reads it; `None` when the server has no settings row.
pub async fn server_scan_settings(db: &Db, server_id: i64) -> DbResult<Option<ServerScanSettings>> {
    Ok(sqlx::query_as(
        "SELECT is_image_scan_enabled, image_scan_memory_mb::bigint AS image_scan_memory_mb, \
         image_scan_syft_timeout::bigint AS image_scan_syft_timeout, \
         image_scan_grype_timeout::bigint AS image_scan_grype_timeout, \
         image_scan_cpus::float8 AS image_scan_cpus, \
         image_candidate_scans_today::bigint AS image_candidate_scans_today, \
         image_candidate_scans_date::text AS image_candidate_scans_date \
         FROM server_settings WHERE server_id = $1 LIMIT 1",
    )
    .bind(server_id)
    .fetch_optional(db.pool())
    .await?)
}

/// `$server->settings->update(['is_image_scan_enabled' => $enabled])`. Returns whether it changed.
pub async fn set_server_scan_enabled(
    db: &Db,
    server_id: i64,
    enabled: bool,
    now: NaiveDateTime,
) -> DbResult<bool> {
    db.ensure_writable()?;
    let done = sqlx::query(
        "UPDATE server_settings SET is_image_scan_enabled = $2, updated_at = $3 \
         WHERE server_id = $1 AND is_image_scan_enabled IS DISTINCT FROM $2",
    )
    .bind(server_id)
    .bind(enabled)
    .bind(now)
    .execute(db.pool())
    .await?;
    Ok(done.rows_affected() > 0)
}

/// The ids of the servers that scan images (`server_settings.is_image_scan_enabled`).
pub async fn scanning_server_ids(db: &Db) -> DbResult<Vec<i64>> {
    Ok(sqlx::query_scalar(
        "SELECT s.id FROM servers s JOIN server_settings ss ON ss.server_id = s.id \
         WHERE s.deleted_at IS NULL AND ss.is_image_scan_enabled ORDER BY s.id",
    )
    .fetch_all(db.pool())
    .await?)
}

/// `$team->scan_images_on_new_servers`.
pub async fn team_scans_new_servers(db: &Db, team_id: i64) -> DbResult<bool> {
    Ok(
        sqlx::query_scalar("SELECT scan_images_on_new_servers FROM teams WHERE id = $1")
            .bind(team_id)
            .fetch_optional(db.pool())
            .await?
            .unwrap_or(false),
    )
}

// ---------------------------------------------------------------------------------------------
// image_scans
// ---------------------------------------------------------------------------------------------

/// One `image_scans` row: the scan result of one image for one resource on one server.
///
/// An *unsaved* scan (`ScanImage` returns one for a failure that is not stored) has `id` 0, see
/// [`ImageScanRow::exists`]; its `image_id` is empty when the image was never inspected.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct ImageScanRow {
    pub id: i64,
    pub uuid: String,
    pub server_id: i64,
    pub image_sbom_id: Option<i64>,
    /// `sha256:{64 hex}`.
    pub image_id: String,
    pub repo_digest: Option<String>,
    pub resource_type: String,
    pub resource_id: i64,
    pub image_ref: String,
    pub deployment_queue_id: Option<i64>,
    /// `ImageScanStatus`: queued, running, completed, failed.
    pub status: String,
    /// `ImageScanErrorCode` value.
    pub error_code: Option<String>,
    pub error_context: Option<Value>,
    pub summary: Option<Value>,
    pub findings: Option<Value>,
    pub candidates: Option<Value>,
    pub base_image_ref: Option<String>,
    pub builds_image: bool,
    pub is_baseline: bool,
    pub grype_db_built_at: Option<NaiveDateTime>,
    pub scanned_at: Option<NaiveDateTime>,
    pub created_at: Option<NaiveDateTime>,
    pub updated_at: Option<NaiveDateTime>,
}

impl ImageScanRow {
    /// `$scan->exists`: the row is stored.
    pub fn exists(&self) -> bool {
        self.id != 0
    }

    pub fn is_completed(&self) -> bool {
        self.status == STATUS_COMPLETED
    }

    /// `ResourceScans::key()` of the owner.
    pub fn owner_key(&self) -> String {
        format!("{}:{}", self.resource_type, self.resource_id)
    }

    /// `ImageScan::candidateEntries()`: written as `{status, checked_at, entries}`; older rows
    /// hold a bare list of entries.
    pub fn candidate_entries(&self) -> Vec<Value> {
        let entries = match &self.candidates {
            Some(Value::Array(list)) => list.clone(),
            Some(Value::Object(map)) => match map.get("entries") {
                Some(Value::Array(list)) => list.clone(),
                _ => Vec::new(),
            },
            _ => Vec::new(),
        };
        entries.into_iter().filter(|e| e.is_object()).collect()
    }

    /// `ImageScan::candidateStatus()`: ok, no_better, cannot_compare, auth_required, or failed.
    /// `None` before the first check.
    pub fn candidate_status(&self) -> Option<&str> {
        match &self.candidates {
            Some(Value::Object(map)) => map.get("status").and_then(Value::as_str),
            _ => None,
        }
    }
}

const SCAN_COLUMNS: &str = "id, uuid, server_id, image_sbom_id, image_id, repo_digest, resource_type, \
    resource_id, image_ref, deployment_queue_id, status, error_code, error_context, summary, findings, \
    candidates, base_image_ref, builds_image, is_baseline, grype_db_built_at, scanned_at, created_at, updated_at";

/// [`SCAN_COLUMNS`] without the two large JSON columns, for lists (`findings` and `candidates`
/// read as null).
const SCAN_COLUMNS_LIGHT: &str = "id, uuid, server_id, image_sbom_id, image_id, repo_digest, resource_type, \
    resource_id, image_ref, deployment_queue_id, status, error_code, error_context, summary, NULL::jsonb AS findings, \
    NULL::jsonb AS candidates, base_image_ref, builds_image, is_baseline, grype_db_built_at, scanned_at, created_at, updated_at";

/// A value of a scan column for [`first_or_create_scan`] and [`update_scan`].
#[derive(Debug, Clone, PartialEq)]
pub enum ScanValue {
    Text(Option<String>),
    Int(Option<i64>),
    Bool(bool),
    Json(Option<Value>),
    Time(Option<NaiveDateTime>),
}

/// The columns [`first_or_create_scan`] and [`update_scan`] accept.
const WRITABLE_SCAN_COLUMNS: &[&str] = &[
    "server_id",
    "image_sbom_id",
    "repo_digest",
    "image_ref",
    "deployment_queue_id",
    "status",
    "error_code",
    "error_context",
    "summary",
    "findings",
    "candidates",
    "base_image_ref",
    "builds_image",
    "is_baseline",
    "grype_db_built_at",
    "scanned_at",
];

fn check_scan_columns(values: &[(&'static str, ScanValue)]) -> DbResult<()> {
    match values
        .iter()
        .find(|(c, _)| !WRITABLE_SCAN_COLUMNS.contains(c))
    {
        Some((c, _)) => Err(DbError::Schema(format!(
            "image_scans.{c} is not a writable column"
        ))),
        None => Ok(()),
    }
}

fn bind_scan_values<'q>(
    mut q: sqlx::query::QueryAs<'q, sqlx::Postgres, ImageScanRow, sqlx::postgres::PgArguments>,
    values: &'q [(&'static str, ScanValue)],
) -> sqlx::query::QueryAs<'q, sqlx::Postgres, ImageScanRow, sqlx::postgres::PgArguments> {
    for (_, v) in values {
        q = match v {
            ScanValue::Text(t) => q.bind(t.as_deref()),
            ScanValue::Int(i) => q.bind(*i),
            ScanValue::Bool(b) => q.bind(*b),
            ScanValue::Json(j) => q.bind(j.as_ref().map(sqlx::types::Json)),
            ScanValue::Time(t) => q.bind(*t),
        };
    }
    q
}

pub async fn find_scan(db: &Db, id: i64) -> DbResult<Option<ImageScanRow>> {
    Ok(sqlx::query_as(&format!(
        "SELECT {SCAN_COLUMNS} FROM image_scans WHERE id = $1"
    ))
    .bind(id)
    .fetch_optional(db.pool())
    .await?)
}

pub async fn find_scan_by_uuid(db: &Db, uuid: &str) -> DbResult<Option<ImageScanRow>> {
    Ok(sqlx::query_as(&format!(
        "SELECT {SCAN_COLUMNS} FROM image_scans WHERE uuid = $1 LIMIT 1"
    ))
    .bind(uuid)
    .fetch_optional(db.pool())
    .await?)
}

/// The scan row of (resource, image ID).
pub async fn find_scan_of_image(
    db: &Db,
    resource_type: &str,
    resource_id: i64,
    image_id: &str,
) -> DbResult<Option<ImageScanRow>> {
    Ok(sqlx::query_as(&format!(
        "SELECT {SCAN_COLUMNS} FROM image_scans WHERE resource_type = $1 AND resource_id = $2 AND image_id = $3 LIMIT 1"
    ))
    .bind(resource_type)
    .bind(resource_id)
    .bind(image_id)
    .fetch_optional(db.pool())
    .await?)
}

/// `$resource->imageScans()->get()`: every scan row of a resource.
pub async fn scans_of_resource(
    db: &Db,
    resource_type: &str,
    resource_id: i64,
) -> DbResult<Vec<ImageScanRow>> {
    Ok(sqlx::query_as(&format!(
        "SELECT {SCAN_COLUMNS} FROM image_scans WHERE resource_type = $1 AND resource_id = $2"
    ))
    .bind(resource_type)
    .bind(resource_id)
    .fetch_all(db.pool())
    .await?)
}

/// Every scan row of a server, without `findings` and `candidates`.
pub async fn scans_on_server(db: &Db, server_id: i64) -> DbResult<Vec<ImageScanRow>> {
    Ok(sqlx::query_as(&format!(
        "SELECT {SCAN_COLUMNS_LIGHT} FROM image_scans WHERE server_id = $1"
    ))
    .bind(server_id)
    .fetch_all(db.pool())
    .await?)
}

/// Whether the resource has a completed scan (a resource without one is scanned as a baseline).
pub async fn has_completed_scan(db: &Db, resource_type: &str, resource_id: i64) -> DbResult<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM image_scans WHERE resource_type = $1 AND resource_id = $2 AND status = 'completed')",
    )
    .bind(resource_type)
    .bind(resource_id)
    .fetch_one(db.pool())
    .await?)
}

/// `ImageScan::query()->firstOrCreate($keys, $values)` on (resource, image ID). The boolean is
/// `wasRecentlyCreated`. A row that another run inserts at the same time is returned as found.
pub async fn first_or_create_scan(
    db: &Db,
    resource_type: &str,
    resource_id: i64,
    image_id: &str,
    values: &[(&'static str, ScanValue)],
    now: NaiveDateTime,
) -> DbResult<(ImageScanRow, bool)> {
    check_scan_columns(values)?;
    if let Some(found) = find_scan_of_image(db, resource_type, resource_id, image_id).await? {
        return Ok((found, false));
    }
    db.ensure_writable()?;
    let columns: String = values.iter().map(|(c, _)| format!(", \"{c}\"")).collect();
    let params: String = (0..values.len()).map(|i| format!(", ${}", i + 6)).collect();
    let sql = format!(
        "INSERT INTO image_scans (uuid, resource_type, resource_id, image_id, created_at, updated_at{columns}) \
         VALUES ($1, $2, $3, $4, $5, $5{params}) \
         ON CONFLICT (resource_type, resource_id, image_id) DO NOTHING RETURNING {SCAN_COLUMNS}"
    );
    let query = sqlx::query_as::<_, ImageScanRow>(&sql)
        .bind(new_public_id())
        .bind(resource_type)
        .bind(resource_id)
        .bind(image_id)
        .bind(now);
    if let Some(created) = bind_scan_values(query, values)
        .fetch_optional(db.pool())
        .await?
    {
        return Ok((created, true));
    }
    match find_scan_of_image(db, resource_type, resource_id, image_id).await? {
        Some(found) => Ok((found, false)),
        None => Err(DbError::Schema(format!(
            "could not save the scan row of image {image_id}"
        ))),
    }
}

/// `$scan->forceFill($values)->save()`. Returns the stored row, `None` when it is gone.
/// `updated_at` is always written (Eloquent writes it only when a value changed).
pub async fn update_scan(
    db: &Db,
    id: i64,
    values: &[(&'static str, ScanValue)],
    now: NaiveDateTime,
) -> DbResult<Option<ImageScanRow>> {
    check_scan_columns(values)?;
    db.ensure_writable()?;
    let sets: String = values
        .iter()
        .enumerate()
        .map(|(i, (c, _))| format!("\"{c}\" = ${}, ", i + 3))
        .collect();
    let sql = format!(
        "UPDATE image_scans SET {sets}updated_at = $2 WHERE id = $1 RETURNING {SCAN_COLUMNS}"
    );
    let query = sqlx::query_as::<_, ImageScanRow>(&sql).bind(id).bind(now);
    Ok(bind_scan_values(query, values)
        .fetch_optional(db.pool())
        .await?)
}

pub async fn delete_scan(db: &Db, id: i64) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query("DELETE FROM image_scans WHERE id = $1")
        .bind(id)
        .execute(db.pool())
        .await?;
    Ok(())
}

/// The filter of `ResourceScans::currentFor()`: completed, and without a deployment or with one
/// that is not a preview.
const CURRENT_FILTER: &str = "s.status = 'completed' AND (s.deployment_queue_id IS NULL OR EXISTS \
    (SELECT 1 FROM application_deployment_queues q WHERE q.id = s.deployment_queue_id AND q.pull_request_id = 0))";

/// `ResourceScans::latestFirst()`: scans of finished deployments first (the latest first), then
/// scans without a deployment, then scans of deployments that did not finish; each by scan time.
const LATEST_FIRST: &str = "ORDER BY CASE WHEN s.deployment_queue_id IS NULL THEN 1 \
    WHEN (SELECT q.status FROM application_deployment_queues q WHERE q.id = s.deployment_queue_id) = 'finished' THEN 0 \
    ELSE 2 END, s.deployment_queue_id DESC, s.scanned_at DESC, s.id DESC";

fn prefixed(columns: &str) -> String {
    columns
        .split(", ")
        .map(|c| {
            let c = c.trim();
            if c.starts_with("NULL") {
                c.to_owned()
            } else {
                format!("s.{c}")
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// `ResourceScans::currentFor($owner)`: the completed scan of the latest deployment that is not
/// a preview, else the latest scan without a deployment, plus the other images scanned by the
/// same deployment.
pub async fn current_for(
    db: &Db,
    resource_type: &str,
    resource_id: i64,
) -> DbResult<Vec<ImageScanRow>> {
    let columns = prefixed(SCAN_COLUMNS);
    let latest: Option<ImageScanRow> = sqlx::query_as(&format!(
        "SELECT {columns} FROM image_scans s WHERE s.resource_type = $1 AND s.resource_id = $2 AND {CURRENT_FILTER} {LATEST_FIRST} LIMIT 1"
    ))
    .bind(resource_type)
    .bind(resource_id)
    .fetch_optional(db.pool())
    .await?;
    let Some(latest) = latest else {
        return Ok(Vec::new());
    };
    let Some(deployment_id) = latest.deployment_queue_id else {
        return Ok(vec![latest]);
    };
    Ok(sqlx::query_as(&format!(
        "SELECT {columns} FROM image_scans s WHERE s.resource_type = $1 AND s.resource_id = $2 AND {CURRENT_FILTER} \
         AND s.deployment_queue_id = $3 ORDER BY s.scanned_at DESC, s.id DESC"
    ))
    .bind(resource_type)
    .bind(resource_id)
    .bind(deployment_id)
    .fetch_all(db.pool())
    .await?)
}

/// `ResourceScans::currentScanIds($idsByType)`: the ids of the current scans of many owners,
/// with one query that reads no JSON.
pub async fn current_scan_ids(db: &Db, owners: &[(String, i64)]) -> DbResult<Vec<i64>> {
    if owners.is_empty() {
        return Ok(Vec::new());
    }
    let types: Vec<String> = owners.iter().map(|(t, _)| t.clone()).collect();
    let ids: Vec<i64> = owners.iter().map(|(_, id)| *id).collect();
    let rows: Vec<(i64, String, i64, Option<i64>)> = sqlx::query_as(&format!(
        "SELECT s.id, s.resource_type, s.resource_id, s.deployment_queue_id FROM image_scans s \
         WHERE {CURRENT_FILTER} AND (s.resource_type, s.resource_id) IN \
         (SELECT * FROM unnest($1::text[], $2::bigint[])) {LATEST_FIRST}"
    ))
    .bind(&types)
    .bind(&ids)
    .fetch_all(db.pool())
    .await?;
    let mut latest: Vec<((String, i64), Option<i64>)> = Vec::new();
    let mut out = Vec::new();
    for (id, resource_type, resource_id, deployment) in rows {
        let key = (resource_type, resource_id);
        match latest.iter().find(|(k, _)| *k == key) {
            None => {
                latest.push((key, deployment));
                out.push(id);
            }
            Some((_, first)) if first.is_some() && *first == deployment => out.push(id),
            Some(_) => {}
        }
    }
    Ok(out)
}

/// The scans with the given ids, the latest scan first (`ResourceScans::currentForMany()`).
/// With `light`, `findings` and `candidates` read as null.
pub async fn scans_by_ids(db: &Db, ids: &[i64], light: bool) -> DbResult<Vec<ImageScanRow>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let columns = if light {
        SCAN_COLUMNS_LIGHT
    } else {
        SCAN_COLUMNS
    };
    Ok(sqlx::query_as(&format!(
        "SELECT {columns} FROM image_scans WHERE id = ANY($1) ORDER BY scanned_at DESC, id DESC"
    ))
    .bind(ids)
    .fetch_all(db.pool())
    .await?)
}

/// `ResourceScans::deploymentRowsQuery()`: the scan rows of applications in the given
/// deployments, without the JSON columns.
pub async fn deployment_scan_rows(db: &Db, deployment_ids: &[i64]) -> DbResult<Vec<ImageScanRow>> {
    if deployment_ids.is_empty() {
        return Ok(Vec::new());
    }
    Ok(sqlx::query_as(&format!(
        "SELECT {SCAN_COLUMNS_LIGHT} FROM image_scans WHERE resource_type = $1 AND deployment_queue_id = ANY($2)"
    ))
    .bind(APPLICATION_MORPH)
    .bind(deployment_ids)
    .fetch_all(db.pool())
    .await?)
}

/// The distinct owners (`resource_type`, `resource_id`) with a scan row on a server of the team
/// (`ResourceScans::currentForTeam()` before it loads the resources).
pub async fn scan_owner_keys_of_team(db: &Db, team_id: i64) -> DbResult<Vec<(String, i64)>> {
    Ok(sqlx::query_as(
        "SELECT DISTINCT resource_type, resource_id FROM image_scans \
         WHERE server_id IN (SELECT id FROM servers WHERE team_id = $1 AND deleted_at IS NULL)",
    )
    .bind(team_id)
    .fetch_all(db.pool())
    .await?)
}

// ---------------------------------------------------------------------------------------------
// application_deployment_queues (what scans read of a deployment)
// ---------------------------------------------------------------------------------------------

/// The columns of a deployment that scans read.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct DeploymentFacts {
    pub id: i64,
    pub application_id: Option<String>,
    pub pull_request_id: i64,
    pub status: Option<String>,
    pub commit: Option<String>,
    pub deployment_uuid: Option<String>,
}

/// `$scan->deploymentQueue` (`select id, pull_request_id, …`).
pub async fn deployment_facts(
    db: &Db,
    deployment_queue_id: i64,
) -> DbResult<Option<DeploymentFacts>> {
    Ok(sqlx::query_as(
        "SELECT id, application_id::text AS application_id, pull_request_id::bigint AS pull_request_id, \
         status::text AS status, commit::text AS commit, deployment_uuid::text AS deployment_uuid \
         FROM application_deployment_queues WHERE id = $1",
    )
    .bind(deployment_queue_id)
    .fetch_optional(db.pool())
    .await?)
}

/// The latest finished deployment of an application that is not a preview: `(id, commit)`.
pub async fn latest_finished_deployment(
    db: &Db,
    application_id: i64,
) -> DbResult<Option<(i64, Option<String>)>> {
    Ok(sqlx::query_as(
        "SELECT id, commit::text FROM application_deployment_queues \
         WHERE application_id = $1 AND pull_request_id = 0 AND status = $2 ORDER BY id DESC LIMIT 1",
    )
    .bind(application_id.to_string())
    .bind(DEPLOYMENT_FINISHED)
    .fetch_optional(db.pool())
    .await?)
}

/// Writes `scan_gate_result` (and, when given, `scan_gate_bypassed_by`) of a deployment.
pub async fn save_gate_result(
    db: &Db,
    deployment_queue_id: i64,
    result: Option<&Value>,
    now: NaiveDateTime,
) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(
        "UPDATE application_deployment_queues SET scan_gate_result = $2, updated_at = $3 WHERE id = $1",
    )
    .bind(deployment_queue_id)
    .bind(result.map(sqlx::types::Json))
    .bind(now)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// `(scan_gate_result, scan_gate_bypassed_by)` of a deployment.
pub async fn gate_result(
    db: &Db,
    deployment_queue_id: i64,
) -> DbResult<Option<(Option<Value>, Option<i64>)>> {
    Ok(sqlx::query_as(
        "SELECT scan_gate_result, scan_gate_bypassed_by FROM application_deployment_queues WHERE id = $1",
    )
    .bind(deployment_queue_id)
    .fetch_optional(db.pool())
    .await?)
}

// ---------------------------------------------------------------------------------------------
// image_sboms
// ---------------------------------------------------------------------------------------------

/// One `image_sboms` row: the package list of one image on one server, shared by the resources
/// that run it.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct ImageSbomRow {
    pub id: i64,
    pub server_id: i64,
    pub image_id: String,
    pub platform: String,
    /// A list of `Package::toArray()` rows.
    pub packages: Value,
    pub packages_truncated: bool,
    pub last_used_at: NaiveDateTime,
}

const SBOM_COLUMNS: &str =
    "id, server_id, image_id, platform, packages, packages_truncated, last_used_at";

pub async fn find_sbom(db: &Db, server_id: i64, image_id: &str) -> DbResult<Option<ImageSbomRow>> {
    Ok(sqlx::query_as(&format!(
        "SELECT {SBOM_COLUMNS} FROM image_sboms WHERE server_id = $1 AND image_id = $2 LIMIT 1"
    ))
    .bind(server_id)
    .bind(image_id)
    .fetch_optional(db.pool())
    .await?)
}

pub async fn find_sbom_by_id(db: &Db, id: i64) -> DbResult<Option<ImageSbomRow>> {
    Ok(sqlx::query_as(&format!(
        "SELECT {SBOM_COLUMNS} FROM image_sboms WHERE id = $1"
    ))
    .bind(id)
    .fetch_optional(db.pool())
    .await?)
}

/// Every SBOM row of a server without its packages (`packages` reads as an empty list).
pub async fn sboms_on_server(db: &Db, server_id: i64) -> DbResult<Vec<ImageSbomRow>> {
    Ok(sqlx::query_as(
        "SELECT id, server_id, image_id, platform, '[]'::jsonb AS packages, packages_truncated, last_used_at \
         FROM image_sboms WHERE server_id = $1",
    )
    .bind(server_id)
    .fetch_all(db.pool())
    .await?)
}

/// What a new SBOM run stores.
#[derive(Debug, Clone)]
pub struct SbomValues<'a> {
    pub platform: &'a str,
    pub packages: &'a Value,
    pub packages_truncated: bool,
    pub last_used_at: NaiveDateTime,
}

/// `ImageSbom::query()->create([...])`. A row that another run created at the same time is
/// updated instead (the table is unique on server and image ID).
pub async fn create_sbom(
    db: &Db,
    server_id: i64,
    image_id: &str,
    v: &SbomValues<'_>,
) -> DbResult<ImageSbomRow> {
    db.ensure_writable()?;
    Ok(sqlx::query_as(&format!(
        "INSERT INTO image_sboms (server_id, image_id, platform, packages, packages_truncated, last_used_at) \
         VALUES ($1, $2, $3, $4, $5, $6) \
         ON CONFLICT (server_id, image_id) DO UPDATE SET platform = EXCLUDED.platform, packages = EXCLUDED.packages, \
         packages_truncated = EXCLUDED.packages_truncated, last_used_at = EXCLUDED.last_used_at \
         RETURNING {SBOM_COLUMNS}"
    ))
    .bind(server_id)
    .bind(image_id)
    .bind(v.platform)
    .bind(sqlx::types::Json(v.packages))
    .bind(v.packages_truncated)
    .bind(v.last_used_at)
    .fetch_one(db.pool())
    .await?)
}

/// `$sbom->forceFill($values)->save()` after its file was created once more.
pub async fn update_sbom(db: &Db, id: i64, v: &SbomValues<'_>) -> DbResult<Option<ImageSbomRow>> {
    db.ensure_writable()?;
    Ok(sqlx::query_as(&format!(
        "UPDATE image_sboms SET platform = $2, packages = $3, packages_truncated = $4, last_used_at = $5 \
         WHERE id = $1 RETURNING {SBOM_COLUMNS}"
    ))
    .bind(id)
    .bind(v.platform)
    .bind(sqlx::types::Json(v.packages))
    .bind(v.packages_truncated)
    .bind(v.last_used_at)
    .fetch_optional(db.pool())
    .await?)
}

/// `$sbom->forceFill(['last_used_at' => now()])->save()`.
pub async fn touch_sbom(db: &Db, id: i64, now: NaiveDateTime) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query("UPDATE image_sboms SET last_used_at = $2 WHERE id = $1")
        .bind(id)
        .bind(now)
        .execute(db.pool())
        .await?;
    Ok(())
}

pub async fn delete_sboms(db: &Db, ids: &[i64]) -> DbResult<()> {
    if ids.is_empty() {
        return Ok(());
    }
    db.ensure_writable()?;
    sqlx::query("DELETE FROM image_sboms WHERE id = ANY($1)")
        .bind(ids)
        .execute(db.pool())
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// image_scan_policies
// ---------------------------------------------------------------------------------------------

/// One `image_scan_policies` row: the scan rules at one level. A null field inherits. The
/// instance row has `scope_type` null and concrete values.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct ImageScanPolicyRow {
    pub id: i64,
    pub uuid: String,
    pub scope_type: Option<String>,
    pub scope_id: Option<i64>,
    /// `ImageScanGateMode`: off, on, strict.
    pub gate_mode: Option<String>,
    /// `ImageScanOnError`: deploy, block.
    pub on_scan_error: Option<String>,
    pub count_medium: Option<bool>,
    pub created_at: Option<NaiveDateTime>,
    pub updated_at: Option<NaiveDateTime>,
}

const POLICY_COLUMNS: &str = "id, uuid, scope_type, scope_id, gate_mode, on_scan_error, count_medium, created_at, updated_at";

/// `ImageScanPolicyResolver::instanceRow()`.
pub async fn instance_policy(db: &Db) -> DbResult<Option<ImageScanPolicyRow>> {
    Ok(sqlx::query_as(&format!(
        "SELECT {POLICY_COLUMNS} FROM image_scan_policies WHERE scope_type IS NULL ORDER BY id LIMIT 1"
    ))
    .fetch_optional(db.pool())
    .await?)
}

/// `ImageScanPolicyResolver::scopeRow($scope)`.
pub async fn scope_policy(
    db: &Db,
    scope_type: &str,
    scope_id: i64,
) -> DbResult<Option<ImageScanPolicyRow>> {
    Ok(sqlx::query_as(&format!(
        "SELECT {POLICY_COLUMNS} FROM image_scan_policies WHERE scope_type = $1 AND scope_id = $2 LIMIT 1"
    ))
    .bind(scope_type)
    .bind(scope_id)
    .fetch_optional(db.pool())
    .await?)
}

/// The rule rows of many scopes with one query (`countMediumForMany()`).
pub async fn scope_policies(
    db: &Db,
    scopes: &[(String, i64)],
) -> DbResult<Vec<ImageScanPolicyRow>> {
    if scopes.is_empty() {
        return Ok(Vec::new());
    }
    let types: Vec<String> = scopes.iter().map(|(t, _)| t.clone()).collect();
    let ids: Vec<i64> = scopes.iter().map(|(_, id)| *id).collect();
    Ok(sqlx::query_as(&format!(
        "SELECT {POLICY_COLUMNS} FROM image_scan_policies WHERE (scope_type, scope_id) IN \
         (SELECT * FROM unnest($1::text[], $2::bigint[]))"
    ))
    .bind(&types)
    .bind(&ids)
    .fetch_all(db.pool())
    .await?)
}

/// Saves the rules of one level: the instance row for `scope` `None`, else the row of the scope
/// (`ImageScanPolicyResolver::forScope($scope)->fill([...])->save()`).
pub async fn save_policy(
    db: &Db,
    scope: Option<(&str, i64)>,
    gate_mode: Option<&str>,
    on_scan_error: Option<&str>,
    count_medium: Option<bool>,
    now: NaiveDateTime,
) -> DbResult<ImageScanPolicyRow> {
    db.ensure_writable()?;
    let existing = match scope {
        None => instance_policy(db).await?,
        Some((scope_type, scope_id)) => scope_policy(db, scope_type, scope_id).await?,
    };
    let row = match existing {
        Some(row) => {
            sqlx::query_as(&format!(
                "UPDATE image_scan_policies SET gate_mode = $2, on_scan_error = $3, count_medium = $4, updated_at = $5 \
                 WHERE id = $1 RETURNING {POLICY_COLUMNS}"
            ))
            .bind(row.id)
            .bind(gate_mode)
            .bind(on_scan_error)
            .bind(count_medium)
            .bind(now)
            .fetch_one(db.pool())
            .await?
        }
        None => {
            sqlx::query_as(&format!(
                "INSERT INTO image_scan_policies (uuid, scope_type, scope_id, gate_mode, on_scan_error, count_medium, created_at, updated_at) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $7) RETURNING {POLICY_COLUMNS}"
            ))
            .bind(new_public_id())
            .bind(scope.map(|(t, _)| t))
            .bind(scope.map(|(_, id)| id))
            .bind(gate_mode)
            .bind(on_scan_error)
            .bind(count_medium)
            .bind(now)
            .fetch_one(db.pool())
            .await?
        }
    };
    Ok(row)
}

// ---------------------------------------------------------------------------------------------
// image_scan_ignores
// ---------------------------------------------------------------------------------------------

/// One `image_scan_ignores` row: a vulnerability or package a team chose to ignore, for one
/// resource or (resource null) all team resources.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct ImageScanIgnoreRow {
    pub id: i64,
    pub uuid: String,
    pub team_id: i64,
    pub resource_type: Option<String>,
    pub resource_id: Option<i64>,
    pub vulnerability_id: Option<String>,
    pub package: Option<String>,
    pub reason: String,
    pub expires_at: Option<NaiveDateTime>,
    pub created_by: Option<i64>,
    pub created_at: Option<NaiveDateTime>,
    pub updated_at: Option<NaiveDateTime>,
}

const IGNORE_COLUMNS: &str = "id, uuid, team_id, resource_type, resource_id, vulnerability_id, package, \
    reason, expires_at, created_by, created_at, updated_at";

/// `IgnoreMatcher::forResource()`: the active ignores of the team that apply to the resource
/// (team-wide ones and its own).
pub async fn active_ignores_for(
    db: &Db,
    team_id: i64,
    resource_type: &str,
    resource_id: i64,
    now: NaiveDateTime,
) -> DbResult<Vec<ImageScanIgnoreRow>> {
    Ok(sqlx::query_as(&format!(
        "SELECT {IGNORE_COLUMNS} FROM image_scan_ignores \
         WHERE (expires_at IS NULL OR expires_at > $4) AND team_id = $1 \
         AND (resource_type IS NULL OR (resource_type = $2 AND resource_id = $3))"
    ))
    .bind(team_id)
    .bind(resource_type)
    .bind(resource_id)
    .bind(now)
    .fetch_all(db.pool())
    .await?)
}

/// Every active ignore of a team (`ImageScanIgnore::query()->active()->where('team_id', …)`).
pub async fn active_ignores_of_team(
    db: &Db,
    team_id: i64,
    now: NaiveDateTime,
) -> DbResult<Vec<ImageScanIgnoreRow>> {
    Ok(sqlx::query_as(&format!(
        "SELECT {IGNORE_COLUMNS} FROM image_scan_ignores \
         WHERE (expires_at IS NULL OR expires_at > $2) AND team_id = $1"
    ))
    .bind(team_id)
    .bind(now)
    .fetch_all(db.pool())
    .await?)
}

/// Every ignore of a team, also the ended ones, in insertion order.
pub async fn ignores_of_team(db: &Db, team_id: i64) -> DbResult<Vec<ImageScanIgnoreRow>> {
    Ok(sqlx::query_as(&format!(
        "SELECT {IGNORE_COLUMNS} FROM image_scan_ignores WHERE team_id = $1 ORDER BY id"
    ))
    .bind(team_id)
    .fetch_all(db.pool())
    .await?)
}

pub async fn find_ignore(
    db: &Db,
    team_id: i64,
    uuid: &str,
) -> DbResult<Option<ImageScanIgnoreRow>> {
    Ok(sqlx::query_as(&format!(
        "SELECT {IGNORE_COLUMNS} FROM image_scan_ignores WHERE team_id = $1 AND uuid = $2 LIMIT 1"
    ))
    .bind(team_id)
    .bind(uuid)
    .fetch_optional(db.pool())
    .await?)
}

/// A new ignore.
#[derive(Debug, Clone)]
pub struct NewIgnore<'a> {
    pub team_id: i64,
    /// `None` for all resources of the team.
    pub resource: Option<(&'a str, i64)>,
    pub vulnerability_id: Option<&'a str>,
    pub package: Option<&'a str>,
    pub reason: &'a str,
    pub expires_at: Option<NaiveDateTime>,
    pub created_by: Option<i64>,
}

/// `ImageScanIgnore::create([...])`.
pub async fn create_ignore(
    db: &Db,
    n: &NewIgnore<'_>,
    now: NaiveDateTime,
) -> DbResult<ImageScanIgnoreRow> {
    db.ensure_writable()?;
    Ok(sqlx::query_as(&format!(
        "INSERT INTO image_scan_ignores (uuid, team_id, resource_type, resource_id, vulnerability_id, package, reason, \
         expires_at, created_by, created_at, updated_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $10) \
         RETURNING {IGNORE_COLUMNS}"
    ))
    .bind(new_public_id())
    .bind(n.team_id)
    .bind(n.resource.map(|(t, _)| t))
    .bind(n.resource.map(|(_, id)| id))
    .bind(n.vulnerability_id)
    .bind(n.package)
    .bind(n.reason)
    .bind(n.expires_at)
    .bind(n.created_by)
    .bind(now)
    .fetch_one(db.pool())
    .await?)
}

pub async fn delete_ignore(db: &Db, id: i64) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query("DELETE FROM image_scan_ignores WHERE id = $1")
        .bind(id)
        .execute(db.pool())
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn scan(candidates: Option<Value>) -> ImageScanRow {
        ImageScanRow {
            id: 0,
            uuid: String::new(),
            server_id: 1,
            image_sbom_id: None,
            image_id: String::new(),
            repo_digest: None,
            resource_type: APPLICATION_MORPH.to_owned(),
            resource_id: 7,
            image_ref: "nginx:1.27".to_owned(),
            deployment_queue_id: None,
            status: STATUS_FAILED.to_owned(),
            error_code: None,
            error_context: None,
            summary: None,
            findings: None,
            candidates,
            base_image_ref: None,
            builds_image: false,
            is_baseline: true,
            grype_db_built_at: None,
            scanned_at: None,
            created_at: None,
            updated_at: None,
        }
    }

    #[test]
    fn candidate_entries_reads_both_shapes() {
        assert!(scan(None).candidate_entries().is_empty());
        let bare = scan(Some(json!([{"ref": "nginx:1.28"}, "junk"])));
        assert_eq!(bare.candidate_entries(), vec![json!({"ref": "nginx:1.28"})]);
        assert_eq!(bare.candidate_status(), None);
        let keyed = scan(Some(
            json!({"status": "ok", "entries": [{"ref": "nginx:1.28"}]}),
        ));
        assert_eq!(keyed.candidate_entries().len(), 1);
        assert_eq!(keyed.candidate_status(), Some("ok"));
        assert!(
            scan(Some(json!({"status": 3, "entries": "x"})))
                .candidate_entries()
                .is_empty()
        );
    }

    #[test]
    fn unsaved_scan_does_not_exist() {
        let s = scan(None);
        assert!(!s.exists());
        assert_eq!(s.owner_key(), format!("{APPLICATION_MORPH}:7"));
    }

    #[test]
    fn knows_the_resource_tables() {
        assert_eq!(
            resource_table(APPLICATION_MORPH),
            Some(("applications", APPLICATION_MORPH))
        );
        assert_eq!(
            resource_table(r"App\Models\StandaloneRedis").map(|t| t.0),
            Some("standalone_redis")
        );
        assert_eq!(resource_table(r"App\Models\Server"), None);
    }

    #[test]
    fn refuses_unknown_scan_columns() {
        assert!(check_scan_columns(&[("status", ScanValue::Text(Some("failed".into())))]).is_ok());
        assert!(check_scan_columns(&[("uuid", ScanValue::Text(None))]).is_err());
    }
}
