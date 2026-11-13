//! UI read models and writes changed by the Coolify v4.7.5 port of the general interface: the
//! dashboard's deployment lists (`Dashboard\ActiveDeployments`), the new resource page
//! (`Project\Resource\Create`), the command palette (`GlobalSearch`) and onboarding (`Boarding\Index`).

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

/// A project with one environment, a server with a destination, an application and a Redis
/// database for Acme. `scan_gate_result` is the column of `create_image_scanning_tables`; a baseline
/// that already has it keeps its own.
const FIXTURE: &str = r#"
ALTER TABLE application_deployment_queues ADD COLUMN IF NOT EXISTS scan_gate_result jsonb;
INSERT INTO private_keys (id, uuid, name, private_key, team_id) VALUES (900, 'pk', 'k', 'x', 1);
INSERT INTO servers (id, uuid, name, ip, port, team_id, private_key_id) VALUES (900, 'srv-a', 'alpha', '10.0.0.1', 22, 1, 900);
INSERT INTO standalone_dockers (id, name, uuid, network, server_id) VALUES (910, 'coolify', 'dest-a', 'coolify', 900);
INSERT INTO projects (id, uuid, name, team_id, created_at, updated_at) VALUES (800, 'proj', 'Web', 1, now(), now());
INSERT INTO environments (id, uuid, name, project_id, created_at, updated_at) VALUES (810, 'env', 'production', 800, now(), now());
INSERT INTO applications (id, uuid, name, environment_id, build_pack, git_branch, git_repository, destination_type, destination_id) VALUES
  (820, 'app-a', 'shop', 810, 'dockerfile', 'main', 'r', 'App\Models\StandaloneDocker', 910);
INSERT INTO standalone_redis (id, uuid, name, environment_id, destination_type, destination_id)
  VALUES (840, 'redis-a', 'cache', 810, 'App\Models\StandaloneDocker', 910);
"#;

/// `active-deployments.blade.php`: recent deployments show when they finished, and a deployment the
/// image scan gate stopped (`isImageScanBlocked()`) is marked, whatever its status says.
#[tokio::test]
async fn recent_deployments_carry_the_finish_time_and_the_blocked_mark() {
    let h = Harness::with_extra_sql(FIXTURE).await;
    sqlx::raw_sql(
        "INSERT INTO application_deployment_queues \
           (id, application_id, deployment_uuid, status, application_name, created_at, finished_at, scan_gate_result) VALUES \
           (1, '820', 'd1', 'finished', 'shop', '2026-11-01 10:00:00', '2026-11-01 10:05:00', NULL), \
           (2, '820', 'd2', 'failed', 'shop', '2026-11-02 10:00:00', '2026-11-02 10:01:00', '{\"blocked\": true}'), \
           (3, '820', 'd3', 'failed', 'shop', '2026-11-03 10:00:00', '2026-11-03 10:02:00', '{\"blocked\": false, \"error\": \"x\"}'), \
           (4, '820', 'd4', 'in_progress', 'shop', '2026-11-04 10:00:00', NULL, NULL)",
    )
    .execute(h.tdb.db.pool())
    .await
    .unwrap();
    let alice = Ui::login(&h, "alice@example.test").await;
    let v = alice.get("/ui/layout/recent-deployments").await;

    let recent = v["recent"].as_array().unwrap();
    assert_eq!(
        recent
            .iter()
            .map(|d| d["id"].as_i64().unwrap())
            .collect::<Vec<_>>(),
        vec![3, 2, 1]
    );
    assert_eq!(
        recent
            .iter()
            .map(|d| d["image_scan_blocked"].as_bool().unwrap())
            .collect::<Vec<_>>(),
        vec![false, true, false]
    );
    assert!(
        recent[2]["finished_at"]
            .as_str()
            .unwrap()
            .starts_with("2026-11-01T10:05:00"),
        "{}",
        recent[2]["finished_at"]
    );

    let active = v["active"].as_array().unwrap();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0]["id"], 4);
    assert_eq!(active[0]["finished_at"], Value::Null);
    assert_eq!(active[0]["image_scan_blocked"], false);
    // The top bar's indicator reads the same rows.
    let indicator = alice.get("/ui/layout/deployments").await;
    assert_eq!(indicator["deployments"][0]["image_scan_blocked"], false);
    h.finish().await;
}

/// `Create::mount()` and `createResource()`: Redis asks for its image (Redis or Valkey) like
/// PostgreSQL, and the chosen image is the one the database gets.
#[tokio::test]
async fn a_redis_database_needs_its_image() {
    let h = Harness::with_extra_sql(FIXTURE).await;
    let alice = Ui::login(&h, "alice@example.test").await;
    let body = |image: Option<&str>| {
        let mut b = json!({
            "project_uuid": "proj",
            "environment_uuid": "env",
            "destination_uuid": "dest-a",
            "type": "redis",
        });
        if let Some(image) = image {
            b["database_image"] = json!(image);
        }
        b
    };

    let r = alice.send("POST", "/ui/databases", Some(body(None))).await;
    assert_eq!(r.status, 422, "{}", r.body);
    assert_eq!(
        r.json()["errors"]["database_image"],
        json!(["The database image field is required."])
    );

    let r = alice
        .send(
            "POST",
            "/ui/databases",
            Some(body(Some("valkey/valkey:9-alpine"))),
        )
        .await;
    assert_eq!(r.status, 201, "{}", r.body);
    let uuid = r.json()["uuid"].as_str().unwrap().to_owned();
    let image: String = sqlx::query_scalar("SELECT image FROM standalone_redis WHERE uuid = $1")
        .bind(&uuid)
        .fetch_one(h.tdb.db.pool())
        .await
        .unwrap();
    assert_eq!(image, "valkey/valkey:9-alpine");
    h.finish().await;
}

/// Upstream `PendingResourceCreationAuthorizationTest`: `createAnyResource` in the environment. A
/// member with only a read grant on the environment is refused and nothing is created; with a
/// write grant on the environment only, the database is created.
#[tokio::test]
async fn creating_a_database_needs_write_on_the_environment() {
    let h = Harness::with_extra_sql(FIXTURE).await;
    let body = json!({
        "project_uuid": "proj",
        "environment_uuid": "env",
        "destination_uuid": "dest-a",
        "type": "redis",
        "database_image": "valkey/valkey:9-alpine",
    });

    grant(&h, "read").await;
    let bob = Ui::login(&h, "bob@example.test").await;
    let r = bob.send("POST", "/ui/databases", Some(body.clone())).await;
    assert_eq!(r.status, 403, "{}", r.body);
    assert_eq!(created_databases(&h).await, 0);

    grant(&h, "write").await;
    let bob = Ui::login(&h, "bob@example.test").await;
    let r = bob.send("POST", "/ui/databases", Some(body)).await;
    assert_eq!(r.status, 201, "{}", r.body);
    assert_eq!(created_databases(&h).await, 1);
    h.finish().await;
}

/// Limits Bob to one level on the fixture's environment.
async fn grant(h: &Harness, level: &str) {
    sqlx::query("UPDATE team_user SET access = $1::json WHERE team_id = 1 AND user_id = 2")
        .bind(json!([{"scope": "environment", "uuid": "env", "levels": [level]}]).to_string())
        .execute(h.tdb.db.pool())
        .await
        .unwrap();
}

/// Redis databases other than the fixture's.
async fn created_databases(h: &Harness) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM standalone_redis WHERE uuid <> 'redis-a'")
        .fetch_one(h.tdb.db.pool())
        .await
        .unwrap()
}

/// `GlobalSearch::loadSearchableItems()`: a Redis database is also found as Valkey.
#[tokio::test]
async fn a_redis_database_is_found_as_valkey() {
    let h = Harness::with_extra_sql(FIXTURE).await;
    let alice = Ui::login(&h, "alice@example.test").await;
    let items = alice.get("/ui/search/items").await;
    let redis = items["resources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["uuid"] == "redis-a")
        .expect("the Redis database is searchable");
    assert_eq!(redis["type"], "database");
    assert_eq!(redis["subtype"], "redis");
    assert_eq!(
        redis["search_text"],
        "cache redis-a redis valkey  database databases db"
    );
    h.finish().await;
}
