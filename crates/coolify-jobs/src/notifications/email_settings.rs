//! Email settings shared by the instance (`InstanceSettings`) and the teams
//! (`EmailNotificationSettings`): `enabled_email_provider()` and `mail_from_identity()` of
//! `bootstrap/helpers/notifications.php`, `App\Support\SmtpPresets`, `App\Traits\HasSmtpServers`
//! and `App\Rules\SafeSmtpHost`.

use std::net::IpAddr;

use coolify_db::Db;
use serde_json::{Map, Value, json};

use super::php::{filled, php_trim};
use crate::databases::s3::{HostStatus, UrlPolicy, preferred_ip};

/// `email_provider_labels()`: the providers in the order the provider selectors show them.
pub const EMAIL_PROVIDER_LABELS: [(&str, &str); 3] =
    [("none", "None"), ("smtp", "SMTP"), ("resend", "Resend")];

/// `email_provider_labels()[$provider]`.
pub fn email_provider_label(provider: &str) -> Option<&'static str> {
    EMAIL_PROVIDER_LABELS
        .iter()
        .find(|(k, _)| *k == provider)
        .map(|(_, l)| *l)
}

/// `enabled_email_provider($settings)`: the provider that is turned on. Only one provider can be
/// on at a time; Resend wins when both flags are set.
pub fn enabled_email_provider(resend_enabled: bool, smtp_enabled: bool) -> Option<&'static str> {
    if resend_enabled {
        Some("resend")
    } else if smtp_enabled {
        Some("smtp")
    } else {
        None
    }
}

/// [`enabled_email_provider`] of a settings row.
pub fn enabled_email_provider_of(row: &Map<String, Value>) -> Option<&'static str> {
    let flag = |k: &str| row.get(k).and_then(Value::as_bool).unwrap_or(false);
    enabled_email_provider(flag("resend_enabled"), flag("smtp_enabled"))
}

/// `mail_from_identity($settings, $asNotification)`: the address and the name of the sender.
/// `get` returns the decrypted attribute. With `as_notification`, the optional notification
/// sender of the instance settings replaces the default sender.
pub fn mail_from_identity(
    get: impl Fn(&str) -> Option<String>,
    resend_enabled: bool,
    as_notification: bool,
) -> Result<(String, String), String> {
    // Each provider has its own sender. Resend uses the SMTP sender when it has none.
    let provider_sender = |field: &str| -> String {
        let value = if resend_enabled {
            get(&format!("resend_{field}")).unwrap_or_default()
        } else {
            String::new()
        };
        let value = php_trim(&value);
        if value.is_empty() {
            php_trim(&get(&format!("smtp_{field}")).unwrap_or_default()).to_owned()
        } else {
            value.to_owned()
        }
    };
    let notification_address = get("notification_from_address").filter(|a| !php_trim(a).is_empty());
    let (address, mut name) = match notification_address {
        Some(address) if as_notification => (
            address,
            php_trim(&get("notification_from_name").unwrap_or_default()).to_owned(),
        ),
        _ => {
            let address = provider_sender("from_address");
            if address.is_empty() {
                return Err("Transactional email sender address is not configured.".to_owned());
            }
            (address, String::new())
        }
    };
    if name.is_empty() {
        name = provider_sender("from_name");
    }
    if name.is_empty() {
        name = "Coolify".to_owned();
    }
    Ok((address, name))
}

/// One entry of `SmtpPresets::PRESETS`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SmtpPreset {
    pub key: &'static str,
    pub label: &'static str,
    pub host: &'static str,
    pub port: u16,
    pub encryption: &'static str,
    pub username: Option<&'static str>,
}

const fn preset(
    key: &'static str,
    label: &'static str,
    host: &'static str,
    port: u16,
    encryption: &'static str,
    username: Option<&'static str>,
) -> SmtpPreset {
    SmtpPreset {
        key,
        label,
        host,
        port,
        encryption,
        username,
    }
}

/// `SmtpPresets::PRESETS`: SMTP settings of common email providers. A preset only fills the
/// connection fields; the user still enters the username and password from the provider.
pub const SMTP_PRESETS: [SmtpPreset; 12] = [
    preset(
        "postshiba",
        "PostShiba",
        "smtp.postshiba.com",
        587,
        "starttls",
        None,
    ),
    preset(
        "amazon-ses",
        "Amazon SES (us-east-1)",
        "email-smtp.us-east-1.amazonaws.com",
        587,
        "starttls",
        None,
    ),
    preset(
        "brevo",
        "Brevo",
        "smtp-relay.brevo.com",
        587,
        "starttls",
        None,
    ),
    preset("gmail", "Gmail", "smtp.gmail.com", 587, "starttls", None),
    preset(
        "mailgun",
        "Mailgun",
        "smtp.mailgun.org",
        587,
        "starttls",
        None,
    ),
    preset(
        "mailjet",
        "Mailjet",
        "in-v3.mailjet.com",
        587,
        "starttls",
        None,
    ),
    preset(
        "microsoft-365",
        "Microsoft 365",
        "smtp.office365.com",
        587,
        "starttls",
        None,
    ),
    preset(
        "postmark",
        "Postmark",
        "smtp.postmarkapp.com",
        587,
        "starttls",
        None,
    ),
    preset(
        "proton-mail",
        "Proton Mail",
        "smtp.protonmail.ch",
        587,
        "starttls",
        None,
    ),
    preset(
        "resend",
        "Resend",
        "smtp.resend.com",
        465,
        "tls",
        Some("resend"),
    ),
    preset(
        "sendgrid",
        "SendGrid",
        "smtp.sendgrid.net",
        587,
        "starttls",
        Some("apikey"),
    ),
    preset("zoho", "Zoho Mail", "smtp.zoho.com", 465, "tls", None),
];

/// `SmtpPresets::PRESETS[$key] ?? null`.
pub fn smtp_preset(key: &str) -> Option<&'static SmtpPreset> {
    SMTP_PRESETS.iter().find(|p| p.key == key)
}

/// `SmtpPresets::requiresLogin($host)`: whether the host belongs to a provider of the presets.
/// All of them need a username and password.
pub fn smtp_requires_login(host: Option<&str>) -> bool {
    let host = php_trim(host.unwrap_or_default()).to_lowercase();
    !host.is_empty() && SMTP_PRESETS.iter().any(|p| p.host == host)
}

/// `SmtpPresets::options()`: options for a listbox.
pub fn smtp_preset_options() -> Value {
    Value::Array(
        SMTP_PRESETS
            .iter()
            .map(|p| json!({"value": p.key, "label": p.label}))
            .collect(),
    )
}

/// The presets as the settings pages need them: `SmtpPresets::options()` plus the fields that
/// `addSmtpServer` fills.
pub fn smtp_presets_json() -> Value {
    Value::Array(
        SMTP_PRESETS
            .iter()
            .map(|p| {
                json!({
                    "value": p.key,
                    "label": p.label,
                    "host": p.host,
                    "port": p.port,
                    "encryption": p.encryption,
                    "username": p.username,
                })
            })
            .collect(),
    )
}

/// `HasSmtpServers::DEFAULT_SMTP_SERVER_NAME`.
pub const DEFAULT_SMTP_SERVER_NAME: &str = "Default";

/// `HasSmtpServers::SMTP_SERVER_COLUMNS`: the columns that belong to one SMTP server, including
/// its sender, because each server usually sends from its own verified domain.
pub const SMTP_SERVER_COLUMNS: [&str; 9] = [
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

/// `HasSmtpServers` over the attributes of a settings model (decrypted, `smtp_servers` as the
/// array of its `encrypted:array` cast). The smtp_* columns always hold the server in use;
/// `smtp_servers` holds the other saved servers. The methods change the attributes but do not
/// save them.
#[derive(Debug)]
pub struct SmtpServers<'a>(pub &'a mut Map<String, Value>);

/// `smtpServerName()` of a model that is only read.
pub fn smtp_server_name(model: &Map<String, Value>) -> String {
    match model.get("smtp_server_name") {
        Some(Value::String(name)) if filled(model.get("smtp_server_name")) => name.clone(),
        _ => DEFAULT_SMTP_SERVER_NAME.to_owned(),
    }
}

/// `savedSmtpServers()` of a model that is only read.
pub fn saved_smtp_servers(model: &Map<String, Value>) -> Vec<Value> {
    match model.get("smtp_servers") {
        Some(Value::Array(list)) => list.clone(),
        // `array_values()` of a decoded JSON object.
        Some(Value::Object(map)) => map.values().cloned().collect(),
        _ => Vec::new(),
    }
}

fn server_id(server: &Value) -> Option<&str> {
    server.get("id").and_then(Value::as_str)
}

impl SmtpServers<'_> {
    /// `smtpServerName()`.
    pub fn name(&self) -> String {
        smtp_server_name(self.0)
    }

    /// `savedSmtpServers()`.
    pub fn saved(&self) -> Vec<Value> {
        saved_smtp_servers(self.0)
    }

    /// `keepActiveSmtpServer()`: keeps the server in use in the saved list, so that new values
    /// can replace it. A server without a host has nothing worth keeping. `id` is `Str::uuid()`.
    pub fn keep_active(&mut self, id: &str) {
        if !filled(self.0.get("smtp_host")) {
            return;
        }
        let mut server = Map::new();
        server.insert("id".to_owned(), json!(id));
        server.insert("name".to_owned(), json!(self.name()));
        for column in SMTP_SERVER_COLUMNS {
            server.insert(
                column.to_owned(),
                self.0.get(column).cloned().unwrap_or(Value::Null),
            );
        }
        let mut servers = self.saved();
        servers.push(Value::Object(server));
        self.0
            .insert("smtp_servers".to_owned(), Value::Array(servers));
    }

    /// `switchToSmtpServer($id)`: uses the saved server with the given ID, and keeps the server
    /// in use in the saved list. `new_id` is the `Str::uuid()` of the kept server.
    pub fn switch_to(&mut self, id: &str, new_id: &str) -> bool {
        let saved = self.saved();
        let Some(target) = saved.iter().find(|s| server_id(s) == Some(id)).cloned() else {
            return false;
        };
        let rest: Vec<Value> = saved
            .into_iter()
            .filter(|s| server_id(s) != Some(id))
            .collect();
        self.0.insert("smtp_servers".to_owned(), Value::Array(rest));
        self.keep_active(new_id);
        self.use_server(&target);
        true
    }

    /// `deleteActiveSmtpServer()`: deletes the server in use and uses the first saved server
    /// instead.
    pub fn delete_active(&mut self) -> bool {
        let mut servers = self.saved();
        if servers.is_empty() {
            return false;
        }
        let first = servers.remove(0);
        self.0
            .insert("smtp_servers".to_owned(), Value::Array(servers));
        self.use_server(&first);
        true
    }

    /// `useSmtpServer($server)`.
    fn use_server(&mut self, server: &Value) {
        self.0.insert(
            "smtp_server_name".to_owned(),
            server.get("name").cloned().unwrap_or(Value::Null),
        );
        for column in SMTP_SERVER_COLUMNS {
            self.0.insert(
                column.to_owned(),
                server.get(column).cloned().unwrap_or(Value::Null),
            );
        }
    }
}

/// `ManagesSmtpServers::smtpServerOptions()`: the server in use first, then the saved ones.
pub fn smtp_server_options(model: &Map<String, Value>) -> Value {
    let mut options = vec![json!({"value": "active", "label": smtp_server_name(model)})];
    for server in saved_smtp_servers(model) {
        options.push(json!({
            "value": server.get("id").cloned().unwrap_or(Value::Null),
            "label": server.get("name").cloned().unwrap_or(Value::Null),
        }));
    }
    Value::Array(options)
}

/// The exception codes of `SafeSmtpHost::connectionAddress`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmtpHostErrorKind {
    /// `HOST_NOT_ALLOWED`: the host is invalid or points to a blocked address. Sending again
    /// fails again.
    NotAllowed,
    /// `HOST_UNRESOLVED`: the host did not resolve. This can be temporary, so the send can be
    /// retried.
    Unresolved,
}

/// The `RuntimeException` of `SafeSmtpHost::connectionAddress`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmtpHostError {
    pub kind: SmtpHostErrorKind,
    pub message: String,
}

/// `normalizeIpAddress($value)`: an IPv6 address without brackets in its canonical form; other
/// values trimmed.
fn normalize_ip_address(value: &str) -> String {
    let value = php_trim(value);
    let without_brackets = value.trim_matches(|c| c == '[' || c == ']');
    match without_brackets.parse::<std::net::Ipv6Addr>() {
        Ok(ip) => ip.to_string(),
        Err(_) => value.to_owned(),
    }
}

/// `SafeSmtpHost::normalizeHost($host)`: the host to check and connect to: an IP address without
/// brackets, or a lower-case host name without the trailing dot of a fully qualified name
/// (`smtp.gmail.com.`). TLS verifies this name.
pub fn normalize_smtp_host(host: &str) -> String {
    let host = normalize_ip_address(host);
    if host.parse::<IpAddr>().is_ok() {
        return host;
    }
    let host = host.to_lowercase();
    match host.strip_suffix('.') {
        Some(stripped) => stripped.to_owned(),
        None => host,
    }
}

/// `SafeSmtpHost::isValidHost($host)`: an IP address, or a host name whose labels have letters,
/// digits, hyphens, and underscores (Docker service names such as `mail_server`).
pub fn is_valid_smtp_host(host: &str) -> bool {
    if host.parse::<IpAddr>().is_ok() {
        return true;
    }
    if host.is_empty() || host.len() > 253 {
        return false;
    }
    // `/\A[a-z0-9_](?:[a-z0-9_-]{0,61}[a-z0-9_])?\z/i` for each label.
    host.split('.').all(|label| {
        let bytes = label.as_bytes();
        let edge = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
        let inner = |b: u8| edge(b) || b == b'-';
        match bytes {
            [] => false,
            [only] => edge(*only),
            [first, middle @ .., last] => {
                bytes.len() <= 63 && edge(*first) && edge(*last) && middle.iter().all(|b| inner(*b))
            }
        }
    })
}

/// `SafeSmtpHost::validate`: the failure message for a host that is not blank, or `None`.
/// `is_instance_admin` and `settings_link` (`route('settings.advanced').'#endpoint-section'`)
/// pick the "blocked" message.
pub async fn smtp_host_error(
    db: &Db,
    value: &str,
    is_instance_admin: bool,
    settings_link: &str,
) -> Option<String> {
    let host = normalize_smtp_host(value);
    if !is_valid_smtp_host(&host) {
        return Some("The SMTP host must be a valid hostname or IP address.".to_owned());
    }
    let policy = UrlPolicy::for_smtp_host(db).await;
    smtp_host_status_message(
        policy.check_connection_host(&host).await.status,
        is_instance_admin,
        settings_link,
    )
}

/// The `match` of `SafeSmtpHost::validate` over the status of `checkConnectionHost`.
pub fn smtp_host_status_message(
    status: HostStatus,
    is_instance_admin: bool,
    settings_link: &str,
) -> Option<String> {
    match status {
        HostStatus::Allowed => None,
        HostStatus::Unresolved => Some("The SMTP host could not be resolved.".to_owned()),
        HostStatus::LinkLocal => {
            Some("The SMTP host must not point to link-local addresses.".to_owned())
        }
        HostStatus::Blocked if is_instance_admin => Some(format!(
            "The SMTP host points to a local or private address that is not allowed. Configure allowed internal targets: {settings_link}"
        )),
        HostStatus::Blocked => Some(
            "The SMTP host points to a local or private address that is not allowed. Ask your Coolify administrator to allow this host."
                .to_owned(),
        ),
    }
}

/// `SafeSmtpHost::connectionAddress($host)`: checks the host again just before a connection and
/// returns the address to connect to, so a DNS change after validation cannot point the
/// connection at a blocked address.
pub async fn smtp_connection_address(db: &Db, host: &str) -> Result<String, SmtpHostError> {
    let host = normalize_smtp_host(host);
    if !is_valid_smtp_host(&host) {
        return Err(SmtpHostError {
            kind: SmtpHostErrorKind::NotAllowed,
            message: "The SMTP host is not a valid hostname or IP address.".to_owned(),
        });
    }
    let policy = UrlPolicy::for_smtp_host(db).await;
    let checked = policy.check_connection_host(&host).await;
    connection_address_of(&host, checked.status, &checked.ips)
}

/// The decision of `connectionAddress` after `checkConnectionHost`.
pub fn connection_address_of(
    host: &str,
    status: HostStatus,
    ips: &[IpAddr],
) -> Result<String, SmtpHostError> {
    match status {
        HostStatus::Unresolved => Err(SmtpHostError {
            kind: SmtpHostErrorKind::Unresolved,
            message: "The SMTP host could not be resolved.".to_owned(),
        }),
        HostStatus::Blocked | HostStatus::LinkLocal => Err(SmtpHostError {
            kind: SmtpHostErrorKind::NotAllowed,
            message: "The SMTP host is not allowed: it points to a local or private address."
                .to_owned(),
        }),
        HostStatus::Allowed if ips.is_empty() => Ok(host.to_owned()),
        HostStatus::Allowed => Ok(preferred_ip(ips).to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: std::collections::HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        move |k: &str| map.get(k).cloned()
    }

    #[test]
    fn only_one_provider_is_enabled() {
        assert_eq!(enabled_email_provider(false, false), None);
        assert_eq!(enabled_email_provider(false, true), Some("smtp"));
        assert_eq!(enabled_email_provider(true, false), Some("resend"));
        assert_eq!(enabled_email_provider(true, true), Some("resend"));
        assert_eq!(email_provider_label("smtp"), Some("SMTP"));
        assert_eq!(email_provider_label("none"), Some("None"));
        assert_eq!(email_provider_label("x"), None);
    }

    #[test]
    fn the_sender_defaults_to_the_smtp_sender() {
        let s = settings(&[
            ("smtp_from_address", " smtp@example.com "),
            ("smtp_from_name", " SMTP Name "),
        ]);
        assert_eq!(
            mail_from_identity(&s, false, false),
            Ok(("smtp@example.com".to_owned(), "SMTP Name".to_owned()))
        );
        let s = settings(&[("smtp_from_address", "smtp@example.com")]);
        assert_eq!(
            mail_from_identity(&s, false, false),
            Ok(("smtp@example.com".to_owned(), "Coolify".to_owned()))
        );
    }

    #[test]
    fn a_missing_sender_address_is_an_error() {
        let s = settings(&[("smtp_from_name", "Name")]);
        assert_eq!(
            mail_from_identity(&s, false, false),
            Err("Transactional email sender address is not configured.".to_owned())
        );
        // The Resend sender is only read when Resend is on.
        let s = settings(&[("resend_from_address", "resend@example.com")]);
        assert!(mail_from_identity(&s, false, false).is_err());
    }

    #[test]
    fn resend_has_its_own_sender_and_falls_back_to_the_smtp_sender() {
        let s = settings(&[
            ("smtp_from_address", "smtp@example.com"),
            ("smtp_from_name", "SMTP Name"),
            ("resend_from_address", "resend@example.com"),
            ("resend_from_name", "Resend Name"),
        ]);
        assert_eq!(
            mail_from_identity(&s, true, false),
            Ok(("resend@example.com".to_owned(), "Resend Name".to_owned()))
        );
        assert_eq!(
            mail_from_identity(&s, false, false),
            Ok(("smtp@example.com".to_owned(), "SMTP Name".to_owned()))
        );
        let s = settings(&[
            ("smtp_from_address", "smtp@example.com"),
            ("smtp_from_name", "SMTP Name"),
            ("resend_from_address", "  "),
        ]);
        assert_eq!(
            mail_from_identity(&s, true, false),
            Ok(("smtp@example.com".to_owned(), "SMTP Name".to_owned()))
        );
    }

    #[test]
    fn notifications_use_the_notification_sender_when_it_is_set() {
        let s = settings(&[
            ("smtp_from_address", "smtp@example.com"),
            ("smtp_from_name", "SMTP Name"),
            (
                "notification_from_address",
                "alerts@notifications.example.com",
            ),
            ("notification_from_name", " Alerts "),
        ]);
        assert_eq!(
            mail_from_identity(&s, false, true),
            Ok((
                "alerts@notifications.example.com".to_owned(),
                "Alerts".to_owned()
            ))
        );
        // Transactional email keeps the default sender.
        assert_eq!(
            mail_from_identity(&s, false, false),
            Ok(("smtp@example.com".to_owned(), "SMTP Name".to_owned()))
        );
        // Without a notification name, the name of the provider sender is used.
        let s = settings(&[
            ("smtp_from_address", "smtp@example.com"),
            ("smtp_from_name", "SMTP Name"),
            ("notification_from_address", "alerts@example.com"),
        ]);
        assert_eq!(
            mail_from_identity(&s, false, true),
            Ok(("alerts@example.com".to_owned(), "SMTP Name".to_owned()))
        );
        // Without a notification address, alerts use the default sender.
        let s = settings(&[
            ("smtp_from_address", "smtp@example.com"),
            ("notification_from_name", "Alerts"),
        ]);
        assert_eq!(
            mail_from_identity(&s, false, true),
            Ok(("smtp@example.com".to_owned(), "Coolify".to_owned()))
        );
        // The notification sender needs no default sender.
        let s = settings(&[("notification_from_address", "alerts@example.com")]);
        assert_eq!(
            mail_from_identity(&s, false, true),
            Ok(("alerts@example.com".to_owned(), "Coolify".to_owned()))
        );
    }

    #[test]
    fn presets() {
        assert_eq!(SMTP_PRESETS.len(), 12);
        let resend = smtp_preset("resend").unwrap();
        assert_eq!(
            (resend.host, resend.port, resend.encryption, resend.username),
            ("smtp.resend.com", 465, "tls", Some("resend"))
        );
        assert_eq!(smtp_preset("sendgrid").unwrap().username, Some("apikey"));
        assert!(smtp_preset("nope").is_none());
        assert!(smtp_requires_login(Some(" SMTP.Gmail.com ")));
        assert!(!smtp_requires_login(Some("mail.internal.example")));
        assert!(!smtp_requires_login(Some("")));
        assert!(!smtp_requires_login(None));
        let options = smtp_preset_options();
        assert_eq!(
            options[0],
            json!({"value": "postshiba", "label": "PostShiba"})
        );
        assert_eq!(options.as_array().unwrap().len(), 12);
    }

    fn model() -> Map<String, Value> {
        json!({
            "smtp_server_name": null,
            "smtp_servers": null,
            "smtp_from_address": "a@one.example",
            "smtp_from_name": "One",
            "smtp_host": "smtp.one.example",
            "smtp_port": 587,
            "smtp_encryption": "starttls",
            "smtp_username": "user-one",
            "smtp_password": "secret-one",
            "smtp_timeout": null,
            "smtp_ehlo_domain": null,
        })
        .as_object()
        .unwrap()
        .clone()
    }

    #[test]
    fn the_server_in_use_is_named_default_until_it_gets_a_name() {
        let mut m = model();
        assert_eq!(smtp_server_name(&m), "Default");
        m.insert("smtp_server_name".into(), json!("  "));
        assert_eq!(smtp_server_name(&m), "Default");
        m.insert("smtp_server_name".into(), json!("Production"));
        assert_eq!(smtp_server_name(&m), "Production");
        assert!(saved_smtp_servers(&m).is_empty());
    }

    #[test]
    fn keeping_the_active_server_saves_its_columns() {
        let mut m = model();
        SmtpServers(&mut m).keep_active("id-1");
        let saved = saved_smtp_servers(&m);
        assert_eq!(saved.len(), 1);
        let keys: Vec<&String> = saved[0].as_object().unwrap().keys().collect();
        assert_eq!(keys[0], "id");
        assert_eq!(keys[1], "name");
        assert_eq!(keys.len(), 2 + SMTP_SERVER_COLUMNS.len());
        assert_eq!(saved[0]["name"], "Default");
        assert_eq!(saved[0]["smtp_host"], "smtp.one.example");
        assert_eq!(saved[0]["smtp_port"], 587);
        assert_eq!(saved[0]["smtp_password"], "secret-one");
    }

    #[test]
    fn a_server_without_a_host_is_not_kept() {
        let mut m = model();
        m.insert("smtp_host".into(), Value::Null);
        SmtpServers(&mut m).keep_active("id-1");
        assert!(saved_smtp_servers(&m).is_empty());
    }

    #[test]
    fn switching_swaps_the_server_in_use_with_a_saved_one() {
        let mut m = model();
        // A new server: the server in use is kept, then the new values replace it.
        SmtpServers(&mut m).keep_active("id-1");
        m.insert("smtp_server_name".into(), json!("Two"));
        m.insert("smtp_host".into(), json!("smtp.two.example"));
        m.insert("smtp_from_address".into(), json!("b@two.example"));
        m.insert("smtp_password".into(), json!("secret-two"));

        assert!(!SmtpServers(&mut m).switch_to("missing", "id-2"));
        assert_eq!(m["smtp_host"], "smtp.two.example");

        assert!(SmtpServers(&mut m).switch_to("id-1", "id-2"));
        assert_eq!(m["smtp_host"], "smtp.one.example");
        assert_eq!(m["smtp_from_address"], "a@one.example");
        assert_eq!(m["smtp_password"], "secret-one");
        assert_eq!(m["smtp_server_name"], "Default");
        let saved = saved_smtp_servers(&m);
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0]["id"], "id-2");
        assert_eq!(saved[0]["name"], "Two");
        assert_eq!(saved[0]["smtp_host"], "smtp.two.example");
        assert_eq!(
            smtp_server_options(&m),
            json!([
                {"value": "active", "label": "Default"},
                {"value": "id-2", "label": "Two"},
            ])
        );
    }

    #[test]
    fn deleting_the_active_server_uses_the_first_saved_one() {
        let mut m = model();
        assert!(!SmtpServers(&mut m).delete_active());
        SmtpServers(&mut m).keep_active("id-1");
        m.insert("smtp_server_name".into(), json!("Two"));
        m.insert("smtp_host".into(), json!("smtp.two.example"));
        assert!(SmtpServers(&mut m).delete_active());
        assert_eq!(m["smtp_host"], "smtp.one.example");
        assert_eq!(m["smtp_server_name"], "Default");
        assert_eq!(m["smtp_servers"], json!([]));
        assert!(!SmtpServers(&mut m).delete_active());
    }

    #[test]
    fn hosts_are_normalized() {
        assert_eq!(normalize_smtp_host(" SMTP.Gmail.com. "), "smtp.gmail.com");
        assert_eq!(normalize_smtp_host("[2A01:4F8::1]"), "2a01:4f8::1");
        assert_eq!(normalize_smtp_host("192.168.1.10"), "192.168.1.10");
        assert_eq!(normalize_smtp_host("Mail_Server"), "mail_server");
    }

    #[test]
    fn host_syntax() {
        for ok in [
            "smtp.gmail.com",
            "mail_server",
            "10.0.0.5",
            "2a01:4f8::1",
            "a",
            "a-b.example",
        ] {
            assert!(is_valid_smtp_host(ok), "{ok}");
        }
        let long_label = "a".repeat(64);
        let long_host = format!("{}.example", "a.".repeat(130));
        for bad in [
            "",
            "-smtp.example",
            "smtp-.example",
            "smtp..example",
            "smtp.example/x",
            "user@smtp.example",
            "smtp example",
            "smtp.example:587",
            long_label.as_str(),
            long_host.as_str(),
        ] {
            assert!(!is_valid_smtp_host(bad), "{bad}");
        }
    }

    #[test]
    fn validation_messages() {
        let url = "http://coolify.example/settings/advanced#endpoint-section";
        assert_eq!(
            smtp_host_status_message(HostStatus::Allowed, true, url),
            None
        );
        assert_eq!(
            smtp_host_status_message(HostStatus::Unresolved, true, url).as_deref(),
            Some("The SMTP host could not be resolved.")
        );
        assert_eq!(
            smtp_host_status_message(HostStatus::LinkLocal, false, url).as_deref(),
            Some("The SMTP host must not point to link-local addresses.")
        );
        assert_eq!(
            smtp_host_status_message(HostStatus::Blocked, true, url).as_deref(),
            Some(
                "The SMTP host points to a local or private address that is not allowed. Configure allowed internal targets: http://coolify.example/settings/advanced#endpoint-section"
            )
        );
        assert_eq!(
            smtp_host_status_message(HostStatus::Blocked, false, url).as_deref(),
            Some(
                "The SMTP host points to a local or private address that is not allowed. Ask your Coolify administrator to allow this host."
            )
        );
    }

    #[test]
    fn the_connection_address_is_a_checked_address() {
        let v4: IpAddr = "93.184.216.34".parse().unwrap();
        let v6: IpAddr = "2606:2800:220:1::1".parse().unwrap();
        assert_eq!(
            connection_address_of("smtp.example.com", HostStatus::Allowed, &[v6, v4]).as_deref(),
            Ok("93.184.216.34")
        );
        assert_eq!(
            connection_address_of("smtp.example.com", HostStatus::Allowed, &[v6]).as_deref(),
            Ok("2606:2800:220:1::1")
        );
        // An IP host has no resolved addresses: connect to the host itself.
        assert_eq!(
            connection_address_of("203.0.114.9", HostStatus::Allowed, &[]).as_deref(),
            Ok("203.0.114.9")
        );
        let unresolved =
            connection_address_of("smtp.example.com", HostStatus::Unresolved, &[]).unwrap_err();
        assert_eq!(unresolved.kind, SmtpHostErrorKind::Unresolved);
        assert_eq!(unresolved.message, "The SMTP host could not be resolved.");
        for status in [HostStatus::Blocked, HostStatus::LinkLocal] {
            let blocked = connection_address_of("smtp.example.com", status, &[]).unwrap_err();
            assert_eq!(blocked.kind, SmtpHostErrorKind::NotAllowed);
            assert_eq!(
                blocked.message,
                "The SMTP host is not allowed: it points to a local or private address."
            );
        }
    }
}
