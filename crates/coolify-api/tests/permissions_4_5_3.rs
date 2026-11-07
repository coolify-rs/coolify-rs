//! Permission fixes of Coolify 4.5.3: commands and configuration need `read:sensitive` (or an
//! admin in the UI), write tokens set manual webhook secrets, a PATCH of a variable keeps what it
//! does not send, a promotion keeps the member's tokens, and `server_timezone` must be filled.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::Harness;
use serde_json::{Value, json};

const W: Option<&str> = Some("acme_owner_write");

/// `Application::COMMAND_AND_CONFIGURATION_ATTRIBUTES`.
const APP_COMMANDS: [&str; 11] = [
    "install_command",
    "build_command",
    "start_command",
    "pre_deployment_command",
    "post_deployment_command",
    "health_check_command",
    "health_check_response_text",
    "custom_docker_run_options",
    "docker_compose_custom_start_command",
    "docker_compose_custom_build_command",
    "custom_nginx_configuration",
];

struct Team {
    server: String,
    project: String,
}

fn uuid_of(r: &common::Resp) -> String {
    assert!(r.status.is_success(), "{}", r.body);
    r.json()["uuid"].as_str().unwrap().to_owned()
}

async fn team(h: &Harness) -> Team {
    let pem = coolify_compat::sshkey::generate(coolify_compat::sshkey::KeyKind::Ed25519).unwrap();
    let key = uuid_of(
        &h.call(
            "POST",
            "/api/v1/security/keys",
            W,
            Some(json!({"name": "k", "private_key": pem})),
        )
        .await,
    );
    let server = uuid_of(
        &h.call(
            "POST",
            "/api/v1/servers",
            W,
            Some(json!({"name": "s", "ip": "203.0.113.60", "private_key_uuid": key})),
        )
        .await,
    );
    sqlx::query(
        "UPDATE server_settings SET is_reachable = true, is_usable = true, docker_version = '29.1.3' \
         WHERE server_id = (SELECT id FROM servers WHERE uuid = $1)",
    )
    .bind(&server)
    .execute(h.tdb.db.pool())
    .await
    .unwrap();
    let project = uuid_of(
        &h.call("POST", "/api/v1/projects", W, Some(json!({"name": "Shop"})))
            .await,
    );
    Team { server, project }
}

async fn application(h: &Harness, t: &Team) -> String {
    uuid_of(
        &h.call(
            "POST",
            "/api/v1/applications/dockerimage",
            W,
            Some(json!({
                "server_uuid": t.server,
                "project_uuid": t.project,
                "environment_name": "production",
                "docker_registry_image_name": "nginx",
                "docker_registry_image_tag": "alpine",
                "ports_exposes": "80",
                "instant_deploy": false,
            })),
        )
        .await,
    )
}

async fn database(h: &Harness, t: &Team, kind: &str) -> String {
    uuid_of(
        &h.call(
            "POST",
            &format!("/api/v1/databases/{kind}"),
            W,
            Some(json!({"server_uuid": t.server, "project_uuid": t.project, "environment_name": "production"})),
        )
        .await,
    )
}

async fn ui_get(h: &Harness, email: &str, path: &str) -> Value {
    let (cookie, _) = h.ui_login(email).await;
    let r = h.request("GET", path, &[("cookie", &cookie)], None).await;
    assert_eq!(r.status, 200, "{}", r.body);
    r.json()
}

fn keys_present(v: &Value, keys: &[&str]) -> Vec<String> {
    keys.iter()
        .filter(|k| v.get(**k).is_some())
        .map(|k| (*k).to_owned())
        .collect()
}

#[tokio::test]
async fn application_commands_and_configuration_need_read_sensitive() {
    let h = Harness::new().await;
    let t = team(&h).await;
    let uuid = application(&h, &t).await;
    sqlx::query(
        "UPDATE applications SET install_command = 'npm ci', build_command = 'npm run build', \
         start_command = 'npm start', pre_deployment_command = 'php artisan migrate', \
         post_deployment_command = 'php artisan cache:clear', health_check_command = 'curl -fsS localhost', \
         health_check_response_text = 'OK', custom_docker_run_options = '--cap-add SYS_ADMIN', \
         docker_compose_custom_start_command = 'docker compose up -d', \
         docker_compose_custom_build_command = 'docker compose build', \
         custom_nginx_configuration = 'c2VydmVyIHt9' WHERE uuid = $1",
    )
    .bind(&uuid)
    .execute(h.tdb.db.pool())
    .await
    .unwrap();
    let path = format!("/api/v1/applications/{uuid}");

    // A read token, also of the owner, gets none of them; neither does the list.
    let read = h.get(&path, "acme_owner_read").await.json();
    assert_eq!(keys_present(&read, &APP_COMMANDS), Vec::<String>::new());
    assert_eq!(read["uuid"], uuid.as_str());
    let list = h
        .get("/api/v1/applications", "acme_owner_read")
        .await
        .json();
    assert_eq!(keys_present(&list[0], &APP_COMMANDS), Vec::<String>::new());

    // read:sensitive and root tokens of an owner get them, decoded like before.
    for token in ["acme_owner_sensitive", "acme_owner_root"] {
        let full = h.get(&path, token).await.json();
        assert_eq!(keys_present(&full, &APP_COMMANDS).len(), 11, "{token}");
        assert_eq!(full["install_command"], "npm ci");
        assert_eq!(full["health_check_command"], "curl -fsS localhost");
        assert_eq!(full["health_check_response_text"], "OK");
        assert_eq!(full["custom_docker_run_options"], "--cap-add SYS_ADMIN");
        assert_eq!(full["custom_nginx_configuration"], "server {}");
    }

    // The UI: a member does not see them, an admin does.
    let member = ui_get(&h, "bob@example.test", &format!("/ui{path}")).await;
    assert_eq!(keys_present(&member, &APP_COMMANDS), Vec::<String>::new());
    let owner = ui_get(&h, "alice@example.test", &format!("/ui{path}")).await;
    assert_eq!(keys_present(&owner, &APP_COMMANDS).len(), 11);
    h.finish().await;
}

#[tokio::test]
async fn database_configuration_needs_read_sensitive() {
    let h = Harness::new().await;
    let t = team(&h).await;
    let pg = database(&h, &t, "postgresql").await;
    sqlx::query(
        "UPDATE standalone_postgresqls SET postgres_conf = 'max_connections = 50', \
         custom_docker_run_options = '--shm-size 1g' WHERE uuid = $1",
    )
    .bind(&pg)
    .execute(h.tdb.db.pool())
    .await
    .unwrap();
    let path = format!("/api/v1/databases/{pg}");
    let configuration = ["postgres_conf", "custom_docker_run_options"];

    let read = h.get(&path, "acme_owner_read").await.json();
    assert_eq!(keys_present(&read, &configuration), Vec::<String>::new());
    let list = h.get("/api/v1/databases", "acme_owner_read").await.json();
    assert_eq!(keys_present(&list[0], &configuration), Vec::<String>::new());
    let full = h.get(&path, "acme_owner_sensitive").await.json();
    assert_eq!(full["postgres_conf"], "max_connections = 50");
    assert_eq!(full["custom_docker_run_options"], "--shm-size 1g");
    // `serializeApiResponse` sorts the keys: the configuration sits in its alphabetical place.
    let keys: Vec<&str> = full
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    let at = |k: &str| keys.iter().position(|x| *x == k).unwrap();
    assert!(at("custom_docker_run_options") < at("database_type"));
    assert!(at("postgres_conf") < at("postgres_db"));

    let member = ui_get(&h, "bob@example.test", &format!("/ui{path}")).await;
    assert_eq!(keys_present(&member, &configuration), Vec::<String>::new());
    let owner = ui_get(&h, "alice@example.test", &format!("/ui{path}")).await;
    assert_eq!(owner["custom_docker_run_options"], "--shm-size 1g");

    // KeyDB appends `database_type` like the other engines, and hides `keydb_conf`.
    let keydb = database(&h, &t, "keydb").await;
    sqlx::query("UPDATE standalone_keydbs SET keydb_conf = 'maxmemory 1gb' WHERE uuid = $1")
        .bind(&keydb)
        .execute(h.tdb.db.pool())
        .await
        .unwrap();
    let read = h
        .get(&format!("/api/v1/databases/{keydb}"), "acme_owner_read")
        .await
        .json();
    assert_eq!(read["database_type"], "standalone-keydb");
    assert!(read.get("keydb_conf").is_none());
    let full = h
        .get(
            &format!("/api/v1/databases/{keydb}"),
            "acme_owner_sensitive",
        )
        .await
        .json();
    assert_eq!(full["keydb_conf"], "maxmemory 1gb");
    h.finish().await;
}

#[tokio::test]
async fn write_tokens_set_manual_webhook_secrets() {
    let h = Harness::new().await;
    let t = team(&h).await;
    let uuid = application(&h, &t).await;
    let r = h
        .call(
            "PATCH",
            &format!("/api/v1/applications/{uuid}"),
            W,
            Some(json!({"manual_webhook_secret_github": "a-new-webhook-secret-1234"})),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    let stored = h
        .get(
            &format!("/api/v1/applications/{uuid}"),
            "acme_owner_sensitive",
        )
        .await
        .json();
    assert_eq!(
        stored["manual_webhook_secret_github"],
        "a-new-webhook-secret-1234"
    );
    // On create too.
    let r = h
        .call(
            "POST",
            "/api/v1/applications/dockerimage",
            W,
            Some(json!({
                "server_uuid": t.server,
                "project_uuid": t.project,
                "environment_name": "production",
                "docker_registry_image_name": "nginx",
                "ports_exposes": "80",
                "manual_webhook_secret_gitlab": "another-webhook-secret-5678",
                "instant_deploy": false,
            })),
        )
        .await;
    assert_eq!(r.status, 201, "{}", r.body);
    h.finish().await;
}

#[tokio::test]
async fn env_patch_keeps_omitted_value_and_flags() {
    let h = Harness::new().await;
    let t = team(&h).await;
    let app = application(&h, &t).await;
    let envs = format!("/api/v1/applications/{app}/envs");
    let r = h
        .call(
            "POST",
            &envs,
            W,
            Some(json!({"key": "NEXTAUTH_SECRET", "value": "super-secret", "is_literal": true, "is_multiline": true, "is_buildtime": true})),
        )
        .await;
    assert_eq!(r.status, 201, "{}", r.body);
    let r = h
        .call(
            "PATCH",
            &envs,
            W,
            Some(json!({"key": "NEXTAUTH_SECRET", "is_buildtime": false})),
        )
        .await;
    assert_eq!(r.status, 201, "{}", r.body);
    let r = h
        .call(
            "PATCH",
            &format!("{envs}/bulk"),
            W,
            Some(json!({"data": [{"key": "NEXTAUTH_SECRET", "is_runtime": false}]})),
        )
        .await;
    assert_eq!(r.status, 201, "{}", r.body);
    let list = h.get(&envs, "acme_owner_sensitive").await.json();
    let env = list
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["key"] == "NEXTAUTH_SECRET" && e["is_preview"] == false)
        .unwrap();
    assert_eq!(env["value"], "super-secret");
    assert_eq!(env["is_buildtime"], false);
    assert_eq!(env["is_runtime"], false);
    assert_eq!(env["is_literal"], true);
    assert_eq!(env["is_multiline"], true);

    let db = database(&h, &t, "postgresql").await;
    let envs = format!("/api/v1/databases/{db}/envs");
    let r = h
        .call(
            "POST",
            &envs,
            W,
            Some(json!({"key": "SECRET", "value": "super-secret"})),
        )
        .await;
    assert_eq!(r.status, 201, "{}", r.body);
    let r = h
        .call(
            "PATCH",
            &envs,
            W,
            Some(json!({"key": "SECRET", "is_literal": true})),
        )
        .await;
    assert_eq!(r.status, 201, "{}", r.body);
    let list = h.get(&envs, "acme_owner_sensitive").await.json();
    let env = list
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["key"] == "SECRET")
        .unwrap();
    assert_eq!(env["value"], "super-secret");
    assert_eq!(env["is_literal"], true);
    h.finish().await;
}

async fn token_count(h: &Harness, user: i64) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM personal_access_tokens WHERE tokenable_id = $1 AND team_id = '1'",
    )
    .bind(user)
    .fetch_one(h.tdb.db.pool())
    .await
    .unwrap()
}

#[tokio::test]
async fn promotion_keeps_member_tokens_and_demotion_revokes_them() {
    let h = Harness::new().await;
    let (cookie, csrf) = h.ui_login("alice@example.test").await;
    let headers = [("cookie", cookie.as_str()), ("x-csrf-token", csrf.as_str())];
    assert_eq!(token_count(&h, 2).await, 2);
    let r = h
        .request(
            "POST",
            "/ui/team/members/2/role",
            &headers,
            Some(json!({"role": "admin"})),
        )
        .await;
    r.assert(200, r#"{"message":"Member updated."}"#);
    assert_eq!(token_count(&h, 2).await, 2);
    // The token works with the new role.
    assert_eq!(
        h.get("/api/v1/projects", "acme_member_read").await.status,
        200
    );
    let r = h
        .request(
            "POST",
            "/ui/team/members/2/role",
            &headers,
            Some(json!({"role": "owner"})),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(token_count(&h, 2).await, 2);
    // A demotion revokes them.
    let r = h
        .request(
            "POST",
            "/ui/team/members/2/role",
            &headers,
            Some(json!({"role": "member"})),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(token_count(&h, 2).await, 0);
    h.get("/api/v1/projects", "acme_member_read")
        .await
        .assert(401, r#"{"message":"Unauthenticated."}"#);
    h.finish().await;
}

#[tokio::test]
async fn server_timezone_must_be_filled() {
    let h = Harness::new().await;
    let t = team(&h).await;
    let path = format!("/api/v1/servers/{}", t.server);
    for body in [
        json!({"server_timezone": ""}),
        json!({"server_timezone": null}),
    ] {
        h.call("PATCH", &path, W, Some(body)).await.assert(
            422,
            r#"{"message":"Validation failed.","errors":{"server_timezone":["The server timezone field must have a value."]}}"#,
        );
    }
    // `bail`: a number is not a string, and the timezone check does not run.
    h.call("PATCH", &path, W, Some(json!({"server_timezone": 5})))
        .await
        .assert(
            422,
            r#"{"message":"Validation failed.","errors":{"server_timezone":["The server timezone field must be a string."]}}"#,
        );
    h.finish().await;
}

#[tokio::test]
async fn two_factor_blocked_member_switches_only_to_teams_without_2fa() {
    let h = Harness::new().await;
    // Bob is a member of Acme (which now requires 2FA) and of Other (which does not).
    sqlx::query("UPDATE teams SET is_two_factor_required = true WHERE id = 1")
        .execute(h.tdb.db.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO team_user (team_id, user_id, role) VALUES (2, 2, 'member')")
        .execute(h.tdb.db.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE users SET current_team_id = 1 WHERE id = 2")
        .execute(h.tdb.db.pool())
        .await
        .unwrap();
    let (cookie, csrf) = h.ui_login("bob@example.test").await;
    let headers = [("cookie", cookie.as_str()), ("x-csrf-token", csrf.as_str())];
    let page = h
        .request("GET", "/ui/auth/two-factor-required", &headers, None)
        .await;
    assert_eq!(page.status, 200, "{}", page.body);
    assert_eq!(
        page.json()["teams_without_two_factor"],
        json!([{"id": 2, "name": "Other"}])
    );
    let switch = "/ui/auth/two-factor-required/switch-team";
    // A team that requires 2FA is refused.
    h.request("POST", switch, &headers, Some(json!({"team_id": 1})))
        .await
        .assert(200, r#"{"redirect":"\/two-factor-required"}"#);
    h.request("POST", switch, &headers, Some(json!({"team_id": 2})))
        .await
        .assert(200, r#"{"redirect":"\/"}"#);
    let current: Option<i64> = sqlx::query_scalar("SELECT current_team_id FROM users WHERE id = 2")
        .fetch_one(h.tdb.db.pool())
        .await
        .unwrap();
    assert_eq!(current, Some(2));
    h.finish().await;
}
