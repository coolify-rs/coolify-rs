//! API changes of Coolify v4.6.0 (the API area of the port): Git HTTPS credentials of
//! applications, and the smaller API fixes. Status codes, messages and key order follow the PHP.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::Harness;
use serde_json::{Value, json};

const W: Option<&str> = Some("acme_owner_write");

/// The columns of `add_git_https_credentials_to_applications_table` (v4.6.0), which the
/// v4.5.3 baseline of the tests does not have.
const GIT_HTTPS_COLUMNS: &str = "ALTER TABLE applications ADD COLUMN git_https_username varchar(255), \
     ADD COLUMN git_https_password text;";

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
    let h = Harness::with_extra_sql(GIT_HTTPS_COLUMNS).await;
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
    let h = Harness::with_extra_sql(GIT_HTTPS_COLUMNS).await;
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
    let h = Harness::with_extra_sql(GIT_HTTPS_COLUMNS).await;
    let t = team(&h).await;
    let uuid = uuid_of(
        &h.call(
            "POST",
            "/api/v1/applications/public",
            W,
            Some(public_body(
                &t,
                "https://git.example.test/o/r.git",
                json!({"git_https_username": "deploy", "git_https_password": "https-token-1"}),
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
    sqlx::query(
        "UPDATE applications SET docker_compose = 'services:\n  app:\n    environment:\n      SECRET: shown-once-value\n      TOKEN: https-token-1\n' WHERE uuid = $1",
    )
    .bind(&uuid)
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
