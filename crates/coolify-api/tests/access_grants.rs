//! Scoped access grants of team members and API tokens (upstream 4.6.0, 2796609e9 and later):
//! paused tokens, token scopes, the token-within-creator-access check, the `delete` ability,
//! `VisibleThroughAccessGrants` on projects, and the team-wide gate of the UI. The baseline schema
//! is 4.6.0; the last test drops the 4.6.0 columns to check a database that is not upgraded yet.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{Harness, Resp};
use serde_json::{Value, json};

/// Two projects of the Acme team.
const SQL: &str = r#"
INSERT INTO projects (id, uuid, name, team_id, created_at, updated_at) VALUES
  (101, 'p-one', 'One', 1, now(), now()),
  (102, 'p-two', 'Two', 1, now(), now());
INSERT INTO environments (id, uuid, name, project_id, created_at, updated_at) VALUES
  (201, 'e-one', 'production', 101, now(), now()),
  (202, 'e-two', 'production', 102, now(), now());
"#;

const EXCEEDS: &str = r#"{"message":"This API token has more access than you have in this team. Revoke it and create a new token."}"#;
const SCOPED: &str = r#"{"message":"This API token is limited to selected projects, environments, or resources, so it cannot use this endpoint."}"#;

async fn exec(h: &Harness, sql: &str) {
    sqlx::query(sql).execute(h.tdb.db.pool()).await.unwrap();
}

/// A token of `user` in the Acme team with these abilities and scopes; returns the bearer value.
async fn token(h: &Harness, user: i64, abilities: &str, scopes: Option<&str>) -> String {
    let plain = format!("grant-test-{}", rand_suffix());
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO personal_access_tokens (tokenable_type, tokenable_id, name, token, team_id, abilities, scopes, created_at, updated_at) \
         VALUES ('App\\Models\\User', $1, 'grant test', $2, '1', $3, $4::json, now(), now()) RETURNING id",
    )
    .bind(user)
    .bind(coolify_compat::hash_token(&plain))
    .bind(abilities)
    .bind(scopes)
    .fetch_one(h.tdb.db.pool())
    .await
    .unwrap();
    format!("{id}|{plain}")
}

fn rand_suffix() -> String {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    format!(
        "{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    )
}

async fn send(h: &Harness, method: &str, path: &str, bearer: &str, body: Option<Value>) -> Resp {
    h.request(
        method,
        path,
        &[("authorization", &format!("Bearer {bearer}"))],
        body,
    )
    .await
}

fn project_uuids(r: &Resp) -> Vec<String> {
    let mut v: Vec<String> = r
        .json()
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["uuid"].as_str().unwrap().to_string())
        .collect();
    v.sort();
    v
}

#[tokio::test]
async fn paused_tokens_do_not_authenticate() {
    let h = Harness::with_extra_sql(SQL).await;
    let t = token(&h, 1, r#"["root"]"#, None).await;
    assert_eq!(
        send(&h, "GET", "/api/v1/projects", &t, None).await.status,
        200
    );
    let id = t.split('|').next().unwrap();
    exec(
        &h,
        &format!("UPDATE personal_access_tokens SET paused_at = now() WHERE id = {id}"),
    )
    .await;
    send(&h, "GET", "/api/v1/projects", &t, None)
        .await
        .assert(401, r#"{"message":"Unauthenticated."}"#);
    // Resumed: the same secret works again.
    exec(
        &h,
        &format!("UPDATE personal_access_tokens SET paused_at = NULL WHERE id = {id}"),
    )
    .await;
    assert_eq!(
        send(&h, "GET", "/api/v1/projects", &t, None).await.status,
        200
    );
    h.finish().await;
}

#[tokio::test]
async fn scoped_token_sees_only_its_projects_and_no_team_level_routes() {
    let h = Harness::with_extra_sql(SQL).await;
    let t = token(
        &h,
        1,
        r#"["root"]"#,
        Some(r#"[{"scope":"project","uuid":"p-one"}]"#),
    )
    .await;
    let r = send(&h, "GET", "/api/v1/projects", &t, None).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(project_uuids(&r), vec!["p-one"]);
    // A project outside the scope does not exist for the token.
    send(&h, "GET", "/api/v1/projects/p-two", &t, None)
        .await
        .assert(404, r#"{"message":"Project not found."}"#);
    // Team-level endpoints are refused before the ability check, and audited.
    send(&h, "GET", "/api/v1/servers", &t, None)
        .await
        .assert(403, SCOPED);
    send(&h, "GET", "/api/v1/security/keys", &t, None)
        .await
        .assert(403, SCOPED);
    let denied: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_events WHERE event = 'api.auth.scope_denied'",
    )
    .fetch_one(h.tdb.db.pool())
    .await
    .unwrap();
    // 4.7.5: one row per token in each 300 second window, whatever the path.
    assert_eq!(denied, 1);
    // `version` is one of the scoped routes.
    assert_eq!(
        send(&h, "GET", "/api/v1/version", &t, None).await.status,
        200
    );
    // An unscoped token of the same owner sees both projects and the team routes.
    let r = h.get("/api/v1/projects", "acme_owner_root").await;
    assert_eq!(project_uuids(&r), vec!["p-one", "p-two"]);
    assert_eq!(
        h.get("/api/v1/servers", "acme_owner_root").await.status,
        200
    );
    h.finish().await;
}

#[tokio::test]
async fn scoped_admin_token_cannot_act_outside_its_scope() {
    let h = Harness::with_extra_sql(SQL).await;
    let t = token(
        &h,
        1,
        r#"["read","write","delete"]"#,
        Some(r#"[{"scope":"project","uuid":"p-one"}]"#),
    )
    .await;
    // Hidden: not found, not forbidden.
    send(
        &h,
        "PATCH",
        "/api/v1/projects/p-two",
        &t,
        Some(json!({"name": "Nope"})),
    )
    .await
    .assert(404, r#"{"message":"Project not found."}"#);
    send(&h, "DELETE", "/api/v1/projects/p-two", &t, None)
        .await
        .assert(404, r#"{"message":"Project not found."}"#);
    // Creating a project needs write on the whole team (`ProjectPolicy::create`).
    let r = send(
        &h,
        "POST",
        "/api/v1/projects",
        &t,
        Some(json!({"name": "Third"})),
    )
    .await;
    assert_eq!(r.status, 403, "{}", r.body);
    // Inside the scope the token works.
    let r = send(
        &h,
        "PATCH",
        "/api/v1/projects/p-one",
        &t,
        Some(json!({"name": "Renamed"})),
    )
    .await;
    assert_eq!(r.status, 201, "{}", r.body);
    h.finish().await;
}

#[tokio::test]
async fn member_tokens_cannot_exceed_the_member_access() {
    let h = Harness::with_extra_sql(SQL).await;
    // Without stored grants Bob reads the whole team (the default), with the fixture read token.
    let r = h.get("/api/v1/projects", "acme_member_read").await;
    assert_eq!(project_uuids(&r), vec!["p-one", "p-two"]);

    exec(
        &h,
        r#"UPDATE team_user SET access = '[{"scope":"project","uuid":"p-one","levels":["write"]}]' WHERE team_id = 1 AND user_id = 2"#,
    )
    .await;
    // Team-wide read is now more than Bob has.
    h.get("/api/v1/projects", "acme_member_read")
        .await
        .assert(403, EXCEEDS);
    let audited: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_events WHERE event = 'api.auth.token_exceeds_access'",
    )
    .fetch_one(h.tdb.db.pool())
    .await
    .unwrap();
    assert_eq!(audited, 1);
    // A token limited to Bob's project, with levels he holds there.
    let t = token(
        &h,
        2,
        r#"["read","write"]"#,
        Some(r#"[{"scope":"project","uuid":"p-one"}]"#),
    )
    .await;
    let r = send(&h, "GET", "/api/v1/projects", &t, None).await;
    assert_eq!(project_uuids(&r), vec!["p-one"]);
    let r = send(
        &h,
        "PATCH",
        "/api/v1/projects/p-one",
        &t,
        Some(json!({"name": "Bob project"})),
    )
    .await;
    assert_eq!(r.status, 201, "{}", r.body);
    // Delete is not part of write: a token with it is more than Bob has.
    let t = token(
        &h,
        2,
        r#"["read","write","delete"]"#,
        Some(r#"[{"scope":"project","uuid":"p-one"}]"#),
    )
    .await;
    send(&h, "DELETE", "/api/v1/projects/p-one", &t, None)
        .await
        .assert(403, EXCEEDS);
    // Abilities outside the access levels are for owners and admins only.
    let t = token(&h, 2, r#"["write:sensitive"]"#, None).await;
    send(&h, "GET", "/api/v1/projects", &t, None)
        .await
        .assert(403, EXCEEDS);
    h.finish().await;
}

#[tokio::test]
async fn member_grants_allow_writes_the_role_alone_would_not() {
    let h = Harness::with_extra_sql(SQL).await;
    exec(
        &h,
        r#"UPDATE team_user SET access = '[{"scope":"team","uuid":null,"levels":["read"]},{"scope":"project","uuid":"p-two","levels":["write","delete"]}]' WHERE team_id = 1 AND user_id = 2"#,
    )
    .await;
    let t = token(
        &h,
        2,
        r#"["read","write","delete"]"#,
        Some(r#"[{"scope":"project","uuid":"p-two"}]"#),
    )
    .await;
    // Write on p-two, read only on p-one (through the token scope p-one is hidden).
    let r = send(&h, "DELETE", "/api/v1/projects/p-two", &t, None).await;
    assert_eq!(r.status, 200, "{}", r.body);
    // An unscoped read token of Bob covers the whole team: it may read, but not change, p-one.
    let t = token(&h, 2, r#"["read"]"#, None).await;
    let r = send(&h, "GET", "/api/v1/projects", &t, None).await;
    assert_eq!(project_uuids(&r), vec!["p-one"]);
    h.finish().await;
}

#[tokio::test]
async fn delete_routes_need_the_delete_ability() {
    let h = Harness::with_extra_sql(SQL).await;
    h.call(
        "DELETE",
        "/api/v1/projects/p-two",
        Some("acme_owner_write"),
        None,
    )
    .await
    .assert(403, r#"{"message":"Missing required permissions: delete"}"#);
    let r = h
        .call(
            "DELETE",
            "/api/v1/projects/p-two",
            Some("acme_owner_delete"),
            None,
        )
        .await;
    r.assert(200, r#"{"message":"Project deleted."}"#);
    h.finish().await;
}

#[tokio::test]
async fn restricted_members_get_no_team_level_pages() {
    let h = Harness::with_extra_sql(SQL).await;
    exec(
        &h,
        r#"UPDATE team_user SET access = '[{"scope":"project","uuid":"p-one","levels":["read"]}]' WHERE team_id = 1 AND user_id = 2"#,
    )
    .await;
    let (cookie, csrf) = h.ui_login("bob@example.test").await;
    let get = |path: &'static str| {
        let cookie = cookie.clone();
        let csrf = csrf.clone();
        let h = &h;
        async move {
            h.request(
                "GET",
                path,
                &[("cookie", &cookie), ("x-csrf-token", &csrf)],
                None,
            )
            .await
        }
    };
    let denied = r#"{"message":"Your access in this team is limited to some projects."}"#;
    get("/ui/servers").await.assert(403, denied);
    get("/ui/keys").await.assert(403, denied);
    get("/ui/team/invitations").await.assert(403, denied);
    // The API bridge keeps only the project and resource endpoints.
    get("/ui/api/v1/servers").await.assert(403, denied);
    // The dashboard lists only the granted project and hides servers and keys.
    let r = get("/ui/dashboard").await;
    assert_eq!(r.status, 200, "{}", r.body);
    let d = r.json();
    assert_eq!(d["shows_team_sections"], json!(false));
    assert_eq!(d["servers"], json!([]));
    let projects: Vec<&str> = d["projects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["uuid"].as_str().unwrap())
        .collect();
    assert_eq!(projects, vec!["p-one"]);
    // The session flags the restriction for the navigation and the 403 pages.
    let s = get("/ui/session").await.json();
    assert_eq!(s["current_team"]["covers_whole_team"], json!(false));
    h.finish().await;
}

#[tokio::test]
async fn making_someone_a_member_resets_their_access() {
    let h = Harness::with_extra_sql(SQL).await;
    exec(
        &h,
        r#"UPDATE team_user SET role = 'admin', access = '[{"scope":"project","uuid":"p-one","levels":["read"]}]' WHERE team_id = 1 AND user_id = 2"#,
    )
    .await;
    coolify_db::teams::set_member_role(&h.tdb.db, 1, 2, coolify_domain::Role::Member)
        .await
        .unwrap();
    let access: Option<String> =
        sqlx::query_scalar("SELECT access::text FROM team_user WHERE team_id = 1 AND user_id = 2")
            .fetch_one(h.tdb.db.pool())
            .await
            .unwrap();
    assert_eq!(access, None);
    h.finish().await;
}

#[tokio::test]
async fn without_the_4_6_0_columns_nothing_changes() {
    // The 4.5.3 schema: no grants, no scopes, no pause. Members read the whole team.
    let h = Harness::with_extra_sql(
        "ALTER TABLE team_user DROP COLUMN access; \
         ALTER TABLE personal_access_tokens DROP COLUMN scopes, DROP COLUMN paused_at; \
         ALTER TABLE team_invitations DROP COLUMN access;",
    )
    .await;
    assert_eq!(
        h.get("/api/v1/projects", "acme_member_read").await.status,
        200
    );
    assert_eq!(
        h.get("/api/v1/servers", "acme_member_read").await.status,
        200
    );
    h.finish().await;
}
