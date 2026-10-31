//! The `instance_settings` row (id 0) as a record, and `oauth_settings`.

use chrono::NaiveDateTime;

use crate::records::{Record, parse_row};
use crate::servers::{ColumnValue, bind_all, set_clause};
use crate::{Db, DbError, DbResult};

/// `encrypted` casts of `InstanceSettings` that hold email configuration.
pub const SECRET_COLUMNS: &[&str] = &[
    "smtp_from_address",
    "smtp_from_name",
    "smtp_recipients",
    "smtp_host",
    "smtp_username",
    "smtp_password",
    "resend_api_key",
];

fn decode(text: &str) -> DbResult<Record> {
    parse_row(text).map_err(|e| DbError::Schema(format!("decoding settings: {e}")))
}

/// `InstanceSettings::get()`.
pub async fn row(db: &Db) -> DbResult<Record> {
    let t: Option<String> =
        sqlx::query_scalar("SELECT row_to_json(s)::text FROM instance_settings s WHERE id = 0")
            .fetch_optional(db.pool())
            .await?;
    match t {
        Some(t) => decode(&t),
        None => Ok(Record::new()),
    }
}

/// `$settings->save()` with the given attributes.
pub async fn update(
    db: &Db,
    values: &[(&'static str, ColumnValue)],
    now: NaiveDateTime,
) -> DbResult<()> {
    if values.is_empty() {
        return Ok(());
    }
    db.ensure_writable()?;
    let sql = format!(
        "UPDATE instance_settings SET {}, updated_at = $1 WHERE id = 0",
        set_clause(values, 2)
    );
    bind_all(sqlx::query(&sql).bind(now), values)
        .execute(db.pool())
        .await?;
    Ok(())
}

/// `User::isInstanceAdmin`: admin or owner of the root team.
pub async fn is_instance_admin(db: &Db, user_id: i64) -> DbResult<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM team_user WHERE team_id = 0 AND user_id = $1 AND role IN ('admin','owner'))",
    )
    .bind(user_id)
    .fetch_one(db.pool())
    .await?)
}

/// `OauthSetting::all()`.
pub async fn oauth_rows(db: &Db) -> DbResult<Vec<Record>> {
    let rows: Vec<String> =
        sqlx::query_scalar("SELECT row_to_json(o)::text FROM oauth_settings o ORDER BY id")
            .fetch_all(db.pool())
            .await?;
    rows.iter().map(|t| decode(t)).collect()
}
