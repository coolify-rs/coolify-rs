//! The pinned SSH host key of a server: the `ssh_host_key*` and `is_ssh_host_key_check_*`
//! columns of `server_settings` (upstream v4.7.5, migrations
//! `add_ssh_host_key_to_server_settings_table` and
//! `add_ssh_host_key_check_enforced_to_server_settings_table`).

use chrono::NaiveDateTime;

use crate::{Db, DbResult};

/// The host key columns as they are saved now (`loadSavedHostKeySettings()`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostKeySettings {
    /// `server_settings.id`.
    pub settings_id: i64,
    /// Pinned host key(s), one "type base64" line per key.
    pub ssh_host_key: Option<String>,
    /// `tofu`, `upgrade`, `transfer` or `manual`.
    pub ssh_host_key_source: Option<String>,
    pub ssh_host_key_trusted_at: Option<NaiveDateTime>,
    /// The host key that the server sent when it did not match the pinned key.
    pub ssh_host_key_pending: Option<String>,
    pub is_ssh_host_key_check_disabled: bool,
    pub is_ssh_host_key_check_enforced: bool,
    /// `server_settings.is_usable`: a server that was never usable is new.
    pub is_usable: bool,
}

type Columns = (
    i64,
    Option<String>,
    Option<String>,
    Option<NaiveDateTime>,
    Option<String>,
    Option<bool>,
    Option<bool>,
    Option<bool>,
);

/// `ServerSetting::query()->whereKey(...)->first($columns)` for the settings of a server.
pub async fn load(db: &Db, server_id: i64) -> DbResult<Option<HostKeySettings>> {
    let row: Option<Columns> = sqlx::query_as(
        "SELECT id::bigint, ssh_host_key, ssh_host_key_source::text, ssh_host_key_trusted_at, \
         ssh_host_key_pending, is_ssh_host_key_check_disabled, is_ssh_host_key_check_enforced, \
         is_usable FROM server_settings WHERE server_id = $1 LIMIT 1",
    )
    .bind(server_id)
    .fetch_optional(db.pool())
    .await?;
    Ok(row.map(
        |(settings_id, key, source, trusted_at, pending, disabled, enforced, usable)| {
            HostKeySettings {
                settings_id,
                ssh_host_key: key,
                ssh_host_key_source: source,
                ssh_host_key_trusted_at: trusted_at,
                ssh_host_key_pending: pending,
                is_ssh_host_key_check_disabled: disabled.unwrap_or(false),
                is_ssh_host_key_check_enforced: enforced.unwrap_or(false),
                is_usable: usable.unwrap_or(false),
            }
        },
    ))
}

/// `ServerSetting::query()->whereKey($id)->whereNull('ssh_host_key')->update($pin)` of
/// `learnHostKey()`: writes only the pin. Returns false when another process pinned a key first.
pub async fn pin_if_unset(
    db: &Db,
    settings_id: i64,
    key: &str,
    source: &str,
    now: NaiveDateTime,
) -> DbResult<bool> {
    db.ensure_writable()?;
    let done = sqlx::query(
        "UPDATE server_settings SET ssh_host_key = $2, ssh_host_key_source = $3, \
         ssh_host_key_trusted_at = $4, ssh_host_key_pending = NULL, updated_at = $4 \
         WHERE id = $1 AND ssh_host_key IS NULL",
    )
    .bind(settings_id)
    .bind(key)
    .bind(source)
    .bind(now)
    .execute(db.pool())
    .await?;
    Ok(done.rows_affected() > 0)
}

/// `forgetSshHostKey()`: a query update of the four key columns, so that no model event runs.
pub async fn forget(db: &Db, settings_id: i64, now: NaiveDateTime) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(
        "UPDATE server_settings SET ssh_host_key = NULL, ssh_host_key_source = NULL, \
         ssh_host_key_trusted_at = NULL, ssh_host_key_pending = NULL, updated_at = $2 \
         WHERE id = $1",
    )
    .bind(settings_id)
    .bind(now)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// `$settings->forceFill(['ssh_host_key' => $key, 'ssh_host_key_source' => $key ? 'manual' : null,
/// 'ssh_host_key_trusted_at' => $key ? now() : null, 'ssh_host_key_pending' => null])->save()`:
/// a key that an admin gave (`create_server` and `reset_host_key` of the API), or none, so that
/// the next connection pins the key the server sends.
pub async fn set_manual(
    db: &Db,
    settings_id: i64,
    key: Option<&str>,
    now: NaiveDateTime,
) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(
        "UPDATE server_settings SET ssh_host_key = $2, \
         ssh_host_key_source = CASE WHEN $2::text IS NULL THEN NULL ELSE 'manual' END, \
         ssh_host_key_trusted_at = CASE WHEN $2::text IS NULL THEN NULL ELSE $3 END, \
         ssh_host_key_pending = NULL, updated_at = $3 WHERE id = $1",
    )
    .bind(settings_id)
    .bind(key)
    .bind(now)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// `$settings->forceFill(['ssh_host_key_pending' => $newKey])->save()`.
pub async fn set_pending(
    db: &Db,
    settings_id: i64,
    pending: &str,
    now: NaiveDateTime,
) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(
        "UPDATE server_settings SET ssh_host_key_pending = $2, updated_at = $3 \
         WHERE id = $1 AND ssh_host_key_pending IS DISTINCT FROM $2",
    )
    .bind(settings_id)
    .bind(pending)
    .bind(now)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// `HostKey::trustPendingKey()`: the pending key becomes the pinned key. `expected_pending` is
/// the key the page showed, so a key that changed later is not trusted. Returns false when the
/// pending key is another one now.
pub async fn trust_pending(
    db: &Db,
    settings_id: i64,
    expected_pending: &str,
    now: NaiveDateTime,
) -> DbResult<bool> {
    db.ensure_writable()?;
    let done = sqlx::query(
        "UPDATE server_settings SET ssh_host_key = ssh_host_key_pending, \
         ssh_host_key_source = 'manual', ssh_host_key_trusted_at = $3, \
         ssh_host_key_pending = NULL, updated_at = $3 \
         WHERE id = $1 AND ssh_host_key_pending = $2",
    )
    .bind(settings_id)
    .bind(expected_pending)
    .bind(now)
    .execute(db.pool())
    .await?;
    Ok(done.rows_affected() > 0)
}

/// `HostKey::setHostKeyCheckDisabled()` and `setHostKeyCheckMode()`: a model save, so
/// `updated_at` changes only when a value changes.
pub async fn set_check(
    db: &Db,
    settings_id: i64,
    disabled: bool,
    enforced: bool,
    now: NaiveDateTime,
) -> DbResult<bool> {
    db.ensure_writable()?;
    let done = sqlx::query(
        "UPDATE server_settings SET is_ssh_host_key_check_disabled = $2, \
         is_ssh_host_key_check_enforced = $3, updated_at = $4 WHERE id = $1 AND \
         (is_ssh_host_key_check_disabled IS DISTINCT FROM $2 \
          OR is_ssh_host_key_check_enforced IS DISTINCT FROM $3)",
    )
    .bind(settings_id)
    .bind(disabled)
    .bind(enforced)
    .bind(now)
    .execute(db.pool())
    .await?;
    Ok(done.rows_affected() > 0)
}
