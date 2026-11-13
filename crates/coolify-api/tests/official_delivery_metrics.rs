//! The first-party `coolify-rs/delivery-metrics` plugin (plugins/official/delivery-metrics) against the real
//! API: installed with its grant and enabled per team, it records deployment events, answers the four DORA
//! metrics and the CSV exports under `/api/v1/ext/...` for the calling team only, exports an inventory without
//! sensitive fields, follows the team's settings, and serves its panels through the UI panel endpoint.
//!
//! A stand-in for `ssh` answers the server jobs, and the server runs no builds (`concurrent_builds = 0`).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use base64::Engine as _;
use common::Harness;
use coolify_remote::{Remote, RemoteConfig};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

const ID: &str = "coolify-rs/delivery-metrics";
const EXT: &str = "/api/v1/ext/coolify-rs/delivery-metrics";
const ADMIN: Option<&str> = Some("root_root");
const FAKE_SSH: &str = "#!/bin/sh\ncat > /dev/null\necho ok\n";
const DB_PASSWORD: &str = "Pw-not-in-the-csv-31337";

fn plugins_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugins")
}

fn manifest() -> String {
    std::fs::read_to_string(plugins_dir().join("official/delivery-metrics/plugin.toml")).unwrap()
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
            std::fs::read(plugins_dir().join("target/wasm32-wasip2/release/delivery_metrics.wasm"))
                .unwrap();
        base64::engine::general_purpose::STANDARD.encode(wasm)
    })
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

/// Installs the plugin as the instance admin: the first request answers the capabilities, which must be
/// exactly these, and sending them back as `grant` installs it.
async fn install(h: &Harness) {
    let body = json!({"manifest": manifest(), "component": component()});
    let r = h
        .call("POST", "/api/v1/plugins", ADMIN, Some(body.clone()))
        .await;
    assert_eq!(r.status, 422, "{}", r.body);
    let grant = r.json()["capabilities"].clone();
    assert_eq!(
        grant,
        json!([
            "events: deployment.succeeded, deployment.failed",
            "storage",
            "routes",
            "resources: projects, applications, services, databases, servers",
            "ui: dashboard, application"
        ])
    );
    let mut body = body;
    body["grant"] = grant;
    let r = h.call("POST", "/api/v1/plugins", ADMIN, Some(body)).await;
    assert_eq!(r.status, 201, "{}", r.body);
}

async fn enable(h: &Harness, token: &str) {
    let r = h
        .call(
            "POST",
            &format!("/api/v1/plugins/{ID}/enable"),
            Some(token),
            None,
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
}

struct Team {
    server: String,
    project: String,
    app: String,
}

/// A key, a server that builds nothing, a project and a Docker image application, through the API.
async fn team(h: &Harness, token: &str, name: &str) -> Team {
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
    let ip = format!("203.0.113.{}", 50 + name.len());
    let server = uuid(
        h.call(
            "POST",
            "/api/v1/servers",
            t,
            Some(json!({"name": format!("{name}-box"), "ip": ip, "private_key_uuid": key})),
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
        h.call(
            "POST",
            "/api/v1/projects",
            t,
            Some(json!({"name": format!("{name}-shop")})),
        )
        .await,
    );
    let app = uuid(
        h.call(
            "POST",
            "/api/v1/applications/dockerimage",
            t,
            Some(json!({
                "server_uuid": server,
                "project_uuid": project,
                "environment_name": "production",
                "name": format!("{name}-web"),
                "docker_registry_image_name": "nginx",
                "docker_registry_image_tag": "alpine",
                "ports_exposes": "80",
                "instant_deploy": false,
            })),
        )
        .await,
    );
    Team {
        server,
        project,
        app,
    }
}

/// Delivers one event through the development test-event endpoint, synchronously.
async fn event(h: &Harness, token: &str, kind: &str, payload: Value) {
    let r = h
        .call(
            "POST",
            &format!("/api/v1/plugins/{ID}/test-event"),
            Some(token),
            Some(json!({"kind": kind, "payload": payload})),
        )
        .await;
    r.assert(200, r#"{"delivered":true}"#);
}

fn deployment(app: &str, name: &str, n: u32) -> Value {
    json!({
        "application_name": name,
        "application_uuid": app,
        "deployment_uuid": format!("dep-{app}-{n}"),
        "project_name": "Shop",
        "project_uuid": "p",
        "environment_name": "production",
        "environment_uuid": "e",
        "fqdn": null,
        "preview": null,
        "commit": {"sha": "0123456789abcdef0123456789abcdef01234567", "message": "m", "url": null, "branch": "main"}
    })
}

async fn get_json(h: &Harness, path: &str, token: &str) -> Value {
    let r = h
        .call("GET", &format!("{EXT}{path}"), Some(token), None)
        .await;
    assert_eq!(r.status, 200, "{path}: {}", r.body);
    assert_eq!(r.ctype, "application/json", "{path}");
    r.json()
}

async fn get_csv(h: &Harness, path: &str, token: &str) -> String {
    let r = h
        .call("GET", &format!("{EXT}{path}"), Some(token), None)
        .await;
    assert_eq!(r.status, 200, "{path}: {}", r.body);
    assert_eq!(r.ctype, "text/csv; charset=utf-8", "{path}: served inline");
    assert!(r.body.ends_with("\r\n"), "{path}");
    r.body
}

/// Rows of a CSV without quoted line breaks, split naively: enough for the exports here.
fn rows(csv: &str) -> Vec<Vec<String>> {
    csv.split("\r\n")
        .filter(|l| !l.is_empty())
        .map(|l| l.split(',').map(str::to_owned).collect())
        .collect()
}

#[tokio::test]
async fn metrics_and_exports_for_the_calling_team() {
    let (h, _tmp) = harness().await;
    let acme = team(&h, "acme_owner_write", "acme").await;
    let other = team(&h, "carol_root", "other").await;
    let r = h
        .call(
            "POST",
            "/api/v1/databases/postgresql",
            Some("acme_owner_write"),
            Some(json!({
                "server_uuid": acme.server,
                "project_uuid": acme.project,
                "environment_name": "production",
                "name": "acme-db",
                "postgres_password": DB_PASSWORD,
                "instant_deploy": false,
            })),
        )
        .await;
    assert!(r.status.is_success(), "{}", r.body);
    let database = r.json()["uuid"].as_str().unwrap().to_owned();

    install(&h).await;
    // Not enabled for the team yet.
    let r = h
        .call(
            "GET",
            &format!("{EXT}/metrics.json"),
            Some("acme_owner_read"),
            None,
        )
        .await;
    assert_eq!(r.status, 404, "{}", r.body);
    enable(&h, "acme_owner_write").await;
    enable(&h, "carol_root").await;

    // Nothing recorded yet.
    let v = get_json(&h, "/metrics.json", "acme_owner_read").await;
    assert_eq!(v["scope"], "team");
    assert_eq!(v["window_days"], 30);
    assert_eq!(v["metrics"]["deployments"], 0);
    assert_eq!(v["metrics"]["deployment_frequency"]["band"], Value::Null);
    assert_eq!(v["applications"], json!([]));

    // Acme: the web application fails, is restored, deploys again; a worker deploys once with a commit
    // time; a preview fails. The worker's name tries a formula.
    let web = acme.app.as_str();
    event(
        &h,
        "acme_owner_write",
        "deployment.failed",
        deployment(web, "acme-web", 1),
    )
    .await;
    event(
        &h,
        "acme_owner_write",
        "deployment.succeeded",
        deployment(web, "acme-web", 2),
    )
    .await;
    event(
        &h,
        "acme_owner_write",
        "deployment.succeeded",
        deployment(web, "acme-web", 3),
    )
    .await;
    let mut worker = deployment("worker-1", "=SUM(A1:A9)", 1);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    worker["commit"]["committed_at"] = json!(now - 7200);
    event(&h, "acme_owner_write", "deployment.succeeded", worker).await;
    let mut preview = deployment(web, "acme-web", 4);
    preview["preview"] = json!({"pull_request_id": 12, "fqdn": null});
    event(&h, "acme_owner_write", "deployment.failed", preview).await;
    // The same deployment delivered twice is recorded once.
    event(
        &h,
        "acme_owner_write",
        "deployment.succeeded",
        deployment(web, "acme-web", 3),
    )
    .await;
    // Carol's team: one failure of its own application.
    event(
        &h,
        "carol_root",
        "deployment.failed",
        deployment(&other.app, "other-web", 1),
    )
    .await;

    // Real events take the host's dispatch path too, for teams that enabled the plugin only.
    let host = h.state.plugins.clone().unwrap();
    let real = coolify_jobs::notifications::Deployment {
        application_name: "acme-web".into(),
        application_uuid: web.into(),
        deployment_uuid: "dispatched-1".into(),
        project_name: "acme-shop".into(),
        project_uuid: acme.project.clone(),
        environment_name: "production".into(),
        environment_uuid: "env".into(),
        fqdn: None,
        preview: None,
        commit: None,
        blocked_title: None,
        blocked_chat_title: None,
    };
    host.dispatch(
        1,
        "deployment.succeeded",
        serde_json::to_value(&real).unwrap(),
    );
    host.dispatch(0, "deployment.failed", serde_json::to_value(&real).unwrap());
    let mut v = Value::Null;
    for _ in 0..100 {
        v = get_json(&h, "/metrics.json", "acme_owner_read").await;
        if v["metrics"]["deployments"] == 5 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // Previews don't count by default: 5 deployments, 4 successful, 1 failed.
    let m = &v["metrics"];
    assert_eq!(
        (&m["deployments"], &m["succeeded"], &m["failed"]),
        (&json!(5), &json!(4), &json!(1)),
        "{v}"
    );
    assert_eq!(m["deployment_frequency"]["deployments"], 4);
    assert_eq!(m["deployment_frequency"]["per_day"], 0.133);
    assert_eq!(m["deployment_frequency"]["per_week"], 0.933);
    assert_eq!(m["deployment_frequency"]["band"], "high");
    assert_eq!(
        m["change_failure_rate"],
        json!({"failed": 1, "total": 5, "rate": 0.2, "band": "high"})
    );
    let restore = &m["time_to_restore"];
    assert_eq!(
        (&restore["restored"], &restore["unrestored"]),
        (&json!(1), &json!(0))
    );
    assert!(
        restore["median_seconds"].as_i64().unwrap() < 60,
        "{restore}"
    );
    assert_eq!(restore["band"], "elite");
    let lead = &m["lead_time"];
    assert_eq!(lead["available"], true, "the worker's commit time");
    assert_eq!(lead["samples"], 1);
    let secs = lead["median_seconds"].as_i64().unwrap();
    assert!((7200..7260).contains(&secs), "{secs}");
    assert_eq!(lead["band"], "high");
    // Names come from the API where the application exists.
    assert_eq!(v["applications_total"], 2);
    assert_eq!(v["applications"][0]["uuid"], web);
    assert_eq!(v["applications"][0]["name"], "acme-web");
    assert_eq!(v["applications"][0]["deployments"], 4);
    assert_eq!(v["applications"][1]["uuid"], "worker-1");
    assert_eq!(v["storage"]["deployments"], 6, "the preview is recorded");
    assert_eq!(v["recent"][0]["deployment_uuid"], "dispatched-1");

    let v90 = get_json(&h, "/metrics.json?window=90&top=1", "acme_owner_read").await;
    assert_eq!(v90["window_days"], 90);
    assert_eq!(v90["metrics"]["deployment_frequency"]["per_day"], 0.044);
    assert_eq!(v90["applications"].as_array().unwrap().len(), 1);
    let r = h
        .call(
            "GET",
            &format!("{EXT}/metrics.json?window=7"),
            Some("acme_owner_read"),
            None,
        )
        .await;
    r.assert(422, r#"{"message":"The window must be 30 or 90 (days)."}"#);
    let r = h
        .call(
            "GET",
            &format!("{EXT}/metrics.json?app=..%2Fx"),
            Some("acme_owner_read"),
            None,
        )
        .await;
    assert_eq!(r.status, 422, "{}", r.body);

    // One application.
    let a = get_json(&h, &format!("/metrics.json?app={web}"), "acme_owner_read").await;
    assert_eq!(a["scope"], "application");
    assert_eq!(a["application"], json!({"uuid": web, "name": "acme-web"}));
    assert_eq!(a["metrics"]["deployments"], 4);
    assert_eq!(a["metrics"]["change_failure_rate"]["rate"], 0.25);
    assert_eq!(a["recent"].as_array().unwrap().len(), 4);

    // Team isolation: Carol sees her failure and nothing of Acme's.
    let c = get_json(&h, "/metrics.json", "carol_root").await;
    assert_eq!(c["metrics"]["deployments"], 1);
    assert_eq!(c["metrics"]["change_failure_rate"]["rate"], 1.0);
    assert_eq!(c["metrics"]["time_to_restore"]["unrestored"], 1);
    assert_eq!(c["applications"][0]["uuid"], other.app.as_str());
    assert!(!c.to_string().contains(web));
    let c = get_json(&h, &format!("/metrics.json?app={web}"), "carol_root").await;
    assert_eq!(c["metrics"]["deployments"], 0);
    assert_eq!(c["application"]["name"], Value::Null);
    // The root team never enabled it.
    let r = h
        .call(
            "GET",
            &format!("{EXT}/metrics.json"),
            Some("root_root"),
            None,
        )
        .await;
    assert_eq!(r.status, 404, "{}", r.body);

    // The metrics as CSV: the team, then each application.
    let csv = get_csv(&h, "/metrics.csv", "acme_owner_read").await;
    let lines = rows(&csv);
    assert_eq!(lines[0].len(), 23);
    assert_eq!(
        lines[0][..4],
        [
            "scope",
            "application_uuid",
            "application_name",
            "window_days"
        ]
    );
    assert_eq!(lines[1][..4], ["team", "", "", "30"]);
    assert_eq!(lines[1][6..12], ["5", "4", "1", "0.133", "0.933", "high"]);
    assert_eq!(lines[2][..3], ["application", web, "acme-web"]);
    assert_eq!(lines[3][..3], ["application", "worker-1", "'=SUM(A1:A9)"]);
    assert_eq!(lines.len(), 4);
    assert!(
        get_csv(&h, "/metrics.csv?window=90", "acme_owner_read")
            .await
            .contains("\r\nteam,,,90,")
    );

    // Every recorded deployment, previews too, with whether it counts.
    let csv = get_csv(&h, "/deployments.csv", "acme_owner_read").await;
    let lines = rows(&csv);
    assert_eq!(
        lines[0],
        [
            "finished_at",
            "application_uuid",
            "application_name",
            "project",
            "environment",
            "outcome",
            "pull_request",
            "deployment_uuid",
            "commit_sha",
            "commit_at",
            "lead_time_seconds",
            "counted"
        ]
    );
    assert_eq!(lines.len(), 7);
    let preview_row = lines.iter().find(|l| l[6] == "12").unwrap();
    assert_eq!(preview_row[5], "failed");
    assert_eq!(preview_row[11], "no");
    let worker_row = lines.iter().find(|l| l[1] == "worker-1").unwrap();
    assert_eq!(worker_row[2], "'=SUM(A1:A9)", "formulas are neutralized");
    assert_eq!(worker_row[8], "0123456789abcdef0123456789abcdef01234567");
    assert!(worker_row[10].parse::<i64>().unwrap() >= 7200);
    assert_eq!(
        rows(
            &get_csv(
                &h,
                "/deployments.csv?window=90&app=worker-1",
                "acme_owner_read"
            )
            .await
        )
        .len(),
        2
    );
    let carol = get_csv(&h, "/deployments.csv", "carol_root").await;
    assert_eq!(rows(&carol).len(), 2);
    assert!(!carol.contains(web) && carol.contains(&other.app));

    // The team's settings: counting previews raises the failure rate, an environment filter hides all.
    let settings = format!("/api/v1/plugins/{ID}/settings");
    let r = h
        .call(
            "PATCH",
            &settings,
            Some("acme_owner_write"),
            Some(json!({"include_previews": "yes"})),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    let v = get_json(&h, "/metrics.json", "acme_owner_read").await;
    assert_eq!(v["settings"]["include_previews"], true);
    assert_eq!(v["metrics"]["change_failure_rate"]["rate"], 0.3333);
    assert_eq!(
        v["metrics"]["time_to_restore"]["unrestored"], 1,
        "the preview's own streak"
    );
    let r = h
        .call(
            "PATCH",
            &settings,
            Some("acme_owner_write"),
            Some(json!({"environments": "staging", "default_window": "90"})),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    let v = get_json(&h, "/metrics.json", "acme_owner_read").await;
    assert_eq!(v["window_days"], 90);
    assert_eq!(v["metrics"]["deployments"], 0);
    assert_eq!(
        get_json(&h, "/metrics.json", "carol_root").await["metrics"]["deployments"],
        1
    );

    // The inventory: the calling team's resources, the fields the API shows without read:sensitive.
    let sensitive = h
        .call(
            "GET",
            &format!("/api/v1/databases/{database}"),
            Some("acme_owner_sensitive"),
            None,
        )
        .await;
    assert!(
        sensitive.body.contains(DB_PASSWORD),
        "the API has the password for read:sensitive"
    );
    let csv = get_csv(&h, "/inventory.csv", "acme_owner_read").await;
    let lines = rows(&csv);
    assert_eq!(
        lines[0],
        [
            "kind",
            "name",
            "uuid",
            "type",
            "status",
            "address",
            "project",
            "environment"
        ]
    );
    let find = |kind: &str, uuid: &str| {
        lines
            .iter()
            .find(|l| l[0] == kind && l[2] == uuid)
            .unwrap_or_else(|| panic!("{kind} {uuid} in {csv}"))
            .clone()
    };
    assert_eq!(find("project", &acme.project)[1], "acme-shop");
    let app_row = find("application", web);
    assert_eq!(app_row[1], "acme-web");
    assert_eq!(app_row[3], "dockerimage");
    assert_eq!(app_row[6..], ["acme-shop", "production"]);
    let db_row = find("database", &database);
    assert_eq!(db_row[1], "acme-db");
    assert_eq!(db_row[6..], ["acme-shop", "production"]);
    let server_row = find("server", &acme.server);
    assert_eq!(server_row[1], "acme-box");
    assert_eq!(server_row[4], "reachable");
    assert_eq!(server_row[5], "203.0.113.54");
    assert!(
        lines
            .iter()
            .any(|l| l[0] == "environment" && l[1] == "production")
    );
    assert!(!csv.contains(DB_PASSWORD), "no sensitive fields");
    assert!(!csv.contains("PRIVATE KEY") && !csv.contains("postgres://"));
    for theirs in [&other.app, &other.project, &other.server] {
        assert!(
            !csv.contains(theirs.as_str()),
            "only the calling team's resources"
        );
    }
    let carol = get_csv(&h, "/inventory.csv", "carol_root").await;
    assert!(carol.contains(&other.app) && !carol.contains(web) && !carol.contains(&database));

    // Unknown routes and methods.
    let r = h
        .call("GET", &format!("{EXT}/nope"), Some("acme_owner_read"), None)
        .await;
    r.assert(404, r#"{"message":"Not found."}"#);
    let r = h
        .call(
            "POST",
            &format!("{EXT}/metrics.json"),
            Some("acme_owner_root"),
            Some(json!({})),
        )
        .await;
    r.assert(405, r#"{"message":"Method not allowed."}"#);
    h.tdb.drop_db().await;
}

struct Res {
    status: StatusCode,
    headers: HeaderMap,
    body: String,
}

/// A request with the session cookie, as the page or one of its frames sends it.
async fn browse(h: &Harness, cookie: &str, path: &str, frame: bool) -> Res {
    let mut b = Request::builder()
        .method("GET")
        .uri(path)
        .header("cookie", cookie);
    if frame {
        b = b
            .header("sec-fetch-dest", "iframe")
            .header("sec-fetch-site", "same-origin");
    }
    let res = h
        .app
        .clone()
        .oneshot(b.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    Res {
        status,
        headers,
        body: String::from_utf8(bytes.to_vec()).unwrap(),
    }
}

/// The JSON a panel carries for its first render.
fn initial(html: &str) -> Value {
    let start = html
        .find(r#"<script type="application/json" id="initial">"#)
        .unwrap();
    let rest = &html[start..];
    let json = &rest[rest.find('>').unwrap() + 1..rest.find("</script>").unwrap()];
    serde_json::from_str(json).unwrap()
}

#[tokio::test]
async fn panels_show_the_signed_in_teams_metrics() {
    let (h, _tmp) = harness().await;
    install(&h).await;
    enable(&h, "acme_owner_write").await;
    enable(&h, "carol_root").await;
    event(
        &h,
        "acme_owner_write",
        "deployment.failed",
        deployment("app-1", "<img src=x onerror=alert(1)>", 1),
    )
    .await;
    event(
        &h,
        "acme_owner_write",
        "deployment.succeeded",
        deployment("app-1", "web", 2),
    )
    .await;
    event(
        &h,
        "carol_root",
        "deployment.succeeded",
        deployment("app-9", "theirs", 1),
    )
    .await;

    let (alice, _) = h.ui_login("alice@example.test").await;
    let (carol, _) = h.ui_login("carol@example.test").await;
    let nav: Value =
        serde_json::from_str(&browse(&h, &alice, "/ui/plugins/nav", false).await.body).unwrap();
    assert_eq!(
        nav["panels"],
        json!([
            {"plugin": ID, "name": "Delivery metrics", "verified": false, "slot": "dashboard", "index": 0, "title": "Delivery metrics"},
            {"plugin": ID, "name": "Delivery metrics", "verified": false, "slot": "application", "index": 0, "title": "Delivery metrics"}
        ])
    );

    let dashboard = format!("/ui/plugins/{ID}/panels/dashboard");
    let r = browse(&h, &alice, &dashboard, true).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.headers["content-type"], "text/html; charset=utf-8");
    assert_eq!(
        r.headers["content-security-policy"],
        coolify_api::ui_plugins::PANEL_CSP
    );
    assert!(r.body.contains("coolify-rs:request"), "the bridge");
    assert!(r.body.contains(".dark .stat"), "the SDK's styles");
    assert!(r.body.contains(r#"data-window="90""#), "the window toggle");
    assert!(!r.body.contains("<img"), "names are only data");
    let v = initial(&r.body);
    assert_eq!(v["scope"], "team");
    assert_eq!(v["metrics"]["deployments"], 2);
    assert_eq!(v["metrics"]["change_failure_rate"]["rate"], 0.5);
    assert_eq!(v["applications"][0]["uuid"], "app-1");
    assert_eq!(v["applications"][0]["name"], "web");

    let r = browse(
        &h,
        &alice,
        &format!("/ui/plugins/{ID}/panels/application?resource=app-1"),
        true,
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert!(r.body.contains(r#"<div id="panel" data-app="app-1">"#));
    let v = initial(&r.body);
    assert_eq!(v["scope"], "application");
    assert_eq!(v["application"]["uuid"], "app-1");
    assert_eq!(v["recent"].as_array().unwrap().len(), 2);
    assert_eq!(v["metrics"]["time_to_restore"]["restored"], 1);

    // Carol's dashboard has her team's data only.
    let v = initial(&browse(&h, &carol, &dashboard, true).await.body);
    assert_eq!(v["metrics"]["deployments"], 1);
    assert_eq!(v["applications"][0]["uuid"], "app-9");

    // The window toggle's request goes through the bridge's endpoint, for the session's team.
    let r = browse(
        &h,
        &alice,
        &format!("/ui/ext/{ID}/metrics.json?window=90&app=app-1"),
        false,
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    let v: Value = serde_json::from_str(&r.body).unwrap();
    assert_eq!(
        (&v["window_days"], &v["metrics"]["deployments"]),
        (&json!(90), &json!(2))
    );
    h.tdb.drop_db().await;
}

/// Events of a team run at the same time; every deployment is still recorded once.
#[tokio::test]
async fn concurrent_events_are_all_recorded() {
    let h = Harness::with_plugins(true).await;
    install(&h).await;
    enable(&h, "acme_owner_write").await;
    let host = h.state.plugins.clone().unwrap();
    for n in 0..30 {
        let kind = if n % 3 == 0 {
            "deployment.failed"
        } else {
            "deployment.succeeded"
        };
        host.dispatch(1, kind, deployment(&format!("app-{}", n % 4), "web", n));
    }
    let mut v = Value::Null;
    for _ in 0..200 {
        v = get_json(&h, "/metrics.json", "acme_owner_read").await;
        if v["storage"]["deployments"] == 30 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(v["storage"]["deployments"], 30, "{v}");
    assert_eq!(v["metrics"]["deployments"], 30);
    assert_eq!(v["metrics"]["failed"], 10);
    assert_eq!(v["applications_total"], 4);
    h.tdb.drop_db().await;
}
