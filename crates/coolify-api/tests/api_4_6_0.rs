//! API changes of Coolify v4.6.0 (the API area of the port): Git HTTPS credentials of
//! applications, and the smaller API fixes. Status codes, messages and key order follow the PHP.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::Harness;
use serde_json::{Value, json};

const W: Option<&str> = Some("acme_owner_write");
/// DELETE routes need the `delete` ability since v4.6.0 (`api.ability:delete`).
const D: Option<&str> = Some("acme_owner_delete");

struct Team {
    server: String,
    project: String,
}

fn uuid_of(r: &common::Resp) -> String {
    assert!(r.status.is_success(), "{}", r.body);
    r.json()["uuid"].as_str().unwrap().to_owned()
}

async fn team(h: &Harness) -> Team {
    let pem = coolify_compat::sshkey::generate(coolify_compat::sshkey::KeyKind::Ed25519).unwrap();
    let key = uuid_of(
        &h.call(
            "POST",
            "/api/v1/security/keys",
            W,
            Some(json!({"name": "k", "private_key": pem})),
        )
        .await,
    );
    let server = uuid_of(
        &h.call(
            "POST",
            "/api/v1/servers",
            W,
            Some(json!({"name": "s", "ip": "203.0.113.61", "private_key_uuid": key})),
        )
        .await,
    );
    sqlx::query(
        "UPDATE server_settings SET is_reachable = true, is_usable = true, docker_version = '29.1.3' \
         WHERE server_id = (SELECT id FROM servers WHERE uuid = $1)",
    )
    .bind(&server)
    .execute(h.tdb.db.pool())
    .await
    .unwrap();
    let project = uuid_of(
        &h.call("POST", "/api/v1/projects", W, Some(json!({"name": "Shop"})))
            .await,
    );
    Team { server, project }
}

fn public_body(t: &Team, repository: &str, extra: Value) -> Value {
    let mut body = json!({
        "server_uuid": t.server,
        "project_uuid": t.project,
        "environment_name": "production",
        "git_repository": repository,
        "git_branch": "main",
        "build_pack": "nixpacks",
        "ports_exposes": "3000",
        "instant_deploy": false,
    });
    for (k, v) in extra.as_object().unwrap() {
        body[k] = v.clone();
    }
    body
}

async fn stored(h: &Harness, uuid: &str) -> (Option<String>, Option<String>) {
    let (user, cipher): (Option<String>, Option<String>) = sqlx::query_as(
        "SELECT git_https_username, git_https_password FROM applications WHERE uuid = $1",
    )
    .bind(uuid)
    .fetch_one(h.tdb.db.pool())
    .await
    .unwrap();
    let enc = coolify_compat::Encrypter::new(common::APP_KEY).unwrap();
    (user, cipher.map(|c| enc.decrypt_string(&c).unwrap()))
}

#[tokio::test]
async fn git_https_credentials_on_create() {
    let h = Harness::new().await;
    let t = team(&h).await;

    // Only the public type takes them, checked right after the allowed fields.
    let r = h
        .call(
            "POST",
            "/api/v1/applications/dockerimage",
            W,
            Some(json!({
                "server_uuid": t.server, "project_uuid": t.project, "environment_name": "production",
                "docker_registry_image_name": "nginx", "git_https_username": "u",
            })),
        )
        .await;
    r.assert(
        422,
        r#"{"message":"Validation failed.","errors":{"git_https_password":"HTTPS credentials can only be used with a public or custom Git host repository (POST \/applications\/public)."}}"#,
    );

    // github.com uses a GitHub App.
    let r = h
        .call(
            "POST",
            "/api/v1/applications/public",
            W,
            Some(public_body(
                &t,
                "https://github.com/o/r",
                json!({"git_https_password": "tok"}),
            )),
        )
        .await;
    r.assert(
        422,
        r#"{"message":"Validation failed.","errors":{"git_https_password":"HTTPS credentials are not used for github.com. Use a GitHub App (POST \/applications\/private-github-app) for a private GitHub repository."}}"#,
    );

    // A username needs a password.
    let r = h
        .call(
            "POST",
            "/api/v1/applications/public",
            W,
            Some(public_body(
                &t,
                "https://git.example.test/o/r.git",
                json!({"git_https_username": "u"}),
            )),
        )
        .await;
    r.assert(
        422,
        r#"{"message":"Validation failed.","errors":{"git_https_password":"The git_https_password is required with git_https_username."}}"#,
    );

    // The `sharedDataApplications()` rules.
    let r = h
        .call(
            "POST",
            "/api/v1/applications/public",
            W,
            Some(public_body(
                &t,
                "https://git.example.test/o/r.git",
                json!({"git_https_username": "a:b", "git_https_password": "x\ny"}),
            )),
        )
        .await;
    r.assert(
        422,
        r#"{"message":"Validation failed.","errors":{"git_https_username":["The git https username field format is invalid."],"git_https_password":["The git https password field format is invalid."]}}"#,
    );

    // Stored encrypted; the password never comes back, the username does.
    let r = h
        .call(
            "POST",
            "/api/v1/applications/public",
            W,
            Some(public_body(
                &t,
                "https://git.example.test/o/r.git",
                json!({"git_https_username": "deploy", "git_https_password": "s3cret-token"}),
            )),
        )
        .await;
    let uuid = uuid_of(&r);
    assert_eq!(
        stored(&h, &uuid).await,
        (Some("deploy".into()), Some("s3cret-token".into()))
    );
    for token in ["acme_owner_read", "acme_owner_sensitive", "acme_owner_root"] {
        let r = h.get(&format!("/api/v1/applications/{uuid}"), token).await;
        assert_eq!(r.status, 200, "{}", r.body);
        let v = r.json();
        assert_eq!(v["git_https_username"], "deploy", "{token}");
        assert!(v.get("git_https_password").is_none(), "{token}");
        assert!(!r.body.contains("s3cret-token"), "{token}");
    }
    h.finish().await;
}

#[tokio::test]
async fn git_https_credentials_on_update() {
    let h = Harness::new().await;
    let t = team(&h).await;
    let uuid = uuid_of(
        &h.call(
            "POST",
            "/api/v1/applications/public",
            W,
            Some(public_body(
                &t,
                "https://git.example.test/o/r.git",
                json!({}),
            )),
        )
        .await,
    );
    let path = format!("/api/v1/applications/{uuid}");

    // A username alone needs a password, stored or sent.
    let r = h
        .call("PATCH", &path, W, Some(json!({"git_https_username": "u"})))
        .await;
    r.assert(
        422,
        r#"{"message":"Validation failed.","errors":{"git_https_password":"The git_https_password is required with git_https_username."}}"#,
    );
    let r = h
        .call(
            "PATCH",
            &path,
            W,
            Some(json!({"git_https_username": "u", "git_https_password": "p1"})),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(
        stored(&h, &uuid).await,
        (Some("u".into()), Some("p1".into()))
    );
    // The stored password covers a new username.
    let r = h
        .call("PATCH", &path, W, Some(json!({"git_https_username": "v"})))
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(
        stored(&h, &uuid).await,
        (Some("v".into()), Some("p1".into()))
    );
    // An empty password removes both.
    let r = h
        .call("PATCH", &path, W, Some(json!({"git_https_password": null})))
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(stored(&h, &uuid).await, (None, None));

    // Not for an application with a deploy key.
    sqlx::query(
        "UPDATE applications SET private_key_id = (SELECT id FROM private_keys ORDER BY id DESC LIMIT 1) WHERE uuid = $1",
    )
    .bind(&uuid)
    .execute(h.tdb.db.pool())
    .await
    .unwrap();
    let r = h
        .call("PATCH", &path, W, Some(json!({"git_https_password": "p"})))
        .await;
    r.assert(
        422,
        r#"{"message":"Validation failed.","errors":{"git_https_password":"HTTPS credentials can only be used by applications that clone a repository without a Git source or deploy key."}}"#,
    );
    h.finish().await;
}

/// `redactLockedApiResourceValues()`: shown-once values and the HTTPS credential never come back
/// in the generated compose, also not for `read:sensitive`, in the resource and environment lists
/// too.
#[tokio::test]
async fn locked_values_are_hidden_in_generated_compose() {
    // The locked values come from the jobs (`logRedactionSecretsFor()`), which need a remote; no
    // command runs on a server here.
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = coolify_remote::RemoteConfig::new(tmp.path().join("data"));
    cfg.ssh_binary = "/usr/bin/false".into();
    let h = Harness::with_remote(coolify_remote::Remote::new(cfg).unwrap()).await;
    let t = team(&h).await;
    let uuid = uuid_of(
        &h.call(
            "POST",
            "/api/v1/applications/public",
            W,
            Some(public_body(
                &t,
                "https://git.example.test/o/r.git",
                json!({}),
            )),
        )
        .await,
    );
    let r = h
        .call(
            "POST",
            &format!("/api/v1/applications/{uuid}/envs"),
            W,
            Some(json!({"key": "SECRET", "value": "shown-once-value", "is_shown_once": true})),
        )
        .await;
    assert_eq!(r.status, 201, "{}", r.body);
    // The v4.6.0 Git HTTPS credentials, set directly.
    let enc = coolify_compat::Encrypter::new(common::APP_KEY).unwrap();
    sqlx::query(
        "UPDATE applications SET git_https_username = 'deploy', git_https_password = $2, \
         docker_compose = 'services:\n  app:\n    environment:\n      SECRET: shown-once-value\n      TOKEN: https-token-1\n' WHERE uuid = $1",
    )
    .bind(&uuid)
    .bind(enc.encrypt_string("https-token-1"))
    .execute(h.tdb.db.pool())
    .await
    .unwrap();
    let expected =
        "services:\n  app:\n    environment:\n      SECRET: <locked>\n      TOKEN: <locked>\n";
    let r = h
        .get(
            &format!("/api/v1/applications/{uuid}"),
            "acme_owner_sensitive",
        )
        .await;
    assert_eq!(r.json()["docker_compose"], expected, "{}", r.body);
    let r = h.get("/api/v1/applications", "acme_owner_sensitive").await;
    assert_eq!(r.json()[0]["docker_compose"], expected, "{}", r.body);
    let r = h.get("/api/v1/resources", "acme_owner_sensitive").await;
    assert_eq!(r.json()[0]["docker_compose"], expected, "{}", r.body);
    let r = h
        .get(
            &format!("/api/v1/projects/{}/production", t.project),
            "acme_owner_sensitive",
        )
        .await;
    assert_eq!(
        r.json()["applications"][0]["docker_compose"],
        expected,
        "{}",
        r.body
    );
    assert!(!r.body.contains("shown-once-value") && !r.body.contains("https-token-1"));
    h.finish().await;
}

/// An integration token row of the Acme team, as `IntegrationToken::create()` stores it.
async fn insert_token(
    h: &Harness,
    uuid: &str,
    provider: &str,
    capabilities: &str,
    metadata: Option<&str>,
) -> i64 {
    let enc = coolify_compat::Encrypter::new(common::APP_KEY).unwrap();
    sqlx::query_scalar(
        "INSERT INTO integration_tokens (uuid, team_id, provider, name, token, capabilities, metadata, created_at, updated_at) \
         VALUES ($1, 1, $2, $3, $4, $5::json, $6::json, '2026-11-01 10:00:00', '2026-11-01 10:00:00') RETURNING id",
    )
    .bind(uuid)
    .bind(provider)
    .bind(format!("{provider} token"))
    .bind(enc.encrypt_string("secret-token-value"))
    .bind(capabilities)
    .bind(metadata)
    .fetch_one(h.tdb.db.pool())
    .await
    .unwrap()
}

/// `IntegrationTokensController` (v4.6.0): list, show, update, delete and the zone sync, and the
/// rules of create. No provider is contacted: every request here fails or succeeds before that.
#[tokio::test]
async fn integration_token_endpoints() {
    let h = Harness::new().await;
    let r = h
        .get("/api/v1/security/integration-tokens", "acme_owner_read")
        .await;
    r.assert(200, "[]");
    // Admins and owners only.
    let r = h
        .get("/api/v1/security/integration-tokens", "acme_member_read")
        .await;
    assert_eq!(r.status, 403, "{}", r.body);

    insert_token(
        &h,
        "vaulttoken000000000000001",
        "vault",
        r#"["secrets"]"#,
        Some(r#"{"base_url":"https://vault.example.test","namespace":"ns"}"#),
    )
    .await;
    let cf = insert_token(
        &h,
        "cftoken000000000000000001",
        "cloudflare",
        r#"["dns"]"#,
        Some(r#"{"automatic_dns":false,"zones_synced_at":"2026-11-01T10:00:00+00:00"}"#),
    )
    .await;
    sqlx::query(
        "INSERT INTO dns_provider_zones (uuid, integration_token_id, provider_zone_id, name, account_id, account_name, created_at, updated_at) \
         VALUES ('zone00000000000000000001', $1, 'z1', 'example.test', 'a1', 'Acme account', now(), now())",
    )
    .bind(cf)
    .execute(h.tdb.db.pool())
    .await
    .unwrap();

    // The secret and the provider settings never come back.
    let r = h
        .get(
            "/api/v1/security/integration-tokens/vaulttoken000000000000001",
            "acme_owner_read",
        )
        .await;
    r.assert(
        200,
        r#"{"uuid":"vaulttoken000000000000001","name":"vault token","provider":"vault","capabilities":["secrets"],"created_at":"2026-11-01T10:00:00.000000Z","updated_at":"2026-11-01T10:00:00.000000Z"}"#,
    );
    let r = h
        .get(
            "/api/v1/security/integration-tokens/cftoken000000000000000001",
            "acme_owner_read",
        )
        .await;
    r.assert(
        200,
        r#"{"uuid":"cftoken000000000000000001","name":"cloudflare token","provider":"cloudflare","capabilities":["dns"],"automatic_dns":false,"proxied_dns":false,"dns_zones_count":1,"zones_synced_at":"2026-11-01T10:00:00+00:00","created_at":"2026-11-01T10:00:00.000000Z","updated_at":"2026-11-01T10:00:00.000000Z"}"#,
    );
    let r = h
        .get(
            "/api/v1/security/integration-tokens/nope",
            "acme_owner_read",
        )
        .await;
    r.assert(404, r#"{"message":"Integration token not found."}"#);
    assert!(
        !h.get("/api/v1/security/integration-tokens", "acme_owner_read")
            .await
            .body
            .contains("secret-token-value")
    );

    // Create: the settings only for a secret manager, the DNS switches only for Cloudflare.
    let r = h
        .call(
            "POST",
            "/api/v1/security/integration-tokens",
            W,
            Some(json!({"provider": "cloudflare", "name": "cf", "token": "t", "metadata": {}, "automatic_dns": "x", "extra": 1})),
        )
        .await;
    r.assert(
        422,
        r#"{"message":"Validation failed.","errors":{"metadata":["The metadata field is prohibited."],"automatic_dns":["The automatic dns field must be true or false."],"extra":["This field is not allowed."]}}"#,
    );
    let r = h
        .call(
            "POST",
            "/api/v1/security/integration-tokens",
            W,
            Some(json!({"provider": "doppler", "name": "d", "token": "nope", "proxied_dns": true})),
        )
        .await;
    r.assert(
        422,
        r#"{"message":"Validation failed.","errors":{"token":["The token field format is invalid."],"proxied_dns":["The proxied dns field is prohibited."]}}"#,
    );

    // Update: a connection setting needs the secret again (checked before the provider).
    let path = "/api/v1/security/integration-tokens/vaulttoken000000000000001";
    let r = h
        .call(
            "PATCH",
            path,
            W,
            Some(json!({"metadata": {"base_url": "https://other.example.test"}})),
        )
        .await;
    r.assert(
        422,
        r#"{"message":"Validation failed.","errors":{"token":["Enter the token again when you change the base URL or the namespace."]}}"#,
    );
    // A rename alone is not checked with the provider.
    let r = h
        .call("PATCH", path, W, Some(json!({"name": "Renamed"})))
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.json()["name"], "Renamed");

    // Zones are Cloudflare's.
    let r = h.call("POST", &format!("{path}/sync-zones"), W, None).await;
    r.assert(
        422,
        r#"{"message":"Only Cloudflare tokens have DNS zones."}"#,
    );

    // The DNS zones of the team.
    let r = h.get("/api/v1/dns/zones", "acme_owner_read").await;
    r.assert(
        200,
        r#"[{"zone":"example.test","account_name":"Acme account","integration_token_uuid":"cftoken000000000000000001","integration_token_name":"cloudflare token","managed_records_count":0}]"#,
    );

    // Delete.
    let r = h.call("DELETE", path, D, None).await;
    r.assert(200, r#"{"message":"Integration token deleted."}"#);
    let r = h.call("DELETE", path, D, None).await;
    r.assert(404, r#"{"message":"Integration token not found."}"#);
    h.finish().await;
}

/// `SecretManagerController` (v4.6.0) on applications and databases, without fetching secrets.
#[tokio::test]
async fn secret_manager_endpoints() {
    let h = Harness::new().await;
    let t = team(&h).await;
    let uuid = uuid_of(
        &h.call(
            "POST",
            "/api/v1/applications/public",
            W,
            Some(public_body(
                &t,
                "https://git.example.test/o/r.git",
                json!({}),
            )),
        )
        .await,
    );
    let token = insert_token(
        &h,
        "vaulttoken000000000000002",
        "vault",
        r#"["secrets"]"#,
        Some(r#"{"base_url":"https://vault.example.test"}"#),
    )
    .await;
    let path = format!("/api/v1/applications/{uuid}/secret-manager");

    let r = h.get(&path, "acme_owner_read").await;
    r.assert(
        200,
        r#"{"linked":false,"integration_token_uuid":null,"provider":null,"settings":null,"available_tokens":[{"uuid":"vaulttoken000000000000002","name":"vault token","provider":"vault"}]}"#,
    );
    let r = h.call("DELETE", &path, D, None).await;
    r.assert(
        404,
        r#"{"message":"The resource has no secret manager source."}"#,
    );
    let r = h.get(&format!("{path}/keys"), "acme_owner_write").await;
    r.assert(
        404,
        r#"{"message":"The resource has no secret manager source."}"#,
    );

    let r = h
        .call(
            "PATCH",
            &path,
            W,
            Some(json!({"integration_token_uuid": "nope"})),
        )
        .await;
    r.assert(
        404,
        r#"{"message":"Secret manager integration token not found."}"#,
    );
    let r = h
        .call(
            "PATCH",
            &path,
            W,
            Some(json!({"integration_token_uuid": "vaulttoken000000000000002", "settings": {"mount": "kv"}, "x": 1})),
        )
        .await;
    r.assert(
        422,
        r#"{"message":"Validation failed.","errors":{"settings.path":["This field is required."],"x":["This field is not allowed."]}}"#,
    );
    let r = h
        .call(
            "PATCH",
            &path,
            W,
            Some(json!({"integration_token_uuid": "vaulttoken000000000000002", "settings": {"mount": "kv", "path": "app", "other": "dropped"}})),
        )
        .await;
    r.assert(
        200,
        r#"{"integration_token_uuid":"vaulttoken000000000000002","provider":"vault","settings":{"mount":"kv","path":"app"}}"#,
    );
    let linked: i64 = sqlx::query_scalar(
        "SELECT integration_token_id FROM secret_manager_links WHERE resourceable_type = 'App\\Models\\Application'",
    )
    .fetch_one(h.tdb.db.pool())
    .await
    .unwrap();
    assert_eq!(linked, token);
    let r = h.get(&path, "acme_owner_read").await;
    assert_eq!(r.json()["linked"], true, "{}", r.body);
    assert_eq!(r.json()["settings"], json!({"mount": "kv", "path": "app"}));
    // A used token cannot be deleted.
    let r = h
        .call(
            "DELETE",
            "/api/v1/security/integration-tokens/vaulttoken000000000000002",
            D,
            None,
        )
        .await;
    r.assert(
        409,
        r#"{"message":"This token is used by one or more resources as a secret manager source. Remove those links first."}"#,
    );
    let r = h.call("DELETE", &path, D, None).await;
    r.assert(
        200,
        r#"{"message":"Secret manager source removed. Existing {{vault.*}} variables fail the next deployment until they are removed."}"#,
    );

    let r = h
        .get("/api/v1/databases/nope/secret-manager", "acme_owner_read")
        .await;
    r.assert(404, r#"{"message":"Database not found."}"#);
    let r = h
        .get("/api/v1/services/nope/secret-manager", "acme_owner_read")
        .await;
    r.assert(404, r#"{"message":"Service not found."}"#);
    // The old application-only route is gone with its controller; the generic one answers.
    let r = h
        .call(
            "PATCH",
            "/api/v1/applications/nope/secret-manager",
            W,
            Some(json!({})),
        )
        .await;
    r.assert(404, r#"{"message":"Application not found."}"#);
    h.finish().await;
}

/// `DnsController` (v4.6.0) for an application, without a Cloudflare call.
#[tokio::test]
async fn application_dns_endpoints() {
    let h = Harness::new().await;
    let t = team(&h).await;
    let uuid = uuid_of(
        &h.call(
            "POST",
            "/api/v1/applications/public",
            W,
            Some(public_body(
                &t,
                "https://git.example.test/o/r.git",
                json!({}),
            )),
        )
        .await,
    );
    sqlx::query("UPDATE applications SET fqdn = 'https://App.example.test' WHERE uuid = $1")
        .bind(&uuid)
        .execute(h.tdb.db.pool())
        .await
        .unwrap();
    let cf = insert_token(
        &h,
        "cftoken000000000000000002",
        "cloudflare",
        r#"["dns"]"#,
        None,
    )
    .await;
    sqlx::query(
        "INSERT INTO dns_provider_zones (uuid, integration_token_id, provider_zone_id, name, created_at, updated_at) \
         VALUES ('zone00000000000000000002', $1, 'z2', 'example.test', now(), now())",
    )
    .bind(cf)
    .execute(h.tdb.db.pool())
    .await
    .unwrap();
    let path = format!("/api/v1/applications/{uuid}/dns");

    // 203.0.113.0/24 is a documentation range: the server has no public address.
    let r = h.get(&path, "acme_owner_read").await;
    r.assert(
        200,
        r#"{"default_address":null,"public_addresses":[],"hostnames":[{"hostname":"app.example.test","providers":[{"integration_token_uuid":"cftoken000000000000000002","integration_token_name":"cloudflare token","zone":"example.test","automatic_dns":true,"proxied_dns":false}],"records":[]}]}"#,
    );

    let r = h
        .call(
            "POST",
            &path,
            W,
            Some(json!({"hostname": "other.example.test"})),
        )
        .await;
    r.assert(
        422,
        r#"{"message":"Validation failed.","errors":{"hostname":["other.example.test is not a domain of this resource."]}}"#,
    );
    let r = h
        .call(
            "POST",
            &path,
            W,
            Some(json!({"hostname": "APP.example.test."})),
        )
        .await;
    r.assert(
        422,
        r#"{"message":"Validation failed.","errors":{"address":["The server has no public IP address; add the DNS record manually."]}}"#,
    );
    let r = h.call("POST", &path, W, Some(json!({"name": "x"}))).await;
    r.assert(
        422,
        r#"{"message":"Validation failed.","errors":{"hostname":["This field is required."],"name":["This field is not allowed."]}}"#,
    );
    let r = h
        .call(
            "POST",
            &format!("{path}/replace"),
            W,
            Some(json!({"hostname": "app.example.test"})),
        )
        .await;
    r.assert(
        422,
        r#"{"message":"Validation failed.","errors":{"record_id":["This field is required."],"current":["This field is required."]}}"#,
    );

    // Release: only a hostname the resource no longer uses, and only with a record of it.
    let r = h
        .call("DELETE", &format!("{path}/app.example.test"), D, None)
        .await;
    r.assert(
        422,
        r#"{"message":"Validation failed.","errors":{"hostname":["The resource still uses app.example.test. Remove the domain first; its DNS record is then released."]}}"#,
    );
    let r = h
        .call("DELETE", &format!("{path}/gone.example.test"), D, None)
        .await;
    r.assert(
        404,
        r#"{"message":"No managed DNS record of this resource for gone.example.test."}"#,
    );

    let r = h.get("/api/v1/services/nope/dns", "acme_owner_read").await;
    r.assert(404, r#"{"message":"Service not found."}"#);
    h.finish().await;
}

/// `fix(api): accept the same port mappings as the UI` (v4.6.0).
#[tokio::test]
async fn port_mappings_like_the_ui() {
    let h = Harness::new().await;
    let t = team(&h).await;
    let ok_body = public_body(
        &t,
        "https://git.example.test/o/r.git",
        json!({"ports_mappings": "8080:80/udp,127.0.0.1:8080:80,:3000,[::1]::81"}),
    );
    let r = h
        .call("POST", "/api/v1/applications/public", W, Some(ok_body))
        .await;
    assert_eq!(r.status, 201, "{}", r.body);
    let r = h
        .call(
            "POST",
            "/api/v1/applications/public",
            W,
            Some(public_body(
                &t,
                "https://git.example.test/o/r.git",
                json!({"ports_mappings": "8080:80,8080:81"}),
            )),
        )
        .await;
    r.assert(
        422,
        r#"{"message":"Validation failed.","errors":{"ports_mappings":"Each host IP, port, and protocol can only be used once."}}"#,
    );
    let r = h
        .call(
            "POST",
            "/api/v1/applications/public",
            W,
            Some(public_body(
                &t,
                "https://git.example.test/o/r.git",
                json!({"ports_mappings": "abc"}),
            )),
        )
        .await;
    r.assert(
        422,
        r#"{"message":"Validation failed.","errors":{"ports_mappings":["The ports mappings field format is invalid."]}}"#,
    );
    h.finish().await;
}

/// `fix(application): reject Docker image names shared by building applications` (v4.6.0).
#[tokio::test]
async fn shared_docker_image_names_are_rejected() {
    let h = Harness::new().await;
    let t = team(&h).await;
    let body = |extra: Value| public_body(&t, "https://git.example.test/o/r.git", extra);
    let first = uuid_of(
        &h.call(
            "POST",
            "/api/v1/applications/public",
            W,
            Some(body(
                json!({"name": "first", "docker_registry_image_name": "registry.example.test/app"}),
            )),
        )
        .await,
    );
    let message = "The application first also builds the Docker image registry.example.test/app. Both push the same image tags, so one application can run the image of the other. Set a different Docker image name for each application in the General settings.";
    let expected = json!({"message": "Validation failed.", "errors": {"docker_registry_image_name": [message]}});
    let r = h
        .call(
            "POST",
            "/api/v1/applications/public",
            W,
            Some(body(
                json!({"docker_registry_image_name": "registry.example.test/app"}),
            )),
        )
        .await;
    assert_eq!(r.status, 422, "{}", r.body);
    assert_eq!(r.json(), expected);
    // Update: only when the name or the build pack changes, and never against itself.
    let second = uuid_of(
        &h.call(
            "POST",
            "/api/v1/applications/public",
            W,
            Some(body(json!({"name": "second"}))),
        )
        .await,
    );
    let r = h
        .call(
            "PATCH",
            &format!("/api/v1/applications/{second}"),
            W,
            Some(json!({"docker_registry_image_name": "registry.example.test/app"})),
        )
        .await;
    assert_eq!(r.status, 422, "{}", r.body);
    assert_eq!(r.json(), expected);
    let r = h
        .call(
            "PATCH",
            &format!("/api/v1/applications/{first}"),
            W,
            Some(json!({"docker_registry_image_name": "registry.example.test/app", "description": "same"})),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    h.finish().await;
}

/// `fix(avatar): change avatar URL when an existing avatar is replaced` (v4.6.0): storing the same
/// path again still moves `updated_at`, which the avatar URL carries; a removal saves only a change.
#[tokio::test]
async fn replaced_avatar_touches_updated_at() {
    let h = Harness::new().await;
    let at = |s: &str| chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S").unwrap();
    let updated = |table: &'static str| {
        let pool = h.tdb.db.pool().clone();
        async move {
            sqlx::query_scalar::<_, chrono::NaiveDateTime>(&format!(
                "SELECT updated_at FROM {table} WHERE id = 1"
            ))
            .fetch_one(&pool)
            .await
            .unwrap()
        }
    };
    let db = &h.tdb.db;
    coolify_db::profile::set_avatar(db, 1, Some("avatars/1.png"), at("2026-11-01 10:00:00"))
        .await
        .unwrap();
    coolify_db::profile::set_avatar(db, 1, Some("avatars/1.png"), at("2026-11-02 10:00:00"))
        .await
        .unwrap();
    assert_eq!(updated("users").await, at("2026-11-02 10:00:00"));
    coolify_db::profile::set_avatar(db, 1, None, at("2026-11-03 10:00:00"))
        .await
        .unwrap();
    coolify_db::profile::set_avatar(db, 1, None, at("2026-11-04 10:00:00"))
        .await
        .unwrap();
    assert_eq!(updated("users").await, at("2026-11-03 10:00:00"));

    coolify_db::team_pages::set_team_avatar(db, 1, Some("teams/1.png"), at("2026-11-01 10:00:00"))
        .await
        .unwrap();
    coolify_db::team_pages::set_team_avatar(db, 1, Some("teams/1.png"), at("2026-11-05 10:00:00"))
        .await
        .unwrap();
    assert_eq!(updated("teams").await, at("2026-11-05 10:00:00"));
    h.finish().await;
}

/// `Settings\Updates` (v4.6.0): the update channel, the custom image tag and the channel an instance
/// keeps following after a switch to a more stable one.
#[tokio::test]
async fn update_channel_settings() {
    let h = Harness::new().await;
    let (cookie, csrf) = h.ui_login("root@example.test").await;
    let headers = [("cookie", cookie.as_str()), ("x-csrf-token", csrf.as_str())];
    let values = |channel: &str, tag: Value| {
        json!({
            "update_check_frequency": "0 * * * *",
            "auto_update_frequency": "0 0 * * *",
            "is_auto_update_enabled": false,
            "docker_registry_url": "docker.io",
            "update_channel": channel,
            "update_custom_tag": tag,
        })
    };
    let post = |body: Value| h.request("POST", "/ui/settings/updates", &headers, Some(body));
    let stored = || async {
        sqlx::query_as::<_, (String, Option<String>, Option<String>)>(
            "SELECT update_channel, update_custom_tag, previous_update_channel FROM instance_settings WHERE id = 0",
        )
        .fetch_one(h.tdb.db.pool())
        .await
        .unwrap()
    };

    let r = post(json!({"action": "submit", "values": values("weekly", json!(null))})).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(
        r.json()["errors"],
        json!({"update_channel": ["The selected update channel is invalid."]})
    );
    let r = post(json!({"action": "instant", "values": values("custom", json!(null))})).await;
    assert_eq!(
        r.json()["errors"],
        json!({"update_custom_tag": ["The update custom tag field is required when update channel is custom."]})
    );
    let r = post(json!({"action": "instant", "values": values("custom", json!("-bad"))})).await;
    assert_eq!(
        r.json()["errors"],
        json!({"update_custom_tag": ["The update custom tag field format is invalid."]})
    );

    let r = post(json!({"action": "instant", "values": values("nightly", json!(null))})).await;
    assert_eq!(
        r.json()["settings"]["update_channel"],
        "nightly",
        "{}",
        r.body
    );
    assert_eq!(stored().await, ("nightly".into(), None, None));
    // Back to stable: the nightlies of this version stay until its stable release.
    post(json!({"action": "instant", "values": values("stable", json!(null))})).await;
    assert_eq!(
        stored().await,
        ("stable".into(), None, Some("nightly".into()))
    );
    post(json!({"action": "instant", "values": values("custom", json!("pr-1234"))})).await;
    assert_eq!(
        stored().await,
        ("custom".into(), Some("pr-1234".into()), None)
    );
    h.finish().await;
}

/// A token of Bob (user 2, a member of Acme) with these abilities and scopes; the bearer value.
async fn member_token(h: &Harness, abilities: &str, scopes: Option<&str>) -> String {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let plain = format!(
        "grant-4-6-0-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    );
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO personal_access_tokens (tokenable_type, tokenable_id, name, token, team_id, abilities, scopes, created_at, updated_at) \
         VALUES ('App\\Models\\User', 2, 'grant test', $1, '1', $2, $3::json, now(), now()) RETURNING id",
    )
    .bind(coolify_compat::hash_token(&plain))
    .bind(abilities)
    .bind(scopes)
    .fetch_one(h.tdb.db.pool())
    .await
    .unwrap();
    format!("{id}|{plain}")
}

async fn as_bearer(
    h: &Harness,
    method: &str,
    path: &str,
    bearer: &str,
    body: Option<Value>,
) -> common::Resp {
    h.request(
        method,
        path,
        &[("authorization", &format!("Bearer {bearer}"))],
        body,
    )
    .await
}

async fn set_member_access(h: &Harness, project: &str, levels: &str) {
    sqlx::query(&format!(
        r#"UPDATE team_user SET access = '[{{"scope":"project","uuid":"{project}","levels":{levels}}}]' WHERE team_id = 1 AND user_id = 2"#
    ))
    .execute(h.tdb.db.pool())
    .await
    .unwrap();
}

/// The v4.6.0 DNS, secret manager, integration token and zone routes under access grants:
/// `findResource()` hides resources without a grant (404), `authorize('view' | 'update')` is
/// `canAccess(Read | Write)`, and the team-level routes refuse a member limited to some projects.
#[tokio::test]
async fn new_routes_follow_access_grants() {
    const SCOPED: &str = r#"{"message":"This API token is limited to selected projects, environments, or resources, so it cannot use this endpoint."}"#;
    let h = Harness::new().await;
    let t = team(&h).await;
    let shop_app = uuid_of(
        &h.call(
            "POST",
            "/api/v1/applications/public",
            W,
            Some(public_body(
                &t,
                "https://git.example.test/o/r.git",
                json!({}),
            )),
        )
        .await,
    );
    let other = Team {
        server: t.server.clone(),
        project: uuid_of(
            &h.call(
                "POST",
                "/api/v1/projects",
                W,
                Some(json!({"name": "Other"})),
            )
            .await,
        ),
    };
    let other_app = uuid_of(
        &h.call(
            "POST",
            "/api/v1/applications/public",
            W,
            Some(public_body(
                &other,
                "https://git.example.test/o/s.git",
                json!({}),
            )),
        )
        .await,
    );
    let scope = format!(r#"[{{"scope":"project","uuid":"{}"}}]"#, t.project);

    // Bob may read the Shop project only.
    set_member_access(&h, &t.project, r#"["read"]"#).await;
    let read = member_token(&h, r#"["read"]"#, Some(&scope)).await;
    let r = as_bearer(
        &h,
        "GET",
        &format!("/api/v1/applications/{shop_app}/dns"),
        &read,
        None,
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    let r = as_bearer(
        &h,
        "GET",
        &format!("/api/v1/applications/{shop_app}/secret-manager"),
        &read,
        None,
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    // Resources of other projects are hidden.
    as_bearer(
        &h,
        "GET",
        &format!("/api/v1/applications/{other_app}/dns"),
        &read,
        None,
    )
    .await
    .assert(404, r#"{"message":"Application not found."}"#);
    as_bearer(
        &h,
        "GET",
        &format!("/api/v1/applications/{other_app}/secret-manager"),
        &read,
        None,
    )
    .await
    .assert(404, r#"{"message":"Application not found."}"#);
    // Writes need the write ability; the token cannot hold more than Bob.
    as_bearer(
        &h,
        "POST",
        &format!("/api/v1/applications/{shop_app}/dns"),
        &read,
        Some(json!({"name": "x"})),
    )
    .await
    .assert(403, r#"{"message":"Missing required permissions: write"}"#);
    // Team-level routes need access to the whole team.
    as_bearer(&h, "GET", "/api/v1/dns/zones", &read, None)
        .await
        .assert(403, SCOPED);
    as_bearer(
        &h,
        "GET",
        "/api/v1/security/integration-tokens",
        &read,
        None,
    )
    .await
    .assert(403, SCOPED);

    // With write on the Shop project, Bob passes `authorize('update')` and reaches the validation.
    set_member_access(&h, &t.project, r#"["write"]"#).await;
    let write = member_token(&h, r#"["read","write"]"#, Some(&scope)).await;
    let r = as_bearer(
        &h,
        "POST",
        &format!("/api/v1/applications/{shop_app}/dns"),
        &write,
        Some(json!({"name": "x"})),
    )
    .await;
    assert_eq!(r.status, 422, "{}", r.body);
    let r = as_bearer(
        &h,
        "PATCH",
        &format!("/api/v1/applications/{shop_app}/secret-manager"),
        &write,
        Some(json!({"settings": "x"})),
    )
    .await;
    // No token of the team is linked yet: the request got past `authorize('update')`.
    r.assert(
        404,
        r#"{"message":"Secret manager integration token not found."}"#,
    );
    as_bearer(
        &h,
        "POST",
        &format!("/api/v1/applications/{other_app}/dns"),
        &write,
        Some(json!({"name": "x"})),
    )
    .await
    .assert(404, r#"{"message":"Application not found."}"#);

    // A member without stored grants reads the whole team, but is no admin: no integration tokens.
    sqlx::query("UPDATE team_user SET access = NULL WHERE team_id = 1 AND user_id = 2")
        .execute(h.tdb.db.pool())
        .await
        .unwrap();
    let team_read = member_token(&h, r#"["read"]"#, None).await;
    let r = as_bearer(&h, "GET", "/api/v1/dns/zones", &team_read, None).await;
    assert_eq!(r.status, 403, "{}", r.body);
    let r = as_bearer(
        &h,
        "GET",
        "/api/v1/security/integration-tokens",
        &team_read,
        None,
    )
    .await;
    assert_eq!(r.status, 403, "{}", r.body);
    h.finish().await;
}

/// `otherApplicationBuildingSameImage()` runs under `VisibleThroughAccessGrants`: a member limited
/// to one project does not learn the name of an application in another one.
#[tokio::test]
async fn shared_image_check_skips_hidden_applications() {
    let h = Harness::new().await;
    let t = team(&h).await;
    let shop_app = uuid_of(
        &h.call(
            "POST",
            "/api/v1/applications/public",
            W,
            Some(public_body(
                &t,
                "https://git.example.test/o/r.git",
                json!({}),
            )),
        )
        .await,
    );
    let other = Team {
        server: t.server.clone(),
        project: uuid_of(
            &h.call(
                "POST",
                "/api/v1/projects",
                W,
                Some(json!({"name": "Other"})),
            )
            .await,
        ),
    };
    let other_app = uuid_of(
        &h.call(
            "POST",
            "/api/v1/applications/public",
            W,
            Some(public_body(
                &other,
                "https://git.example.test/o/s.git",
                json!({}),
            )),
        )
        .await,
    );
    sqlx::query(
        "UPDATE applications SET docker_registry_image_name = 'registry.example.test/shared' WHERE uuid = $1",
    )
    .bind(&other_app)
    .execute(h.tdb.db.pool())
    .await
    .unwrap();
    let body = json!({"docker_registry_image_name": "registry.example.test/shared"});
    let path = format!("/api/v1/applications/{shop_app}");
    // The owner sees both applications: the shared name is rejected.
    let r = h.call("PATCH", &path, W, Some(body.clone())).await;
    assert_eq!(r.status, 422, "{}", r.body);
    // Bob, with write on the Shop project only, does not see the other application.
    set_member_access(&h, &t.project, r#"["write"]"#).await;
    let scope = format!(r#"[{{"scope":"project","uuid":"{}"}}]"#, t.project);
    let write = member_token(&h, r#"["read","write"]"#, Some(&scope)).await;
    let r = as_bearer(&h, "PATCH", &path, &write, Some(body)).await;
    assert!(r.status.is_success(), "{}", r.body);
    h.finish().await;
}
