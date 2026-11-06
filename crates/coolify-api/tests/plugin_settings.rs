//! Per-team plugin settings over the API and the web UI (ADR 0014): who reads and who changes them, secrets
//! that are written but never read back, validation in the API's usual shape, and an audit log without
//! values. `crates/coolify-plugins/tests/settings.rs` covers the host underneath.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::path::PathBuf;
use std::sync::OnceLock;

use base64::Engine as _;
use common::Harness;
use serde_json::{Value, json};

const ID: &str = "test/configured";
const SETTINGS: &str = "/api/v1/plugins/test/configured/settings";
const UI_SETTINGS: &str = "/ui/plugins/test/configured/settings";
const SECRET: &str = "tk_api_settings_value";

fn plugins_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugins")
}

/// `plugins/testing/configured`, built from source the first time.
fn component() -> &'static str {
    static B64: OnceLock<String> = OnceLock::new();
    B64.get_or_init(|| {
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let status = std::process::Command::new(cargo)
            .args(["build", "-q", "--release", "--target", "wasm32-wasip2"])
            .current_dir(plugins_dir())
            .status()
            .unwrap();
        assert!(status.success(), "building plugins/ failed");
        let wasm =
            std::fs::read(plugins_dir().join("target/wasm32-wasip2/release/configured.wasm"))
                .unwrap();
        base64::engine::general_purpose::STANDARD.encode(wasm)
    })
}

/// A harness with the configured test plugin installed and switched on for both fixture teams.
async fn harness() -> Harness {
    let h = Harness::with_plugins(true).await;
    let manifest =
        std::fs::read_to_string(plugins_dir().join("testing/configured/plugin.toml")).unwrap();
    let r = h
        .call(
            "POST",
            "/api/v1/plugins",
            Some("root_root"),
            Some(json!({
                "manifest": manifest,
                "component": component(),
                // Settings are not a capability: the grant is the capability list alone.
                "grant": ["events: deployment.succeeded", "storage", "routes"],
            })),
        )
        .await;
    assert_eq!(r.status, 201, "{}", r.body);
    for token in ["acme_owner_write", "carol_root"] {
        let r = h
            .call(
                "POST",
                "/api/v1/plugins/test/configured/enable",
                Some(token),
                None,
            )
            .await;
        assert_eq!(r.status, 200, "{}", r.body);
    }
    h
}

fn setting<'a>(body: &'a Value, key: &str) -> &'a Value {
    body["settings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["key"] == key)
        .unwrap_or_else(|| panic!("{key} in {body}"))
}

/// What the plugin itself reads for `key`, through its route, as the token's team.
async fn plugin_reads(h: &Harness, token: &str, key: &str) -> Value {
    let r = h
        .call(
            "GET",
            &format!("/api/v1/ext/test/configured/config/{key}"),
            Some(token),
            None,
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    r.json()["value"].clone()
}

#[tokio::test]
async fn members_read_admins_write_and_secrets_are_never_returned() {
    let h = harness().await;

    // Any member reads the declarations; nothing is set yet.
    let r = h
        .call("GET", SETTINGS, Some("acme_member_read"), None)
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    let body = r.json();
    assert_eq!(body["plugin"], ID);
    assert_eq!(body["needs_configuration"], true);
    assert_eq!(
        setting(&body, "topic"),
        &json!({
            "key": "topic", "label": "Topic", "help": "The topic alerts are published to.", "kind": "text",
            "secret": false, "required": true, "options": [], "default": null, "set": false, "value": null,
        })
    );
    assert_eq!(
        setting(&body, "token"),
        &json!({
            "key": "token", "label": "Access token", "help": null, "kind": "text",
            "secret": true, "required": true, "options": [], "default": null, "set": false,
        })
    );
    assert_eq!(setting(&body, "priority")["kind"], "select");
    assert_eq!(setting(&body, "webhook")["kind"], "url");

    // Writing needs the write ability and an owner or admin.
    for token in [
        "acme_member_write_legacy",
        "acme_owner_read",
        "acme_member_read",
    ] {
        let r = h
            .call("PATCH", SETTINGS, Some(token), Some(json!({"topic": "x"})))
            .await;
        assert_eq!(r.status, 403, "{token}: {}", r.body);
    }
    let r = h
        .call(
            "PATCH",
            SETTINGS,
            Some("acme_owner_write"),
            Some(json!({"topic": "alpha", "token": SECRET, "priority": "high"})),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert!(!r.body.contains(SECRET), "{}", r.body);
    let body = r.json();
    assert_eq!(body["needs_configuration"], false);
    assert_eq!(setting(&body, "token")["set"], true);
    assert!(setting(&body, "token").get("value").is_none());
    assert_eq!(setting(&body, "topic")["value"], "alpha");

    // Members see that the secret is set, never its value; the other team sees nothing of it.
    let r = h
        .call("GET", SETTINGS, Some("acme_member_read"), None)
        .await;
    assert!(!r.body.contains(SECRET), "{}", r.body);
    assert_eq!(setting(&r.json(), "token")["set"], true);
    let r = h.call("GET", SETTINGS, Some("carol_root"), None).await;
    assert_eq!(setting(&r.json(), "token")["set"], false);
    assert_eq!(setting(&r.json(), "topic")["value"], Value::Null);

    // The plugin reads the secret for its own team only.
    assert_eq!(plugin_reads(&h, "acme_owner_read", "token").await, SECRET);
    assert_eq!(plugin_reads(&h, "carol_root", "token").await, Value::Null);
    assert_eq!(
        plugin_reads(&h, "carol_root", "server").await,
        "https://ntfy.sh"
    );

    // At rest the secret is the APP_KEY payload, not the value.
    let raw: String = sqlx::query_scalar(
        "SELECT value FROM coolify_rs_plugins.team_settings WHERE plugin_id = $1 AND team_id = 1 AND key = 'token'",
    )
    .bind(ID)
    .fetch_one(h.tdb.db.pool())
    .await
    .unwrap();
    assert!(!raw.contains(SECRET));
    let e = coolify_compat::Encrypter::new(common::APP_KEY).unwrap();
    assert_eq!(e.decrypt_string(&raw).unwrap(), SECRET);

    // Clearing.
    let r = h
        .call(
            "PATCH",
            SETTINGS,
            Some("acme_owner_write"),
            Some(json!({"token": null})),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(setting(&r.json(), "token")["set"], false);
    assert_eq!(r.json()["needs_configuration"], true);
    assert_eq!(
        plugin_reads(&h, "acme_owner_read", "token").await,
        Value::Null
    );

    // Every change is audited with its keys and who made it, never a value.
    let audit: Vec<(Value, Option<i64>, Option<i64>)> = sqlx::query_as(
        "SELECT detail, team_id, user_id FROM coolify_rs_plugins.audit WHERE action = 'team_settings_updated' ORDER BY id",
    )
    .fetch_all(h.tdb.db.pool())
    .await
    .unwrap();
    assert_eq!(
        audit,
        [
            (
                json!({"set": ["topic", "token", "priority"], "cleared": []}),
                Some(1),
                Some(1)
            ),
            (json!({"set": [], "cleared": ["token"]}), Some(1), Some(1)),
        ]
    );
    let all: Vec<Value> = sqlx::query_scalar("SELECT detail FROM coolify_rs_plugins.audit")
        .fetch_all(h.tdb.db.pool())
        .await
        .unwrap();
    assert!(!serde_json::to_string(&all).unwrap().contains(SECRET));
    h.finish().await;
}

#[tokio::test]
async fn invalid_settings_answer_422_and_change_nothing() {
    let h = harness().await;
    let owner = Some("acme_owner_write");
    let r = h
        .call(
            "PATCH",
            SETTINGS,
            owner,
            Some(json!({"topic": "alpha", "server": "http://ntfy.sh", "nope": "x", "priority": 3})),
        )
        .await;
    r.assert(
        422,
        r#"{"message":"Validation failed.","errors":{"nope":["The plugin has no such setting."],"priority":["Priority must be a string or null."],"server":["Server must be an https:\/\/ URL."]}}"#,
    );
    let r = h
        .call(
            "PATCH",
            SETTINGS,
            owner,
            Some(json!({"webhook": "https://x/\u{1}"})),
        )
        .await;
    assert_eq!(r.status, 422, "{}", r.body);
    assert!(
        r.json()["errors"]["webhook"][0]
            .as_str()
            .unwrap()
            .contains("control characters")
    );
    let r = h
        .call("PATCH", SETTINGS, owner, Some(json!(["topic"])))
        .await;
    assert_eq!(r.status, 422, "{}", r.body);
    assert!(r.body.contains("JSON object"), "{}", r.body);

    let r = h.call("GET", SETTINGS, Some("acme_owner_read"), None).await;
    assert_eq!(setting(&r.json(), "topic")["set"], false);

    // Unknown plugins are not found, for readers and writers.
    let missing = "/api/v1/plugins/test/missing/settings";
    h.call("GET", missing, Some("acme_member_read"), None)
        .await
        .assert(404, r#"{"message":"Plugin not found."}"#);
    h.call("PATCH", missing, owner, Some(json!({"topic": "x"})))
        .await
        .assert(404, r#"{"message":"Plugin not found."}"#);
    h.finish().await;
}

#[tokio::test]
async fn the_team_page_shows_settings_and_needs_configuration() {
    let h = harness().await;
    let (alice, alice_csrf) = h.ui_login("alice@example.test").await;
    let (bob, bob_csrf) = h.ui_login("bob@example.test").await;

    let page = h
        .request(
            "GET",
            "/ui/plugins/team",
            &[("cookie", alice.as_str())],
            None,
        )
        .await;
    assert_eq!(page.status, 200, "{}", page.body);
    let item = page.json()["plugins"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == ID)
        .unwrap()
        .clone();
    assert_eq!(item["needs_configuration"], true);
    assert_eq!(item["settings"].as_array().unwrap().len(), 5);
    // Settings are not capabilities.
    assert_eq!(
        item["capabilities"],
        json!(["events: deployment.succeeded", "storage", "routes"])
    );

    // Owners change settings through the session with the CSRF token; members can't.
    let body = json!({"topic": "alpha", "token": SECRET});
    let r = h
        .request(
            "PATCH",
            UI_SETTINGS,
            &[("cookie", alice.as_str())],
            Some(body.clone()),
        )
        .await;
    assert_eq!(r.status, 419, "{}", r.body);
    let r = h
        .request(
            "PATCH",
            UI_SETTINGS,
            &[
                ("cookie", bob.as_str()),
                ("x-csrf-token", bob_csrf.as_str()),
            ],
            Some(body.clone()),
        )
        .await;
    assert_eq!(r.status, 403, "{}", r.body);
    let r = h
        .request(
            "PATCH",
            UI_SETTINGS,
            &[
                ("cookie", alice.as_str()),
                ("x-csrf-token", alice_csrf.as_str()),
            ],
            Some(body),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert!(!r.body.contains(SECRET));

    for cookie in [&alice, &bob] {
        let r = h
            .request("GET", UI_SETTINGS, &[("cookie", cookie.as_str())], None)
            .await;
        assert_eq!(r.status, 200, "{}", r.body);
        assert!(!r.body.contains(SECRET), "{}", r.body);
        assert_eq!(setting(&r.json(), "token")["set"], true);
        let page = h
            .request(
                "GET",
                "/ui/plugins/team",
                &[("cookie", cookie.as_str())],
                None,
            )
            .await;
        assert!(!page.body.contains(SECRET), "{}", page.body);
        let item = page.json()["plugins"][0].clone();
        assert_eq!(item["needs_configuration"], false);
    }

    // Validation errors come back by field, for the form.
    let r = h
        .request(
            "PATCH",
            UI_SETTINGS,
            &[
                ("cookie", alice.as_str()),
                ("x-csrf-token", alice_csrf.as_str()),
            ],
            Some(json!({"webhook": "http://insecure.example"})),
        )
        .await;
    assert_eq!(r.status, 422, "{}", r.body);
    assert_eq!(
        r.json()["errors"]["webhook"],
        json!(["Webhook URL must be an https:// URL."])
    );
    h.finish().await;
}
