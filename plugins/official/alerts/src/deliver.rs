//! Which service gets which request for an alert, and the requests themselves.
//!
//! Incidents: PagerDuty and Opsgenie get an incident for every problem, keyed by [`Alert::key`], so repeats
//! of the same problem add to one incident. When a later event says the same resource is fine again (the
//! next deployment of the application, backup of the database or run of the task succeeded, the server is
//! reachable again), the incident is resolved, but only where this plugin opened it: the plugin remembers
//! the keys it opened ([`Open`]). Problems nothing in Coolify reports the end of (disk usage, Docker
//! cleanup, restart limits, expiring certificates, missing database backups) open incidents that are closed
//! by hand.
//!
//! ntfy and Google Chat get problems under "failed" and "all", successes under "all", and under "failed"
//! also the recovery of a problem they were told about.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::event::{Alert, Category, Incident, Severity};
use crate::text::head;

pub const NTFY_URL: &str = "https://ntfy.sh/";
pub const PAGERDUTY_URL: &str = "https://events.pagerduty.com/v2/enqueue";
pub const OPSGENIE_US: &str = "https://api.opsgenie.com";
pub const OPSGENIE_EU: &str = "https://api.eu.opsgenie.com";
/// Google Chat webhooks must start with this; anything else is refused before a request is made.
pub const GOOGLE_CHAT_PREFIX: &str = "https://chat.googleapis.com/";

/// The setting keys of the manifest.
pub mod keys {
    pub const NTFY_TOPIC: &str = "ntfy_topic";
    pub const NTFY_PRIORITY: &str = "ntfy_priority";
    pub const GOOGLE_CHAT: &str = "google_chat_webhook";
    pub const PAGERDUTY: &str = "pagerduty_routing_key";
    pub const OPSGENIE: &str = "opsgenie_api_key";
    pub const OPSGENIE_REGION: &str = "opsgenie_region";
    pub const COOLIFY_URL: &str = "coolify_url";
}

/// The services, in the order they are sent to: incident tools first, so a slow chat service never delays a
/// page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Service {
    Pagerduty,
    Opsgenie,
    Ntfy,
    GoogleChat,
}

impl Service {
    pub const ALL: [Service; 4] = [
        Self::Pagerduty,
        Self::Opsgenie,
        Self::Ntfy,
        Self::GoogleChat,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Self::Pagerduty => "pagerduty",
            Self::Opsgenie => "opsgenie",
            Self::Ntfy => "ntfy",
            Self::GoogleChat => "google_chat",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Pagerduty => "PagerDuty",
            Self::Opsgenie => "Opsgenie",
            Self::Ntfy => "ntfy",
            Self::GoogleChat => "Google Chat",
        }
    }

    fn is_incident_tool(self) -> bool {
        matches!(self, Self::Pagerduty | Self::Opsgenie)
    }
}

/// What to send for a category.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Off,
    /// Problems, and the recovery of problems that were sent.
    Failed,
    /// Successes too.
    All,
}

impl Mode {
    /// The setting's value; unset or unknown is "failed", the default.
    pub fn parse(v: Option<&str>) -> Mode {
        match v {
            Some("off") => Mode::Off,
            Some("all") => Mode::All,
            _ => Mode::Failed,
        }
    }
}

/// The team's settings, as the plugin reads them. Secrets are only ever put into requests.
#[derive(Debug, Clone, Default)]
pub struct Config {
    pub ntfy_topic: Option<String>,
    pub ntfy_priority: Option<String>,
    pub google_chat: Option<String>,
    pub pagerduty_key: Option<String>,
    pub opsgenie_key: Option<String>,
    pub opsgenie_region: Option<String>,
    pub coolify_url: Option<String>,
    pub modes: Vec<(Category, Mode)>,
}

impl Config {
    /// Reads every setting through `get` (`config::get` in the plugin).
    pub fn load(get: impl Fn(&str) -> Option<String>) -> Config {
        let get = |k: &str| get(k).filter(|v| !v.trim().is_empty());
        Config {
            ntfy_topic: get(keys::NTFY_TOPIC),
            ntfy_priority: get(keys::NTFY_PRIORITY),
            google_chat: get(keys::GOOGLE_CHAT),
            pagerduty_key: get(keys::PAGERDUTY),
            opsgenie_key: get(keys::OPSGENIE),
            opsgenie_region: get(keys::OPSGENIE_REGION),
            coolify_url: get(keys::COOLIFY_URL),
            modes: Category::ALL
                .iter()
                .map(|c| (*c, Mode::parse(get(c.setting()).as_deref())))
                .collect(),
        }
    }

    pub fn mode(&self, c: Category) -> Mode {
        self.modes
            .iter()
            .find(|(k, _)| *k == c)
            .map_or(Mode::Failed, |(_, m)| *m)
    }

    pub fn configured(&self, s: Service) -> bool {
        match s {
            Service::Pagerduty => self.pagerduty_key.is_some(),
            Service::Opsgenie => self.opsgenie_key.is_some(),
            Service::Ntfy => self.ntfy_topic.is_some(),
            Service::GoogleChat => self.google_chat.is_some(),
        }
    }

    fn opsgenie_base(&self) -> &'static str {
        match self.opsgenie_region.as_deref() {
            Some("eu") => OPSGENIE_EU,
            _ => OPSGENIE_US,
        }
    }

    /// ntfy's priority 1 to 5; "high" unless the team chose another.
    fn ntfy_priority(&self) -> u8 {
        match self.ntfy_priority.as_deref() {
            Some("min") => 1,
            Some("low") => 2,
            Some("default") => 3,
            Some("urgent") => 5,
            _ => 4,
        }
    }
}

/// One HTTP request for a service.
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    pub service: Service,
    pub method: &'static str,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Request {
    fn json(service: Service, url: String, body: &Value) -> Request {
        Request {
            service,
            method: "POST",
            url,
            headers: vec![("content-type".into(), "application/json".into())],
            body: serde_json::to_vec(body).unwrap_or_default(),
        }
    }

    /// The body, parsed: for tests.
    pub fn json_body(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or_default()
    }
}

/// What to do for one service.
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    Send(Request),
    /// The service is configured but can't be used; the reason never repeats the secret.
    Refused(Service, String),
}

impl Step {
    pub fn service(&self) -> Service {
        match self {
            Step::Send(r) => r.service,
            Step::Refused(s, _) => *s,
        }
    }
}

/// The incidents this plugin opened, by key, with the services that were told. Kept in storage.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Open(pub BTreeMap<String, OpenEntry>);

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct OpenEntry {
    pub services: Vec<Service>,
    /// Unix seconds when it was first opened.
    pub since: u64,
}

/// Most open incidents remembered per team; the oldest are forgotten first.
pub const MAX_OPEN: usize = 300;

impl Open {
    /// Records the result of sending `alert`: `sent` lists the services that accepted their request.
    /// Returns whether anything changed.
    pub fn record(&mut self, alert: &Alert, sent: &[Service], now: u64) -> bool {
        match alert.incident {
            Incident::Open => {
                if sent.is_empty() {
                    return false;
                }
                let entry = self.0.entry(alert.key.clone()).or_insert(OpenEntry {
                    services: Vec::new(),
                    since: now,
                });
                for s in sent {
                    if !entry.services.contains(s) {
                        entry.services.push(*s);
                    }
                }
                entry.services.sort();
                while self.0.len() > MAX_OPEN {
                    let oldest = self
                        .0
                        .iter()
                        .min_by_key(|(_, e)| e.since)
                        .map(|(k, _)| k.clone());
                    match oldest {
                        Some(k) => self.0.remove(&k),
                        None => break,
                    };
                }
                true
            }
            Incident::OpenOnly => false,
            Incident::Resolve => {
                let Some(entry) = self.0.get_mut(&alert.key) else {
                    return false;
                };
                entry.services.retain(|s| !sent.contains(s));
                if entry.services.is_empty() {
                    self.0.remove(&alert.key);
                }
                true
            }
        }
    }

    /// Forgets services the team no longer has configured, so their recoveries are not waited for.
    pub fn prune(&mut self, key: &str, cfg: &Config) -> bool {
        let Some(entry) = self.0.get_mut(key) else {
            return false;
        };
        let before = entry.services.len();
        entry.services.retain(|s| cfg.configured(*s));
        let changed = entry.services.len() != before;
        if entry.services.is_empty() {
            self.0.remove(key);
        }
        changed
    }
}

/// What to send for `alert`. `open` is what [`Open`] remembers for its key.
pub fn plan(cfg: &Config, alert: &Alert, open: Option<&OpenEntry>) -> Vec<Step> {
    let mode = cfg.mode(alert.category);
    if mode == Mode::Off {
        return Vec::new();
    }
    let told = |s: Service| open.is_some_and(|e| e.services.contains(&s));
    let mut steps = Vec::new();
    for s in Service::ALL {
        if !cfg.configured(s) {
            continue;
        }
        let wanted = match alert.incident {
            Incident::Open | Incident::OpenOnly => true,
            Incident::Resolve if s.is_incident_tool() => told(s),
            Incident::Resolve => mode == Mode::All || told(s),
        };
        if wanted {
            steps.push(step(cfg, s, alert));
        }
    }
    steps
}

/// The request for one configured service.
fn step(cfg: &Config, s: Service, alert: &Alert) -> Step {
    let none = String::new();
    match s {
        Service::Pagerduty => {
            let key = cfg.pagerduty_key.as_ref().unwrap_or(&none);
            Step::Send(match alert.incident {
                Incident::Resolve => pagerduty_resolve(key, &alert.key),
                _ => pagerduty_trigger(key, alert, cfg.coolify_url.as_deref()),
            })
        }
        Service::Opsgenie => {
            let key = cfg.opsgenie_key.as_ref().unwrap_or(&none);
            Step::Send(match alert.incident {
                Incident::Resolve => opsgenie_close(cfg.opsgenie_base(), key, alert),
                _ => opsgenie_create(cfg.opsgenie_base(), key, alert),
            })
        }
        Service::Ntfy => Step::Send(ntfy(
            cfg.ntfy_topic.as_ref().unwrap_or(&none),
            cfg.ntfy_priority(),
            alert,
        )),
        Service::GoogleChat => {
            match google_chat(cfg.google_chat.as_ref().unwrap_or(&none), alert) {
                Ok(r) => Step::Send(r),
                Err(e) => Step::Refused(s, e),
            }
        }
    }
}

/// The test alert of `POST /test`.
pub fn test_alert() -> Alert {
    Alert {
        category: Category::Deployments,
        severity: Severity::Info,
        incident: Incident::Open,
        key: "coolify-rs-test".into(),
        title: "Test alert from coolify-rs".into(),
        body: "The coolify-rs alerts plugin can reach this service. PagerDuty and Opsgenie resolve this \
               test right away."
            .into(),
        link: None,
        source: "coolify-rs".into(),
        details: Vec::new(),
    }
}

/// Every request `POST /test` makes, per configured service: incident tools open the test incident and
/// resolve it at once, so nobody stays paged.
pub fn plan_test(cfg: &Config) -> Vec<(Service, Vec<Step>)> {
    let alert = test_alert();
    let resolved = Alert {
        severity: Severity::Success,
        incident: Incident::Resolve,
        title: "Test alert resolved".into(),
        body: "The test alert is resolved.".into(),
        ..alert.clone()
    };
    Service::ALL
        .iter()
        .filter(|s| cfg.configured(**s))
        .map(|s| {
            let mut steps = vec![step(cfg, *s, &alert)];
            if s.is_incident_tool() {
                steps.push(step(cfg, *s, &resolved));
            }
            (*s, steps)
        })
        .collect()
}

/// ntfy.sh: a JSON message published to the server's root, so the topic (which is its password) travels in
/// the body, never in a URL.
pub fn ntfy(topic: &str, priority: u8, alert: &Alert) -> Request {
    let priority = if alert.severity.is_problem() {
        priority
    } else {
        priority.min(3)
    };
    let mut body = json!({
        "topic": topic,
        "title": alert.title,
        "message": head(&alert.body, 3000),
        "priority": priority,
        "tags": [alert.severity.ntfy_tag()],
    });
    if let Some(link) = &alert.link {
        body["click"] = json!(link);
    }
    Request::json(Service::Ntfy, NTFY_URL.into(), &body)
}

/// Google Chat: a text message to the space's incoming webhook. The webhook must be on chat.googleapis.com.
pub fn google_chat(webhook: &str, alert: &Alert) -> Result<Request, String> {
    let ok = webhook.starts_with(GOOGLE_CHAT_PREFIX)
        && webhook.len() > GOOGLE_CHAT_PREFIX.len()
        && !webhook
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || c == '\\');
    if !ok {
        return Err(
            "the Google Chat webhook URL must start with https://chat.googleapis.com/".into(),
        );
    }
    let icon = match alert.severity {
        Severity::Critical => "🔴",
        Severity::Error => "❌",
        Severity::Warning => "⚠️",
        Severity::Info => "ℹ️",
        Severity::Success => "✅",
    };
    let mut text = format!("{icon} *{}*\n{}", alert.title, head(&alert.body, 3000));
    if let Some(link) = &alert.link {
        text.push_str(&format!("\n<{link}|Open>"));
    }
    let mut r = Request::json(
        Service::GoogleChat,
        webhook.into(),
        &json!({ "text": text }),
    );
    r.headers = vec![(
        "content-type".into(),
        "application/json; charset=UTF-8".into(),
    )];
    Ok(r)
}

fn details(alert: &Alert) -> Map<String, Value> {
    let mut d = Map::new();
    d.insert("message".into(), json!(alert.body));
    for (k, v) in &alert.details {
        d.insert(k.clone(), json!(v));
    }
    if let Some(link) = &alert.link {
        d.insert("link".into(), json!(link));
    }
    d
}

/// PagerDuty Events API v2: trigger an alert deduplicated on the alert's key.
pub fn pagerduty_trigger(routing_key: &str, alert: &Alert, coolify_url: Option<&str>) -> Request {
    let mut body = json!({
        "routing_key": routing_key,
        "event_action": "trigger",
        "dedup_key": alert.key,
        "payload": {
            "summary": head(&alert.title, 1024),
            "source": alert.source,
            "severity": alert.severity.pagerduty(),
            "class": alert.category.name(),
            "custom_details": details(alert),
        },
        "client": "coolify-rs",
    });
    if let Some(url) = coolify_url {
        body["client_url"] = json!(url);
    }
    if let Some(link) = &alert.link {
        body["links"] = json!([{ "href": link, "text": "Open in Coolify" }]);
    }
    Request::json(Service::Pagerduty, PAGERDUTY_URL.into(), &body)
}

/// PagerDuty Events API v2: resolve the alert with this key.
pub fn pagerduty_resolve(routing_key: &str, key: &str) -> Request {
    let body = json!({
        "routing_key": routing_key,
        "event_action": "resolve",
        "dedup_key": key,
    });
    Request::json(Service::Pagerduty, PAGERDUTY_URL.into(), &body)
}

fn opsgenie_auth(r: &mut Request, api_key: &str) {
    r.headers
        .push(("authorization".into(), format!("GenieKey {api_key}")));
}

/// Opsgenie Alerts API: create an alert whose alias is the alert's key, so repeats add to it.
pub fn opsgenie_create(base: &str, api_key: &str, alert: &Alert) -> Request {
    let mut description = alert.body.clone();
    if let Some(link) = &alert.link {
        description.push_str(&format!("\n\n{link}"));
    }
    let details: Map<String, Value> = alert
        .details
        .iter()
        .map(|(k, v)| (k.clone(), json!(v)))
        .collect();
    let body = json!({
        "message": head(&alert.title, 130),
        "alias": alert.key,
        "description": head(&description, 15000),
        "priority": alert.severity.opsgenie(),
        "source": "coolify-rs",
        "entity": alert.source,
        "tags": ["coolify-rs", alert.category.name()],
        "details": details,
    });
    let mut r = Request::json(Service::Opsgenie, format!("{base}/v2/alerts"), &body);
    opsgenie_auth(&mut r, api_key);
    r
}

/// Opsgenie Alerts API: close the alert with this alias. Keys hold only `a-z0-9._-`, so the alias needs no
/// escaping in the path.
pub fn opsgenie_close(base: &str, api_key: &str, alert: &Alert) -> Request {
    let body = json!({
        "source": "coolify-rs",
        "note": head(&format!("{}: {}", alert.title, alert.body), 25000),
    });
    let mut r = Request::json(
        Service::Opsgenie,
        format!("{base}/v2/alerts/{}/close?identifierType=alias", alert.key),
        &body,
    );
    opsgenie_auth(&mut r, api_key);
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::alert;
    use serde_json::json;

    fn cfg(pairs: &[(&str, &str)]) -> Config {
        let map: BTreeMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        Config::load(|k| map.get(k).cloned())
    }

    fn everything() -> Config {
        cfg(&[
            ("ntfy_topic", "secret-topic"),
            (
                "google_chat_webhook",
                "https://chat.googleapis.com/v1/spaces/S/messages?key=K&token=T",
            ),
            ("pagerduty_routing_key", "R0UT1NG"),
            ("opsgenie_api_key", "GENIE"),
            ("opsgenie_region", "eu"),
            ("coolify_url", "https://coolify.example.com"),
        ])
    }

    fn unreachable() -> Alert {
        alert(
            "server.unreachable",
            &json!({"server": {"id": 1, "name": "web-1", "uuid": "srv1"}}),
            Some("https://coolify.example.com"),
        )
        .unwrap()
    }

    fn sends(steps: &[Step]) -> Vec<&Request> {
        steps
            .iter()
            .filter_map(|s| match s {
                Step::Send(r) => Some(r),
                Step::Refused(..) => None,
            })
            .collect()
    }

    #[test]
    fn nothing_without_settings_or_when_off() {
        let a = unreachable();
        assert!(plan(&Config::default(), &a, None).is_empty());
        let mut off = everything();
        off.modes = vec![(Category::Servers, Mode::Off)];
        assert!(plan(&off, &a, None).is_empty());
    }

    #[test]
    fn a_problem_goes_everywhere_in_order() {
        let steps = plan(&everything(), &unreachable(), None);
        let order: Vec<Service> = steps.iter().map(Step::service).collect();
        assert_eq!(order, Service::ALL.to_vec());
        let r = sends(&steps);

        assert_eq!(r[0].url, "https://events.pagerduty.com/v2/enqueue");
        assert_eq!(
            r[0].json_body(),
            json!({
                "routing_key": "R0UT1NG",
                "event_action": "trigger",
                "dedup_key": "coolify-rs-server-srv1",
                "payload": {
                    "summary": "Server unreachable: web-1",
                    "source": "web-1",
                    "severity": "critical",
                    "class": "servers",
                    "custom_details": {
                        "message": "coolify-rs cannot reach the server web-1. Its resources may be down.",
                        "server_uuid": "srv1",
                        "link": "https://coolify.example.com/server/srv1",
                    },
                },
                "client": "coolify-rs",
                "client_url": "https://coolify.example.com",
                "links": [{"href": "https://coolify.example.com/server/srv1", "text": "Open in Coolify"}],
            })
        );

        assert_eq!(r[1].url, "https://api.eu.opsgenie.com/v2/alerts");
        assert!(
            r[1].headers
                .contains(&("authorization".into(), "GenieKey GENIE".into()))
        );
        assert_eq!(
            r[1].json_body(),
            json!({
                "message": "Server unreachable: web-1",
                "alias": "coolify-rs-server-srv1",
                "description": "coolify-rs cannot reach the server web-1. Its resources may be down.\n\nhttps://coolify.example.com/server/srv1",
                "priority": "P1",
                "source": "coolify-rs",
                "entity": "web-1",
                "tags": ["coolify-rs", "servers"],
                "details": {"server_uuid": "srv1"},
            })
        );

        assert_eq!(r[2].url, "https://ntfy.sh/");
        assert_eq!(
            r[2].json_body(),
            json!({
                "topic": "secret-topic",
                "title": "Server unreachable: web-1",
                "message": "coolify-rs cannot reach the server web-1. Its resources may be down.",
                "priority": 4,
                "tags": ["rotating_light"],
                "click": "https://coolify.example.com/server/srv1",
            })
        );

        assert_eq!(
            r[3].url,
            "https://chat.googleapis.com/v1/spaces/S/messages?key=K&token=T"
        );
        assert_eq!(
            r[3].json_body(),
            json!({"text": "🔴 *Server unreachable: web-1*\ncoolify-rs cannot reach the server web-1. Its resources may be down.\n<https://coolify.example.com/server/srv1|Open>"})
        );
    }

    #[test]
    fn recoveries_resolve_only_what_was_opened() {
        let down = unreachable();
        let up = Alert {
            incident: Incident::Resolve,
            severity: Severity::Success,
            title: "Server reachable again: web-1".into(),
            body: "The server web-1 is reachable again.".into(),
            ..down.clone()
        };
        let cfg = everything();
        // Nothing was opened: under "failed", a success sends nothing at all.
        assert!(plan(&cfg, &up, None).is_empty());

        let mut open = Open::default();
        assert!(open.record(&down, &[Service::Pagerduty, Service::Ntfy], 100));
        let entry = open.0.get(&down.key).cloned();
        let steps = plan(&cfg, &up, entry.as_ref());
        let r = sends(&steps);
        assert_eq!(r.len(), 2, "{steps:?}");
        assert_eq!(
            r[0].json_body(),
            json!({"routing_key": "R0UT1NG", "event_action": "resolve", "dedup_key": "coolify-rs-server-srv1"})
        );
        assert_eq!(r[1].service, Service::Ntfy);
        assert_eq!(r[1].json_body()["tags"], json!(["white_check_mark"]));
        assert_eq!(r[1].json_body()["priority"], json!(3));

        // A resolve that worked forgets the incident; one that failed keeps it for the next success.
        assert!(open.record(&up, &[Service::Ntfy], 200));
        assert_eq!(open.0[&down.key].services, vec![Service::Pagerduty]);
        assert!(open.record(&up, &[Service::Pagerduty], 300));
        assert!(open.0.is_empty());

        // Opsgenie closes by alias.
        let mut open = Open::default();
        open.record(&down, &[Service::Opsgenie], 1);
        let steps = plan(&cfg, &up, open.0.get(&down.key));
        let r = sends(&steps);
        assert_eq!(
            r[0].url,
            "https://api.eu.opsgenie.com/v2/alerts/coolify-rs-server-srv1/close?identifierType=alias"
        );
        assert_eq!(r[0].json_body()["source"], "coolify-rs");

        // Under "all", chat services hear about every success.
        let mut all = cfg.clone();
        all.modes = vec![(Category::Servers, Mode::All)];
        let services: Vec<Service> = plan(&all, &up, None).iter().map(Step::service).collect();
        assert_eq!(services, vec![Service::Ntfy, Service::GoogleChat]);
    }

    #[test]
    fn open_incidents_are_bounded_and_pruned() {
        let mut open = Open::default();
        let mut a = unreachable();
        for i in 0..(MAX_OPEN + 5) {
            a.key = format!("k{i}");
            open.record(&a, &[Service::Pagerduty], i as u64);
        }
        assert_eq!(open.0.len(), MAX_OPEN);
        assert!(!open.0.contains_key("k0") && open.0.contains_key(&format!("k{}", MAX_OPEN + 4)));
        // Only-open problems are never remembered.
        let disk = Alert {
            incident: Incident::OpenOnly,
            key: "disk".into(),
            ..a.clone()
        };
        assert!(!open.record(&disk, &[Service::Pagerduty], 1));
        // A service that is no longer configured is forgotten.
        let key = format!("k{}", MAX_OPEN + 4);
        assert!(open.prune(&key, &cfg(&[("ntfy_topic", "t")])));
        assert!(!open.0.contains_key(&key));
    }

    #[test]
    fn google_chat_only_on_its_own_host() {
        let a = unreachable();
        for bad in [
            "https://evil.example.com/v1/spaces/S/messages",
            "https://chat.googleapis.com.evil.example.com/v1",
            "https://chat.googleapis.com@evil.example.com/v1",
            "http://chat.googleapis.com/v1/spaces/S",
            "https://chat.googleapis.com:8443/v1/spaces/S",
            "https://chat.googleapis.com/",
            "https://chat.googleapis.com/v1 x",
        ] {
            let err = google_chat(bad, &a).unwrap_err();
            // The same fixed reason every time: it never repeats the URL, which holds the space's key.
            assert_eq!(
                err, "the Google Chat webhook URL must start with https://chat.googleapis.com/",
                "{bad}"
            );
        }
        let c = cfg(&[("google_chat_webhook", "https://evil.example.com/hook")]);
        assert_eq!(
            plan(&c, &a, None),
            vec![Step::Refused(
                Service::GoogleChat,
                "the Google Chat webhook URL must start with https://chat.googleapis.com/".into()
            )]
        );
    }

    #[test]
    fn settings_defaults_and_priorities() {
        let c = cfg(&[
            ("send_deployments", "all"),
            ("send_backups", "off"),
            ("ntfy_priority", "min"),
        ]);
        assert_eq!(c.mode(Category::Deployments), Mode::All);
        assert_eq!(c.mode(Category::Backups), Mode::Off);
        assert_eq!(c.mode(Category::Tasks), Mode::Failed);
        assert_eq!(c.opsgenie_base(), OPSGENIE_US);
        assert_eq!(c.ntfy_priority(), 1);
        assert_eq!(Config::default().ntfy_priority(), 4);
        // Blank values are unset.
        assert!(!cfg(&[("ntfy_topic", "  ")]).configured(Service::Ntfy));
    }

    #[test]
    fn the_test_resolves_incidents_right_away() {
        let plan = plan_test(&everything());
        let counts: Vec<(Service, usize)> =
            plan.iter().map(|(s, steps)| (*s, steps.len())).collect();
        assert_eq!(
            counts,
            vec![
                (Service::Pagerduty, 2),
                (Service::Opsgenie, 2),
                (Service::Ntfy, 1),
                (Service::GoogleChat, 1)
            ]
        );
        let Step::Send(resolve) = &plan[0].1[1] else {
            panic!()
        };
        assert_eq!(resolve.json_body()["event_action"], "resolve");
        let Step::Send(trigger) = &plan[0].1[0] else {
            panic!()
        };
        assert_eq!(trigger.json_body()["payload"]["severity"], "info");
        assert!(plan_test(&Config::default()).is_empty());
    }
}
