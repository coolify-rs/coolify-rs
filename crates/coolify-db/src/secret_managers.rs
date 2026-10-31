//! `integration_tokens` lookups and `secret_manager_links` (`SecretManagerLink`). Only the source
//! coordinates are stored; no secret manager is contacted here.

use chrono::NaiveDateTime;
use coolify_domain::new_public_id;

use crate::{Db, DbResult};

/// An integration token of a secret manager provider.
#[derive(Clone)]
pub struct SecretManagerToken {
    pub id: i64,
    pub uuid: String,
    pub provider: String,
    pub token_encrypted: String,
    /// JSON text.
    pub capabilities: String,
}

impl std::fmt::Debug for SecretManagerToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecretManagerToken")
            .field("uuid", &self.uuid)
            .field("provider", &self.provider)
            .finish_non_exhaustive()
    }
}

/// `IntegrationToken::where('team_id', ...)->where('uuid', ...)->whereIn('provider', SECRET_MANAGER_PROVIDERS)->first()`.
pub async fn token_in_team(
    db: &Db,
    team_id: i64,
    uuid: &str,
) -> DbResult<Option<SecretManagerToken>> {
    let row: Option<(i64, String, String, String, String)> = sqlx::query_as(
        "SELECT id, uuid, provider, token, capabilities::text FROM integration_tokens \
         WHERE team_id = $1 AND uuid = $2 AND provider IN ('doppler', 'infisical', 'vault') LIMIT 1",
    )
    .bind(team_id)
    .bind(uuid)
    .fetch_optional(db.pool())
    .await?;
    Ok(row.map(
        |(id, uuid, provider, token_encrypted, capabilities)| SecretManagerToken {
            id,
            uuid,
            provider,
            token_encrypted,
            capabilities,
        },
    ))
}

/// `$resource->secretManagerLink()->updateOrCreate([], [...])`. `settings` is the JSON text
/// `json_encode` gives; a row whose token and settings are the same is left alone.
pub async fn update_or_create_link(
    db: &Db,
    morph: &str,
    resource_id: i64,
    token_id: i64,
    settings: Option<&str>,
    now: NaiveDateTime,
) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(
        "INSERT INTO secret_manager_links (uuid, resourceable_type, resourceable_id, integration_token_id, settings, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5::json, $6, $6) \
         ON CONFLICT (resourceable_type, resourceable_id) DO UPDATE SET \
         integration_token_id = EXCLUDED.integration_token_id, settings = EXCLUDED.settings, updated_at = EXCLUDED.updated_at \
         WHERE secret_manager_links.integration_token_id IS DISTINCT FROM EXCLUDED.integration_token_id \
         OR secret_manager_links.settings::jsonb IS DISTINCT FROM EXCLUDED.settings::jsonb",
    )
    .bind(new_public_id())
    .bind(morph)
    .bind(resource_id)
    .bind(token_id)
    .bind(settings)
    .bind(now)
    .execute(db.pool())
    .await?;
    Ok(())
}
