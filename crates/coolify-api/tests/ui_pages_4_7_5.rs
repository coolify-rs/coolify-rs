//! Pages of the Coolify v4.7.5 port whose read models changed: the private key editor
//! (`Security\PrivateKey\Show`: no stored key, the reveal with a password, the "Used by" list).

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

/// A real key of Acme: the uuid, the id and the stored key.
async fn acme_key(h: &Harness) -> (String, i64, String) {
    let fixtures: Value = serde_json::from_str(include_str!(
        "../../coolify-compat/tests/fixtures/ssh_keys.json"
    ))
    .unwrap();
    let private_key = fixtures["cases"][0]["private_key"].clone();
    let stored = fixtures["cases"][0]["stored_private_key"]
        .as_str()
        .unwrap()
        .to_string();
    let r = h
        .call(
            "POST",
            "/api/v1/security/keys",
            Some("acme_owner_write"),
            Some(json!({"name": "deploy", "private_key": private_key})),
        )
        .await;
    assert_eq!(r.status, 201, "{}", r.body);
    let uuid = r.json()["uuid"].as_str().unwrap().to_string();
    let id: i64 = sqlx::query_scalar("SELECT id FROM private_keys WHERE uuid = $1")
        .bind(&uuid)
        .fetch_one(h.tdb.db.pool())
        .await
        .unwrap();
    (uuid, id, stored)
}

/// `Show::render()`: the editor never carries the stored key; the "Used by" list counts every
/// use per type and lists the rows that match the search, with links.
#[tokio::test]
async fn the_key_editor_lists_its_uses_without_the_key() {
    let h = Harness::new().await;
    let (uuid, id, _) = acme_key(&h).await;
    sqlx::raw_sql(&format!(
        "INSERT INTO servers (id, uuid, name, ip, port, team_id, private_key_id) VALUES (900, 'srv-a', 'alpha', '10.0.0.1', 22, 1, {id}); \
         INSERT INTO projects (id, uuid, name, team_id, created_at, updated_at) VALUES (800, 'proj', 'Web', 1, now(), now()); \
         INSERT INTO environments (id, uuid, name, project_id, created_at, updated_at) VALUES (810, 'env', 'production', 800, now(), now()); \
         INSERT INTO applications (id, uuid, name, environment_id, build_pack, git_branch, git_repository, private_key_id) VALUES \
           (820, 'app-a', 'shop', 810, 'dockerfile', 'main', 'r', {id});"
    ))
    .execute(h.tdb.db.pool())
    .await
    .unwrap();
    let alice = Ui::login(&h, "alice@example.test").await;

    let v = alice.get(&format!("/ui/keys/{uuid}")).await;
    assert!(v.get("private_key").is_none(), "{v}");
    assert!(v["public_key"].as_str().unwrap().starts_with("ssh-"), "{v}");
    assert_eq!(
        v["counts"],
        json!([{"type": "Server", "count": 1}, {"type": "Application", "count": 1}])
    );
    assert_eq!(v["matches"], 2);
    assert_eq!(
        v["rows"],
        json!([
            {"type": "Server", "name": "alpha", "url": "/server/srv-a"},
            {"type": "Application", "name": "shop", "url": "/project/proj/environment/env/application/app-a"},
        ])
    );
    assert_eq!(v["in_use"], true);

    let v = alice
        .get(&format!("/ui/keys/{uuid}?usage_search=SHO"))
        .await;
    assert_eq!(v["matches"], 1);
    assert_eq!(v["rows"][0]["name"], "shop");
    assert_eq!(v["counts"].as_array().unwrap().len(), 2);
    h.finish().await;
}

/// `Show::revealPrivateKey($password)`: the key comes back only after the password.
#[tokio::test]
async fn the_stored_key_is_revealed_after_the_password() {
    let h = Harness::new().await;
    let (uuid, _, stored) = acme_key(&h).await;
    let alice = Ui::login(&h, "alice@example.test").await;
    let path = format!("/ui/keys/{uuid}/reveal");

    let r = alice
        .send("POST", &path, Some(json!({"password": "wrong"})))
        .await;
    assert_eq!(r.status, 422, "{}", r.body);
    assert_eq!(r.json()["message"], "The provided password is incorrect.");

    let r = alice
        .send("POST", &path, Some(json!({"password": common::PASSWORD})))
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.json()["private_key"], stored.as_str());
    h.finish().await;
}
