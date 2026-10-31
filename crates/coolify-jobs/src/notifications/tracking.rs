//! `App\Traits\TracksDeliveryFailures`.

use coolify_compat::Encrypter;
use coolify_compat::redact::redact_text;
use coolify_db::audit::{self, Actor};
use coolify_db::notification_settings::{self as ns, Channel};
use coolify_db::{Db, DbResult};
use serde_json::json;

use super::messages::{Mail, general};
use super::{Ctx, channels};

/// `DELIVERY_FAILURE_THRESHOLD`.
const THRESHOLD: i32 = 3;

/// `notificationChannelName()`.
pub fn channel_name(ch: &Channel) -> &'static str {
    match ch.name {
        "email" => "Email",
        "discord" => "Discord",
        "slack" => "Slack",
        "telegram" => "Telegram",
        "pushover" => "Pushover",
        _ => "Webhook",
    }
}

/// `recordDeliverySuccess()`.
pub async fn record_success(db: &Db, ch: &Channel, team_id: i64) {
    if let Err(e) = ns::reset_delivery_failures(db, ch, team_id).await {
        tracing::warn!(error = %e, "could not reset the delivery failure count");
    }
}

/// `recordDeliveryFailure($reason)`.
pub async fn record_failure(
    db: &Db,
    encrypter: Option<&Encrypter>,
    ch: &'static Channel,
    team_id: i64,
    reason: &str,
) {
    if let Err(e) = record_failure_inner(db, encrypter, ch, team_id, reason).await {
        tracing::warn!(error = %e, "could not record a delivery failure");
    }
}

async fn record_failure_inner(
    db: &Db,
    encrypter: Option<&Encrypter>,
    ch: &'static Channel,
    team_id: i64,
    reason: &str,
) -> DbResult<()> {
    let reason = redact_text(reason, &[], &[]);
    let outcome = ns::record_delivery_failure(db, ch, team_id, THRESHOLD).await?;
    let channel = channel_name(ch);
    tracing::warn!(
        channel,
        team_id,
        failure_count = outcome.failure_count,
        reason = %reason,
        "Notification delivery failed"
    );
    if !outcome.disabled {
        return Ok(());
    }
    let lower = channel.to_lowercase();
    audit::record(
        db,
        Some(team_id),
        &format!("notifications.{lower}.auto_disabled"),
        &json!({"team_id": team_id, "reason": reason}),
        "warning",
        &Actor::default(),
        None,
        crate::Jobs::now(),
    )
    .await?;

    // `send_internal_notification(...)`: the root team, `general` event.
    let internal = format!(
        "{channel} notifications disabled for team {team_id} after {THRESHOLD} failed deliveries.\nReason: {reason}"
    );
    channels::spawn_general(db, encrypter, 0, general(internal, true));

    let ctx = Ctx::load(db).await;
    let url = format!("{}/notifications/{lower}", ctx.base_url);

    // `sendChannelDisabledEmail($reason)`.
    if let Err(e) = send_disabled_email(db, encrypter, team_id, channel, &reason, &url).await {
        tracing::warn!("Failed to send notification channel disabled email: {e}");
    }

    // `notifyOtherChannels($reason)`.
    if team_id != 0 {
        let message = format!(
            "{channel} notifications were disabled after {THRESHOLD} failed deliveries in a row.\nReason: {reason}\nPlease fix the settings and enable {channel} notifications again: {url}"
        );
        channels::spawn_general(db, encrypter, team_id, general(message, false));
    }
    Ok(())
}

async fn send_disabled_email(
    db: &Db,
    encrypter: Option<&Encrypter>,
    team_id: i64,
    channel: &str,
    reason: &str,
    url: &str,
) -> Result<(), String> {
    let settings = coolify_db::instance_settings::row(db)
        .await
        .map_err(|e| e.to_string())?;
    let enabled = |k: &str| settings.get(k).and_then(serde_json::Value::as_bool) == Some(true);
    if !enabled("smtp_enabled") && !enabled("resend_enabled") {
        return Ok(());
    }
    let admins: Vec<String> = sqlx::query_scalar(
        "SELECT u.email FROM users u JOIN team_user t ON t.user_id = u.id \
         WHERE t.team_id = $1 AND t.role IN ('admin', 'owner') ORDER BY u.id",
    )
    .bind(team_id)
    .fetch_all(db.pool())
    .await
    .map_err(|e| e.to_string())?;
    let mail = Mail::ChannelDisabled {
        channel: channel.to_owned(),
        failures: i64::from(THRESHOLD),
        reason: reason.to_owned(),
        url: url.to_owned(),
    };
    for admin in admins {
        channels::send_transactional(db, encrypter, &admin, &mail).await?;
    }
    Ok(())
}
