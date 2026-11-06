//! Plugin endpoints end to end (ADR 0010): install with an explicit grant, per-team enablement, events into
//! the example plugin, its route under `/api/v1/ext`, and the parity rule: with a plugin installed and
//! enabled, every endpoint Coolify defines answers exactly as it does without the plugin system.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

use base64::Engine as _;
use common::Harness;
use serde_json::{Value, json};

const ID: &str = "coolify-rs/deploy-stats";
const ADMIN: Option<&str> = Some("root_root");
const STATS: &str = "/api/v1/ext/coolify-rs/deploy-stats/stats";

fn example() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugins")
}

/// The example plugin, built from source with the pinned toolchain the first time.
fn component() -> &'static str {
    static B64: OnceLock<String> = OnceLock::new();
    B64.get_or_init(|| {
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let status = std::process::Command::new(cargo)
            .args(["build", "-q", "--release", "--target", "wasm32-wasip2"])
            .current_dir(example())
            .status()
            .unwrap();
        assert!(status.success(), "building plugins/ failed");
        let wasm = std::fs::read(example().join("target/wasm32-wasip2/release/deploy_stats.wasm"))
            .unwrap();
        base64::engine::general_purpose::STANDARD.encode(wasm)
    })
}

fn manifest() -> String {
    std::fs::read_to_string(example().join("examples/deploy-stats/plugin.toml")).unwrap()
}

fn grant() -> Value {
    json!([
        "events: deployment.succeeded, deployment.failed",
        "storage",
        "routes"
    ])
}

fn install_body(grant: Option<Value>) -> Value {
    let mut b = json!({"manifest": manifest(), "component": component()});
    if let Some(g) = grant {
        b["grant"] = g;
    }
    b
}

async fn stats(h: &Harness, token: &str) -> Value {
    let r = h.call("GET", STATS, Some(token), None).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.ctype, "application/json");
    r.json()
}

#[tokio::test]
async fn switched_off_and_unverified_installs_refused() {
    let h = Harness::new().await;
    h.call("GET", "/api/v1/plugins", ADMIN, None).await.assert(
        404,
        r#"{"message":"Plugins are switched off on this instance."}"#,
    );
    h.call("GET", STATS, ADMIN, None).await.assert(
        404,
        r#"{"message":"Plugins are switched off on this instance."}"#,
    );
    h.tdb.drop_db().await;

    let h = Harness::with_plugins(false).await;
    let r = h
        .call(
            "POST",
            "/api/v1/plugins",
            ADMIN,
            Some(install_body(Some(grant()))),
        )
        .await;
    assert_eq!(r.status, 403, "{}", r.body);
    assert!(r.body.contains("COOLIFY_RS_ALLOW_UNVERIFIED_PLUGINS"));
    h.call("GET", "/api/v1/plugins", ADMIN, None)
        .await
        .assert(200, "[]");
    h.tdb.drop_db().await;
}

#[tokio::test]
async fn install_grant_enable_events_and_routes() {
    let h = Harness::with_plugins(true).await;

    // Only instance admins install, and only with the capability list repeated back.
    let r = h
        .call(
            "POST",
            "/api/v1/plugins",
            Some("acme_owner_write"),
            Some(install_body(Some(grant()))),
        )
        .await;
    r.assert(
        403,
        r#"{"message":"Only instance admins can manage plugins."}"#,
    );
    let r = h
        .call("POST", "/api/v1/plugins", ADMIN, Some(install_body(None)))
        .await;
    assert_eq!(r.status, 422, "{}", r.body);
    assert_eq!(r.json()["capabilities"], grant());
    let r = h
        .call(
            "POST",
            "/api/v1/plugins",
            ADMIN,
            Some(install_body(Some(json!(["storage", "routes"])))),
        )
        .await;
    assert_eq!(r.status, 422, "a partial grant is not a grant: {}", r.body);
    let r = h
        .call(
            "POST",
            "/api/v1/plugins",
            ADMIN,
            Some(install_body(Some(grant()))),
        )
        .await;
    assert_eq!(r.status, 201, "{}", r.body);
    let p = r.json();
    assert_eq!(p["id"], ID);
    assert_eq!(p["verified"], false);
    assert_eq!(p["origin"], "upload");
    assert_eq!(p["enabled"], true);

    // Installed, but no team has it yet.
    let r = h.call("GET", STATS, Some("acme_owner_read"), None).await;
    r.assert(
        404,
        r#"{"message":"Plugin not found, or not enabled for this team."}"#,
    );
    let r = h
        .call(
            "POST",
            "/api/v1/plugins/coolify-rs/deploy-stats/enable",
            Some("acme_member_write_legacy"),
            None,
        )
        .await;
    r.assert(
        403,
        // Coolify's own write guard turns members away before the handler's admin check.
        r#"{"message":"Missing required team role."}"#,
    );
    let r = h
        .call(
            "POST",
            "/api/v1/plugins/coolify-rs/deploy-stats/enable",
            Some("acme_owner_write"),
            None,
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.json()["enabled_for_team"], true);
    let r = h
        .call(
            "GET",
            "/api/v1/plugins/coolify-rs/deploy-stats",
            Some("acme_owner_read"),
            None,
        )
        .await;
    assert_eq!(r.json()["enabled_for_team"], true);
    let r = h
        .call(
            "GET",
            "/api/v1/plugins/coolify-rs/deploy-stats",
            Some("carol_root"),
            None,
        )
        .await;
    assert_eq!(r.json()["enabled_for_team"], false);
    assert_eq!(
        stats(&h, "acme_owner_read").await,
        json!({"succeeded": 0, "failed": 0, "last": null})
    );

    // Events reach the plugin only for teams that enabled it.
    let host = h.state.plugins.clone().unwrap();
    let deployed = json!({"application_name": "web", "deployment_uuid": "d1"});
    host.dispatch(1, "deployment.succeeded", deployed.clone());
    host.dispatch(2, "deployment.succeeded", deployed.clone());
    host.dispatch(1, "backup.failed", json!({}));
    let mut seen = Value::Null;
    for _ in 0..100 {
        seen = stats(&h, "acme_owner_read").await;
        if seen["succeeded"] == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        seen,
        json!({"succeeded": 1, "failed": 0, "last": {"kind": "deployment.succeeded", "application": "web", "deployment_uuid": "d1"}})
    );
    h.call("GET", STATS, Some("carol_root"), None).await.assert(
        404,
        r#"{"message":"Plugin not found, or not enabled for this team."}"#,
    );

    // The plugin's own 404, served as JSON.
    let r = h
        .call(
            "GET",
            "/api/v1/ext/coolify-rs/deploy-stats/nope",
            Some("acme_owner_read"),
            None,
        )
        .await;
    r.assert(404, r#"{"message":"Not found."}"#);
    // Plugin routes need a token like every other API route.
    assert_eq!(h.call("GET", STATS, None, None).await.status, 401);

    // Switched off for everyone, then uninstalled with its data.
    let r = h
        .call(
            "PATCH",
            "/api/v1/plugins/coolify-rs/deploy-stats",
            ADMIN,
            Some(json!({"enabled": false})),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(
        h.call("GET", STATS, Some("acme_owner_read"), None)
            .await
            .status,
        404
    );
    h.call(
        "DELETE",
        "/api/v1/plugins/coolify-rs/deploy-stats",
        ADMIN,
        None,
    )
    .await
    .assert(200, r#"{"message":"Plugin uninstalled."}"#);
    let left: i64 = sqlx::query_scalar(
        "SELECT (SELECT count(*) FROM coolify_rs_plugins.storage) + (SELECT count(*) FROM coolify_rs_plugins.team_plugins)",
    )
    .fetch_one(h.tdb.db.pool())
    .await
    .unwrap();
    assert_eq!(left, 0);
    h.tdb.drop_db().await;
}

/// Coolify's endpoints answer byte for byte the same with a plugin installed and enabled for the team as
/// without the plugin system, and the plugin schema stays out of `public`.
#[tokio::test]
async fn parity_with_a_plugin_installed() {
    let plain = Harness::new().await;
    let with = Harness::with_plugins(true).await;
    let r = with
        .call(
            "POST",
            "/api/v1/plugins",
            ADMIN,
            Some(install_body(Some(grant()))),
        )
        .await;
    assert_eq!(r.status, 201, "{}", r.body);
    for token in ["acme_owner_write", "carol_root"] {
        let r = with
            .call(
                "POST",
                "/api/v1/plugins/coolify-rs/deploy-stats/enable",
                Some(token),
                None,
            )
            .await;
        assert_eq!(r.status, 200, "{}", r.body);
    }
    with.state
        .plugins
        .clone()
        .unwrap()
        .dispatch(1, "deployment.succeeded", json!({}));

    let probes: &[(&str, &str, &str)] = &[
        ("GET", "/api/v1/version", "acme_owner_read"),
        ("GET", "/api/v1/health", "acme_owner_read"),
        ("GET", "/api/v1/teams", "acme_owner_read"),
        ("GET", "/api/v1/teams/current", "acme_owner_read"),
        ("GET", "/api/v1/teams/current/members", "acme_owner_read"),
        ("GET", "/api/v1/projects", "acme_owner_read"),
        ("GET", "/api/v1/servers", "acme_owner_read"),
        ("GET", "/api/v1/security/keys", "acme_owner_sensitive"),
        ("GET", "/api/v1/applications", "acme_owner_read"),
        ("GET", "/api/v1/services", "acme_owner_read"),
        ("GET", "/api/v1/databases", "acme_owner_read"),
        ("GET", "/api/v1/resources", "acme_owner_read"),
        ("GET", "/api/v1/teams", "carol_root"),
        ("GET", "/api/v1/projects/nope", "acme_owner_read"),
        ("GET", "/api/v1/teams", "acme_owner_expired"),
        ("POST", "/api/v1/projects", "acme_owner_read"),
    ];
    for (method, path, token) in probes {
        let a = plain.call(method, path, Some(token), None).await;
        let b = with.call(method, path, Some(token), None).await;
        assert_eq!(
            (a.status, &a.ctype, &a.body),
            (b.status, &b.ctype, &b.body),
            "{method} {path} as {token}"
        );
    }

    let public: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM information_schema.tables WHERE table_schema = 'public' AND table_name LIKE '%plugin%'",
    )
    .fetch_one(with.tdb.db.pool())
    .await
    .unwrap();
    assert_eq!(public, 0, "plugin tables must stay out of Coolify's schema");
    plain.tdb.drop_db().await;
    with.tdb.drop_db().await;
}
