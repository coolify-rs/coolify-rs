//! Users and memberships for browser sessions.

use chrono::NaiveDateTime;
use coolify_domain::{Role, TeamId};

use crate::{Db, DbResult};

/// Login-relevant user fields. Never log `password` or `two_factor_secret`.
#[derive(sqlx::FromRow)]
pub struct LoginUser {
    pub id: i64,
    pub name: String,
    pub email: String,
    pub password: Option<String>,
    pub two_factor_secret: Option<String>,
    pub two_factor_confirmed_at: Option<NaiveDateTime>,
    pub current_team_id: Option<i64>,
    pub force_password_reset: bool,
}

impl std::fmt::Debug for LoginUser {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoginUser")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl LoginUser {
    /// Fortify treats a user as 2FA-enabled once the secret is confirmed.
    pub fn has_two_factor(&self) -> bool {
        self.two_factor_secret.is_some() && self.two_factor_confirmed_at.is_some()
    }
}

const COLUMNS: &str = "id, name, email, password, two_factor_secret, two_factor_confirmed_at, current_team_id, force_password_reset";

/// Upstream lowercases emails on write (`User` mutator) and on login.
pub async fn find_by_email(db: &Db, email: &str) -> DbResult<Option<LoginUser>> {
    Ok(
        sqlx::query_as(&format!("SELECT {COLUMNS} FROM users WHERE email = $1"))
            .bind(email.trim().to_lowercase())
            .fetch_optional(db.pool())
            .await?,
    )
}

pub async fn get(db: &Db, id: i64) -> DbResult<Option<LoginUser>> {
    Ok(
        sqlx::query_as(&format!("SELECT {COLUMNS} FROM users WHERE id = $1"))
            .bind(id)
            .fetch_optional(db.pool())
            .await?,
    )
}

#[derive(Debug, Clone)]
pub struct Membership {
    pub team_id: TeamId,
    pub team_name: String,
    pub role: Role,
    /// `teams.is_two_factor_required` (absent before upstream 2026_10_25 → false).
    pub two_factor_required: bool,
}

pub async fn memberships(db: &Db, user_id: i64) -> DbResult<Vec<Membership>> {
    let has_flag = db.schema().has_column("teams", "is_two_factor_required");
    let flag = if has_flag {
        "t.is_two_factor_required"
    } else {
        "false"
    };
    let rows: Vec<(i64, String, String, bool)> = sqlx::query_as(&format!(
        "SELECT t.id, t.name, tu.role, {flag} FROM team_user tu JOIN teams t ON t.id = tu.team_id \
         WHERE tu.user_id = $1 ORDER BY tu.id"
    ))
    .bind(user_id)
    .fetch_all(db.pool())
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|(team_id, team_name, role, two_factor_required)| {
            Some(Membership {
                team_id,
                team_name,
                role: Role::parse(&role)?,
                two_factor_required,
            })
        })
        .collect())
}

/// Persists the selected team like upstream (`users.current_team_id`).
pub async fn set_current_team(db: &Db, user_id: i64, team_id: TeamId) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query("UPDATE users SET current_team_id = $2 WHERE id = $1")
        .bind(user_id)
        .bind(team_id)
        .execute(db.pool())
        .await?;
    Ok(())
}

/// `instance_settings.disable_two_step_confirmation`.
pub async fn two_step_confirmation_disabled(db: &Db) -> DbResult<bool> {
    Ok(sqlx::query_scalar(
        "SELECT COALESCE((SELECT disable_two_step_confirmation FROM instance_settings WHERE id = 0), false)",
    )
    .fetch_one(db.pool())
    .await?)
}

/// `User::hasSsoIdentity()`.
pub async fn has_sso_identity(db: &Db, user_id: i64) -> DbResult<bool> {
    Ok(
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM oauth_identities WHERE user_id = $1)")
            .bind(user_id)
            .fetch_one(db.pool())
            .await?,
    )
}
