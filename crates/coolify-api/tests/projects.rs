//! Projects and environments. Expected bodies come from the 4.5.0 reference
//! (compat/diff) and upstream specs (EnvironmentUpdateApiTest, ApiTokenPermissionTest).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::Harness;
use serde_json::json;

const W: Option<&str> = Some("acme_owner_write");

#[tokio::test]
async fn validation_messages_match_upstream() {
    let h = Harness::new().await;
    let post = |b| h.call("POST", "/api/v1/projects", W, Some(b));
    post(json!({})).await.assert(
        400,
        r#"{"message":"Invalid request.","error":"Empty JSON."}"#,
    );
    post(json!({"name": ""})).await.assert(
        422,
        r#"{"message":"Validation failed.","errors":{"name":["The name field is required."]}}"#,
    );
    post(json!({"name": "ab", "description": "x".repeat(300), "foo": 1})).await.assert(
        422,
        r#"{"message":"Validation failed.","errors":{"name":["The name must be at least 3 characters."],"description":["The description may not be greater than 255 characters."],"foo":["This field is not allowed."]}}"#,
    );
    post(json!({"name": "bad<name>"})).await.assert(
        422,
        r#"{"message":"Validation failed.","errors":{"name":["The name may only contain letters (including Unicode), numbers, spaces, and these characters: - _ . \/ @ & ( ) # , : +"]}}"#,
    );
    post(json!({"name": 123}))
        .await
        .assert(422, r#"{"message":"Validation failed.","errors":{"name":["The name field must be a string."]}}"#);
    h.finish().await;
}

#[tokio::test]
async fn project_lifecycle() {
    let h = Harness::new().await;
    let r = h
        .call(
            "POST",
            "/api/v1/projects",
            W,
            Some(json!({"name": "Proj One", "description": "First"})),
        )
        .await;
    assert_eq!(r.status, 201);
    let uuid = r.json()["uuid"].as_str().unwrap().to_string();
    assert_eq!(uuid.len(), 24);

    // The `production` environment and project_settings row are created.
    let envs = h
        .get(
            &format!("/api/v1/projects/{uuid}/environments"),
            "acme_owner_read",
        )
        .await;
    assert_eq!(envs.json()[0]["name"], "production");
    let settings: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM project_settings s JOIN projects p ON p.id = s.project_id WHERE p.uuid = $1",
    )
    .bind(&uuid)
    .fetch_one(h.tdb.db.pool())
    .await
    .unwrap();
    assert_eq!(settings, 1);

    // Key order of `serializeApiResponse` and nested environments.
    let show = h
        .get(&format!("/api/v1/projects/{uuid}"), "acme_owner_read")
        .await;
    let keys: Vec<String> = show.json().as_object().unwrap().keys().cloned().collect();
    assert_eq!(
        keys,
        vec![
            "id",
            "uuid",
            "description",
            "name",
            "canvas_position",
            "environments",
            "icon_path",
            "icon_s3_storage_id",
            "icon_storage_type",
            "team_id",
            "created_at",
            "updated_at"
        ]
    );

    // PATCH answers 201; an explicit null description becomes "" (strip_tags).
    let p = h
        .call(
            "PATCH",
            &format!("/api/v1/projects/{uuid}"),
            W,
            Some(json!({"description": null})),
        )
        .await;
    p.assert(
        201,
        &format!(r#"{{"uuid":"{uuid}","name":"Proj One","description":""}}"#),
    );

    // Other teams cannot see it.
    h.get(&format!("/api/v1/projects/{uuid}"), "carol_root")
        .await
        .assert(404, r#"{"message":"Project not found."}"#);
    // Validation precedes lookup.
    h.call("PATCH", "/api/v1/projects/missing", W, Some(json!({"name": "x"})))
        .await
        .assert(422, r#"{"message":"Validation failed.","errors":{"name":["The name must be at least 3 characters."]}}"#);

    h.call("DELETE", &format!("/api/v1/projects/{uuid}"), W, None)
        .await
        .assert(200, r#"{"message":"Project deleted."}"#);
    let left: i64 = sqlx::query_scalar("SELECT count(*) FROM environments")
        .fetch_one(h.tdb.db.pool())
        .await
        .unwrap();
    assert_eq!(left, 0, "deleting a project deletes its environments");
    h.finish().await;
}

#[tokio::test]
async fn environments() {
    let h = Harness::new().await;
    let uuid = h
        .call("POST", "/api/v1/projects", W, Some(json!({"name": "Envs"})))
        .await
        .json()["uuid"]
        .as_str()
        .unwrap()
        .to_string();
    let base = format!("/api/v1/projects/{uuid}");
    assert_eq!(
        h.call(
            "POST",
            &format!("{base}/environments"),
            W,
            Some(json!({"name": "staging"}))
        )
        .await
        .status,
        201
    );
    h.call(
        "POST",
        &format!("{base}/environments"),
        W,
        Some(json!({"name": "staging"})),
    )
    .await
    .assert(
        409,
        r#"{"message":"Environment with this name already exists."}"#,
    );
    h.call("POST", &format!("{base}/environments"), W, Some(json!({"name": "x1", "description": "d"}))).await.assert(
        422,
        r#"{"message":"Validation failed.","errors":{"name":["The name must be at least 3 characters."],"description":["This field is not allowed."]}}"#,
    );
    h.call(
        "PATCH",
        &format!("{base}/environments/staging"),
        W,
        Some(json!({"name": "production"})),
    )
    .await
    .assert(
        409,
        r#"{"message":"Environment with this name already exists."}"#,
    );
    let r = h
        .call(
            "PATCH",
            &format!("{base}/environments/staging"),
            W,
            Some(json!({"description": "Stage é/ü"})),
        )
        .await;
    assert_eq!(r.status, 200);
    assert!(
        r.body
            .ends_with(r#""name":"staging","description":"Stage \u00e9\/\u00fc"}"#),
        "{}",
        r.body
    );

    let details = h.get(&format!("{base}/staging"), "acme_owner_read").await;
    let d = details.json();
    assert_eq!(d["applications"], json!([]));
    assert_eq!(d["redis"], json!([]));

    h.call("DELETE", &format!("{base}/environments/staging"), W, None)
        .await
        .assert(200, r#"{"message":"Environment deleted."}"#);
    h.call("DELETE", &format!("{base}/environments/staging"), W, None)
        .await
        .assert(404, r#"{"message":"Environment not found."}"#);
    h.finish().await;
}

#[tokio::test]
async fn resources_block_deletion_unless_soft_deleted() {
    let h = Harness::new().await;
    let uuid = h
        .call("POST", "/api/v1/projects", W, Some(json!({"name": "Busy"})))
        .await
        .json()["uuid"]
        .as_str()
        .unwrap()
        .to_string();
    let pool = h.tdb.db.pool().clone();
    sqlx::query(
        "INSERT INTO services (uuid, name, environment_id, docker_compose_raw) \
         SELECT 'svc', 'svc', e.id, '' FROM environments e JOIN projects p ON p.id = e.project_id WHERE p.uuid = $1",
    )
    .bind(&uuid)
    .execute(&pool)
    .await
    .unwrap();
    h.call("DELETE", &format!("/api/v1/projects/{uuid}"), W, None)
        .await
        .assert(
            400,
            r#"{"message":"Project has resources, so it cannot be deleted."}"#,
        );
    h.call(
        "DELETE",
        &format!("/api/v1/projects/{uuid}/environments/production"),
        W,
        None,
    )
    .await
    .assert(
        400,
        r#"{"message":"Environment has resources, so it cannot be deleted."}"#,
    );
    // `environment_details` lists the resources (compat/diff/environment_details_diff.py).
    let details = h
        .get(
            &format!("/api/v1/projects/{uuid}/production"),
            "acme_owner_read",
        )
        .await;
    assert_eq!(details.status, 200);
    assert_eq!(details.json()["services"][0]["uuid"], "svc");

    sqlx::query("UPDATE services SET deleted_at = now()")
        .execute(&pool)
        .await
        .unwrap();
    h.call("DELETE", &format!("/api/v1/projects/{uuid}"), W, None)
        .await
        .assert(200, r#"{"message":"Project deleted."}"#);
    h.finish().await;
}

#[tokio::test]
async fn teams_are_token_scoped() {
    let h = Harness::new().await;
    let teams = h.get("/api/v1/teams", "acme_owner_read").await.json();
    assert_eq!(teams.as_array().unwrap().len(), 1);
    assert_eq!(teams[0]["name"], "Acme");
    assert!(teams[0].get("custom_server_limit").is_none());
    h.get("/api/v1/teams/0", "acme_owner_read")
        .await
        .assert(404, r#"{"message":"Team not found."}"#);
    let members = h
        .get("/api/v1/team/members", "acme_member_read")
        .await
        .json();
    let m = members.as_array().unwrap();
    assert_eq!(m.len(), 2);
    for user in m {
        for hidden in [
            "password",
            "remember_token",
            "two_factor_secret",
            "two_factor_recovery_codes",
            "email_change_code",
        ] {
            assert!(user.get(hidden).is_none(), "{hidden} leaked");
        }
    }
    // Members cannot change team settings even with a valid (read) token...
    h.call(
        "PATCH",
        "/api/v1/team",
        Some("acme_member_read"),
        Some(json!({"is_build_server_fallback_enabled": false})),
    )
    .await
    .assert(403, r#"{"message":"Missing required permissions: write"}"#);
    let r = h
        .call(
            "PATCH",
            "/api/v1/team",
            W,
            Some(json!({"is_build_server_fallback_enabled": false})),
        )
        .await;
    assert_eq!(r.status, 200);
    assert_eq!(r.json()["is_build_server_fallback_enabled"], false);
    h.finish().await;
}

#[tokio::test]
async fn read_only_mode_refuses_writes() {
    // A migration this build does not know puts the server in read-only mode.
    let h = Harness::with_extra_sql(
        "INSERT INTO migrations (migration, batch) VALUES ('2099_01_01_000000_future_change', 99);",
    )
    .await;
    assert!(h.tdb.db.schema().read_only_reason.is_some());
    assert_eq!(
        h.get("/api/v1/projects", "acme_owner_read").await.status,
        200
    );
    let r = h
        .call(
            "POST",
            "/api/v1/projects",
            W,
            Some(json!({"name": "Blocked"})),
        )
        .await;
    assert_eq!(r.status, 503, "{}", r.body);
    h.finish().await;
}

#[tokio::test]
async fn teams_expose_two_factor_flag_on_main_schema() {
    let h = Harness::new().await;
    let t = h.get("/api/v1/team", "acme_owner_read").await.json();
    assert_eq!(t["is_two_factor_required"], false);
    h.finish().await;
}

#[tokio::test]
async fn older_upstream_database_is_read_only() {
    // A 4.5.0 database lacks migrations this build expects.
    let h = Harness::on_4_5_0().await;
    let reason = h
        .tdb
        .db
        .schema()
        .read_only_reason
        .clone()
        .unwrap_or_default();
    assert!(reason.contains("missing 4 upstream migration"), "{reason}");
    // Reads keep working and adapt to the missing columns.
    let t = h.get("/api/v1/team", "acme_owner_read").await.json();
    assert!(t.get("is_two_factor_required").is_none());
    let r = h
        .call(
            "POST",
            "/api/v1/projects",
            W,
            Some(json!({"name": "Blocked"})),
        )
        .await;
    assert_eq!(r.status, 503, "{}", r.body);
    h.finish().await;
}
