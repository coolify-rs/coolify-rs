//! `instance_settings` (singleton row id 0).

use chrono::NaiveDateTime;

use crate::{Db, DbResult};

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ApiSettings {
    pub is_api_enabled: bool,
    pub allowed_ips: Option<String>,
}

pub async fn api_settings(db: &Db) -> DbResult<ApiSettings> {
    // Upstream uses InstanceSettings::findOrFail(0); a missing row is a 500 there too.
    Ok(
        sqlx::query_as("SELECT is_api_enabled, allowed_ips FROM instance_settings WHERE id = 0")
            .fetch_one(db.pool())
            .await?,
    )
}

/// `webhook_allowed_internal_hosts` (as text) and `webhook_allow_localhost`.
pub async fn webhook_policy(db: &Db) -> DbResult<Option<(Option<String>, Option<bool>)>> {
    Ok(sqlx::query_as(
        "SELECT webhook_allowed_internal_hosts::text, webhook_allow_localhost FROM instance_settings WHERE id = 0",
    )
    .fetch_optional(db.pool())
    .await?)
}

/// `OtherController@disable_api`.
pub async fn disable_api(db: &Db, now: NaiveDateTime) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(
        "UPDATE instance_settings SET is_api_enabled = false, updated_at = $1 \
         WHERE id = 0 AND is_api_enabled IS DISTINCT FROM false",
    )
    .bind(now)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// What deployments read from `instance_settings` (id 0).
#[derive(Debug, Clone, Default)]
pub struct DeploymentSettings {
    pub docker_registry_url: Option<String>,
    pub helper_version: Option<String>,
    pub public_ipv4: Option<String>,
    pub public_ipv6: Option<String>,
}

pub async fn deployment_settings(db: &Db) -> DbResult<DeploymentSettings> {
    type Row = (
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    );
    let r: Option<Row> = sqlx::query_as(
        "SELECT docker_registry_url, helper_version, public_ipv4, public_ipv6 FROM instance_settings WHERE id = 0",
    )
    .fetch_optional(db.pool())
    .await?;
    Ok(r.map(
        |(docker_registry_url, helper_version, public_ipv4, public_ipv6)| DeploymentSettings {
            docker_registry_url,
            helper_version,
            public_ipv4,
            public_ipv6,
        },
    )
    .unwrap_or_default())
}
