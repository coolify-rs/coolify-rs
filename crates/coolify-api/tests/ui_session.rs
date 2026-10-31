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
            "/ui/api/v1/no-such-route",
            &[("cookie", &cookie)],
            None
        )
        .await
        .status,
        404
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
    let h = Harness::with_extra_sql(
        "INSERT INTO team_user (team_id, user_id, role) VALUES (0, 1, 'member'); \
         SELECT setval('teams_id_seq', 1000);",
    )
    .await;
    let (cookie, _) = h.ui_login("alice@example.test").await;
    let team_of = |r: &serde_json::Value| r["current_team"]["id"].as_i64();
    // `DecideWhatToDoWithUser`: the session follows to the only team left.
    sqlx::query("DELETE FROM team_user WHERE user_id = 1 AND team_id <> 0")
        .execute(h.tdb.db.pool())
        .await
        .unwrap();
    let r = h
        .request("GET", "/ui/session", &[("cookie", &cookie)], None)
        .await;
    assert_eq!(team_of(&r.json()), Some(0));
    // Without any team, a personal team is created again.
    sqlx::query("DELETE FROM team_user WHERE user_id = 1")
        .execute(h.tdb.db.pool())
        .await
        .unwrap();
    let r = h
        .request("GET", "/ui/session", &[("cookie", &cookie)], None)
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.json()["current_team"]["role"], "owner");
    assert!(team_of(&r.json()).is_some_and(|t| t > 1000));
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
    let r = h
        .request("POST", "/ui/session", &[], Some(body.clone()))
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.json()["two_factor"], true);
    let guest = r
        .set_cookie
        .clone()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    let path = "/ui/session/two-factor";
    let cookie = [("cookie", guest.as_str())];
    // Without the password step there is nobody to challenge.
    assert_eq!(
        h.request("POST", path, &[], Some(json!({"code": "000000"})))
            .await
            .status,
        422
    );
    let r = h
        .request("POST", path, &cookie, Some(json!({"code": "000000"})))
        .await;
    assert_eq!(r.status, 422);
    assert_eq!(
        r.json()["errors"]["code"][0],
        "The provided two factor authentication code was invalid."
    );
    let r = h
        .request(
            "POST",
            path,
            &cookie,
            Some(json!({"recovery_code": "nope"})),
        )
        .await;
    assert_eq!(
        r.json()["errors"]["recovery_code"][0],
        "The provided two factor recovery code was invalid."
    );
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let code = coolify_compat::totp::code_at(
        &coolify_compat::totp::base32_decode(secret).unwrap(),
        now / 30,
    );
    let good = json!({"code": code});
    let r = h.request("POST", path, &cookie, Some(good.clone())).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert!(r.set_cookie.is_some());
    // The challenge is over, and the same code cannot be replayed.
    assert_eq!(
        h.request("POST", path, &cookie, Some(good)).await.status,
        422
    );
    h.finish().await;
}

#[tokio::test]
async fn recovery_code_is_consumed() {
    let h = Harness::new().await;
    let secret = "JBSWY3DPEHPK3PXP";
    let enc = coolify_compat::Encrypter::new(common::APP_KEY).unwrap();
    let codes = json!(["aaaaaaaaaa-bbbbbbbbbb", "cccccccccc-dddddddddd"]).to_string();
    sqlx::query(
        "UPDATE users SET two_factor_secret = $1, two_factor_recovery_codes = $2, \
         two_factor_confirmed_at = now() WHERE id = 1",
    )
    .bind(enc.encrypt_serialized(secret))
    .bind(enc.encrypt_serialized(&codes))
    .execute(h.tdb.db.pool())
    .await
    .unwrap();
    let body = json!({"email": "alice@example.test", "password": PASSWORD});
    let r = h.request("POST", "/ui/session", &[], Some(body)).await;
    let guest = r
        .set_cookie
        .clone()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    let cookie = [("cookie", guest.as_str())];
    let r = h
        .request(
            "POST",
            "/ui/session/two-factor",
            &cookie,
            Some(json!({"recovery_code": "aaaaaaaaaa-bbbbbbbbbb"})),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    let stored: String =
        sqlx::query_scalar("SELECT two_factor_recovery_codes FROM users WHERE id = 1")
            .fetch_one(h.tdb.db.pool())
            .await
            .unwrap();
    let list: Vec<String> =
        serde_json::from_str(&enc.decrypt_maybe_serialized(&stored).unwrap()).unwrap();
    assert_eq!(list.len(), 2);
    assert!(!list.iter().any(|c| c == "aaaaaaaaaa-bbbbbbbbbb"));
    assert!(list.iter().any(|c| c == "cccccccccc-dddddddddd"));
    h.finish().await;
}

#[tokio::test]
async fn forced_password_reset_blocks_everything_else() {
    let h = Harness::new().await;
    sqlx::query("UPDATE users SET force_password_reset = true WHERE id = 1")
        .execute(h.tdb.db.pool())
        .await
        .unwrap();
    let (cookie, csrf) = h.ui_login("alice@example.test").await;
    let c = [("cookie", cookie.as_str()), ("x-csrf-token", csrf.as_str())];
    let info = h.request("GET", "/ui/session", &c, None).await.json();
    assert_eq!(info["force_password_reset"], true);
    assert_eq!(
        h.request("GET", "/ui/api/v1/projects", &c, None)
            .await
            .status,
        403
    );
    assert_eq!(h.request("GET", "/ui/profile", &c, None).await.status, 403);
    let weak =
        json!({"email": "alice@example.test", "password": "x", "password_confirmation": "y"});
    let r = h
        .request("POST", "/ui/auth/force-password-reset", &c, Some(weak))
        .await;
    assert_eq!(r.status, 422, "{}", r.body);
    assert!(r.json()["errors"]["password"].is_array());
    let good = json!({"email": "alice@example.test", "password": "N3w-Passw0rd!", "password_confirmation": "N3w-Passw0rd!"});
    let r = h
        .request("POST", "/ui/auth/force-password-reset", &c, Some(good))
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.json()["signed_out"], true);
    // The password change ended the session.
    assert_eq!(h.request("GET", "/ui/session", &c, None).await.status, 401);
    let flag: bool = sqlx::query_scalar("SELECT force_password_reset FROM users WHERE id = 1")
        .fetch_one(h.tdb.db.pool())
        .await
        .unwrap();
    assert!(!flag);
    h.finish().await;
}

#[tokio::test]
async fn registration_creates_user_and_personal_team() {
    let h = Harness::new().await;
    // The seed inserts explicit ids.
    for q in [
        "SELECT setval('users_id_seq', (SELECT max(id) FROM users))",
        "SELECT setval('teams_id_seq', (SELECT max(id) FROM teams))",
    ] {
        sqlx::query(q).execute(h.tdb.db.pool()).await.unwrap();
    }
    let r = h
        .request(
            "POST",
            "/ui/auth/register",
            &[],
            Some(json!({"name": "", "email": "alice@example.test", "password": "x", "password_confirmation": "y"})),
        )
        .await;
    assert_eq!(r.status, 422, "{}", r.body);
    let e = r.json();
    assert_eq!(e["errors"]["name"][0], "The name field is required.");
    assert_eq!(e["errors"]["email"][0], "The email has already been taken.");
    assert_eq!(
        e["errors"]["password"][0],
        "The password field must be at least 8 characters."
    );
    let r = h
        .request(
            "POST",
            "/ui/auth/register",
            &[],
            Some(json!({"name": "New One", "email": "New.One@Example.test", "password": "N3w-Passw0rd!", "password_confirmation": "N3w-Passw0rd!"})),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.json()["redirect"], "/");
    assert_eq!(r.json()["current_team"]["name"], "New One's Team");
    assert_eq!(r.json()["show_boarding"], true);
    let verified: Option<chrono::NaiveDateTime> = sqlx::query_scalar(
        "SELECT email_verified_at FROM users WHERE email = 'new.one@example.test'",
    )
    .fetch_one(h.tdb.db.pool())
    .await
    .unwrap();
    assert!(verified.is_some());
    h.finish().await;
}

#[tokio::test]
async fn reset_tokens_follow_the_laravel_broker() {
    let h = Harness::new().await;
    let hash = coolify_compat::password::hash("plain-token").unwrap();
    sqlx::query(
        "INSERT INTO password_reset_tokens (email, token, created_at) VALUES ('alice@example.test', $1, now())",
    )
    .bind(&hash)
    .execute(h.tdb.db.pool())
    .await
    .unwrap();
    let body = |token: &str, pass: &str| json!({"token": token, "email": "alice@example.test", "password": pass, "password_confirmation": pass});
    let r = h
        .request(
            "POST",
            "/ui/auth/reset-password",
            &[],
            Some(body("wrong", "N3w-Passw0rd!")),
        )
        .await;
    assert_eq!(
        r.json()["errors"]["email"][0],
        "This password reset token is invalid."
    );
    let r = h
        .request(
            "POST",
            "/ui/auth/reset-password",
            &[],
            Some(body("plain-token", "weak")),
        )
        .await;
    assert!(r.json()["errors"]["password"].is_array());
    let r = h
        .request(
            "POST",
            "/ui/auth/reset-password",
            &[],
            Some(body("plain-token", "N3w-Passw0rd!")),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    let left: i64 = sqlx::query_scalar("SELECT count(*) FROM password_reset_tokens")
        .fetch_one(h.tdb.db.pool())
        .await
        .unwrap();
    assert_eq!(left, 0);
    // An expired token no longer works.
    sqlx::query(
        "INSERT INTO password_reset_tokens (email, token, created_at) VALUES ('alice@example.test', $1, now() - interval '11 minutes')",
    )
    .bind(&hash)
    .execute(h.tdb.db.pool())
    .await
    .unwrap();
    let r = h
        .request(
            "POST",
            "/ui/auth/reset-password",
            &[],
            Some(body("plain-token", "N3w-Passw0rd!")),
        )
        .await;
    assert_eq!(r.status, 422);
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
            r#"{"message":"Your team requires two-factor authentication. Configure it to continue."}"#,
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

#[tokio::test]
async fn several_teams_without_a_stored_choice_wait_for_selection() {
    let h = Harness::with_extra_sql(
        "INSERT INTO team_user (team_id, user_id, role) VALUES (0, 1, 'member'); \
         UPDATE users SET current_team_id = NULL WHERE id = 1;",
    )
    .await;
    let (cookie, csrf) = h.ui_login("alice@example.test").await;
    let headers = [("cookie", cookie.as_str()), ("x-csrf-token", csrf.as_str())];
    let r = h.request("GET", "/ui/session", &headers, None).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert!(r.json()["current_team"].is_null());
    h.request("GET", "/ui/api/v1/projects", &headers, None)
        .await
        .assert(
            403,
            r#"{"message":"You are no longer a member of this team."}"#,
        );
    let r = h
        .request(
            "POST",
            "/ui/session/team",
            &headers,
            Some(json!({"team_id": 0})),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.json()["current_team"]["id"], 0);
    h.finish().await;
}
