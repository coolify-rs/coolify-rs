//! Rows behind `/profile`: `App\Livewire\Profile\{Index, Passkeys}`, the `User` email change
//! columns, Fortify's two-factor columns, and `passkeys`.

use chrono::NaiveDateTime;

use crate::{Db, DbResult};

/// Profile columns of the signed-in user. Secrets stay encrypted here.
#[derive(sqlx::FromRow)]
pub struct ProfileUser {
    pub id: i64,
    pub name: String,
    pub email: String,
    pub password: Option<String>,
    pub two_factor_secret: Option<String>,
    pub two_factor_recovery_codes: Option<String>,
    pub two_factor_confirmed_at: Option<NaiveDateTime>,
    pub pending_email: Option<String>,
    pub email_change_code: Option<String>,
    pub email_change_code_expires_at: Option<NaiveDateTime>,
    pub avatar_path: Option<String>,
    pub avatar_storage_type: Option<String>,
    pub updated_at: Option<NaiveDateTime>,
}

impl std::fmt::Debug for ProfileUser {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProfileUser")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

pub async fn get(db: &Db, id: i64) -> DbResult<Option<ProfileUser>> {
    Ok(sqlx::query_as(
        "SELECT id, name, email, password, two_factor_secret, two_factor_recovery_codes, \
           two_factor_confirmed_at, pending_email, email_change_code, email_change_code_expires_at, \
           avatar_path, avatar_storage_type, updated_at FROM users WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(db.pool())
    .await?)
}

/// `Auth::user()->update(['name' => ...])`: `updated_at` moves only when the name changes.
pub async fn set_name(db: &Db, id: i64, name: &str, now: NaiveDateTime) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(
        "UPDATE users SET name = $2, updated_at = $3 WHERE id = $1 AND name IS DISTINCT FROM $2",
    )
    .bind(id)
    .bind(name)
    .bind(now)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// `requestEmailChange`: the pending address, its code and when the code expires.
pub async fn set_email_change(
    db: &Db,
    id: i64,
    new_email: &str,
    code: &str,
    expires_at: NaiveDateTime,
    now: NaiveDateTime,
) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(
        "UPDATE users SET pending_email = $2, email_change_code = $3, email_change_code_expires_at = $4, updated_at = $5 WHERE id = $1",
    )
    .bind(id)
    .bind(new_email.to_lowercase())
    .bind(code)
    .bind(expires_at)
    .bind(now)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// `confirmEmailChange`.
pub async fn confirm_email_change(
    db: &Db,
    id: i64,
    email: &str,
    now: NaiveDateTime,
) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(
        "UPDATE users SET email = $2, pending_email = NULL, email_change_code = NULL, \
           email_change_code_expires_at = NULL, updated_at = $3 WHERE id = $1",
    )
    .bind(id)
    .bind(email.to_lowercase())
    .bind(now)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// `clearEmailChangeRequest`: `updated_at` moves only when something was pending.
pub async fn clear_email_change(db: &Db, id: i64, now: NaiveDateTime) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(
        "UPDATE users SET pending_email = NULL, email_change_code = NULL, \
           email_change_code_expires_at = NULL, updated_at = $2 \
         WHERE id = $1 AND (pending_email IS NOT NULL OR email_change_code IS NOT NULL OR email_change_code_expires_at IS NOT NULL)",
    )
    .bind(id)
    .bind(now)
    .execute(db.pool())
    .await?;
    Ok(())
}

pub async fn email_taken(db: &Db, email: &str) -> DbResult<bool> {
    Ok(
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM users WHERE email = $1)")
            .bind(email)
            .fetch_one(db.pool())
            .await?,
    )
}

/// `$user->update(['avatar_path' => ...])`.
pub async fn set_avatar(db: &Db, id: i64, path: Option<&str>, now: NaiveDateTime) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(
        "UPDATE users SET avatar_path = $2, avatar_storage_type = $3, avatar_s3_storage_id = NULL, updated_at = $4 \
         WHERE id = $1 AND (avatar_path IS DISTINCT FROM $2 OR avatar_storage_type IS DISTINCT FROM $3 OR avatar_s3_storage_id IS NOT NULL)",
    )
    .bind(id)
    .bind(path)
    .bind(path.map(|_| "local"))
    .bind(now)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// Avatar columns of a user who shares a team with the viewer (`ProfileAvatarController`).
pub async fn avatar_of_teammate(
    db: &Db,
    viewer: i64,
    user_id: i64,
) -> DbResult<Option<(Option<String>, Option<String>)>> {
    Ok(sqlx::query_as(
        "SELECT u.avatar_path, u.avatar_storage_type FROM users u WHERE u.id = $2 AND EXISTS ( \
           SELECT 1 FROM team_user a JOIN team_user b ON a.team_id = b.team_id \
           WHERE a.user_id = u.id AND b.user_id = $1)",
    )
    .bind(viewer)
    .bind(user_id)
    .fetch_optional(db.pool())
    .await?)
}

/// `avatar_path` and `avatar_storage_type`.
pub async fn avatar_of(db: &Db, id: i64) -> DbResult<Option<(Option<String>, Option<String>)>> {
    Ok(
        sqlx::query_as("SELECT avatar_path, avatar_storage_type FROM users WHERE id = $1")
            .bind(id)
            .fetch_optional(db.pool())
            .await?,
    )
}

/// `EnableTwoFactorAuthentication`: values are encrypted by the caller.
pub async fn set_two_factor(
    db: &Db,
    id: i64,
    secret: &str,
    recovery_codes: &str,
    now: NaiveDateTime,
) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(
        "UPDATE users SET two_factor_secret = $2, two_factor_recovery_codes = $3, updated_at = $4 WHERE id = $1",
    )
    .bind(id)
    .bind(secret)
    .bind(recovery_codes)
    .bind(now)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// `ConfirmTwoFactorAuthentication`.
pub async fn confirm_two_factor(db: &Db, id: i64, now: NaiveDateTime) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query("UPDATE users SET two_factor_confirmed_at = $2, updated_at = $2 WHERE id = $1")
        .bind(id)
        .bind(now)
        .execute(db.pool())
        .await?;
    Ok(())
}

/// `DisableTwoFactorAuthentication`.
pub async fn disable_two_factor(db: &Db, id: i64, now: NaiveDateTime) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(
        "UPDATE users SET two_factor_secret = NULL, two_factor_recovery_codes = NULL, \
           two_factor_confirmed_at = NULL, updated_at = $2 \
         WHERE id = $1 AND (two_factor_secret IS NOT NULL OR two_factor_recovery_codes IS NOT NULL OR two_factor_confirmed_at IS NOT NULL)",
    )
    .bind(id)
    .bind(now)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// `GenerateNewRecoveryCodes`.
pub async fn set_recovery_codes(db: &Db, id: i64, codes: &str, now: NaiveDateTime) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query("UPDATE users SET two_factor_recovery_codes = $2, updated_at = $3 WHERE id = $1")
        .bind(id)
        .bind(codes)
        .bind(now)
        .execute(db.pool())
        .await?;
    Ok(())
}

/// The latest `oauth_identities.provider` of the user.
pub async fn sso_provider(db: &Db, id: i64) -> DbResult<Option<String>> {
    Ok(sqlx::query_scalar(
        "SELECT provider FROM oauth_identities WHERE user_id = $1 ORDER BY id DESC LIMIT 1",
    )
    .bind(id)
    .fetch_optional(db.pool())
    .await?)
}

/// One row of `passkeys`, the `credential` JSON as stored.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct PasskeyRow {
    pub id: i64,
    pub name: String,
    pub credential_id: String,
    pub aaguid: Option<String>,
    pub last_used_at: Option<NaiveDateTime>,
    pub created_at: Option<NaiveDateTime>,
}

/// `Auth::user()->passkeys()->latest()->get()`.
pub async fn passkeys(db: &Db, user_id: i64) -> DbResult<Vec<PasskeyRow>> {
    Ok(sqlx::query_as(
        "SELECT id, name, credential_id, credential->>'aaguid' AS aaguid, last_used_at, created_at \
         FROM passkeys WHERE user_id = $1 ORDER BY created_at DESC",
    )
    .bind(user_id)
    .fetch_all(db.pool())
    .await?)
}

pub async fn passkey_exists(db: &Db, credential_id: &str) -> DbResult<bool> {
    Ok(
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM passkeys WHERE credential_id = $1)")
            .bind(credential_id)
            .fetch_one(db.pool())
            .await?,
    )
}

/// `$user->passkeys()->create([...])`. `credential` is the JSON text upstream stores.
pub async fn create_passkey(
    db: &Db,
    user_id: i64,
    name: &str,
    credential_id: &str,
    credential: &str,
    now: NaiveDateTime,
) -> DbResult<i64> {
    db.ensure_writable()?;
    Ok(sqlx::query_scalar(
        "INSERT INTO passkeys (user_id, name, credential_id, credential, created_at, updated_at) \
         VALUES ($1, $2, $3, $4::json, $5, $5) RETURNING id",
    )
    .bind(user_id)
    .bind(name)
    .bind(credential_id)
    .bind(credential)
    .bind(now)
    .fetch_one(db.pool())
    .await?)
}

pub async fn rename_passkey(
    db: &Db,
    user_id: i64,
    id: i64,
    name: &str,
    now: NaiveDateTime,
) -> DbResult<bool> {
    db.ensure_writable()?;
    let exists: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM passkeys WHERE id = $1 AND user_id = $2)")
            .bind(id)
            .bind(user_id)
            .fetch_one(db.pool())
            .await?;
    if exists {
        sqlx::query(
            "UPDATE passkeys SET name = $3, updated_at = $4 WHERE id = $1 AND user_id = $2 AND name IS DISTINCT FROM $3",
        )
        .bind(id)
        .bind(user_id)
        .bind(name)
        .bind(now)
        .execute(db.pool())
        .await?;
    }
    Ok(exists)
}

pub async fn passkey_owned(db: &Db, user_id: i64, id: i64) -> DbResult<bool> {
    Ok(
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM passkeys WHERE id = $1 AND user_id = $2)")
            .bind(id)
            .bind(user_id)
            .fetch_one(db.pool())
            .await?,
    )
}

pub async fn delete_passkey(db: &Db, user_id: i64, id: i64) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query("DELETE FROM passkeys WHERE id = $1 AND user_id = $2")
        .bind(id)
        .bind(user_id)
        .execute(db.pool())
        .await?;
    Ok(())
}
