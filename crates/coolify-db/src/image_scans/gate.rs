//! Queries of the deploy gate (`SecurityGate`, `DeploymentImageScan`): what the gate reads of the
//! deployments and the scans of one application, and the few rows it writes.
//!
//! `scan_gate_result` is `jsonb`. Laravel's `where('scan_gate_result->blocked', true)` is
//! `("scan_gate_result"->'blocked')::jsonb = 'true'::jsonb` on PostgreSQL ([`BLOCKED`]).

use chrono::NaiveDateTime;
use serde_json::Value;

use super::{DEPLOYMENT_FINISHED, ImageScanRow, SCAN_COLUMNS, STATUS_COMPLETED, prefixed};
use crate::{Db, DbResult};

/// `where('scan_gate_result->blocked', true)` on the deployment `q`.
const BLOCKED: &str = "(q.scan_gate_result->'blocked') = 'true'::jsonb";

/// The columns of `application_deployment_queues` that `DeploymentImageScan` reads of its own
/// deployment (`$this->deployment`).
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct GateDeployment {
    pub id: i64,
    /// `application_id` is a string column.
    pub application_id: Option<String>,
    pub deployment_uuid: Option<String>,
    /// The main deployment that queued this one for an additional server.
    pub parent_deployment_uuid: Option<String>,
    /// 0 unless the deployment is a preview.
    pub pull_request_id: i64,
    pub scan_gate_result: Option<Value>,
    /// The user who chose "Deploy anyway".
    pub scan_gate_bypassed_by: Option<i64>,
}

/// The deployment of a `DeploymentImageScan`.
pub async fn deployment(db: &Db, deployment_queue_id: i64) -> DbResult<Option<GateDeployment>> {
    Ok(sqlx::query_as(
        "SELECT id, application_id::text AS application_id, deployment_uuid::text AS deployment_uuid, \
         parent_deployment_uuid::text AS parent_deployment_uuid, pull_request_id::bigint AS pull_request_id, \
         scan_gate_result, scan_gate_bypassed_by::bigint AS scan_gate_bypassed_by \
         FROM application_deployment_queues WHERE id = $1",
    )
    .bind(deployment_queue_id)
    .fetch_optional(db.pool())
    .await?)
}

/// `ApplicationDeploymentQueue::query()->whereKey($id)->value('status')`.
pub async fn deployment_status(db: &Db, deployment_queue_id: i64) -> DbResult<Option<String>> {
    let status: Option<Option<String>> =
        sqlx::query_scalar("SELECT status::text FROM application_deployment_queues WHERE id = $1")
            .bind(deployment_queue_id)
            .fetch_optional(db.pool())
            .await?;
    Ok(status.flatten())
}

/// `(scan_gate_result, scan_gate_bypassed_by)` of the main deployment that queued a deployment:
/// `->where('deployment_uuid', $parent)->where('application_id', $applicationId)->value(…)`.
pub async fn parent_gate(
    db: &Db,
    parent_deployment_uuid: &str,
    application_id: &str,
) -> DbResult<Option<(Option<Value>, Option<i64>)>> {
    Ok(sqlx::query_as(
        "SELECT scan_gate_result, scan_gate_bypassed_by::bigint FROM application_deployment_queues \
         WHERE deployment_uuid = $1 AND application_id = $2 LIMIT 1",
    )
    .bind(parent_deployment_uuid)
    .bind(application_id)
    .fetch_optional(db.pool())
    .await?)
}

/// The query of `DeploymentImageScan::runningImageScan()`: the completed scans of the resource
/// whose deployment finished and is not a preview, the latest deployment first.
fn running_scans_sql(columns: &str) -> String {
    format!(
        "SELECT {columns} FROM image_scans s \
         WHERE s.resource_type = $1 AND s.resource_id = $2 AND s.status = '{STATUS_COMPLETED}' \
         AND EXISTS (SELECT 1 FROM application_deployment_queues q WHERE q.id = s.deployment_queue_id \
         AND q.pull_request_id = 0 AND q.status = '{DEPLOYMENT_FINISHED}') \
         ORDER BY s.deployment_queue_id DESC, s.scanned_at DESC, s.id DESC"
    )
}

/// `runningImageScan()` of an application that is not a Compose application: the first scan of
/// the query.
pub async fn running_scan(
    db: &Db,
    resource_type: &str,
    resource_id: i64,
) -> DbResult<Option<ImageScanRow>> {
    Ok(sqlx::query_as(&format!(
        "{} LIMIT 1",
        running_scans_sql(&prefixed(SCAN_COLUMNS))
    ))
    .bind(resource_type)
    .bind(resource_id)
    .fetch_optional(db.pool())
    .await?)
}

/// `runningImageScan()` of a Compose application: `(id, image_ref)` of every scan of the query,
/// in its order. The caller picks the first one with the image name it looks for.
pub async fn running_scan_refs(
    db: &Db,
    resource_type: &str,
    resource_id: i64,
) -> DbResult<Vec<(i64, String)>> {
    Ok(
        sqlx::query_as(&running_scans_sql("s.id, s.image_ref::text"))
            .bind(resource_type)
            .bind(resource_id)
            .fetch_all(db.pool())
            .await?,
    )
}

/// `hasEarlierFinishedDeployment()`: a finished deployment of the application that is not a
/// preview and not this deployment exists.
pub async fn has_earlier_finished_deployment(
    db: &Db,
    application_id: i64,
    deployment_queue_id: i64,
) -> DbResult<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM application_deployment_queues \
         WHERE application_id = $1 AND pull_request_id = 0 AND status = $2 AND id != $3)",
    )
    .bind(application_id.to_string())
    .bind(DEPLOYMENT_FINISHED)
    .bind(deployment_queue_id)
    .fetch_one(db.pool())
    .await?)
}

/// The query of `latestEarlierImageScan()`: the 50 newest completed scans of the resource that
/// have no deployment or a deployment that is not a preview and not this one, without the scans
/// of deployments that the gate blocked and without the images in `excluded_image_ids`.
pub async fn earlier_scans(
    db: &Db,
    resource_type: &str,
    resource_id: i64,
    deployment_queue_id: i64,
    excluded_image_ids: &[String],
) -> DbResult<Vec<ImageScanRow>> {
    let columns = prefixed(SCAN_COLUMNS);
    Ok(sqlx::query_as(&format!(
        "SELECT {columns} FROM image_scans s \
         WHERE s.resource_type = $1 AND s.resource_id = $2 AND s.status = '{STATUS_COMPLETED}' \
         AND (s.deployment_queue_id IS NULL OR EXISTS (SELECT 1 FROM application_deployment_queues q \
         WHERE q.id = s.deployment_queue_id AND q.pull_request_id = 0 AND q.id != $3)) \
         AND NOT EXISTS (SELECT 1 FROM application_deployment_queues q \
         WHERE q.id = s.deployment_queue_id AND {BLOCKED}) \
         AND s.image_id != ALL($4) \
         ORDER BY s.scanned_at DESC, s.id DESC LIMIT 50"
    ))
    .bind(resource_type)
    .bind(resource_id)
    .bind(deployment_queue_id)
    .bind(excluded_image_ids)
    .fetch_all(db.pool())
    .await?)
}

/// `blockedImageIds()`: the images that the gate blocked in an earlier deployment of the
/// application that is not a preview: the image ID stored with the result, or a scan linked to
/// the blocked deployment. Each ID once; empty strings and values that are not strings are left
/// out.
pub async fn blocked_image_ids(
    db: &Db,
    resource_type: &str,
    application_id: i64,
    deployment_queue_id: i64,
) -> DbResult<Vec<String>> {
    let stored: Vec<Option<String>> = sqlx::query_scalar(&format!(
        "SELECT CASE WHEN jsonb_typeof(q.scan_gate_result->'image_id') = 'string' \
         THEN q.scan_gate_result->>'image_id' END \
         FROM application_deployment_queues q \
         WHERE q.application_id = $1 AND q.pull_request_id = 0 AND q.id != $2 AND {BLOCKED} ORDER BY q.id"
    ))
    .bind(application_id.to_string())
    .bind(deployment_queue_id)
    .fetch_all(db.pool())
    .await?;
    let scanned: Vec<Option<String>> = sqlx::query_scalar(&format!(
        "SELECT s.image_id::text FROM image_scans s \
         WHERE s.resource_type = $1 AND s.resource_id = $2 AND s.image_id IS NOT NULL \
         AND EXISTS (SELECT 1 FROM application_deployment_queues q WHERE q.id = s.deployment_queue_id \
         AND q.pull_request_id = 0 AND q.id != $3 AND {BLOCKED}) ORDER BY s.id"
    ))
    .bind(resource_type)
    .bind(application_id)
    .bind(deployment_queue_id)
    .fetch_all(db.pool())
    .await?;
    let mut ids: Vec<String> = Vec::new();
    for id in stored.into_iter().chain(scanned).flatten() {
        if !id.is_empty() && !ids.contains(&id) {
            ids.push(id);
        }
    }
    Ok(ids)
}

/// `rollbackCommitRanBefore()`: a finished deployment of the application ran the commit, for the
/// same pull request (0 for the application itself).
pub async fn commit_ran_before(
    db: &Db,
    application_id: i64,
    commit: &str,
    pull_request_id: i64,
) -> DbResult<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM application_deployment_queues \
         WHERE application_id = $1 AND status = $2 AND commit = $3 AND pull_request_id = $4)",
    )
    .bind(application_id.to_string())
    .bind(DEPLOYMENT_FINISHED)
    .bind(commit)
    .bind(pull_request_id)
    .fetch_one(db.pool())
    .await?)
}

/// `imageRanBefore()`: a finished deployment of the resource scanned the image.
pub async fn image_ran_before(
    db: &Db,
    resource_type: &str,
    resource_id: i64,
    image_id: &str,
) -> DbResult<bool> {
    Ok(sqlx::query_scalar(&format!(
        "SELECT EXISTS (SELECT 1 FROM image_scans s \
         WHERE s.resource_type = $1 AND s.resource_id = $2 AND s.image_id = $3 \
         AND EXISTS (SELECT 1 FROM application_deployment_queues q \
         WHERE q.id = s.deployment_queue_id AND q.status = '{DEPLOYMENT_FINISHED}'))"
    ))
    .bind(resource_type)
    .bind(resource_id)
    .bind(image_id)
    .fetch_one(db.pool())
    .await?)
}

/// `linkReusedScans()`: the scans of the resource with the given ids join the deployment, unless
/// they belong to it already. Returns the number of rows that changed.
pub async fn link_scans(
    db: &Db,
    resource_type: &str,
    resource_id: i64,
    scan_ids: &[i64],
    deployment_queue_id: i64,
    now: NaiveDateTime,
) -> DbResult<u64> {
    if scan_ids.is_empty() {
        return Ok(0);
    }
    db.ensure_writable()?;
    Ok(sqlx::query(
        "UPDATE image_scans SET deployment_queue_id = $4, updated_at = $5 \
         WHERE resource_type = $1 AND resource_id = $2 AND id = ANY($3) \
         AND (deployment_queue_id IS NULL OR deployment_queue_id != $4)",
    )
    .bind(resource_type)
    .bind(resource_id)
    .bind(scan_ids)
    .bind(deployment_queue_id)
    .bind(now)
    .execute(db.pool())
    .await?
    .rows_affected())
}

/// `$server->name` of a server that still exists.
pub async fn server_name(db: &Db, server_id: i64) -> DbResult<Option<String>> {
    let name: Option<Option<String>> =
        sqlx::query_scalar("SELECT name::text FROM servers WHERE id = $1 AND deleted_at IS NULL")
            .bind(server_id)
            .fetch_optional(db.pool())
            .await?;
    Ok(name.flatten())
}

/// `Application::forgetStaleComposeImageOverrides()`: writes `docker_compose_image_overrides`
/// (a `json` column) of an application: the JSON text as PHP's `json_encode` writes it, or null.
pub async fn set_compose_image_overrides(
    db: &Db,
    application_id: i64,
    overrides_json: Option<&str>,
    now: NaiveDateTime,
) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(
        "UPDATE applications SET docker_compose_image_overrides = $2::json, updated_at = $3 WHERE id = $1",
    )
    .bind(application_id)
    .bind(overrides_json)
    .bind(now)
    .execute(db.pool())
    .await?;
    Ok(())
}
