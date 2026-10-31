//! The six `*_notification_settings` tables (`TracksDeliveryFailures` included).

use crate::records::{Record, parse_row};
use crate::servers::{ColumnValue, bind_all, set_clause};
use crate::{Db, DbError, DbResult};

/// One notification channel: its table and the attribute lists of its model.
#[derive(Debug, Clone, Copy)]
pub struct Channel {
    pub name: &'static str,
    pub table: &'static str,
    /// `$fillable` without `team_id`.
    pub fillable: &'static [&'static str],
    /// `$hidden`.
    pub hidden: &'static [&'static str],
    /// `encrypted` casts.
    pub encrypted: &'static [&'static str],
}

macro_rules! events {
    ($ch:literal) => {
        [
            concat!("deployment_success_", $ch, "_notifications"),
            concat!("deployment_failure_", $ch, "_notifications"),
            concat!("status_change_", $ch, "_notifications"),
            concat!("restart_limit_reached_", $ch, "_notifications"),
            concat!("backup_success_", $ch, "_notifications"),
            concat!("backup_failure_", $ch, "_notifications"),
            concat!("scheduled_task_success_", $ch, "_notifications"),
            concat!("scheduled_task_failure_", $ch, "_notifications"),
            concat!("docker_cleanup_success_", $ch, "_notifications"),
            concat!("docker_cleanup_failure_", $ch, "_notifications"),
            concat!("server_disk_usage_", $ch, "_notifications"),
            concat!("server_reachable_", $ch, "_notifications"),
            concat!("server_unreachable_", $ch, "_notifications"),
            concat!("server_patch_", $ch, "_notifications"),
            concat!("traefik_outdated_", $ch, "_notifications"),
        ]
    };
}

const fn cat<const A: usize, const B: usize, const N: usize>(
    a: [&'static str; A],
    b: [&'static str; B],
) -> [&'static str; N] {
    let mut out = [""; N];
    let mut i = 0;
    while i < A {
        out[i] = a[i];
        i += 1;
    }
    let mut j = 0;
    while j < B {
        out[A + j] = b[j];
        j += 1;
    }
    out
}

const TELEGRAM_THREADS: [&str; 14] = [
    "telegram_notifications_deployment_success_thread_id",
    "telegram_notifications_deployment_failure_thread_id",
    "telegram_notifications_status_change_thread_id",
    "telegram_notifications_restart_limit_reached_thread_id",
    "telegram_notifications_backup_success_thread_id",
    "telegram_notifications_backup_failure_thread_id",
    "telegram_notifications_scheduled_task_success_thread_id",
    "telegram_notifications_scheduled_task_failure_thread_id",
    "telegram_notifications_docker_cleanup_success_thread_id",
    "telegram_notifications_docker_cleanup_failure_thread_id",
    "telegram_notifications_server_disk_usage_thread_id",
    "telegram_notifications_server_reachable_thread_id",
    "telegram_notifications_server_unreachable_thread_id",
    "telegram_notifications_server_patch_thread_id",
];

static EMAIL_FILLABLE: [&str; 30] = cat::<14, 16, 30>(
    [
        "smtp_enabled",
        "smtp_from_address",
        "smtp_from_name",
        "smtp_recipients",
        "smtp_host",
        "smtp_port",
        "smtp_encryption",
        "smtp_username",
        "smtp_password",
        "smtp_timeout",
        "smtp_ehlo_domain",
        "resend_enabled",
        "resend_api_key",
        "use_instance_email_settings",
    ],
    [
        "deployment_success_email_notifications",
        "deployment_failure_email_notifications",
        "deployment_commit_details_email_notifications",
        "status_change_email_notifications",
        "restart_limit_reached_email_notifications",
        "backup_success_email_notifications",
        "backup_failure_email_notifications",
        "scheduled_task_success_email_notifications",
        "scheduled_task_failure_email_notifications",
        "docker_cleanup_success_email_notifications",
        "docker_cleanup_failure_email_notifications",
        "server_disk_usage_email_notifications",
        "server_reachable_email_notifications",
        "server_unreachable_email_notifications",
        "server_patch_email_notifications",
        "traefik_outdated_email_notifications",
    ],
);
static DISCORD_FILLABLE: [&str; 18] = cat::<17, 1, 18>(
    cat::<2, 15, 17>(
        ["discord_enabled", "discord_webhook_url"],
        events!("discord"),
    ),
    ["discord_ping_enabled"],
);
static SLACK_FILLABLE: [&str; 17] =
    cat::<2, 15, 17>(["slack_enabled", "slack_webhook_url"], events!("slack"));
static WEBHOOK_FILLABLE: [&str; 17] =
    cat::<2, 15, 17>(["webhook_enabled", "webhook_url"], events!("webhook"));
static PUSHOVER_FILLABLE: [&str; 18] = cat::<3, 15, 18>(
    [
        "pushover_enabled",
        "pushover_user_key",
        "pushover_api_token",
    ],
    events!("pushover"),
);
static TELEGRAM_BASE: [&str; 18] = cat::<3, 15, 18>(
    ["telegram_enabled", "telegram_token", "telegram_chat_id"],
    events!("telegram"),
);
static TELEGRAM_FILLABLE: [&str; 33] = cat::<18, 15, 33>(
    cat::<3, 15, 18>(
        ["telegram_enabled", "telegram_token", "telegram_chat_id"],
        events!("telegram"),
    ),
    cat::<14, 1, 15>(
        TELEGRAM_THREADS,
        ["telegram_notifications_traefik_outdated_thread_id"],
    ),
);
static TELEGRAM_SECRET: [&str; 17] = cat::<2, 15, 17>(
    ["telegram_token", "telegram_chat_id"],
    cat::<14, 1, 15>(
        TELEGRAM_THREADS,
        ["telegram_notifications_traefik_outdated_thread_id"],
    ),
);

pub static EMAIL: Channel = Channel {
    name: "email",
    table: "email_notification_settings",
    fillable: &EMAIL_FILLABLE,
    hidden: &[
        "smtp_from_address",
        "smtp_from_name",
        "smtp_recipients",
        "smtp_host",
        "smtp_username",
        "smtp_password",
        "resend_api_key",
    ],
    encrypted: &[
        "smtp_from_address",
        "smtp_from_name",
        "smtp_recipients",
        "smtp_host",
        "smtp_username",
        "smtp_password",
        "resend_api_key",
    ],
};
pub static DISCORD: Channel = Channel {
    name: "discord",
    table: "discord_notification_settings",
    fillable: &DISCORD_FILLABLE,
    hidden: &["discord_webhook_url"],
    encrypted: &["discord_webhook_url"],
};
pub static SLACK: Channel = Channel {
    name: "slack",
    table: "slack_notification_settings",
    fillable: &SLACK_FILLABLE,
    hidden: &["slack_webhook_url"],
    encrypted: &["slack_webhook_url"],
};
pub static TELEGRAM: Channel = Channel {
    name: "telegram",
    table: "telegram_notification_settings",
    fillable: &TELEGRAM_FILLABLE,
    hidden: &TELEGRAM_SECRET,
    encrypted: &TELEGRAM_SECRET,
};
pub static PUSHOVER: Channel = Channel {
    name: "pushover",
    table: "pushover_notification_settings",
    fillable: &PUSHOVER_FILLABLE,
    hidden: &["pushover_user_key", "pushover_api_token"],
    encrypted: &["pushover_user_key", "pushover_api_token"],
};
pub static WEBHOOK: Channel = Channel {
    name: "webhook",
    table: "webhook_notification_settings",
    fillable: &WEBHOOK_FILLABLE,
    hidden: &["webhook_url"],
    encrypted: &["webhook_url"],
};

/// Every channel, in `Team::getEnabledChannels` order is not implied.
pub static ALL: [&Channel; 6] = [&EMAIL, &DISCORD, &SLACK, &TELEGRAM, &PUSHOVER, &WEBHOOK];

/// Telegram's non-secret columns (events and the switch).
pub fn telegram_base() -> &'static [&'static str] {
    &TELEGRAM_BASE
}

fn decode(text: &str) -> DbResult<Record> {
    parse_row(text).map_err(|e| DbError::Schema(format!("decoding notification settings: {e}")))
}

pub async fn find(db: &Db, ch: &Channel, team_id: i64) -> DbResult<Option<Record>> {
    let sql = format!(
        "SELECT row_to_json(t)::text FROM {} t WHERE team_id = $1 LIMIT 1",
        ch.table
    );
    let row: Option<String> = sqlx::query_scalar(&sql)
        .bind(team_id)
        .fetch_optional(db.pool())
        .await?;
    row.as_deref().map(decode).transpose()
}

/// `firstOrCreate(['team_id' => $teamId])`. The bool says the row was created.
pub async fn first_or_create(db: &Db, ch: &Channel, team_id: i64) -> DbResult<(Record, bool)> {
    if let Some(r) = find(db, ch, team_id).await? {
        return Ok((r, false));
    }
    db.ensure_writable()?;
    let sql = format!(
        "INSERT INTO {} (team_id) VALUES ($1) ON CONFLICT (team_id) DO NOTHING",
        ch.table
    );
    sqlx::query(&sql).bind(team_id).execute(db.pool()).await?;
    let r = find(db, ch, team_id)
        .await?
        .ok_or_else(|| DbError::Schema("notification settings row vanished".into()))?;
    Ok((r, true))
}

/// `Team::createDefaultNotificationSettings`.
pub async fn create_defaults(db: &Db, team_id: i64) -> DbResult<()> {
    for ch in ALL {
        first_or_create(db, ch, team_id).await?;
    }
    Ok(())
}

/// `$settings->fill($values)->save()`. The `saving` hook of `TracksDeliveryFailures` sets
/// `failure_count` back to zero when anything else changes.
pub async fn update(
    db: &Db,
    ch: &Channel,
    team_id: i64,
    values: &[(&'static str, ColumnValue)],
) -> DbResult<()> {
    if values.is_empty() {
        return Ok(());
    }
    db.ensure_writable()?;
    let reset = if values.iter().any(|(k, _)| *k == "failure_count") {
        ""
    } else {
        ", failure_count = 0"
    };
    let sql = format!(
        "UPDATE {} SET {}{reset} WHERE team_id = $1",
        ch.table,
        set_clause(values, 2)
    );
    bind_all(sqlx::query(&sql).bind(team_id), values)
        .execute(db.pool())
        .await?;
    Ok(())
}

/// What `recordDeliveryFailure` did to the row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeliveryFailure {
    /// `failure_count` after the increment.
    pub failure_count: i32,
    /// The channel was enabled and has been turned off.
    pub disabled: bool,
}

/// `TracksDeliveryFailures::recordDeliveryFailure`: counts a failed delivery; disables an
/// enabled channel at the limit. Turning it off is a save, so the `saving` hook sets
/// `failure_count` back to zero.
pub async fn record_delivery_failure(
    db: &Db,
    ch: &Channel,
    team_id: i64,
    limit: i32,
) -> DbResult<DeliveryFailure> {
    db.ensure_writable()?;
    let (enabled, off) = match ch.name {
        "email" => (
            "(smtp_enabled OR resend_enabled OR use_instance_email_settings)",
            "smtp_enabled = false, resend_enabled = false",
        ),
        "discord" => ("discord_enabled", "discord_enabled = false"),
        "slack" => ("slack_enabled", "slack_enabled = false"),
        "telegram" => ("telegram_enabled", "telegram_enabled = false"),
        "pushover" => ("pushover_enabled", "pushover_enabled = false"),
        _ => ("webhook_enabled", "webhook_enabled = false"),
    };
    let sql = format!(
        "UPDATE {} SET failure_count = failure_count + 1 WHERE team_id = $1 RETURNING failure_count, {enabled}",
        ch.table
    );
    let row: Option<(i32, bool)> = sqlx::query_as(&sql)
        .bind(team_id)
        .fetch_optional(db.pool())
        .await?;
    let Some((failure_count, is_enabled)) = row else {
        return Ok(DeliveryFailure {
            failure_count: 0,
            disabled: false,
        });
    };
    if failure_count < limit || !is_enabled {
        return Ok(DeliveryFailure {
            failure_count,
            disabled: false,
        });
    }
    let sql = format!(
        "UPDATE {} SET {off}, failure_count = 0 WHERE team_id = $1",
        ch.table
    );
    sqlx::query(&sql).bind(team_id).execute(db.pool()).await?;
    Ok(DeliveryFailure {
        failure_count,
        disabled: true,
    })
}

/// `TracksDeliveryFailures::resetDeliveryFailures`.
pub async fn reset_delivery_failures(db: &Db, ch: &Channel, team_id: i64) -> DbResult<()> {
    db.ensure_writable()?;
    let sql = format!(
        "UPDATE {} SET failure_count = 0 WHERE team_id = $1 AND failure_count <> 0",
        ch.table
    );
    sqlx::query(&sql).bind(team_id).execute(db.pool()).await?;
    Ok(())
}
