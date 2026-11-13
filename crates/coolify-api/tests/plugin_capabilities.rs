//! The data capabilities of ADR 0011 against the real API: a team admin must consent before a plugin may
//! deploy for the team; `resources` answers exactly what the API answers a token without `read:sensitive`,
//! for the calling team only; `deploy` goes through `POST /api/v1/deploy`'s code path and can't reach another
//! team's application; and the example `inventory` plugin lists applications created through the API.
//!
//! A stand-in for `ssh` answers the server jobs, and the server runs no builds at once
//! (`concurrent_builds = 0`), so a queued deployment never starts.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::OnceLock;

use base64::Engine as _;
use common::Harness;
use coolify_remote::{Remote, RemoteConfig};
use serde_json::{Value, json};

const ADMIN: Option<&str> = Some("root_root");
const HOSTILE: &str = "test/hostile";

const FAKE_SSH: &str = "#!/bin/sh\ncat > /dev/null\necho ok\n";

fn plugins_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugins")
}

/// A component from `plugins/`, built from source with the pinned toolchain the first time, as base64.
fn component(name: &str) -> String {
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
        std::fs::read(plugins_dir().join(format!("target/wasm32-wasip2/release/{name}.wasm")))
            .unwrap();
    base64::engine::general_purpose::STANDARD.encode(wasm)
}

const HOSTILE_MANIFEST: &str = r#"
id = "test/hostile"
name = "Hostile"
version = "0.1.0"
description = "Tries to reach other teams' resources."
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

/// Installs a plugin as the instance admin, approving exactly what it asks for.
async fn install(h: &Harness, manifest: &str, wasm: &str) {
    let body = json!({"manifest": manifest, "component": component(wasm)});
    let r = h
        .call("POST", "/api/v1/plugins", ADMIN, Some(body.clone()))
        .await;
    assert_eq!(r.status, 422, "{}", r.body);
    let mut body = body;
    body["grant"] = r.json()["capabilities"].clone();
    let r = h.call("POST", "/api/v1/plugins", ADMIN, Some(body)).await;
    assert_eq!(r.status, 201, "{}", r.body);
}

async fn enable(h: &Harness, id: &str, token: &str, body: Option<Value>) -> common::Resp {
    h.call(
        "POST",
        &format!("/api/v1/plugins/{id}/enable"),
        Some(token),
        body,
    )
    .await
}

struct Team {
    server: String,
    project: String,
    app: String,
}

/// A key, a usable server that builds nothing, a project and a Docker image application, all through the
/// API with `token`.
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
            Some(json!({"name": name, "ip": ip, "private_key_uuid": key})),
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
        h.call("POST", "/api/v1/projects", t, Some(json!({"name": name})))
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

#[tokio::test]
async fn enabling_a_deploy_plugin_needs_the_teams_consent() {
    let (h, _tmp) = harness().await;
    install(&h, HOSTILE_MANIFEST, "hostile").await;

    // The instance admin's grant is not the team's: enabling asks the team admin explicitly.
    let r = enable(&h, HOSTILE, "acme_owner_write", None).await;
    assert_eq!(r.status, 422, "{}", r.body);
    assert!(
        r.json()["message"]
            .as_str()
            .unwrap()
            .contains("grant_deploy")
    );
    assert!(
        r.json()["capabilities"]
            .as_array()
            .unwrap()
            .contains(&json!("deploy"))
    );
    let r = enable(
        &h,
        HOSTILE,
        "acme_owner_write",
        Some(json!({"grant_deploy": "yes"})),
    )
    .await;
    assert_eq!(r.status, 422, "{}", r.body);
    let show = format!("/api/v1/plugins/{HOSTILE}");
    let r = h.call("GET", &show, Some("acme_owner_read"), None).await;
    assert_eq!(r.json()["enabled_for_team"], false, "{}", r.body);

    // Members can't answer for the team.
    let r = enable(
        &h,
        HOSTILE,
        "acme_member_write_legacy",
        Some(json!({"grant_deploy": true})),
    )
    .await;
    assert_eq!(r.status, 403, "{}", r.body);

    let r = enable(
        &h,
        HOSTILE,
        "acme_owner_write",
        Some(json!({"grant_deploy": false})),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.json()["enabled_for_team"], true);
    assert_eq!(r.json()["deploy_granted_for_team"], false);
    let r = enable(
        &h,
        HOSTILE,
        "acme_owner_write",
        Some(json!({"grant_deploy": true})),
    )
    .await;
    assert_eq!(r.json()["deploy_granted_for_team"], true, "{}", r.body);
    let r = h.call("GET", &show, Some("acme_owner_read"), None).await;
    assert_eq!(r.json()["deploy_granted_for_team"], true, "{}", r.body);
    let r = h.call("GET", &show, Some("carol_root"), None).await;
    assert_eq!(r.json()["deploy_granted_for_team"], false, "{}", r.body);

    // Disabling forgets the consent.
    let r = h
        .call(
            "POST",
            &format!("/api/v1/plugins/{HOSTILE}/disable"),
            Some("acme_owner_write"),
            None,
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    let r = h.call("GET", &show, Some("acme_owner_read"), None).await;
    assert_eq!(r.json()["deploy_granted_for_team"], false, "{}", r.body);

    // A plugin without `deploy` needs no answer, and a consent sent anyway is not recorded.
    let inventory =
        std::fs::read_to_string(plugins_dir().join("examples/inventory/plugin.toml")).unwrap();
    install(&h, &inventory, "inventory").await;
    let r = enable(&h, "coolify-rs/inventory", "acme_owner_write", None).await;
    assert_eq!(r.status, 200, "{}", r.body);
    let r = enable(
        &h,
        "coolify-rs/inventory",
        "acme_owner_write",
        Some(json!({"grant_deploy": true})),
    )
    .await;
    assert_eq!(r.json()["deploy_granted_for_team"], false, "{}", r.body);

    let audit: Vec<(String, Option<i64>, Option<i64>)> = sqlx::query_as(
        "SELECT action, team_id, user_id FROM coolify_rs_plugins.audit \
         WHERE plugin_id = $1 AND action LIKE 'team_%' ORDER BY id",
    )
    .bind(HOSTILE)
    .fetch_all(h.tdb.db.pool())
    .await
    .unwrap();
    let actions: Vec<&str> = audit.iter().map(|(a, ..)| a.as_str()).collect();
    assert_eq!(
        actions,
        [
            "team_enabled",
            "team_deploy_withdrawn",
            "team_enabled",
            "team_deploy_granted",
            "team_disabled"
        ]
    );
    assert!(audit.iter().all(|(_, t, u)| *t == Some(1) && *u == Some(1)));
    h.tdb.drop_db().await;
}

#[tokio::test]
async fn inventory_lists_the_calling_teams_applications() {
    let (h, _tmp) = harness().await;
    let acme = team(&h, "acme_owner_write", "acme").await;
    let other = team(&h, "carol_root", "other").await;
    let inventory =
        std::fs::read_to_string(plugins_dir().join("examples/inventory/plugin.toml")).unwrap();
    install(&h, &inventory, "inventory").await;
    for token in ["acme_owner_write", "carol_root"] {
        assert_eq!(
            enable(&h, "coolify-rs/inventory", token, None).await.status,
            200
        );
    }
    let list = "/api/v1/ext/coolify-rs/inventory/applications";
    for (token, own, name) in [
        ("acme_owner_read", &acme, "acme-web"),
        ("carol_root", &other, "other-web"),
    ] {
        let r = h.call("GET", list, Some(token), None).await;
        assert_eq!(r.status, 200, "{}", r.body);
        let api = h
            .call("GET", "/api/v1/applications", Some(token), None)
            .await
            .json();
        let a = &api[0];
        assert_eq!(
            r.json(),
            json!([{
                "uuid": own.app,
                "name": name,
                "status": a["status"],
                "build_pack": "dockerimage",
                "fqdn": a["fqdn"],
            }]),
            "{token}"
        );
    }
    let one = |uuid: &str| format!("{list}/{uuid}");
    let r = h
        .call("GET", &one(&acme.app), Some("acme_owner_read"), None)
        .await;
    let api = h
        .call(
            "GET",
            &format!("/api/v1/applications/{}", acme.app),
            Some("acme_owner_read"),
            None,
        )
        .await;
    assert_eq!((r.status.as_u16(), r.json()), (200, api.json()));
    let r = h
        .call("GET", &one(&other.app), Some("acme_owner_read"), None)
        .await;
    r.assert(404, r#"{"message":"Application not found."}"#);
    h.tdb.drop_db().await;
}

#[tokio::test]
async fn resources_are_what_the_api_shows_a_token_without_read_sensitive() {
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
                "instant_deploy": false,
            })),
        )
        .await;
    assert!(r.status.is_success(), "{}", r.body);
    let database = r.json()["uuid"].as_str().unwrap().to_owned();
    install(&h, HOSTILE_MANIFEST, "hostile").await;
    let r = enable(
        &h,
        HOSTILE,
        "acme_owner_write",
        Some(json!({"grant_deploy": false})),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);

    let ext = |path: &str| format!("/api/v1/ext/{HOSTILE}{path}");
    let same = |plugin: common::Resp, api: common::Resp, what: &str| {
        assert_eq!(api.status, 200, "{what}: {}", api.body);
        assert_eq!(plugin.status, 200, "{what}: {}", plugin.body);
        assert_eq!(plugin.body, api.body, "{what}");
    };
    // Byte for byte what the API answers a token with `read` and without `read:sensitive`.
    for kind in [
        "projects",
        "servers",
        "applications",
        "services",
        "databases",
    ] {
        let plugin = h
            .call(
                "GET",
                &ext(&format!("/resources/{kind}")),
                Some("acme_owner_root"),
                None,
            )
            .await;
        let api = h
            .call(
                "GET",
                &format!("/api/v1/{kind}"),
                Some("acme_owner_read"),
                None,
            )
            .await;
        same(plugin, api, kind);
    }
    for (kind, uuid) in [
        ("projects", &acme.project),
        ("servers", &acme.server),
        ("applications", &acme.app),
        ("databases", &database),
    ] {
        let plugin = h
            .call(
                "GET",
                &ext(&format!("/resources/{kind}/{uuid}")),
                Some("acme_owner_read"),
                None,
            )
            .await;
        let api = h
            .call(
                "GET",
                &format!("/api/v1/{kind}/{uuid}"),
                Some("acme_owner_read"),
                None,
            )
            .await;
        same(plugin, api, kind);
    }
    let plugin = h
        .call(
            "GET",
            &ext(&format!("/environments/{}", acme.project)),
            Some("acme_owner_read"),
            None,
        )
        .await;
    let api = h
        .call(
            "GET",
            &format!("/api/v1/projects/{}/environments", acme.project),
            Some("acme_owner_read"),
            None,
        )
        .await;
    same(plugin, api, "environments");

    // Secrets a sensitive token sees are not there.
    let sensitive = h
        .call(
            "GET",
            &format!("/api/v1/databases/{database}"),
            Some("acme_owner_sensitive"),
            None,
        )
        .await;
    assert!(
        sensitive.json()["postgres_password"].is_string(),
        "{}",
        sensitive.body
    );
    let plugin = h
        .call(
            "GET",
            &ext(&format!("/resources/databases/{database}")),
            Some("acme_owner_read"),
            None,
        )
        .await;
    assert!(
        plugin.json().get("postgres_password").is_none(),
        "{}",
        plugin.body
    );
    assert!(
        !plugin
            .body
            .contains(sensitive.json()["postgres_password"].as_str().unwrap())
    );

    // Another team's resources do not exist for this team's calls, whatever the plugin asks for.
    for (kind, uuid) in [
        ("projects", &other.project),
        ("servers", &other.server),
        ("applications", &other.app),
    ] {
        let r = h
            .call(
                "GET",
                &ext(&format!("/resources/{kind}/{uuid}")),
                Some("acme_owner_root"),
                None,
            )
            .await;
        assert_eq!(r.status, 404, "{kind}: {}", r.body);
    }
    let r = h
        .call(
            "GET",
            &ext(&format!("/environments/{}", other.project)),
            Some("acme_owner_read"),
            None,
        )
        .await;
    assert_eq!(r.status, 404, "{}", r.body);
    let all = h
        .call(
            "GET",
            &ext("/resources/applications"),
            Some("acme_owner_read"),
            None,
        )
        .await;
    assert!(!all.body.contains(&other.app), "{}", all.body);
    h.tdb.drop_db().await;
}

#[tokio::test]
async fn deploys_take_the_api_path_for_the_calling_team_only() {
    let (h, _tmp) = harness().await;
    let acme = team(&h, "acme_owner_write", "acme").await;
    let other = team(&h, "carol_root", "other").await;
    install(&h, HOSTILE_MANIFEST, "hostile").await;
    for token in ["acme_owner_write", "carol_root"] {
        let r = enable(&h, HOSTILE, token, Some(json!({"grant_deploy": true}))).await;
        assert_eq!(r.status, 200, "{}", r.body);
    }
    let harness = &h;
    let deploy = |token: &'static str, uuid: &str| {
        let path = format!("/api/v1/ext/{HOSTILE}/deploy?uuid={uuid}&force=1");
        async move { harness.call("POST", &path, Some(token), None).await }
    };

    // On the caller's authority: their token must be able to deploy through the API.
    let r = deploy("acme_owner_read", &acme.app).await;
    r.assert(422, r#"{"error":"Missing required permissions: deploy"}"#);
    let r = deploy("acme_owner_root", &acme.app).await;
    assert_eq!(r.status, 200, "{}", r.body);
    let answer = r.json();
    assert_eq!(
        answer["deployments"][0]["message"],
        "Application acme-web deployment queued."
    );
    assert_eq!(answer["deployments"][0]["resource_uuid"], acme.app.as_str());
    assert_eq!(
        queued(&h, &acme.app).await,
        [("queued".to_owned(), true, true)]
    );

    // Another team's application does not exist for this team, and services or databases are not applications.
    let r = deploy("carol_root", &acme.app).await;
    r.assert(422, r#"{"error":"Application not found."}"#);
    let r = deploy("acme_owner_root", &acme.project).await;
    r.assert(422, r#"{"error":"Application not found."}"#);
    let r = deploy("acme_owner_root", &format!("{},{}", acme.app, other.app)).await;
    r.assert(422, r#"{"error":"Application not found."}"#);
    assert_eq!(queued(&h, &acme.app).await.len(), 1);
    assert!(queued(&h, &other.app).await.is_empty());

    // From an event, on the authority of the team admin who granted it (Carol, user 3).
    let host = h.state.plugins.clone().unwrap();
    host.call_event(
        HOSTILE,
        coolify_plugins::Event {
            kind: "deployment.succeeded".into(),
            version: 1,
            team_id: 2,
            payload: json!({"do": "deploy", "uuid": other.app}).to_string(),
        },
    )
    .await
    .unwrap();
    let stored = coolify_plugins::store::kv_get(host.db(), HOSTILE, 2, "deployed")
        .await
        .unwrap()
        .unwrap();
    let stored: Value = serde_json::from_slice(&stored).unwrap();
    assert_eq!(
        stored["deployments"][0]["message"], "Application other-web deployment queued.",
        "{stored}"
    );
    assert_eq!(
        queued(&h, &other.app).await,
        [("queued".to_owned(), true, false)]
    );

    // A grantor who is no longer a team admin deploys nothing.
    sqlx::query("UPDATE team_user SET role = 'member' WHERE team_id = 2 AND user_id = 3")
        .execute(h.tdb.db.pool())
        .await
        .unwrap();
    host.call_event(
        HOSTILE,
        coolify_plugins::Event {
            kind: "deployment.succeeded".into(),
            version: 1,
            team_id: 2,
            payload: json!({"do": "deploy", "uuid": other.app}).to_string(),
        },
    )
    .await
    .unwrap();
    let stored = coolify_plugins::store::kv_get(host.db(), HOSTILE, 2, "deployed")
        .await
        .unwrap()
        .unwrap();
    let stored: Value = serde_json::from_slice(&stored).unwrap();
    assert!(
        stored["error"]
            .as_str()
            .unwrap()
            .contains("no longer an admin"),
        "{stored}"
    );
    assert_eq!(queued(&h, &other.app).await.len(), 1);

    let audit: Vec<(String, i64, Value)> = sqlx::query_as(
        "SELECT plugin_id, team_id, detail FROM coolify_rs_plugins.audit WHERE action = 'deploy' ORDER BY id",
    )
    .fetch_all(h.tdb.db.pool())
    .await
    .unwrap();
    let seen: Vec<(i64, &str, bool)> = audit
        .iter()
        .map(|(p, t, d)| {
            assert_eq!(p, HOSTILE);
            (
                *t,
                d["on_behalf_of"].as_str().unwrap(),
                d["ok"].as_bool().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        seen,
        [
            (1, "caller", false),
            (1, "caller", true),
            (2, "caller", false),
            (1, "caller", false),
            (2, "grantor", true),
            (2, "grantor", false),
        ]
    );
    h.tdb.drop_db().await;
}
