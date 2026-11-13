//! Queries of the scan jobs (`ScanResourceImagesJob`, `ImageRescanJob`).

use chrono::NaiveDateTime;

use super::{
    APPLICATION_MORPH, IGNORE_COLUMNS, ImageScanIgnoreRow, OwnerTuple, SERVICE_APPLICATION_MORPH,
    SERVICE_DATABASE_MORPH, SERVICE_MORPH, ScanOwner, owner_from, owner_select,
};
use crate::databases::{Kind, STANDALONE_DOCKER_MORPH, SWARM_DOCKER_MORPH};
use crate::{Db, DbResult};

// ---------------------------------------------------------------------------------------------
// ScanResourceImagesJob
// ---------------------------------------------------------------------------------------------

/// The columns of a deployment that `ScanResourceImagesJob` reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::FromRow)]
pub struct JobDeployment {
    pub id: i64,
    /// 0 unless the deployment is a preview.
    pub pull_request_id: i64,
    /// `$deployment->server` is `Server::find($deployment->server_id)`.
    pub server_id: Option<i64>,
}

/// `ScanResourceImagesJob::deployment()`:
/// `ApplicationDeploymentQueue::query()->whereKey($id)->where('application_id', (string) $applicationId)->first()`.
pub async fn job_deployment(
    db: &Db,
    deployment_queue_id: i64,
    application_id: i64,
) -> DbResult<Option<JobDeployment>> {
    Ok(sqlx::query_as(
        "SELECT id, pull_request_id::bigint AS pull_request_id, server_id::bigint AS server_id \
         FROM application_deployment_queues WHERE id = $1 AND application_id = $2 LIMIT 1",
    )
    .bind(deployment_queue_id)
    .bind(application_id.to_string())
    .fetch_optional(db.pool())
    .await?)
}

/// `ScanResourceImagesJob::imageRefsFor($application)` before `unique()->take()`: the image refs
/// of the completed scans of a resource, the latest scan first.
pub async fn completed_image_refs(
    db: &Db,
    resource_type: &str,
    resource_id: i64,
) -> DbResult<Vec<String>> {
    Ok(sqlx::query_scalar(
        "SELECT image_ref FROM image_scans WHERE resource_type = $1 AND resource_id = $2 \
         AND status = 'completed' ORDER BY scanned_at DESC, id DESC",
    )
    .bind(resource_type)
    .bind(resource_id)
    .fetch_all(db.pool())
    .await?)
}

/// `$service->applications()->count() + $service->databases()->count()`.
pub async fn service_part_count(db: &Db, service_id: i64) -> DbResult<i64> {
    Ok(sqlx::query_scalar(
        "SELECT (SELECT count(*) FROM service_applications WHERE service_id = $1 AND deleted_at IS NULL) \
         + (SELECT count(*) FROM service_databases WHERE service_id = $1 AND deleted_at IS NULL)",
    )
    .bind(service_id)
    .fetch_one(db.pool())
    .await?)
}

// ---------------------------------------------------------------------------------------------
// ImageRescanJob: the resources of a server
// ---------------------------------------------------------------------------------------------

/// The condition of `Server::primaryApplications()` and `Server::databases()`: the destination of
/// `t` is on the server `$1` (`$2` and `$3` are the destination classes).
const DESTINATION_ON_SERVER: &str = "((t.destination_type = $2 AND t.destination_id IN (SELECT id FROM standalone_dockers WHERE server_id = $1)) \
     OR (t.destination_type = $3 AND t.destination_id IN (SELECT id FROM swarm_dockers WHERE server_id = $1)))";

async fn owners_where(
    db: &Db,
    table: &'static str,
    morph: &'static str,
    condition: &str,
    server_id: i64,
    with_destination_classes: bool,
) -> DbResult<Vec<ScanOwner>> {
    let sql = format!(
        "{} WHERE t.deleted_at IS NULL AND {condition}",
        owner_select(table)
    );
    let mut query = sqlx::query_as::<_, OwnerTuple>(&sql).bind(server_id);
    if with_destination_classes {
        query = query.bind(STANDALONE_DOCKER_MORPH).bind(SWARM_DOCKER_MORPH);
    }
    query
        .fetch_all(db.pool())
        .await?
        .into_iter()
        .map(|row| owner_from(morph, row))
        .collect()
}

/// `$server->applications()`: the applications whose destination is on the server, and the ones
/// that use it as an additional server (`additional_destinations`).
pub async fn applications_on_server(db: &Db, server_id: i64) -> DbResult<Vec<ScanOwner>> {
    let condition = format!(
        "({DESTINATION_ON_SERVER} OR t.id IN (SELECT application_id FROM additional_destinations WHERE server_id = $1))"
    );
    owners_where(
        db,
        "applications",
        APPLICATION_MORPH,
        &condition,
        server_id,
        true,
    )
    .await
}

/// `$server->databases()`: the standalone databases whose destination is on the server, without
/// the instance database (`isInstanceDatabase()`: the PostgreSQL row with id 0).
pub async fn databases_on_server(db: &Db, server_id: i64) -> DbResult<Vec<ScanOwner>> {
    let mut out = Vec::new();
    for kind in Kind::ALL {
        let condition = if kind == Kind::Postgresql {
            format!("{DESTINATION_ON_SERVER} AND t.id <> 0")
        } else {
            DESTINATION_ON_SERVER.to_owned()
        };
        out.extend(
            owners_where(db, kind.table(), kind.morph(), &condition, server_id, true).await?,
        );
    }
    Ok(out)
}

/// `$server->services()->get()` (`hasMany` on `services.server_id`).
pub async fn services_on_server(db: &Db, server_id: i64) -> DbResult<Vec<ScanOwner>> {
    owners_where(
        db,
        "services",
        SERVICE_MORPH,
        "t.server_id = $1",
        server_id,
        false,
    )
    .await
}

/// `->with(['applications', 'databases'])` of [`services_on_server`]: the applications and
/// databases of every service of the server, with two queries.
pub async fn service_parts_on_server(db: &Db, server_id: i64) -> DbResult<Vec<ScanOwner>> {
    let mut out = Vec::new();
    for (table, morph) in [
        ("service_applications", SERVICE_APPLICATION_MORPH),
        ("service_databases", SERVICE_DATABASE_MORPH),
    ] {
        // `sv` is the service of the part in `owner_select()`, joined only when it is not deleted.
        out.extend(owners_where(db, table, morph, "sv.server_id = $1", server_id, false).await?);
    }
    Ok(out)
}

/// `ApplicationPreview::query()->where('application_id', …)->where('pull_request_id', …)->exists()`.
pub async fn preview_exists(db: &Db, application_id: i64, pull_request_id: i64) -> DbResult<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM application_previews WHERE application_id = $1 \
         AND pull_request_id = $2 AND deleted_at IS NULL)",
    )
    .bind(application_id)
    .bind(pull_request_id)
    .fetch_one(db.pool())
    .await?)
}

/// `ImageRescanJob::existingPreviews($application)`: the pull requests of its previews.
pub async fn preview_pull_request_ids(db: &Db, application_id: i64) -> DbResult<Vec<i64>> {
    Ok(sqlx::query_scalar(
        "SELECT pull_request_id::bigint FROM application_previews \
         WHERE application_id = $1 AND deleted_at IS NULL",
    )
    .bind(application_id)
    .fetch_all(db.pool())
    .await?)
}

// ---------------------------------------------------------------------------------------------
// ImageRescanJob: ended ignores
// ---------------------------------------------------------------------------------------------

/// `ImageRescanJob::refreshCountsAfterEndedIgnores()`: the ignores of a team that ended in the
/// last two days: by their end date, or by the days (`signal_limit` is `now` minus
/// `IgnoreMatcher::EXPLOIT_SIGNAL_IGNORE_DAYS`) after which an ignore no longer covers a problem
/// with an exploit signal.
pub async fn ended_ignores(
    db: &Db,
    team_id: i64,
    now: NaiveDateTime,
    signal_limit: NaiveDateTime,
) -> DbResult<Vec<ImageScanIgnoreRow>> {
    let two_days = chrono::Duration::days(2);
    Ok(sqlx::query_as(&format!(
        "SELECT {IGNORE_COLUMNS} FROM image_scan_ignores WHERE team_id = $1 AND (\
         (expires_at IS NOT NULL AND expires_at <= $2 AND expires_at > $3) \
         OR (created_at <= $4 AND created_at > $5 AND (expires_at IS NULL OR expires_at > $3)))"
    ))
    .bind(team_id)
    .bind(now)
    .bind(now - two_days)
    .bind(signal_limit)
    .bind(signal_limit - two_days)
    .fetch_all(db.pool())
    .await?)
}

// ---------------------------------------------------------------------------------------------
// ImageRescanJob: retention
// ---------------------------------------------------------------------------------------------

/// What the retention reads of a scan row, with the pull request of its deployment
/// (`->with('deploymentQueue:id,pull_request_id')`).
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct RetentionScan {
    pub id: i64,
    pub resource_type: String,
    pub resource_id: i64,
    pub image_id: String,
    pub image_ref: String,
    pub deployment_queue_id: Option<i64>,
    /// `$scan->deploymentQueue?->pull_request_id`; null without a deployment.
    pub pull_request_id: Option<i64>,
}

/// `ImageRescanJob::applyRetention()`: the scan rows of a server, grouped by resource, in each
/// group the latest scan first (`coalesce(scanned_at, updated_at) desc, id desc`).
pub async fn retention_scans(db: &Db, server_id: i64) -> DbResult<Vec<RetentionScan>> {
    Ok(sqlx::query_as(
        "SELECT s.id, s.resource_type, s.resource_id, s.image_id, s.image_ref, s.deployment_queue_id, \
         (SELECT q.pull_request_id::bigint FROM application_deployment_queues q WHERE q.id = s.deployment_queue_id) AS pull_request_id \
         FROM image_scans s WHERE s.server_id = $1 \
         ORDER BY s.resource_type, s.resource_id, COALESCE(s.scanned_at, s.updated_at) DESC, s.id DESC",
    )
    .bind(server_id)
    .fetch_all(db.pool())
    .await?)
}

/// What the retention reads of an application and its settings.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct RetentionApplication {
    pub id: i64,
    pub uuid: String,
    pub docker_registry_image_name: Option<String>,
    /// `$application->settings?->docker_images_to_keep`; null without a settings row.
    pub docker_images_to_keep: Option<i64>,
}

impl RetentionApplication {
    /// `$application->docker_registry_image_name ?: $application->uuid`.
    pub fn repository(&self) -> &str {
        match self.docker_registry_image_name.as_deref() {
            Some(name) if !name.is_empty() && name != "0" => name,
            _ => &self.uuid,
        }
    }
}

/// `Application::withTrashed()->with('settings')->find($id)`.
pub async fn retention_application(db: &Db, id: i64) -> DbResult<Option<RetentionApplication>> {
    Ok(sqlx::query_as(
        "SELECT a.id, a.uuid::text AS uuid, a.docker_registry_image_name::text AS docker_registry_image_name, \
         (SELECT st.docker_images_to_keep::bigint FROM application_settings st WHERE st.application_id = a.id LIMIT 1) AS docker_images_to_keep \
         FROM applications a WHERE a.id = $1",
    )
    .bind(id)
    .fetch_optional(db.pool())
    .await?)
}

/// `ImageRescanJob::rollbackList()`: `(id, commit)` of the finished deployments of an application
/// that are not previews, the latest first.
pub async fn finished_deployments(
    db: &Db,
    application_id: i64,
) -> DbResult<Vec<(i64, Option<String>)>> {
    Ok(sqlx::query_as(
        "SELECT id, commit::text FROM application_deployment_queues \
         WHERE application_id = $1 AND pull_request_id = 0 AND status = 'finished' \
         ORDER BY created_at DESC, id DESC",
    )
    .bind(application_id.to_string())
    .fetch_all(db.pool())
    .await?)
}

/// `ImageScan::query()->where('server_id', $server->id)->whereKey($ids)->delete()`.
pub async fn delete_scans_on_server(db: &Db, server_id: i64, ids: &[i64]) -> DbResult<()> {
    if ids.is_empty() {
        return Ok(());
    }
    db.ensure_writable()?;
    sqlx::query("DELETE FROM image_scans WHERE server_id = $1 AND id = ANY($2)")
        .bind(server_id)
        .bind(ids)
        .execute(db.pool())
        .await?;
    Ok(())
}

/// `ImageRescanJob::deleteUnusedSboms()`: `(id, image_id)` of the SBOMs of a server that no scan
/// uses and that were last used before `before`, at most `limit`.
pub async fn unused_sboms(
    db: &Db,
    server_id: i64,
    before: NaiveDateTime,
    limit: i64,
) -> DbResult<Vec<(i64, String)>> {
    Ok(sqlx::query_as(
        "SELECT b.id, b.image_id FROM image_sboms b WHERE b.server_id = $1 AND b.last_used_at < $2 \
         AND NOT EXISTS (SELECT 1 FROM image_scans s WHERE s.image_sbom_id = b.id) \
         ORDER BY b.id LIMIT $3",
    )
    .bind(server_id)
    .bind(before)
    .bind(limit)
    .fetch_all(db.pool())
    .await?)
}

/// Deletes the given SBOM rows of a server that still have no scan
/// (`->whereKey($ids)->whereDoesntHave('scans')->delete()`).
pub async fn delete_unused_sboms(db: &Db, server_id: i64, ids: &[i64]) -> DbResult<()> {
    if ids.is_empty() {
        return Ok(());
    }
    db.ensure_writable()?;
    sqlx::query(
        "DELETE FROM image_sboms b WHERE b.server_id = $1 AND b.id = ANY($2) \
         AND NOT EXISTS (SELECT 1 FROM image_scans s WHERE s.image_sbom_id = b.id)",
    )
    .bind(server_id)
    .bind(ids)
    .execute(db.pool())
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn application(image_name: Option<&str>) -> RetentionApplication {
        RetentionApplication {
            id: 7,
            uuid: "appuuid".to_owned(),
            docker_registry_image_name: image_name.map(str::to_owned),
            docker_images_to_keep: Some(2),
        }
    }

    #[test]
    fn the_repository_is_the_registry_image_name_or_the_uuid() {
        assert_eq!(
            application(Some("ghcr.io/acme/app")).repository(),
            "ghcr.io/acme/app"
        );
        assert_eq!(application(Some("")).repository(), "appuuid");
        assert_eq!(application(None).repository(), "appuuid");
    }

    #[test]
    fn the_destination_condition_reads_both_destination_types() {
        assert!(DESTINATION_ON_SERVER.contains("standalone_dockers WHERE server_id = $1"));
        assert!(DESTINATION_ON_SERVER.contains("swarm_dockers WHERE server_id = $1"));
        assert!(DESTINATION_ON_SERVER.contains("t.destination_type = $2"));
        assert!(DESTINATION_ON_SERVER.contains("t.destination_type = $3"));
    }
}
