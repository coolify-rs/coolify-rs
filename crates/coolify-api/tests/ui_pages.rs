//! Web UI read models and writes that the REST API has no endpoint for
//! (`/ui/dashboard`, `/ui/projects/{uuid}`, `/ui/keys*`, `/ui/team*`).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::Harness;
use serde_json::{Value, json};

struct Ui<'a> {
    h: &'a Harness,
    cookie: String,
    csrf: String,
}

impl Ui<'_> {
    async fn login<'a>(h: &'a Harness, email: &str) -> Ui<'a> {
        let (cookie, csrf) = h.ui_login(email).await;
        Ui { h, cookie, csrf }
    }

    async fn send(&self, method: &str, path: &str, body: Option<Value>) -> common::Resp {
        self.h
            .request(
                method,
                path,
                &[("cookie", &self.cookie), ("x-csrf-token", &self.csrf)],
                body,
            )
            .await
    }

    async fn get(&self, path: &str) -> Value {
        let r = self.send("GET", path, None).await;
        assert_eq!(r.status, 200, "{path}: {}", r.body);
        r.json()
    }
}

async fn exec(h: &Harness, sql: &str) {
    sqlx::raw_sql(sql).execute(h.tdb.db.pool()).await.unwrap();
}

#[tokio::test]
async fn dashboard_and_project_counts() {
    let h = Harness::new().await;
    let alice = Ui::login(&h, "alice@example.test").await;
    let r = alice
        .send("POST", "/ui/api/v1/projects", Some(json!({"name": "Web"})))
        .await;
    assert_eq!(r.status, 201, "{}", r.body);
    let uuid = r.json()["uuid"].as_str().unwrap().to_string();
    // One live and one trashed service in the production environment.
    exec(
        &h,
        &format!(
            "INSERT INTO services (uuid, name, environment_id, docker_compose_raw) \
             SELECT 's1', 'one', e.id, '' FROM environments e JOIN projects p ON p.id = e.project_id WHERE p.uuid = '{uuid}'"
        ),
    )
    .await;
    exec(
        &h,
        &format!(
            "INSERT INTO services (uuid, name, environment_id, docker_compose_raw, deleted_at) \
             SELECT 's2', 'gone', e.id, '', now() FROM environments e JOIN projects p ON p.id = e.project_id WHERE p.uuid = '{uuid}'"
        ),
    )
    .await;

    let d = alice.get("/ui/dashboard").await;
    let p = d["projects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["uuid"] == uuid.as_str())
        .unwrap();
    assert_eq!(p["resources"], 1);
    assert_eq!(p["environments"][0]["name"], "production");
    assert_eq!(d["servers"], json!([]));
    assert_eq!(d["private_keys"], 0);
    assert_eq!(d["can_create"], true);

    let page = alice.get(&format!("/ui/projects/{uuid}")).await;
    assert_eq!(page["project"]["name"], "Web");
    assert_eq!(page["environments"][0]["resources"], 1);
    assert!(
        page["projects"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x["uuid"] == uuid.as_str())
    );

    // A member reads but cannot create; another team cannot see the project.
    let bob = Ui::login(&h, "bob@example.test").await;
    assert_eq!(bob.get("/ui/dashboard").await["can_create"], false);
    let carol = Ui::login(&h, "carol@example.test").await;
    carol
        .send("GET", &format!("/ui/projects/{uuid}"), None)
        .await
        .assert(404, r#"{"message":"Project not found."}"#);
    // No session, no data.
    assert_eq!(
        h.request("GET", "/ui/dashboard", &[], None).await.status,
        401
    );
    h.finish().await;
}

#[tokio::test]
async fn server_status_labels() {
    let h = Harness::new().await;
    exec(
        &h,
        "INSERT INTO private_keys (id, uuid, name, private_key, team_id) VALUES (900, 'pk', 'k', 'x', 1)",
    )
    .await;
    exec(
        &h,
        "INSERT INTO servers (id, uuid, name, ip, team_id, private_key_id, proxy) VALUES \
         (900, 'a', 'alpha', '10.0.0.1', 1, 900, '{\"type\":\"TRAEFIK\",\"status\":\"exited\"}'), \
         (901, 'b', 'beta', '10.0.0.2', 1, 900, '{}')",
    )
    .await;
    exec(
        &h,
        "INSERT INTO server_settings (server_id, is_reachable, is_usable) VALUES (900, true, true), (901, true, false)",
    )
    .await;
    let alice = Ui::login(&h, "alice@example.test").await;
    let d = alice.get("/ui/dashboard").await;
    let statuses: Vec<(String, String)> = d["servers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            (
                s["status"].as_str().unwrap().to_string(),
                s["status_type"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(
        statuses,
        [
            ("Attention required".into(), "warning".into()),
            ("Not ready".into(), "warning".into())
        ]
    );
    assert_eq!(d["private_keys"], 1);
    h.finish().await;
}

#[tokio::test]
async fn keys_generate_list_and_cleanup() {
    let h = Harness::new().await;
    let alice = Ui::login(&h, "alice@example.test").await;
    for kind in ["ed25519", "rsa"] {
        let r = alice
            .send("POST", "/ui/keys/generate", Some(json!({"type": kind})))
            .await;
        assert_eq!(r.status, 201, "{}", r.body);
    }
    alice
        .send("POST", "/ui/keys/generate", Some(json!({"type": "dsa"})))
        .await
        .assert(
            400,
            r#"{"message":"Invalid request.","error":"Invalid private key type."}"#,
        );
    let list = alice.get("/ui/keys").await;
    let keys = list["keys"].as_array().unwrap();
    assert_eq!(keys.len(), 2);
    assert!(
        keys.iter()
            .all(|k| k["description"] == "Created by Coolify")
    );
    assert!(keys.iter().all(|k| k["in_use"] == false));

    // The generated keys are real: the REST API derives their public keys.
    let uuid = keys[0]["uuid"].as_str().unwrap();
    let key = alice.get(&format!("/ui/api/v1/security/keys/{uuid}")).await;
    assert!(key["public_key"].as_str().unwrap().starts_with("ssh-"));

    // A server uses one key; cleanup deletes only the other.
    exec(
        &h,
        &format!(
            "INSERT INTO servers (uuid, name, ip, team_id, private_key_id) \
             SELECT 'srv', 'srv', '10.0.0.9', 1, id FROM private_keys WHERE uuid = '{uuid}'"
        ),
    )
    .await;
    let r = alice
        .send("POST", "/ui/keys/cleanup", Some(json!({})))
        .await;
    r.assert(200, r#"{"deleted":1}"#);
    let left = alice.get("/ui/keys").await;
    assert_eq!(left["keys"].as_array().unwrap().len(), 1);
    assert_eq!(left["keys"][0]["in_use"], true);

    // Inspect derives the public key of a pasted key, or null.
    let pem = coolify_compat::sshkey::generate(coolify_compat::sshkey::KeyKind::Ed25519).unwrap();
    let r = alice
        .send(
            "POST",
            "/ui/keys/inspect",
            Some(json!({"private_key": pem})),
        )
        .await
        .json();
    assert!(
        r["public_key"]
            .as_str()
            .unwrap()
            .starts_with("ssh-ed25519 ")
    );
    let r = alice
        .send(
            "POST",
            "/ui/keys/inspect",
            Some(json!({"private_key": "nope"})),
        )
        .await;
    r.assert(200, r#"{"public_key":null}"#);

    // Members cannot generate or clean up.
    let bob = Ui::login(&h, "bob@example.test").await;
    assert_eq!(
        bob.send("POST", "/ui/keys/generate", Some(json!({"type": "rsa"})))
            .await
            .status,
        403
    );
    assert_eq!(
        bob.send("POST", "/ui/keys/cleanup", Some(json!({})))
            .await
            .status,
        403
    );
    h.finish().await;
}

#[tokio::test]
async fn team_settings_update() {
    let h = Harness::new().await;
    let alice = Ui::login(&h, "alice@example.test").await;
    let team = alice.get("/ui/team").await;
    assert_eq!(team["team"]["name"], "Acme");
    assert_eq!(team["team"]["is_mcp_server_enabled"], true);
    assert_eq!(team["members"].as_array().unwrap().len(), 2);
    assert_eq!(team["members"][0]["two_factor"], false);

    let body = |name: &str, twofa: bool| {
        json!({
            "name": name,
            "description": "Docs team",
            "is_mcp_server_enabled": false,
            "is_build_server_fallback_enabled": false,
            "is_two_factor_required": twofa,
        })
    };
    alice
        .send("PATCH", "/ui/team", Some(body("", false)))
        .await
        .assert(
            422,
            r#"{"message":"Validation failed.","errors":{"name":["The Name field is required."]}}"#,
        );
    // Requiring 2FA needs it on the acting account first.
    alice.send("PATCH", "/ui/team", Some(body("Acme", true))).await.assert(
        422,
        r#"{"message":"Enable two-factor authentication on your profile before you require it for the team."}"#,
    );
    let r = alice
        .send("PATCH", "/ui/team", Some(body("Acme Inc", false)))
        .await;
    r.assert(200, r#"{"message":"Team updated."}"#);
    let team = alice.get("/ui/team").await;
    assert_eq!(team["team"]["name"], "Acme Inc");
    assert_eq!(team["team"]["description"], "Docs team");
    assert_eq!(team["team"]["is_build_server_fallback_enabled"], false);
    // The session reflects the new name.
    let info = alice.get("/ui/session").await;
    assert_eq!(info["current_team"]["name"], "Acme Inc");

    let bob = Ui::login(&h, "bob@example.test").await;
    assert_eq!(
        bob.send("PATCH", "/ui/team", Some(body("Mine", false)))
            .await
            .status,
        403
    );
    // Members do not see others' 2FA state.
    assert_eq!(
        bob.get("/ui/team").await["members"][0]["two_factor"],
        Value::Null
    );
    h.finish().await;
}

#[tokio::test]
async fn member_roles_and_removal() {
    let h = Harness::new().await;
    let bob = Ui::login(&h, "bob@example.test").await;
    bob.send(
        "POST",
        "/ui/team/members/1/role",
        Some(json!({"role": "member"})),
    )
    .await
    .assert(
        403,
        r#"{"message":"You are not authorized to perform this action.","error":"Unauthorized"}"#,
    );

    let alice = Ui::login(&h, "alice@example.test").await;
    let r = alice
        .send(
            "POST",
            "/ui/team/members/2/role",
            Some(json!({"role": "admin"})),
        )
        .await;
    r.assert(200, r#"{"message":"Member updated."}"#);
    let role: String =
        sqlx::query_scalar("SELECT role FROM team_user WHERE team_id = 1 AND user_id = 2")
            .fetch_one(h.tdb.db.pool())
            .await
            .unwrap();
    assert_eq!(role, "admin");
    // A promotion keeps the member's tokens for this team (4.5.3); a demotion revokes them
    // (tests/permissions_4_5_3.rs).
    let tokens: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM personal_access_tokens WHERE tokenable_id = 2 AND team_id = '1'",
    )
    .fetch_one(h.tdb.db.pool())
    .await
    .unwrap();
    assert_eq!(tokens, 2);

    // An admin cannot act on an owner.
    let bob = Ui::login(&h, "bob@example.test").await;
    bob.send("DELETE", "/ui/team/members/1", None).await.assert(
        403,
        r#"{"message":"You are not authorized to perform this action."}"#,
    );
    // Removal detaches the member and clears their stored team.
    exec(&h, "UPDATE users SET current_team_id = 1 WHERE id = 2").await;
    alice
        .send("DELETE", "/ui/team/members/2", None)
        .await
        .assert(200, r#"{"message":"Member removed."}"#);
    let left: i64 =
        sqlx::query_scalar("SELECT count(*) FROM team_user WHERE team_id = 1 AND user_id = 2")
            .fetch_one(h.tdb.db.pool())
            .await
            .unwrap();
    assert_eq!(left, 0);
    let current: Option<i64> = sqlx::query_scalar("SELECT current_team_id FROM users WHERE id = 2")
        .fetch_one(h.tdb.db.pool())
        .await
        .unwrap();
    assert_eq!(current, None);
    alice
        .send("DELETE", "/ui/team/members/2", None)
        .await
        .assert(404, r#"{"message":"Member not found."}"#);
    h.finish().await;
}

#[tokio::test]
async fn servers_form_create_list_and_delete() {
    let h = Harness::new().await;
    let alice = Ui::login(&h, "alice@example.test").await;
    let r = alice
        .send(
            "POST",
            "/ui/keys/generate",
            Some(json!({"type": "ed25519"})),
        )
        .await;
    assert_eq!(r.status, 201, "{}", r.body);
    let form = alice.get("/ui/servers/form").await;
    let key_id = form["keys"][0]["id"].as_i64().unwrap();
    assert_eq!(form["name"].as_str().unwrap().split('-').count(), 3);

    // Livewire ByIp messages.
    let r = alice
        .send("POST", "/ui/servers", Some(json!({"name": "web", "ip": "", "user": "", "port": 0, "server_role": "x", "private_key_id": key_id})))
        .await;
    assert_eq!(
        r.json()["errors"],
        json!({
            "ip": ["The IP Address/Domain is required."],
            "user": ["The User field is required."],
            "port": ["The Port field must be between 1 and 65535."],
            "server_role": ["The selected Server Role is invalid."]
        })
    );
    let body = json!({"name": "web", "ip": "10.9.9.9", "user": "root", "port": 22, "server_role": "both", "private_key_id": key_id});
    let r = alice.send("POST", "/ui/servers", Some(body.clone())).await;
    assert_eq!(r.status, 201, "{}", r.body);
    let uuid = r.json()["uuid"].as_str().unwrap().to_string();
    alice.send("POST", "/ui/servers", Some(body)).await.assert(
        422,
        r#"{"message":"A server with this IP\/Domain already exists."}"#,
    );
    // ByIp sets the proxy status before the type, and saves the default role
    // without touching the settings row.
    let (proxy, role_updated): (String, bool) = sqlx::query_as(
        "SELECT s.proxy::text, ss.updated_at > ss.created_at FROM servers s JOIN server_settings ss ON ss.server_id = s.id WHERE s.uuid = $1",
    )
    .bind(&uuid)
    .fetch_one(h.tdb.db.pool())
    .await
    .unwrap();
    assert_eq!(
        proxy,
        r#"{"redirect_enabled":true,"status":"exited","type":"TRAEFIK"}"#
    );
    assert!(!role_updated);

    let list = alice.get("/ui/servers").await;
    let s = &list["servers"][0];
    assert_eq!(s["status"], "Validation required");
    assert_eq!(s["functional"], false);
    assert_eq!(list["confirm"], json!({"text": true, "password": true}));

    // Deleting from the UI asks for the account password.
    alice
        .send(
            "POST",
            &format!("/ui/servers/{uuid}/delete"),
            Some(json!({"password": "nope"})),
        )
        .await
        .assert(422, r#"{"message":"The provided password is incorrect."}"#);
    let bob = Ui::login(&h, "bob@example.test").await;
    assert_eq!(
        bob.send(
            "POST",
            &format!("/ui/servers/{uuid}/delete"),
            Some(json!({"password": common::PASSWORD}))
        )
        .await
        .status,
        403
    );
    alice
        .send(
            "POST",
            &format!("/ui/servers/{uuid}/delete"),
            Some(json!({"password": common::PASSWORD})),
        )
        .await
        .assert(200, r#"{"message":"Server deleted."}"#);
    assert_eq!(alice.get("/ui/servers").await["servers"], json!([]));
    h.finish().await;
}

/// `ActiveDeployments`: the five newest unfinished and the five newest finished deployments of
/// the team's applications, deleted ones included; the finished list reads at most five per app.
#[tokio::test]
async fn dashboard_recent_deployments() {
    let h = Harness::new().await;
    let alice = Ui::login(&h, "alice@example.test").await;
    let r = alice
        .send(
            "POST",
            "/ui/api/v1/projects",
            Some(json!({"name": "Deploys"})),
        )
        .await;
    assert_eq!(r.status, 201, "{}", r.body);
    let uuid = r.json()["uuid"].as_str().unwrap().to_string();
    exec(
        &h,
        &format!(
            "INSERT INTO applications (id, uuid, name, environment_id, build_pack, git_branch, git_repository, \
                                       destination_type, destination_id, deleted_at) \
             SELECT v.id, v.uuid, v.name, e.id, 'dockerfile', 'main', 'r', 'App\\Models\\StandaloneDocker', 0, v.deleted \
               FROM environments e JOIN projects p ON p.id = e.project_id, \
                    (VALUES (901, 'app-a', 'a', NULL::timestamp), (902, 'app-b', 'b', now()::timestamp)) v(id, uuid, name, deleted) \
              WHERE p.uuid = '{uuid}'"
        ),
    )
    .await;
    // App a: eight finished (ids 1..8) and one in progress; deleted app b: two finished, newest.
    exec(
        &h,
        "INSERT INTO application_deployment_queues (id, application_id, deployment_uuid, status, application_name) \
         SELECT i, '901', 'd' || i, 'finished', 'a' FROM generate_series(1, 8) i; \
         INSERT INTO application_deployment_queues (id, application_id, deployment_uuid, status, application_name) VALUES \
           (9, '901', 'd9', 'in_progress', 'a'), (10, '902', 'd10', 'failed', 'b'), (11, '902', 'd11', 'finished', 'b'), \
           (12, '999', 'd12', 'finished', 'elsewhere')",
    )
    .await;
    let v = alice.get("/ui/layout/recent-deployments").await;
    let ids = |k: &str| -> Vec<i64> {
        v[k].as_array()
            .unwrap()
            .iter()
            .map(|d| d["id"].as_i64().unwrap())
            .collect()
    };
    assert_eq!(ids("active"), vec![9]);
    assert_eq!(ids("recent"), vec![11, 10, 8, 7, 6]);
    assert_eq!(v["recent"][0]["application_uuid"], "app-b");
    assert_eq!(v["recent"][0]["project"], "Deploys");
    h.finish().await;
}
