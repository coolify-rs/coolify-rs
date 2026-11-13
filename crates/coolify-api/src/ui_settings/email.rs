//! `App\Livewire\SettingsEmail` with `App\Livewire\Concerns\ManagesSmtpServers`: the provider
//! that sends the instance's email, its named SMTP servers and templates, the Resend sender and
//! the separate sender of notification emails.

use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::{Extension, Json};
use coolify_db::instance_settings as settings;
use coolify_db::records::Record;
use coolify_jobs::notifications::Mail;
use coolify_jobs::notifications::email_settings as es;
use serde_json::{Map, Value, json};

use super::{
    Action, admin, as_f64, audit_instance, boolean, nullable, plain, save, server_error, text,
    to_dashboard,
};
use crate::audit::Meta;
use crate::auth::AuthCtx;
use crate::response::{ApiError, ApiResult, ok};
use crate::state::AppState;
use crate::ui_notifications::{blank, numeric, summarize, toast_error, unauthorized};

/// The component properties that mirror a column.
const EMAIL_KEYS: &[(&str, &str)] = &[
    ("smtpEnabled", "smtp_enabled"),
    ("smtpServerName", "smtp_server_name"),
    ("smtpHost", "smtp_host"),
    ("smtpPort", "smtp_port"),
    ("smtpEncryption", "smtp_encryption"),
    ("smtpUsername", "smtp_username"),
    ("smtpPassword", "smtp_password"),
    ("smtpTimeout", "smtp_timeout"),
    ("smtpEhloDomain", "smtp_ehlo_domain"),
    ("smtpFromAddress", "smtp_from_address"),
    ("smtpFromName", "smtp_from_name"),
    ("notificationFromAddress", "notification_from_address"),
    ("notificationFromName", "notification_from_name"),
    ("resendEnabled", "resend_enabled"),
    ("resendApiKey", "resend_api_key"),
    ("resendFromAddress", "resend_from_address"),
    ("resendFromName", "resend_from_name"),
];

/// What `syncData(true)` assigns.
const ALL_EMAIL_PROPS: &[&str] = &[
    "smtpEnabled",
    "smtpServerName",
    "smtpHost",
    "smtpPort",
    "smtpEncryption",
    "smtpUsername",
    "smtpPassword",
    "smtpTimeout",
    "smtpEhloDomain",
    "smtpFromAddress",
    "smtpFromName",
    "notificationFromAddress",
    "notificationFromName",
    "resendEnabled",
    "resendApiKey",
    "resendFromAddress",
    "resendFromName",
];

/// What `switchToSmtpServer`, `deleteActiveSmtpServer` and a rename can change.
const SERVER_COLUMNS: [&str; 11] = [
    "smtp_server_name",
    settings::SMTP_SERVERS,
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

/// `syncData()`: the component properties, with `emailProvider` as `activeEmailProvider()`.
fn email_values(state: &AppState, row: &Record) -> Result<Value, ApiError> {
    let mut out = Map::new();
    for (prop, col) in EMAIL_KEYS {
        let v = plain(state, row, col)?;
        // `?string` properties: integers arrive as strings.
        let v = match v {
            Value::Number(n) => json!(n.to_string()),
            other => other,
        };
        out.insert((*prop).to_owned(), v);
    }
    out.insert("emailProvider".to_owned(), json!(active_provider(row)));
    Ok(Value::Object(out))
}

/// `activeEmailProvider()`: the provider that sends email now.
fn active_provider(row: &Record) -> &'static str {
    es::enabled_email_provider_of(row).unwrap_or("none")
}

/// `is_transactional_emails_enabled()`.
fn transactional_enabled(row: &Record) -> bool {
    es::enabled_email_provider_of(row).is_some()
}

/// The attributes `HasSmtpServers` works on, decrypted.
fn email_model(state: &AppState, row: &Record) -> Result<Map<String, Value>, ApiError> {
    let mut model = Map::new();
    for column in SERVER_COLUMNS {
        model.insert(column.to_owned(), plain(state, row, column)?);
    }
    Ok(model)
}

/// The page: the form and what the SMTP server picker and the test button show.
fn page(state: &AppState, ctx: &AuthCtx, row: &Record) -> Result<Value, ApiError> {
    let model = email_model(state, row)?;
    Ok(json!({
        "settings": email_values(state, row)?,
        "active_provider": active_provider(row),
        "smtp_server_name": es::smtp_server_name(&model),
        "smtp_server_options": es::smtp_server_options(&model),
        "smtp_presets": es::smtp_presets_json(),
        "transactional_enabled": transactional_enabled(row),
        "can_send_test": transactional_enabled(row) && ctx.is_team_admin(),
    }))
}

pub(super) async fn show(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
) -> ApiResult {
    if !admin(&state, &ctx).await? {
        return Ok(to_dashboard());
    }
    let row = settings::row(&state.db).await?;
    let address: Option<String> = sqlx::query_scalar("SELECT email FROM users WHERE id = $1")
        .bind(ctx.user_id)
        .fetch_optional(state.db.pool())
        .await
        .map_err(|_| ApiError::Internal)?;
    let mut res = page(&state, &ctx, &row)?;
    res["test_email"] = json!(address);
    Ok(ok(res).into_response())
}

/// `'nullable|email'` with the default message.
fn email_attribute(v: &Map<String, Value>, prop: &str, attribute: &str, e: &mut Vec<String>) {
    if let Some(a) = text(v, prop).filter(|s| !s.trim().is_empty())
        && !crate::rules::is_email(a)
    {
        e.push(format!(
            "The {attribute} field must be a valid email address."
        ));
    }
}

/// `'nullable|string|max:255'` with the default message.
fn max_255(v: &Map<String, Value>, prop: &str, attribute: &str, e: &mut Vec<String>) {
    if text(v, prop).is_some_and(|s| s.chars().count() > 255) {
        e.push(format!(
            "The {attribute} field must not be greater than 255 characters."
        ));
    }
}

/// The `#[Validate]` attributes of `SettingsEmail`, in property order.
fn email_rules(v: &Map<String, Value>) -> Vec<String> {
    let mut e = Vec::new();
    email_attribute(v, "smtpFromAddress", "smtp from address", &mut e);
    email_attribute(
        v,
        "notificationFromAddress",
        "notification from address",
        &mut e,
    );
    max_255(v, "notificationFromName", "notification from name", &mut e);
    if let Some(p) = v.get("smtpPort").filter(|p| !blank(Some(p))) {
        if !numeric(p) {
            e.push("The smtp port field must be a number.".to_owned());
        } else if as_f64(p) < 1.0 {
            e.push("The smtp port field must be at least 1.".to_owned());
        } else if as_f64(p) > 65535.0 {
            e.push("The smtp port field must not be greater than 65535.".to_owned());
        }
    }
    if let Some(enc) = text(v, "smtpEncryption").filter(|s| !s.trim().is_empty())
        && !matches!(enc, "starttls" | "tls" | "none")
    {
        e.push("The selected smtp encryption is invalid.".to_owned());
    }
    if let Some(t) = v.get("smtpTimeout").filter(|p| !blank(Some(p)))
        && !numeric(t)
    {
        e.push("The smtp timeout field must be a number.".to_owned());
    }
    email_attribute(v, "resendFromAddress", "resend from address", &mut e);
    match v.get("emailProvider") {
        Some(p) if blank(Some(p)) => e.push("The email provider field is required.".to_owned()),
        Some(p) if !matches!(p.as_str(), Some("none" | "smtp" | "resend")) => {
            e.push("The selected email provider is invalid.".to_owned());
        }
        _ => {}
    }
    email_attribute(v, "testEmailAddress", "test email address", &mut e);
    e
}

fn hostname_rule(v: &Map<String, Value>, attr: &str) -> Option<String> {
    let host = v.get("smtpEhloDomain")?;
    if blank(Some(host)) {
        return None;
    }
    crate::rules::valid_hostname(host).map(|m| m.replace(":attribute", attr))
}

/// `'required|email'` and `'required|string'` of a sender, with the custom messages.
fn sender_rules(v: &Map<String, Value>, address: &str, name: &str, e: &mut Vec<String>) {
    match text(v, address) {
        None => e.push("From Address is required.".to_owned()),
        Some(s) if s.trim().is_empty() => e.push("From Address is required.".to_owned()),
        Some(a) if !crate::rules::is_email(a) => {
            e.push("Please enter a valid email address.".to_owned());
        }
        _ => {}
    }
    if blank(v.get(name)) {
        e.push("From Name is required.".to_owned());
    }
}

/// `validateSmtpSettings`, in rule order.
fn smtp_rules(v: &Map<String, Value>) -> Vec<String> {
    let mut e = Vec::new();
    max_255(v, "smtpServerName", "smtp server name", &mut e);
    sender_rules(v, "smtpFromAddress", "smtpFromName", &mut e);
    if blank(v.get("smtpHost")) {
        e.push("SMTP Host is required.".to_owned());
    }
    match v.get("smtpPort") {
        p if blank(p) => e.push("SMTP Port is required.".to_owned()),
        Some(p) if !numeric(p) => e.push("SMTP Port must be a number.".to_owned()),
        _ => {}
    }
    match text(v, "smtpEncryption") {
        None | Some("") => e.push("Encryption type is required.".to_owned()),
        Some(s) if !matches!(s, "starttls" | "tls" | "none") => {
            e.push("The selected smtp encryption is invalid.".to_owned());
        }
        _ => {}
    }
    // `Rule::requiredIf(SmtpPresets::requiresLogin($this->smtpHost))`, then `required_with`.
    let needs_login = es::smtp_requires_login(text(v, "smtpHost"));
    if needs_login && blank(v.get("smtpUsername")) {
        e.push("This provider needs a username.".to_owned());
    }
    if blank(v.get("smtpPassword")) {
        if needs_login {
            e.push("This provider needs a password.".to_owned());
        } else if !blank(v.get("smtpUsername")) {
            e.push("Enter the password for this username.".to_owned());
        }
    }
    if let Some(t) = v.get("smtpTimeout").filter(|p| !blank(Some(p)))
        && !numeric(t)
    {
        e.push("The smtp timeout field must be a number.".to_owned());
    }
    if let Some(m) = hostname_rule(v, "smtp ehlo domain") {
        e.push(m);
    }
    e
}

/// `validateResendSettings`: the key and the sender are required while Resend is on.
fn resend_rules(v: &Map<String, Value>) -> Vec<String> {
    let mut e = Vec::new();
    if boolean(v, "resendEnabled") {
        if blank(v.get("resendApiKey")) {
            e.push("Resend API Key is required.".to_owned());
        }
        sender_rules(v, "resendFromAddress", "resendFromName", &mut e);
    } else if let Some(a) = text(v, "resendFromAddress").filter(|s| !s.trim().is_empty())
        && !crate::rules::is_email(a)
    {
        e.push("Please enter a valid email address.".to_owned());
    }
    e
}

/// `enableSelectedEmailProvider()`: turns on the provider selected in the provider list and
/// returns the errors of `validateEnabledEmailProvider()`. When they are not empty nothing is
/// saved, so the saved provider stays on.
fn enable_selected_provider(v: &mut Map<String, Value>) -> Vec<String> {
    let provider = text(v, "emailProvider").unwrap_or("none").to_owned();
    v.insert("smtpEnabled".into(), json!(provider == "smtp"));
    v.insert("resendEnabled".into(), json!(provider == "resend"));
    match provider.as_str() {
        "smtp" => smtp_rules(v),
        "resend" => resend_rules(v),
        _ => Vec::new(),
    }
}

fn email_pair(v: &Map<String, Value>, prop: &str) -> Value {
    if matches!(prop, "smtpEnabled" | "resendEnabled") {
        json!(boolean(v, prop))
    } else {
        nullable(v, prop)
    }
}

/// The columns of `EMAIL_KEYS` the action assigns.
fn email_pairs(v: &Map<String, Value>, props: &[&str]) -> Vec<(&'static str, Value)> {
    let mut out: Vec<(&'static str, Value)> = Vec::new();
    for (prop, col) in EMAIL_KEYS {
        if props.contains(prop) {
            out.push((col, email_pair(v, prop)));
        }
    }
    out
}

/// The pairs of a model that `HasSmtpServers` changed.
fn model_pairs(model: &Map<String, Value>) -> Vec<(&'static str, Value)> {
    SERVER_COLUMNS
        .iter()
        .map(|column| (*column, model.get(*column).cloned().unwrap_or(Value::Null)))
        .collect()
}

/// `'required|string|max:255'` with `'Server name is required.'`.
fn server_name_error(v: &Map<String, Value>, prop: &str, attribute: &str) -> Option<String> {
    if blank(v.get(prop)) {
        return Some("Server name is required.".to_owned());
    }
    match text(v, prop) {
        Some(s) if s.chars().count() > 255 => Some(format!(
            "The {attribute} field must not be greater than 255 characters."
        )),
        Some(_) => None,
        None => Some(format!("The {attribute} field must be a string.")),
    }
}

/// The first rules of `sendTestEmail`.
fn test_rules(v: &Map<String, Value>) -> Vec<String> {
    let mut e = Vec::new();
    match text(v, "testEmailAddress") {
        None => e.push("Test email address is required.".to_owned()),
        Some(s) if s.trim().is_empty() => {
            e.push("Test email address is required.".to_owned());
        }
        Some(a) if !crate::rules::is_email(a) => {
            e.push("Please enter a valid email address.".to_owned());
        }
        _ => {}
    }
    match text(v, "notificationFromAddress").filter(|s| !s.trim().is_empty()) {
        None if boolean(v, "testUsesNotificationSender") => e.push(
            "Notification from address is required to test the notification sender.".to_owned(),
        ),
        Some(a) if !crate::rules::is_email(a) => {
            e.push("The notification from address field must be a valid email address.".to_owned());
        }
        _ => {}
    }
    max_255(v, "notificationFromName", "notification from name", &mut e);
    e
}

#[allow(clippy::too_many_lines)]
pub(super) async fn action(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Json(body): Json<Action>,
) -> ApiResult {
    if !admin(&state, &ctx).await? {
        return Ok(unauthorized());
    }
    let mut v = body.values;
    // Livewire does not hydrate a typed property from null: it keeps its default, here `'starttls'`.
    if v.get("smtpEncryption").is_none_or(Value::is_null) {
        v.insert("smtpEncryption".into(), json!("starttls"));
    }
    let saved = settings::row(&state.db).await?;
    // `saveEmailSettings()`: `auditChangedFields`, `save()`, `auditLog`.
    let run_save = |pairs: Vec<(&'static str, Value)>| {
        let state = state.clone();
        let ctx = ctx.clone();
        let meta = meta.clone();
        async move {
            let (changed, _) = save(&state, &pairs).await?;
            audit_instance(
                &state,
                &ctx,
                &meta,
                "ui.settings.email.updated",
                "email",
                changed,
            )
            .await;
            Ok::<_, ApiError>(())
        }
    };
    let invalid = |m: String| Ok(toast_error(StatusCode::UNPROCESSABLE_ENTITY, m));
    let mut message: Option<String> = None;
    let mut extra: Option<(&str, Value)> = None;
    match body.action.as_str() {
        "submit" | "provider" => {
            // `submit()`; `selectEmailProvider()` is the same after a provider that validates.
            let errors = enable_selected_provider(&mut v);
            if !errors.is_empty() && body.action == "provider" {
                // Not complete yet: show the fields of the provider and the "Not active yet"
                // notice, without an error. Nothing is saved.
                return Ok(ok(json!({"pending": true})).into_response());
            }
            if let Some(m) = summarize(&errors) {
                return invalid(m);
            }
            // `syncData(true)`: the attribute rules, then `ValidHostname`.
            let mut e = email_rules(&v);
            if e.is_empty()
                && let Some(m) = hostname_rule(&v, "smtp ehlo domain")
            {
                e.push(m);
            }
            if let Some(m) = summarize(&e) {
                return invalid(m);
            }
            let mut pairs = email_pairs(&v, ALL_EMAIL_PROPS);
            // `fillSmtpServerSettings()`: a new server first keeps the server in use in the
            // saved list.
            if boolean(&v, "isNewSmtpServer") {
                let mut model = email_model(&state, &saved)?;
                es::SmtpServers(&mut model).keep_active(&uuid4());
                pairs.push((
                    settings::SMTP_SERVERS,
                    model.remove(settings::SMTP_SERVERS).unwrap_or(Value::Null),
                ));
            }
            run_save(pairs).await?;
            message = Some("Transactional email settings updated.".to_owned());
        }
        "addServer" => {
            // `addSmtpServer()`: starts a new server with the fields of the selected template or
            // empty fields. The server in use stays saved until the new one is saved.
            let mut e = Vec::new();
            e.extend(server_name_error(
                &v,
                "newSmtpServerName",
                "new smtp server name",
            ));
            let template = text(&v, "newSmtpServerTemplate").filter(|t| !t.is_empty());
            let preset = template.and_then(es::smtp_preset);
            if template.is_some() && preset.is_none() {
                e.push("The selected new smtp server template is invalid.".to_owned());
            }
            if let Some(m) = summarize(&e) {
                return invalid(m);
            }
            extra = Some((
                "new_server",
                json!({
                    "smtpServerName": v.get("newSmtpServerName"),
                    "smtpHost": preset.map(|p| p.host),
                    "smtpPort": preset.map(|p| p.port.to_string()),
                    "smtpEncryption": preset.map_or("starttls", |p| p.encryption),
                    "smtpUsername": preset.and_then(|p| p.username),
                    "smtpPassword": null,
                    "smtpTimeout": null,
                    "smtpEhloDomain": null,
                }),
            ));
        }
        "switchServer" => {
            // `switchSmtpServer($id)`.
            let id = text(&v, "selectedSmtpServer").unwrap_or("active");
            if id != "active" {
                let mut model = email_model(&state, &saved)?;
                if !es::SmtpServers(&mut model).switch_to(id, &uuid4()) {
                    return Ok(server_error(
                        "This SMTP server does not exist anymore. Reload the page.",
                    ));
                }
                run_save(model_pairs(&model)).await?;
                message = Some(format!(
                    "Switched to SMTP server {}.",
                    es::smtp_server_name(&model)
                ));
            }
        }
        "renameServer" => {
            // `renameSmtpServer()`: only the name is saved; other unsaved changes on the page
            // stay unsaved.
            if let Some(m) =
                server_name_error(&v, "renamedSmtpServerName", "renamed smtp server name")
            {
                return invalid(m);
            }
            run_save(vec![(
                "smtp_server_name",
                v.get("renamedSmtpServerName")
                    .cloned()
                    .unwrap_or(Value::Null),
            )])
            .await?;
            message = Some("SMTP server renamed.".to_owned());
        }
        "deleteServer" => {
            // `deleteSmtpServer()`: deletes the server in use and uses the next saved server.
            let mut model = email_model(&state, &saved)?;
            let name = es::smtp_server_name(&model);
            if !es::SmtpServers(&mut model).delete_active() {
                return Ok(server_error(
                    "This is the only SMTP server. To stop using SMTP, select another provider.",
                ));
            }
            run_save(model_pairs(&model)).await?;
            message = Some(format!(
                "SMTP server {name} deleted. Now using {}.",
                es::smtp_server_name(&model)
            ));
        }
        "test" => {
            // `sendTestEmail`.
            if let Some(m) = summarize(&test_rules(&v)) {
                return invalid(m);
            }
            // Send with the sender on the page, so that a changed sender can be tested before
            // Save. The provider is the saved one.
            let resend = saved.get("resend_enabled") == Some(&json!(true));
            v.insert("resendEnabled".into(), json!(resend));
            let (errors, sender) = if resend {
                (resend_rules(&v), ["resendFromAddress", "resendFromName"])
            } else {
                (smtp_rules(&v), ["smtpFromAddress", "smtpFromName"])
            };
            if let Some(m) = summarize(&errors) {
                return invalid(m);
            }
            let mut props = sender.to_vec();
            props.extend(["notificationFromAddress", "notificationFromName"]);
            run_save(email_pairs(&v, &props)).await?;
            // `RateLimiter::attempt('test-email:'.$team->id, 0, ..., 10)`.
            let key = format!("test-email:{}", ctx.team_id);
            if crate::rules::throttle(&key, 1, Duration::from_secs(10)).is_some() {
                return Ok(server_error("Too many messages sent!"));
            }
            let to = text(&v, "testEmailAddress").unwrap_or_default().to_owned();
            // `new Test($this->testEmailAddress, usesNotificationSender: ...)`.
            let sent = if boolean(&v, "testUsesNotificationSender") {
                coolify_jobs::notifications::send_transactional_as_notification(
                    &state.db,
                    state.encrypter.as_ref(),
                    &to,
                    Mail::Test,
                )
                .await
            } else {
                coolify_jobs::notifications::send_transactional(
                    &state.db,
                    state.encrypter.as_ref(),
                    &to,
                    Mail::Test,
                )
                .await
            };
            if let Err(m) = sent {
                return Ok(server_error(m));
            }
            message = Some("Test Email sent.".to_owned());
        }
        _ => return Err(ApiError::Message(StatusCode::NOT_FOUND, "Not found.")),
    }
    let row = settings::row(&state.db).await?;
    let mut res = page(&state, &ctx, &row)?;
    if let Some(m) = message {
        res["message"] = json!(m);
    }
    if let Some((key, value)) = extra {
        res[key] = value;
    }
    Ok(ok(res).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn form(pairs: Value) -> Map<String, Value> {
        pairs.as_object().unwrap().clone()
    }

    fn smtp_form() -> Map<String, Value> {
        form(json!({
            "emailProvider": "smtp",
            "smtpFromAddress": "a@example.com",
            "smtpFromName": "A",
            "smtpHost": "mail.example.com",
            "smtpPort": "587",
            "smtpEncryption": "starttls",
        }))
    }

    #[test]
    fn the_selected_provider_is_turned_on_and_validated() {
        let mut v = smtp_form();
        assert!(enable_selected_provider(&mut v).is_empty());
        assert_eq!(v["smtpEnabled"], true);
        assert_eq!(v["resendEnabled"], false);

        let mut v = form(json!({"emailProvider": "resend", "smtpFromAddress": "a@example.com"}));
        assert_eq!(
            enable_selected_provider(&mut v),
            [
                "Resend API Key is required.",
                "From Address is required.",
                "From Name is required.",
            ]
        );
        assert_eq!(v["resendEnabled"], true);
        assert_eq!(v["smtpEnabled"], false);

        let mut v = form(json!({"emailProvider": "none"}));
        assert!(enable_selected_provider(&mut v).is_empty());
        assert_eq!(v["smtpEnabled"], false);
        assert_eq!(v["resendEnabled"], false);
    }

    #[test]
    fn a_known_provider_needs_a_login() {
        let mut v = smtp_form();
        v.insert("smtpHost".into(), json!("smtp.sendgrid.net"));
        assert_eq!(
            smtp_rules(&v),
            [
                "This provider needs a username.",
                "This provider needs a password."
            ]
        );
        v.insert("smtpUsername".into(), json!("apikey"));
        assert_eq!(smtp_rules(&v), ["This provider needs a password."]);
        v.insert("smtpHost".into(), json!("mail.example.com"));
        assert_eq!(smtp_rules(&v), ["Enter the password for this username."]);
        v.insert("smtpPassword".into(), json!("secret"));
        assert!(smtp_rules(&v).is_empty());
    }

    #[test]
    fn resend_is_only_checked_while_it_is_on() {
        let v = form(json!({"resendEnabled": false}));
        assert!(resend_rules(&v).is_empty());
        let v = form(json!({"resendEnabled": false, "resendFromAddress": "nope"}));
        assert_eq!(resend_rules(&v), ["Please enter a valid email address."]);
        let v = form(json!({
            "resendEnabled": true,
            "resendApiKey": "re_123",
            "resendFromAddress": "a@example.com",
            "resendFromName": "A",
        }));
        assert!(resend_rules(&v).is_empty());
    }

    #[test]
    fn the_test_email_needs_the_notification_sender_it_tests() {
        let v = form(json!({"testEmailAddress": "to@example.com"}));
        assert!(test_rules(&v).is_empty());
        let v =
            form(json!({"testEmailAddress": "to@example.com", "testUsesNotificationSender": true}));
        assert_eq!(
            test_rules(&v),
            ["Notification from address is required to test the notification sender."]
        );
        let v = form(json!({
            "testEmailAddress": "",
            "notificationFromAddress": "nope",
            "notificationFromName": "n".repeat(256),
        }));
        assert_eq!(
            test_rules(&v),
            [
                "Test email address is required.",
                "The notification from address field must be a valid email address.",
                "The notification from name field must not be greater than 255 characters.",
            ]
        );
    }

    #[test]
    fn the_form_rules_cover_the_new_senders() {
        let v = form(json!({
            "notificationFromAddress": "nope",
            "resendFromAddress": "nope",
            "emailProvider": "sendmail",
        }));
        assert_eq!(
            email_rules(&v),
            [
                "The notification from address field must be a valid email address.",
                "The resend from address field must be a valid email address.",
                "The selected email provider is invalid.",
            ]
        );
    }

    #[test]
    fn a_save_assigns_every_email_column_once() {
        let pairs = email_pairs(&smtp_form(), ALL_EMAIL_PROPS);
        assert_eq!(pairs.len(), EMAIL_KEYS.len());
        assert!(pairs.iter().any(|(c, _)| *c == "notification_from_address"));
        assert!(pairs.iter().any(|(c, _)| *c == "resend_from_name"));
        assert!(pairs.iter().any(|(c, _)| *c == "smtp_server_name"));
        let host = pairs.iter().find(|(c, _)| *c == "smtp_host").unwrap();
        assert_eq!(host.1, "mail.example.com");
    }

    #[test]
    fn a_server_name_is_required() {
        let v = form(json!({"newSmtpServerName": " "}));
        assert_eq!(
            server_name_error(&v, "newSmtpServerName", "new smtp server name").as_deref(),
            Some("Server name is required.")
        );
        let v = form(json!({"renamedSmtpServerName": "Production"}));
        assert_eq!(
            server_name_error(&v, "renamedSmtpServerName", "renamed smtp server name"),
            None
        );
    }
}
