//! Sending upstream's notifications: `app/Notifications`, the channels and the
//! `SendMessageTo*Job` / `SendWebhookJob` jobs. Port of what `$team->notify(new X(...))` does.
//!
//! # Interface
//!
//! ```ignore
//! // `$team->notify(new X(...))`: spawns delivery, never returns an error. Failures are logged
//! // and counted in `failure_count` as upstream does (auto-disable after 3 in a row).
//! notifications::notify(db, encrypter, team_id, Notification::Unreachable { server }).await;
//!
//! // The UI's send-test buttons (`new Test(channel: ...)`). Errors are returned for the page.
//! notifications::notify_test(db, encrypter, team_id, "discord", None).await?;
//! notifications::notify_test(db, encrypter, team_id, "email", Some(address)).await?;
//!
//! // Transactional mail through the instance settings (reset password, invitation, ...).
//! notifications::send_transactional(db, encrypter, "user@x.test", Mail::Test).await?;
//! ```
//!
//! [`Notification`] and [`Mail`] are built from plain data (names, uuids, urls), so callers need
//! no models. Upstream's queue is a tokio task per channel with the same tries and backoff (ADR
//! 0009: lost when the process restarts). [`deliver`] runs a notification to the end on the
//! current task (tests).

mod channels;
pub mod lookup;
mod markdown;
mod messages;
mod php;
mod tracking;
mod views;

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use coolify_compat::Encrypter;
use coolify_db::{Db, instance_settings};
use serde_json::Value;

pub use messages::{
    CommitDetails, DatabaseBackup, Deployment, DiscordMessage, Mail, MailMessage, Notification,
    Preview, PushoverMessage, RestartKind, RestartLimit, ServerRef, SlackMessage, SslResource,
    Task, TaskResource, TelegramMessage, TraefikServer, VolumeBackup, git_commit_link,
};

static VERSION: OnceLock<String> = OnceLock::new();

/// Sets `config('constants.coolify.version')` for the footers. Called by [`crate::Jobs::set_version`].
pub fn set_version(version: &str) {
    let _ = VERSION.set(version.to_owned());
}

/// The configuration the messages read.
#[derive(Debug, Clone, PartialEq)]
pub struct Ctx {
    /// `base_url()`.
    pub base_url: String,
    /// `config('app.url')`.
    pub app_url: String,
    /// `config('app.name')`.
    pub app_name: String,
    /// `config('constants.coolify.version')`.
    pub version: String,
    /// `isCloud()`.
    pub cloud: bool,
    /// `isDev()`.
    pub dev: bool,
}

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name)
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| default.to_owned())
}

impl Ctx {
    /// Reads `instance_settings` and the process environment (as `config/app.php`).
    pub async fn load(db: &Db) -> Self {
        let settings = instance_settings::row(db).await.unwrap_or_default();
        Self::from_settings(&settings)
    }

    fn from_settings(s: &coolify_db::records::Record) -> Self {
        let text = |k: &str| {
            s.get(k)
                .and_then(Value::as_str)
                .filter(|v| !v.is_empty())
                .map(str::to_owned)
        };
        let app_url = env_or("APP_URL", "http://localhost");
        let port = env_or("APP_PORT", "8000");
        let dev = std::env::var("APP_ENV").is_ok_and(|v| v == "local");
        // `base_url()`.
        let base_url = if let Some(fqdn) = text("fqdn") {
            fqdn
        } else if let Some(ip) = text("public_ipv4") {
            if dev {
                format!("http://localhost:{port}")
            } else {
                format!("http://{ip}:{port}")
            }
        } else if let Some(ip) = text("public_ipv6") {
            format!("http://[{ip}]:{port}")
        } else {
            app_url.clone()
        };
        Self {
            base_url,
            app_url,
            app_name: env_or("APP_NAME", "Coolify"),
            version: VERSION
                .get()
                .cloned()
                .or_else(|| std::env::var("COOLIFY_VERSION").ok())
                .unwrap_or_else(|| "4.5.1".to_owned()),
            cloud: std::env::var("SELF_HOSTED").is_ok_and(|v| v == "false" || v == "0"),
            dev,
        }
    }
}

/// `RateLimiter::attempt($key, 0, ..., $decay)`: true when the key is free, and marks it.
fn rate_limiter_attempt(key: &str, decay: Duration) -> bool {
    static SEEN: Mutex<Option<HashMap<String, Instant>>> = Mutex::new(None);
    let mut guard = SEEN.lock().unwrap_or_else(|p| p.into_inner());
    let map = guard.get_or_insert_with(HashMap::new);
    let now = Instant::now();
    map.retain(|_, until| *until > now);
    if map.contains_key(key) {
        return false;
    }
    map.insert(key.to_owned(), now + decay);
    true
}

/// `NotificationThrottle::claim($notifiable, $notification, $sentBefore)`.
async fn claim_throttle(
    db: &Db,
    notifiable_type: &str,
    notifiable_id: i64,
    notification: &str,
    sent_before: Option<DateTime<Utc>>,
) -> Result<bool, sqlx::Error> {
    let pool = db.pool();
    if let Some(before) = sent_before {
        let renewed = sqlx::query(
            "UPDATE notification_throttles SET sent_at = now() WHERE notifiable_type = $1 \
             AND notifiable_id = $2 AND notification = $3 AND sent_at < $4",
        )
        .bind(notifiable_type)
        .bind(notifiable_id)
        .bind(notification)
        .bind(before)
        .execute(pool)
        .await?
        .rows_affected();
        if renewed == 1 {
            return Ok(true);
        }
    }
    let inserted = sqlx::query(
        "INSERT INTO notification_throttles (notifiable_type, notifiable_id, notification, sent_at) \
         VALUES ($1, $2, $3, now()) ON CONFLICT DO NOTHING",
    )
    .bind(notifiable_type)
    .bind(notifiable_id)
    .bind(notification)
    .execute(pool)
    .await?
    .rows_affected();
    Ok(inserted == 1)
}

/// `NotificationThrottle::release`: true when a record was removed.
pub async fn release_throttle(
    db: &Db,
    notifiable_type: &str,
    notifiable_id: i64,
    notification: &str,
) -> Result<bool, sqlx::Error> {
    Ok(sqlx::query(
        "DELETE FROM notification_throttles WHERE notifiable_type = $1 AND notifiable_id = $2 AND notification = $3",
    )
    .bind(notifiable_type)
    .bind(notifiable_id)
    .bind(notification)
    .execute(db.pool())
    .await?
    .rows_affected()
        > 0)
}

/// `Server::sendReachableNotification`.
pub async fn send_reachable_notification(
    db: &Db,
    encrypter: Option<&Encrypter>,
    team_id: i64,
    server: ServerRef,
) {
    let released = release_throttle(db, SERVER_MODEL, server.id, UNREACHABLE_CLASS)
        .await
        .unwrap_or(false);
    if released {
        notify(db, encrypter, team_id, Notification::Reachable { server }).await;
    }
}

/// `Server::sendUnreachableNotification`: `NotificationThrottle::sendOnce(..., null, ...)`.
pub async fn send_unreachable_notification(
    db: &Db,
    encrypter: Option<&Encrypter>,
    team_id: i64,
    server: ServerRef,
) {
    let claimed = claim_throttle(db, SERVER_MODEL, server.id, UNREACHABLE_CLASS, None)
        .await
        .unwrap_or(false);
    if claimed {
        notify(db, encrypter, team_id, Notification::Unreachable { server }).await;
    }
}

const SERVER_MODEL: &str = "App\\Models\\Server";
const UNREACHABLE_CLASS: &str = "App\\Notifications\\Server\\Unreachable";
const HIGH_DISK_CLASS: &str = "App\\Notifications\\Server\\HighDiskUsage";

/// `NotificationThrottle::release($server, HighDiskUsage::class)`.
pub async fn release_high_disk_usage(db: &Db, server_id: i64) {
    if let Err(e) = release_throttle(db, SERVER_MODEL, server_id, HIGH_DISK_CLASS).await {
        tracing::warn!(error = %e, "could not release the disk usage throttle");
    }
}

/// `$team->notify($notification)`. Runs the throttle and rate limit here, then delivers on a
/// spawned task for each channel. Never fails.
pub async fn notify(
    db: &Db,
    encrypter: Option<&Encrypter>,
    team_id: i64,
    notification: Notification,
) {
    let db = db.clone();
    let encrypter = encrypter.cloned();
    tokio::spawn(async move {
        deliver(&db, encrypter.as_ref(), team_id, notification).await;
    });
}

/// `notify` on the current task, waiting until every channel has finished.
pub async fn deliver(
    db: &Db,
    encrypter: Option<&Encrypter>,
    team_id: i64,
    notification: Notification,
) {
    if let Some(t) = notification.throttle() {
        let before = Utc::now() - chrono::Duration::minutes(t.minutes);
        match claim_throttle(
            db,
            &t.notifiable_type,
            t.notifiable_id,
            t.notification,
            Some(before),
        )
        .await
        {
            Ok(true) => {}
            Ok(false) => return,
            Err(e) => {
                tracing::warn!(error = %e, "could not claim a notification throttle");
                return;
            }
        }
    }
    // `isEmailRateLimited`: `RateLimiter::attempt($key, 0, ..., 3600)`.
    if let Some(key) = notification.rate_limit_key()
        && !rate_limiter_attempt(&key, Duration::from_secs(3600))
    {
        return;
    }
    channels::deliver_all(db, encrypter, team_id, &notification).await;
}

/// `$team->notifyNow(new Test($emails, $channel))` and the Telegram page's direct send. Errors
/// are returned so the page can show them. Discord, Slack, Pushover and Webhook go through the
/// same request as the queued job, once; test notifications never count as delivery failures.
pub async fn notify_test(
    db: &Db,
    encrypter: Option<&Encrypter>,
    team_id: i64,
    channel: &str,
    emails: Option<String>,
) -> Result<(), String> {
    channels::send_test(db, encrypter, team_id, channel, emails).await
}

/// Transactional mail through the instance settings: `TransactionalEmailChannel`,
/// `send_user_an_email` and, for `Mail::Test`, `EmailChannel`.
pub async fn send_transactional(
    db: &Db,
    encrypter: Option<&Encrypter>,
    to: &str,
    mail: Mail,
) -> Result<(), String> {
    channels::send_transactional(db, encrypter, to, &mail).await
}

/// Renders a notification on every channel, for the differential test.
pub fn render(
    ctx: &Ctx,
    notification: &Notification,
    commit_details: bool,
    timestamp: i64,
) -> Value {
    use coolify_compat::php_json::encode;
    let mail = notification.to_mail(ctx, commit_details);
    let slack = notification.to_slack(ctx);
    let telegram = notification.to_telegram(ctx);
    serde_json::json!({
        "subject": mail.as_ref().map(|m| m.subject.clone()),
        "html": mail.as_ref().map(|m| m.html.clone()),
        "discord_body": encode(&notification.to_discord(ctx).to_payload(ctx, timestamp)),
        "slack_body": encode(&channels::slack_body(ctx, "https://hooks.slack.com/services/x", &slack)),
        "mattermost_body": encode(&channels::slack_body(ctx, "https://mm.example/hooks/x", &slack)),
        "telegram_body": encode(&channels::telegram_payload(ctx, &telegram, " C ", Some("7"))),
        "pushover_body": encode(&notification.to_pushover(ctx).to_payload(ctx, "T", "U")),
        "webhook_body": encode(&notification.to_webhook(ctx)),
        "event": notification.event(),
    })
}

/// Renders a transactional mail, for the differential test.
pub fn render_mail(ctx: &Ctx, mail: &Mail) -> MailMessage {
    mail.render(ctx)
}
