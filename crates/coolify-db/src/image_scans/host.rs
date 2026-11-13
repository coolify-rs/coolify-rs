//! `server_host_scans`: the last scan of the OS packages of a server (`ServerHostScan`).
//!
//! One row per server (migration `2026_11_06_145819_create_server_host_scans_table`). A failed
//! scan sets `error_code` and keeps the findings of the last completed scan (`scanned_at`).
//! `error_context` holds the values of the error message.

use chrono::NaiveDateTime;
use serde_json::Value;

use crate::{Db, DbResult};

/// `ServerHostScan::MAX_STORED_FINDINGS`: relevant findings that are stored; the summary counts
/// all of them.
pub const MAX_STORED_FINDINGS: usize = 500;

/// One `server_host_scans` row.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct ServerHostScanRow {
    pub id: i64,
    pub server_id: i64,
    /// `ImageScanErrorCode` value of the last scan, null when it completed.
    pub error_code: Option<String>,
    pub error_context: Option<Value>,
    /// `{name, version}` of an end-of-life OS.
    pub eol: Option<Value>,
    /// `{critical, high, medium, low, unknown, fixable, exploited, likely, other_kernels, total,
    /// running_kernel}`.
    pub summary: Option<Value>,
    /// A list of `Finding::toArray()` rows.
    pub findings: Option<Value>,
    pub findings_truncated: bool,
    pub grype_db_built_at: Option<NaiveDateTime>,
    /// The time of the last completed scan.
    pub scanned_at: Option<NaiveDateTime>,
    pub created_at: Option<NaiveDateTime>,
    pub updated_at: Option<NaiveDateTime>,
}

const COLUMNS: &str = "id, server_id, error_code, error_context, eol, summary, findings, \
    findings_truncated, grype_db_built_at, scanned_at, created_at, updated_at";

/// `$server->hostScan` (`hasOne`).
pub async fn find(db: &Db, server_id: i64) -> DbResult<Option<ServerHostScanRow>> {
    Ok(sqlx::query_as(&format!(
        "SELECT {COLUMNS} FROM server_host_scans WHERE server_id = $1 LIMIT 1"
    ))
    .bind(server_id)
    .fetch_optional(db.pool())
    .await?)
}

/// What a completed host scan stores.
#[derive(Debug, Clone)]
pub struct CompletedHostScan<'a> {
    pub eol: Option<&'a Value>,
    pub summary: &'a Value,
    pub findings: &'a Value,
    pub findings_truncated: bool,
    pub grype_db_built_at: NaiveDateTime,
    pub scanned_at: NaiveDateTime,
}

/// `ServerHostScan::query()->updateOrCreate(['server_id' => $server->id], [...])` of a completed
/// scan: clears the error and replaces the findings.
pub async fn save_completed(
    db: &Db,
    server_id: i64,
    v: &CompletedHostScan<'_>,
    now: NaiveDateTime,
) -> DbResult<ServerHostScanRow> {
    db.ensure_writable()?;
    Ok(sqlx::query_as(&format!(
        "INSERT INTO server_host_scans (server_id, error_code, error_context, eol, summary, findings, \
         findings_truncated, grype_db_built_at, scanned_at, created_at, updated_at) \
         VALUES ($1, NULL, NULL, $2, $3, $4, $5, $6, $7, $8, $8) \
         ON CONFLICT (server_id) DO UPDATE SET error_code = NULL, error_context = NULL, eol = EXCLUDED.eol, \
         summary = EXCLUDED.summary, findings = EXCLUDED.findings, findings_truncated = EXCLUDED.findings_truncated, \
         grype_db_built_at = EXCLUDED.grype_db_built_at, scanned_at = EXCLUDED.scanned_at, updated_at = EXCLUDED.updated_at \
         RETURNING {COLUMNS}"
    ))
    .bind(server_id)
    .bind(v.eol.map(sqlx::types::Json))
    .bind(sqlx::types::Json(v.summary))
    .bind(sqlx::types::Json(v.findings))
    .bind(v.findings_truncated)
    .bind(v.grype_db_built_at)
    .bind(v.scanned_at)
    .bind(now)
    .fetch_one(db.pool())
    .await?)
}

/// `ScanHostPackages::recordFailure()`:
/// `updateOrCreate(['server_id' => $server->id], ['error_code' => $code, 'error_context' => $context ?: null])`.
/// Keeps the findings of the last completed scan. A row that already holds the same error is not
/// written again (Eloquent saves only changed values).
pub async fn save_failure(
    db: &Db,
    server_id: i64,
    error_code: &str,
    error_context: Option<&Value>,
    now: NaiveDateTime,
) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(
        "INSERT INTO server_host_scans (server_id, error_code, error_context, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $4) \
         ON CONFLICT (server_id) DO UPDATE SET error_code = EXCLUDED.error_code, \
         error_context = EXCLUDED.error_context, updated_at = EXCLUDED.updated_at \
         WHERE server_host_scans.error_code IS DISTINCT FROM EXCLUDED.error_code \
         OR server_host_scans.error_context IS DISTINCT FROM EXCLUDED.error_context",
    )
    .bind(server_id)
    .bind(error_code)
    .bind(error_context.map(sqlx::types::Json))
    .bind(now)
    .execute(db.pool())
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stores_at_most_500_findings() {
        assert_eq!(MAX_STORED_FINDINGS, 500);
    }

    #[test]
    fn reads_every_column_of_the_table() {
        assert_eq!(COLUMNS.split(", ").count(), 12);
        for column in [
            "server_id",
            "error_code",
            "error_context",
            "eol",
            "summary",
            "findings",
            "findings_truncated",
            "grype_db_built_at",
            "scanned_at",
        ] {
            assert!(COLUMNS.split(", ").any(|c| c.trim() == column), "{column}");
        }
    }
}
