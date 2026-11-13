//! Queries of the version checks (`RefreshCandidates`): the daily budget of candidate scans from
//! a registry, kept per server in `server_settings` (`image_candidate_scans_today`,
//! `image_candidate_scans_date`).

use chrono::{NaiveDate, NaiveDateTime};

use crate::{Db, DbResult};

/// The candidate scan counters of a server: the scans counted on `date`. `None` when the server
/// has no settings row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::FromRow)]
pub struct CandidateScanCounters {
    /// `image_candidate_scans_today`.
    pub scans_today: i64,
    /// `image_candidate_scans_date`: the day the count belongs to.
    pub date: Option<NaiveDate>,
}

/// `ServerSetting::query()->where('server_id', $id)->first(['image_candidate_scans_today',
/// 'image_candidate_scans_date'])` of `RefreshCandidates::dailyScansLeft()`.
pub async fn candidate_scan_counters(
    db: &Db,
    server_id: i64,
) -> DbResult<Option<CandidateScanCounters>> {
    Ok(sqlx::query_as(
        "SELECT image_candidate_scans_today::bigint AS scans_today, \
         image_candidate_scans_date::date AS date \
         FROM server_settings WHERE server_id = $1 LIMIT 1",
    )
    .bind(server_id)
    .fetch_optional(db.pool())
    .await?)
}

/// `RefreshCandidates::reserveDailyScan($server)`: counts a registry scan against the daily
/// limit of the server. A count of an earlier day (or without a day) starts again at 0 first.
/// False when the limit is reached or the server has no settings row.
///
/// Both statements write `updated_at`, as Eloquent's `update()` and `increment()` do.
pub async fn reserve_candidate_scan(
    db: &Db,
    server_id: i64,
    today: NaiveDate,
    daily_limit: i64,
    now: NaiveDateTime,
) -> DbResult<bool> {
    db.ensure_writable()?;
    sqlx::query(
        "UPDATE server_settings SET image_candidate_scans_today = 0, \
         image_candidate_scans_date = $2::date, updated_at = $3 \
         WHERE server_id = $1 \
         AND (image_candidate_scans_date IS NULL OR image_candidate_scans_date::date < $2::date)",
    )
    .bind(server_id)
    .bind(today)
    .bind(now)
    .execute(db.pool())
    .await?;
    let done = sqlx::query(
        "UPDATE server_settings SET image_candidate_scans_today = image_candidate_scans_today + 1, \
         updated_at = $3 \
         WHERE server_id = $1 AND image_candidate_scans_today < $2::bigint",
    )
    .bind(server_id)
    .bind(daily_limit)
    .bind(now)
    .execute(db.pool())
    .await?;
    Ok(done.rows_affected() > 0)
}
