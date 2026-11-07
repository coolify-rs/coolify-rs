//! Queries of the scheduled instance jobs (`ScheduledJobManager`, `Kernel`).

use chrono::NaiveDateTime;
use coolify_domain::new_public_id;

use crate::records::Record;
use crate::{Db, DbResult};

/// `ScheduledVolumeBackup::where('enabled', true)`, by id.
pub async fn enabled_volume_backups(db: &Db) -> DbResult<Vec<Record>> {
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT row_to_json(b)::text FROM scheduled_volume_backups b WHERE b.enabled ORDER BY b.id",
    )
    .fetch_all(db.pool())
    .await?;
    Ok(rows
        .iter()
        .filter_map(|t| serde_json::from_str(t).ok())
        .collect())
}

/// `$backup->executions()->where('stop_recovery_pending', true)->exists()`.
pub async fn volume_backup_has_stop_recovery_pending(db: &Db, backup_id: i64) -> DbResult<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM scheduled_volume_backup_executions WHERE scheduled_volume_backup_id = $1 AND stop_recovery_pending)",
    )
    .bind(backup_id)
    .fetch_one(db.pool())
    .await?)
}

/// `recoverStoppedVolumeBackupContainers()`: `(id, schedule id, stop_recovery_pending)`.
pub async fn volume_executions_needing_recovery(db: &Db) -> DbResult<Vec<(i64, i64, bool)>> {
    Ok(sqlx::query_as(
        "SELECT id, scheduled_volume_backup_id, stop_recovery_pending FROM scheduled_volume_backup_executions \
         WHERE (stop_recovery_pending OR s3_cleanup_pending) AND NOT recovery_needs_attention ORDER BY id",
    )
    .fetch_all(db.pool())
    .await?)
}

/// `ScheduledDatabaseBackupExecution::create([...])` of `reportMissedOccurrence()`.
pub async fn record_missed_database_backup(
    db: &Db,
    backup_id: i64,
    database_name: Option<&str>,
    message: &str,
    now: NaiveDateTime,
) -> DbResult<()> {
    db.ensure_writable()?;
    let (column, value, host) =
        crate::worker_host::insert_parts(db.schema(), "scheduled_database_backup_executions", 6);
    let sql = format!(
        "INSERT INTO scheduled_database_backup_executions \
         (uuid, database_name, scheduled_database_backup_id, status, message, local_storage_deleted, finished_at, created_at, updated_at{column}) \
         VALUES ($1, $2, $3, 'failed', $4, false, $5, $5, $5{value})"
    );
    let mut query = sqlx::query(&sql)
        .bind(new_public_id())
        .bind(database_name)
        .bind(backup_id)
        .bind(message)
        .bind(now);
    if let Some(host) = host {
        query = query.bind(host);
    }
    query.execute(db.pool()).await?;
    Ok(())
}

/// `ScheduledVolumeBackupExecution::create([...])` of `reportMissedOccurrence()`.
pub async fn record_missed_volume_backup(
    db: &Db,
    backup_id: i64,
    message: &str,
    now: NaiveDateTime,
) -> DbResult<()> {
    db.ensure_writable()?;
    let (column, value, host) =
        crate::worker_host::insert_parts(db.schema(), "scheduled_volume_backup_executions", 5);
    let sql = format!(
        "INSERT INTO scheduled_volume_backup_executions \
         (uuid, scheduled_volume_backup_id, status, message, finished_at, created_at, updated_at{column}) \
         VALUES ($1, $2, 'failed', $3, $4, $4, $4{value})"
    );
    let mut query = sqlx::query(&sql)
        .bind(new_public_id())
        .bind(backup_id)
        .bind(message)
        .bind(now);
    if let Some(host) = host {
        query = query.bind(host);
    }
    query.execute(db.pool()).await?;
    Ok(())
}

/// `DockerCleanupExecution::create([...])` of `reportMissedOccurrence()`.
pub async fn record_missed_cleanup(
    db: &Db,
    server_id: i64,
    message: &str,
    now: NaiveDateTime,
) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(
        "INSERT INTO docker_cleanup_executions (uuid, server_id, status, message, finished_at, created_at, updated_at) \
         VALUES ($1, $2, 'failed', $3, $4, $4, $4)",
    )
    .bind(new_public_id())
    .bind(server_id)
    .bind(message)
    .bind(now)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// `failInterruptedExecutions()`: `(id, server id)`.
pub async fn running_cleanups_before(db: &Db, before: NaiveDateTime) -> DbResult<Vec<(i64, i64)>> {
    Ok(sqlx::query_as(
        "SELECT id, server_id FROM docker_cleanup_executions \
         WHERE status = 'running' AND finished_at IS NULL AND created_at < $1 ORDER BY id",
    )
    .bind(before)
    .fetch_all(db.pool())
    .await?)
}

/// The guarded update of `failInterruptedExecutions()`: whether this call moved the row.
pub async fn fail_running_cleanup(
    db: &Db,
    id: i64,
    message: &str,
    now: NaiveDateTime,
) -> DbResult<bool> {
    db.ensure_writable()?;
    Ok(sqlx::query(
        "UPDATE docker_cleanup_executions SET status = 'failed', message = $2, finished_at = $3, updated_at = $3 \
         WHERE id = $1 AND status = 'running' AND finished_at IS NULL",
    )
    .bind(id)
    .bind(message)
    .bind(now)
    .execute(db.pool())
    .await?
    .rows_affected()
        == 1)
}

/// `sanctum:prune-expired --hours=1`.
pub async fn prune_expired_tokens(db: &Db, now: NaiveDateTime) -> DbResult<u64> {
    db.ensure_writable()?;
    Ok(
        sqlx::query("DELETE FROM personal_access_tokens WHERE expires_at < $1 - interval '1 hour'")
            .bind(now)
            .execute(db.pool())
            .await?
            .rows_affected(),
    )
}

/// `cleanup:unreachable-servers`, off Coolify Cloud: `forceDisableServer()` for servers that stayed
/// unreachable. The key file it deletes does not exist here: keys are read from the database.
pub async fn force_disable_abandoned_servers(db: &Db) -> DbResult<u64> {
    db.ensure_writable()?;
    Ok(sqlx::query(
        "UPDATE server_settings SET force_disabled = true WHERE server_id IN ( \
           SELECT s.id FROM servers s WHERE s.unreachable_count >= 3 \
             AND s.updated_at < now() - interval '7 days' \
             AND EXISTS (SELECT 1 FROM notification_throttles t WHERE t.notifiable_type = 'App\\Models\\Server' \
               AND t.notifiable_id = s.id AND t.notification = 'App\\Notifications\\Server\\Unreachable'))",
    )
    .execute(db.pool())
    .await?
    .rows_affected())
}

/// `TeamInvitation::all()` with `isValid()`: an invitation older than
/// `constants.invitation.link.expiration_days` goes; returns the invited users to check.
pub async fn cleanup_expired_invitations(db: &Db, now: NaiveDateTime) -> DbResult<Vec<i64>> {
    db.ensure_writable()?;
    let emails: Vec<String> = sqlx::query_scalar(
        "DELETE FROM team_invitations WHERE created_at < $1 - interval '3 days' RETURNING email",
    )
    .bind(now)
    .fetch_all(db.pool())
    .await?;
    let mut users = Vec::new();
    for email in emails {
        // `deleteIfNotVerifiedAndForcePasswordReset()`, up to `$this->delete()`.
        let id: Option<i64> = sqlx::query_scalar(
            "SELECT u.id FROM users u WHERE u.email = $1 AND u.email_verified_at IS NULL \
               AND u.force_password_reset = true \
               AND NOT EXISTS (SELECT 1 FROM team_invitations i WHERE i.email = u.email) \
               AND NOT EXISTS (SELECT 1 FROM team_user tu JOIN teams t ON t.id = tu.team_id \
                 WHERE tu.user_id = u.id AND (t.personal_team = false \
                   OR EXISTS (SELECT 1 FROM team_user o WHERE o.team_id = t.id AND o.user_id <> u.id)))",
        )
        .bind(&email)
        .fetch_optional(db.pool())
        .await?;
        users.extend(id);
    }
    Ok(users)
}

/// `cleanupExpiredEmailChangeRequests()`.
pub async fn cleanup_expired_email_changes(db: &Db, now: NaiveDateTime) -> DbResult<u64> {
    db.ensure_writable()?;
    Ok(sqlx::query(
        "UPDATE users SET pending_email = NULL, email_change_code = NULL, email_change_code_expires_at = NULL \
         WHERE email_change_code_expires_at IS NOT NULL AND email_change_code_expires_at < $1",
    )
    .bind(now)
    .execute(db.pool())
    .await?
    .rows_affected())
}

/// The models a notification throttle can point at, with their tables.
const THROTTLE_MODELS: [(&str, &str); 7] = [
    ("App\\Models\\Server", "servers"),
    ("App\\Models\\Application", "applications"),
    ("App\\Models\\Service", "services"),
    ("App\\Models\\ScheduledTask", "scheduled_tasks"),
    (
        "App\\Models\\ScheduledDatabaseBackup",
        "scheduled_database_backups",
    ),
    (
        "App\\Models\\ScheduledVolumeBackup",
        "scheduled_volume_backups",
    ),
    ("App\\Models\\S3Storage", "s3_storages"),
];

/// `NotificationThrottle::deleteOrphans()` for the models above. Every table is `withoutGlobalScopes`,
/// so a soft deleted row still counts as existing. Other types are left alone rather than guessed at.
pub async fn delete_orphaned_notification_throttles(db: &Db) -> DbResult<u64> {
    db.ensure_writable()?;
    let mut deleted = 0;
    for (model, table) in THROTTLE_MODELS {
        deleted += sqlx::query(&format!(
            "DELETE FROM notification_throttles t WHERE t.notifiable_type = $1 \
             AND NOT EXISTS (SELECT 1 FROM {table} x WHERE x.id = t.notifiable_id)"
        ))
        .bind(model)
        .execute(db.pool())
        .await?
        .rows_affected();
    }
    Ok(deleted)
}

/// `cleanupTable()`: the table and column names are constants of the caller.
pub async fn delete_older_in_batches(
    db: &Db,
    table: &'static str,
    column: &'static str,
    older_than: NaiveDateTime,
    batch: i64,
) -> DbResult<()> {
    db.ensure_writable()?;
    loop {
        let deleted = sqlx::query(&format!(
            "DELETE FROM {table} WHERE ctid IN (SELECT ctid FROM {table} WHERE {column} < $1 LIMIT $2)"
        ))
        .bind(older_than)
        .bind(batch)
        .execute(db.pool())
        .await?
        .rows_affected();
        if deleted == 0 {
            return Ok(());
        }
    }
}

/// `cleanupTable('sessions', 'last_activity', ...)`: the column is a timestamp number.
pub async fn delete_old_sessions(db: &Db, older_than: i64, batch: i64) -> DbResult<()> {
    db.ensure_writable()?;
    loop {
        let deleted = sqlx::query(
            "DELETE FROM sessions WHERE ctid IN (SELECT ctid FROM sessions WHERE last_activity < $1 LIMIT $2)",
        )
        .bind(i32::try_from(older_than).unwrap_or(i32::MAX))
        .bind(batch)
        .execute(db.pool())
        .await?
        .rows_affected();
        if deleted == 0 {
            return Ok(());
        }
    }
}

/// `cleanupTable('github_runner_executions', ...)`: runs that no longer need work.
pub async fn delete_finished_runner_executions(
    db: &Db,
    older_than: NaiveDateTime,
    batch: i64,
) -> DbResult<()> {
    db.ensure_writable()?;
    loop {
        let deleted = sqlx::query(
            "DELETE FROM github_runner_executions WHERE ctid IN (SELECT ctid FROM github_runner_executions \
             WHERE created_at < $1 AND status NOT IN ('queued', 'provisioning', 'idle', 'running') LIMIT $2)",
        )
        .bind(older_than)
        .bind(batch)
        .execute(db.pool())
        .await?
        .rows_affected();
        if deleted == 0 {
            return Ok(());
        }
    }
}

/// A token of `ApiTokenExpirationWarningJob`.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ExpiringToken {
    pub id: i64,
    pub name: String,
    pub team_id: i64,
    pub expires_at: NaiveDateTime,
}

/// The query of `ApiTokenExpirationWarningJob`, with the team and membership checks of its loop.
pub async fn tokens_expiring_within_a_day(
    db: &Db,
    now: NaiveDateTime,
) -> DbResult<Vec<ExpiringToken>> {
    Ok(sqlx::query_as(
        "SELECT t.id, t.name, tm.id AS team_id, t.expires_at FROM personal_access_tokens t \
         JOIN teams tm ON tm.id::text = t.team_id \
         JOIN team_user tu ON tu.team_id = tm.id AND tu.user_id = t.tokenable_id \
         WHERE t.expires_at IS NOT NULL AND t.expires_at > $1 AND t.expires_at <= $1 + interval '1 day' \
           AND t.api_token_expiration_warning_sent_at IS NULL \
           AND t.tokenable_type = 'App\\Models\\User' ORDER BY t.id",
    )
    .bind(now)
    .fetch_all(db.pool())
    .await?)
}

pub async fn mark_token_warning_sent(db: &Db, id: i64, now: NaiveDateTime) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(
        "UPDATE personal_access_tokens SET api_token_expiration_warning_sent_at = $2 \
         WHERE id = $1 AND api_token_expiration_warning_sent_at IS NULL",
    )
    .bind(id)
    .bind(now)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// A schedule of `CheckMissingDatabaseBackupsJob` or `CheckMissingVolumeBackupsJob`.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct MissingBackupCheck {
    pub id: i64,
    pub uuid: String,
    pub team_id: i64,
    pub days: i64,
    pub created_at: Option<NaiveDateTime>,
    pub last_execution_at: Option<NaiveDateTime>,
    pub latest_log_at: Option<NaiveDateTime>,
}

/// `ScheduledDatabaseBackup::...->where('missing_backup_notification_days', '>', 0)`; a schedule
/// whose team is gone is left out, as `! $backup->team` returns.
pub async fn database_backups_with_missing_check(db: &Db) -> DbResult<Vec<MissingBackupCheck>> {
    Ok(sqlx::query_as(
        "SELECT b.id, b.uuid, b.team_id, b.missing_backup_notification_days::bigint AS days, b.created_at, \
           b.last_execution_at, \
           (SELECT e.created_at FROM scheduled_database_backup_executions e WHERE e.scheduled_database_backup_id = b.id \
            ORDER BY e.created_at DESC, e.id DESC LIMIT 1) AS latest_log_at \
         FROM scheduled_database_backups b JOIN teams t ON t.id = b.team_id \
         WHERE b.enabled AND b.missing_backup_notification_days > 0 ORDER BY b.id",
    )
    .fetch_all(db.pool())
    .await?)
}

/// `ScheduledVolumeBackup::...->where('missing_backup_notification_days', '>', 0)`.
pub async fn volume_backups_with_missing_check(db: &Db) -> DbResult<Vec<MissingBackupCheck>> {
    Ok(sqlx::query_as(
        "SELECT b.id, b.uuid, b.team_id, b.missing_backup_notification_days::bigint AS days, b.created_at, \
           NULL::timestamp AS last_execution_at, \
           (SELECT e.created_at FROM scheduled_volume_backup_executions e WHERE e.scheduled_volume_backup_id = b.id \
            ORDER BY e.created_at DESC, e.id DESC LIMIT 1) AS latest_log_at \
         FROM scheduled_volume_backups b JOIN teams t ON t.id = b.team_id \
         WHERE b.enabled AND b.missing_backup_notification_days > 0 ORDER BY b.id",
    )
    .fetch_all(db.pool())
    .await?)
}

/// `S3Storage::query()->where('is_usable', false)->where('id', '>', $lastId)`, by id.
pub async fn unusable_s3_storages_after(db: &Db, after: i64) -> DbResult<Vec<Record>> {
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT row_to_json(s)::text FROM s3_storages s WHERE s.is_usable = false AND s.id > $1 ORDER BY s.id",
    )
    .bind(after)
    .fetch_all(db.pool())
    .await?;
    Ok(rows
        .iter()
        .filter_map(|t| serde_json::from_str(t).ok())
        .collect())
}

/// The `save()` of `testConnection(shouldSave: true)`.
pub async fn save_s3_connection_result(
    db: &Db,
    id: i64,
    is_usable: bool,
    unusable_email_sent: bool,
    now: NaiveDateTime,
) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(
        "UPDATE s3_storages SET is_usable = $2, unusable_email_sent = $3, updated_at = $4 WHERE id = $1",
    )
    .bind(id)
    .bind(is_usable)
    .bind(unusable_email_sent)
    .bind(now)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// The addresses of `$team->members()->wherePivotIn('role', ['admin', 'owner'])`.
pub async fn team_admin_emails(db: &Db, team_id: i64) -> DbResult<Vec<String>> {
    Ok(sqlx::query_scalar(
        "SELECT u.email FROM users u JOIN team_user tu ON tu.user_id = u.id \
         WHERE tu.team_id = $1 AND tu.role IN ('admin', 'owner') ORDER BY u.id",
    )
    .bind(team_id)
    .fetch_all(db.pool())
    .await?)
}

/// `instanceSettings()->update_check_frequency`.
pub async fn update_check_frequency(db: &Db) -> DbResult<Option<String>> {
    Ok(sqlx::query_scalar::<_, Option<String>>(
        "SELECT update_check_frequency FROM instance_settings WHERE id = 0",
    )
    .fetch_optional(db.pool())
    .await?
    .flatten())
}

/// `$settings->update(['helper_version' => $latestVersion])`.
pub async fn set_helper_version(db: &Db, version: &str) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query("UPDATE instance_settings SET helper_version = $1 WHERE id = 0")
        .bind(version)
        .execute(db.pool())
        .await?;
    Ok(())
}

/// `instanceSettings()->update(['new_version_available' => $newVersionAvailable])`.
pub async fn set_new_version_available(db: &Db, available: bool) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query("UPDATE instance_settings SET new_version_available = $1 WHERE id = 0")
        .bind(available)
        .execute(db.pool())
        .await?;
    Ok(())
}

/// The first statement of `CheckTraefikVersionJob::handle`.
pub async fn clear_traefik_info_of_other_proxies(db: &Db, now: NaiveDateTime) -> DbResult<u64> {
    db.ensure_writable()?;
    Ok(sqlx::query(
        "UPDATE servers SET detected_traefik_version = NULL, traefik_outdated_info = NULL, updated_at = $1 \
         WHERE (proxy IS NULL OR proxy->>'type' <> 'TRAEFIK') \
           AND (detected_traefik_version IS NOT NULL OR traefik_outdated_info IS NOT NULL)",
    )
    .bind(now)
    .execute(db.pool())
    .await?
    .rows_affected())
}

/// A row of `RegenerateSslCertJob`.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct CertificateToRegenerate {
    pub id: i64,
    pub server_id: i64,
    pub resource_type: Option<String>,
    pub resource_id: Option<i64>,
}

/// The query of `RegenerateSslCertJob::handle`. Without `force`, certificates that end within 14 days.
pub async fn certificates_to_regenerate(
    db: &Db,
    server_id: Option<i64>,
    force: bool,
    now: NaiveDateTime,
) -> DbResult<Vec<CertificateToRegenerate>> {
    Ok(sqlx::query_as(
        "SELECT id, server_id, resource_type, resource_id::bigint AS resource_id FROM ssl_certificates \
         WHERE is_ca_certificate = false AND ($1::bigint IS NULL OR server_id = $1) \
           AND ($2 OR valid_until <= $3 + interval '14 days') ORDER BY id",
    )
    .bind(server_id)
    .bind(force)
    .bind(now)
    .fetch_all(db.pool())
    .await?)
}

/// Ids a query of `cleanup:stucked-resources` selects.
pub async fn ids(db: &Db, sql: &str) -> DbResult<Vec<i64>> {
    Ok(sqlx::query_scalar(sql).fetch_all(db.pool()).await?)
}

/// The per-row deletes of `cleanup:stucked-resources`: the statement holds its own table name.
pub async fn delete_by_id(db: &Db, sql: &str, id: i64) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(sql).bind(id).execute(db.pool()).await?;
    Ok(())
}

/// `ApplicationPreview::withTrashed()->...` for `cleanup_orphaned_preview_containers`.
pub async fn preview_exists_with_trashed(
    db: &Db,
    application_id: i64,
    pull_request_id: i64,
) -> DbResult<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM application_previews WHERE application_id = $1 AND pull_request_id = $2)",
    )
    .bind(application_id)
    .bind(pull_request_id)
    .fetch_one(db.pool())
    .await?)
}

/// `Application::withTrashed()->where('uuid', $uuid)->value('id')`.
pub async fn application_id_with_trashed(db: &Db, uuid: &str) -> DbResult<Option<i64>> {
    Ok(
        sqlx::query_scalar("SELECT id FROM applications WHERE uuid = $1")
            .bind(uuid)
            .fetch_optional(db.pool())
            .await?,
    )
}

/// The active deployments of `CleanupHelperContainersJob`.
pub async fn active_deployment_uuids(db: &Db, server_id: i64) -> DbResult<Vec<String>> {
    Ok(sqlx::query_scalar(
        "SELECT deployment_uuid FROM application_deployment_queues \
         WHERE server_id = $1 AND status IN ('in_progress', 'queued')",
    )
    .bind(server_id)
    .fetch_all(db.pool())
    .await?)
}

/// `$team->delete()` with `deleteOwnedRecords()`, for a team without members and servers.
pub async fn delete_team_and_owned_records(db: &Db, team_id: i64) -> DbResult<()> {
    db.ensure_writable()?;
    let mut tx = db.pool().begin().await?;
    for sql in [
        "DELETE FROM personal_access_tokens WHERE team_id = ($1::bigint)::text",
        "DELETE FROM private_keys WHERE team_id = $1",
        "UPDATE github_apps SET team_id = 0 WHERE team_id = $1 AND is_system_wide",
        "UPDATE gitlab_apps SET team_id = 0 WHERE team_id = $1 AND is_system_wide",
        "UPDATE gitea_apps SET team_id = 0 WHERE team_id = $1 AND is_system_wide",
        "DELETE FROM github_apps WHERE team_id = $1",
        "DELETE FROM gitlab_apps WHERE team_id = $1",
        "DELETE FROM gitea_apps WHERE team_id = $1",
        "DELETE FROM tags WHERE team_id = $1",
        "DELETE FROM shared_environment_variables WHERE team_id = $1",
        "DELETE FROM s3_storages WHERE team_id = $1",
        "DELETE FROM teams WHERE id = $1",
    ] {
        sqlx::query(sql).bind(team_id).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(())
}

/// The guarded update of `GetContainersStatus`: whether this call reached the limit first.
pub async fn claim_application_restart_limit(
    db: &Db,
    id: i64,
    now: NaiveDateTime,
) -> DbResult<bool> {
    db.ensure_writable()?;
    Ok(sqlx::query(
        "UPDATE applications SET restart_limit_reached = true, updated_at = $2 WHERE id = $1 AND restart_limit_reached = false",
    )
    .bind(id)
    .bind(now)
    .execute(db.pool())
    .await?
    .rows_affected()
        == 1)
}

/// `HasRestartLimit::trackRestartCount()` for a preview.
pub async fn claim_preview_restart_limit(db: &Db, id: i64, now: NaiveDateTime) -> DbResult<bool> {
    db.ensure_writable()?;
    Ok(sqlx::query(
        "UPDATE application_previews SET restart_limit_reached = true, updated_at = $2 WHERE id = $1 AND restart_limit_reached = false",
    )
    .bind(id)
    .bind(now)
    .execute(db.pool())
    .await?
    .rows_affected()
        == 1)
}
