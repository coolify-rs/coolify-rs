//! Queries of the scan view models (`SecurityStatus`, `SecurityActions`, `SecurityCard`,
//! `TeamVulnerabilities`) and of the vulnerability API: the resources of a team, where they are
//! (project, environment, server), the scans of a forced rebuild, the scans of one deployment,
//! and the search in the stored findings.

use chrono::NaiveDateTime;
use serde_json::Value;

use super::{
    APPLICATION_MORPH, ImageScanRow, OwnerTuple, SCAN_COLUMNS, SERVICE_MORPH, ScanOwner,
    owner_from, owner_select,
};
use crate::databases::Kind;
use crate::{Db, DbResult};

/// `TeamVulnerabilities::resources($teamId)`: the applications, services and standalone
/// databases of a team (not deleted), in that order, each table by id.
pub async fn team_resources(db: &Db, team_id: i64) -> DbResult<Vec<ScanOwner>> {
    let mut tables: Vec<(&'static str, &'static str)> = vec![
        ("applications", APPLICATION_MORPH),
        ("services", SERVICE_MORPH),
    ];
    tables.extend(Kind::ALL.into_iter().map(|k| (k.table(), k.morph())));
    let mut out = Vec::new();
    for (table, morph) in tables {
        let rows: Vec<OwnerTuple> = sqlx::query_as(&format!(
            "{} WHERE t.deleted_at IS NULL AND p.team_id = $1 ORDER BY t.id",
            owner_select(table)
        ))
        .bind(team_id)
        .fetch_all(db.pool())
        .await?;
        for r in rows {
            out.push(owner_from(morph, r)?);
        }
    }
    Ok(out)
}

/// The resources of one table with the given ids in a team (`$class::query()->whereIn('id',
/// $ids)->whereRelation('environment.project', 'team_id', $teamId)`).
pub async fn team_resources_by_ids(
    db: &Db,
    morph: &str,
    ids: &[i64],
    team_id: i64,
) -> DbResult<Vec<ScanOwner>> {
    let Some((table, morph)) = super::resource_table(morph) else {
        return Ok(Vec::new());
    };
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let rows: Vec<OwnerTuple> = sqlx::query_as(&format!(
        "{} WHERE t.deleted_at IS NULL AND t.id = ANY($1) AND p.team_id = $2 ORDER BY t.id",
        owner_select(table)
    ))
    .bind(ids)
    .bind(team_id)
    .fetch_all(db.pool())
    .await?;
    rows.into_iter().map(|r| owner_from(morph, r)).collect()
}

/// The environment and project of a resource page.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Place {
    pub environment_id: i64,
    pub environment_uuid: String,
    pub environment_name: String,
    pub project_id: i64,
    pub project_uuid: String,
    pub project_name: String,
}

/// `$resource->environment` and `$resource->environment->project` of many resources, by
/// environment id.
pub async fn places(db: &Db, environment_ids: &[i64]) -> DbResult<Vec<Place>> {
    if environment_ids.is_empty() {
        return Ok(Vec::new());
    }
    Ok(sqlx::query_as(
        "SELECT e.id AS environment_id, e.uuid::text AS environment_uuid, e.name::text AS environment_name, \
         p.id AS project_id, p.uuid::text AS project_uuid, p.name::text AS project_name \
         FROM environments e JOIN projects p ON p.id = e.project_id WHERE e.id = ANY($1)",
    )
    .bind(environment_ids)
    .fetch_all(db.pool())
    .await?)
}

/// What the view models read of a server: `uuid`, `name`, and its image scanning switch.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct ServerBrief {
    pub id: i64,
    pub uuid: String,
    pub name: String,
    pub team_id: i64,
    /// `$server->settings?->is_image_scan_enabled`; `None` without a settings row.
    pub is_image_scan_enabled: Option<bool>,
}

/// The servers with the given ids (deleted ones too, like a relation without the scope is not).
pub async fn servers_brief(db: &Db, ids: &[i64]) -> DbResult<Vec<ServerBrief>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    Ok(sqlx::query_as(
        "SELECT s.id, s.uuid::text AS uuid, s.name::text AS name, s.team_id::bigint AS team_id, \
         ss.is_image_scan_enabled FROM servers s LEFT JOIN server_settings ss ON ss.server_id = s.id \
         WHERE s.id = ANY($1) AND s.deleted_at IS NULL",
    )
    .bind(ids)
    .fetch_all(db.pool())
    .await?)
}

/// The findings of the scans that `SecurityActions::idsBeforeForcedRebuild()` compares: the
/// latest completed scan of the owner, up to `scan_id`, of a rebuild without cache (a
/// deployment that is not a preview with `force_rebuild`), and the completed scan before it
/// that is not a preview. `None` when one of them is missing.
pub async fn forced_rebuild_findings(
    db: &Db,
    resource_type: &str,
    resource_id: i64,
    scan_id: i64,
) -> DbResult<Option<(Option<Value>, Option<Value>)>> {
    let rebuilt: Option<(i64, Option<Value>)> = sqlx::query_as(
        "SELECT s.id, s.findings FROM image_scans s \
         WHERE s.resource_type = $1 AND s.resource_id = $2 AND s.status = 'completed' AND s.id <= $3 \
         AND EXISTS (SELECT 1 FROM application_deployment_queues q WHERE q.id = s.deployment_queue_id \
         AND q.pull_request_id = 0 AND q.force_rebuild = true) \
         ORDER BY s.id DESC LIMIT 1",
    )
    .bind(resource_type)
    .bind(resource_id)
    .bind(scan_id)
    .fetch_optional(db.pool())
    .await?;
    let Some((rebuilt_id, rebuilt_findings)) = rebuilt else {
        return Ok(None);
    };
    let previous: Option<(Option<Value>,)> = sqlx::query_as(
        "SELECT s.findings FROM image_scans s \
         WHERE s.resource_type = $1 AND s.resource_id = $2 AND s.status = 'completed' AND s.id < $3 \
         AND (s.deployment_queue_id IS NULL OR EXISTS (SELECT 1 FROM application_deployment_queues q \
         WHERE q.id = s.deployment_queue_id AND q.pull_request_id = 0)) \
         ORDER BY s.id DESC LIMIT 1",
    )
    .bind(resource_type)
    .bind(resource_id)
    .bind(rebuilt_id)
    .fetch_optional(db.pool())
    .await?;
    Ok(previous.map(|(findings,)| (rebuilt_findings, findings)))
}

/// The completed scans of an application in one deployment, by id (`SecurityCard::gateAction()`).
pub async fn deployment_completed_scans(
    db: &Db,
    application_id: i64,
    deployment_queue_id: i64,
) -> DbResult<Vec<ImageScanRow>> {
    Ok(sqlx::query_as(&format!(
        "SELECT {SCAN_COLUMNS} FROM image_scans WHERE resource_type = $1 AND resource_id = $2 \
         AND deployment_queue_id = $3 AND status = 'completed' ORDER BY id"
    ))
    .bind(APPLICATION_MORPH)
    .bind(application_id)
    .bind(deployment_queue_id)
    .fetch_all(db.pool())
    .await?)
}

/// The distinct owners (`resource_type`, `resource_id`) with a completed scan on a server of the
/// team (`TeamVulnerabilities::updateNowResources()`).
pub async fn completed_owner_keys_of_team(db: &Db, team_id: i64) -> DbResult<Vec<(String, i64)>> {
    Ok(sqlx::query_as(
        "SELECT DISTINCT resource_type, resource_id FROM image_scans \
         WHERE server_id IN (SELECT id FROM servers WHERE team_id = $1) AND status = 'completed'",
    )
    .bind(team_id)
    .fetch_all(db.pool())
    .await?)
}

/// `TeamVulnerabilities::search()`: the findings of the scans among `ids` whose stored findings
/// contain the LIKE `pattern` (escape character `!`), the latest scan first, at most `limit`.
pub async fn search_findings(
    db: &Db,
    ids: &[i64],
    pattern: &str,
    limit: i64,
) -> DbResult<Vec<(i64, Option<Value>)>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    Ok(sqlx::query_as(
        "SELECT id, findings FROM image_scans WHERE id = ANY($1) \
         AND LOWER(CAST(findings AS TEXT)) LIKE $2 ESCAPE '!' ORDER BY id DESC LIMIT $3",
    )
    .bind(ids)
    .bind(pattern)
    .bind(limit)
    .fetch_all(db.pool())
    .await?)
}

/// The servers of a team, for `TeamVulnerabilities::serversNotScanning()`.
pub async fn team_servers(db: &Db, team_id: i64) -> DbResult<Vec<ServerBrief>> {
    Ok(sqlx::query_as(
        "SELECT s.id, s.uuid::text AS uuid, s.name::text AS name, s.team_id::bigint AS team_id, \
         ss.is_image_scan_enabled FROM servers s LEFT JOIN server_settings ss ON ss.server_id = s.id \
         WHERE s.team_id = $1 AND s.deleted_at IS NULL ORDER BY s.id",
    )
    .bind(team_id)
    .fetch_all(db.pool())
    .await?)
}

/// `$server->settings->update($limits)` of the scanner limits (`ServerImageScanningController`).
/// Only the given columns change; `updated_at` is written.
pub async fn update_scanner_limits(
    db: &Db,
    server_id: i64,
    memory_mb: Option<i64>,
    syft_timeout: Option<i64>,
    grype_timeout: Option<i64>,
    cpus: Option<f64>,
    now: NaiveDateTime,
) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(
        "UPDATE server_settings SET \
         image_scan_memory_mb = COALESCE($2, image_scan_memory_mb), \
         image_scan_syft_timeout = COALESCE($3, image_scan_syft_timeout), \
         image_scan_grype_timeout = COALESCE($4, image_scan_grype_timeout), \
         image_scan_cpus = COALESCE($5::numeric, image_scan_cpus), \
         updated_at = $6 WHERE server_id = $1",
    )
    .bind(server_id)
    .bind(memory_mb.map(|v| v as i32))
    .bind(syft_timeout.map(|v| v as i32))
    .bind(grype_timeout.map(|v| v as i32))
    .bind(cpus)
    .bind(now)
    .execute(db.pool())
    .await?;
    Ok(())
}
