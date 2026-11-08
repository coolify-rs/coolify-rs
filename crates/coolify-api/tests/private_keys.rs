//! Private keys API. Expected bodies were observed on the upstream
//! `sha-b5823c7` reference (compat/diff); keys are throwaway fixtures whose
//! public keys/fingerprints were computed by upstream (phpseclib).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::Harness;
use serde_json::{Value, json};

fn fixture() -> Vec<Value> {
    let v: Value = serde_json::from_str(include_str!(
        "../../coolify-compat/tests/fixtures/ssh_keys.json"
    ))
    .unwrap();
    v["cases"].as_array().unwrap().clone()
}

const W: Option<&str> = Some("acme_owner_write");
/// DELETE routes need the `delete` ability since 4.6.0.
const DEL: Option<&str> = Some("acme_owner_delete");

#[tokio::test]
async fn create_list_show_with_sensitive_gating() {
    let h = Harness::new().await;
    let cases = fixture();
    let mut uuids = Vec::new();
    for c in &cases {
        let r = h
            .call(
                "POST",
                "/api/v1/security/keys",
                W,
                Some(json!({"name": format!("probe {}", c["file"].as_str().unwrap()), "private_key": c["private_key"]})),
            )
            .await;
        assert_eq!(r.status, 201, "{}", r.body);
        uuids.push(r.json()["uuid"].as_str().unwrap().to_string());
    }
    // Duplicate fingerprint in the same team.
    h.call(
        "POST",
        "/api/v1/security/keys",
        W,
        Some(json!({"private_key": cases[0]["private_key"]})),
    )
    .await
    .assert(422, r#"{"message":"Private key already exists."}"#);
    h.call(
        "POST",
        "/api/v1/security/keys",
        W,
        Some(json!({"private_key": "garbage"})),
    )
    .await
    .assert(422, r#"{"message":"Invalid private key."}"#);
    h.call(
        "POST",
        "/api/v1/security/keys",
        W,
        Some(json!({"name": "x"})),
    )
    .await
    .assert(
        422,
        r#"{"message":"Validation failed.","errors":{"private_key":["This field is required."]}}"#,
    );

    // Without read:sensitive the private key is hidden; public key and fingerprint match upstream.
    let list = h
        .get("/api/v1/security/keys", "acme_owner_read")
        .await
        .json();
    let items = list.as_array().unwrap();
    assert_eq!(items.len(), 4);
    for (item, c) in items.iter().zip(&cases) {
        assert!(item.get("private_key").is_none());
        assert_eq!(item["public_key"], c["public_key"]);
        assert_eq!(item["fingerprint"], c["fingerprint"]);
        assert_eq!(item["description"], "Created by Coolify via API");
        let keys: Vec<&String> = item.as_object().unwrap().keys().collect();
        assert_eq!(
            keys,
            vec![
                "id",
                "uuid",
                "description",
                "name",
                "fingerprint",
                "is_git_related",
                "public_key",
                "team_id",
                "created_at",
                "updated_at"
            ]
        );
    }
    // With read:sensitive (and admin role) the stored, formatted key is returned.
    let shown = h
        .get(
            &format!("/api/v1/security/keys/{}", uuids[0]),
            "acme_owner_sensitive",
        )
        .await
        .json();
    assert_eq!(shown["private_key"], cases[0]["stored_private_key"]);
    // Other teams cannot see it.
    h.get(&format!("/api/v1/security/keys/{}", uuids[0]), "carol_root")
        .await
        .assert(404, r#"{"message":"Private Key not found."}"#);

    // Stored encrypted with APP_KEY, decryptable by Laravel's format.
    let stored: String =
        sqlx::query_scalar("SELECT private_key FROM private_keys ORDER BY id LIMIT 1")
            .fetch_one(h.tdb.db.pool())
            .await
            .unwrap();
    let enc = coolify_compat::Encrypter::new(common::APP_KEY).unwrap();
    assert_eq!(
        enc.decrypt_string(&stored).unwrap(),
        cases[0]["stored_private_key"].as_str().unwrap()
    );
    h.finish().await;
}

#[tokio::test]
async fn update_and_delete() {
    let h = Harness::new().await;
    let cases = fixture();
    let uuid = h
        .call(
            "POST",
            "/api/v1/security/keys",
            W,
            Some(json!({"name": "First", "private_key": cases[0]["private_key"]})),
        )
        .await
        .json()["uuid"]
        .as_str()
        .unwrap()
        .to_string();
    let other = h
        .call(
            "POST",
            "/api/v1/security/keys",
            W,
            Some(json!({"private_key": cases[1]["private_key"]})),
        )
        .await;
    assert_eq!(other.status, 201);
    let path = format!("/api/v1/security/keys/{uuid}");

    // Rename without resending the key (upstream PrivateKeyUpdateApiTest).
    h.call("PATCH", &path, W, Some(json!({"name": "Renamed"})))
        .await
        .assert(201, &format!(r#"{{"uuid":"{uuid}"}}"#));
    h.call("PATCH", &path, W, Some(json!({"private_key": "nope"})))
        .await
        .assert(422, r#"{"message":"The private key is invalid.","errors":{"private_key":["The private key is invalid."]}}"#);
    h.call("PATCH", &path, W, Some(json!({"private_key": cases[1]["private_key"]})))
        .await
        .assert(422, r#"{"message":"This private key already exists.","errors":{"private_key":["This private key already exists."]}}"#);
    h.call("PATCH", &path, W, Some(json!({"foo": 1})))
        .await
        .assert(
            422,
            r#"{"message":"Validation failed.","errors":{"foo":["This field is not allowed."]}}"#,
        );
    let shown = h.get(&path, "acme_owner_read").await.json();
    assert_eq!(shown["name"], "Renamed");

    // In use by a server: refused.
    sqlx::query(
        "INSERT INTO servers (uuid, name, ip, team_id, private_key_id) SELECT 'srv', 'srv', '10.0.0.1', 1, id FROM private_keys WHERE uuid = $1",
    )
    .bind(&uuid)
    .execute(h.tdb.db.pool())
    .await
    .unwrap();
    let r = h.call("DELETE", &path, DEL, None).await;
    assert_eq!(r.status, 422);
    assert_eq!(
        r.json()["message"],
        "Private Key is in use and cannot be deleted."
    );
    sqlx::query("UPDATE servers SET deleted_at = now()")
        .execute(h.tdb.db.pool())
        .await
        .unwrap();
    h.call("DELETE", &path, DEL, None)
        .await
        .assert(200, r#"{"message":"Private Key deleted."}"#);
    h.call("DELETE", &path, DEL, None)
        .await
        .assert(404, r#"{"message":"Private Key not found."}"#);
    h.finish().await;
}
