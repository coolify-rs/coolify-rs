//! `Team::getEnabledChannels`, the channels of `app/Notifications/Channels` and the jobs they
//! dispatch (`SendMessageToDiscordJob`, ...), with `HandlesNotificationResponse`.

use std::net::IpAddr;
use std::time::Duration;

use coolify_compat::Encrypter;
use coolify_compat::php_json;
use coolify_db::notification_settings::{self as ns, Channel};
use coolify_db::records::Record;
use coolify_db::{Db, instance_settings};
use lettre::message::{Mailbox, header::ContentType};
use lettre::transport::smtp::AsyncSmtpTransportBuilder;
use lettre::transport::smtp::authentication::Credentials;
use lettre::transport::smtp::client::{Tls, TlsParameters};
use lettre::transport::smtp::extension::ClientId;
use lettre::{AsyncSmtpTransport, AsyncTransport, Tokio1Executor};
use serde_json::{Value, json};

use super::messages::{
    Mail, MailMessage, Notification, TelegramMessage, button_parts, test_notification,
};
use super::php::{client_status_text, php_trim, truthy};
use super::{Ctx, tracking};
use crate::databases::s3::{UrlPolicy, endpoint_error, safe_url_pin};

/// `Laravel's PendingRequest`: Guzzle's default User-Agent header is `GuzzleHttp/7`.
const USER_AGENT: &str = "GuzzleHttp/7";
const TELEGRAM_API: &str = "https://api.telegram.org";
const PUSHOVER_API: &str = "https://api.pushover.net";
const RESEND_API: &str = "https://api.resend.com";
/// `Resend::VERSION` of the reference.
const RESEND_USER_AGENT: &str = "resend-php/1.3.0";

/// Decrypted columns of a settings row.
struct Settings {
    row: Record,
    encrypter: Option<Encrypter>,
}

impl Settings {
    fn flag(&self, key: &str) -> bool {
        self.row.get(key).and_then(Value::as_bool).unwrap_or(false)
    }

    fn plain(&self, key: &str) -> Option<String> {
        match self.row.get(key) {
            Some(Value::String(s)) => Some(s.clone()),
            Some(Value::Number(n)) => Some(n.to_string()),
            _ => None,
        }
    }

    /// An `encrypted` cast.
    fn secret(&self, key: &str) -> Result<Option<String>, String> {
        let Some(payload) = self.plain(key) else {
            return Ok(None);
        };
        let enc = self
            .encrypter
            .as_ref()
            .ok_or("APP_KEY is not available to decrypt notification settings.")?;
        enc.decrypt_string(&payload)
            .map(Some)
            .map_err(|e| format!("The payload is invalid ({key}): {e}"))
    }

    fn int(&self, key: &str) -> Option<i64> {
        self.row.get(key).and_then(Value::as_i64)
    }
}

/// `isEnabled()` of each settings model.
fn is_enabled(ch: &Channel, s: &Settings) -> bool {
    match ch.name {
        "email" => {
            s.flag("smtp_enabled")
                || s.flag("resend_enabled")
                || s.flag("use_instance_email_settings")
        }
        "discord" => s.flag("discord_enabled"),
        "slack" => s.flag("slack_enabled"),
        "telegram" => s.flag("telegram_enabled"),
        "pushover" => s.flag("pushover_enabled"),
        _ => s.flag("webhook_enabled"),
    }
}

/// `$alwaysSendEvents`.
const ALWAYS_SEND: [&str; 7] = [
    "server_force_enabled",
    "server_force_disabled",
    "general",
    "test",
    "ssl_certificate_renewal",
    "hetzner_deletion_failure",
    "api_token_expiring",
];

pub(super) async fn any_enabled(
    db: &Db,
    encrypter: Option<&Encrypter>,
    team_id: i64,
    event: &str,
) -> bool {
    !enabled_channels(db, encrypter, team_id, event)
        .await
        .is_empty()
}

/// `HasNotificationSettings::getEnabledChannels($event)`. Rows are created by `Team::created`
/// upstream; a missing row (`null` relation) is a disabled channel.
async fn enabled_channels(
    db: &Db,
    encrypter: Option<&Encrypter>,
    team_id: i64,
    event: &str,
) -> Vec<(&'static Channel, Settings)> {
    let order: [&'static Channel; 6] = [
        &ns::EMAIL,
        &ns::DISCORD,
        &ns::TELEGRAM,
        &ns::SLACK,
        &ns::PUSHOVER,
        &ns::WEBHOOK,
    ];
    let mut out = Vec::new();
    for ch in order {
        if event == "general" && ch.name == "email" {
            continue;
        }
        let row = match ns::find(db, ch, team_id).await {
            Ok(Some(r)) => r,
            Ok(None) => continue,
            Err(e) => {
                tracing::warn!(error = %e, channel = ch.name, "could not read notification settings");
                continue;
            }
        };
        let settings = Settings {
            row,
            encrypter: encrypter.cloned(),
        };
        if !is_enabled(ch, &settings) {
            continue;
        }
        let key = format!("{event}_{}_notifications", ch.name);
        if ALWAYS_SEND.contains(&event) || settings.flag(&key) {
            out.push((ch, settings));
        }
    }
    out
}

/// `$team->getNotificationSettings($channel)` for `Test::via`.
async fn named_channel(
    db: &Db,
    encrypter: Option<&Encrypter>,
    team_id: i64,
    channel: &str,
) -> Vec<(&'static Channel, Settings)> {
    let Some(ch) = ns::ALL.iter().copied().find(|c| c.name == channel) else {
        return Vec::new();
    };
    match ns::find(db, ch, team_id).await {
        Ok(Some(row)) => vec![(
            ch,
            Settings {
                row,
                encrypter: encrypter.cloned(),
            },
        )],
        _ => Vec::new(),
    }
}

/// Queues `notification` for a team: a task for each enabled channel (`via()`), awaited here.
pub async fn deliver_all(
    db: &Db,
    encrypter: Option<&Encrypter>,
    team_id: i64,
    notification: &Notification,
) {
    let ctx = Ctx::load(db).await;
    let channels = match notification {
        // `Test::via`: a named channel is used whether it is enabled or not.
        Notification::Test {
            channel: Some(channel),
            ..
        } => named_channel(db, encrypter, team_id, channel).await,
        _ => enabled_channels(db, encrypter, team_id, notification.event()).await,
    };
    let mut tasks = Vec::new();
    for (ch, settings) in channels {
        let (db, encrypter, ctx, notification) = (
            db.clone(),
            encrypter.cloned(),
            ctx.clone(),
            notification.clone(),
        );
        tasks.push(tokio::spawn(async move {
            send_channel(
                &db,
                encrypter.as_ref(),
                team_id,
                ch,
                settings,
                &ctx,
                &notification,
            )
            .await;
        }));
    }
    for t in tasks {
        let _ = t.await;
    }
}

/// `$team->notify(new GeneralNotification(...))` from a place that must not recurse into itself.
pub fn spawn_general(
    db: &Db,
    encrypter: Option<&Encrypter>,
    team_id: i64,
    notification: Notification,
) {
    let (db, encrypter) = (db.clone(), encrypter.cloned());
    tokio::spawn(async move {
        Box::pin(deliver_all(&db, encrypter.as_ref(), team_id, &notification)).await;
    });
}

/// One queued job: `Channel::send`, and the job it dispatches.
async fn send_channel(
    db: &Db,
    encrypter: Option<&Encrypter>,
    team_id: i64,
    ch: &'static Channel,
    settings: Settings,
    ctx: &Ctx,
    notification: &Notification,
) {
    let result = match ch.name {
        "email" => email_with_retries(db, encrypter, team_id, &settings, ctx, notification).await,
        "discord" => discord(db, encrypter, team_id, &settings, ctx, notification).await,
        "slack" => slack(db, encrypter, team_id, &settings, ctx, notification).await,
        "telegram" => telegram(db, encrypter, team_id, &settings, ctx, notification).await,
        "pushover" => pushover(db, encrypter, team_id, &settings, ctx, notification).await,
        _ => webhook(db, encrypter, team_id, &settings, ctx, notification).await,
    };
    if let Err(e) = result {
        tracing::warn!(channel = ch.name, team_id, error = %e, "notification delivery failed");
    }
}

/// `$tries` and `$maxExceptions`, whichever ends the job first, with `$backoff`.
async fn with_retries<F, Fut>(attempts: u32, backoff: u64, mut send: F) -> Result<(), String>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<(), String>>,
{
    let mut last = Ok(());
    for attempt in 0..attempts {
        if attempt > 0 {
            tokio::time::sleep(Duration::from_secs(backoff)).await;
        }
        last = send().await;
        if last.is_ok() {
            return Ok(());
        }
    }
    last
}

/// The HTTP response the notification jobs look at.
struct Response {
    status: u16,
    body: String,
}

impl Response {
    fn json(&self, key: &str) -> Option<Value> {
        serde_json::from_str::<Value>(&self.body)
            .ok()
            .and_then(|v| v.get(key).cloned())
    }
}

/// `Http::post($url, $payload)` (JSON body in PHP's `json_encode` form). `pin` is
/// `SafeWebhookUrl::httpClientOptions`: no redirects, the validated addresses only.
async fn post_json(
    url: &str,
    payload: &Value,
    pin: Option<(String, Vec<IpAddr>, u16)>,
) -> Result<Response, reqwest::Error> {
    let mut builder = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .user_agent(USER_AGENT);
    if let Some((host, ips, port)) = pin {
        builder = builder.redirect(reqwest::redirect::Policy::none());
        if !ips.is_empty() {
            let addrs: Vec<std::net::SocketAddr> = ips
                .into_iter()
                .map(|ip| std::net::SocketAddr::new(ip, port))
                .collect();
            builder = builder.resolve_to_addrs(&host, &addrs);
        }
    }
    let response = builder
        .build()?
        .post(url)
        .header("Content-Type", "application/json")
        .body(php_json::encode(payload))
        .send()
        .await?;
    let status = response.status().as_u16();
    let body = response.text().await.unwrap_or_default();
    Ok(Response { status, body })
}

/// `SafeWebhookUrl::redactedUrlForLog`.
fn redacted_url(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return "[invalid-url]".to_owned();
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host = authority.rsplit('@').next().unwrap_or_default();
    if host.is_empty() {
        return "[invalid-url]".to_owned();
    }
    format!("{}://{}", scheme.to_lowercase(), host.to_lowercase())
}

/// `sendNotificationRequest`: errors name only the scheme and host.
async fn request(
    channel: &str,
    url: &str,
    payload: &Value,
    pin: Option<(String, Vec<IpAddr>, u16)>,
) -> Result<Response, String> {
    post_json(url, payload, pin).await.map_err(|e| {
        let kind = if e.is_connect() || e.is_timeout() {
            "ConnectionException"
        } else {
            "RequestException"
        };
        format!(
            "{channel} notification request to {} failed ({kind}).",
            redacted_url(url)
        )
    })
}

/// `handleNotificationResponse`. `Err` means the queue retries.
#[allow(clippy::too_many_arguments)]
async fn handle_response(
    db: &Db,
    encrypter: Option<&Encrypter>,
    track: Option<(&'static Channel, i64)>,
    response: &Response,
    channel: &str,
    is_configuration_error: bool,
) -> Result<(), String> {
    let status = response.status;
    if (200..300).contains(&status) {
        if let Some((ch, team)) = track {
            tracking::record_success(db, ch, team).await;
        }
        return Ok(());
    }
    if status == 429 || (500..600).contains(&status) {
        return Err(format!(
            "{channel} notification failed with {status} status code."
        ));
    }
    let reason = format!(
        "{channel} returned HTTP {status} ({}).",
        client_status_text(status)
    );
    // A test notification is not tracked; its caller shows the reason.
    let Some((ch, team)) = track else {
        return Err(reason);
    };
    if is_configuration_error {
        tracking::record_failure(db, encrypter, ch, team, &reason).await;
        return Ok(());
    }
    tracing::warn!(reason = %reason, "{channel} rejected the notification");
    Ok(())
}

/// `$tries`, or one try for a test notification, which reports its error to the page.
fn attempts(notification: &Notification, tries: u32) -> u32 {
    if notification.is_test() { 1 } else { tries }
}

/// The check at the top of the webhook jobs: `SafeWebhookUrl` and `httpClientOptions`. `None`
/// means "blocked": the job logs and ends without a failure.
async fn safe_target(db: &Db, job: &str, url: &str) -> Option<(String, Vec<IpAddr>, u16)> {
    let policy = UrlPolicy::load(db).await;
    if let Some(error) = endpoint_error(url, "webhook url", &policy, "").await {
        tracing::warn!(
            url = %redacted_url(url),
            error = %error,
            "{job}: blocked unsafe webhook URL"
        );
        return None;
    }
    match safe_url_pin(url, &policy).await {
        Ok(pin) => Some(pin),
        Err(e) => {
            tracing::warn!(
                url = %redacted_url(url),
                error = %e,
                "{job}: blocked unsafe webhook URL at send time"
            );
            None
        }
    }
}

/// `DiscordChannel::send` and `SendMessageToDiscordJob`.
async fn discord(
    db: &Db,
    encrypter: Option<&Encrypter>,
    team_id: i64,
    settings: &Settings,
    ctx: &Ctx,
    notification: &Notification,
) -> Result<(), String> {
    let mut message = notification.to_discord(ctx);
    if !settings.flag("discord_enabled") {
        return Ok(());
    }
    let Some(url) = settings
        .secret("discord_webhook_url")?
        .filter(|u| truthy(Some(u)))
    else {
        return Ok(());
    };
    if !settings.flag("discord_ping_enabled") {
        message.is_critical = false;
    }
    let track = (!notification.is_test()).then_some((&ns::DISCORD, team_id));
    with_retries(attempts(notification, 5), 10, || async {
        let Some(pin) = safe_target(db, "SendMessageToDiscordJob", &url).await else {
            return Ok(());
        };
        let payload = message.to_payload(ctx, chrono::Utc::now().timestamp());
        let response = request("Discord", &url, &payload, Some(pin)).await?;
        let config = matches!(response.status, 401 | 403 | 404);
        handle_response(db, encrypter, track, &response, "Discord", config).await
    })
    .await
}

/// `SendMessageToSlackJob::isSlackWebhook`.
fn is_slack_webhook(url: &str) -> bool {
    let Some((scheme, rest)) = url.split_once("://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host = authority.rsplit('@').next().unwrap_or_default();
    scheme == "https" && host == "hooks.slack.com"
}

/// `sendToSlack` / `sendToMattermost`.
pub(super) fn slack_body(ctx: &Ctx, url: &str, m: &super::messages::SlackMessage) -> Value {
    if is_slack_webhook(url) {
        json!({
            "text": m.title,
            "blocks": [
                {"type": "section", "text": {"type": "plain_text", "text": "Coolify Notification"}},
            ],
            "attachments": [{
                "color": m.color,
                "blocks": [
                    {"type": "header", "text": {"type": "plain_text", "text": m.title}},
                    {"type": "section", "text": {"type": "mrkdwn", "text": m.description}},
                ],
            }],
        })
    } else {
        json!({
            "username": ctx.app_name,
            "attachments": [{
                "title": m.title,
                "color": m.color,
                "text": m.description,
                "footer": ctx.app_name,
            }],
        })
    }
}

/// `SlackChannel::send` and `SendMessageToSlackJob`.
async fn slack(
    db: &Db,
    encrypter: Option<&Encrypter>,
    team_id: i64,
    settings: &Settings,
    ctx: &Ctx,
    notification: &Notification,
) -> Result<(), String> {
    let message = notification.to_slack(ctx);
    if !settings.flag("slack_enabled") {
        return Ok(());
    }
    let Some(url) = settings
        .secret("slack_webhook_url")?
        .filter(|u| truthy(Some(u)))
    else {
        return Ok(());
    };
    let track = (!notification.is_test()).then_some((&ns::SLACK, team_id));
    with_retries(attempts(notification, 5), 10, || async {
        let Some(pin) = safe_target(db, "SendMessageToSlackJob", &url).await else {
            return Ok(());
        };
        let body = slack_body(ctx, &url, &message);
        let response = request("Slack", &url, &body, Some(pin)).await?;
        let config = matches!(response.status, 403 | 404 | 410);
        handle_response(db, encrypter, track, &response, "Slack", config).await
    })
    .await
}

fn telegram_base() -> String {
    std::env::var("COOLIFY_RS_TELEGRAM_API").unwrap_or_else(|_| TELEGRAM_API.to_owned())
}

/// `SendMessageToTelegramJob::sendMessage`.
async fn telegram_send(
    ctx: &Ctx,
    message: &TelegramMessage,
    token: &str,
    chat_id: &str,
    thread_id: Option<&str>,
) -> Result<Response, String> {
    let url = format!("{}/bot{}/sendMessage", telegram_base(), php_trim(token));
    let payload = telegram_payload(ctx, message, chat_id, thread_id);
    request("Telegram", &url, &payload, None).await
}

/// The body of `SendMessageToTelegramJob::sendMessage`.
pub(super) fn telegram_payload(
    ctx: &Ctx,
    message: &TelegramMessage,
    chat_id: &str,
    thread_id: Option<&str>,
) -> Value {
    let inline: Vec<Value> = message
        .buttons
        .iter()
        .map(|b| {
            let (text, url) = button_parts(b, &ctx.app_url);
            json!({"text": text, "url": url})
        })
        .collect();
    let mut payload = json!({
        "reply_markup": php_json::encode(&json!({"inline_keyboard": [inline]})),
        "chat_id": php_trim(chat_id),
        "text": message.message,
    });
    if truthy(thread_id) {
        payload["message_thread_id"] = json!(thread_id);
    }
    payload
}

/// `TelegramChannel::send` and `SendMessageToTelegramJob`: 3 exceptions end the job.
async fn telegram(
    db: &Db,
    encrypter: Option<&Encrypter>,
    team_id: i64,
    settings: &Settings,
    ctx: &Ctx,
    notification: &Notification,
) -> Result<(), String> {
    let message = notification.to_telegram(ctx);
    let token = settings.secret("telegram_token")?;
    let chat_id = settings.secret("telegram_chat_id")?;
    let thread_id = match notification.telegram_thread_column() {
        Some(column) => settings.secret(column)?,
        None => None,
    };
    let (Some(token), Some(chat_id)) = (
        token.filter(|t| truthy(Some(t))),
        chat_id.filter(|c| truthy(Some(c))),
    ) else {
        return Ok(());
    };
    if !truthy(Some(&message.message)) {
        return Ok(());
    }
    let track = (!notification.is_test()).then_some((&ns::TELEGRAM, team_id));
    with_retries(attempts(notification, 3), 10, || async {
        let response = telegram_send(ctx, &message, &token, &chat_id, thread_id.as_deref()).await?;
        let description = response.json("description").map(|v| match v {
            Value::String(s) => s,
            other => other.to_string(),
        });
        let config = matches!(response.status, 401 | 403 | 404)
            || (response.status == 400
                && description
                    .as_deref()
                    .is_some_and(|d| d.to_lowercase().contains("chat not found")));
        handle_response(db, encrypter, track, &response, "Telegram", config).await
    })
    .await
}

fn pushover_url() -> String {
    let base = std::env::var("COOLIFY_RS_PUSHOVER_API").unwrap_or_else(|_| PUSHOVER_API.to_owned());
    format!("{base}/1/messages.json")
}

/// `PushoverChannel::send` and `SendMessageToPushoverJob`.
async fn pushover(
    db: &Db,
    encrypter: Option<&Encrypter>,
    team_id: i64,
    settings: &Settings,
    ctx: &Ctx,
    notification: &Notification,
) -> Result<(), String> {
    let message = notification.to_pushover(ctx);
    if !settings.flag("pushover_enabled") {
        return Ok(());
    }
    let user = settings.secret("pushover_user_key")?;
    let token = settings.secret("pushover_api_token")?;
    let (Some(user), Some(token)) = (
        user.filter(|t| truthy(Some(t))),
        token.filter(|t| truthy(Some(t))),
    ) else {
        return Ok(());
    };
    let track = (!notification.is_test()).then_some((&ns::PUSHOVER, team_id));
    let url = pushover_url();
    with_retries(attempts(notification, 5), 10, || async {
        let payload = message.to_payload(ctx, &token, &user);
        let response = request("Pushover", &url, &payload, None).await?;
        let invalid =
            |k: &str| response.json(k).as_ref().and_then(Value::as_str) == Some("invalid");
        let config = (400..500).contains(&response.status) && (invalid("token") || invalid("user"));
        handle_response(db, encrypter, track, &response, "Pushover", config).await
    })
    .await
}

/// `WebhookChannel::send` and `SendWebhookJob`.
async fn webhook(
    db: &Db,
    encrypter: Option<&Encrypter>,
    team_id: i64,
    settings: &Settings,
    ctx: &Ctx,
    notification: &Notification,
) -> Result<(), String> {
    if !settings.flag("webhook_enabled") {
        return Ok(());
    }
    let Some(url) = settings.secret("webhook_url")?.filter(|u| truthy(Some(u))) else {
        return Ok(());
    };
    let payload = notification.to_webhook(ctx);
    let track = (!notification.is_test()).then_some((&ns::WEBHOOK, team_id));
    with_retries(attempts(notification, 5), 10, || async {
        let Some(pin) = safe_target(db, "SendWebhookJob", &url).await else {
            return Ok(());
        };
        let response = request("Webhook", &url, &payload, Some(pin)).await?;
        let config = matches!(response.status, 401 | 403 | 404 | 410);
        handle_response(db, encrypter, track, &response, "Webhook", config).await
    })
    .await
}

/// `mail_from_identity`.
fn mail_from(settings: &Settings) -> Result<(String, String), String> {
    let address = settings
        .secret("smtp_from_address")?
        .filter(|a| !php_trim(a).is_empty())
        .ok_or("Transactional email sender address is not configured.")?;
    let name = settings.secret("smtp_from_name")?.unwrap_or_default();
    let name = php_trim(&name);
    Ok((
        address,
        if name.is_empty() {
            "Coolify".to_owned()
        } else {
            name.to_owned()
        },
    ))
}

/// `Symfony\Component\Mime\Address::toString()`: `"Name" <address>`, encoded when not ASCII.
fn formatted_from(address: &str, name: &str) -> String {
    if name.is_ascii() {
        let escaped = name.replace('\\', "\\\\").replace('"', "\\\"");
        format!("\"{escaped}\" <{address}>")
    } else {
        use base64::Engine;
        let encoded = base64::engine::general_purpose::STANDARD.encode(name);
        format!("=?UTF-8?B?{encoded}?= <{address}>")
    }
}

/// The failure of `Mailer::send`.
struct SmtpFailure {
    code: Option<u16>,
    message: String,
}

/// `SmtpTransportFactory::fromSettings` and `$mailer->send($email)`.
async fn smtp_send(
    settings: &Settings,
    from: &(String, String),
    to: &[String],
    mail: &MailMessage,
) -> Result<(), SmtpFailure> {
    smtp_send_with_cc(settings, from, to, None, mail).await
}

/// `->replyTo($email)->cc($cc)` of `send_user_an_email`.
async fn smtp_send_with_cc(
    settings: &Settings,
    from: &(String, String),
    to: &[String],
    cc: Option<&str>,
    mail: &MailMessage,
) -> Result<(), SmtpFailure> {
    let fail = |message: String| SmtpFailure {
        code: None,
        message,
    };
    let host = settings
        .secret("smtp_host")
        .map_err(&fail)?
        .unwrap_or_default();
    let host = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_owned();
    let port = settings.int("smtp_port").unwrap_or(0) as u16;
    let mode = settings.plain("smtp_encryption").map(|m| m.to_lowercase());
    let tls_parameters = TlsParameters::new(host.clone()).map_err(|e| fail(e.to_string()))?;
    let implicit_tls =
        mode.as_deref() == Some("tls") || (mode.as_deref() != Some("none") && port == 465);
    let symfony_host = host.clone();
    let tls = match mode.as_deref() {
        Some("none") => Tls::None,
        Some("tls") => Tls::Wrapper(tls_parameters),
        // `new EsmtpTransport($host, $port, null)`: implicit TLS on 465, else STARTTLS if offered.
        _ if port == 465 => Tls::Wrapper(tls_parameters),
        _ => Tls::Opportunistic(tls_parameters),
    };
    let mut builder: AsyncSmtpTransportBuilder =
        AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&host)
            .port(port)
            .tls(tls);
    if let Some(ehlo) = settings.plain("smtp_ehlo_domain").filter(|d| !d.is_empty()) {
        builder = builder.hello_name(ClientId::Domain(ehlo));
    }
    if let Some(seconds) = settings.int("smtp_timeout") {
        builder = builder.timeout(Some(Duration::from_secs(seconds.max(0) as u64)));
    }
    let username = settings
        .secret("smtp_username")
        .map_err(&fail)?
        .unwrap_or_default();
    let password = settings
        .secret("smtp_password")
        .map_err(&fail)?
        .unwrap_or_default();

    // Symfony leaves `"` in the header unescaped (`Notif "E2E" <a@b.c>`), which mail clients read
    // as a quoted word: the name they show has no quotes.
    let mailbox: Mailbox = format!("{} <{}>", quote_name(&from.1.replace('"', "")), from.0)
        .parse()
        .map_err(|e: lettre::address::AddressError| fail(e.to_string()))?;
    let mut message = lettre::Message::builder()
        .from(mailbox)
        .subject(mail.subject.clone());
    if let Some(cc) = cc {
        let mb: Mailbox = cc
            .parse()
            .map_err(|e: lettre::address::AddressError| fail(e.to_string()))?;
        message = message.cc(mb);
    }
    for recipient in to {
        let mb: Mailbox = recipient
            .parse()
            .map_err(|e: lettre::address::AddressError| fail(e.to_string()))?;
        message = message.to(mb.clone());
        if cc.is_some() {
            message = message.reply_to(mb);
        }
    }
    let message = message
        .header(ContentType::TEXT_HTML)
        .body(mail.html.clone())
        .map_err(|e| fail(e.to_string()))?;

    let authed = if username.is_empty() {
        builder.clone()
    } else {
        builder
            .clone()
            .credentials(Credentials::new(username, password))
    };
    let send = |t: AsyncSmtpTransport<Tokio1Executor>, m: lettre::Message| async move {
        t.send(m).await.map(|_| ())
    };
    match send(authed.build(), message.clone()).await {
        Ok(()) => Ok(()),
        Err(e) => {
            let text = e.to_string();
            // Symfony skips authentication on a server without `AUTH`; lettre refuses to continue.
            if text.contains("No compatible authentication mechanism") {
                return send(builder.build(), message)
                    .await
                    .map_err(|e| SmtpFailure {
                        code: smtp_code(&e),
                        message: e.to_string(),
                    });
            }
            // Symfony's `SocketStream::initialize` message, which the test email toast shows.
            let message = if text.contains("Connection refused") {
                let scheme = if implicit_tls { "ssl://" } else { "" };
                let url = format!("{scheme}{symfony_host}:{port}");
                format!(
                    "Connection could not be established with host \"{url}\": stream_socket_client(): Unable to connect to {url} (Connection refused)"
                )
            } else {
                text
            };
            Err(SmtpFailure {
                code: smtp_code(&e),
                message,
            })
        }
    }
}

fn smtp_code(e: &lettre::transport::smtp::Error) -> Option<u16> {
    e.status().and_then(|c| c.to_string().parse().ok())
}

fn quote_name(name: &str) -> String {
    format!("\"{}\"", name.replace('\\', "\\\\").replace('"', "\\\""))
}

/// The `ErrorException` of the Resend SDK.
struct ResendError {
    status: i64,
    name: Option<String>,
    message: String,
}

enum ResendFailure {
    /// `ErrorException`.
    Api(ResendError),
    /// `TransporterException`.
    Transport,
}

fn resend_url() -> String {
    let base = std::env::var("COOLIFY_RS_RESEND_API").unwrap_or_else(|_| RESEND_API.to_owned());
    format!("{base}/emails")
}

/// `$resend->emails->send($body)`.
async fn resend_send(api_key: &str, body: &Value) -> Result<(), ResendFailure> {
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .user_agent(RESEND_USER_AGENT)
        .build()
        .map_err(|_| ResendFailure::Transport)?;
    let response = client
        .post(resend_url())
        .header("Authorization", format!("Bearer {api_key}"))
        .header("Content-Type", "application/json")
        .body(php_json::encode(body))
        .send()
        .await
        .map_err(|_| ResendFailure::Transport)?;
    let status = response.status().as_u16();
    let is_json = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|t| t.contains("application/json"));
    let text = response.text().await.unwrap_or_default();
    let parsed: Option<Value> = serde_json::from_str(&text).ok();
    let from_contents = |contents: &Value| {
        let status = contents
            .get("code")
            .or_else(|| contents.get("statusCode"))
            .and_then(|v| {
                v.as_i64()
                    .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
            })
            .unwrap_or(0);
        ResendError {
            status,
            name: contents
                .get("name")
                .and_then(Value::as_str)
                .map(str::to_owned),
            message: contents
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("Resend did not accept the email.")
                .to_owned(),
        }
    };
    if status >= 400
        && is_json
        && let Some(v) = &parsed
    {
        if let Some(error) = v.get("error") {
            let contents = if error.is_object() {
                error.clone()
            } else {
                json!({"message": error.as_str().map(str::to_owned).unwrap_or_else(|| error.to_string())})
            };
            return Err(ResendFailure::Api(from_contents(&contents)));
        }
        return Err(ResendFailure::Api(from_contents(v)));
    }
    // A response without `id`: `throw new ErrorException($response->toArray())`.
    let id = parsed.as_ref().and_then(|v| v.get("id"));
    let has_id = id.is_some_and(|v| match v {
        Value::Null => false,
        Value::String(s) => !php_trim(s).is_empty(),
        _ => true,
    });
    if !has_id {
        let contents = parsed.unwrap_or_else(|| json!({}));
        return Err(ResendFailure::Api(from_contents(&contents)));
    }
    Ok(())
}

/// `EmailChannel::resend` error names to user messages.
fn resend_user_message(e: &ResendError) -> String {
    let (status, name) = (e.status, e.name.as_deref());
    if name == Some("restricted_api_key") || (status == 401 && name.is_none()) {
        "Your Resend API key has restricted permissions. Please use an API key with Full Access permissions.".to_owned()
    } else if status == 401 || (status == 403 && name != Some("validation_error")) {
        "Invalid Resend API key. Please verify your API key in the Resend dashboard and update it in settings.".to_owned()
    } else if matches!(status, 400 | 422) || name == Some("validation_error") {
        format!("Email validation failed: {}", e.message)
    } else if status == 429 && matches!(name, None | Some("rate_limit_exceeded")) {
        "Resend rate limit exceeded. Please try again in a few minutes.".to_owned()
    } else {
        format!("Failed to send email via Resend: {}", e.message)
    }
}

/// The team members' addresses (`$team->members`).
async fn team_members(db: &Db, team_id: i64) -> Result<Vec<String>, String> {
    sqlx::query_scalar(
        "SELECT u.email FROM users u JOIN team_user t ON t.user_id = u.id WHERE t.team_id = $1 ORDER BY u.id",
    )
    .bind(team_id)
    .fetch_all(db.pool())
    .await
    .map_err(|e| e.to_string())
}

/// `FILTER_VALIDATE_EMAIL`, close enough for addresses `users.email` accepted.
fn valid_email(email: &str) -> bool {
    email.parse::<lettre::Address>().is_ok()
}

/// `EmailChannel::send`. Team settings count as the tracked `EmailNotificationSettings`.
#[allow(clippy::too_many_arguments)]
async fn email_channel(
    db: &Db,
    encrypter: Option<&Encrypter>,
    team_id: i64,
    team_settings: &Settings,
    transactional: bool,
    custom_emails: Option<&str>,
    is_test: bool,
    mail: &MailMessage,
) -> Result<(), String> {
    let use_instance = team_settings.flag("use_instance_email_settings") || transactional;
    let instance;
    let settings = if use_instance {
        instance = Settings {
            row: instance_settings::row(db)
                .await
                .map_err(|e| e.to_string())?,
            encrypter: encrypter.cloned(),
        };
        &instance
    } else {
        team_settings
    };
    let tracked = !use_instance;
    let members = team_members(db, team_id).await?;
    let recipients: Vec<String> = match custom_emails.filter(|e| !e.is_empty() && *e != "0") {
        Some(e) => vec![e.to_owned()],
        None => members.iter().filter(|m| valid_email(m)).cloned().collect(),
    };
    if recipients.is_empty() {
        return Err("No email recipients found".to_owned());
    }
    if !is_test {
        for r in &recipients {
            if !members.contains(r) {
                let text = format!(
                    "Recipient is not part of the team: {r}\nTeam: {team_id}\nNotification: Notification\nNotifiable: App\\Models\\Team\nEmail Settings:\n{}",
                    email_settings_json(team_settings)
                );
                spawn_internal(db, encrypter, text);
                return Err("Recipient is not part of the team".to_owned());
            }
        }
    }

    if settings.flag("resend_enabled") {
        let from = mail_from(settings)?;
        let body = json!({
            "from": formatted_from(&from.0, &from.1),
            "to": recipients,
            "subject": mail.subject,
            "html": mail.html,
        });
        let api_key = settings.secret("resend_api_key")?.unwrap_or_default();
        match resend_send(&api_key, &body).await {
            Ok(()) => {}
            Err(ResendFailure::Transport) => {
                spawn_internal(
                    db,
                    encrypter,
                    "Resend Transport Error: request failed".to_owned(),
                );
                return Err(
                    "Unable to connect to Resend API. Please check your internet connection and try again."
                        .to_owned(),
                );
            }
            Err(ResendFailure::Api(e)) => {
                let user_message = resend_user_message(&e);
                if !is_test && tracked {
                    if matches!(e.status, 401 | 403) {
                        tracking::record_failure(db, encrypter, &ns::EMAIL, team_id, &user_message)
                            .await;
                        return Ok(());
                    }
                    if matches!(e.status, 400 | 422) {
                        tracing::warn!(team_id, reason = %user_message, "Resend rejected the email");
                        return Ok(());
                    }
                    return Err(user_message);
                }
                let message = coolify_compat::redact::redact_text(&e.message, &[], &[]);
                spawn_internal(
                    db,
                    encrypter,
                    format!(
                        "Resend Error\nStatus Code: {}\nMessage: {message}\nNotification: Notification\nEmail Settings:\n{}",
                        e.status,
                        email_settings_json(team_settings)
                    ),
                );
                return Err(user_message);
            }
        }
    } else if settings.flag("smtp_enabled") {
        let from = mail_from(settings)?;
        if let Err(e) = smtp_send(settings, &from, &recipients, mail).await {
            if !is_test && tracked && matches!(e.code, Some(530 | 535)) {
                tracking::record_failure(
                    db,
                    encrypter,
                    &ns::EMAIL,
                    team_id,
                    "SMTP authentication failed. Please verify the SMTP username and password.",
                )
                .await;
                return Ok(());
            }
            return Err(e.message);
        }
    }

    if !is_test && tracked {
        tracking::record_success(db, &ns::EMAIL, team_id).await;
    }
    Ok(())
}

/// `json_encode($emailSettings, JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES)` with the secrets masked.
/// Credentials never leave: anything with a secret column name becomes `********`.
fn email_settings_json(settings: &Settings) -> String {
    let mut row: serde_json::Map<String, Value> = settings
        .row
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    for hidden in ["smtp_from_address", "smtp_from_name", "smtp_recipients", "smtp_host", "smtp_port", "smtp_encryption", "smtp_username", "smtp_timeout", "smtp_ehlo_domain"] {
        row.remove(hidden);
    }
    for secret in ["smtp_password", "resend_api_key"] {
        row.insert(secret.to_owned(), json!("********"));
    }
    serde_json::to_string_pretty(&row).unwrap_or_default()
}

/// `send_internal_notification($message)`.
fn spawn_internal(db: &Db, encrypter: Option<&Encrypter>, message: String) {
    spawn_general(db, encrypter, 0, super::messages::general(message, true));
}

/// `CustomEmailNotification`: `$tries = 5`, `$backoff = [10, 20, 30, 40, 50]`.
async fn email_with_retries(
    db: &Db,
    encrypter: Option<&Encrypter>,
    team_id: i64,
    settings: &Settings,
    ctx: &Ctx,
    notification: &Notification,
) -> Result<(), String> {
    let commit_details = settings.flag("deployment_commit_details_email_notifications");
    let Some(mail) = notification.to_mail(ctx, commit_details) else {
        return Ok(());
    };
    let emails = match notification {
        Notification::Test { emails, .. } => emails.as_deref(),
        _ => None,
    };
    let mut last = Ok(());
    for delay in [0u64, 10, 20, 30, 40] {
        if delay > 0 {
            tokio::time::sleep(Duration::from_secs(delay)).await;
        }
        last = email_channel(
            db,
            encrypter,
            team_id,
            settings,
            false,
            emails,
            notification.is_test(),
            &mail,
        )
        .await;
        if last.is_ok() {
            return Ok(());
        }
    }
    last
}

/// `new Test(channel: $channel)` delivered right away.
pub async fn send_test(
    db: &Db,
    encrypter: Option<&Encrypter>,
    team_id: i64,
    channel: &str,
    emails: Option<String>,
) -> Result<(), String> {
    let ctx = Ctx::load(db).await;
    let notification = test_notification(channel, emails.clone());
    let ch: &'static Channel = match channel {
        "email" => &ns::EMAIL,
        "discord" => &ns::DISCORD,
        "telegram" => &ns::TELEGRAM,
        "slack" => &ns::SLACK,
        "pushover" => &ns::PUSHOVER,
        "webhook" => &ns::WEBHOOK,
        _ => return Ok(()),
    };
    let row = ns::first_or_create(db, ch, team_id)
        .await
        .map_err(|e| e.to_string())?
        .0;
    let settings = Settings {
        row,
        encrypter: encrypter.cloned(),
    };
    match ch.name {
        "email" => {
            // `RateLimiter::attempt('test-email:'.$team->id, 0, ..., 10)`: the hit is recorded after
            // the callback, so a failed send leaves the key free.
            let limit_key = format!("test-email:{team_id}");
            if super::rate_limiter_taken(&limit_key) {
                return Err("Too many messages sent!".to_owned());
            }
            let mail = notification
                .to_mail(&ctx, false)
                .ok_or("No mail for this notification.")?;
            email_channel(
                db,
                encrypter,
                team_id,
                &settings,
                false,
                emails.as_deref(),
                true,
                &mail,
            )
            .await?;
            super::rate_limiter_hit(&limit_key, Duration::from_secs(10));
            Ok(())
        }
        "telegram" => {
            // `Livewire\Notifications\Telegram::sendTestNotification`.
            let message = notification.to_telegram(&ctx);
            let token = settings.secret("telegram_token")?.unwrap_or_default();
            let chat = settings.secret("telegram_chat_id")?.unwrap_or_default();
            let response = telegram_send(&ctx, &message, &token, &chat, None).await?;
            if (200..300).contains(&response.status) {
                return Ok(());
            }
            let reason = if matches!(response.status, 401 | 404) {
                "The Bot API token is not valid. Copy it again from @BotFather.".to_owned()
            } else {
                match response.json("description") {
                    Some(Value::String(s)) => s,
                    _ => format!("HTTP {}", response.status),
                }
            };
            Err(format!(
                "Telegram rejected the test notification. {}",
                super::php::e(&reason)
            ))
        }
        "discord" => {
            once(discord(
                db,
                encrypter,
                team_id,
                &settings,
                &ctx,
                &notification,
            ))
            .await
        }
        "slack" => {
            once(slack(
                db,
                encrypter,
                team_id,
                &settings,
                &ctx,
                &notification,
            ))
            .await
        }
        "pushover" => {
            once(pushover(
                db,
                encrypter,
                team_id,
                &settings,
                &ctx,
                &notification,
            ))
            .await
        }
        _ => {
            once(webhook(
                db,
                encrypter,
                team_id,
                &settings,
                &ctx,
                &notification,
            ))
            .await
        }
    }
}

async fn once(f: impl Future<Output = Result<(), String>>) -> Result<(), String> {
    f.await
}

/// Transactional mail: `TransactionalEmailChannel`, `send_user_an_email`, `ResetPassword`.
pub async fn send_transactional(
    db: &Db,
    encrypter: Option<&Encrypter>,
    to: &str,
    mail: &Mail,
) -> Result<(), String> {
    let ctx = Ctx::load(db).await;
    let settings = Settings {
        row: instance_settings::row(db)
            .await
            .map_err(|e| e.to_string())?,
        encrypter: encrypter.cloned(),
    };
    let message = mail.render(&ctx);
    let enabled = settings.flag("smtp_enabled") || settings.flag("resend_enabled");
    if !enabled {
        return match mail {
            // `TransactionalEmailChannel` and the `EmailChannel` of `Test` send nothing.
            Mail::EmailChangeVerification { .. } | Mail::InvitationLink { .. } | Mail::Test => {
                Ok(())
            }
            // `send_user_an_email` and `ResetPassword::via`.
            _ => Err("No email settings found.".to_owned()),
        };
    }
    let from = mail_from(&settings)?;
    let recipient = match mail {
        Mail::EmailChangeVerification { new_email, .. } => new_email.as_str(),
        _ => to,
    };
    // `send_user_an_email($mail, $email, 'feedback@coollabs.io')`.
    let cc = matches!(mail, Mail::Help { .. }).then(|| {
        std::env::var("COOLIFY_RS_FEEDBACK_EMAIL")
            .unwrap_or_else(|_| "feedback@coollabs.io".to_owned())
    });
    if settings.flag("resend_enabled") {
        let mut body = laravel_resend_body(&from, recipient, &message);
        if let Some(cc) = &cc {
            body["cc"] = json!([cc]);
            body["reply_to"] = json!([recipient]);
        }
        let api_key = settings.secret("resend_api_key")?.unwrap_or_default();
        return resend_send(&api_key, &body).await.map_err(|e| match e {
            ResendFailure::Api(e) => {
                format!("Request to Resend API failed. Reason: {}.", e.message)
            }
            ResendFailure::Transport => {
                "Request to Resend API failed. Reason: connection error.".to_owned()
            }
        });
    }
    smtp_send_with_cc(
        &settings,
        &from,
        &[recipient.to_owned()],
        cc.as_deref(),
        &message,
    )
    .await
    .map_err(|e| e.message)
}

/// `Illuminate\Mail\Transport\ResendTransport::doSend`.
fn laravel_resend_body(from: &(String, String), to: &str, m: &MailMessage) -> Value {
    json!({
        "from": formatted_from(&from.0, &from.1),
        "to": [to],
        "cc": [],
        "bcc": [],
        "reply_to": [],
        "headers": serde_json::Map::<String, Value>::new(),
        "subject": m.subject,
        "html": m.html,
        "text": Value::Null,
        "attachments": [],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slack_or_mattermost() {
        assert!(is_slack_webhook("https://hooks.slack.com/services/x"));
        assert!(!is_slack_webhook("http://hooks.slack.com/services/x"));
        assert!(!is_slack_webhook("https://mm.example/hooks/x"));
    }

    #[test]
    fn redacted_urls_hide_the_path() {
        assert_eq!(
            redacted_url("https://U:P@Discord.com:8443/api/webhooks/1/secret"),
            "https://discord.com:8443"
        );
        assert_eq!(redacted_url("nope"), "[invalid-url]");
    }

    #[test]
    fn from_formatting() {
        assert_eq!(formatted_from("a@b.c", "Coolify"), "\"Coolify\" <a@b.c>");
    }

    #[test]
    fn resend_messages() {
        let e = |status, name: Option<&str>| ResendError {
            status,
            name: name.map(str::to_owned),
            message: "m".into(),
        };
        assert!(resend_user_message(&e(401, None)).contains("restricted permissions"));
        assert!(resend_user_message(&e(401, Some("validation_error"))).contains("Invalid Resend"));
        assert!(
            resend_user_message(&e(403, Some("validation_error"))).starts_with("Email validation")
        );
        assert!(resend_user_message(&e(429, None)).contains("rate limit"));
        assert!(resend_user_message(&e(500, None)).ends_with(": m"));
    }
}
