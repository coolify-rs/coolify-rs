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
use coolify_jobs::notifications::email_settings as es;
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
        // `filled(enabled_email_provider($this)) || $this->use_instance_email_settings`.
        "email" => {
            es::enabled_email_provider_of(row).is_some()
                || is_enabled("use_instance_email_settings")
        }
        n => is_enabled(&format!("{n}_enabled")),
    };
    let mut res = json!({
        "settings": Value::Object(out),
        "enabled": enabled,
        "can_update": can_update,
    });
    if ch.name == "email" {
        // `ManagesSmtpServers`: the name of the server in use is a form field; the saved servers
        // are only listed by id and name, their settings never leave the server.
        let model = email_model(state, ch, row)?;
        res["settings"][ns::SMTP_SERVER_NAME] = row
            .get(ns::SMTP_SERVER_NAME)
            .cloned()
            .unwrap_or(Value::Null);
        // `activeEmailProvider()`.
        res["active_provider"] = json!(es::enabled_email_provider_of(row).unwrap_or("none"));
        res["smtp_server_name"] = json!(es::smtp_server_name(&model));
        res["smtp_server_options"] = es::smtp_server_options(&model);
        res["smtp_presets"] = es::smtp_presets_json();
    }
    Ok(res)
}

/// The attributes `HasSmtpServers` works on: the name, the saved servers and the columns of the
/// server in use, decrypted.
fn email_model(
    state: &AppState,
    ch: &Channel,
    row: &Map<String, Value>,
) -> Result<Map<String, Value>, ApiError> {
    let mut model = Map::new();
    model.insert(
        ns::SMTP_SERVER_NAME.to_owned(),
        row.get(ns::SMTP_SERVER_NAME)
            .cloned()
            .unwrap_or(Value::Null),
    );
    model.insert(
        ns::SMTP_SERVERS.to_owned(),
        decrypted(state, ch, row, ns::SMTP_SERVERS)?,
    );
    for column in es::SMTP_SERVER_COLUMNS {
        model.insert(column.to_owned(), decrypted(state, ch, row, column)?);
    }
    Ok(model)
}

/// The columns of `HasSmtpServers` that are not `$fillable`.
const SERVER_EXTRAS: [&str; 2] = [ns::SMTP_SERVER_NAME, ns::SMTP_SERVERS];

/// What `switchToSmtpServer`, `deleteActiveSmtpServer` and a rename can change.
const SERVER_COLUMNS: [&str; 11] = [
    ns::SMTP_SERVER_NAME,
    ns::SMTP_SERVERS,
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

/// `Str::uuid()`.
fn uuid4() -> String {
    let mut b: [u8; 16] = rand::random();
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: Vec<String> = b.iter().map(|x| format!("{x:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        h[..4].concat(),
        h[4..6].concat(),
        h[6..8].concat(),
        h[8..10].concat(),
        h[10..].concat()
    )
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
    let extras: &[&'static str] = if ch.name == "email" {
        &SERVER_EXTRAS
    } else {
        &[]
    };
    for key in row.keys() {
        let Some(col) = ch.fillable.iter().chain(extras).copied().find(|f| f == key) else {
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
        if col == ns::SMTP_SERVERS {
            // The `encrypted:array` cast: `encrypt(json_encode($value))`.
            let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
            let cipher = match new {
                Value::Null => None,
                list => Some(enc.encrypt_string(&coolify_compat::php_json::encode(list))),
            };
            changes.push((col, ColumnValue::Text(cipher)));
            continue;
        }
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
            if !blank(values.get("resend_from_address"))
                && let Some(m) = crate::rules::email_rule(&values["resend_from_address"])
            {
                errors.push(m.replace(":attribute", "resend from address"));
            }
            // `#[Validate(['required', 'in:none,smtp,resend'])] public string $emailProvider`.
            // The property always has a value upstream; a request without it is not checked.
            match values.get("email_provider") {
                Some(v) if blank(Some(v)) => {
                    errors.push("The email provider field is required.".to_owned());
                }
                Some(v) if !matches!(v.as_str(), Some("none" | "smtp" | "resend")) => {
                    errors.push("The selected email provider is invalid.".to_owned());
                }
                _ => {}
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
            // `#[Validate(['nullable', 'string', 'max:255'])]` of the image vulnerabilities topic.
            let thread = "telegram_notifications_image_vulnerabilities_thread_id";
            if values
                .get(thread)
                .and_then(Value::as_str)
                .is_some_and(|t| t.chars().count() > 255)
            {
                errors.push(format!(
                    "The {} field must not be greater than 255 characters.",
                    attr(thread)
                ));
            }
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
    if ch.name != "email" {
        save_columns(state, ctx, meta, ch, values, ch.fillable).await?;
        return Ok(None);
    }
    let (row, _) = ns::first_or_create(&state.db, ch, ctx.team_id).await?;
    // `if ($this->smtpHost !== $this->settings->smtp_host)`: a stored host is checked again at
    // send time, so an unchanged host must not block saving other fields.
    if let Some(m) = changed_host_error(state, ctx, headers, ch, &row, values).await? {
        return Ok(Some(toast_error(StatusCode::UNPROCESSABLE_ENTITY, m)));
    }
    // `fillSmtpServerSettings()`: a new server first keeps the server in use in the saved list.
    let mut values = values.clone();
    values.remove(ns::SMTP_SERVERS);
    if is_true(&values, "is_new_smtp_server") {
        let mut model = email_model(state, ch, &row)?;
        es::SmtpServers(&mut model).keep_active(&uuid4());
        values.insert(
            ns::SMTP_SERVERS.to_owned(),
            model.remove(ns::SMTP_SERVERS).unwrap_or(Value::Null),
        );
    }
    let mut columns: Vec<&str> = ch.fillable.to_vec();
    columns.extend(SERVER_EXTRAS);
    save_columns(state, ctx, meta, ch, &values, &columns).await?;
    Ok(None)
}

/// `new SafeSmtpHost` for a host that differs from the stored one. A blank host passes.
async fn changed_host_error(
    state: &AppState,
    ctx: &AuthCtx,
    headers: &HeaderMap,
    ch: &Channel,
    row: &Map<String, Value>,
    values: &Map<String, Value>,
) -> Result<Option<String>, ApiError> {
    let new = values.get("smtp_host").cloned().unwrap_or(Value::Null);
    if new == decrypted(state, ch, row, "smtp_host")? {
        return Ok(None);
    }
    let Some(host) = new.as_str().filter(|h| !h.trim().is_empty()) else {
        return Ok(None);
    };
    let is_admin = instance_settings::is_instance_admin(&state.db, ctx.user_id).await?;
    Ok(es::smtp_host_error(
        &state.db,
        host,
        is_admin,
        &crate::safe_url::settings_url(headers),
    )
    .await)
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

/// `validateSmtpSettings`, in rule order. The `SafeSmtpHost` rule of a changed host is checked
/// by the caller ([`changed_host_error`]) right after the `required` rule of the host.
fn smtp_rules(values: &Map<String, Value>, host_error: Option<String>) -> Vec<String> {
    let mut e = Vec::new();
    if let Some(name) = values.get(ns::SMTP_SERVER_NAME).and_then(Value::as_str)
        && name.chars().count() > 255
    {
        e.push("The smtp server name field must not be greater than 255 characters.".to_owned());
    }
    e.extend(from_rules(values, "smtp"));
    if blank(values.get("smtp_host")) {
        e.push("SMTP Host is required.".to_owned());
    } else if let Some(m) = host_error {
        e.push(m);
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
    // `Rule::requiredIf(SmtpPresets::requiresLogin($this->smtpHost))`, then `required_with`.
    let needs_login = es::smtp_requires_login(values.get("smtp_host").and_then(Value::as_str));
    if needs_login && blank(values.get("smtp_username")) {
        e.push("This provider needs a username.".to_owned());
    }
    if blank(values.get("smtp_password")) {
        if needs_login {
            e.push("This provider needs a password.".to_owned());
        } else if !blank(values.get("smtp_username")) {
            e.push("Enter the password for this username.".to_owned());
        }
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

/// The sender rules of a provider (`smtp` or `resend`): `{provider}_from_address` and
/// `{provider}_from_name`.
fn from_rules(values: &Map<String, Value>, provider: &str) -> Vec<String> {
    let mut e = Vec::new();
    let address = format!("{provider}_from_address");
    if blank(values.get(&address)) {
        e.push("From Address is required.".to_owned());
    } else if crate::rules::email_rule(&values[&address]).is_some() {
        e.push("Please enter a valid email address.".to_owned());
    }
    if blank(values.get(&format!("{provider}_from_name"))) {
        e.push("From Name is required.".to_owned());
    }
    e
}

/// `validateResendSettings` with Resend turned on.
fn resend_rules(values: &Map<String, Value>) -> Vec<String> {
    let mut e = Vec::new();
    if blank(values.get("resend_api_key")) {
        e.push("Resend API Key is required.".to_owned());
    }
    e.extend(from_rules(values, "resend"));
    e
}

/// `enableSelectedEmailProvider()`: turns on the provider selected in the provider list and
/// returns the errors of `validateEnabledEmailProvider()`. When they are not empty nothing is
/// saved, so the saved provider stays on.
async fn enable_selected_provider(
    state: &AppState,
    ctx: &AuthCtx,
    headers: &HeaderMap,
    ch: &'static Channel,
    values: &mut Map<String, Value>,
) -> Result<Vec<String>, ApiError> {
    let provider = values
        .get("email_provider")
        .and_then(Value::as_str)
        .unwrap_or("none")
        .to_owned();
    values.insert("smtp_enabled".into(), json!(provider == "smtp"));
    values.insert("resend_enabled".into(), json!(provider == "resend"));
    Ok(match provider.as_str() {
        "smtp" => {
            let (row, _) = ns::first_or_create(&state.db, ch, ctx.team_id).await?;
            let host_error = changed_host_error(state, ctx, headers, ch, &row, values).await?;
            smtp_rules(values, host_error)
        }
        "resend" => resend_rules(values),
        _ => Vec::new(),
    })
}

/// `saveEmailSettings()` after `switchToSmtpServer`, `deleteActiveSmtpServer` or a rename: only
/// the model changes of the action are saved, not the form.
async fn save_server_model(
    state: &AppState,
    ctx: &AuthCtx,
    meta: &Meta,
    ch: &'static Channel,
    model: &Map<String, Value>,
) -> Result<(), ApiError> {
    save_columns(state, ctx, meta, ch, model, &SERVER_COLUMNS).await
}

/// `'required|string|max:255'` with `'Server name is required.'`.
fn server_name_error(name: Option<&Value>, attribute: &str) -> Option<String> {
    if blank(name) {
        return Some("Server name is required.".to_owned());
    }
    match name.and_then(Value::as_str) {
        Some(s) if s.chars().count() > 255 => Some(format!(
            "The {attribute} field must not be greater than 255 characters."
        )),
        Some(_) => None,
        None => Some(format!("The {attribute} field must be a string.")),
    }
}

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
    // The messages of `switchSmtpServer` and `deleteSmtpServer` name a server.
    let mut switched: Option<String> = None;
    // The saved servers are never taken from the request.
    values.remove(ns::SMTP_SERVERS);
    // Set by `enableSelectedEmailProvider` when the selected provider is not complete yet.
    let mut pending = false;
    // `addSmtpServer`: the fields of the new server, for the form.
    let mut new_server: Option<Value> = None;
    match (ch.name, act) {
        ("email", "instant") => {
            // `instantSave()` with no type turns both senders off.
            values.insert("smtp_enabled".into(), json!(false));
            values.insert("resend_enabled".into(), json!(false));
            if let Some(r) = save_model(&state, &ctx, &meta, ch, &values, &headers).await? {
                return Ok(r);
            }
        }
        ("email", "submit" | "provider") => {
            // `submit()`; `selectEmailProvider()` is the same after a provider that validates.
            let errors = if act == "provider" || !is_true(&values, "use_instance_email_settings") {
                enable_selected_provider(&state, &ctx, &headers, ch, &mut values).await?
            } else {
                Vec::new()
            };
            if !errors.is_empty() && act == "provider" {
                // Not complete yet: show the fields of the provider and the "Not active yet"
                // notice, without an error. Nothing is saved.
                pending = true;
                message = "";
            } else if let Some(r) = validation_failed(&errors) {
                return Ok(r);
            } else if let Some(r) = save_model(&state, &ctx, &meta, ch, &values, &headers).await? {
                return Ok(r);
            }
        }
        ("email", "addServer") => {
            // `addSmtpServer()`: starts a new server with the fields of the selected template or
            // empty fields. Nothing is saved until the form is.
            if !ctx.is_team_admin() {
                return Ok(unauthorized());
            }
            let mut errors = Vec::new();
            errors.extend(server_name_error(body.get("name"), "new smtp server name"));
            let template = body.get("template").filter(|t| !blank(Some(t)));
            let preset = template.and_then(Value::as_str).and_then(es::smtp_preset);
            if template.is_some() && preset.is_none() {
                errors.push("The selected new smtp server template is invalid.".to_owned());
            }
            if let Some(r) = validation_failed(&errors) {
                return Ok(r);
            }
            new_server = Some(json!({
                "smtp_server_name": body.get("name"),
                "smtp_host": preset.map(|p| p.host),
                "smtp_port": preset.map(|p| p.port.to_string()),
                "smtp_encryption": preset.map_or("starttls", |p| p.encryption),
                "smtp_username": preset.and_then(|p| p.username),
                "smtp_password": null,
                "smtp_timeout": null,
                "smtp_ehlo_domain": null,
            }));
            message = "";
        }
        ("email", "switchServer") => {
            // `switchSmtpServer($id)`.
            if !ctx.is_team_admin() {
                return Ok(unauthorized());
            }
            let id = body.get("id").and_then(Value::as_str).unwrap_or("active");
            message = "";
            if id != "active" {
                let (row, _) = ns::first_or_create(&state.db, ch, ctx.team_id).await?;
                let mut model = email_model(&state, ch, &row)?;
                if !es::SmtpServers(&mut model).switch_to(id, &uuid4()) {
                    return Ok(toast_error(
                        StatusCode::UNPROCESSABLE_ENTITY,
                        "This SMTP server does not exist anymore. Reload the page.",
                    ));
                }
                save_server_model(&state, &ctx, &meta, ch, &model).await?;
                switched = Some(format!(
                    "Switched to SMTP server {}.",
                    es::smtp_server_name(&model)
                ));
            }
        }
        ("email", "renameServer") => {
            // `renameSmtpServer()`: only the name is saved; other unsaved changes on the page
            // stay unsaved.
            if !ctx.is_team_admin() {
                return Ok(unauthorized());
            }
            if let Some(m) = server_name_error(body.get("name"), "renamed smtp server name") {
                return Ok(toast_error(StatusCode::UNPROCESSABLE_ENTITY, m));
            }
            let (row, _) = ns::first_or_create(&state.db, ch, ctx.team_id).await?;
            let mut model = email_model(&state, ch, &row)?;
            model.insert(
                ns::SMTP_SERVER_NAME.to_owned(),
                body.get("name").cloned().unwrap_or(Value::Null),
            );
            save_server_model(&state, &ctx, &meta, ch, &model).await?;
            message = "SMTP server renamed.";
        }
        ("email", "deleteServer") => {
            // `deleteSmtpServer()`: deletes the server in use and uses the next saved server.
            if !ctx.is_team_admin() {
                return Ok(unauthorized());
            }
            let (row, _) = ns::first_or_create(&state.db, ch, ctx.team_id).await?;
            let mut model = email_model(&state, ch, &row)?;
            let name = es::smtp_server_name(&model);
            if !es::SmtpServers(&mut model).delete_active() {
                return Ok(toast_error(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "This is the only SMTP server. To stop using SMTP, select another provider.",
                ));
            }
            save_server_model(&state, &ctx, &meta, ch, &model).await?;
            message = "";
            switched = Some(format!(
                "SMTP server {name} deleted. Now using {}.",
                es::smtp_server_name(&model)
            ));
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
    res["message"] = json!(switched.as_deref().unwrap_or(message));
    if pending {
        res["pending"] = json!(true);
    }
    if let Some(server) = new_server {
        res["new_server"] = server;
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn form(pairs: Value) -> Map<String, Value> {
        pairs.as_object().unwrap().clone()
    }

    #[test]
    fn smtp_settings_need_a_login_for_a_known_provider() {
        let mut v = form(json!({
            "smtp_from_address": "a@example.com",
            "smtp_from_name": "A",
            "smtp_host": "smtp.gmail.com",
            "smtp_port": "587",
            "smtp_encryption": "starttls",
        }));
        assert_eq!(
            smtp_rules(&v, None),
            [
                "This provider needs a username.",
                "This provider needs a password."
            ]
        );
        // A relay that needs no login.
        v.insert("smtp_host".into(), json!("mail.internal.example"));
        assert!(smtp_rules(&v, None).is_empty());
        // A username needs its password.
        v.insert("smtp_username".into(), json!("relay"));
        assert_eq!(
            smtp_rules(&v, None),
            ["Enter the password for this username."]
        );
        v.insert("smtp_password".into(), json!("secret"));
        assert!(smtp_rules(&v, None).is_empty());
    }

    #[test]
    fn smtp_settings_report_the_host_rule_after_the_sender() {
        let v = form(json!({
            "smtp_host": "10.0.0.5",
            "smtp_port": "abc",
            "smtp_encryption": "ssl",
        }));
        assert_eq!(
            smtp_rules(&v, Some("The SMTP host could not be resolved.".to_owned())),
            [
                "From Address is required.",
                "From Name is required.",
                "The SMTP host could not be resolved.",
                "SMTP Port must be a number.",
                "The selected smtp encryption is invalid.",
            ]
        );
        // The `required` rule of the host comes before the host check.
        assert_eq!(
            smtp_rules(&form(json!({})), Some("unused".to_owned()))[2],
            "SMTP Host is required."
        );
    }

    #[test]
    fn resend_settings_have_their_own_sender() {
        assert_eq!(
            resend_rules(&form(
                json!({"smtp_from_address": "a@example.com", "smtp_from_name": "A"})
            )),
            [
                "Resend API Key is required.",
                "From Address is required.",
                "From Name is required.",
            ]
        );
        assert_eq!(
            resend_rules(&form(json!({
                "resend_api_key": "re_123",
                "resend_from_address": "nope",
                "resend_from_name": "A",
            }))),
            ["Please enter a valid email address."]
        );
    }

    #[test]
    fn server_names_are_required() {
        assert_eq!(
            server_name_error(None, "new smtp server name").as_deref(),
            Some("Server name is required.")
        );
        assert_eq!(
            server_name_error(Some(&json!("  ")), "new smtp server name").as_deref(),
            Some("Server name is required.")
        );
        assert_eq!(server_name_error(Some(&json!("Production")), "x"), None);
        assert_eq!(
            server_name_error(Some(&json!("a".repeat(256))), "renamed smtp server name").as_deref(),
            Some("The renamed smtp server name field must not be greater than 255 characters.")
        );
    }

    #[test]
    fn uuids_are_version_four() {
        let id = uuid4();
        assert_eq!(id.len(), 36);
        assert_eq!(id.as_bytes()[14], b'4');
        assert_ne!(id, uuid4());
    }

    #[test]
    fn summarize_like_validation_exception() {
        assert_eq!(summarize(&[]), None);
        assert_eq!(summarize(&["a".to_owned()]).as_deref(), Some("a"));
        assert_eq!(
            summarize(&["a".to_owned(), "b".to_owned()]).as_deref(),
            Some("a (and 1 more error)")
        );
        assert_eq!(
            summarize(&["a".to_owned(), "b".to_owned(), "c".to_owned()]).as_deref(),
            Some("a (and 2 more errors)")
        );
    }
}
