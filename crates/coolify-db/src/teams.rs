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
