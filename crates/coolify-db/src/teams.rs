//! Teams and members.

use chrono::NaiveDateTime;
use coolify_domain::TeamId;

use crate::records::{Record, TEAM, USER, parse_row};
use crate::{Db, DbError, DbResult};

/// The team as `Team::toArray()` would render it (before controller tweaks).
pub async fn get(db: &Db, team_id: TeamId) -> DbResult<Option<Record>> {
    let row: Option<String> =
        sqlx::query_scalar("SELECT row_to_json(t)::text FROM teams t WHERE t.id = $1")
            .bind(team_id)
            .fetch_optional(db.pool())
            .await?;
    row.map(|r| parse_row(&r).map(|raw| TEAM.present(raw)))
        .transpose()
        .map_err(|e| DbError::Schema(format!("decoding team row: {e}")))
}

/// Members in pivot insertion order, rendered like `User::toArray()`.
/// The team's `name`.
pub async fn find_name(db: &Db, team_id: TeamId) -> DbResult<Option<String>> {
    Ok(sqlx::query_scalar("SELECT name FROM teams WHERE id = $1")
        .bind(team_id)
        .fetch_optional(db.pool())
        .await?)
}

pub async fn members(db: &Db, team_id: TeamId) -> DbResult<Vec<Record>> {
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT row_to_json(u)::text FROM users u JOIN team_user tu ON tu.user_id = u.id \
         WHERE tu.team_id = $1",
    )
    .bind(team_id)
    .fetch_all(db.pool())
    .await?;
    rows.iter()
        .map(|r| parse_row(r).map(|raw| USER.present(raw)))
        .collect::<Result<_, _>>()
        .map_err(|e| DbError::Schema(format!("decoding user row: {e}")))
}

/// `PATCH /team`: only touches `updated_at` when the value changes (Eloquent dirty check).
pub async fn set_build_server_fallback(
    db: &Db,
    team_id: TeamId,
    enabled: bool,
    now: NaiveDateTime,
) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(
        "UPDATE teams SET is_build_server_fallback_enabled = $2, updated_at = $3 \
         WHERE id = $1 AND is_build_server_fallback_enabled IS DISTINCT FROM $2",
    )
    .bind(team_id)
    .bind(enabled)
    .bind(now)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// Fields of the web UI team settings form (`Livewire\Team\Index::submit`).
#[derive(Debug, Clone)]
pub struct TeamSettings {
    pub name: String,
    pub description: Option<String>,
    pub is_mcp_server_enabled: bool,
    pub is_build_server_fallback_enabled: bool,
    pub is_two_factor_required: bool,
}

/// `$team->save()` after `syncData(true)`: `HasSafeStringAttribute` strips
/// tags from name/description, and `updated_at` moves only when a column
/// changes (Eloquent dirty check).
pub async fn update_settings(
    db: &Db,
    team_id: TeamId,
    s: &TeamSettings,
    now: NaiveDateTime,
) -> DbResult<()> {
    db.ensure_writable()?;
    let name = coolify_domain::naming::strip_tags(Some(&s.name));
    // An assigned null becomes "" (strip_tags on null), as for projects.
    let description = coolify_domain::naming::strip_tags(s.description.as_deref());
    sqlx::query(
        "UPDATE teams SET name = $2, description = $3, is_mcp_server_enabled = $4, \
         is_build_server_fallback_enabled = $5, is_two_factor_required = $6, updated_at = $7 \
         WHERE id = $1 AND (name IS DISTINCT FROM $2 OR description IS DISTINCT FROM $3 \
           OR is_mcp_server_enabled IS DISTINCT FROM $4 \
           OR is_build_server_fallback_enabled IS DISTINCT FROM $5 \
           OR is_two_factor_required IS DISTINCT FROM $6)",
    )
    .bind(team_id)
    .bind(name)
    .bind(description)
    .bind(s.is_mcp_server_enabled)
    .bind(s.is_build_server_fallback_enabled)
    .bind(s.is_two_factor_required)
    .bind(now)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// A team member as the members page lists it.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct MemberRow {
    pub id: i64,
    pub name: String,
    pub email: String,
    pub role: String,
    pub two_factor: bool,
    pub avatar_path: Option<String>,
    pub updated_at: Option<NaiveDateTime>,
}

/// `currentTeam()->members` with the pivot role (no ORDER BY, like Eloquent).
pub async fn member_rows(db: &Db, team_id: TeamId) -> DbResult<Vec<MemberRow>> {
    Ok(sqlx::query_as(
        "SELECT u.id, u.name, u.email, COALESCE(tu.role, 'member') AS role, \
                (u.two_factor_confirmed_at IS NOT NULL) AS two_factor, u.avatar_path, u.updated_at \
         FROM users u JOIN team_user tu ON tu.user_id = u.id WHERE tu.team_id = $1",
    )
    .bind(team_id)
    .fetch_all(db.pool())
    .await?)
}

const USER_TOKENABLE: &str = "App\\Models\\User";

/// `updateExistingPivot(role)` + `RevokeUserTeamTokens::forUserTeam`, in one transaction.
pub async fn set_member_role(db: &Db, team_id: TeamId, user_id: i64, role: &str) -> DbResult<()> {
    db.ensure_writable()?;
    let mut tx = db.pool().begin().await?;
    sqlx::query("UPDATE team_user SET role = $3 WHERE team_id = $1 AND user_id = $2")
        .bind(team_id)
        .bind(user_id)
        .bind(role)
        .execute(&mut *tx)
        .await?;
    revoke_tokens(&mut tx, team_id, user_id).await?;
    tx.commit().await?;
    Ok(())
}

/// `teams()->detach`, token revocation and `clearStoredTeamIfMatches`, in one transaction.
pub async fn remove_member(db: &Db, team_id: TeamId, user_id: i64) -> DbResult<()> {
    db.ensure_writable()?;
    let mut tx = db.pool().begin().await?;
    sqlx::query("DELETE FROM team_user WHERE team_id = $1 AND user_id = $2")
        .bind(team_id)
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
    revoke_tokens(&mut tx, team_id, user_id).await?;
    sqlx::query("UPDATE users SET current_team_id = NULL WHERE id = $1 AND current_team_id = $2")
        .bind(user_id)
        .bind(team_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

async fn revoke_tokens(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    team_id: TeamId,
    user_id: i64,
) -> DbResult<()> {
    sqlx::query(
        "DELETE FROM personal_access_tokens WHERE tokenable_type = $1 AND tokenable_id = $2 AND team_id = $3",
    )
    .bind(USER_TOKENABLE)
    .bind(user_id)
    .bind(team_id.to_string())
    .execute(&mut **tx)
    .await?;
    Ok(())
}
