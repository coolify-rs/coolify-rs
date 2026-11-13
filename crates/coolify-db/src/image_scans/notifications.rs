//! `image_scan_notification_states`: what the image security notifications told a team about one
//! resource (`ImageScanNotificationState`), and the other queries of `ScanNotifications`,
//! `ImageScanDigestJob` and `ImageScanAlertCheckJob`.
//!
//! One row per scan owner (unique on `resource_type`, `resource_id`). `notified_ids`,
//! `digest_ids` and `notified_candidates` are JSON lists of strings. Every write of
//! `ScanNotifications` locks the row first ([`lock_state`]), so the alert and the digest never
//! overwrite each other.

use chrono::NaiveDateTime;
use serde_json::Value;

use super::{IGNORE_COLUMNS, ImageScanIgnoreRow};
use crate::{Db, DbResult};

/// The notification event of both image security notifications
/// (`$notifiable->getEnabledChannels('image_vulnerabilities')`): channel `x` sends them when its
/// settings row has `image_vulnerabilities_{x}_notifications` (email on by default, the others
/// off).
pub const EVENT: &str = "image_vulnerabilities";

/// The Telegram settings column with the thread of the image security notifications.
pub const TELEGRAM_THREAD_COLUMN: &str = "telegram_notifications_image_vulnerabilities_thread_id";

/// One `image_scan_notification_states` row.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct NotificationStateRow {
    pub id: i64,
    pub resource_type: String,
    pub resource_id: i64,
    pub team_id: i64,
    /// Relevant findings with an exploit signal that an immediate alert named.
    pub notified_ids: Option<Value>,
    /// Better images (and base images) that a digest covered.
    pub notified_candidates: Option<Value>,
    /// Relevant findings with an action that a digest named, or an alert named with its action.
    pub digest_ids: Option<Value>,
    /// The row records findings and fixes without notifying until the baseline ends.
    pub is_baseline: bool,
    /// A digest said that the resource needs a check.
    pub check_reported: bool,
    /// Digests that held new problems back while the resource needed a check (a `smallint`).
    pub held_digests: i64,
    pub baseline_deployment_queue_id: Option<i64>,
    pub created_at: Option<NaiveDateTime>,
    pub updated_at: Option<NaiveDateTime>,
}

/// `array_map('strval', (array) $value)` of a stored JSON list.
fn string_list(value: Option<&Value>) -> Vec<String> {
    let text = |v: &Value| match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(true) => "1".to_owned(),
        _ => String::new(),
    };
    match value {
        Some(Value::Array(items)) => items.iter().map(text).collect(),
        Some(Value::Object(map)) => map.values().map(text).collect(),
        None | Some(Value::Null) => Vec::new(),
        Some(scalar) => vec![text(scalar)],
    }
}

impl NotificationStateRow {
    /// `(array) $state->notified_ids`.
    pub fn notified_ids(&self) -> Vec<String> {
        string_list(self.notified_ids.as_ref())
    }

    /// `(array) $state->digest_ids`.
    pub fn digest_ids(&self) -> Vec<String> {
        string_list(self.digest_ids.as_ref())
    }

    /// `(array) $state->notified_candidates`.
    pub fn notified_candidates(&self) -> Vec<String> {
        string_list(self.notified_candidates.as_ref())
    }
}

const STATE_COLUMNS: &str = "id, resource_type, resource_id::bigint AS resource_id, team_id, notified_ids, \
    notified_candidates, digest_ids, is_baseline, check_reported, held_digests::bigint AS held_digests, \
    baseline_deployment_queue_id::bigint AS baseline_deployment_queue_id, created_at, updated_at";

/// A new state row (`ImageScanNotificationState::query()->insertOrIgnore([...])`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NewState {
    pub resource_type: String,
    pub resource_id: i64,
    pub team_id: i64,
    pub notified_ids: Vec<String>,
    pub digest_ids: Vec<String>,
    pub notified_candidates: Vec<String>,
    pub check_reported: bool,
    pub held_digests: i64,
    pub is_baseline: bool,
    pub baseline_deployment_queue_id: Option<i64>,
}

/// What `$state->update([...])` changes; `None` keeps the stored value.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StateUpdate {
    pub is_baseline: Option<bool>,
    pub notified_ids: Option<Vec<String>>,
    pub digest_ids: Option<Vec<String>>,
    pub notified_candidates: Option<Vec<String>>,
    pub check_reported: Option<bool>,
    pub held_digests: Option<i64>,
}

/// `ScanNotifications::stateQuery($owner)->first()`.
pub async fn find_state(
    db: &Db,
    resource_type: &str,
    resource_id: i64,
) -> DbResult<Option<NotificationStateRow>> {
    Ok(sqlx::query_as(&format!(
        "SELECT {STATE_COLUMNS} FROM image_scan_notification_states \
         WHERE resource_type = $1 AND resource_id = $2 LIMIT 1"
    ))
    .bind(resource_type)
    .bind(resource_id)
    .fetch_optional(db.pool())
    .await?)
}

const INSERT_STATE: &str = "INSERT INTO image_scan_notification_states \
    (resource_type, resource_id, team_id, notified_ids, digest_ids, notified_candidates, check_reported, \
    held_digests, is_baseline, baseline_deployment_queue_id, created_at, updated_at) \
    VALUES ($1, $2, $3, $4, $5, $6, $7, $8::smallint, $9, $10, $11, $11) \
    ON CONFLICT (resource_type, resource_id) DO NOTHING";

/// `ImageScanNotificationState::query()->insertOrIgnore([...])` outside a lock: creates the row
/// unless the resource has one (first-or-create; the existing row wins). Returns whether it
/// created it.
pub async fn create_state_or_ignore(db: &Db, new: &NewState, now: NaiveDateTime) -> DbResult<bool> {
    db.ensure_writable()?;
    let done = sqlx::query(INSERT_STATE)
        .bind(&new.resource_type)
        .bind(new.resource_id)
        .bind(new.team_id)
        .bind(sqlx::types::Json(&new.notified_ids))
        .bind(sqlx::types::Json(&new.digest_ids))
        .bind(sqlx::types::Json(&new.notified_candidates))
        .bind(new.check_reported)
        .bind(new.held_digests)
        .bind(new.is_baseline)
        .bind(new.baseline_deployment_queue_id)
        .bind(now)
        .execute(db.pool())
        .await?;
    Ok(done.rows_affected() > 0)
}

/// The state row of one owner, locked in a transaction (`ScanNotifications::locked()`:
/// `stateQuery($owner)->lockForUpdate()->first()`). Nothing is written until [`LockedState::commit`];
/// dropping it rolls back.
#[derive(Debug)]
pub struct LockedState {
    tx: sqlx::Transaction<'static, sqlx::Postgres>,
    row: Option<NotificationStateRow>,
}

/// Starts the transaction of `ScanNotifications::locked()` and locks the state row of the owner
/// when it has one.
pub async fn lock_state(db: &Db, resource_type: &str, resource_id: i64) -> DbResult<LockedState> {
    db.ensure_writable()?;
    let mut tx = db.pool().begin().await?;
    let row = sqlx::query_as(&format!(
        "SELECT {STATE_COLUMNS} FROM image_scan_notification_states \
         WHERE resource_type = $1 AND resource_id = $2 LIMIT 1 FOR UPDATE"
    ))
    .bind(resource_type)
    .bind(resource_id)
    .fetch_optional(&mut *tx)
    .await?;
    Ok(LockedState { tx, row })
}

impl LockedState {
    /// The locked row; `None` when the owner has none (nothing is locked then).
    pub fn row(&self) -> Option<&NotificationStateRow> {
        self.row.as_ref()
    }

    /// `insertOrIgnore([...])` in the transaction. A row that another run created at the same
    /// time wins.
    pub async fn create_or_ignore(&mut self, new: &NewState, now: NaiveDateTime) -> DbResult<bool> {
        let done = sqlx::query(INSERT_STATE)
            .bind(&new.resource_type)
            .bind(new.resource_id)
            .bind(new.team_id)
            .bind(sqlx::types::Json(&new.notified_ids))
            .bind(sqlx::types::Json(&new.digest_ids))
            .bind(sqlx::types::Json(&new.notified_candidates))
            .bind(new.check_reported)
            .bind(new.held_digests)
            .bind(new.is_baseline)
            .bind(new.baseline_deployment_queue_id)
            .bind(now)
            .execute(&mut *self.tx)
            .await?;
        Ok(done.rows_affected() > 0)
    }

    /// `$state->update([...])` of the locked row. Does nothing without a row. `updated_at` is
    /// always written (Eloquent writes it only when a value changed).
    pub async fn update(&mut self, update: &StateUpdate, now: NaiveDateTime) -> DbResult<()> {
        let Some(id) = self.row.as_ref().map(|r| r.id) else {
            return Ok(());
        };
        sqlx::query(
            "UPDATE image_scan_notification_states SET \
             is_baseline = COALESCE($2, is_baseline), \
             notified_ids = COALESCE($3, notified_ids), \
             digest_ids = COALESCE($4, digest_ids), \
             notified_candidates = COALESCE($5, notified_candidates), \
             check_reported = COALESCE($6, check_reported), \
             held_digests = COALESCE($7::smallint, held_digests), \
             updated_at = $8 WHERE id = $1",
        )
        .bind(id)
        .bind(update.is_baseline)
        .bind(update.notified_ids.as_ref().map(sqlx::types::Json))
        .bind(update.digest_ids.as_ref().map(sqlx::types::Json))
        .bind(update.notified_candidates.as_ref().map(sqlx::types::Json))
        .bind(update.check_reported)
        .bind(update.held_digests)
        .bind(now)
        .execute(&mut *self.tx)
        .await?;
        Ok(())
    }

    /// Ends the transaction and frees the lock.
    pub async fn commit(self) -> DbResult<()> {
        self.tx.commit().await?;
        Ok(())
    }
}

/// `ScanNotifications::isDeploying($owner)`: a deployment of the application that is not a
/// preview is queued or running.
pub async fn application_is_deploying(db: &Db, application_id: i64) -> DbResult<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM application_deployment_queues \
         WHERE application_id = $1 AND pull_request_id = 0 AND status IN ('queued', 'in_progress'))",
    )
    .bind(application_id.to_string())
    .fetch_one(db.pool())
    .await?)
}

/// `Team::query()->find($teamId) instanceof Team`.
pub async fn team_exists(db: &Db, team_id: i64) -> DbResult<bool> {
    Ok(
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM teams WHERE id = $1)")
            .bind(team_id)
            .fetch_one(db.pool())
            .await?,
    )
}

/// `ImageScanDigestJob::endingIgnores()` before the cache filter: the ignores of the team that
/// end after `now` and at the latest at `until`, the earliest first.
pub async fn ending_ignores(
    db: &Db,
    team_id: i64,
    now: NaiveDateTime,
    until: NaiveDateTime,
) -> DbResult<Vec<ImageScanIgnoreRow>> {
    Ok(sqlx::query_as(&format!(
        "SELECT {IGNORE_COLUMNS} FROM image_scan_ignores \
         WHERE team_id = $1 AND expires_at IS NOT NULL AND expires_at > $2 AND expires_at <= $3 \
         ORDER BY expires_at"
    ))
    .bind(team_id)
    .bind(now)
    .bind(until)
    .fetch_all(db.pool())
    .await?)
}

/// A server of a team as the digest names it.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct TeamScanServer {
    pub id: i64,
    pub name: String,
    pub uuid: String,
}

/// `ImageScanDigestJob::failingServers()` before the cache filter: the servers of the team that
/// scan images (`settings.is_image_scan_enabled`), in the order of their ids.
pub async fn scanning_servers_of_team(db: &Db, team_id: i64) -> DbResult<Vec<TeamScanServer>> {
    Ok(sqlx::query_as(
        "SELECT s.id, s.name::text AS name, s.uuid::text AS uuid FROM servers s \
         JOIN server_settings ss ON ss.server_id = s.id \
         WHERE s.team_id = $1 AND s.deleted_at IS NULL AND ss.is_image_scan_enabled ORDER BY s.id",
    )
    .bind(team_id)
    .fetch_all(db.pool())
    .await?)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn row() -> NotificationStateRow {
        NotificationStateRow {
            id: 1,
            resource_type: super::super::APPLICATION_MORPH.to_owned(),
            resource_id: 7,
            team_id: 1,
            notified_ids: Some(json!(["CVE-2026-0001", 7])),
            notified_candidates: None,
            digest_ids: Some(json!({"0": "CVE-2026-0002"})),
            is_baseline: false,
            check_reported: false,
            held_digests: 0,
            baseline_deployment_queue_id: None,
            created_at: None,
            updated_at: None,
        }
    }

    #[test]
    fn stored_lists_read_as_strings() {
        let state = row();
        assert_eq!(state.notified_ids(), vec!["CVE-2026-0001", "7"]);
        assert_eq!(state.digest_ids(), vec!["CVE-2026-0002"]);
        assert!(state.notified_candidates().is_empty());
    }

    #[test]
    fn names_the_event_columns() {
        assert_eq!(
            format!("{EVENT}_email_notifications"),
            "image_vulnerabilities_email_notifications"
        );
        assert_eq!(
            TELEGRAM_THREAD_COLUMN,
            "telegram_notifications_image_vulnerabilities_thread_id"
        );
    }
}
