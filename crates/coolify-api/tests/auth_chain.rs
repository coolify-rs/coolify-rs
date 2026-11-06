//! The authentication/authorization chain. Expected bodies are the bytes
//! observed from the 4.5.0 reference (compat/diff) and upstream Pest specs:
//! ApiAuthorizationTest, LegacyMemberTokenTest, ApiTokenTeamLifecycleSecurityTest,
//! ApiTokenExpirationTest, IpAllowlistTest, StateChangingRoutesUsePostTest.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::Harness;

#[tokio::test]
async fn health_is_public_plain_text() {
    let h = Harness::new().await;
    for p in ["/api/health", "/api/v1/health"] {
        let r = h.call("GET", p, None, None).await;
        r.assert(200, "OK");
        assert!(r.ctype.starts_with("text/html"));
    }
    h.finish().await;
}

#[tokio::test]
async fn authentication_failures() {
    let h = Harness::new().await;
    h.call("GET", "/api/v1/projects", None, None)
        .await
        .assert(401, r#"{"message":"Unauthenticated."}"#);
    h.get("/api/v1/projects", "acme_owner_expired")
        .await
        .assert(401, r#"{"message":"Unauthenticated."}"#);
    // Valid id, wrong secret; and a bare (id-less) unknown secret.
    let id = h
        .token("acme_owner_root")
        .split('|')
        .next()
        .unwrap()
        .to_string();
    h.call_bearer("GET", "/api/v1/projects", &format!("{id}|not-the-secret"))
        .await
        .assert(401, r#"{"message":"Unauthenticated."}"#);
    h.call_bearer("GET", "/api/v1/projects", "no-pipe-unknown")
        .await
        .assert(401, r#"{"message":"Unauthenticated."}"#);
    // The id-less form works when the secret matches (Sanctum lookup by hash).
    let secret = h
        .token("acme_owner_read")
        .split_once('|')
        .unwrap()
        .1
        .to_string();
    assert_eq!(
        h.call_bearer("GET", "/api/v1/projects", &secret)
            .await
            .status,
        200
    );
    // Token bound to a team the user does not belong to.
    h.get("/api/v1/projects", "alice_foreign_team")
        .await
        .assert(401, r#"{"message":"Invalid token."}"#);
    h.finish().await;
}

#[tokio::test]
async fn removed_member_loses_access() {
    let h = Harness::new().await;
    h.get("/api/v1/projects", "acme_member_read").await;
    sqlx::query("DELETE FROM team_user WHERE team_id = 1 AND user_id = 2")
        .execute(h.tdb.db.pool())
        .await
        .unwrap();
    h.get("/api/v1/projects", "acme_member_read")
        .await
        .assert(401, r#"{"message":"Invalid token."}"#);
    h.finish().await;
}

#[tokio::test]
async fn members_with_elevated_tokens_are_refused() {
    let h = Harness::new().await;
    h.get("/api/v1/projects", "acme_member_write_legacy")
        .await
        .assert(403, r#"{"message":"Missing required team role."}"#);
    // Demoting an owner invalidates their elevated tokens immediately.
    sqlx::query("UPDATE team_user SET role = 'member' WHERE team_id = 1 AND user_id = 1")
        .execute(h.tdb.db.pool())
        .await
        .unwrap();
    h.get("/api/v1/projects", "acme_owner_root")
        .await
        .assert(403, r#"{"message":"Missing required team role."}"#);
    h.get("/api/v1/projects", "acme_owner_read").await;
    h.finish().await;
}

#[tokio::test]
async fn abilities_are_enforced() {
    let h = Harness::new().await;
    h.get("/api/v1/version", "acme_owner_write")
        .await
        .assert(403, r#"{"message":"Missing required permissions: read"}"#);
    h.call(
        "POST",
        "/api/v1/projects",
        Some("acme_owner_read"),
        Some(serde_json::json!({"name": "Nope"})),
    )
    .await
    .assert(403, r#"{"message":"Missing required permissions: write"}"#);
    let v = h.get("/api/v1/version", "acme_owner_root").await;
    v.assert(200, common::VERSION);
    assert!(v.ctype.starts_with("text/html"));
    h.finish().await;
}

#[tokio::test]
async fn api_disabled_and_ip_allowlist() {
    let h = Harness::new().await;
    let pool = h.tdb.db.pool().clone();
    sqlx::query("UPDATE instance_settings SET allowed_ips = '10.0.0.0/8' WHERE id = 0")
        .execute(&pool)
        .await
        .unwrap();
    // In-process requests come from 127.0.0.1.
    h.get("/api/v1/projects", "acme_owner_read").await.assert(
        403,
        r#"{"success":false,"message":"You are not allowed to access the API."}"#,
    );
    sqlx::query("UPDATE instance_settings SET allowed_ips = '127.0.0.1, 10.0.0.0/8' WHERE id = 0")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        h.get("/api/v1/projects", "acme_owner_read").await.status,
        200
    );

    sqlx::query("UPDATE instance_settings SET is_api_enabled = false WHERE id = 0")
        .execute(&pool)
        .await
        .unwrap();
    h.get("/api/v1/projects", "acme_owner_read")
        .await
        .assert(403, r#"{"success":false,"message":"API is disabled."}"#);
    // Authentication still runs first.
    h.call("GET", "/api/v1/projects", None, None)
        .await
        .assert(401, r#"{"message":"Unauthenticated."}"#);
    h.finish().await;
}

#[tokio::test]
async fn disable_endpoint() {
    let h = Harness::new().await;
    let r = h
        .call("GET", "/api/v1/disable", Some("acme_owner_write"), None)
        .await;
    r.assert(
        405,
        r#"{"message":"This endpoint has changed to a POST request."}"#,
    );
    assert_eq!(r.allow.as_deref(), Some("POST"));
    h.call("POST", "/api/v1/disable", Some("acme_owner_write"), None)
        .await
        .assert(
            403,
            r#"{"message":"You are not allowed to disable the API."}"#,
        );
    h.call("POST", "/api/v1/disable", Some("root_root"), None)
        .await
        .assert(200, r#"{"message":"API disabled."}"#);
    h.get("/api/v1/projects", "root_read")
        .await
        .assert(403, r#"{"success":false,"message":"API is disabled."}"#);
    h.finish().await;
}

#[tokio::test]
async fn last_used_at_is_recorded() {
    let h = Harness::new().await;
    h.get("/api/v1/version", "acme_owner_read").await;
    let used: Option<chrono::NaiveDateTime> = sqlx::query_scalar(
        "SELECT last_used_at FROM personal_access_tokens WHERE name = 'acme_owner_read'",
    )
    .fetch_one(h.tdb.db.pool())
    .await
    .unwrap();
    assert!(used.is_some());
    h.finish().await;
}

#[tokio::test]
async fn unknown_routes() {
    let h = Harness::new().await;
    let nf = r#"{"message":"Not found.","docs":"https:\/\/coolify.io\/docs"}"#;
    h.call("GET", "/api/v1/nope", None, None)
        .await
        .assert(404, nf);
    h.call("DELETE", "/api/v1/projects", Some("acme_owner_root"), None)
        .await
        .assert(404, nf);
    let r = h
        .call(
            "POST",
            "/api/v1/applications/x/migrate",
            Some("acme_owner_root"),
            None,
        )
        .await;
    assert_eq!(r.status, 404);
    h.finish().await;
}
