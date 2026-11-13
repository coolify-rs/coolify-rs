//! The first-party plugin `coolify-rs/deploy-chains` (plugins/official/deploy-chains) against the real API:
//! rules are saved through its routes (loops, other teams' applications and read-only callers refused), a
//! `deployment.succeeded` queues the targets' deployments and nothing else, previews never trigger a chain,
//! and a withdrawn consent or the deploy rate limit is recorded in the history instead of deploying. Its
//! panels are served through the web UI's panel endpoint and write through the bridge.
//!
//! A stand-in for `ssh` answers the server jobs, and the server runs no builds at once
//! (`concurrent_builds = 0`), so a queued deployment never starts.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::OnceLock;

use axum::body::Body;
use axum::http::Request;
use base64::Engine as _;
use common::Harness;
use coolify_remote::{Remote, RemoteConfig};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

const ADMIN: Option<&str> = Some("root_root");
const ID: &str = "coolify-rs/deploy-chains";
const EXT: &str = "/api/v1/ext/coolify-rs/deploy-chains";
const FAKE_SSH: &str = "#!/bin/sh\ncat > /dev/null\necho ok\n";

fn plugins_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugins")
}

/// The component, built from source with the pinned toolchain the first time, as base64.
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
            std::fs::read(plugins_dir().join("target/wasm32-wasip2/release/deploy_chains.wasm"))
                .unwrap();
        base64::engine::general_purpose::STANDARD.encode(wasm)
    })
}

fn manifest() -> String {
    std::fs::read_to_string(plugins_dir().join("official/deploy-chains/plugin.toml")).unwrap()
}

/// Installs the plugin as the instance admin, approving exactly what it asks for.
async fn install(h: &Harness) {
    let body = json!({"manifest": manifest(), "component": component()});
    let r = h
        .call("POST", "/api/v1/plugins", ADMIN, Some(body.clone()))
        .await;
    assert_eq!(r.status, 422, "{}", r.body);
    let capabilities = r.json()["capabilities"].clone();
    assert_eq!(
        capabilities,
        json!([
            "events: deployment.succeeded",
            "storage",
            "routes",
            "resources: applications",
            "deploy",
            "ui: application, dashboard"
        ])
    );
    let mut body = body;
    body["grant"] = capabilities;
    let r = h.call("POST", "/api/v1/plugins", ADMIN, Some(body)).await;
    assert_eq!(r.status, 201, "{}", r.body);
}

async fn enable(h: &Harness, token: &str, grant_deploy: bool) {
    let r = h
        .call(
            "POST",
            &format!("/api/v1/plugins/{ID}/enable"),
            Some(token),
            Some(json!({"grant_deploy": grant_deploy})),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.json()["deploy_granted_for_team"], grant_deploy);
}

/// A key, a usable server that builds nothing, a project and one Docker image application per name, all
/// through the API with `token`. Returns the applications' uuids.
async fn team(h: &Harness, token: &str, prefix: &str, apps: &[&str]) -> Vec<String> {
    let t = Some(token);
    let uuid = |r: common::Resp| {
        assert!(r.status.is_success(), "{}", r.body);
        r.json()["uuid"].as_str().unwrap().to_owned()
    };
    let pem = coolify_compat::sshkey::generate(coolify_compat::sshkey::KeyKind::Ed25519).unwrap();
    let key = uuid(
        h.call(
            "POST",
            "/api/v1/security/keys",
            t,
            Some(json!({"name": "key", "private_key": pem})),
        )
        .await,
    );
    let ip = format!("203.0.113.{}", 50 + prefix.len());
    let server = uuid(
        h.call(
            "POST",
            "/api/v1/servers",
            t,
            Some(json!({"name": prefix, "ip": ip, "private_key_uuid": key})),
        )
        .await,
    );
    sqlx::query(
        "UPDATE server_settings SET is_reachable = true, is_usable = true, docker_version = '29.1.3', \
         concurrent_builds = 0 WHERE server_id = (SELECT id FROM servers WHERE uuid = $1)",
    )
    .bind(&server)
    .execute(h.tdb.db.pool())
    .await
    .unwrap();
    let project = uuid(
        h.call("POST", "/api/v1/projects", t, Some(json!({"name": prefix})))
            .await,
    );
    let mut out = vec![];
    for name in apps {
        out.push(uuid(
            h.call(
                "POST",
                "/api/v1/applications/dockerimage",
                t,
                Some(json!({
                    "server_uuid": server,
                    "project_uuid": project,
                    "environment_name": "production",
                    "name": format!("{prefix}-{name}"),
                    "docker_registry_image_name": "nginx",
                    "docker_registry_image_tag": "alpine",
                    "ports_exposes": "80",
                    "instant_deploy": false,
                })),
            )
            .await,
        ));
    }
    out
}

async fn harness() -> (Harness, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let bin = root.join("bin");
    std::fs::create_dir(&bin).unwrap();
    std::fs::write(bin.join("ssh"), FAKE_SSH).unwrap();
    std::fs::set_permissions(bin.join("ssh"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut cfg = RemoteConfig::new(root.join("data"));
    cfg.ssh_binary = bin.join("ssh");
    (
        Harness::with_plugins_and_remote(Remote::new(cfg).unwrap()).await,
        tmp,
    )
}

/// The application's queued deployments: (status, is_api, force_rebuild).
async fn queued(h: &Harness, app: &str) -> Vec<(String, bool, bool)> {
    sqlx::query_as(
        "SELECT q.status, q.is_api, q.force_rebuild FROM application_deployment_queues q \
         JOIN applications a ON a.id = q.application_id::bigint WHERE a.uuid = $1 ORDER BY q.id",
    )
    .bind(app)
    .fetch_all(h.tdb.db.pool())
    .await
    .unwrap()
}

async fn put_rule(h: &Harness, token: &str, source: &str, body: Value) -> common::Resp {
    h.call(
        "PUT",
        &format!("{EXT}/rules/{source}"),
        Some(token),
        Some(body),
    )
    .await
}

fn targets(list: &[(&str, bool)]) -> Value {
    json!({
        "targets": list.iter().map(|(u, f)| json!({"uuid": u, "force": f})).collect::<Vec<_>>(),
    })
}

/// Sends a `deployment.succeeded` of `app` to the plugin for the token's team, and waits for it.
async fn succeeded(h: &Harness, token: &str, app: &str, extra: Value) {
    let mut payload = json!({
        "application_name": "source",
        "application_uuid": app,
        "deployment_uuid": "source-deployment-1",
        "project_name": "p",
        "project_uuid": "p",
        "environment_name": "production",
        "environment_uuid": "e",
        "fqdn": null,
        "preview": null,
        "commit": {"sha": "abc", "message": "m", "url": null, "branch": "main"},
    });
    for (k, v) in extra.as_object().unwrap() {
        payload[k] = v.clone();
    }
    let r = h
        .call(
            "POST",
            &format!("/api/v1/plugins/{ID}/test-event"),
            Some(token),
            Some(json!({"kind": "deployment.succeeded", "payload": payload})),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.json(), json!({"delivered": true}));
}

async fn history(h: &Harness, token: &str) -> Vec<Value> {
    let r = h
        .call("GET", &format!("{EXT}/history"), Some(token), None)
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    r.json()["history"].as_array().unwrap().clone()
}

#[tokio::test]
async fn chains_deploy_the_targets_of_a_successful_deployment_only() {
    let (h, _tmp) = harness().await;
    let acme = team(
        &h,
        "acme_owner_write",
        "acme",
        &["api", "worker", "scheduler", "web"],
    )
    .await;
    let (api, worker, scheduler, web) = (&acme[0], &acme[1], &acme[2], &acme[3]);
    let other = team(&h, "carol_root", "other", &["app"]).await;
    let foreign = &other[0];
    install(&h).await;
    enable(&h, "acme_owner_write", true).await;
    enable(&h, "carol_root", true).await;

    // After api, deploy worker (rebuilding) and scheduler.
    let r = put_rule(
        &h,
        "acme_owner_root",
        api,
        targets(&[(worker, true), (scheduler, false)]),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(
        r.json(),
        json!({
            "source": {"uuid": api, "name": "acme-api", "missing": false},
            "branch": null,
            "targets": [
                {"uuid": worker, "name": "acme-worker", "missing": false, "force": true},
                {"uuid": scheduler, "name": "acme-scheduler", "missing": false, "force": false},
            ],
        })
    );

    // Read-only callers see the rules and change nothing.
    for token in ["acme_owner_read", "acme_member_read"] {
        let r = put_rule(&h, token, api, targets(&[(web, false)])).await;
        assert_eq!(r.status, 403, "{token}: {}", r.body);
        let r = h
            .call("DELETE", &format!("{EXT}/rules/{api}"), Some(token), None)
            .await;
        assert_eq!(r.status, 403, "{token}: {}", r.body);
        let r = h
            .call("GET", &format!("{EXT}/rules"), Some(token), None)
            .await;
        assert_eq!(r.status, 200, "{}", r.body);
        assert_eq!(r.json()["can_write"], false);
        assert_eq!(r.json()["rules"].as_array().unwrap().len(), 1);
    }
    let r = h
        .call(
            "GET",
            &format!("{EXT}/rules"),
            Some("acme_owner_root"),
            None,
        )
        .await;
    assert_eq!(r.json()["can_write"], true);

    // Loops are refused: back to the source, through a longer path, and to itself.
    let r = put_rule(&h, "acme_owner_root", worker, targets(&[(api, false)])).await;
    assert_eq!(r.status, 422, "{}", r.body);
    assert_eq!(
        r.json(),
        json!({"message": "Validation failed.", "errors": {"targets": [
            "This would deploy in a loop: acme-worker → acme-api → acme-worker."
        ]}})
    );
    let r = put_rule(&h, "acme_owner_root", scheduler, targets(&[(web, false)])).await;
    assert_eq!(r.status, 200, "{}", r.body);
    let r = put_rule(&h, "acme_owner_root", web, targets(&[(api, false)])).await;
    assert_eq!(r.status, 422, "{}", r.body);
    assert_eq!(
        r.json()["errors"]["targets"][0],
        "This would deploy in a loop: acme-web → acme-api → acme-scheduler → acme-web."
    );
    let r = put_rule(&h, "acme_owner_root", web, targets(&[(web, false)])).await;
    assert_eq!(r.status, 422, "{}", r.body);
    assert_eq!(
        r.json()["errors"]["targets"][0],
        "An application can't deploy itself after it deploys."
    );

    // Another team's applications are not this team's, as source or target, and nothing was stored.
    let r = put_rule(&h, "acme_owner_root", web, targets(&[(foreign, false)])).await;
    assert_eq!(r.status, 422, "{}", r.body);
    assert_eq!(
        r.json()["errors"]["targets"][0],
        "Target 1 is not an application of this team."
    );
    let r = put_rule(&h, "acme_owner_root", foreign, targets(&[(web, false)])).await;
    assert_eq!(r.status, 422, "{}", r.body);
    assert_eq!(
        r.json()["errors"]["source"][0],
        "The source is not an application of this team."
    );
    let r = put_rule(&h, "acme_owner_root", web, json!({"targets": "worker"})).await;
    assert_eq!(r.status, 422, "{}", r.body);
    let r = h
        .call("GET", &format!("{EXT}/rules"), Some("carol_root"), None)
        .await;
    assert_eq!(r.json()["rules"], json!([]), "{}", r.body);
    let r = h
        .call(
            "GET",
            &format!("{EXT}/rules"),
            Some("acme_owner_read"),
            None,
        )
        .await;
    let mut sources: Vec<String> = r.json()["rules"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["source"]["name"].as_str().unwrap().to_owned())
        .collect();
    sources.sort();
    assert_eq!(sources, ["acme-api", "acme-scheduler"]);

    // A preview deployment of api triggers nothing.
    succeeded(
        &h,
        "acme_owner_write",
        api,
        json!({"preview": {"pull_request_id": 7, "fqdn": null}}),
    )
    .await;
    for app in &acme {
        assert!(queued(&h, app).await.is_empty());
    }
    assert!(history(&h, "acme_owner_read").await.is_empty());

    // api deployed: worker and scheduler are queued, through the API's path, and nothing else.
    succeeded(&h, "acme_owner_write", api, json!({})).await;
    assert_eq!(
        queued(&h, worker).await,
        [("queued".to_owned(), true, true)]
    );
    assert_eq!(
        queued(&h, scheduler).await,
        [("queued".to_owned(), true, false)]
    );
    assert!(queued(&h, api).await.is_empty());
    assert!(
        queued(&h, web).await.is_empty(),
        "web follows scheduler, which has not deployed"
    );
    assert!(queued(&h, foreign).await.is_empty());

    let hist = history(&h, "acme_member_read").await;
    assert_eq!(hist.len(), 2, "{hist:?}");
    // Newest first: scheduler was deployed after worker.
    for (e, (target, name, force)) in hist.iter().zip([
        (scheduler, "acme-scheduler", false),
        (worker, "acme-worker", true),
    ]) {
        assert_eq!(e["source"], api.as_str());
        assert_eq!(e["source_name"], "acme-api");
        assert_eq!(e["source_deployment"], "source-deployment-1");
        assert_eq!(e["target"], target.as_str());
        assert_eq!(e["target_name"], name);
        assert_eq!(e["force"], force);
        assert_eq!(e["status"], "queued", "{e}");
        assert_eq!(
            e["message"],
            format!("Application {name} deployment queued.")
        );
        assert!(e["deployment"].as_str().is_some_and(|d| !d.is_empty()));
    }
    let r = h
        .call(
            "GET",
            &format!("{EXT}/history?application={scheduler}"),
            Some("acme_owner_read"),
            None,
        )
        .await;
    assert_eq!(r.json()["history"].as_array().unwrap().len(), 1);

    // The deployments ran on the authority of the admin who granted deploy (Alice, user 1).
    let audit: Vec<(Option<i64>, Value)> = sqlx::query_as(
        "SELECT user_id, detail FROM coolify_rs_plugins.audit WHERE plugin_id = $1 AND action = 'deploy' ORDER BY id",
    )
    .bind(ID)
    .fetch_all(h.tdb.db.pool())
    .await
    .unwrap();
    assert_eq!(audit.len(), 2);
    for (user, detail) in &audit {
        assert_eq!(*user, Some(1));
        assert_eq!(detail["on_behalf_of"], "grantor");
        assert_eq!(detail["ok"], true);
    }

    // A branch filter: api's chain now runs for `release` only.
    let mut only_release = targets(&[(worker, false)]);
    only_release["branch"] = json!("release");
    let r = put_rule(&h, "acme_owner_root", api, only_release).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.json()["branch"], "release");
    succeeded(&h, "acme_owner_write", api, json!({})).await;
    assert_eq!(history(&h, "acme_owner_read").await.len(), 2);

    // Removing a rule: an empty list or DELETE.
    let r = put_rule(&h, "acme_owner_root", api, targets(&[])).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.json()["targets"], json!([]));
    let r = h
        .call(
            "DELETE",
            &format!("{EXT}/rules/{api}"),
            Some("acme_owner_root"),
            None,
        )
        .await;
    assert_eq!(r.status, 404, "{}", r.body);
    let r = h
        .call(
            "DELETE",
            &format!("{EXT}/rules/{scheduler}"),
            Some("acme_owner_root"),
            None,
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    let r = h
        .call(
            "GET",
            &format!("{EXT}/rules"),
            Some("acme_owner_read"),
            None,
        )
        .await;
    assert_eq!(r.json()["rules"], json!([]));
    h.tdb.drop_db().await;
}

#[tokio::test]
async fn consent_and_the_rate_limit_are_recorded_instead_of_deploying() {
    let (h, _tmp) = harness().await;
    let acme = team(&h, "acme_owner_write", "acme", &["api", "worker"]).await;
    let (api, worker) = (&acme[0], &acme[1]);
    install(&h).await;
    enable(&h, "acme_owner_write", true).await;
    let r = put_rule(&h, "acme_owner_root", api, targets(&[(worker, false)])).await;
    assert_eq!(r.status, 200, "{}", r.body);

    // The team withdraws its consent: the plugin stays on, deploys nothing and says why.
    enable(&h, "acme_owner_write", false).await;
    succeeded(&h, "acme_owner_write", api, json!({})).await;
    assert!(queued(&h, worker).await.is_empty());
    let hist = history(&h, "acme_owner_read").await;
    assert_eq!(hist.len(), 1, "{hist:?}");
    assert_eq!(hist[0]["status"], "failed");
    assert_eq!(
        hist[0]["message"],
        "the team has not granted deploy to this plugin"
    );
    assert_eq!(hist[0]["deployment"], Value::Null);

    // Granted again, but the team's plugins already queued 20 deployments in the last ten minutes.
    enable(&h, "acme_owner_write", true).await;
    sqlx::query(
        "INSERT INTO coolify_rs_plugins.audit (plugin_id, team_id, user_id, action, detail) \
         SELECT 'acme/other-plugin', 1, 1, 'deploy', '{}'::jsonb FROM generate_series(1, 20)",
    )
    .execute(h.tdb.db.pool())
    .await
    .unwrap();
    succeeded(&h, "acme_owner_write", api, json!({})).await;
    assert!(queued(&h, worker).await.is_empty());
    let hist = history(&h, "acme_owner_read").await;
    assert_eq!(hist.len(), 2, "{hist:?}");
    assert_eq!(hist[0]["status"], "failed");
    assert_eq!(
        hist[0]["message"],
        "the team's plugins may queue at most 20 deployments in 600 seconds"
    );

    // Once the window has passed, the chain deploys again.
    sqlx::query(
        "UPDATE coolify_rs_plugins.audit SET at = now() - interval '1 hour' WHERE action = 'deploy'",
    )
    .execute(h.tdb.db.pool())
    .await
    .unwrap();
    succeeded(&h, "acme_owner_write", api, json!({})).await;
    assert_eq!(
        queued(&h, worker).await,
        [("queued".to_owned(), true, false)]
    );
    assert_eq!(history(&h, "acme_owner_read").await[0]["status"], "queued");
    h.tdb.drop_db().await;
}

/// A response of the web UI's endpoints.
struct Res {
    status: u16,
    content_type: String,
    body: String,
}

impl Res {
    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or_else(|e| panic!("not JSON ({e}): {}", self.body))
    }
}

/// A signed-in browser's request: its cookie, and the CSRF token on writes.
async fn browse(
    h: &Harness,
    session: &(String, String),
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Res {
    let mut b = Request::builder()
        .method(method)
        .uri(path)
        .header("cookie", &session.0)
        .header("sec-fetch-dest", "iframe")
        .header("sec-fetch-site", "same-origin");
    if method != "GET" {
        b = b.header("x-csrf-token", &session.1);
    }
    let req = match body {
        Some(v) => b
            .header("content-type", "application/json")
            .body(Body::from(v.to_string()))
            .unwrap(),
        None => b.body(Body::empty()).unwrap(),
    };
    let res = h.app.clone().oneshot(req).await.unwrap();
    let status = res.status().as_u16();
    let content_type = res
        .headers()
        .get("content-type")
        .map(|v| v.to_str().unwrap().to_owned())
        .unwrap_or_default();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    Res {
        status,
        content_type,
        body: String::from_utf8(bytes.to_vec()).unwrap(),
    }
}

#[tokio::test]
async fn panels_show_the_chain_and_write_through_the_bridge() {
    let (h, _tmp) = harness().await;
    let acme = team(
        &h,
        "acme_owner_write",
        "acme",
        &["api", "worker", "scheduler"],
    )
    .await;
    let (api, worker, scheduler) = (&acme[0], &acme[1], &acme[2]);
    install(&h).await;
    enable(&h, "acme_owner_write", true).await;
    let alice = h.ui_login("alice@example.test").await;
    let bob = h.ui_login("bob@example.test").await;

    let nav = browse(&h, &bob, "GET", "/ui/plugins/nav", None)
        .await
        .json();
    assert_eq!(
        nav["panels"],
        json!([
            {"plugin": ID, "name": "Deploy chains", "verified": false, "slot": "application", "index": 0, "title": "Deploy chain"},
            {"plugin": ID, "name": "Deploy chains", "verified": false, "slot": "dashboard", "index": 0, "title": "Deploy chains"}
        ])
    );

    // Writes through the bridge: the session's role decides, as for the API's tokens.
    let bridge = format!("/ui/ext/{ID}/rules/{api}");
    let body = targets(&[(worker, true)]);
    let r = browse(&h, &bob, "PUT", &bridge, Some(body.clone())).await;
    assert_eq!(r.status, 403, "{}", r.body);
    let r = browse(&h, &alice, "PUT", &bridge, Some(body)).await;
    assert_eq!(r.status, 200, "{}", r.body);
    let r = browse(
        &h,
        &alice,
        "PUT",
        &format!("/ui/ext/{ID}/rules/{worker}"),
        Some(targets(&[(api, false)])),
    )
    .await;
    assert_eq!(r.status, 422, "{}", r.body);
    assert!(
        r.json()["errors"]["targets"][0]
            .as_str()
            .unwrap()
            .contains("loop")
    );
    succeeded(&h, "acme_owner_write", api, json!({})).await;

    // The application panel: the chain with names, the picker, and the history.
    let panel = format!("/ui/plugins/{ID}/panels/application?resource={api}");
    let r = browse(&h, &bob, "GET", &panel, None).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.content_type, "text/html; charset=utf-8");
    assert!(r.body.contains("After this application deploys, deploy:"));
    assert!(
        r.body.contains("coolify-rs:request"),
        "the bridge is included"
    );
    let data = panel_data(&r.body);
    assert_eq!(data["app"], api.as_str());
    assert_eq!(data["can_write"], false, "Bob is a member");
    assert_eq!(data["rule"]["targets"][0]["name"], "acme-worker");
    assert_eq!(data["rule"]["targets"][0]["force"], true);
    let picker: Vec<&str> = data["applications"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["name"].as_str().unwrap())
        .collect();
    assert_eq!(picker, ["acme-api", "acme-scheduler", "acme-worker"]);
    assert_eq!(data["history"][0]["target_name"], "acme-worker");
    assert_eq!(data["history"][0]["status"], "queued");
    let r = browse(&h, &alice, "GET", &panel, None).await;
    assert_eq!(panel_data(&r.body)["can_write"], true);

    // The target's panel lists what deploys it; one without a chain shows none.
    let r = browse(
        &h,
        &alice,
        "GET",
        &format!("/ui/plugins/{ID}/panels/application?resource={worker}"),
        None,
    )
    .await;
    let data = panel_data(&r.body);
    assert_eq!(data["sources"][0]["name"], "acme-api");
    assert_eq!(data["rule"]["targets"], json!([]));
    let r = browse(
        &h,
        &alice,
        "GET",
        &format!("/ui/plugins/{ID}/panels/application?resource={scheduler}"),
        None,
    )
    .await;
    assert_eq!(panel_data(&r.body)["sources"], json!([]));

    // The dashboard's overview lists every rule.
    let r = browse(
        &h,
        &bob,
        "GET",
        &format!("/ui/plugins/{ID}/panels/dashboard"),
        None,
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    let data = panel_data(&r.body);
    assert_eq!(data["rules"][0]["source"]["name"], "acme-api");
    assert_eq!(data["rules"][0]["targets"][0]["name"], "acme-worker");
    assert_eq!(data["history"].as_array().unwrap().len(), 1);
    h.tdb.drop_db().await;
}

/// The JSON a panel embeds in `<script type="application/json" id="data">`.
fn panel_data(body: &str) -> Value {
    let start = r#"<script type="application/json" id="data">"#;
    let from = body.find(start).expect("the panel embeds its data") + start.len();
    let to = from + body[from..].find("</script>").unwrap();
    serde_json::from_str(&body[from..to]).unwrap()
}
