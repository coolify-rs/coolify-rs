//! Admission queries of the deployment queue added by Coolify v4.7 (bootstrap/helpers/applications.php):
//! build slots of build servers, deployments that a newer deployment replaces, and the
//! conditional status updates that cannot race with a cancel.

use chrono::NaiveDateTime;

use crate::applications::{Deployment, deployment};
use crate::{Db, DbResult};

/// `active_builds_on_server($server_id)`: the running deployments that build on a server: its own
/// deployments without a build server, and the deployments that it builds as a build server.
pub async fn active_builds_on_server(db: &Db, server_id: i64) -> DbResult<i64> {
    Ok(sqlx::query_scalar(
        "SELECT count(*) FROM application_deployment_queues WHERE status = 'in_progress' \
         AND (build_server_id = $1 OR (server_id = $1 AND build_server_id IS NULL))",
    )
    .bind(server_id as i32)
    .fetch_one(db.pool())
    .await?)
}

/// One build server in `least_busy_build_server()`: its running builds and its
/// "Concurrent builds" setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuildSlots {
    pub server_id: i64,
    pub builds: i64,
    pub concurrent_builds: i64,
}

/// `active_builds_on_server()` and `$server->settings->concurrent_builds` of each server in `ids`.
pub async fn build_slots(db: &Db, ids: &[i64]) -> DbResult<Vec<BuildSlots>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let rows: Vec<(i64, i64, i64)> = sqlx::query_as(
        "SELECT s.id::bigint, \
           (SELECT count(*) FROM application_deployment_queues q WHERE q.status = 'in_progress' \
             AND (q.build_server_id = s.id OR (q.server_id = s.id AND q.build_server_id IS NULL))), \
           COALESCE(st.concurrent_builds, 2)::bigint \
         FROM servers s LEFT JOIN server_settings st ON st.server_id = s.id \
         WHERE s.id = ANY($1) ORDER BY s.id",
    )
    .bind(ids)
    .fetch_all(db.pool())
    .await?;
    Ok(rows
        .into_iter()
        .map(|(server_id, builds, concurrent_builds)| BuildSlots {
            server_id,
            builds,
            concurrent_builds,
        })
        .collect())
}

/// `Server::where('team_id', $teamId)->pluck('id')`.
pub async fn team_server_ids(db: &Db, team_id: i64) -> DbResult<Vec<i64>> {
    Ok(sqlx::query_scalar(
        "SELECT id::bigint FROM servers WHERE team_id = $1 AND deleted_at IS NULL ORDER BY id",
    )
    .bind(team_id)
    .fetch_all(db.pool())
    .await?)
}

/// The deployments that `cancel_superseded_deployments()` cancelled.
#[derive(Debug, Clone, Default)]
pub struct Superseded {
    /// They were queued.
    pub queued: Vec<Deployment>,
    /// They were building (`in_progress` before the deploy phase); their helper must be stopped.
    pub building: Vec<Deployment>,
}

impl Superseded {
    pub fn is_empty(&self) -> bool {
        self.queued.is_empty() && self.building.is_empty()
    }
}

/// The new deployment of `cancel_superseded()`.
#[derive(Debug, Clone, Copy)]
pub struct NewerDeployment {
    pub id: i64,
    pub application_id: i64,
    pub pull_request_id: i64,
    pub server_id: i64,
}

/// The candidate query and the conditional cancel of `cancel_superseded_deployments()`: the older
/// deployments of the same application, pull request and server that the new deployment `new`
/// replaces. `from_webhook`: only webhook deployments are replaced, else webhook, API and MCP ones.
/// `cancel_building`: also the running deployments that have not started to deploy containers.
/// The returned rows have the status they had before the cancel.
pub async fn cancel_superseded(
    db: &Db,
    new: &NewerDeployment,
    from_webhook: bool,
    cancel_building: bool,
    now: NaiveDateTime,
) -> DbResult<Superseded> {
    db.ensure_writable()?;
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT row_to_json(q)::text FROM application_deployment_queues q \
         WHERE application_id = $1 AND pull_request_id = $2 AND server_id = $3 \
           AND (is_webhook OR (NOT $4 AND (is_api OR is_mcp))) \
           AND NOT rollback AND NOT restart_only AND id <> $5 \
           AND (status = 'queued' OR ($6 AND status = 'in_progress' AND deploy_started_at IS NULL)) \
         ORDER BY id",
    )
    .bind(new.application_id.to_string())
    .bind(new.pull_request_id as i32)
    .bind(new.server_id as i32)
    .bind(from_webhook)
    .bind(new.id)
    .bind(cancel_building)
    .fetch_all(db.pool())
    .await?;
    let mut out = Superseded::default();
    for text in &rows {
        let old = deployment(text)?;
        // A conditional update, so a build that starts to deploy at the same moment is never cancelled.
        let done = sqlx::query(
            "UPDATE application_deployment_queues SET status = 'cancelled-by-user', updated_at = $3 \
             WHERE id = $1 AND status = $2 AND deploy_started_at IS NULL",
        )
        .bind(old.id)
        .bind(&old.status)
        .bind(now)
        .execute(db.pool())
        .await?;
        if done.rows_affected() != 1 {
            continue;
        }
        if old.status == "in_progress" {
            out.building.push(old);
        } else {
            out.queued.push(old);
        }
    }
    Ok(out)
}

/// `handle()`: the job moves to `in_progress` unless a cancel came first. False when it was
/// cancelled (or the row is gone).
pub async fn start_unless_cancelled(
    db: &Db,
    id: i64,
    worker: &str,
    now: NaiveDateTime,
) -> DbResult<bool> {
    db.ensure_writable()?;
    sqlx::query(
        "UPDATE application_deployment_queues SET status = 'in_progress', horizon_job_worker = $2, updated_at = $3 \
         WHERE id = $1 AND status <> 'cancelled-by-user'",
    )
    .bind(id)
    .bind(worker)
    .bind(now)
    .execute(db.pool())
    .await?;
    let status: Option<String> =
        sqlx::query_scalar("SELECT status FROM application_deployment_queues WHERE id = $1")
            .bind(id)
            .fetch_optional(db.pool())
            .await?;
    Ok(status.is_some_and(|s| s != "cancelled-by-user"))
}

/// `startDeployPhase()`: marks that the deployment starts to change containers. From here on a
/// newer deployment does not cancel it. Conditional, so exactly one of the two wins.
pub async fn start_deploy_phase(db: &Db, id: i64, now: NaiveDateTime) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(
        "UPDATE application_deployment_queues SET deploy_started_at = $2, updated_at = $2 \
         WHERE id = $1 AND status = 'in_progress' AND deploy_started_at IS NULL",
    )
    .bind(id)
    .bind(now)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// The status change of admission (`$deployment->update(['status' => 'in_progress'])`) together
/// with the build server that `next_queuable()` reserved.
pub async fn admit(
    db: &Db,
    id: i64,
    build_server_id: Option<i64>,
    now: NaiveDateTime,
) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(
        "UPDATE application_deployment_queues SET status = 'in_progress', \
         build_server_id = COALESCE($2, build_server_id), updated_at = $3 WHERE id = $1",
    )
    .bind(id)
    .bind(build_server_id.map(|v| v as i32))
    .bind(now)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// The first check of `GetRollbackImages::allows($application, $tag)`: the tag is the commit of a
/// successful production deployment that was not itself a rollback.
pub async fn was_deployed(db: &Db, application_id: i64, commit: &str) -> DbResult<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM application_deployment_queues WHERE application_id = $1 \
         AND status = 'finished' AND pull_request_id = 0 AND NOT rollback AND commit = $2)",
    )
    .bind(application_id.to_string())
    .bind(commit)
    .fetch_one(db.pool())
    .await?)
}

/// `CleanupHelperContainersJob::activeTransferOwners()`: UUIDs of the running backups and database
/// imports. Their upload and restore containers (helper or rclone) are named "backup-of-",
/// "volume-backup-", "volume-upload-", "s3-restore-", or "backup-decompress-" and the UUID.
pub async fn active_transfer_owners(db: &Db) -> DbResult<Vec<String>> {
    let rows: Vec<Option<String>> = sqlx::query_scalar(
        "SELECT uuid::text FROM scheduled_database_backup_executions WHERE status = 'running' \
         UNION ALL SELECT uuid::text FROM scheduled_volume_backup_executions WHERE status = 'running' \
         UNION ALL SELECT properties->>'operation_uuid' FROM activity_log \
           WHERE properties->>'operation' = 'database_import' AND properties->>'status' IN ('queued', 'in_progress')",
    )
    .fetch_all(db.pool())
    .await?;
    Ok(rows
        .into_iter()
        .flatten()
        .filter(|uuid| !uuid.is_empty())
        .collect())
}

/// `Application::query()->lockForUpdate()->value('git_ssh_known_hosts')` and the conditional
/// write of `save_git_ssh_known_hosts()`, in one transaction. `merge` gets the stored text and
/// answers the text to store, or `None` to leave the row alone. The stored text after the call.
pub async fn merge_git_ssh_known_hosts<F>(
    db: &Db,
    application_id: i64,
    now: NaiveDateTime,
    merge: F,
) -> DbResult<Option<String>>
where
    F: FnOnce(Option<&str>) -> Option<String>,
{
    db.ensure_writable()?;
    let mut tx = db.pool().begin().await?;
    let current: Option<Option<String>> =
        sqlx::query_scalar("SELECT git_ssh_known_hosts FROM applications WHERE id = $1 FOR UPDATE")
            .bind(application_id)
            .fetch_optional(&mut *tx)
            .await?;
    let Some(current) = current else {
        tx.commit().await?;
        return Ok(None);
    };
    let Some(next) = merge(current.as_deref()) else {
        tx.commit().await?;
        return Ok(current);
    };
    sqlx::query("UPDATE applications SET git_ssh_known_hosts = $2, updated_at = $3 WHERE id = $1")
        .bind(application_id)
        .bind(&next)
        .bind(now)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Some(next))
}
