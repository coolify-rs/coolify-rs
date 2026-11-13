//! Image scanning API (4.7.x): `VulnerabilitiesController` and `ServerImageScanningController`.
//! The scanner never runs here: a fresh server does not scan images, so its resources are
//! `not_checked` (scanning off for the server).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::os::unix::fs::PermissionsExt;

use common::Harness;
use coolify_remote::{Remote, RemoteConfig};
use serde_json::{Value, json};

const W: Option<&str> = Some("acme_owner_write");
const R: Option<&str> = Some("acme_owner_read");

/// Answers every script with "ok".
const FAKE_SSH: &str = "#!/bin/sh\ncat >/dev/null\necho ok\n";

struct Fixture {
    h: Harness,
    _tmp: tempfile::TempDir,
    server: String,
    database: String,
}

async fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let bin = root.join("bin");
    std::fs::create_dir(&bin).unwrap();
    std::fs::write(bin.join("ssh"), FAKE_SSH).unwrap();
    std::fs::set_permissions(bin.join("ssh"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut cfg = RemoteConfig::new(root.join("data"));
    cfg.ssh_binary = bin.join("ssh");
    let h = Harness::with_remote(Remote::new(cfg).unwrap()).await;

    let pem = coolify_compat::sshkey::generate(coolify_compat::sshkey::KeyKind::Ed25519).unwrap();
    let key = h
        .call(
            "POST",
            "/api/v1/security/keys",
            W,
            Some(json!({"name": "key", "private_key": pem})),
        )
        .await
        .json()["uuid"]
        .as_str()
        .unwrap()
        .to_owned();
    let server = h
        .call(
            "POST",
            "/api/v1/servers",
            W,
            Some(json!({"name": "srv", "ip": "203.0.113.51", "private_key_uuid": key})),
        )
        .await
        .json()["uuid"]
        .as_str()
        .unwrap()
        .to_owned();
    sqlx::query(
        "UPDATE server_settings SET is_reachable = true, is_usable = true, docker_version = '29.1.3' \
         WHERE server_id = (SELECT id FROM servers WHERE uuid = $1)",
    )
    .bind(&server)
    .execute(h.tdb.db.pool())
    .await
    .unwrap();
    let project = h
        .call("POST", "/api/v1/projects", W, Some(json!({"name": "Shop"})))
        .await
        .json()["uuid"]
        .as_str()
        .unwrap()
        .to_owned();
    let r = h
        .call(
            "POST",
            "/api/v1/databases/postgresql",
            W,
            Some(json!({"server_uuid": server, "project_uuid": project, "environment_name": "production", "name": "orders"})),
        )
        .await;
    assert_eq!(r.status, 201, "{}", r.body);
    let database = r.json()["uuid"].as_str().unwrap().to_owned();
    Fixture {
        h,
        _tmp: tmp,
        server,
        database,
    }
}

#[tokio::test]
async fn vulnerabilities_list_resources_that_are_not_checked() {
    let f = fixture().await;
    let r = f.h.call("GET", "/api/v1/vulnerabilities", R, None).await;
    assert_eq!(r.status, 200, "{}", r.body);
    let body = r.json();
    let resources = body["resources"].as_array().unwrap();
    assert_eq!(resources.len(), 1);
    let entry = &resources[0];
    assert_eq!(entry["uuid"], json!(f.database));
    assert_eq!(entry["type"], json!("database"));
    assert_eq!(entry["server_uuid"], json!(f.server));
    assert_eq!(entry["security"]["status"], json!("not_checked"));
    assert_eq!(entry["security"]["label"], json!("Not checked"));
    assert_eq!(
        entry["security"]["action"],
        json!("Image scanning is off for this server.")
    );
    assert_eq!(entry["security"]["check_reason"], json!("server"));

    // The status filter.
    let r =
        f.h.call("GET", "/api/v1/vulnerabilities?status=update_now", R, None)
            .await;
    r.assert(200, r#"{"resources":[]}"#);
    let r =
        f.h.call("GET", "/api/v1/vulnerabilities?status=bad", R, None)
            .await;
    r.assert(
        422,
        r#"{"message":"Validation failed.","errors":{"status":["Allowed values: update_now, update_available, no_fix, check_needed, ok, not_checked."]}}"#,
    );
    // Without a token.
    let r = f.h.call("GET", "/api/v1/vulnerabilities", None, None).await;
    assert_eq!(r.status, 401);
}

#[tokio::test]
async fn vulnerabilities_of_one_resource() {
    let f = fixture().await;
    let path = format!("/api/v1/databases/{}/vulnerabilities", f.database);
    let r = f.h.call("GET", &path, R, None).await;
    assert_eq!(r.status, 200, "{}", r.body);
    let body: Value = r.json();
    assert_eq!(body["uuid"], json!(f.database));
    assert_eq!(body["type"], json!("database"));
    assert_eq!(body["off_reason"], json!("server"));
    assert_eq!(body["security"], Value::Null);
    assert_eq!(body["images"], json!([]));
    assert_eq!(body["findings"], json!([]));
    assert_eq!(
        body["pagination"],
        json!({"page": 1, "per_page": 50, "total": 0})
    );

    // Validation of the query.
    let r =
        f.h.call("GET", &format!("{path}?per_page=500&state=bogus"), R, None)
            .await;
    assert_eq!(r.status, 422, "{}", r.body);
    assert!(r.json()["errors"]["per_page"].is_array());

    // Another team, and a resource type that does not match.
    let r = f.h.call("GET", &path, Some("carol_root"), None).await;
    r.assert(404, r#"{"message":"Database not found."}"#);
    let r =
        f.h.call(
            "GET",
            &format!("/api/v1/applications/{}/vulnerabilities", f.database),
            R,
            None,
        )
        .await;
    r.assert(404, r#"{"message":"Application not found."}"#);
}

#[tokio::test]
async fn server_image_scanning_settings() {
    let f = fixture().await;
    let path = format!("/api/v1/servers/{}/image-scanning", f.server);
    let r = f.h.call("GET", &path, R, None).await;
    assert_eq!(r.status, 200, "{}", r.body);
    let body = r.json();
    assert_eq!(body["is_image_scan_enabled"], json!(false));
    assert_eq!(body["image_scan_memory_mb"], json!(1024));
    assert_eq!(body["image_scan_syft_timeout"], json!(600));
    assert_eq!(body["image_scan_grype_timeout"], json!(600));
    assert_eq!(body["scanner_state"], Value::Null);

    // `viewSecurity` and `update` need a team admin or owner.
    let r = f.h.call("GET", &path, Some("acme_member_read"), None).await;
    assert_eq!(r.status, 403, "{}", r.body);
    let r =
        f.h.call(
            "PATCH",
            &path,
            Some("acme_member_write_legacy"),
            Some(json!({"image_scan_syft_timeout": 1200})),
        )
        .await;
    assert_eq!(r.status, 403, "{}", r.body);

    // A read token cannot write.
    let r =
        f.h.call(
            "PATCH",
            &path,
            R,
            Some(json!({"image_scan_syft_timeout": 1200})),
        )
        .await;
    assert_eq!(r.status, 403, "{}", r.body);

    // Validation.
    let r =
        f.h.call(
            "PATCH",
            &path,
            W,
            Some(json!({"image_scan_syft_timeout": 1234, "foo": 1})),
        )
        .await;
    r.assert(
        422,
        r#"{"message":"Validation failed.","errors":{"image_scan_syft_timeout":["The selected image scan syft timeout is invalid."],"foo":["This field is not allowed."]}}"#,
    );

    // A limit changes and is audited.
    let r =
        f.h.call(
            "PATCH",
            &path,
            W,
            Some(json!({"image_scan_syft_timeout": 1200})),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.json()["image_scan_syft_timeout"], json!(1200));
    let audited: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_events WHERE event = 'api.server.image_scanner_syft_timeout_changed'",
    )
    .fetch_one(f.h.tdb.db.pool())
    .await
    .unwrap();
    assert_eq!(audited, 1);

    // Another team does not find the server.
    let r = f.h.call("GET", &path, Some("carol_root"), None).await;
    r.assert(404, r#"{"message":"Server not found."}"#);
}
