//! `Admin\Index` in dev mode, and the self-hosted answers of the cloud pages. The reference runs in
//! production and has no root password in the fixtures, so this path is only tested here.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::Harness;
use serde_json::json;

const ROOT_USER: &str = "INSERT INTO users (id, name, email, password, created_at, updated_at) \
    SELECT 0, 'Root', 'root@example.test', password, now(), now() FROM users WHERE email = 'alice@example.test' \
    ON CONFLICT DO NOTHING; \
    INSERT INTO teams (id, name, personal_team, created_at, updated_at) VALUES (0, 'Root Team', true, now(), now()) ON CONFLICT DO NOTHING; \
    INSERT INTO team_user (team_id, user_id, role, created_at, updated_at) VALUES (0, 0, 'owner', now(), now()) ON CONFLICT DO NOTHING;";

#[tokio::test]
async fn admin_search_switch_and_back() {
    // `isDev()` reads the process environment: `APP_ENV=local cargo test -p coolify-api --test ui_admin`.
    if std::env::var("APP_ENV").as_deref() != Ok("local") {
        return;
    }
    let h = Harness::with_extra_sql(ROOT_USER).await;

    let (alice, _) = h.ui_login("alice@example.test").await;
    assert_eq!(
        h.request("GET", "/ui/admin", &[("cookie", &alice)], None)
            .await
            .status,
        403
    );

    let (root, csrf) = h.ui_login("root@example.test").await;
    let headers = [("cookie", root.as_str()), ("x-csrf-token", csrf.as_str())];
    let page = h.request("GET", "/ui/admin", &headers, None).await;
    assert_eq!(page.status, 200, "{}", page.body);
    assert_eq!(page.json()["impersonating"], false);

    let found = h
        .request(
            "POST",
            "/ui/admin/search",
            &headers,
            Some(json!({"search": "alice@"})),
        )
        .await
        .json();
    let user = &found["users"][0];
    assert_eq!(user["email"], "alice@example.test");
    assert_eq!(user["active"], false);

    let switched = h
        .request(
            "POST",
            "/ui/admin/switch",
            &headers,
            Some(json!({"user_id": user["id"]})),
        )
        .await;
    assert_eq!(switched.status, 200, "{}", switched.body);
    let session = h.request("GET", "/ui/session", &headers, None).await.json();
    assert_eq!(session["user"]["email"], "alice@example.test");
    assert_eq!(session["features"]["impersonating"], true);

    // While impersonating: the page stays open, but only root switches users.
    let page = h.request("GET", "/ui/admin", &headers, None).await.json();
    assert_eq!(page["impersonating"], true);
    let denied = h
        .request(
            "POST",
            "/ui/admin/switch",
            &headers,
            Some(json!({"user_id": 0})),
        )
        .await;
    assert_eq!(denied.status, 403);

    let back = h
        .request("POST", "/ui/admin/back", &headers, Some(json!({})))
        .await;
    assert_eq!(back.status, 200, "{}", back.body);
    let session = h.request("GET", "/ui/session", &headers, None).await.json();
    assert_eq!(session["user"]["email"], "root@example.test");
    assert_eq!(session["features"]["impersonating"], false);

    let missing = h
        .request(
            "POST",
            "/ui/admin/switch",
            &headers,
            Some(json!({"user_id": 987654})),
        )
        .await;
    assert_eq!(missing.status, 404);
    h.finish().await;
}
