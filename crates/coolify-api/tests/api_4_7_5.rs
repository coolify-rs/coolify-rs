//! API access checks and request validation of Coolify v4.6.0..v4.7.5: resources placed on a
//! server (`ServerPolicy::placeResource`), team credentials, cloud provider tokens limited to
//! admins, preview changes that need the write permission, rollback commits, `docker_tag` and
//! `cancel_previous` of a deploy, names validated like the UI, and `PATCH /teams/current`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{Harness, Resp};
use serde_json::{Value, json};

const W: Option<&str> = Some("acme_owner_write");

const PLACE_DENIED: &str = r#"{"message":"You need write access on the whole team, or on a resource that already runs on this server, to place resources on it.","error":"Unauthorized"}"#;
const CREDENTIALS_DENIED: &str = r#"{"message":"You need write access on the whole team to use team credentials, such as private keys, git sources, and S3 storages.","error":"Unauthorized"}"#;

struct Team {
    key: String,
    server: String,
    project: String,
}

fn uuid_of(r: &Resp) -> String {
    assert!(r.status.is_success(), "{}", r.body);
    r.json()["uuid"].as_str().unwrap().to_owned()
}

/// A usable server with its default destination, and a project.
async fn team(h: &Harness) -> Team {
    let pem = coolify_compat::sshkey::generate(coolify_compat::sshkey::KeyKind::Ed25519).unwrap();
    let key = uuid_of(
        &h.call(
            "POST",
            "/api/v1/security/keys",
            W,
            Some(json!({"name": "key", "private_key": pem})),
        )
        .await,
    );
    let server = uuid_of(
        &h.call(
            "POST",
            "/api/v1/servers",
            W,
            Some(json!({"name": "srv", "ip": "203.0.113.75", "private_key_uuid": key})),
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
    Team {
        key,
        server,
        project,
    }
}

async fn application(h: &Harness, t: &Team) -> String {
    uuid_of(
        &h.call(
            "POST",
            "/api/v1/applications/dockerimage",
            W,
            Some(json!({
                "server_uuid": t.server,
                "project_uuid": t.project,
                "environment_name": "production",
                "docker_registry_image_name": "nginx",
                "docker_registry_image_tag": "alpine",
                "ports_exposes": "80",
                "instant_deploy": false,
            })),
        )
        .await,
    )
}

/// A token of `user` in the Acme team with these abilities and scopes; returns the bearer value.
async fn token(h: &Harness, user: i64, abilities: &str, scopes: Option<&str>) -> String {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let plain = format!(
        "api-4-7-5-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    );
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO personal_access_tokens (tokenable_type, tokenable_id, name, token, team_id, abilities, scopes, created_at, updated_at) \
         VALUES ('App\\Models\\User', $1, '4.7.5 test', $2, '1', $3, $4::json, now(), now()) RETURNING id",
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

async fn send(h: &Harness, method: &str, path: &str, bearer: &str, body: Option<Value>) -> Resp {
    h.request(
        method,
        path,
        &[("authorization", &format!("Bearer {bearer}"))],
        body,
    )
    .await
}

fn project_scope(uuid: &str) -> String {
    json!([{"scope": "project", "uuid": uuid}]).to_string()
}

/// `ServerPolicy::placeResource` (8011d0d6a): a token limited to a project places resources only
/// on servers where it has write access on a resource.
#[tokio::test]
async fn a_scoped_token_places_resources_only_where_it_has_a_resource() {
    let h = Harness::new().await;
    let t = team(&h).await;
    application(&h, &t).await;
    let other = uuid_of(
        &h.call(
            "POST",
            "/api/v1/projects",
            W,
            Some(json!({"name": "Other"})),
        )
        .await,
    );

    // The owner's token for the project without a resource on the server.
    let outsider = token(&h, 1, r#"["root"]"#, Some(&project_scope(&other))).await;
    let database = |project: &str| json!({"server_uuid": t.server, "project_uuid": project, "environment_name": "production"});
    send(
        &h,
        "POST",
        "/api/v1/databases/postgresql",
        &outsider,
        Some(database(&other)),
    )
    .await
    .assert(403, PLACE_DENIED);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM standalone_postgresqls")
            .fetch_one(h.tdb.db.pool())
            .await
            .unwrap(),
        0
    );

    // The token for the project whose application runs on the server.
    let insider = token(&h, 1, r#"["root"]"#, Some(&project_scope(&t.project))).await;
    let r = send(
        &h,
        "POST",
        "/api/v1/databases/postgresql",
        &insider,
        Some(database(&t.project)),
    )
    .await;
    assert_eq!(r.status, 201, "{}", r.body);

    // The same holds for a service: write on the other project is not write on a resource of
    // the server.
    let writer = token(&h, 1, r#"["read","write"]"#, Some(&project_scope(&other))).await;
    send(
        &h,
        "POST",
        "/api/v1/services",
        &writer,
        Some(json!({"server_uuid": t.server, "project_uuid": other, "environment_name": "production", "docker_compose_raw": "c2VydmljZXM6CiAgd2ViOgogICAgaW1hZ2U6IG5naW54Cg=="})),
    )
    .await
    .assert(403, PLACE_DENIED);
    h.finish().await;
}

/// `ResourceCreatePolicy::useTeamCredentials` (8011d0d6a): a private key is a team credential.
#[tokio::test]
async fn a_scoped_token_cannot_use_a_team_private_key() {
    let h = Harness::new().await;
    let t = team(&h).await;
    application(&h, &t).await;
    let scoped = token(&h, 1, r#"["root"]"#, Some(&project_scope(&t.project))).await;
    let body = json!({
        "server_uuid": t.server,
        "project_uuid": t.project,
        "environment_name": "production",
        "private_key_uuid": t.key,
        "git_repository": "git@github.com:acme/app.git",
        "git_branch": "main",
        "build_pack": "dockerfile",
        "ports_exposes": "80",
        "instant_deploy": false,
    });
    send(
        &h,
        "POST",
        "/api/v1/applications/private-deploy-key",
        &scoped,
        Some(body.clone()),
    )
    .await
    .assert(403, CREDENTIALS_DENIED);
    // The same request with the team-wide token of the owner is not refused for its credentials.
    let r = h
        .call(
            "POST",
            "/api/v1/applications/private-deploy-key",
            W,
            Some(body),
        )
        .await;
    assert_ne!(r.status, 403, "{}", r.body);
    h.finish().await;
}

/// `CloudProviderTokensController::index`: limited to owners and admins.
#[tokio::test]
async fn cloud_provider_tokens_are_listed_for_admins_only() {
    let h = Harness::new().await;
    h.get("/api/v1/cloud-tokens", "acme_member_read")
        .await
        .assert(403, r#"{"message":"This action is unauthorized."}"#);
    h.get("/api/v1/cloud-tokens", "acme_owner_read")
        .await
        .assert(200, "[]");
    h.finish().await;
}

/// `ApplicationPreviewsController::store` (c75e7078b): opening a preview needs the write
/// permission; the deploy permission only redeploys an existing preview.
#[tokio::test]
async fn opening_a_preview_needs_the_write_permission() {
    let h = Harness::new().await;
    let t = team(&h).await;
    let app = application(&h, &t).await;
    let deployer = token(&h, 1, r#"["read","deploy"]"#, None).await;
    send(
        &h,
        "POST",
        &format!("/api/v1/applications/{app}/previews"),
        &deployer,
        Some(json!({"pull_request_id": 5, "docker_tag": "pr-5"})),
    )
    .await
    .assert(
        403,
        r#"{"message":"Missing required permissions: write. Opening a preview, changing its pull request URL, git type, or docker tag, or choosing a commit needs the write permission. The deploy permission only redeploys an existing preview."}"#,
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM application_previews")
            .fetch_one(h.tdb.db.pool())
            .await
            .unwrap(),
        0
    );
    h.finish().await;
}

/// `ApplicationsController::rollback_by_uuid` (c75e7078b): the commit must be a rollback image or
/// a successful production deployment.
#[tokio::test]
async fn a_rollback_commit_must_have_been_deployed() {
    let h = Harness::new().await;
    let t = team(&h).await;
    let app = application(&h, &t).await;
    h.call(
        "POST",
        &format!("/api/v1/applications/{app}/rollback"),
        Some("acme_owner_root"),
        Some(json!({"commit": "0123456789abcdef0123456789abcdef01234567"})),
    )
    .await
    .assert(
        422,
        r#"{"message":"Validation failed.","errors":{"commit":["The commit is not a rollback image or a successful deployment of this application."]}}"#,
    );
    h.finish().await;
}

/// `DeployController::deploy`: `cancel_previous` and (f4cbf63e7) `docker_tag`.
#[tokio::test]
async fn deploy_parameters_are_validated() {
    let h = Harness::new().await;
    let root = "acme_owner_root";
    h.call(
        "POST",
        "/api/v1/deploy?uuid=x&cancel_previous=all",
        Some(root),
        None,
    )
    .await
    .assert(
        400,
        r#"{"message":"cancel_previous must be one of: none, queued, building."}"#,
    );
    h.call(
        "POST",
        "/api/v1/deploy?uuid=x&pull_request_id=3&docker_tag=a%3Bb",
        Some(root),
        None,
    )
    .await
    .assert(
        400,
        r#"{"message":"docker_tag must be a valid Docker tag: letters, digits, underscores, periods, and dashes, at most 128 characters."}"#,
    );
    // A valid value goes on to the next check.
    h.call(
        "POST",
        "/api/v1/deploy?cancel_previous=Queued",
        Some(root),
        None,
    )
    .await
    .assert(400, r#"{"message":"You must provide uuid or tag."}"#);
    h.finish().await;
}

/// 32541a32c: resource names and descriptions are validated like the UI.
#[tokio::test]
async fn application_names_are_validated_like_the_ui() {
    let h = Harness::new().await;
    let t = team(&h).await;
    h.call(
        "POST",
        "/api/v1/applications/dockerimage",
        W,
        Some(json!({
            "server_uuid": t.server,
            "project_uuid": t.project,
            "environment_name": "production",
            "docker_registry_image_name": "nginx",
            "ports_exposes": "80",
            "name": "a;",
            "description": "<script>",
        })),
    )
    .await
    .assert(
        422,
        r#"{"message":"Validation failed.","errors":{"name":["The name must be at least 3 characters.","The name may only contain letters (including Unicode), numbers, spaces, and these characters: - _ . \/ @ & ( ) # , : +"],"description":["The description may only contain letters (including Unicode), numbers, spaces, and common punctuation: - _ . , ! ? ( ) ' \" + = * \/ @ &"]}}"#,
    );
    h.finish().await;
}

/// `TeamController::update_current_team` (`PATCH /team`): one of its two fields is required.
#[tokio::test]
async fn team_update_needs_a_field() {
    let h = Harness::new().await;
    h.call("PATCH", "/api/v1/team", W, Some(json!({})))
        .await
        .assert(
            422,
            r#"{"message":"Validation failed.","errors":{"is_build_server_fallback_enabled":["The is build server fallback enabled field is required when scan images on new servers is not present."],"scan_images_on_new_servers":["The scan images on new servers field is required when is build server fallback enabled is not present."]}}"#,
        );
    h.call(
        "PATCH",
        "/api/v1/team",
        W,
        Some(json!({"is_build_server_fallback_enabled": false, "extra": 1})),
    )
    .await
    .assert(
        422,
        r#"{"message":"Validation failed.","errors":{"extra":["This field is not allowed."]}}"#,
    );
    let r = h
        .call(
            "PATCH",
            "/api/v1/team",
            W,
            Some(json!({"is_build_server_fallback_enabled": false})),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.json()["is_build_server_fallback_enabled"], json!(false));
    h.finish().await;
}
