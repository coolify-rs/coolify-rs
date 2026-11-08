//! Rows the server and destination pages of the web UI read that no other module reads.

use crate::records::{Record, parse_row};
use crate::{Db, DbResult};

/// A resource connected to a destination (`Destination\Resources::resourceRow`).
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct DestinationResource {
    pub uuid: String,
    pub name: Option<String>,
    pub project_name: Option<String>,
    pub project_uuid: Option<String>,
    pub environment_name: Option<String>,
    pub environment_uuid: Option<String>,
}

/// `$destination->applications` (and the other relations): rows of `table` on the destination.
pub async fn destination_resources(
    db: &Db,
    table: &str,
    destination_id: i64,
    destination_class: &str,
) -> DbResult<Vec<DestinationResource>> {
    Ok(sqlx::query_as(&format!(
        "SELECT r.uuid, r.name, p.name AS project_name, p.uuid AS project_uuid, \
                e.name AS environment_name, e.uuid AS environment_uuid \
           FROM {table} r \
           LEFT JOIN environments e ON e.id = r.environment_id \
           LEFT JOIN projects p ON p.id = e.project_id \
          WHERE r.destination_id = $1 AND r.destination_type = $2 AND r.deleted_at IS NULL"
    ))
    .bind(destination_id)
    .bind(destination_class)
    .fetch_all(db.pool())
    .await?)
}

/// Rows of a `SELECT row_to_json(t)::text ... $1` query, for tables without a model module.
pub async fn json_rows(db: &Db, sql: &str, bind: i64) -> DbResult<Vec<Record>> {
    let rows: Vec<String> = sqlx::query_scalar(sql)
        .bind(bind)
        .fetch_all(db.pool())
        .await?;
    Ok(rows.iter().filter_map(|r| parse_row(r).ok()).collect())
}

/// `$server->sslCertificates()->where('is_ca_certificate', true)->first()`.
pub async fn ca_certificate_row(db: &Db, server_id: i64) -> DbResult<Option<Record>> {
    Ok(json_rows(
        db,
        "SELECT row_to_json(c)::text FROM ssl_certificates c \
         WHERE server_id = $1 AND is_ca_certificate = true LIMIT 1",
        server_id,
    )
    .await?
    .into_iter()
    .next())
}

/// `$caCertificate->ssl_certificate = ...; save()`.
pub async fn update_ca_certificate(
    db: &Db,
    id: i64,
    cipher: &str,
    now: chrono::NaiveDateTime,
) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query("UPDATE ssl_certificates SET ssl_certificate = $1, updated_at = $2 WHERE id = $3")
        .bind(cipher)
        .bind(now)
        .bind(id)
        .execute(db.pool())
        .await?;
    Ok(())
}

/// One row of `DeploymentsIndicator::deployments`.
#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize)]
pub struct ActiveDeployment {
    pub id: i64,
    pub application_uuid: Option<String>,
    pub application_name: Option<String>,
    pub deployment_url: Option<String>,
    pub pull_request_id: i32,
    pub server_name: Option<String>,
    pub status: String,
    pub project_name: Option<String>,
    pub environment_name: Option<String>,
    /// For `visibleToCurrentActor()`; not part of the page data.
    #[serde(skip)]
    pub environment_id: i64,
}

/// `DeploymentsIndicator::deployments`: `ownedByTeam` goes through the application, also a deleted one.
pub async fn active_deployments(db: &Db, team_id: i64) -> DbResult<Vec<ActiveDeployment>> {
    Ok(sqlx::query_as::<_, ActiveDeployment>(
        "SELECT q.id, a.uuid AS application_uuid, q.application_name, q.deployment_url, q.pull_request_id, \
                q.server_name, q.status, p.name AS project_name, e.name AS environment_name, \
                e.id AS environment_id \
           FROM application_deployment_queues q \
           JOIN applications a ON a.id::text = q.application_id \
           JOIN environments e ON e.id = a.environment_id \
           JOIN projects p ON p.id = e.project_id \
          WHERE p.team_id = $1 AND q.status IN ('in_progress', 'queued') \
          ORDER BY q.id",
    )
    .bind(team_id)
    .fetch_all(db.pool())
    .await?
    .into_iter()
    // `visibleToCurrentActor()`.
    .filter(|d: &ActiveDeployment| {
        crate::access::resource_visible(
            Some(d.environment_id),
            d.application_uuid.as_deref().unwrap_or_default(),
        )
    })
    .collect())
}
