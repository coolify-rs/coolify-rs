//! Servers API (read side, create, update, delete). Response shapes and the
//! rows written were compared with the upstream `sha-b5823c7` reference by
//! compat/diff/differential.py; these tests pin the behaviour locally.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{APP_KEY, Harness};
use coolify_compat::Encrypter;
use serde_json::json;

const W: Option<&str> = Some("acme_owner_write");

async fn key_uuid(h: &Harness) -> String {
    let pem = coolify_compat::sshkey::generate(coolify_compat::sshkey::KeyKind::Ed25519).unwrap();
    let r = h
        .call(
            "POST",
            "/api/v1/security/keys",
            W,
            Some(json!({"name": "srv key", "private_key": pem})),
        )
        .await;
    assert_eq!(r.status, 201, "{}", r.body);
    r.json()["uuid"].as_str().unwrap().to_string()
}

async fn scalar_i64(h: &Harness, sql: &str) -> i64 {
    sqlx::query_scalar(sql)
        .fetch_one(h.tdb.db.pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn create_writes_upstream_rows_and_reads_back() {
    let h = Harness::new().await;
    let key = key_uuid(&h).await;
    let r = h
        .call(
            "POST",
            "/api/v1/servers",
            W,
            Some(json!({"name": "web <b>1</b>", "ip": "[2001:DB8::0001]", "private_key_uuid": key, "user": " deploy ", "port": "2222", "proxy_type": "caddy"})),
        )
        .await;
    assert_eq!(r.status, 201, "{}", r.body);
    let uuid = r.json()["uuid"].as_str().unwrap().to_string();

    // Rows written by the model hooks.
    let (ip, user, port, proxy, name, description): (String, String, i32, String, String, String) = sqlx::query_as(
        "SELECT ip, \"user\", port, proxy::text, name, description FROM servers WHERE uuid = $1",
    )
    .bind(&uuid)
    .fetch_one(h.tdb.db.pool())
    .await
    .unwrap();
    assert_eq!(
        (ip.as_str(), user.as_str(), port),
        ("2001:db8::1", "deploy", 2222)
    );
    assert_eq!(
        proxy,
        r#"{"redirect_enabled":true,"type":"CADDY","status":"exited"}"#
    );
    assert_eq!((name.as_str(), description.as_str()), ("web 1", ""));
    let sid = scalar_i64(&h, &format!("SELECT id FROM servers WHERE uuid = '{uuid}'")).await;
    assert_eq!(scalar_i64(&h, &format!("SELECT count(*) FROM standalone_dockers WHERE server_id = {sid} AND network = 'coolify'")).await, 1);
    let token: String = sqlx::query_scalar(&format!(
        "SELECT sentinel_token FROM server_settings WHERE server_id = {sid}"
    ))
    .fetch_one(h.tdb.db.pool())
    .await
    .unwrap();
    // `encrypt(json_encode(['server_uuid' => ...]))`, encrypted again by the cast.
    let enc = Encrypter::new(APP_KEY).unwrap();
    let inner = enc.decrypt_string(&token).unwrap();
    assert_eq!(
        enc.decrypt_maybe_serialized(&inner).unwrap(),
        format!(r#"{{"server_uuid":"{uuid}"}}"#)
    );
    let vars: Vec<(String, String)> = sqlx::query_as(&format!(
        "SELECT key, value FROM shared_environment_variables WHERE server_id = {sid} ORDER BY id"
    ))
    .fetch_all(h.tdb.db.pool())
    .await
    .unwrap();
    assert_eq!(vars.len(), 2);
    assert_eq!(enc.decrypt_string(&vars[0].1).unwrap(), uuid);
    assert_eq!(enc.decrypt_string(&vars[1].1).unwrap(), "web 1");

    // Read back: id and proxy_error_page hidden; secrets only with read:sensitive.
    let show = h
        .get(&format!("/api/v1/servers/{uuid}"), "acme_owner_read")
        .await
        .json();
    assert!(show.get("id").is_none() && show.get("proxy_error_page").is_none());
    assert_eq!(show["settings"]["is_build_server"], false);
    assert!(show["settings"].get("sentinel_token").is_none());
    assert!(show.get("validation_logs").is_none());
    let root = h
        .get(&format!("/api/v1/servers/{uuid}"), "acme_owner_sensitive")
        .await
        .json();
    assert_eq!(root["settings"]["sentinel_token"].as_str().unwrap(), inner);
    let keys: Vec<&str> = show
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(&keys[..3], ["uuid", "name", "canvas_position"]);
    assert_eq!(keys[keys.len() - 2..], ["created_at", "updated_at"]);

    let list = h.get("/api/v1/servers", "acme_owner_read").await.json();
    assert_eq!(list[0]["proxy"], json!({"redirect_enabled": true}));
    assert_eq!(list[0]["is_coolify_host"], false);
    // Other teams do not see it.
    h.call_bearer(
        "GET",
        &format!("/api/v1/servers/{uuid}"),
        h.token("carol_root"),
    )
    .await
    .assert(404, r#"{"message":"Server not found."}"#);
    h.finish().await;
}

#[tokio::test]
async fn create_validation_and_checks() {
    let h = Harness::new().await;
    let key = key_uuid(&h).await;
    h.call("POST", "/api/v1/servers", W, Some(json!({"ip": ""}))).await.assert(
        422,
        r#"{"message":"Validation failed.","errors":{"ip":["The ip field must be a string.","This field is required."],"private_key_uuid":["This field is required."]}}"#,
    );
    let r = h
        .call(
            "POST",
            "/api/v1/servers",
            W,
            Some(json!({"ip": "bad host!", "private_key_uuid": key, "port": 0, "foo": 1})),
        )
        .await;
    assert_eq!(
        r.json()["errors"],
        json!({
            "ip": ["The ip must be a valid IPv4 address, IPv6 address, or hostname."],
            "port": ["The port field must be between 1 and 65535."],
            "foo": ["This field is not allowed."]
        })
    );
    h.call(
        "POST",
        "/api/v1/servers",
        W,
        Some(json!({"ip": "1.2.3.4", "private_key_uuid": "nope"})),
    )
    .await
    .assert(404, r#"{"message":"Private key not found."}"#);
    h.call(
        "POST",
        "/api/v1/servers",
        W,
        Some(json!({"ip": "1.2.3.4", "private_key_uuid": key, "proxy_type": "apache"})),
    )
    .await
    .assert(422, r#"{"message":"Invalid proxy type."}"#);
    assert_eq!(
        h.call(
            "POST",
            "/api/v1/servers",
            W,
            Some(json!({"ip": "1.2.3.4", "private_key_uuid": key}))
        )
        .await
        .status,
        201
    );
    h.call(
        "POST",
        "/api/v1/servers",
        W,
        Some(json!({"ip": " 1.2.3.4 ", "private_key_uuid": key})),
    )
    .await
    .assert(
        400,
        r#"{"message":"A server with this IP\/Domain is already in use."}"#,
    );
    // instant_validate needs SSH: refused before anything is written.
    assert_eq!(
        h.call(
            "POST",
            "/api/v1/servers",
            W,
            Some(json!({"ip": "5.6.7.8", "private_key_uuid": key, "instant_validate": true}))
        )
        .await
        .status,
        501
    );
    assert_eq!(
        scalar_i64(&h, "SELECT count(*) FROM servers WHERE ip = '5.6.7.8'").await,
        0
    );
    h.finish().await;
}

#[tokio::test]
async fn update_and_delete() {
    let h = Harness::new().await;
    let key = key_uuid(&h).await;
    let r = h
        .call(
            "POST",
            "/api/v1/servers",
            W,
            Some(json!({"name": "box", "ip": "10.1.1.1", "private_key_uuid": key})),
        )
        .await;
    let uuid = r.json()["uuid"].as_str().unwrap().to_string();
    let path = format!("/api/v1/servers/{uuid}");
    let r = h
        .call(
            "PATCH",
            &path,
            W,
            Some(json!({"ip": "10.1.1.2", "port": "2200", "server_role": "build", "concurrent_builds": "4", "server_timezone": "Europe/Berlin", "wildcard_domain": "https://apps.example.com"})),
        )
        .await;
    r.assert(201, &format!(r#"{{"uuid":"{uuid}"}}"#));
    let show = h.get(&path, "acme_owner_read").await.json();
    assert_eq!(
        (
            show["ip"].clone(),
            show["ip_previous"].clone(),
            show["port"].clone()
        ),
        (json!("10.1.1.2"), json!("10.1.1.1"), json!(2200))
    );
    assert_eq!(show["settings"]["server_role"], "build");
    assert_eq!(show["settings"]["is_build_server"], true);
    assert_eq!(show["settings"]["concurrent_builds"], 4);
    assert_eq!(show["settings"]["server_timezone"], "Europe/Berlin");
    assert_eq!(
        show["settings"]["wildcard_domain"],
        "https://apps.example.com"
    );

    let errors = h
        .call(
            "PATCH",
            &path,
            W,
            Some(json!({"server_disk_usage_check_frequency": "61 * * * *"})),
        )
        .await
        .json();
    assert_eq!(
        errors["errors"]["server_disk_usage_check_frequency"],
        json!(["Invalid Cron / Human expression for Disk Usage Check Frequency."])
    );
    let errors = h
        .call("PATCH", &path, W, Some(json!({"server_timezone": "Mars/Base", "wildcard_domain": "nope", "connection_timeout": 301})))
        .await
        .json();
    assert_eq!(
        errors["errors"],
        json!({
            "connection_timeout": ["The connection timeout field must not be greater than 300."],
            "wildcard_domain": ["The wildcard domain field must be a valid URL."],
            "server_timezone": ["The server_timezone must be a valid timezone."]
        })
    );
    // Changing the proxy type needs the deploy ability, then SSH (not ported).
    let r = h
        .call("PATCH", &path, W, Some(json!({"proxy_type": "caddy"})))
        .await;
    assert_eq!(r.status, 403, "{}", r.body);
    assert_eq!(
        h.call(
            "PATCH",
            &path,
            Some("acme_owner_root"),
            Some(json!({"proxy_type": "caddy"}))
        )
        .await
        .status,
        501
    );

    // Delete: the row and its settings, destinations and variables go away.
    let sid = scalar_i64(&h, &format!("SELECT id FROM servers WHERE uuid = '{uuid}'")).await;
    // `api.ability:delete` since v4.6.0.
    h.call("DELETE", &path, Some("acme_owner_delete"), None)
        .await
        .assert(200, r#"{"message":"Server deleted."}"#);
    for table in [
        "servers WHERE id",
        "server_settings WHERE server_id",
        "standalone_dockers WHERE server_id",
        "shared_environment_variables WHERE server_id",
    ] {
        assert_eq!(
            scalar_i64(&h, &format!("SELECT count(*) FROM {table} = {sid}")).await,
            0,
            "{table}"
        );
    }
    h.get(&path, "acme_owner_read")
        .await
        .assert(404, r#"{"message":"Server not found."}"#);
    h.finish().await;
}
