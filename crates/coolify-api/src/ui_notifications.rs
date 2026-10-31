//! Page endpoints of the notification screens: `App\Livewire\Notifications\{Email, Discord, Slack,
//! Telegram, Pushover, Webhook}`. A Livewire action becomes one POST with the whole form, because
//! upstream's `syncData(true)` also saves whatever else the form holds.

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::from_fn_with_state;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use coolify_db::instance_settings;
use coolify_db::notification_settings::{self as ns, Channel};
use coolify_db::servers::ColumnValue;
use serde_json::{Map, Value, json};

use crate::audit::Meta;
use crate::auth::AuthCtx;
use crate::response::{ApiError, ApiResult, ok};
use crate::routes::notifications::{column_value, decrypted};
use crate::state::AppState;

pub fn routes(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/ui/notifications/{channel}", get(show).post(action))
        .route("/ui/notifications/{channel}/test", post(send_test))
        .route_layer(from_fn_with_state(state, crate::session::ui_guard))
}

fn channel(name: &str) -> Result<&'static Channel, ApiError> {
    ns::ALL
        .iter()
        .copied()
        .find(|c| c.name == name)
        .ok_or(ApiError::Message(StatusCode::NOT_FOUND, "Not found."))
}

/// `handleError()` for a Livewire action: the page shows the message as a toast.
pub(crate) fn toast_error(status: StatusCode, message: impl Into<String>) -> Response {
    crate::response::PhpJson(status, json!({"message": message.into()})).into_response()
}

/// `$this->authorize(...)` failing, as `handleError` shows it.
pub(crate) fn unauthorized() -> Response {
    toast_error(StatusCode::FORBIDDEN, "This action is unauthorized.")
}

/// `ValidationException::summarize`.
pub(crate) fn summarize(errors: &[String]) -> Option<String> {
    let first = errors.first()?;
    Some(match errors.len() - 1 {
        0 => first.clone(),
        1 => format!("{first} (and 1 more error)"),
        n => format!("{first} (and {n} more errors)"),
    })
}

pub(crate) fn validation_failed(errors: &[String]) -> Option<Response> {
    summarize(errors).map(|m| toast_error(StatusCode::UNPROCESSABLE_ENTITY, m))
}

/// Laravel's `isValidatable` treats null and blank strings as absent for non-implicit rules.
pub(crate) fn blank(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => true,
        Some(Value::String(s)) => s.trim().is_empty(),
        _ => false,
    }
}

/// PHP `is_numeric`, near enough for form input.
pub(crate) fn numeric(v: &Value) -> bool {
    match v {
        Value::Number(_) => true,
        Value::String(s) => s.trim().parse::<f64>().is_ok(),
        _ => false,
    }
}

fn number(v: &Value) -> f64 {
    match v {
        Value::Number(n) => n.as_f64().unwrap_or(0.0),
        Value::String(s) => s.trim().parse().unwrap_or(0.0),
        _ => 0.0,
    }
}

/// The form as upstream's component properties, with secrets the user may not see left out.
fn present(
    state: &AppState,
    ctx: &AuthCtx,
    ch: &Channel,
    row: &Map<String, Value>,
) -> Result<Value, ApiError> {
    let can_update = ctx.is_team_admin();
    let mut out = Map::new();
    for (k, v) in row {
        if !ch.fillable.contains(&k.as_str()) {
            continue;
        }
        let secret = matches!(
            k.as_str(),
            "smtp_password"
                | "resend_api_key"
                | "discord_webhook_url"
                | "slack_webhook_url"
                | "webhook_url"
                | "pushover_user_key"
                | "pushover_api_token"
                | "telegram_token"
                | "telegram_chat_id"
        ) || k.ends_with("_thread_id");
        if secret && !can_update {
            out.insert(k.clone(), Value::Null);
            continue;
        }
        out.insert(
            k.clone(),
            if ch.encrypted.contains(&k.as_str()) {
                decrypted(state, ch, row, k)?
            } else {
                v.clone()
            },
        );
    }
    let is_enabled = |k: &str| row.get(k).and_then(Value::as_bool).unwrap_or(false);
    let enabled = match ch.name {
        "email" => {
            is_enabled("smtp_enabled")
                || is_enabled("resend_enabled")
                || is_enabled("use_instance_email_settings")
        }
        n => is_enabled(&format!("{n}_enabled")),
    };
    Ok(json!({
        "settings": Value::Object(out),
        "enabled": enabled,
        "can_update": can_update,
    }))
}

async fn show(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(name): Path<String>,
) -> ApiResult {
    let ch = channel(&name)?;
    let (row, _) = ns::first_or_create(&state.db, ch, ctx.team_id).await?;
    let mut res = present(&state, &ctx, ch, &row)?;
    if ch.name == "email" {
        let instance_admin = instance_settings::is_instance_admin(&state.db, ctx.user_id).await?;
        let email: Option<String> = sqlx::query_scalar("SELECT email FROM users WHERE id = $1")
            .bind(ctx.user_id)
            .fetch_optional(state.db.pool())
            .await
            .map_err(|_| ApiError::Internal)?;
        res["instance_admin"] = json!(instance_admin);
        res["email"] = json!(email);
    }
    Ok(ok(res).into_response())
}

/// `$this->settings->...; $changedFields = array_keys($this->settings->getDirty()); save();
/// auditLog(...)`: only changed columns are written, in attribute order.
async fn save_columns(
    state: &AppState,
    ctx: &AuthCtx,
    meta: &Meta,
    ch: &'static Channel,
    values: &Map<String, Value>,
    columns: &[&str],
) -> Result<(), ApiError> {
    let (row, _) = ns::first_or_create(&state.db, ch, ctx.team_id).await?;
    let mut dirty: Vec<&'static str> = Vec::new();
    let mut changes: Vec<(&'static str, ColumnValue)> = Vec::new();
    for key in row.keys() {
        let Some(col) = ch.fillable.iter().copied().find(|f| f == key) else {
            continue;
        };
        if !columns.contains(&col) {
            continue;
        }
        let Some(new) = values.get(col) else {
            continue;
        };
        let current = if ch.encrypted.contains(&col) {
            decrypted(state, ch, &row, col)?
        } else {
            row.get(col).cloned().unwrap_or(Value::Null)
        };
        let integer = matches!(col, "smtp_port" | "smtp_timeout");
        let new = if integer && blank(Some(new)) {
            &Value::Null
        } else {
            new
        };
        let same = if integer && !new.is_null() && !current.is_null() {
            number(new) == number(&current)
        } else {
            *new == current
        };
        if same {
            continue;
        }
        dirty.push(col);
        changes.push((col, column_value(state, ch, col, new)?));
    }
    if changes.is_empty() {
        return Ok(());
    }
    ns::update(&state.db, ch, ctx.team_id, &changes).await?;
    let changed: Vec<&str> = dirty
        .into_iter()
        .filter(|c| ch.name != "email" || !matches!(*c, "smtp_password" | "resend_api_key"))
        .collect();
    crate::audit::log(
        state,
        ctx,
        meta,
        &format!("ui.notifications.{}.updated", ch.name),
        json!({"team_id": ctx.team_id, "changed_fields": changed}),
        "info",
    )
    .await;
    Ok(())
}

fn attr(col: &str) -> String {
    col.replace('_', " ")
}

fn is_true(values: &Map<String, Value>, col: &str) -> bool {
    values.get(col).and_then(Value::as_bool).unwrap_or(false)
}

fn required_if(errors: &mut Vec<String>, values: &Map<String, Value>, col: &str, when: &str) {
    if is_true(values, when) && blank(values.get(col)) {
        errors.push(format!(
            "The {} field is required when {} is true.",
            attr(col),
            attr(when)
        ));
    }
}

/// The `#[Validate]` attributes of the component, in property order.
async fn component_rules(
    state: &AppState,
    ch: &Channel,
    values: &Map<String, Value>,
    headers: &HeaderMap,
) -> Result<Vec<String>, ApiError> {
    let mut errors = Vec::new();
    match ch.name {
        "email" => {
            if !blank(values.get("smtp_from_address"))
                && let Some(m) = crate::rules::email_rule(&values["smtp_from_address"])
            {
                errors.push(m.replace(":attribute", "smtp from address"));
            }
            if let Some(v) = values.get("smtp_port").filter(|v| !blank(Some(v))) {
                if !numeric(v) {
                    errors.push("The smtp port field must be a number.".to_owned());
                } else {
                    if number(v) < 1.0 {
                        errors.push("The smtp port field must be at least 1.".to_owned());
                    }
                    if number(v) > 65535.0 {
                        errors
                            .push("The smtp port field must not be greater than 65535.".to_owned());
                    }
                }
            }
            if let Some(v) = values.get("smtp_encryption").filter(|v| !blank(Some(v)))
                && !matches!(v.as_str(), Some("starttls" | "tls" | "none"))
            {
                errors.push("The selected smtp encryption is invalid.".to_owned());
            }
            if let Some(v) = values.get("smtp_timeout").filter(|v| !blank(Some(v)))
                && !numeric(v)
            {
                errors.push("The smtp timeout field must be a number.".to_owned());
            }
        }
        "discord" | "slack" | "webhook" => {
            let col = match ch.name {
                "discord" => "discord_webhook_url",
                "slack" => "slack_webhook_url",
                _ => "webhook_url",
            };
            required_if(&mut errors, values, col, &format!("{}_enabled", ch.name));
            if !blank(values.get(col))
                && let Some(url) = values.get(col).and_then(Value::as_str)
                && let Some(m) = crate::safe_url::check(state, col, url, headers).await?
            {
                // The private-target message names the raw attribute: the Livewire property.
                let camel = col
                    .split('_')
                    .enumerate()
                    .map(|(i, w)| {
                        if i == 0 {
                            w.to_owned()
                        } else {
                            let mut c = w.chars();
                            c.next().map_or(String::new(), |f| {
                                f.to_uppercase().collect::<String>() + c.as_str()
                            })
                        }
                    })
                    .collect::<String>();
                errors.push(m.replace(col, &camel));
            }
        }
        "telegram" => {
            required_if(&mut errors, values, "telegram_token", "telegram_enabled");
            required_if(&mut errors, values, "telegram_chat_id", "telegram_enabled");
        }
        _ => {
            required_if(&mut errors, values, "pushover_user_key", "pushover_enabled");
            required_if(
                &mut errors,
                values,
                "pushover_api_token",
                "pushover_enabled",
            );
        }
    }
    Ok(errors)
}

/// `saveModel()`: authorize, `syncData(true)`.
async fn save_model(
    state: &AppState,
    ctx: &AuthCtx,
    meta: &Meta,
    ch: &'static Channel,
    values: &Map<String, Value>,
    headers: &HeaderMap,
) -> Result<Option<Response>, ApiError> {
    if !ctx.is_team_admin() {
        return Ok(Some(unauthorized()));
    }
    let errors = component_rules(state, ch, values, headers).await?;
    if let Some(r) = validation_failed(&errors) {
        return Ok(Some(r));
    }
    if ch.name == "email"
        && !blank(values.get("smtp_ehlo_domain"))
        && let Some(m) = crate::rules::valid_hostname(&values["smtp_ehlo_domain"])
    {
        return Ok(Some(toast_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            m.replace(":attribute", "smtp ehlo domain"),
        )));
    }
    save_columns(state, ctx, meta, ch, values, ch.fillable).await?;
    Ok(None)
}

/// `instantSave<Channel>Enabled`: the switch needs the credentials, even when turning off.
fn toggle_rules(ch: &Channel, values: &Map<String, Value>) -> Vec<String> {
    let fields: &[(&str, &str)] = match ch.name {
        "discord" => &[("discord_webhook_url", "Discord Webhook URL is required.")],
        "slack" => &[("slack_webhook_url", "Slack Webhook URL is required.")],
        "webhook" => &[("webhook_url", "Webhook URL is required.")],
        "telegram" => &[
            ("telegram_token", "Telegram Token is required."),
            ("telegram_chat_id", "Telegram Chat ID is required."),
        ],
        "pushover" => &[
            ("pushover_user_key", "Pushover User Key is required."),
            ("pushover_api_token", "Pushover API Token is required."),
        ],
        _ => &[],
    };
    fields
        .iter()
        .filter(|(c, _)| blank(values.get(*c)))
        .map(|(_, m)| (*m).to_owned())
        .collect()
}

/// `validateSmtpSettings`.
fn smtp_rules(values: &Map<String, Value>) -> Vec<String> {
    let mut e = from_rules(values);
    if blank(values.get("smtp_host")) {
        e.push("SMTP Host is required.".to_owned());
    }
    match values.get("smtp_port").filter(|v| !blank(Some(v))) {
        None => e.push("SMTP Port is required.".to_owned()),
        Some(v) if !numeric(v) => e.push("SMTP Port must be a number.".to_owned()),
        _ => {}
    }
    match values.get("smtp_encryption").filter(|v| !blank(Some(v))) {
        None => e.push("Encryption type is required.".to_owned()),
        Some(v) if !matches!(v.as_str(), Some("starttls" | "tls" | "none")) => {
            e.push("The selected smtp encryption is invalid.".to_owned());
        }
        _ => {}
    }
    if let Some(v) = values.get("smtp_timeout").filter(|v| !blank(Some(v)))
        && !numeric(v)
    {
        e.push("The smtp timeout field must be a number.".to_owned());
    }
    if !blank(values.get("smtp_ehlo_domain"))
        && let Some(m) = crate::rules::valid_hostname(&values["smtp_ehlo_domain"])
    {
        e.push(m.replace(":attribute", "smtp ehlo domain"));
    }
    e
}

fn from_rules(values: &Map<String, Value>) -> Vec<String> {
    let mut e = Vec::new();
    if blank(values.get("smtp_from_address")) {
        e.push("From Address is required.".to_owned());
    } else if crate::rules::email_rule(&values["smtp_from_address"]).is_some() {
        e.push("Please enter a valid email address.".to_owned());
    }
    if blank(values.get("smtp_from_name")) {
        e.push("From Name is required.".to_owned());
    }
    e
}

/// `submitSmtp`.
const SMTP_COLUMNS: &[&str] = &[
    "smtp_enabled",
    "smtp_from_address",
    "smtp_from_name",
    "smtp_host",
    "smtp_port",
    "smtp_encryption",
    "smtp_username",
    "smtp_password",
    "smtp_timeout",
    "smtp_ehlo_domain",
];

/// `submitResend`.
const RESEND_COLUMNS: &[&str] = &[
    "resend_enabled",
    "resend_api_key",
    "smtp_from_address",
    "smtp_from_name",
];

/// One Livewire action. `action` names the method; `values` is the form.
async fn action(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    headers: HeaderMap,
    Path(name): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult {
    let ch = channel(&name)?;
    let act = body.get("action").and_then(Value::as_str).unwrap_or("save");
    let mut values = body
        .get("values")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    // Livewire drops null properties when it hydrates a request, so the class default of
    // `public ?string $smtpEncryption = 'starttls'` comes back in every action after the mount.
    if ch.name == "email" && values.get("smtp_encryption").is_none_or(Value::is_null) {
        values.insert("smtp_encryption".into(), json!("starttls"));
    }
    let mut message = if ch.name == "email" {
        "Email notifications settings updated."
    } else {
        "Settings saved."
    };
    match (ch.name, act) {
        ("email", "instant") => {
            // `instantSave()` with no type turns both senders off.
            values.insert("smtp_enabled".into(), json!(false));
            values.insert("resend_enabled".into(), json!(false));
            if let Some(r) = save_model(&state, &ctx, &meta, ch, &values, &headers).await? {
                return Ok(r);
            }
        }
        ("email", "smtp") => {
            if !ctx.is_team_admin() {
                return Ok(unauthorized());
            }
            if let Some(r) = validation_failed(&smtp_rules(&values)) {
                return Ok(r);
            }
            let mut cols = SMTP_COLUMNS.to_vec();
            if is_true(&values, "smtp_enabled") {
                values.insert("resend_enabled".into(), json!(false));
                cols.push("resend_enabled");
            }
            save_columns(&state, &ctx, &meta, ch, &values, &cols).await?;
            message = "SMTP settings updated.";
        }
        ("email", "resend") => {
            if !ctx.is_team_admin() {
                return Ok(unauthorized());
            }
            let mut errors = Vec::new();
            if is_true(&values, "resend_enabled") && blank(values.get("resend_api_key")) {
                errors.push("Resend API Key is required.".to_owned());
            }
            errors.extend(from_rules(&values));
            if let Some(r) = validation_failed(&errors) {
                return Ok(r);
            }
            let mut cols = RESEND_COLUMNS.to_vec();
            if is_true(&values, "resend_enabled") {
                values.insert("smtp_enabled".into(), json!(false));
                cols.push("smtp_enabled");
            }
            save_columns(&state, &ctx, &meta, ch, &values, &cols).await?;
            message = "Resend settings updated.";
        }
        ("email", "copy") => {
            // `copyFromInstanceSettings`.
            if !ctx.is_team_admin()
                || !instance_settings::is_instance_admin(&state.db, ctx.user_id).await?
            {
                return Ok(unauthorized());
            }
            let row = instance_settings::row(&state.db).await?;
            let plain = |k: &str| -> Result<Value, ApiError> {
                let v = row.get(k).cloned().unwrap_or(Value::Null);
                if instance_settings::SECRET_COLUMNS.contains(&k)
                    && let Some(c) = v.as_str()
                {
                    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
                    return enc
                        .decrypt_string(c)
                        .map(Value::String)
                        .map_err(|_| ApiError::Internal);
                }
                Ok(v)
            };
            let smtp_on = plain("smtp_enabled")?.as_bool().unwrap_or(false);
            let resend_on = plain("resend_enabled")?.as_bool().unwrap_or(false);
            for k in [
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
                "resend_api_key",
            ] {
                values.insert(k.into(), plain(k)?);
            }
            if smtp_on {
                values.insert("smtp_enabled".into(), json!(true));
                values.insert("resend_enabled".into(), json!(false));
            }
            if resend_on {
                values.insert("resend_enabled".into(), json!(true));
                values.insert("smtp_enabled".into(), json!(false));
            }
            if let Some(r) = save_model(&state, &ctx, &meta, ch, &values, &headers).await? {
                return Ok(r);
            }
        }
        (_, "toggle") => {
            if let Some(r) = validation_failed(&toggle_rules(ch, &values)) {
                return Ok(r);
            }
            if let Some(r) = save_model(&state, &ctx, &meta, ch, &values, &headers).await? {
                return Ok(r);
            }
        }
        _ => {
            if let Some(r) = save_model(&state, &ctx, &meta, ch, &values, &headers).await? {
                return Ok(r);
            }
        }
    }
    let row = ns::find(&state.db, ch, ctx.team_id)
        .await?
        .ok_or(ApiError::Internal)?;
    let mut res = present(&state, &ctx, ch, &row)?;
    res["message"] = json!(message);
    Ok(ok(res).into_response())
}

/// `sendTestNotification` and `sendTestEmail`.
async fn send_test(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(name): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult {
    let ch = channel(&name)?;
    if !ctx.is_team_admin() {
        return Ok(unauthorized());
    }
    let team = ctx.team_id;
    match ch.name {
        "email" => {
            let to = body.get("email").filter(|v| !blank(Some(v)));
            let Some(to) = to else {
                return Ok(toast_error(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "Test email address is required.",
                ));
            };
            if crate::rules::email_rule(to).is_some() {
                return Ok(toast_error(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "Please enter a valid email address.",
                ));
            }
            let to = to.as_str().unwrap_or_default().to_owned();
            if let Err(e) = coolify_jobs::notifications::notify_test(
                &state.db,
                state.encrypter.as_ref(),
                team,
                "email",
                Some(to),
            )
            .await
            {
                return Ok(toast_error(StatusCode::UNPROCESSABLE_ENTITY, e));
            }
            Ok(ok(json!({"message": "Test Email sent."})).into_response())
        }
        "telegram" => {
            // Sent synchronously so that the user sees the error from Telegram.
            if let Err(e) = coolify_jobs::notifications::notify_test(
                &state.db,
                state.encrypter.as_ref(),
                team,
                "telegram",
                None,
            )
            .await
            {
                const TITLE: &str = "Telegram rejected the test notification.";
                let body = match e.strip_prefix(TITLE) {
                    Some(reason) => json!({"message": TITLE, "details": reason.trim()}),
                    None => json!({"message": e}),
                };
                return Ok(
                    crate::response::PhpJson(StatusCode::UNPROCESSABLE_ENTITY, body)
                        .into_response(),
                );
            }
            Ok(ok(json!({"message": "Test notification sent."})).into_response())
        }
        n => {
            // `$this->team->notify(new Test(channel: ...))` is queued: the page says it was
            // sent, and a failure only reaches the log.
            let db = state.db.clone();
            let enc = state.encrypter.clone();
            let n = n.to_owned();
            tokio::spawn(async move {
                if let Err(e) =
                    coolify_jobs::notifications::notify_test(&db, enc.as_ref(), team, &n, None)
                        .await
                {
                    tracing::warn!(channel = %n, error = %e, "test notification failed");
                }
            });
            Ok(ok(json!({"message": "Test notification sent."})).into_response())
        }
    }
}
