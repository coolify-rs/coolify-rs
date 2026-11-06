//! The developer view of environment variables (`All::submit`, `parseEnvDevView`,
//! `formatEnvVariableForDevView`): `# coolify:` flag lines and quoted values.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::Harness;
use serde_json::{Value, json};

const T: Option<&str> = Some("acme_owner_root");

async fn application(h: &Harness) -> String {
    let uuid = |r: common::Resp| {
        assert!(r.status.is_success(), "{}", r.body);
        r.json()["uuid"].as_str().unwrap().to_owned()
    };
    let pem = coolify_compat::sshkey::generate(coolify_compat::sshkey::KeyKind::Ed25519).unwrap();
    let key = uuid(
        h.call(
            "POST",
            "/api/v1/security/keys",
            T,
            Some(json!({"name": "key", "private_key": pem})),
        )
        .await,
    );
    let server = uuid(
        h.call(
            "POST",
            "/api/v1/servers",
            T,
            Some(json!({"name": "srv", "ip": "203.0.113.61", "private_key_uuid": key})),
        )
        .await,
    );
    let project = uuid(
        h.call("POST", "/api/v1/projects", T, Some(json!({"name": "proj"})))
            .await,
    );
    uuid(
        h.call(
            "POST",
            "/api/v1/applications/dockerimage",
            T,
            Some(json!({
                "server_uuid": server,
                "project_uuid": project,
                "environment_name": "production",
                "name": "web",
                "docker_registry_image_name": "nginx",
                "docker_registry_image_tag": "alpine",
                "ports_exposes": "80",
                "instant_deploy": false,
            })),
        )
        .await,
    )
}

async fn flags(h: &Harness, app: &str, key: &str) -> (bool, bool, bool) {
    sqlx::query_as(
        "SELECT is_literal, is_buildtime, is_runtime FROM environment_variables
         WHERE key = $1 AND is_preview = false
           AND resourceable_id = (SELECT id FROM applications WHERE uuid = $2)",
    )
    .bind(key)
    .bind(app)
    .fetch_one(h.tdb.db.pool())
    .await
    .unwrap()
}

#[tokio::test]
async fn flag_lines_and_quoted_values() {
    let h = Harness::new().await;
    let app = application(&h).await;
    let (cookie, csrf) = h.ui_login("alice@example.test").await;
    let headers = [("cookie", cookie.as_str()), ("x-csrf-token", csrf.as_str())];
    let base = format!("/ui/resources/application/{app}/envs");
    let bulk_path = format!("{base}/bulk");
    let bulk = |text: &str| {
        h.request(
            "POST",
            &bulk_path,
            &headers,
            Some(json!({"variables": text, "variables_preview": ""})),
        )
    };
    let dev = || async {
        let r = h
            .request("GET", &format!("{base}/dev"), &headers, None)
            .await;
        assert_eq!(r.status, 200, "{}", r.body);
        r.json()["variables"].as_str().unwrap().to_owned()
    };

    // New variables take the flags of the line above them.
    let r = bulk("# coolify: literal buildtime=false\nA=1\nB='x #y'\nC=\"'q'\"").await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(flags(&h, &app, "A").await, (true, false, true));
    assert_eq!(flags(&h, &app, "B").await, (false, true, true));
    // Values that need quotes come back quoted, so saving the view again keeps them.
    let text = dev().await;
    assert_eq!(
        text,
        "# coolify: literal buildtime=false\nA=1\nB='x #y'\nC=\"'q'\""
    );

    // Without a flag line the flags stay; with one they change.
    let r = bulk("A=2\nB='x #y'\n# coolify: runtime=false\nC=\"'q'\"").await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(flags(&h, &app, "A").await, (true, false, true));
    assert_eq!(flags(&h, &app, "C").await, (false, true, false));
    assert_eq!(
        dev().await,
        "# coolify: literal buildtime=false\nA=2\nB='x #y'\n# coolify: runtime=false\nC=\"'q'\""
    );

    // A bad flag line changes nothing.
    let r = bulk("# coolify: literal=maybe\nA=3").await;
    assert_eq!(r.status, 422, "{}", r.body);
    let body: Value = r.json();
    assert!(
        body.to_string()
            .contains("Line 1: the value of \\\"literal\\\" must be true or false."),
        "{body}"
    );
    assert!(dev().await.contains("A=2"));
    h.finish().await;
}
