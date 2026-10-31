//! `scheduled_job_states` and `scheduled_job_deliveries`: the rows behind
//! `ScheduledJobDeliveryService`. Upstream's scheduler uses the same rows on a shared database, so
//! each schedule occurrence runs once whichever scheduler records it first.

use chrono::{DateTime, NaiveDateTime, Utc};
use coolify_domain::new_public_id;
use serde_json::Value;

use crate::{Db, DbResult};

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Delivery {
    pub id: i64,
    pub uuid: String,
    pub schedule_key: String,
    pub scheduled_for: DateTime<Utc>,
    pub job_type: String,
    pub resource_id: i64,
    /// `payload` as text; see [`Delivery::payload_value`].
    pub payload: Option<String>,
    pub status: String,
    pub claim_token: Option<String>,
    pub enqueued_at: Option<DateTime<Utc>>,
    pub started_at: Option<DateTime<Utc>>,
    pub created_at: Option<NaiveDateTime>,
}

impl Delivery {
    pub fn payload_value(&self) -> Value {
        self.payload
            .as_deref()
            .and_then(|p| serde_json::from_str(p).ok())
            .unwrap_or(Value::Null)
    }
}

const COLUMNS: &str = "id, uuid, schedule_key, scheduled_for, job_type, resource_id, payload::text AS payload, \
    status, claim_token, enqueued_at, started_at, created_at";

/// What `ScheduledJobDelivery::create()` inserts.
#[derive(Debug)]
pub struct NewDelivery<'a> {
    pub job_type: &'a str,
    pub resource_id: i64,
    pub payload: &'a Value,
}

/// The transaction of `recordAndPublish()` and `recordSkipped()`. `is_duplicate` gets the state's
/// `last_scheduled_for` (`isDuplicateOccurrence()`). `Ok(None)` is a duplicate; otherwise the state
/// moves to `scheduled_for`, and the delivery is created when one is given.
pub async fn record_occurrence(
    db: &Db,
    schedule_key: &str,
    scheduled_for: DateTime<Utc>,
    is_duplicate: impl FnOnce(Option<DateTime<Utc>>) -> bool,
    delivery: Option<NewDelivery<'_>>,
    now: NaiveDateTime,
) -> DbResult<Option<Option<Delivery>>> {
    db.ensure_writable()?;
    let mut tx = db.pool().begin().await?;
    sqlx::query(
        "INSERT INTO scheduled_job_states (uuid, schedule_key, created_at, updated_at) \
         VALUES ($1, $2, $3, $3) ON CONFLICT DO NOTHING",
    )
    .bind(new_public_id())
    .bind(schedule_key)
    .bind(now)
    .execute(&mut *tx)
    .await?;
    let last: Option<DateTime<Utc>> = sqlx::query_scalar(
        "SELECT last_scheduled_for FROM scheduled_job_states WHERE schedule_key = $1 FOR UPDATE",
    )
    .bind(schedule_key)
    .fetch_one(&mut *tx)
    .await?;
    if is_duplicate(last) {
        tx.commit().await?;
        return Ok(None);
    }
    sqlx::query(
        "UPDATE scheduled_job_states SET last_scheduled_for = $2, updated_at = $3 WHERE schedule_key = $1",
    )
    .bind(schedule_key)
    .bind(scheduled_for)
    .bind(now)
    .execute(&mut *tx)
    .await?;
    let created = match delivery {
        None => None,
        Some(d) => Some(
            sqlx::query_as(&format!(
                "INSERT INTO scheduled_job_deliveries (uuid, schedule_key, scheduled_for, job_type, resource_id, payload, status, created_at, updated_at) \
                 VALUES ($1, $2, $3, $4, $5, $6, 'pending', $7, $7) RETURNING {COLUMNS}"
            ))
            .bind(new_public_id())
            .bind(schedule_key)
            .bind(scheduled_for)
            .bind(d.job_type)
            .bind(d.resource_id)
            .bind(sqlx::types::Json(d.payload))
            .bind(now)
            .fetch_one(&mut *tx)
            .await?,
        ),
    };
    tx.commit().await?;
    Ok(Some(created))
}

/// `publish()` when the resource is gone.
pub async fn mark_skipped(db: &Db, id: i64) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query("UPDATE scheduled_job_deliveries SET status = 'skipped' WHERE id = $1")
        .bind(id)
        .execute(db.pool())
        .await?;
    Ok(())
}

/// The end of `publish()`.
pub async fn mark_enqueued(db: &Db, id: i64, now: DateTime<Utc>) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(
        "UPDATE scheduled_job_deliveries SET status = 'enqueued', enqueued_at = $2, updated_at = $3 \
         WHERE id = $1 AND status = 'pending'",
    )
    .bind(id)
    .bind(now)
    .bind(now.naive_utc())
    .execute(db.pool())
    .await?;
    Ok(())
}

/// `claim()`.
pub async fn claim(db: &Db, uuid: &str, token: &str, now: DateTime<Utc>) -> DbResult<bool> {
    db.ensure_writable()?;
    let claimed = sqlx::query(
        "UPDATE scheduled_job_deliveries SET status = 'claimed', claim_token = $2, started_at = $3, updated_at = $4 \
         WHERE uuid = $1 AND status IN ('pending', 'enqueued')",
    )
    .bind(uuid)
    .bind(token)
    .bind(now)
    .bind(now.naive_utc())
    .execute(db.pool())
    .await?
    .rows_affected()
        == 1;
    if claimed {
        return Ok(true);
    }
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM scheduled_job_deliveries WHERE uuid = $1 AND status = 'claimed' AND claim_token = $2)",
    )
    .bind(uuid)
    .bind(token)
    .fetch_one(db.pool())
    .await?)
}

/// `complete()`: a finished occurrence has no row.
pub async fn complete(db: &Db, uuid: &str, token: &str) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query("DELETE FROM scheduled_job_deliveries WHERE uuid = $1 AND claim_token = $2")
        .bind(uuid)
        .bind(token)
        .execute(db.pool())
        .await?;
    Ok(())
}

/// `fail()`.
pub async fn fail(db: &Db, uuid: &str, token: &str, now: NaiveDateTime) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(
        "UPDATE scheduled_job_deliveries SET status = 'failed', updated_at = $3 WHERE uuid = $1 AND claim_token = $2",
    )
    .bind(uuid)
    .bind(token)
    .bind(now)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// `skip()`. Returns whether it moved.
pub async fn skip(db: &Db, uuid: &str, now: NaiveDateTime) -> DbResult<bool> {
    db.ensure_writable()?;
    Ok(sqlx::query(
        "UPDATE scheduled_job_deliveries SET status = 'skipped', updated_at = $2 \
         WHERE uuid = $1 AND status IN ('pending', 'enqueued')",
    )
    .bind(uuid)
    .bind(now)
    .execute(db.pool())
    .await?
    .rows_affected()
        == 1)
}

/// The query of `publishPending()`.
pub async fn pending(db: &Db, job_types: &[&str], limit: i64) -> DbResult<Vec<Delivery>> {
    Ok(sqlx::query_as(&format!(
        "SELECT {COLUMNS} FROM scheduled_job_deliveries WHERE status = 'pending' AND job_type = ANY($1) ORDER BY id LIMIT $2"
    ))
    .bind(job_types)
    .bind(limit)
    .fetch_all(db.pool())
    .await?)
}

/// The query of `recoverStaleEnqueued()`.
pub async fn stale_enqueued(
    db: &Db,
    job_types: &[&str],
    before: DateTime<Utc>,
    limit: i64,
) -> DbResult<Vec<Delivery>> {
    Ok(sqlx::query_as(&format!(
        "SELECT {COLUMNS} FROM scheduled_job_deliveries WHERE status = 'enqueued' AND job_type = ANY($1) \
         AND enqueued_at < $2 ORDER BY id LIMIT $3"
    ))
    .bind(job_types)
    .bind(before)
    .bind(limit)
    .fetch_all(db.pool())
    .await?)
}

/// The queries of `failInterruptedClaims()`. Returns the moved uuids.
pub async fn fail_interrupted_claims(
    db: &Db,
    job_types: &[&str],
    started_before: DateTime<Utc>,
    limit: i64,
    now: NaiveDateTime,
) -> DbResult<Vec<String>> {
    db.ensure_writable()?;
    Ok(sqlx::query_scalar(
        "UPDATE scheduled_job_deliveries SET status = 'failed', updated_at = $4 WHERE id IN ( \
           SELECT id FROM scheduled_job_deliveries WHERE status = 'claimed' AND job_type = ANY($1) \
           AND started_at < $2 ORDER BY id LIMIT $3) AND status = 'claimed' RETURNING uuid",
    )
    .bind(job_types)
    .bind(started_before)
    .bind(limit)
    .bind(now)
    .fetch_all(db.pool())
    .await?)
}

/// `$hasNewerOccurrence` of `recoverUnstartedOccurrence()`.
pub async fn has_newer_occurrence(
    db: &Db,
    schedule_key: &str,
    scheduled_for: DateTime<Utc>,
) -> DbResult<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM scheduled_job_states WHERE schedule_key = $1 AND last_scheduled_for > $2) \
         OR EXISTS (SELECT 1 FROM scheduled_job_deliveries WHERE schedule_key = $1 AND scheduled_for > $2)",
    )
    .bind(schedule_key)
    .bind(scheduled_for)
    .fetch_one(db.pool())
    .await?)
}

/// The atomic move of `recoverUnstartedOccurrence()`: only one scheduler moves an occurrence.
pub async fn move_unstarted(
    db: &Db,
    delivery: &Delivery,
    stale_before: DateTime<Utc>,
    status: &str,
    now: NaiveDateTime,
) -> DbResult<bool> {
    db.ensure_writable()?;
    let moved = if delivery.status == "enqueued" {
        sqlx::query(
            "UPDATE scheduled_job_deliveries SET status = $2, updated_at = $3 \
             WHERE id = $1 AND status = 'enqueued' AND enqueued_at < $4",
        )
        .bind(delivery.id)
        .bind(status)
        .bind(now)
        .bind(stale_before)
        .execute(db.pool())
        .await?
    } else {
        sqlx::query(
            "UPDATE scheduled_job_deliveries SET status = $2, updated_at = $3 \
             WHERE id = $1 AND status = $4 AND created_at < $5",
        )
        .bind(delivery.id)
        .bind(status)
        .bind(now)
        .bind(&delivery.status)
        .bind(stale_before.naive_utc())
        .execute(db.pool())
        .await?
    };
    Ok(moved.rows_affected() == 1)
}

/// `deleteOldOccurrences()`.
pub async fn delete_old(db: &Db, now: DateTime<Utc>) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(
        "UPDATE scheduled_job_deliveries SET status = 'failed', updated_at = $2 \
         WHERE status = 'claimed' AND updated_at < $1",
    )
    .bind((now - chrono::Duration::days(2)).naive_utc())
    .bind(now.naive_utc())
    .execute(db.pool())
    .await?;
    sqlx::query(
        "DELETE FROM scheduled_job_deliveries WHERE status IN ('failed', 'skipped') AND created_at < $1",
    )
    .bind((now - chrono::Duration::days(30)).naive_utc())
    .execute(db.pool())
    .await?;
    Ok(())
}
