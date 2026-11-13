//! Stars (upstream `App\Livewire\StarToggle`, `Dashboard::starredResources()`, `unstar()`,
//! `reorderStars()` and `HasStars`): the star button endpoints and the starred cards of the
//! dashboard. Upstream has no tests for stars; the cases here follow the PHP.

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

    async fn toggle(&self, kind: &str, uuid: &str) -> Value {
        let path = format!("/ui/stars/resource/{kind}/{uuid}/toggle");
        let r = self.send("POST", &path, Some(json!({}))).await;
        assert_eq!(r.status, 200, "{path}: {}", r.body);
        r.json()
    }

    /// `(star_id, name)` of the dashboard's starred cards, in order.
    async fn starred(&self) -> Vec<(i64, String)> {
        self.get("/ui/dashboard").await["starred_resources"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| {
                (
                    s["star_id"].as_i64().unwrap(),
                    s["name"].as_str().unwrap().to_owned(),
                )
            })
            .collect()
    }
}

/// The `stars` table as `2026_11_04_000000_create_stars_table` creates it (a baseline that already
/// has it keeps its own), two servers, one project with an application, a service and a database
/// for Acme, and a server and an application of the other team.
const FIXTURE: &str = r#"
CREATE TABLE IF NOT EXISTS stars (
    id bigserial PRIMARY KEY,
    user_id bigint NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    starrable_type character varying(255) NOT NULL,
    starrable_id bigint NOT NULL,
    "position" integer DEFAULT 0 NOT NULL,
    created_at timestamp(0) without time zone,
    updated_at timestamp(0) without time zone,
    CONSTRAINT stars_user_id_starrable_type_starrable_id_unique UNIQUE (user_id, starrable_type, starrable_id)
);
INSERT INTO private_keys (id, uuid, name, private_key, team_id) VALUES (900, 'pk', 'k', 'x', 1), (901, 'pk2', 'k', 'x', 2);
INSERT INTO servers (id, uuid, name, ip, team_id, private_key_id) VALUES
  (900, 'srv-a', 'alpha', '10.0.0.1', 1, 900),
  (901, 'srv-b', 'beta', '10.0.0.2', 1, 900),
  (902, 'srv-other', 'theirs', '10.0.0.3', 2, 901);
INSERT INTO projects (id, uuid, name, team_id, created_at, updated_at) VALUES
  (800, 'proj', 'Web', 1, now(), now()),
  (801, 'proj-other', 'Theirs', 2, now(), now());
INSERT INTO environments (id, uuid, name, project_id, created_at, updated_at) VALUES
  (810, 'env', 'production', 800, now(), now()),
  (811, 'env-other', 'production', 801, now(), now());
INSERT INTO applications (id, uuid, name, environment_id, build_pack, git_branch, git_repository, destination_type, destination_id) VALUES
  (820, 'app-a', 'shop', 810, 'dockerfile', 'main', 'r', 'App\Models\StandaloneDocker', 0),
  (821, 'app-other', 'their-app', 811, 'dockerfile', 'main', 'r', 'App\Models\StandaloneDocker', 0);
INSERT INTO services (id, uuid, name, environment_id, docker_compose_raw) VALUES (830, 'svc-a', 'plausible-abc', 810, '');
INSERT INTO standalone_postgresqls (id, uuid, name, postgres_password, environment_id, destination_type, destination_id)
  VALUES (840, 'pg-a', 'main-db', 'x', 810, 'App\Models\StandaloneDocker', 0);
"#;

async fn exec(h: &Harness, sql: &str) {
    sqlx::raw_sql(sql).execute(h.tdb.db.pool()).await.unwrap();
}

async fn star_rows(h: &Harness) -> Vec<(i64, String, i64, i32)> {
    sqlx::query_as(
        "SELECT user_id, starrable_type::text, starrable_id, position FROM stars ORDER BY user_id, position, id",
    )
    .fetch_all(h.tdb.db.pool())
    .await
    .unwrap()
}

/// `StarToggle`: a click stars the resource for the user at the next position, a second click
/// removes the star. Each user has their own stars.
#[tokio::test]
async fn toggle_stars_and_unstars_for_the_user() {
    let h = Harness::with_extra_sql(FIXTURE).await;
    let alice = Ui::login(&h, "alice@example.test").await;
    let bob = Ui::login(&h, "bob@example.test").await;

    assert_eq!(
        alice.get("/ui/stars/resource/server/srv-a").await,
        json!({"starred": false})
    );
    assert_eq!(
        alice.toggle("server", "srv-a").await,
        json!({"starred": true})
    );
    assert_eq!(
        alice.toggle("application", "app-a").await,
        json!({"starred": true})
    );
    assert_eq!(
        alice.toggle("database", "pg-a").await,
        json!({"starred": true})
    );
    assert_eq!(
        alice.toggle("service", "svc-a").await,
        json!({"starred": true})
    );
    assert_eq!(
        star_rows(&h).await,
        vec![
            (1, "App\\Models\\Server".to_owned(), 900, 0),
            (1, "App\\Models\\Application".to_owned(), 820, 1),
            (1, "App\\Models\\StandalonePostgresql".to_owned(), 840, 2),
            (1, "App\\Models\\Service".to_owned(), 830, 3),
        ]
    );
    assert_eq!(
        alice.get("/ui/stars/resource/database/pg-a").await,
        json!({"starred": true})
    );
    // Bob's star of the same server is his own, at his own first position.
    assert_eq!(
        bob.get("/ui/stars/resource/server/srv-a").await,
        json!({"starred": false})
    );
    assert_eq!(
        bob.toggle("server", "srv-a").await,
        json!({"starred": true})
    );
    assert!(
        star_rows(&h)
            .await
            .contains(&(2, "App\\Models\\Server".to_owned(), 900, 0))
    );

    // The second click removes only Alice's star.
    assert_eq!(
        alice.toggle("server", "srv-a").await,
        json!({"starred": false})
    );
    let rows = star_rows(&h).await;
    assert!(!rows.contains(&(1, "App\\Models\\Server".to_owned(), 900, 0)));
    assert!(rows.contains(&(2, "App\\Models\\Server".to_owned(), 900, 0)));
    // A new star goes after the user's last one, not into the gap.
    assert_eq!(
        alice.toggle("server", "srv-b").await,
        json!({"starred": true})
    );
    assert!(
        star_rows(&h)
            .await
            .contains(&(1, "App\\Models\\Server".to_owned(), 901, 4))
    );
    h.finish().await;
}

/// `abort_unless(in_array(...STARRABLE_TYPES), 404)` and `authorize('view', ...)`: unknown kinds
/// and resources of another team are not found, and nothing is written.
#[tokio::test]
async fn other_teams_resources_and_unknown_kinds_are_not_found() {
    let h = Harness::with_extra_sql(FIXTURE).await;
    let alice = Ui::login(&h, "alice@example.test").await;
    for path in [
        "/ui/stars/resource/server/srv-other/toggle",
        "/ui/stars/resource/application/app-other/toggle",
        "/ui/stars/resource/application/missing/toggle",
        "/ui/stars/resource/project/proj/toggle",
        // A database page names a database, not an application.
        "/ui/stars/resource/database/app-a/toggle",
    ] {
        let r = alice.send("POST", path, Some(json!({}))).await;
        assert_eq!(r.status, 404, "{path}: {}", r.body);
    }
    let r = alice
        .send("GET", "/ui/stars/resource/server/srv-other", None)
        .await;
    assert_eq!(r.status, 404, "{}", r.body);
    assert!(star_rows(&h).await.is_empty());
    // No session, no star.
    let r = h
        .request("GET", "/ui/stars/resource/server/srv-a", &[], None)
        .await;
    assert_eq!(r.status, 401);
    h.finish().await;
}

/// `Dashboard::starredResources()`: the user's stars in the session team, in the chosen order,
/// with the context line and the link of each card.
#[tokio::test]
async fn dashboard_lists_the_starred_resources_in_order() {
    let h = Harness::with_extra_sql(FIXTURE).await;
    let alice = Ui::login(&h, "alice@example.test").await;
    assert_eq!(
        alice.get("/ui/dashboard").await["starred_resources"],
        json!([])
    );

    alice.toggle("application", "app-a").await;
    alice.toggle("server", "srv-a").await;
    alice.toggle("database", "pg-a").await;
    alice.toggle("service", "svc-a").await;
    // Stars of someone else, of another team's resources and of a model that cannot be starred.
    exec(
        &h,
        "INSERT INTO stars (user_id, starrable_type, starrable_id, position) VALUES \
         (2, 'App\\Models\\Server', 901, 0), \
         (1, 'App\\Models\\Server', 902, 10), \
         (1, 'App\\Models\\Application', 821, 11), \
         (1, 'App\\Models\\Project', 800, 12)",
    )
    .await;

    let d = alice.get("/ui/dashboard").await;
    let cards = d["starred_resources"].as_array().unwrap();
    let summary: Vec<(&str, &str, &str, &str)> = cards
        .iter()
        .map(|c| {
            (
                c["type"].as_str().unwrap(),
                c["name"].as_str().unwrap(),
                c["context"].as_str().unwrap(),
                c["href"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        summary,
        vec![
            (
                "application",
                "shop",
                "Web / production",
                "/project/proj/environment/env/application/app-a"
            ),
            ("server", "alpha", "Server", "/server/srv-a"),
            (
                "database",
                "main-db",
                "Web / production",
                "/project/proj/environment/env/database/pg-a"
            ),
            (
                "service",
                "plausible-abc",
                "Web / production",
                "/project/proj/environment/env/service/svc-a"
            ),
        ]
    );
    // What the logo of each card needs.
    assert_eq!(cards[0]["application"]["build_pack"], "dockerfile");
    assert_eq!(cards[0]["application"]["is_http_basic_auth_enabled"], false);
    assert_eq!(cards[0]["database_type"], Value::Null);
    assert_eq!(cards[1]["application"], Value::Null);
    assert_eq!(cards[2]["database_type"], "standalone-postgresql");
    assert!(cards[3]["service"].is_object());
    h.finish().await;
}

/// `Dashboard::reorderStars()` and `unstar()`: the order of the list becomes the positions; ids
/// of other users change nothing.
#[tokio::test]
async fn reorder_and_unstar_touch_only_the_users_stars() {
    let h = Harness::with_extra_sql(FIXTURE).await;
    let alice = Ui::login(&h, "alice@example.test").await;
    let bob = Ui::login(&h, "bob@example.test").await;
    alice.toggle("application", "app-a").await;
    alice.toggle("server", "srv-a").await;
    alice.toggle("database", "pg-a").await;
    bob.toggle("server", "srv-b").await;
    let before = alice.starred().await;
    assert_eq!(
        before.iter().map(|s| s.1.as_str()).collect::<Vec<_>>(),
        vec!["shop", "alpha", "main-db"]
    );
    let bobs = bob.starred().await[0].0;

    // Last first; Bob's star id in the list is ignored.
    let order = vec![before[2].0, bobs, before[0].0, before[1].0];
    let r = alice
        .send(
            "POST",
            "/ui/stars/reorder",
            Some(json!({"star_ids": order})),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(
        alice
            .starred()
            .await
            .iter()
            .map(|s| s.1.clone())
            .collect::<Vec<_>>(),
        vec!["main-db", "shop", "alpha"]
    );
    assert!(
        star_rows(&h)
            .await
            .contains(&(2, "App\\Models\\Server".to_owned(), 901, 0)),
        "Bob's star keeps its position"
    );

    // Alice cannot remove Bob's star; she can remove her own.
    let r = alice
        .send("DELETE", &format!("/ui/stars/star/{bobs}"), None)
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(bob.starred().await.len(), 1);
    let r = alice
        .send("DELETE", &format!("/ui/stars/star/{}", before[0].0), None)
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(
        alice
            .starred()
            .await
            .iter()
            .map(|s| s.1.clone())
            .collect::<Vec<_>>(),
        vec!["main-db", "alpha"]
    );
    h.finish().await;
}

/// `HasStars::bootHasStars()`: deleting a resource deletes its stars, for every user. A star left
/// on a trashed row never shows.
#[tokio::test]
async fn deleting_a_resource_deletes_its_stars() {
    let h = Harness::with_extra_sql(FIXTURE).await;
    let alice = Ui::login(&h, "alice@example.test").await;
    let bob = Ui::login(&h, "bob@example.test").await;
    alice.toggle("server", "srv-a").await;
    bob.toggle("server", "srv-a").await;
    alice.toggle("service", "svc-a").await;
    alice.toggle("database", "pg-a").await;
    alice.toggle("application", "app-a").await;

    let db = &h.tdb.db;
    let now = h.state.now();
    coolify_db::servers::soft_delete(db, 900, now)
        .await
        .unwrap();
    coolify_db::services::soft_delete(db, 830, now)
        .await
        .unwrap();
    coolify_db::databases::soft_delete(db, coolify_db::databases::Kind::Postgresql, 840, now)
        .await
        .unwrap();
    assert_eq!(
        star_rows(&h).await,
        vec![(1, "App\\Models\\Application".to_owned(), 820, 3)]
    );
    coolify_db::applications::soft_delete(db, 820, now)
        .await
        .unwrap();
    assert!(star_rows(&h).await.is_empty());

    // A star that outlived its (trashed) resource is not a card.
    exec(
        &h,
        "INSERT INTO stars (user_id, starrable_type, starrable_id, position) VALUES (1, 'App\\Models\\Server', 900, 0)",
    )
    .await;
    assert!(alice.starred().await.is_empty());
    h.finish().await;
}
