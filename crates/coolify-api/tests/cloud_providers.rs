//! Cloud provider API: how provider errors, `instant_validate` and Hetzner's server type filter
//! answer (`HetznerController`, `CloudProviderApiException`), against a local fake of the
//! Hetzner API. Nothing here contacts a provider.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{APP_KEY, Harness};
use coolify_compat::Encrypter;
use serde_json::json;

/// A Hetzner stand-in: `/server_types` answers a list, every other path a 403.
async fn fake_hetzner() -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let mut buf = vec![0u8; 8192];
            let n = sock.read(&mut buf).await.unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let (status, body) = if req.starts_with("GET /hetzner/v1/server_types") {
                (
                    "200 OK",
                    json!({
                        "server_types": [
                            {"name": "cx22", "locations": [
                                {"name": "nbg1", "available": true},
                                {"name": "fsn1", "available": false}
                            ]},
                            {"name": "cx32", "locations": [{"name": "fsn1"}]},
                            {"name": "old", "deprecated": true, "locations": [{"name": "nbg1"}]}
                        ],
                        "meta": {"pagination": {"next_page": null}}
                    })
                    .to_string(),
                )
            } else {
                (
                    "403 Forbidden",
                    json!({"error": {"code": "forbidden", "message": "insufficient permissions"}})
                        .to_string(),
                )
            };
            let resp = format!(
                "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = sock.write_all(resp.as_bytes()).await;
        }
    });
    format!("http://{addr}")
}

async fn harness() -> Harness {
    let enc = Encrypter::new(APP_KEY).unwrap();
    let token = enc.encrypt_string("hetzner-test-token");
    Harness::with_extra_sql(&format!(
        "INSERT INTO cloud_provider_tokens (id, team_id, provider, token, name, uuid, created_at, updated_at)
         VALUES (1, 1, 'hetzner', '{token}', 'Fake', 'hetznertoken1', '2026-01-02 00:00:00', '2026-01-02 00:00:00');"
    ))
    .await
}

#[tokio::test]
async fn hetzner_errors_instant_validation_and_server_types() {
    coolify_jobs::cloud::set_api_base(&fake_hetzner().await);
    let h = harness().await;

    // A rejected provider request is a 502 with a message built from the status and code.
    h.call(
        "GET",
        "/api/v1/hetzner/locations?cloud_provider_token_uuid=hetznertoken1",
        Some("acme_owner_root"),
        None,
    )
    .await
    .assert(
        502,
        r#"{"message":"Failed to fetch Hetzner locations. The Hetzner API token does not have permission for this request. Use a token with read and write access.","provider_status":403,"provider_error_code":"forbidden"}"#,
    );

    // Server types keep only the locations where they can be ordered.
    let r = h
        .call(
            "GET",
            "/api/v1/hetzner/server-types?cloud_provider_token_uuid=hetznertoken1",
            Some("acme_owner_root"),
            None,
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(
        r.json(),
        json!([
            {"name": "cx22", "locations": [{"name": "nbg1", "available": true}]},
            {"name": "cx32", "locations": [{"name": "fsn1"}]}
        ])
    );
    let r = h
        .call(
            "GET",
            "/api/v1/hetzner/server-types?cloud_provider_token_uuid=hetznertoken1&location=fsn1",
            Some("acme_owner_root"),
            None,
        )
        .await;
    assert_eq!(
        r.json(),
        json!([{"name": "cx32", "locations": [{"name": "fsn1"}]}])
    );

    let pem = coolify_compat::sshkey::generate(coolify_compat::sshkey::KeyKind::Ed25519).unwrap();
    let r = h
        .call(
            "POST",
            "/api/v1/security/keys",
            Some("acme_owner_write"),
            Some(json!({"name": "srv key", "private_key": pem})),
        )
        .await;
    let key = r.json()["uuid"].as_str().unwrap().to_string();
    let body = |extra: serde_json::Value| {
        let mut b = json!({
            "cloud_provider_token_uuid": "hetznertoken1",
            "location": "nbg1",
            "server_type": "cx22",
            "image": 1,
            "private_key_uuid": key,
        });
        b.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        b
    };

    // Instant validation installs Docker, so it needs the deploy permission.
    h.call(
        "POST",
        "/api/v1/servers/hetzner",
        Some("acme_owner_write"),
        Some(body(json!({"instant_validate": true}))),
    )
    .await
    .assert(
        403,
        r#"{"message":"Missing required permissions: deploy. Instant validation installs Docker and starts the proxy on the new server, so it needs a token with the deploy permission."}"#,
    );
    // Without the flag a write token does not validate, and the create reaches the provider.
    h.call(
        "POST",
        "/api/v1/servers/hetzner",
        Some("acme_owner_write"),
        Some(body(json!({}))),
    )
    .await
    .assert(
        502,
        r#"{"message":"Failed to create Hetzner server. The Hetzner API token does not have permission for this request. Use a token with read and write access.","provider_status":403,"provider_error_code":"forbidden"}"#,
    );
    h.finish().await;
}
