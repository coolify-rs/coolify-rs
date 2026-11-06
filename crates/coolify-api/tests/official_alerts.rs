//! The first-party plugins `coolify-rs/alerts` and `coolify-rs/healthchecks` end to end: built from
//! `plugins/official`, installed with their grant, switched on, configured through the settings API, and fed
//! events. A local stand-in answers for ntfy.sh, Google Chat, PagerDuty, Opsgenie and Healthchecks.io: the
//! plugins use the services' real URLs, and the egress test routes (tests only, with `allow_local`) send those
//! host names to the stand-in, which records every request. Nothing reaches the real services.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use base64::Engine as _;
use common::Harness;
use coolify_plugins::egress::Egress;
use coolify_plugins::{Limits, Settings};
use serde_json::{Value, json};

const ALERTS: &str = "coolify-rs/alerts";
const HEALTHCHECKS: &str = "coolify-rs/healthchecks";
const HOSTS: &[&str] = &[
    "ntfy.sh",
    "chat.googleapis.com",
    "events.pagerduty.com",
    "api.opsgenie.com",
    "api.eu.opsgenie.com",
    "hc-ping.com",
];

/// Every secret below contains this, so one search finds any of them.
const MARK: &str = "s3cret";
const NTFY_TOPIC: &str = "ntfy-topic-s3cret-1";
const CHAT: &str = "https://chat.googleapis.com/v1/spaces/AAAA/messages?key=chat-key-s3cret&token=chat-token-s3cret";
const PAGERDUTY: &str = "pd-routing-s3cret-0123456789";
const OPSGENIE: &str = "og-genie-s3cret-key";
const PING_KEY: &str = "hc_ping-s3cret-K3y";

fn plugins_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugins")
}

/// A component of `plugins/`, built from source the first time.
fn component(file: &str) -> String {
    static BUILT: OnceLock<()> = OnceLock::new();
    BUILT.get_or_init(|| {
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let status = std::process::Command::new(cargo)
            .args(["build", "-q", "--release", "--target", "wasm32-wasip2"])
            .current_dir(plugins_dir())
            .status()
            .unwrap();
        assert!(status.success(), "building plugins/ failed");
    });
    let wasm =
        std::fs::read(plugins_dir().join(format!("target/wasm32-wasip2/release/{file}.wasm")))
            .unwrap();
    base64::engine::general_purpose::STANDARD.encode(wasm)
}

/// One request the stand-in received.
#[derive(Debug, Clone)]
struct Seen {
    method: String,
    /// The `Host` header without the stand-in's port: the service the plugin addressed.
    host: String,
    /// Path and query.
    path: String,
    headers: HeaderMap,
    body: String,
}

impl Seen {
    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or_else(|e| panic!("{e}: {}", self.body))
    }

    fn header(&self, name: &str) -> &str {
        self.headers
            .get(name)
            .map_or("", |v| v.to_str().unwrap_or_default())
    }
}

/// The local stand-in for every service.
#[derive(Clone, Default)]
struct StandIn {
    seen: Arc<Mutex<Vec<Seen>>>,
    /// Hosts that take 30 s to answer.
    slow: Arc<Mutex<Vec<String>>>,
    addr: Option<SocketAddr>,
}

impl StandIn {
    async fn start() -> StandIn {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let state = StandIn {
            addr: Some(addr),
            ..StandIn::default()
        };
        let app = axum::Router::new()
            .fallback(record)
            .with_state(state.clone());
        tokio::spawn(async move {
            axum::serve(listener, app.into_make_service())
                .await
                .unwrap();
        });
        state
    }

    /// Plugin host settings whose plugins reach this stand-in for every service.
    fn settings(&self, limits: Limits) -> Settings {
        let addr = self.addr.unwrap();
        Settings {
            limits,
            egress: Egress {
                allow_local: true,
                test_routes: HOSTS.iter().map(|h| ((*h).to_owned(), addr)).collect(),
                ..Egress::default()
            },
            ..Settings::default()
        }
    }

    /// The requests received since the last call.
    fn take(&self) -> Vec<Seen> {
        std::mem::take(&mut *self.seen.lock().unwrap())
    }
}

async fn record(
    State(s): State<StandIn>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> (StatusCode, String) {
    let host = headers
        .get("host")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .split(':')
        .next()
        .unwrap_or_default()
        .to_owned();
    s.seen.lock().unwrap().push(Seen {
        method: method.to_string(),
        host: host.clone(),
        path: uri
            .path_and_query()
            .map(|p| p.as_str().to_owned())
            .unwrap_or_default(),
        headers,
        body: String::from_utf8_lossy(&body).into_owned(),
    });
    let slow = s.slow.lock().unwrap().contains(&host);
    if slow {
        tokio::time::sleep(Duration::from_secs(30)).await;
    }
    (StatusCode::ACCEPTED, r#"{"status":"success"}"#.into())
}

/// Log lines written while the guard lives, on this thread (the tests run on one).
#[derive(Clone, Default)]
struct Logs(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Logs {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Logs {
    type Writer = Logs;
    fn make_writer(&'a self) -> Logs {
        self.clone()
    }
}

impl Logs {
    fn capture(&self) -> tracing::subscriber::DefaultGuard {
        use tracing_subscriber::util::SubscriberInitExt;
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .with_ansi(false)
            .with_writer(self.clone())
            .set_default()
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

/// Installs a plugin from `plugins/official/<dir>` with the grant it asks for, and switches it on for Acme.
async fn install(h: &Harness, dir: &str, file: &str) {
    let manifest =
        std::fs::read_to_string(plugins_dir().join(format!("official/{dir}/plugin.toml"))).unwrap();
    let mut body = json!({"manifest": manifest, "component": component(file)});
    let asked = h
        .call(
            "POST",
            "/api/v1/plugins",
            Some("root_root"),
            Some(body.clone()),
        )
        .await;
    assert_eq!(asked.status, 422, "{}", asked.body);
    body["grant"] = asked.json()["capabilities"].clone();
    let r = h
        .call("POST", "/api/v1/plugins", Some("root_root"), Some(body))
        .await;
    assert_eq!(r.status, 201, "{}", r.body);
    let id = r.json()["id"].as_str().unwrap().to_owned();
    let r = h
        .call(
            "POST",
            &format!("/api/v1/plugins/{id}/enable"),
            Some("acme_owner_write"),
            None,
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
}

/// The answers the tests got back, to check that no secret is in any of them.
#[derive(Default)]
struct Answers(Vec<String>);

impl Answers {
    async fn call(
        &mut self,
        h: &Harness,
        method: &str,
        path: &str,
        token: &str,
        body: Option<Value>,
    ) -> common::Resp {
        let r = h.call(method, path, Some(token), body).await;
        self.0.push(r.body.clone());
        r
    }

    async fn configure(&mut self, h: &Harness, id: &str, settings: Value) {
        let r = self
            .call(
                h,
                "PATCH",
                &format!("/api/v1/plugins/{id}/settings"),
                "acme_owner_write",
                Some(settings),
            )
            .await;
        assert_eq!(r.status, 200, "{}", r.body);
    }

    /// Delivers one event to the plugin for Acme and answers `{"delivered": ..., "error": ...}`.
    async fn event(&mut self, h: &Harness, id: &str, kind: &str, payload: Value) -> Value {
        let r = self
            .call(
                h,
                "POST",
                &format!("/api/v1/plugins/{id}/test-event"),
                "acme_owner_write",
                Some(json!({"kind": kind, "payload": payload})),
            )
            .await;
        assert_eq!(r.status, 200, "{}", r.body);
        r.json()
    }

    #[track_caller]
    fn assert_no_secrets(&self) {
        for body in &self.0 {
            assert!(!body.contains(MARK), "a secret was answered: {body}");
        }
    }
}

/// Nothing secret in the plugin audit log or in the plugins' own storage.
async fn assert_no_secrets_stored(h: &Harness) {
    let audit: Vec<String> =
        sqlx::query_scalar("SELECT action || ' ' || detail::text FROM coolify_rs_plugins.audit")
            .fetch_all(h.tdb.db.pool())
            .await
            .unwrap();
    assert!(!audit.is_empty());
    for row in audit {
        assert!(!row.contains(MARK), "audit: {row}");
    }
    let stored: Vec<Vec<u8>> = sqlx::query_scalar("SELECT value FROM coolify_rs_plugins.storage")
        .fetch_all(h.tdb.db.pool())
        .await
        .unwrap();
    for value in stored {
        let text = String::from_utf8_lossy(&value);
        assert!(!text.contains(MARK), "storage: {text}");
    }
}

fn server() -> Value {
    json!({"server": {"id": 3, "name": "web-1", "uuid": "srv1"}})
}

fn deployment() -> Value {
    json!({
        "application_name": "api", "application_uuid": "app1", "deployment_uuid": "dep1",
        "project_name": "Shop", "project_uuid": "p1", "environment_name": "production",
        "environment_uuid": "e1", "fqdn": "https://api.example.com", "preview": null,
        "commit": {"sha": "0123456789abcdef", "message": "Fix the cart", "url": null, "branch": "main"}
    })
}

fn hosts(seen: &[Seen]) -> Vec<&str> {
    seen.iter().map(|s| s.host.as_str()).collect()
}

#[tokio::test]
async fn alerts_send_each_service_exactly_its_request() {
    let logs = Logs::default();
    let _guard = logs.capture();
    let stand_in = StandIn::start().await;
    let h = Harness::with_plugin_settings(stand_in.settings(Limits::default())).await;
    let mut a = Answers::default();
    install(&h, "alerts", "alerts").await;

    // Nothing configured: the event is handled and nothing is sent.
    let r = a.event(&h, ALERTS, "server.unreachable", server()).await;
    assert_eq!(r, json!({"delivered": true}));
    assert!(stand_in.take().is_empty());

    a.configure(
        &h,
        ALERTS,
        json!({
            "ntfy_topic": NTFY_TOPIC, "google_chat_webhook": CHAT, "pagerduty_routing_key": PAGERDUTY,
            "opsgenie_api_key": OPSGENIE, "opsgenie_region": "eu", "coolify_url": "https://coolify.example.com",
        }),
    )
    .await;

    // A server goes away: an incident in PagerDuty and Opsgenie, a message on ntfy and Google Chat.
    let r = a.event(&h, ALERTS, "server.unreachable", server()).await;
    assert_eq!(r, json!({"delivered": true}));
    let seen = stand_in.take();
    assert_eq!(
        hosts(&seen),
        [
            "events.pagerduty.com",
            "api.eu.opsgenie.com",
            "ntfy.sh",
            "chat.googleapis.com"
        ]
    );
    let link = "https://coolify.example.com/server/srv1";
    let summary = "coolify-rs cannot reach the server web-1. Its resources may be down.";

    let pd = &seen[0];
    assert_eq!(
        (pd.method.as_str(), pd.path.as_str()),
        ("POST", "/v2/enqueue")
    );
    assert_eq!(pd.header("content-type"), "application/json");
    assert_eq!(
        pd.json(),
        json!({
            "routing_key": PAGERDUTY,
            "event_action": "trigger",
            "dedup_key": "coolify-rs-server-srv1",
            "payload": {
                "summary": "Server unreachable: web-1",
                "source": "web-1",
                "severity": "critical",
                "class": "servers",
                "custom_details": {"message": summary, "server_uuid": "srv1", "link": link},
            },
            "client": "coolify-rs",
            "client_url": "https://coolify.example.com",
            "links": [{"href": link, "text": "Open in Coolify"}],
        })
    );

    let og = &seen[1];
    assert_eq!(
        (og.method.as_str(), og.path.as_str()),
        ("POST", "/v2/alerts")
    );
    assert_eq!(og.header("authorization"), format!("GenieKey {OPSGENIE}"));
    assert_eq!(
        og.json(),
        json!({
            "message": "Server unreachable: web-1",
            "alias": "coolify-rs-server-srv1",
            "description": format!("{summary}\n\n{link}"),
            "priority": "P1",
            "source": "coolify-rs",
            "entity": "web-1",
            "tags": ["coolify-rs", "servers"],
            "details": {"server_uuid": "srv1"},
        })
    );

    // ntfy: the topic travels in the body, not in the URL.
    let ntfy = &seen[2];
    assert_eq!((ntfy.method.as_str(), ntfy.path.as_str()), ("POST", "/"));
    assert_eq!(ntfy.header("authorization"), "");
    assert_eq!(
        ntfy.json(),
        json!({
            "topic": NTFY_TOPIC,
            "title": "Server unreachable: web-1",
            "message": summary,
            "priority": 4,
            "tags": ["rotating_light"],
            "click": link,
        })
    );

    let chat = &seen[3];
    assert_eq!(
        (chat.method.as_str(), chat.path.as_str()),
        (
            "POST",
            "/v1/spaces/AAAA/messages?key=chat-key-s3cret&token=chat-token-s3cret"
        )
    );
    assert_eq!(
        chat.header("content-type"),
        "application/json; charset=UTF-8"
    );
    assert_eq!(
        chat.json(),
        json!({"text": format!("🔴 *Server unreachable: web-1*\n{summary}\n<{link}|Open>")})
    );

    // It comes back: the incidents are resolved, and the chats hear about it.
    let r = a.event(&h, ALERTS, "server.reachable", server()).await;
    assert_eq!(r, json!({"delivered": true}));
    let seen = stand_in.take();
    assert_eq!(
        hosts(&seen),
        [
            "events.pagerduty.com",
            "api.eu.opsgenie.com",
            "ntfy.sh",
            "chat.googleapis.com"
        ]
    );
    assert_eq!(
        seen[0].json(),
        json!({"routing_key": PAGERDUTY, "event_action": "resolve", "dedup_key": "coolify-rs-server-srv1"})
    );
    assert_eq!(
        seen[1].path,
        "/v2/alerts/coolify-rs-server-srv1/close?identifierType=alias"
    );
    assert_eq!(
        seen[1].header("authorization"),
        format!("GenieKey {OPSGENIE}")
    );
    assert_eq!(
        seen[1].json(),
        json!({"source": "coolify-rs", "note": "Server reachable again: web-1: The server web-1 is reachable again."})
    );
    assert_eq!(seen[2].json()["title"], "Server reachable again: web-1");
    assert_eq!(seen[2].json()["tags"], json!(["white_check_mark"]));
    assert_eq!(seen[2].json()["priority"], 3);

    // Once resolved, another "reachable" has nothing to resolve; under "failed", successes nobody was told
    // about send nothing.
    a.event(&h, ALERTS, "server.reachable", server()).await;
    a.event(&h, ALERTS, "deployment.succeeded", deployment())
        .await;
    assert!(stand_in.take().is_empty());

    // A failed deployment: the failure's key is the application's, and its link the deployment's page.
    a.event(&h, ALERTS, "deployment.failed", deployment()).await;
    let seen = stand_in.take();
    assert_eq!(seen.len(), 4);
    assert_eq!(seen[0].json()["dedup_key"], "coolify-rs-deployment-app1");
    assert_eq!(
        seen[3].json()["text"],
        "❌ *Deployment failed: api*\nThe deployment of api in Shop/production failed.\nCommit 0123456: Fix the cart\n<https://coolify.example.com/project/p1/environment/e1/application/app1/deployment/dep1|Open>"
    );

    // Categories switched off send nothing; "all" sends successes to the chats too.
    a.configure(
        &h,
        ALERTS,
        json!({"send_servers": "off", "send_deployments": "all"}),
    )
    .await;
    a.event(&h, ALERTS, "server.unreachable", server()).await;
    assert!(stand_in.take().is_empty());
    a.event(&h, ALERTS, "deployment.succeeded", deployment())
        .await;
    let seen = stand_in.take();
    // The failure above is resolved too.
    assert_eq!(
        hosts(&seen),
        [
            "events.pagerduty.com",
            "api.eu.opsgenie.com",
            "ntfy.sh",
            "chat.googleapis.com"
        ]
    );
    assert_eq!(seen[0].json()["event_action"], "resolve");
    assert_eq!(seen[2].json()["title"], "Deployed: api");

    // The status never shows a value, and only owners and admins can send a test alert.
    let r = a
        .call(
            &h,
            "GET",
            "/api/v1/ext/coolify-rs/alerts/status",
            "acme_member_read",
            None,
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    let status = r.json();
    assert!(
        status["services"]
            .as_array()
            .unwrap()
            .iter()
            .all(|s| s["configured"] == true),
        "{status}"
    );
    assert_eq!(status["send"]["servers"], "off");
    for (token, button) in [("acme_member_read", false), ("acme_owner_root", true)] {
        let r = a
            .call(
                &h,
                "GET",
                "/api/v1/ext/coolify-rs/alerts/panel",
                token,
                None,
            )
            .await;
        assert_eq!(r.status, 200, "{}", r.body);
        assert!(
            r.body
                .contains("<b>PagerDuty</b> <span class=\"muted\">configured</span>")
        );
        assert_eq!(r.body.contains("Send test alert"), button, "{token}");
    }
    let r = a
        .call(
            &h,
            "POST",
            "/api/v1/ext/coolify-rs/alerts/test",
            "acme_member_read",
            None,
        )
        .await;
    assert_eq!(r.status, 403, "{}", r.body);
    assert!(stand_in.take().is_empty());

    // The test alert reaches every service, and PagerDuty and Opsgenie resolve it at once.
    let r = a
        .call(
            &h,
            "POST",
            "/api/v1/ext/coolify-rs/alerts/test",
            "acme_owner_root",
            None,
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(
        r.json(),
        json!({"results": [
            {"service": "pagerduty", "label": "PagerDuty", "configured": true, "ok": true, "detail": "HTTP 202"},
            {"service": "opsgenie", "label": "Opsgenie", "configured": true, "ok": true, "detail": "HTTP 202"},
            {"service": "ntfy", "label": "ntfy", "configured": true, "ok": true, "detail": "HTTP 202"},
            {"service": "google_chat", "label": "Google Chat", "configured": true, "ok": true, "detail": "HTTP 202"},
        ]})
    );
    let seen = stand_in.take();
    let calls: Vec<(&str, &str)> = seen
        .iter()
        .map(|s| (s.host.as_str(), s.path.as_str()))
        .collect();
    assert_eq!(
        calls,
        [
            ("events.pagerduty.com", "/v2/enqueue"),
            ("events.pagerduty.com", "/v2/enqueue"),
            ("api.eu.opsgenie.com", "/v2/alerts"),
            (
                "api.eu.opsgenie.com",
                "/v2/alerts/coolify-rs-test/close?identifierType=alias"
            ),
            ("ntfy.sh", "/"),
            (
                "chat.googleapis.com",
                "/v1/spaces/AAAA/messages?key=chat-key-s3cret&token=chat-token-s3cret"
            ),
        ]
    );
    assert_eq!(seen[0].json()["event_action"], "trigger");
    assert_eq!(seen[0].json()["payload"]["severity"], "info");
    assert_eq!(seen[1].json()["event_action"], "resolve");
    assert_eq!(seen[2].json()["priority"], "P5");

    // A Google Chat URL on another host is refused by the plugin before any request; the others still go.
    a.configure(
        &h,
        ALERTS,
        json!({"google_chat_webhook": "https://hooks.example.com/v1/spaces/s3cret"}),
    )
    .await;
    let r = a.event(&h, ALERTS, "deployment.failed", deployment()).await;
    assert_eq!(r["delivered"], false, "{r}");
    let error = r["error"].as_str().unwrap();
    assert!(
        error.ends_with(
            "Google Chat: the Google Chat webhook URL must start with https://chat.googleapis.com/"
        ),
        "{error}"
    );
    assert_eq!(
        hosts(&stand_in.take()),
        ["events.pagerduty.com", "api.eu.opsgenie.com", "ntfy.sh"]
    );

    // The same through the dispatcher, as a real notification does: the failure is logged, without secrets.
    let host = h.state.plugins.clone().unwrap();
    host.dispatch(
        1,
        "scheduled_task.failed",
        json!({"name": "nightly", "uuid": "t1", "output": "exit 1", "resource": null}),
    );
    let mut waited = 0;
    while stand_in.seen.lock().unwrap().len() < 3 && waited < 100 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        waited += 1;
    }
    assert_eq!(stand_in.take().len(), 3);
    tokio::time::sleep(Duration::from_millis(100)).await;
    let text = logs.text();
    assert!(text.contains("plugin event failed"), "{text}");
    assert!(!text.contains(MARK), "a secret was logged: {text}");

    a.assert_no_secrets();
    assert_no_secrets_stored(&h).await;
    h.finish().await;
}

#[tokio::test]
async fn a_slow_service_fails_its_request_not_the_event() {
    let stand_in = StandIn::start().await;
    let limits = Limits {
        event_deadline: Duration::from_secs(2),
        // One timeout would switch the plugin off.
        max_failures: 1,
        ..Limits::default()
    };
    let h = Harness::with_plugin_settings(stand_in.settings(limits)).await;
    let mut a = Answers::default();
    install(&h, "alerts", "alerts").await;
    a.configure(
        &h,
        ALERTS,
        json!({"ntfy_topic": NTFY_TOPIC, "google_chat_webhook": CHAT, "pagerduty_routing_key": PAGERDUTY}),
    )
    .await;
    stand_in.slow.lock().unwrap().push("ntfy.sh".into());

    let started = std::time::Instant::now();
    let r = a.event(&h, ALERTS, "server.unreachable", server()).await;
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
    // PagerDuty went first and got through; ntfy timed out; Google Chat had no time left. The plugin
    // returned that as its error: the call itself did not time out.
    assert_eq!(r["delivered"], false, "{r}");
    let error = r["error"].as_str().unwrap();
    assert!(error.contains("returned an error"), "{error}");
    assert!(
        error.contains("ntfy: ") && error.contains("Google Chat: "),
        "{error}"
    );
    assert!(!error.contains("PagerDuty"), "{error}");
    assert_eq!(hosts(&stand_in.take()), ["events.pagerduty.com", "ntfy.sh"]);

    // The plugin is still on, and the incident it opened is remembered: the recovery resolves it.
    stand_in.slow.lock().unwrap().clear();
    let r = a.event(&h, ALERTS, "server.reachable", server()).await;
    assert_eq!(r, json!({"delivered": true}));
    let seen = stand_in.take();
    assert_eq!(hosts(&seen), ["events.pagerduty.com"]);
    assert_eq!(seen[0].json()["event_action"], "resolve");
    a.assert_no_secrets();
    h.finish().await;
}

#[tokio::test]
async fn healthchecks_ping_each_job() {
    let logs = Logs::default();
    let _guard = logs.capture();
    let stand_in = StandIn::start().await;
    let h = Harness::with_plugin_settings(stand_in.settings(Limits::default())).await;
    let mut a = Answers::default();
    install(&h, "healthchecks", "healthchecks").await;

    let task = |output: &str| {
        json!({"name": "nightly report", "uuid": "t1", "output": output,
               "resource": {"is_application": true, "uuid": "app1", "project_uuid": "p", "environment_uuid": "e"}})
    };
    // No ping key: nothing is sent, and the settings say the plugin needs configuration.
    let r = a
        .event(&h, HEALTHCHECKS, "scheduled_task.succeeded", task(""))
        .await;
    assert_eq!(r, json!({"delivered": true}));
    assert!(stand_in.take().is_empty());
    let r = a
        .call(
            &h,
            "GET",
            "/api/v1/plugins/coolify-rs/healthchecks/settings",
            "acme_owner_read",
            None,
        )
        .await;
    assert_eq!(r.json()["needs_configuration"], true);

    a.configure(
        &h,
        HEALTHCHECKS,
        json!({"ping_key": PING_KEY, "slug_prefix": "Acme Prod"}),
    )
    .await;

    // A task's success pings its check, creating it.
    a.event(&h, HEALTHCHECKS, "scheduled_task.succeeded", task("done"))
        .await;
    let seen = stand_in.take();
    assert_eq!(seen.len(), 1);
    assert_eq!(
        (
            seen[0].method.as_str(),
            seen[0].host.as_str(),
            seen[0].path.as_str()
        ),
        (
            "POST",
            "hc-ping.com",
            "/hc_ping-s3cret-K3y/acme-prod-task-t1?create=1"
        )
    );
    assert_eq!(seen[0].header("content-type"), "text/plain; charset=utf-8");
    assert_eq!(seen[0].body, "Task nightly report succeeded.");

    // A failure pings /fail with the end of the output.
    let output = format!("{}\nTraceback: boom", "x".repeat(3000));
    a.event(&h, HEALTHCHECKS, "scheduled_task.failed", task(&output))
        .await;
    let seen = stand_in.take();
    assert_eq!(
        seen[0].path,
        "/hc_ping-s3cret-K3y/acme-prod-task-t1/fail?create=1"
    );
    assert!(seen[0].body.starts_with("Task nightly report failed.\n\n…"));
    assert!(seen[0].body.ends_with("Traceback: boom"));
    assert!(seen[0].body.chars().count() < 1100);

    // Database backups: one check per database; a missing backup fails the checks of that database.
    let backup = json!({"name": "pg", "uuid": "db1", "project_uuid": "p", "environment_uuid": "e",
                        "database_name": "shop", "frequency": "daily"});
    a.configure(&h, HEALTHCHECKS, json!({"create": "no"})).await;
    a.event(&h, HEALTHCHECKS, "backup.succeeded", backup.clone())
        .await;
    a.event(
        &h,
        HEALTHCHECKS,
        "backup.missing",
        json!({"database_name": "pg", "backup_uuid": "sched1", "days": 2, "last_execution_at": null}),
    )
    .await;
    let seen = stand_in.take();
    let paths: Vec<&str> = seen.iter().map(|s| s.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "/hc_ping-s3cret-K3y/acme-prod-backup-db1-shop",
            "/hc_ping-s3cret-K3y/acme-prod-backup-db1-shop/fail"
        ]
    );
    assert_eq!(seen[1].body, "No backup of pg has run in the last 2 days.");

    // Storage backups.
    let volume = json!({"resource_name": "files", "target": "Volume app_data", "frequency": "daily",
                        "url": "https://coolify.example.com/project/p/environment/e/application/app9"});
    a.event(
        &h,
        HEALTHCHECKS,
        "volume_backup.failed",
        json!({"backup": volume, "output": "tar: error"}),
    )
    .await;
    let seen = stand_in.take();
    assert_eq!(
        seen[0].path,
        "/hc_ping-s3cret-K3y/acme-prod-volume-app9-volume-app_data/fail"
    );
    assert_eq!(
        seen[0].body,
        "Storage backup of Volume app_data (files) failed.\n\ntar: error"
    );

    // The panel's data: the checks pinged, newest first, never the key.
    let r = a
        .call(
            &h,
            "GET",
            "/api/v1/ext/coolify-rs/healthchecks/recent",
            "acme_member_read",
            None,
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    let recent = r.json();
    assert_eq!(recent["configured"], true);
    let checks: Vec<(&str, &str)> = recent["checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| (c["slug"].as_str().unwrap(), c["status"].as_str().unwrap()))
        .collect();
    assert_eq!(
        checks,
        [
            ("acme-prod-volume-app9-volume-app_data", "fail"),
            ("acme-prod-backup-db1-shop", "fail"),
            ("acme-prod-task-t1", "fail"),
        ]
    );
    assert_eq!(recent["checks"][0]["result"], "HTTP 202");
    let r = a
        .call(
            &h,
            "GET",
            "/api/v1/ext/coolify-rs/healthchecks/panel",
            "acme_member_read",
            None,
        )
        .await;
    assert_eq!(r.status, 200);
    assert!(r.body.contains("acme-prod-task-t1"), "{}", r.body);

    // A ping key that could change the URL is refused before any request.
    a.configure(&h, HEALTHCHECKS, json!({"ping_key": "s3cret/../x"}))
        .await;
    let r = a
        .event(&h, HEALTHCHECKS, "scheduled_task.succeeded", task(""))
        .await;
    assert_eq!(r["delivered"], false);
    assert!(
        r["error"]
            .as_str()
            .unwrap()
            .contains("the ping key may only contain letters, digits, - and _"),
        "{r}"
    );
    assert!(stand_in.take().is_empty());

    a.assert_no_secrets();
    assert_no_secrets_stored(&h).await;
    assert!(!logs.text().contains(MARK), "{}", logs.text());
    h.finish().await;
}
