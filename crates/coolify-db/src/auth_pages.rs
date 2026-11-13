//! Rows of the sign-in screens: `CreateNewUser`, the password broker's `password_reset_tokens`,
//! `ForcePasswordReset`, email verification, passkey sign-in, `OauthSetting` and `OauthIdentity`.

use chrono::NaiveDateTime;
use coolify_domain::TeamId;

use crate::{Db, DbResult};

/// `User::count()`.
pub async fn user_count(db: &Db) -> DbResult<i64> {
    Ok(sqlx::query_scalar("SELECT count(*) FROM users")
        .fetch_one(db.pool())
        .await?)
}

/// `User::create` and the `created` hook: the personal team. The caller creates its settings rows.
pub async fn create_user(
    db: &Db,
    name: &str,
    email: &str,
    password_hash: &str,
    now: NaiveDateTime,
) -> DbResult<(i64, TeamId)> {
    db.ensure_writable()?;
    let mut tx = db.pool().begin().await?;
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO users (name, email, password, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $4) RETURNING id",
    )
    .bind(name)
    .bind(email)
    .bind(password_hash)
    .bind(now)
    .fetch_one(&mut *tx)
    .await?;
    let team: TeamId = sqlx::query_scalar(
        "INSERT INTO teams (name, personal_team, show_boarding, is_mcp_server_enabled, \
           is_build_server_fallback_enabled, is_two_factor_required, created_at, updated_at) \
         VALUES ($1, true, true, true, true, false, $2, $2) RETURNING id",
    )
    .bind(coolify_domain::naming::strip_tags(Some(&format!(
        "{name}'s Team"
    ))))
    .bind(now)
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query("INSERT INTO team_user (team_id, user_id, role) VALUES ($1, $2, 'owner')")
        .bind(team)
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok((id, team))
}

/// The first user: `id = 0`, owner of the root team, and `is_registration_enabled` off. The
/// bool says the root team was created here and needs its settings rows.
pub async fn create_root_user(
    db: &Db,
    name: &str,
    email: &str,
    password_hash: &str,
    now: NaiveDateTime,
) -> DbResult<bool> {
    db.ensure_writable()?;
    let mut tx = db.pool().begin().await?;
    sqlx::query(
        "INSERT INTO users (id, name, email, password, created_at, updated_at) \
         VALUES (0, $1, $2, $3, $4, $4)",
    )
    .bind(name)
    .bind(email)
    .bind(password_hash)
    .bind(now)
    .execute(&mut *tx)
    .await?;
    let updated = sqlx::query(
        "UPDATE teams SET name = 'Root Team', personal_team = true, show_boarding = true, \
         updated_at = $1 WHERE id = 0",
    )
    .bind(now)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if updated == 0 {
        sqlx::query(
            "INSERT INTO teams (id, name, personal_team, show_boarding, is_mcp_server_enabled, \
               is_build_server_fallback_enabled, is_two_factor_required, created_at, updated_at) \
             VALUES (0, 'Root Team', true, true, true, true, false, $1, $1)",
        )
        .bind(now)
        .execute(&mut *tx)
        .await?;
    }
    sqlx::query("INSERT INTO team_user (team_id, user_id, role) VALUES (0, 0, 'owner')")
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE instance_settings SET is_registration_enabled = false, updated_at = $1")
        .bind(now)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(updated == 0)
}

/// `createRootTeamOnlyUser`: no personal team, a member of the root team. `None` without one.
pub async fn create_root_team_member(
    db: &Db,
    name: &str,
    email: &str,
    password_hash: &str,
    now: NaiveDateTime,
) -> DbResult<Option<i64>> {
    db.ensure_writable()?;
    let mut tx = db.pool().begin().await?;
    let root: Option<i64> = sqlx::query_scalar("SELECT id FROM teams WHERE id = 0")
        .fetch_optional(&mut *tx)
        .await?;
    if root.is_none() {
        return Ok(None);
    }
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO users (name, email, password, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $4) RETURNING id",
    )
    .bind(name)
    .bind(email)
    .bind(password_hash)
    .bind(now)
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query("INSERT INTO team_user (team_id, user_id, role) VALUES (0, $1, 'member')")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Some(id))
}

/// `markEmailAsVerified`. False when the address was verified already.
pub async fn mark_email_verified(db: &Db, id: i64, now: NaiveDateTime) -> DbResult<bool> {
    db.ensure_writable()?;
    Ok(sqlx::query(
        "UPDATE users SET email_verified_at = $1, updated_at = $1 \
         WHERE id = $2 AND email_verified_at IS NULL",
    )
    .bind(now)
    .bind(id)
    .execute(db.pool())
    .await?
    .rows_affected()
        > 0)
}

// ---- Password broker ----

/// `DatabaseTokenRepository`: the (bcrypt) token and when it was created.
pub async fn reset_token(
    db: &Db,
    email: &str,
) -> DbResult<Option<(String, Option<NaiveDateTime>)>> {
    Ok(
        sqlx::query_as("SELECT token, created_at FROM password_reset_tokens WHERE email = $1")
            .bind(email)
            .fetch_optional(db.pool())
            .await?,
    )
}

/// `DatabaseTokenRepository::create`: one row per email.
pub async fn put_reset_token(db: &Db, email: &str, hash: &str, now: NaiveDateTime) -> DbResult<()> {
    db.ensure_writable()?;
    let mut tx = db.pool().begin().await?;
    sqlx::query("DELETE FROM password_reset_tokens WHERE email = $1")
        .bind(email)
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO password_reset_tokens (email, token, created_at) VALUES ($1, $2, $3)")
        .bind(email)
        .bind(hash)
        .bind(now)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

pub async fn delete_reset_token(db: &Db, email: &str) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query("DELETE FROM password_reset_tokens WHERE email = $1")
        .bind(email)
        .execute(db.pool())
        .await?;
    Ok(())
}

/// `ResetUserPassword::reset` and `CompletePasswordReset`. The new password also revokes the
/// user's API tokens, as `DeletesUserSessions` does for a changed password.
pub async fn reset_password(
    db: &Db,
    id: i64,
    hash: &str,
    remember_token: &str,
    now: NaiveDateTime,
) -> DbResult<()> {
    db.ensure_writable()?;
    let mut tx = db.pool().begin().await?;
    sqlx::query(
        // `force_password_reset => false` (96ed09f0c): a reset password clears a forced reset.
        "UPDATE users SET password = $1, remember_token = $2, force_password_reset = false, updated_at = $3 WHERE id = $4",
    )
    .bind(hash)
    .bind(remember_token)
    .bind(now)
    .bind(id)
    .execute(&mut *tx)
    .await?;
    crate::team_pages::revoke_user_tokens(&mut tx, id).await?;
    tx.commit().await?;
    Ok(())
}

/// `ForcePasswordReset::submit`. The new password also revokes the user's API tokens, as
/// `DeletesUserSessions` does for a changed password.
pub async fn finish_forced_reset(db: &Db, id: i64, hash: &str, now: NaiveDateTime) -> DbResult<()> {
    db.ensure_writable()?;
    let mut tx = db.pool().begin().await?;
    sqlx::query(
        "UPDATE users SET password = $1, force_password_reset = false, updated_at = $2 WHERE id = $3",
    )
    .bind(hash)
    .bind(now)
    .bind(id)
    .execute(&mut *tx)
    .await?;
    crate::team_pages::revoke_user_tokens(&mut tx, id).await?;
    tx.commit().await?;
    Ok(())
}

/// `Boarding\Index::skipBoarding`.
pub async fn finish_boarding(db: &Db, team_id: TeamId, now: NaiveDateTime) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query("UPDATE teams SET show_boarding = false, updated_at = $1 WHERE id = $2")
        .bind(now)
        .bind(team_id)
        .execute(db.pool())
        .await?;
    Ok(())
}

// ---- Passkeys ----

/// A stored passkey, by `credential_id`: id, owner and the `credential` JSON.
pub async fn passkey_by_credential(
    db: &Db,
    credential_id: &str,
) -> DbResult<Option<(i64, i64, String)>> {
    Ok(sqlx::query_as(
        "SELECT id, user_id, credential::text FROM passkeys WHERE credential_id = $1",
    )
    .bind(credential_id)
    .fetch_optional(db.pool())
    .await?)
}

/// `VerifyPasskey::updatePasskey`.
pub async fn update_passkey_use(
    db: &Db,
    id: i64,
    credential: &str,
    now: NaiveDateTime,
) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(
        "UPDATE passkeys SET credential = $1::json, last_used_at = $2, updated_at = $2 WHERE id = $3",
    )
    .bind(credential)
    .bind(now)
    .bind(id)
    .execute(db.pool())
    .await?;
    Ok(())
}

// ---- OAuth ----

/// An `oauth_settings` row. `client_secret` stays encrypted here.
#[derive(Clone, sqlx::FromRow)]
pub struct OauthRow {
    pub id: i64,
    pub provider: String,
    pub enabled: bool,
    pub client_id: Option<String>,
    pub client_secret: Option<String>,
    pub redirect_uri: Option<String>,
    pub tenant: Option<String>,
    pub base_url: Option<String>,
    pub custom_label: Option<String>,
    pub scopes: Option<String>,
    pub allow_registration: bool,
    pub auto_join_root_team: bool,
    pub require_email_verified: bool,
    pub use_pkce: bool,
    pub clock_skew_seconds: i16,
}

impl std::fmt::Debug for OauthRow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OauthRow")
            .field("provider", &self.provider)
            .finish_non_exhaustive()
    }
}

const OAUTH_COLUMNS: &str = "id, provider, enabled, client_id, client_secret, redirect_uri, tenant, base_url, \
    custom_label, scopes, allow_registration, auto_join_root_team, require_email_verified, use_pkce, clock_skew_seconds";

/// `OauthSetting::all()`.
pub async fn oauth_settings(db: &Db) -> DbResult<Vec<OauthRow>> {
    Ok(sqlx::query_as(&format!(
        "SELECT {OAUTH_COLUMNS} FROM oauth_settings ORDER BY id"
    ))
    .fetch_all(db.pool())
    .await?)
}

pub async fn oauth_setting(db: &Db, provider: &str) -> DbResult<Option<OauthRow>> {
    Ok(sqlx::query_as(&format!(
        "SELECT {OAUTH_COLUMNS} FROM oauth_settings WHERE provider = $1"
    ))
    .bind(provider)
    .fetch_optional(db.pool())
    .await?)
}

/// `$oauth->save()`.
pub async fn save_oauth_setting(db: &Db, row: &OauthRow, now: NaiveDateTime) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(
        "UPDATE oauth_settings SET enabled = $1, client_id = $2, client_secret = $3, redirect_uri = $4, \
         tenant = $5, base_url = $6, custom_label = $7, scopes = $8, allow_registration = $9, \
         auto_join_root_team = $10, require_email_verified = $11, use_pkce = $12, clock_skew_seconds = $13, \
         updated_at = $14 WHERE id = $15",
    )
    .bind(row.enabled)
    .bind(&row.client_id)
    .bind(&row.client_secret)
    .bind(&row.redirect_uri)
    .bind(&row.tenant)
    .bind(&row.base_url)
    .bind(&row.custom_label)
    .bind(&row.scopes)
    .bind(row.allow_registration)
    .bind(row.auto_join_root_team)
    .bind(row.require_email_verified)
    .bind(row.use_pkce)
    .bind(row.clock_skew_seconds)
    .bind(now)
    .bind(row.id)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// `instanceSettings()->update(['disable_registration_when_oauth_enabled' => ...])`.
pub async fn set_disable_registration(db: &Db, value: bool, now: NaiveDateTime) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(
        "UPDATE instance_settings SET disable_registration_when_oauth_enabled = $1, updated_at = $2",
    )
    .bind(value)
    .bind(now)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// The user of `OauthIdentity::where($identityKey)`, with the identity's id.
pub async fn identity(
    db: &Db,
    provider: &str,
    issuer: &str,
    provider_user_id: &str,
) -> DbResult<Option<(i64, i64)>> {
    Ok(sqlx::query_as(
        "SELECT id, user_id FROM oauth_identities \
         WHERE provider = $1 AND issuer = $2 AND provider_user_id = $3",
    )
    .bind(provider)
    .bind(issuer)
    .bind(provider_user_id)
    .fetch_optional(db.pool())
    .await?)
}

/// `$identity->update(['email', 'raw_claims', 'last_login_at'])`.
pub async fn touch_identity(
    db: &Db,
    id: i64,
    email: &str,
    raw_claims: &str,
    now: NaiveDateTime,
) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(
        "UPDATE oauth_identities SET email = $1, raw_claims = $2::json, last_login_at = $3, \
         updated_at = $3 WHERE id = $4",
    )
    .bind(email)
    .bind(raw_claims)
    .bind(now)
    .bind(id)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// `OauthIdentity::create`. False on the unique key: another request linked it first.
#[allow(clippy::too_many_arguments)]
pub async fn create_identity(
    db: &Db,
    user_id: i64,
    provider: &str,
    issuer: &str,
    provider_user_id: &str,
    email: &str,
    raw_claims: &str,
    now: NaiveDateTime,
) -> DbResult<bool> {
    db.ensure_writable()?;
    Ok(sqlx::query(
        "INSERT INTO oauth_identities (user_id, provider, issuer, provider_user_id, email, raw_claims, \
           last_login_at, created_at, updated_at) VALUES ($1, $2, $3, $4, $5, $6::json, $7, $7, $7) \
         ON CONFLICT (provider, issuer, provider_user_id) DO NOTHING",
    )
    .bind(user_id)
    .bind(provider)
    .bind(issuer)
    .bind(provider_user_id)
    .bind(email)
    .bind(raw_claims)
    .bind(now)
    .execute(db.pool())
    .await?
    .rows_affected()
        > 0)
}

/// `$user->oauthIdentities()->exists()`.
pub async fn has_identity(db: &Db, user_id: i64) -> DbResult<bool> {
    Ok(
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM oauth_identities WHERE user_id = $1)")
            .bind(user_id)
            .fetch_one(db.pool())
            .await?,
    )
}

/// `users.created_before_oauth_identities`.
pub async fn created_before_identities(db: &Db, user_id: i64) -> DbResult<bool> {
    Ok(sqlx::query_scalar(
        "SELECT COALESCE(created_before_oauth_identities, false) FROM users WHERE id = $1",
    )
    .bind(user_id)
    .fetch_one(db.pool())
    .await?)
}

pub async fn clear_created_before_identities(db: &Db, user_id: i64) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(
        "UPDATE users SET created_before_oauth_identities = false, updated_at = now() WHERE id = $1",
    )
    .bind(user_id)
    .execute(db.pool())
    .await?;
    Ok(())
}
