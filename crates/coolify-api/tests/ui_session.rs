//! Browser sessions and the `/ui/api/v1` bridge.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{Harness, PASSWORD};
use serde_json::json;

#[tokio::test]
async fn login_logout_and_bridge() {
    let h = Harness::new().await;
    // Wrong password: Fortify's message.
    h.request("POST", "/ui/session", &[], Some(json!({"email": "alice@example.test", "password": "nope"})))
        .await
        .assert(
            422,
            r#"{"message":"These credentials do not match our records.","errors":{"email":["These credentials do not match our records."]}}"#,
        );
    // Emails are case-insensitive.
    let (cookie, csrf) = h.ui_login("ALICE@example.test").await;
    let info = h
        .request("GET", "/ui/session", &[("cookie", &cookie)], None)
        .await
        .json();
    assert_eq!(info["user"]["email"], "alice@example.test");
    assert_eq!(info["current_team"]["name"], "Acme");
    assert_eq!(info["current_team"]["role"], "owner");

    // Bridge: reads with the cookie, writes need the CSRF header.
    let r = h
        .request("GET", "/ui/api/v1/projects", &[("cookie", &cookie)], None)
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    let r = h
        .request(
            "POST",
            "/ui/api/v1/projects",
            &[("cookie", &cookie)],
            Some(json!({"name": "From UI"})),
        )
        .await;
    r.assert(419, r#"{"message":"CSRF token mismatch."}"#);
    let r = h
        .request(
            "POST",
            "/ui/api/v1/projects",
            &[("cookie", &cookie), ("x-csrf-token", &csrf)],
            Some(json!({"name": "From UI"})),
        )
        .await;
    assert_eq!(r.status, 201, "{}", r.body);
    // The bridge works even when the REST API is disabled (upstream canvas behaviour).
    sqlx::query("UPDATE instance_settings SET is_api_enabled = false")
        .execute(h.tdb.db.pool())
        .await
        .unwrap();
    assert_eq!(
        h.request("GET", "/ui/api/v1/projects", &[("cookie", &cookie)], None)
            .await
            .status,
        200
    );
    // No session, no access; unknown bridge path is the API 404.
    h.request("GET", "/ui/api/v1/projects", &[], None)
        .await
        .assert(401, r#"{"message":"Unauthenticated."}"#);
    assert_eq!(
        h.request(
            "GET",
            "/ui/api/v1/github-apps",
            &[("cookie", &cookie)],
            None
        )
        .await
        .status,
        501
    );

    // Logout ends the session.
    let r = h
        .request(
            "DELETE",
            "/ui/session",
            &[("cookie", &cookie), ("x-csrf-token", &csrf)],
            None,
        )
        .await;
    assert_eq!(r.status, 200);
    assert!(r.set_cookie.unwrap().contains("Max-Age=0"));
    assert_eq!(
        h.request("GET", "/ui/session", &[("cookie", &cookie)], None)
            .await
            .status,
        401
    );
    h.finish().await;
}

#[tokio::test]
async fn members_are_read_only_in_the_ui() {
    let h = Harness::new().await;
    let (cookie, csrf) = h.ui_login("bob@example.test").await;
    let headers = [("cookie", cookie.as_str()), ("x-csrf-token", csrf.as_str())];
    h.request(
        "POST",
        "/ui/api/v1/projects",
        &headers,
        Some(json!({"name": "Nope"})),
    )
    .await
    .assert(403, r#"{"message":"Missing required permissions: write"}"#);
    assert_eq!(
        h.request("GET", "/ui/api/v1/team/members", &headers, None)
            .await
            .status,
        200
    );
    h.finish().await;
}

#[tokio::test]
async fn password_change_and_membership_loss_end_access() {
    let h = Harness::new().await;
    let (cookie, _) = h.ui_login("alice@example.test").await;
    sqlx::query("DELETE FROM team_user WHERE user_id = 1")
        .execute(h.tdb.db.pool())
        .await
        .unwrap();
    h.request("GET", "/ui/api/v1/projects", &[("cookie", &cookie)], None)
        .await
        .assert(
            403,
            r#"{"message":"You are no longer a member of this team."}"#,
        );
    sqlx::query("UPDATE users SET password = 'changed' WHERE id = 1")
        .execute(h.tdb.db.pool())
        .await
        .unwrap();
    assert_eq!(
        h.request("GET", "/ui/session", &[("cookie", &cookie)], None)
            .await
            .status,
        401
    );
    h.finish().await;
}

#[tokio::test]
async fn team_switching_is_limited_to_memberships() {
    let h = Harness::with_extra_sql(
        "INSERT INTO team_user (team_id, user_id, role) VALUES (0, 1, 'member');",
    )
    .await;
    let (cookie, csrf) = h.ui_login("alice@example.test").await;
    let headers = [("cookie", cookie.as_str()), ("x-csrf-token", csrf.as_str())];
    let r = h
        .request(
            "POST",
            "/ui/session/team",
            &headers,
            Some(json!({"team_id": 0})),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.json()["current_team"]["role"], "member");
    let stored: Option<i64> = sqlx::query_scalar("SELECT current_team_id FROM users WHERE id = 1")
        .fetch_one(h.tdb.db.pool())
        .await
        .unwrap();
    assert_eq!(stored, Some(0));
    h.request(
        "POST",
        "/ui/session/team",
        &headers,
        Some(json!({"team_id": 2})),
    )
    .await
    .assert(403, r#"{"message":"You are not a member of this team."}"#);
    h.finish().await;
}

#[tokio::test]
async fn two_factor_login() {
    let h = Harness::new().await;
    // Fortify stores encrypt($secret): a PHP-serialized string.
    let secret = "JBSWY3DPEHPK3PXP";
    let enc = coolify_compat::Encrypter::new(common::APP_KEY).unwrap();
    let ciphertext = enc.encrypt_string(&format!("s:{}:\"{secret}\";", secret.len()));
    sqlx::query(
        "UPDATE users SET two_factor_secret = $1, two_factor_confirmed_at = now() WHERE id = 1",
    )
    .bind(ciphertext)
    .execute(h.tdb.db.pool())
    .await
    .unwrap();
    let body = json!({"email": "alice@example.test", "password": PASSWORD});
    h.request("POST", "/ui/session", &[], Some(body.clone()))
        .await
        .assert(200, r#"{"two_factor":true}"#);
    let mut wrong = body.clone();
    wrong["code"] = json!("000000");
    assert_eq!(
        h.request("POST", "/ui/session", &[], Some(wrong))
            .await
            .status,
        422
    );
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let code = coolify_compat::totp::code_at(
        &coolify_compat::totp::base32_decode(secret).unwrap(),
        now / 30,
    );
    let mut good = body.clone();
    good["code"] = json!(code);
    let r = h
        .request("POST", "/ui/session", &[], Some(good.clone()))
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert!(r.set_cookie.is_some());
    // The same code cannot be replayed.
    assert_eq!(
        h.request("POST", "/ui/session", &[], Some(good))
            .await
            .status,
        422
    );
    h.finish().await;
}

#[tokio::test]
async fn team_two_factor_policy_blocks_bridge() {
    let h = Harness::new().await;
    sqlx::query("UPDATE teams SET is_two_factor_required = true WHERE id = 1")
        .execute(h.tdb.db.pool())
        .await
        .unwrap();
    let (cookie, _) = h.ui_login("alice@example.test").await;
    let info = h
        .request("GET", "/ui/session", &[("cookie", &cookie)], None)
        .await
        .json();
    assert_eq!(info["current_team"]["two_factor_blocked"], true);
    h.request("GET", "/ui/api/v1/projects", &[("cookie", &cookie)], None)
        .await
        .assert(
            403,
            r#"{"message":"Two-factor authentication is required by this team."}"#,
        );
    h.finish().await;
}

#[tokio::test]
async fn login_is_throttled() {
    let h = Harness::new().await;
    let bad = json!({"email": "alice@example.test", "password": "nope"});
    for _ in 0..5 {
        assert_eq!(
            h.request("POST", "/ui/session", &[], Some(bad.clone()))
                .await
                .status,
            422
        );
    }
    let r = h.request("POST", "/ui/session", &[], Some(bad)).await;
    assert_eq!(r.status, 429, "{}", r.body);
    // Even the right password is refused while throttled.
    let good = json!({"email": "alice@example.test", "password": PASSWORD});
    assert_eq!(
        h.request("POST", "/ui/session", &[], Some(good))
            .await
            .status,
        429
    );
    h.finish().await;
}

#[tokio::test]
async fn serves_the_web_ui_with_security_headers() {
    let dir = std::env::temp_dir().join(format!("coolify-rs-ui-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("assets")).unwrap();
    std::fs::write(dir.join("index.html"), "<!doctype html><title>ui</title>").unwrap();
    std::fs::write(dir.join("assets/app.js"), "console.log(1)").unwrap();
    let h = Harness::with_ui(dir.clone()).await;
    // Client-side routes fall back to index.html.
    let page = h.request("GET", "/projects/abc", &[], None).await;
    assert_eq!(page.status, 200);
    assert!(page.body.contains("<title>ui</title>"));
    let csp = page.csp.unwrap_or_default();
    assert!(
        csp.contains("default-src 'self'") && csp.contains("frame-ancestors 'none'"),
        "{csp}"
    );
    let asset = h.request("GET", "/assets/app.js", &[], None).await;
    assert_eq!(asset.body, "console.log(1)");
    // API paths never fall through to the UI.
    h.request("GET", "/api/v1/nope", &[], None).await.assert(
        404,
        r#"{"message":"Not found.","docs":"https:\/\/coolify.io\/docs"}"#,
    );
    assert_eq!(h.request("GET", "/ui/nope", &[], None).await.status, 404);
    // Writes to UI paths are refused.
    assert_eq!(h.request("POST", "/projects", &[], None).await.status, 405);
    h.finish().await;
    let _ = std::fs::remove_dir_all(dir);
}
