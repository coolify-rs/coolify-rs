//! The web UI's plugin endpoints (ADR 0013): who sees and changes what, panels as sandboxed documents, the
//! `/ui/ext` bridge with its CSRF check, and the parity rule: with no plugin installed, the pages Coolify
//! defines get exactly the same data as without the plugin system, and the UI is told to add nothing.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use base64::Engine as _;
use common::Harness;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

const PANEL: &str = "/ui/plugins/coolify-rs/deploy-board/panels/dashboard";
const EXT: &str = "/ui/ext/coolify-rs/deploy-board";

fn plugins_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugins")
}

/// The deploy-board example, built from source the first time.
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
            std::fs::read(plugins_dir().join("target/wasm32-wasip2/release/deploy_board.wasm"))
                .unwrap();
        base64::engine::general_purpose::STANDARD.encode(wasm)
    })
}

fn manifest() -> String {
    std::fs::read_to_string(plugins_dir().join("examples/deploy-board/plugin.toml")).unwrap()
}

fn grant() -> Value {
    json!([
        "events: deployment.succeeded, deployment.failed",
        "storage",
        "routes",
        "ui: dashboard, application"
    ])
}

struct Res {
    status: StatusCode,
    headers: HeaderMap,
    body: String,
}

impl Res {
    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or_else(|e| panic!("not JSON ({e}): {}", self.body))
    }

    fn header(&self, name: &str) -> &str {
        self.headers
            .get(name)
            .map(|v| v.to_str().unwrap())
            .unwrap_or_default()
    }
}

/// A signed-in browser: its cookie and CSRF token.
struct Browser {
    cookie: String,
    csrf: String,
}

impl Browser {
    async fn login(h: &Harness, email: &str) -> Self {
        let (cookie, csrf) = h.ui_login(email).await;
        Self { cookie, csrf }
    }

    async fn send(
        &self,
        h: &Harness,
        method: &str,
        path: &str,
        body: Option<Value>,
        extra: &[(&str, &str)],
    ) -> Res {
        let mut b = Request::builder()
            .method(method)
            .uri(path)
            .header("cookie", &self.cookie);
        for (k, v) in extra {
            b = b.header(*k, *v);
        }
        let req = match body {
            Some(v) => b
                .header("content-type", "application/json")
                .body(Body::from(v.to_string()))
                .unwrap(),
            None => b.body(Body::empty()).unwrap(),
        };
        let res = h.app.clone().oneshot(req).await.unwrap();
        let status = res.status();
        let headers = res.headers().clone();
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        Res {
            status,
            headers,
            body: String::from_utf8(bytes.to_vec()).unwrap(),
        }
    }

    async fn get(&self, h: &Harness, path: &str) -> Res {
        self.send(h, "GET", path, None, &[]).await
    }

    /// A state-changing request with the CSRF header, as the UI sends it.
    async fn write(&self, h: &Harness, method: &str, path: &str, body: Value) -> Res {
        self.send(h, method, path, Some(body), &[("x-csrf-token", &self.csrf)])
            .await
    }

    /// The panel document, loaded the way a frame of the page loads it.
    async fn frame(&self, h: &Harness, path: &str) -> Res {
        self.send(
            h,
            "GET",
            path,
            None,
            &[
                ("sec-fetch-dest", "iframe"),
                ("sec-fetch-site", "same-origin"),
            ],
        )
        .await
    }
}

async fn install(h: &Harness, root: &Browser) {
    let body = json!({"manifest": manifest(), "component": component(), "grant": grant()});
    let r = root.write(h, "POST", "/ui/plugins", body).await;
    assert_eq!(r.status, 201, "{}", r.body);
}

#[tokio::test]
async fn settings_and_team_pages_follow_the_api_rules() {
    let h = Harness::with_plugins(true).await;
    let root = Browser::login(&h, "root@example.test").await;
    let alice = Browser::login(&h, "alice@example.test").await;
    let bob = Browser::login(&h, "bob@example.test").await;

    // Only instance admins see the settings page.
    let r = root.get(&h, "/ui/plugins").await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.json()["plugins"], json!([]));
    assert_eq!(r.json()["allow_upload"], true);
    assert_eq!(r.json()["confirm"], json!({"text": true, "password": true}));
    let r = alice.get(&h, "/ui/plugins").await;
    assert_eq!(r.status, 403, "{}", r.body);
    assert_eq!(
        r.body,
        r#"{"message":"Only instance admins can manage plugins."}"#
    );

    // The first upload answers the capabilities; only the same list back installs.
    let upload = json!({"manifest": manifest(), "component": component()});
    let r = root.write(&h, "POST", "/ui/plugins", upload.clone()).await;
    assert_eq!(r.status, 422, "{}", r.body);
    assert_eq!(r.json()["capabilities"], grant());
    assert_eq!(r.json()["plugin"]["id"], "coolify-rs/deploy-board");
    let mut partial = upload.clone();
    partial["grant"] = json!(["storage", "routes"]);
    assert_eq!(
        root.write(&h, "POST", "/ui/plugins", partial).await.status,
        422
    );
    let mut granted = upload.clone();
    granted["grant"] = grant();
    // Writes need the session's CSRF token.
    let r = root
        .send(&h, "POST", "/ui/plugins", Some(granted.clone()), &[])
        .await;
    assert_eq!(r.status.as_u16(), 419, "{}", r.body);
    assert_eq!(
        alice
            .write(&h, "POST", "/ui/plugins", granted.clone())
            .await
            .status,
        403
    );
    let r = root.write(&h, "POST", "/ui/plugins", granted).await;
    assert_eq!(r.status, 201, "{}", r.body);
    let p = r.json();
    assert_eq!(p["verified"], false);
    assert_eq!(p["origin"], "upload");
    assert_eq!(
        p["panels"],
        json!([{"slot": "dashboard", "title": "Deployments"}, {"slot": "application", "title": "Deployments"}])
    );

    // Team page: everyone in the team sees it, owners and admins switch it.
    let r = bob.get(&h, "/ui/plugins/team").await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.json()["can_manage"], false);
    assert_eq!(r.json()["plugins"][0]["enabled_for_team"], false);
    let enable = "/ui/plugins/coolify-rs/deploy-board/team";
    let r = bob
        .write(&h, "POST", enable, json!({"enabled": true}))
        .await;
    assert_eq!(
        (r.status.as_u16(), r.body.as_str()),
        (
            403,
            r#"{"message":"Only team owners and admins can enable plugins."}"#
        )
    );
    let r = alice
        .write(&h, "POST", enable, json!({"enabled": true}))
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.json()["enabled_for_team"], true);
    assert_eq!(
        bob.get(&h, "/ui/plugins/team").await.json()["plugins"][0]["enabled_for_team"],
        true
    );

    // The instance switch is the admins'; a switched-off plugin leaves the team page.
    let switch = "/ui/plugins/coolify-rs/deploy-board";
    assert_eq!(
        alice
            .write(&h, "PATCH", switch, json!({"enabled": false}))
            .await
            .status,
        403
    );
    let r = root
        .write(&h, "PATCH", switch, json!({"enabled": false}))
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.json()["enabled"], false);
    assert_eq!(
        alice.get(&h, "/ui/plugins/team").await.json()["plugins"],
        json!([])
    );
    let nav = alice.get(&h, "/ui/plugins/nav").await.json();
    assert_eq!(nav, json!({"settings": false, "team": false, "panels": []}));
    let r = alice
        .write(&h, "POST", enable, json!({"enabled": true}))
        .await;
    assert_eq!(r.status, 422, "{}", r.body);
    root.write(&h, "PATCH", switch, json!({"enabled": true}))
        .await;

    // The catalogue is not part of this build: the page hides it on 404.
    let r = root.get(&h, "/ui/plugins/catalog").await;
    assert_eq!(r.status, 404, "{}", r.body);
    assert_eq!(alice.get(&h, "/ui/plugins/catalog").await.status, 403);

    // Uninstalling takes the password step of the confirmation modal.
    let r = root
        .write(&h, "DELETE", switch, json!({"password": "wrong"}))
        .await;
    assert_eq!(r.status, 422, "{}", r.body);
    assert_eq!(
        alice
            .write(&h, "DELETE", switch, json!({"password": common::PASSWORD}))
            .await
            .status,
        403
    );
    let r = root
        .write(&h, "DELETE", switch, json!({"password": common::PASSWORD}))
        .await;
    assert_eq!(
        (r.status.as_u16(), r.body.as_str()),
        (200, r#"{"message":"Plugin uninstalled."}"#)
    );
    assert_eq!(
        root.get(&h, "/ui/plugins").await.json()["plugins"],
        json!([])
    );
    h.finish().await;
}

#[tokio::test]
async fn panels_are_sandboxed_documents_for_teams_that_enabled_them() {
    let h = Harness::with_plugins(true).await;
    let root = Browser::login(&h, "root@example.test").await;
    let alice = Browser::login(&h, "alice@example.test").await;
    let bob = Browser::login(&h, "bob@example.test").await;
    let carol = Browser::login(&h, "carol@example.test").await;
    install(&h, &root).await;

    // Installed, but Acme has not enabled it: no panels, and the document is a 404.
    let nav = alice.get(&h, "/ui/plugins/nav").await.json();
    assert_eq!(nav, json!({"settings": false, "team": true, "panels": []}));
    assert_eq!(alice.frame(&h, PANEL).await.status, 404);
    let r = alice
        .write(
            &h,
            "POST",
            "/ui/plugins/coolify-rs/deploy-board/team",
            json!({"enabled": true}),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.body);

    // Every member of the team sees the panels; other teams don't.
    for who in [&alice, &bob] {
        let nav = who.get(&h, "/ui/plugins/nav").await.json();
        assert_eq!(
            nav["panels"],
            json!([
                {"plugin": "coolify-rs/deploy-board", "name": "Deploy board", "verified": false, "slot": "dashboard", "index": 0, "title": "Deployments"},
                {"plugin": "coolify-rs/deploy-board", "name": "Deploy board", "verified": false, "slot": "application", "index": 0, "title": "Deployments"}
            ])
        );
    }
    assert_eq!(
        carol.get(&h, "/ui/plugins/nav").await.json()["panels"],
        json!([])
    );
    let r = carol.frame(&h, PANEL).await;
    assert_eq!(r.status, 404, "{}", r.body);
    assert_eq!(carol.get(&h, &format!("{EXT}/stats")).await.status, 404);

    // Events show up in the panel, which is served as a sandboxed document behind our policy.
    let host = h.state.plugins.clone().unwrap();
    host.dispatch(
        1,
        "deployment.succeeded",
        json!({"application_name": "<b>web</b>", "application_uuid": "app-1"}),
    );
    let mut stats = Value::Null;
    for _ in 0..100 {
        stats = bob.get(&h, &format!("{EXT}/stats")).await.json();
        if stats["succeeded"] == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(stats["succeeded"], 1);
    let r = bob.frame(&h, PANEL).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.header("content-type"), "text/html; charset=utf-8");
    assert_eq!(
        r.header("content-security-policy"),
        coolify_api::ui_plugins::PANEL_CSP
    );
    assert!(
        r.header("content-security-policy")
            .starts_with("sandbox allow-scripts;")
    );
    assert_eq!(r.header("x-content-type-options"), "nosniff");
    assert_eq!(r.header("cache-control"), "no-store");
    assert!(r.body.starts_with(&format!(
        "<!doctype html><meta charset=\"utf-8\"><meta http-equiv=\"Content-Security-Policy\" content=\"{}\">",
        coolify_api::ui_plugins::PANEL_CSP_META
    )));
    assert!(r.body.contains(r#"<b id="succeeded">1</b>"#), "{}", r.body);
    assert!(
        r.body.contains("&lt;b&gt;web&lt;/b&gt;"),
        "names are escaped"
    );
    assert!(
        r.body.contains("coolify-rs:request"),
        "the bridge is included"
    );

    // Not as a page, and not framed by another site.
    let r = bob
        .send(
            &h,
            "GET",
            PANEL,
            None,
            &[("sec-fetch-dest", "document"), ("sec-fetch-site", "none")],
        )
        .await;
    assert_eq!(r.status, 403, "{}", r.body);
    let r = bob
        .send(
            &h,
            "GET",
            PANEL,
            None,
            &[
                ("sec-fetch-dest", "iframe"),
                ("sec-fetch-site", "cross-site"),
            ],
        )
        .await;
    assert_eq!(r.status, 403, "{}", r.body);

    // The application panel gets the page's resource; anything else is refused.
    let app_panel = "/ui/plugins/coolify-rs/deploy-board/panels/application";
    let r = bob.frame(&h, &format!("{app_panel}?resource=app-1")).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert!(
        r.body.contains("<b>1</b>") && r.body.contains("(app-1)"),
        "{}",
        r.body
    );
    let r = bob
        .frame(&h, &format!("{app_panel}?resource=x%26slot%3Dsettings"))
        .await;
    assert_eq!(r.status, 400, "{}", r.body);
    for missing in [
        "/ui/plugins/coolify-rs/deploy-board/panels/server",
        "/ui/plugins/coolify-rs/deploy-board/panels/topbar",
        "/ui/plugins/coolify-rs/deploy-board/panels/dashboard?panel=1",
        "/ui/plugins/coolify-rs/nope/panels/dashboard",
    ] {
        assert_eq!(bob.frame(&h, missing).await.status, 404, "{missing}");
    }
    let signed_out = h
        .app
        .clone()
        .oneshot(Request::get(PANEL).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(signed_out.status(), 401);

    // The bridge's endpoint: the session's team, CSRF on writes, the API's response hardening.
    let r = bob
        .send(&h, "POST", &format!("{EXT}/reset"), Some(json!({})), &[])
        .await;
    assert_eq!(r.status.as_u16(), 419, "{}", r.body);
    let r = bob
        .write(&h, "POST", &format!("{EXT}/reset"), json!({}))
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.json()["succeeded"], 0);
    assert_eq!(r.header("x-content-type-options"), "nosniff");
    let r = bob.get(&h, &format!("{EXT}/panels/dashboard")).await;
    assert_eq!(r.status, 200);
    assert_eq!(r.header("content-type"), "application/octet-stream");
    assert_eq!(r.header("content-disposition"), "attachment");
    assert!(r.header("content-security-policy").contains("sandbox"));
    let r = bob.get(&h, &format!("{EXT}/nope")).await;
    assert_eq!(
        (r.status.as_u16(), r.body.as_str()),
        (404, r#"{"message":"Not found."}"#)
    );
    h.finish().await;
}

/// A plugin holding `deploy` (ADR 0011) is switched on for a team only with the team's own answer, which
/// the team page shows and can change.
#[tokio::test]
async fn team_switch_asks_the_team_about_deploy() {
    const MANIFEST: &str = r#"
id = "test/deployer"
name = "Deployer"
version = "0.1.0"
description = "Asks for deploy."
license = "Apache-2.0"
api = "0.1"

[capabilities]
events = ["deployment.succeeded"]
storage = true
routes = true
http = ["hooks.example.com"]
resources = ["projects", "servers", "applications", "services", "databases"]
deploy = true
"#;
    component();
    let wasm =
        std::fs::read(plugins_dir().join("target/wasm32-wasip2/release/hostile.wasm")).unwrap();
    let h = Harness::with_plugins(true).await;
    let root = Browser::login(&h, "root@example.test").await;
    let alice = Browser::login(&h, "alice@example.test").await;
    let upload = json!({"manifest": MANIFEST, "component": base64::engine::general_purpose::STANDARD.encode(wasm)});
    let r = root.write(&h, "POST", "/ui/plugins", upload.clone()).await;
    assert_eq!(r.status, 422, "{}", r.body);
    let mut granted = upload;
    granted["grant"] = r.json()["capabilities"].clone();
    let r = root.write(&h, "POST", "/ui/plugins", granted).await;
    assert_eq!(r.status, 201, "{}", r.body);

    let switch = "/ui/plugins/test/deployer/team";
    let r = alice
        .write(&h, "POST", switch, json!({"enabled": true}))
        .await;
    assert_eq!(r.status, 422, "{}", r.body);
    assert_eq!(r.json()["deploy"], true);
    assert!(
        r.json()["message"]
            .as_str()
            .unwrap()
            .contains("grant_deploy")
    );
    let team = alice.get(&h, "/ui/plugins/team").await.json();
    assert_eq!(
        team["plugins"][0]["enabled_for_team"], false,
        "nothing changed"
    );

    let r = alice
        .write(
            &h,
            "POST",
            switch,
            json!({"enabled": true, "grant_deploy": false}),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.json()["deploy_granted_for_team"], false);
    let p = &alice.get(&h, "/ui/plugins/team").await.json()["plugins"][0];
    assert_eq!(
        (
            &p["enabled_for_team"],
            &p["deploy"],
            &p["deploy_granted_for_team"]
        ),
        (&json!(true), &json!(true), &json!(false))
    );

    let r = alice
        .write(
            &h,
            "POST",
            switch,
            json!({"enabled": true, "grant_deploy": true}),
        )
        .await;
    assert_eq!(r.json()["deploy_granted_for_team"], true);
    assert_eq!(
        alice.get(&h, "/ui/plugins/team").await.json()["plugins"][0]["deploy_granted_for_team"],
        true
    );
    let r = alice
        .write(&h, "POST", switch, json!({"enabled": false}))
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    h.finish().await;
}

/// Dashboard, settings, team and session data are byte for byte the same with the plugin system on and
/// nothing installed as without it, and the navigation endpoint asks the UI to add nothing to them.
#[tokio::test]
async fn no_plugins_installed_changes_nothing() {
    let plain = Harness::new().await;
    let with = Harness::with_plugins(false).await;
    for email in [
        "root@example.test",
        "alice@example.test",
        "bob@example.test",
    ] {
        let a = Browser::login(&plain, email).await;
        let b = Browser::login(&with, email).await;
        for path in [
            "/ui/session",
            "/ui/dashboard",
            "/ui/team",
            "/ui/settings/general",
            "/ui/settings/advanced",
            "/ui/settings/updates",
        ] {
            let x = a.get(&plain, path).await;
            let y = b.get(&with, path).await;
            let mask = |r: &Res, csrf: &str| r.body.replace(csrf, "CSRF");
            assert_eq!(
                (x.status, mask(&x, &a.csrf)),
                (y.status, mask(&y, &b.csrf)),
                "{path} as {email}"
            );
        }
        // Without plugins: nothing to show. With the host but nothing installed and no way to install:
        // no Plugins entry in either menu, and no panels.
        let r = a.get(&plain, "/ui/plugins/nav").await;
        assert_eq!(
            (r.status.as_u16(), r.body.as_str()),
            (
                404,
                r#"{"message":"Plugins are switched off on this instance."}"#
            )
        );
        assert_eq!(
            b.get(&with, "/ui/plugins/nav").await.json(),
            json!({"settings": false, "team": false, "panels": []}),
            "{email}"
        );
    }
    plain.finish().await;
    with.finish().await;

    // In development mode, instance admins get the Settings entry to upload the first plugin.
    let dev = Harness::with_plugins(true).await;
    let root = Browser::login(&dev, "root@example.test").await;
    assert_eq!(
        root.get(&dev, "/ui/plugins/nav").await.json(),
        json!({"settings": true, "team": false, "panels": []})
    );
    let alice = Browser::login(&dev, "alice@example.test").await;
    assert_eq!(
        alice.get(&dev, "/ui/plugins/nav").await.json(),
        json!({"settings": false, "team": false, "panels": []})
    );
    dev.finish().await;
}
