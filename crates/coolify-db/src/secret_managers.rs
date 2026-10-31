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

/// A `secret_manager_links` row with its `integration_tokens` row.
#[derive(Clone)]
#[allow(missing_debug_implementations)]
pub struct LinkWithToken {
    pub link_id: i64,
    pub token_id: i64,
    pub token_uuid: String,
    pub token_name: String,
    pub provider: String,
    pub token_team_id: i64,
    pub token_encrypted: String,
    /// JSON text.
    pub metadata: Option<String>,
    /// JSON text.
    pub settings: Option<String>,
}

/// `$resource->secretManagerLink()->with('integrationToken')->first()`.
pub async fn link_with_token(
    db: &Db,
    morph: &str,
    resource_id: i64,
) -> DbResult<Option<LinkWithToken>> {
    type Row = (
        i64,
        i64,
        String,
        String,
        String,
        i64,
        String,
        Option<String>,
        Option<String>,
    );
    let row: Option<Row> = sqlx::query_as(
        "SELECT l.id, t.id, t.uuid, t.name, t.provider, t.team_id, t.token, t.metadata::text, l.settings::text \
         FROM secret_manager_links l JOIN integration_tokens t ON t.id = l.integration_token_id \
         WHERE l.resourceable_type = $1 AND l.resourceable_id = $2 LIMIT 1",
    )
    .bind(morph)
    .bind(resource_id)
    .fetch_optional(db.pool())
    .await?;
    Ok(row.map(
        |(
            link_id,
            token_id,
            token_uuid,
            token_name,
            provider,
            token_team_id,
            token_encrypted,
            metadata,
            settings,
        )| LinkWithToken {
            link_id,
            token_id,
            token_uuid,
            token_name,
            provider,
            token_team_id,
            token_encrypted,
            metadata,
            settings,
        },
    ))
}

/// A secret manager token of a team, as the section lists it.
#[derive(Debug, Clone)]
pub struct ListedToken {
    pub id: i64,
    pub uuid: String,
    pub name: String,
    pub provider: String,
}

/// `$availableTokens`: the secret manager tokens of a team whose `capabilities` has `secrets`.
pub async fn tokens_of_team(db: &Db, team_id: i64) -> DbResult<Vec<ListedToken>> {
    let rows: Vec<(i64, String, String, String)> = sqlx::query_as(
        "SELECT id, uuid, name, provider FROM integration_tokens \
         WHERE team_id = $1 AND provider IN ('doppler', 'infisical', 'vault') AND capabilities::jsonb ? 'secrets' ORDER BY id",
    )
    .bind(team_id)
    .fetch_all(db.pool())
    .await?;
    Ok(rows
        .into_iter()
        .map(|(id, uuid, name, provider)| ListedToken {
            id,
            uuid,
            name,
            provider,
        })
        .collect())
}

/// `$link->update(['settings' => ...])`.
pub async fn update_link_settings(
    db: &Db,
    link_id: i64,
    settings: Option<&str>,
    now: NaiveDateTime,
) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(
        "UPDATE secret_manager_links SET settings = $2::json, updated_at = $3 WHERE id = $1",
    )
    .bind(link_id)
    .bind(settings)
    .bind(now)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// `$resource->secretManagerLink()->delete()`.
pub async fn delete_link(db: &Db, morph: &str, resource_id: i64) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(
        "DELETE FROM secret_manager_links WHERE resourceable_type = $1 AND resourceable_id = $2",
    )
    .bind(morph)
    .bind(resource_id)
    .execute(db.pool())
    .await?;
    Ok(())
}
