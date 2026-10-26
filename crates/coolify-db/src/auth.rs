//! Sanctum personal access tokens and team membership.

use chrono::NaiveDateTime;
use coolify_compat::ParsedToken;
use coolify_compat::sanctum::{hash_token, verify_token};
use coolify_domain::{Role, TeamId};

use crate::{Db, DbResult};

/// `tokenable_type` written by Sanctum for user tokens (no morph map upstream).
pub const USER_MORPH: &str = "App\\Models\\User";

/// A valid (found, hash-verified, unexpired) token. Never holds the secret.
#[derive(Debug, Clone)]
pub struct AccessToken {
    pub id: i64,
    pub name: String,
    pub user_id: i64,
    /// Raw `team_id` column (a string upstream).
    pub team_id_raw: String,
    pub abilities_raw: Option<String>,
}

impl AccessToken {
    /// The token team, if the column holds an integer.
    pub fn team_id(&self) -> Option<TeamId> {
        self.team_id_raw.trim().parse().ok()
    }
}

#[derive(sqlx::FromRow)]
struct TokenRow {
    id: i64,
    name: String,
    tokenable_type: String,
    tokenable_id: i64,
    token: String,
    team_id: String,
    abilities: Option<String>,
    expires_at: Option<NaiveDateTime>,
}

const TOKEN_COLUMNS: &str =
    "id, name, tokenable_type, tokenable_id, token, team_id, abilities, expires_at";

/// Sanctum `PersonalAccessToken::findToken` + guard checks (expiry, tokenable).
pub async fn find_token(
    db: &Db,
    parsed: &ParsedToken<'_>,
    now: NaiveDateTime,
) -> DbResult<Option<AccessToken>> {
    let row: Option<TokenRow> = match parsed.id {
        Some(id) => {
            let row: Option<TokenRow> = sqlx::query_as(&format!(
                "SELECT {TOKEN_COLUMNS} FROM personal_access_tokens WHERE id = $1"
            ))
            .bind(id)
            .fetch_optional(db.pool())
            .await?;
            row.filter(|r| verify_token(&r.token, parsed.plaintext))
        }
        None => {
            sqlx::query_as(&format!(
                "SELECT {TOKEN_COLUMNS} FROM personal_access_tokens WHERE token = $1"
            ))
            .bind(hash_token(parsed.plaintext))
            .fetch_optional(db.pool())
            .await?
        }
    };
    let Some(row) = row else { return Ok(None) };
    if row.expires_at.is_some_and(|exp| exp <= now) {
        return Ok(None);
    }
    if row.tokenable_type != USER_MORPH {
        return Ok(None);
    }
    // The tokenable must still exist.
    let exists: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM users WHERE id = $1)")
        .bind(row.tokenable_id)
        .fetch_one(db.pool())
        .await?;
    if !exists {
        return Ok(None);
    }
    Ok(Some(AccessToken {
        id: row.id,
        name: row.name,
        user_id: row.tokenable_id,
        team_id_raw: row.team_id,
        abilities_raw: row.abilities,
    }))
}

/// Sanctum updates `last_used_at` on every authenticated request.
pub async fn touch_last_used(db: &Db, token_id: i64, now: NaiveDateTime) -> DbResult<()> {
    sqlx::query("UPDATE personal_access_tokens SET last_used_at = $2 WHERE id = $1")
        .bind(token_id)
        .bind(now)
        .execute(db.pool())
        .await?;
    Ok(())
}

/// Pivot role of `user_id` in `team_id`, if a member. Unknown role strings
/// are treated as non-members (fail closed).
pub async fn membership_role(db: &Db, team_id: TeamId, user_id: i64) -> DbResult<Option<Role>> {
    let role: Option<String> =
        sqlx::query_scalar("SELECT role FROM team_user WHERE team_id = $1 AND user_id = $2")
            .bind(team_id)
            .bind(user_id)
            .fetch_optional(db.pool())
            .await?;
    Ok(role.as_deref().and_then(Role::parse))
}
