//! Backup storage types of the S3 storages API (Coolify v4.7.0): SFTP, FTPS, and WebDAV storages
//! next to S3. The cases follow upstream's `tests/Feature` for `S3StoragesController`; the hosts
//! are public IP addresses, so no test resolves a name or opens a connection.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::Harness;
use serde_json::{Value, json};

const W: Option<&str> = Some("acme_owner_write");

fn errors(body: &Value, field: &str) -> Vec<String> {
    body["errors"][field]
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|m| m.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

async fn create(h: &Harness, body: Value) -> common::Resp {
    h.call("POST", "/api/v1/s3-storages", W, Some(body)).await
}

#[tokio::test]
async fn an_sftp_storage_has_no_bucket_and_hides_its_login() {
    let h = Harness::new().await;
    let r = create(
        &h,
        json!({
            "type": "sftp",
            "name": "Offsite SFTP",
            "endpoint": "sftp://8.8.8.8:2222/srv/backups",
            "key": "backup",
            "secret": "password",
        }),
    )
    .await;
    assert_eq!(r.status, 201, "{}", r.body);
    let uuid = r.json()["uuid"].as_str().unwrap().to_owned();

    let shown = h
        .get(&format!("/api/v1/s3-storages/{uuid}"), "acme_owner_read")
        .await
        .json();
    assert_eq!(shown["type"], "sftp");
    assert_eq!(shown["endpoint"], "sftp://8.8.8.8:2222/srv/backups");
    assert_eq!(shown["bucket"], Value::Null);
    assert_eq!(shown["region"], "us-east-1");
    assert_eq!(shown["use_path_style_endpoint"], true);
    assert_eq!(shown["skip_tls_verification"], false);
    assert_eq!(shown["sftp_host_key"], Value::Null);
    assert_eq!(shown["is_usable"], false);
    let object = shown.as_object().unwrap();
    // An SFTP storage shows the UUID of its private key, never the ID.
    assert_eq!(object.get("private_key_uuid"), Some(&Value::Null));
    for hidden in ["id", "private_key_id", "key", "secret"] {
        assert!(!object.contains_key(hidden), "{hidden} is shown");
    }

    let sensitive = h
        .get(
            &format!("/api/v1/s3-storages/{uuid}"),
            "acme_owner_sensitive",
        )
        .await
        .json();
    assert_eq!(sensitive["key"], "backup");
    assert_eq!(sensitive["secret"], "password");

    // Only the fields of the storage type are accepted.
    let r = h
        .call(
            "PATCH",
            &format!("/api/v1/s3-storages/{uuid}"),
            W,
            Some(json!({"bucket": "my-bucket", "type": "s3", "skip_tls_verification": true})),
        )
        .await;
    assert_eq!(r.status, 422, "{}", r.body);
    let body = r.json();
    for field in ["bucket", "type", "skip_tls_verification"] {
        assert_eq!(
            errors(&body, field),
            ["This field is not allowed."],
            "{field}"
        );
    }

    // An empty password keeps the saved one; another host is another server.
    let r = h
        .call(
            "PATCH",
            &format!("/api/v1/s3-storages/{uuid}"),
            W,
            Some(json!({"endpoint": "sftp://8.8.4.4/srv/backups", "secret": ""})),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    let sensitive = h
        .get(
            &format!("/api/v1/s3-storages/{uuid}"),
            "acme_owner_sensitive",
        )
        .await
        .json();
    assert_eq!(sensitive["endpoint"], "sftp://8.8.4.4/srv/backups");
    assert_eq!(sensitive["secret"], "password");

    // Without a private key and without a saved password there is no login left.
    let keyless = create(
        &h,
        json!({
            "type": "sftp",
            "name": "No login",
            "endpoint": "sftp://8.8.8.8",
            "key": "backup",
        }),
    )
    .await;
    assert_eq!(keyless.status, 422, "{}", keyless.body);
    assert_eq!(
        errors(&keyless.json(), "secret"),
        ["The secret field is required when private key uuid is not present."]
    );
    h.finish().await;
}

#[tokio::test]
async fn each_type_validates_its_own_endpoint_and_fields() {
    let h = Harness::new().await;

    let unknown = create(
        &h,
        json!({"type": "ftp", "name": "Plain FTP", "endpoint": "https://8.8.8.8", "key": "k", "secret": "s", "bucket": "bucket", "region": "r"}),
    )
    .await;
    assert_eq!(unknown.status, 422, "{}", unknown.body);
    assert_eq!(
        errors(&unknown.json(), "type"),
        ["The selected type is invalid."]
    );

    // The scheme follows the type.
    let wrong_scheme = create(
        &h,
        json!({"type": "ftps", "name": "FTPS", "endpoint": "https://8.8.8.8", "key": "bob", "secret": "pw"}),
    )
    .await;
    assert_eq!(wrong_scheme.status, 422, "{}", wrong_scheme.body);
    assert_eq!(
        errors(&wrong_scheme.json(), "endpoint"),
        ["The endpoint must use the ftps scheme."]
    );

    // Team members see the endpoint, so it must not hold credentials.
    let credentials = create(
        &h,
        json!({"type": "webdav", "name": "WebDAV", "endpoint": "https://bob:pw@8.8.8.8/dav", "key": "bob", "secret": "pw"}),
    )
    .await;
    assert_eq!(credentials.status, 422, "{}", credentials.body);
    assert_eq!(
        errors(&credentials.json(), "endpoint"),
        [
            "The endpoint must not contain a username or password. Enter the credentials in their own fields."
        ]
    );

    // rclone reads the user from a config file line.
    let user = create(
        &h,
        json!({"type": "ftps", "name": "FTPS", "endpoint": "ftps://8.8.8.8", "key": "bob\nhost = evil", "secret": "pw"}),
    )
    .await;
    assert_eq!(user.status, 422, "{}", user.body);
    assert_eq!(
        errors(&user.json(), "key"),
        ["The key field format is invalid."]
    );

    // S3 fields are not allowed for the other types, and S3 stays the default type.
    let s3_fields = create(
        &h,
        json!({"type": "webdav", "name": "WebDAV", "endpoint": "https://8.8.8.8/dav", "key": "bob", "secret": "pw", "bucket": "bucket", "region": "eu", "use_path_style_endpoint": false}),
    )
    .await;
    assert_eq!(s3_fields.status, 422, "{}", s3_fields.body);
    let body = s3_fields.json();
    for field in ["bucket", "region", "use_path_style_endpoint"] {
        assert_eq!(
            errors(&body, field),
            ["This field is not allowed."],
            "{field}"
        );
    }

    let s3 = create(
        &h,
        json!({"name": "Plain S3", "endpoint": "https://8.8.8.8", "bucket": "my-bucket", "region": "eu-central-1", "key": "k", "secret": "s", "skip_tls_verification": true}),
    )
    .await;
    assert_eq!(s3.status, 201, "{}", s3.body);
    let uuid = s3.json()["uuid"].as_str().unwrap().to_owned();
    let shown = h
        .get(&format!("/api/v1/s3-storages/{uuid}"), "acme_owner_read")
        .await
        .json();
    assert_eq!(shown["type"], "s3");
    assert_eq!(shown["bucket"], "my-bucket");
    assert_eq!(shown["skip_tls_verification"], true);
    // Only an SFTP storage has a private key.
    assert!(shown.get("private_key_uuid").is_none());

    let webdav = create(
        &h,
        json!({"type": "webdav", "name": "WebDAV", "endpoint": "https://8.8.8.8/remote.php/dav/files/bob/backups", "key": "bob", "secret": "pw"}),
    )
    .await;
    assert_eq!(webdav.status, 201, "{}", webdav.body);
    h.finish().await;
}
