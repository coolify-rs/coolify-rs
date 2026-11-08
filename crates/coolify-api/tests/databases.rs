//! Standalone PostgreSQL databases: API and lifecycle jobs against a scripted
//! stand-in for `ssh`. compat/diff checks the same flows against upstream on
//! a real server; these tests pin them locally.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

use common::Harness;
use coolify_remote::{Remote, RemoteConfig};
use serde_json::{Value, json};

const ROOT: Option<&str> = Some("acme_owner_root");
const W: Option<&str> = Some("acme_owner_write");

/// Records each script it receives and answers `docker container inspect` as a healthy container.
const FAKE_SSH: &str = r#"#!/bin/sh
script=$(cat)
dir=$(dirname "$0")
n=$(ls "$dir" | grep -c '^script')
printf '%s' "$script" > "$dir/script$n"
case "$script" in
  *"docker container inspect '"*)
    uuid=$(printf '%s' "$script" | sed -n "s/.*docker container inspect '\([a-z0-9]*\)'.*/\1/p")
    printf '{"Name":"/%s","State":{"Status":"running","Health":{"Status":"healthy"}}}\n' "$uuid" ;;
  *) echo "ok" ;;
esac
"#;

struct Fixture {
    h: Harness,
    dir: std::path::PathBuf,
    _tmp: tempfile::TempDir,
    project: String,
    server: String,
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
            Some(json!({"name": "k", "private_key": pem})),
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
            Some(json!({"name": "s", "ip": "203.0.113.50", "private_key_uuid": key})),
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
    Fixture {
        h,
        dir: bin,
        _tmp: tmp,
        project,
        server,
    }
}

async fn status(h: &Harness, uuid: &str) -> String {
    sqlx::query_scalar(
        "SELECT status FROM standalone_postgresqls WHERE uuid = $1 \
         UNION ALL SELECT status FROM standalone_redis WHERE uuid = $1",
    )
    .bind(uuid)
    .fetch_one(h.tdb.db.pool())
    .await
    .unwrap()
}

async fn wait_status(h: &Harness, uuid: &str, want: &str) {
    for _ in 0..100 {
        if status(h, uuid).await == want {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!(
        "status of {uuid} is {}, wanted {want}",
        status(h, uuid).await
    );
}

fn scripts(dir: &std::path::Path) -> Vec<String> {
    let mut names: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("script"))
        .collect();
    names.sort_by_key(|e| {
        e.file_name()
            .to_string_lossy()
            .trim_start_matches("script")
            .parse::<u32>()
            .unwrap_or(0)
    });
    names
        .iter()
        .map(|e| std::fs::read_to_string(e.path()).unwrap())
        .collect()
}

#[tokio::test]
async fn create_show_list_and_lifecycle() {
    let f = fixture().await;
    let h = &f.h;
    let body = json!({"server_uuid": f.server, "project_uuid": f.project, "environment_name": "production", "name": "orders <b>db</b>"});
    let r = h
        .call(
            "POST",
            "/api/v1/databases/postgresql",
            W,
            Some(body.clone()),
        )
        .await;
    assert_eq!(r.status, 201, "{}", r.body);
    let created = r.json();
    assert_eq!(
        created.as_object().unwrap().keys().collect::<Vec<_>>(),
        ["uuid"],
        "no URLs without read:sensitive"
    );
    let uuid = created["uuid"].as_str().unwrap().to_owned();

    // The model's `created` hook adds the data volume.
    let (vol, mount): (String, String) = sqlx::query_as(
        "SELECT name, mount_path FROM local_persistent_volumes WHERE resource_type = 'App\\Models\\StandalonePostgresql' \
         AND resource_id = (SELECT id FROM standalone_postgresqls WHERE uuid = $1)",
    )
    .bind(&uuid)
    .fetch_one(h.tdb.db.pool())
    .await
    .unwrap();
    assert_eq!(
        (vol, mount.as_str()),
        (format!("postgres-data-{uuid}"), "/var/lib/postgresql/data")
    );

    let show = h
        .get(&format!("/api/v1/databases/{uuid}"), "acme_owner_read")
        .await
        .json();
    let keys: Vec<&str> = show
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(&keys[..3], ["uuid", "name", "config_hash"]);
    assert_eq!(keys[keys.len() - 2..], ["created_at", "updated_at"]);
    assert_eq!(show["name"], "orders db");
    assert_eq!(show["status"], "exited:unhealthy");
    assert!(show.get("postgres_password").is_none() && show.get("internal_db_url").is_none());
    assert_eq!(show["environment"]["project"]["name"], "Shop");
    assert!(
        show["destination"]["server"]["settings"]
            .get("sentinel_token")
            .is_none()
    );
    let secret = h
        .get(&format!("/api/v1/databases/{uuid}"), "acme_owner_root")
        .await
        .json();
    assert_eq!(secret["postgres_password"].as_str().unwrap().len(), 64);
    assert!(
        secret["internal_db_url"]
            .as_str()
            .unwrap()
            .ends_with(&format!("@{uuid}:5432/postgres"))
    );
    let list = h.get("/api/v1/databases", "acme_owner_read").await.json();
    assert_eq!(list[0]["uuid"], uuid.as_str());
    assert_eq!(list[0]["backup_configs"], json!([]));
    assert!(list[0].get("environment").is_none());

    // Start: the script writes the compose file and starts the container.
    let r = h
        .call(
            "POST",
            &format!("/api/v1/databases/{uuid}/start"),
            ROOT,
            None,
        )
        .await;
    r.assert(200, r#"{"message":"Database starting request queued."}"#);
    let again = h
        .call(
            "POST",
            &format!("/api/v1/databases/{uuid}/start"),
            ROOT,
            None,
        )
        .await;
    assert!(again.status == 409 || again.status == 400, "{}", again.body);
    wait_status(h, &uuid, "running:healthy").await;
    let all = scripts(&f.dir);
    let start = all
        .iter()
        .find(|s| s.contains("docker compose -f"))
        .expect("start script");
    assert!(start.contains(&format!("mkdir -p /data/coolify/databases/{uuid}")));
    assert!(start.contains("docker stop --timeout=10"));
    assert!(start.contains(&format!(
        "docker compose -f /data/coolify/databases/{uuid}/docker-compose.yml up -d"
    )));
    let log: (String, String) = sqlx::query_as(
        "SELECT properties->>'status', description FROM activity_log WHERE properties->>'type_uuid' = $1 ORDER BY id DESC LIMIT 1",
    )
    .bind(&uuid)
    .fetch_one(h.tdb.db.pool())
    .await
    .unwrap();
    assert_eq!(log.0, "finished");
    let entries: Vec<Value> = serde_json::from_str(&log.1).unwrap();
    assert_eq!(entries[0]["type"], "out");
    let state = h
        .call("GET", &format!("/ui/databases/{uuid}/state"), None, None)
        .await;
    assert!(state.status == 401 || state.status == 419 || state.status == 200);

    let r = h
        .call(
            "POST",
            &format!("/api/v1/databases/{uuid}/start"),
            ROOT,
            None,
        )
        .await;
    r.assert(400, r#"{"message":"Database is already running."}"#);
    let r = h
        .call(
            "POST",
            &format!("/api/v1/databases/{uuid}/stop"),
            ROOT,
            None,
        )
        .await;
    r.assert(200, r#"{"message":"Database stopping request queued."}"#);
    wait_status(h, &uuid, "exited:unhealthy").await;
    let r = h
        .call(
            "POST",
            &format!("/api/v1/databases/{uuid}/stop"),
            ROOT,
            None,
        )
        .await;
    r.assert(400, r#"{"message":"Database is already stopped."}"#);

    // Delete: soft delete right away, then the job removes the rows.
    let r = h
        .call("DELETE", &format!("/api/v1/databases/{uuid}"), W, None)
        .await;
    r.assert(403, r#"{"message":"Missing required permissions: delete"}"#);
    let r = h
        .call("DELETE", &format!("/api/v1/databases/{uuid}"), ROOT, None)
        .await;
    r.assert(200, r#"{"message":"Database deletion request queued."}"#);
    h.call("GET", &format!("/api/v1/databases/{uuid}"), ROOT, None)
        .await
        .assert(404, r#"{"message":"Database not found."}"#);
    for _ in 0..100 {
        let left: i64 =
            sqlx::query_scalar("SELECT count(*) FROM standalone_postgresqls WHERE uuid = $1")
                .bind(&uuid)
                .fetch_one(h.tdb.db.pool())
                .await
                .unwrap();
        if left == 0 {
            let scripts = scripts(&f.dir);
            assert!(
                scripts
                    .iter()
                    .any(|s| s.contains(&format!("rm -rf /data/coolify/databases/{uuid}")))
            );
            assert!(
                scripts
                    .iter()
                    .any(|s| s.contains(&format!("docker volume rm -f 'postgres-data-{uuid}'")))
            );
            f.h.finish().await;
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("rows were not deleted");
}

#[tokio::test]
async fn sqlite_and_tags_are_created() {
    let f = fixture().await;
    let h = &f.h;
    let base = json!({"server_uuid": f.server, "project_uuid": f.project, "environment_name": "production"});
    let mut tagged = base.clone();
    tagged["tags"] = json!(["prod"]);
    let created = h
        .call("POST", "/api/v1/databases/postgresql", W, Some(tagged))
        .await;
    assert_eq!(created.status, 201);
    let uuid = created.json()["uuid"].as_str().unwrap().to_owned();
    let tags = h
        .call("GET", &format!("/api/v1/databases/{uuid}/tags"), ROOT, None)
        .await;
    assert_eq!(tags.json()[0]["name"], "prod");
    let sqlite = h
        .call("POST", "/api/v1/databases/sqlite", W, Some(base))
        .await;
    assert_eq!(sqlite.status, 201);
    // `create_standalone_sqlite` sets the image itself (Coolify v4.6.0 drops the column default).
    let image: String = sqlx::query_scalar("SELECT image FROM standalone_sqlites WHERE uuid = $1")
        .bind(sqlite.json()["uuid"].as_str().unwrap())
        .fetch_one(h.tdb.db.pool())
        .await
        .unwrap();
    assert_eq!(image, "peakimages/sqlite:3.54.1-v0.2.0");
    f.h.finish().await;
}

/// `Postgresql\General::save_init_script` (Coolify v4.6.0): a new script or a renamed one must
/// have a safe filename, so a filename cannot leave `docker-entrypoint-initdb.d`.
#[tokio::test]
async fn init_script_filenames_cannot_traverse_paths() {
    let f = fixture().await;
    let h = &f.h;
    let body = json!({"server_uuid": f.server, "project_uuid": f.project, "environment_name": "production"});
    let created = h
        .call("POST", "/api/v1/databases/postgresql", W, Some(body))
        .await;
    assert_eq!(created.status, 201, "{}", created.body);
    let uuid = created.json()["uuid"].as_str().unwrap().to_owned();
    let (cookie, csrf) = h.ui_login("alice@example.test").await;
    let headers = [("cookie", cookie.as_str()), ("x-csrf-token", csrf.as_str())];
    let path = format!("/ui/databases/{uuid}/init-scripts");
    let scripts = || async {
        let v: Option<String> = sqlx::query_scalar(
            "SELECT init_scripts::text FROM standalone_postgresqls WHERE uuid = $1",
        )
        .bind(&uuid)
        .fetch_one(h.tdb.db.pool())
        .await
        .unwrap();
        v.unwrap_or_default()
    };
    let save = |filename: &str, original: &str| {
        h.request(
            "POST",
            &path,
            &headers,
            Some(json!({"action": "save", "filename": filename, "original_filename": original, "content": "SELECT 1;"})),
        )
    };
    let toast_type = |r: &common::Resp| {
        r.json()["toasts"][0]["type"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    };

    // A save without an existing script used to skip the check.
    for bad in [
        "../../../etc/cron.d/x",
        "a/b.sql",
        "..",
        "x\\y.sql",
        "a b.sql",
    ] {
        let r = save(bad, "").await;
        assert_eq!(r.status, 200, "{}", r.body);
        assert_eq!(toast_type(&r), "error", "{bad}: {}", r.body);
        assert!(!scripts().await.contains("SELECT 1;"), "{bad} was saved");
    }
    let r = save("init.sql", "").await;
    assert_eq!(toast_type(&r), "success", "{}", r.body);
    assert!(scripts().await.contains("init.sql"));
    // Saving it again under its own name needs no check; renaming it does.
    let r = save("init.sql", "init.sql").await;
    assert_eq!(toast_type(&r), "success", "{}", r.body);
    let r = save("../init.sql", "init.sql").await;
    assert_eq!(toast_type(&r), "error", "{}", r.body);
    assert!(!scripts().await.contains("../init.sql"));
    f.h.finish().await;
}

#[tokio::test]
async fn redis_create_and_start() {
    let f = fixture().await;
    let h = &f.h;
    let body = json!({"server_uuid": f.server, "project_uuid": f.project, "environment_name": "production", "redis_password": "s3cret-pass"});
    let r = h
        .call("POST", "/api/v1/databases/redis", ROOT, Some(body))
        .await;
    assert_eq!(r.status, 201, "{}", r.body);
    let created = r.json();
    let uuid = created["uuid"].as_str().unwrap().to_owned();
    assert_eq!(
        created["internal_db_url"],
        format!("redis://default:s3cret-pass@{uuid}:6379/0")
    );

    // create_standalone_redis keeps the password and username in variables.
    let keys: Vec<String> = sqlx::query_scalar(
        "SELECT key FROM environment_variables WHERE resourceable_type = 'App\\Models\\StandaloneRedis' ORDER BY id",
    )
    .fetch_all(h.tdb.db.pool())
    .await
    .unwrap();
    assert_eq!(keys, ["REDIS_PASSWORD", "REDIS_USERNAME"]);
    let show = h
        .get(&format!("/api/v1/databases/{uuid}"), "acme_owner_root")
        .await
        .json();
    assert_eq!(show["database_type"], "standalone-redis");
    assert!(show.get("redis_password").is_none() && show.get("legacy_password_quoting").is_none());
    assert_eq!(
        show["internal_db_url"],
        format!("redis://default:s3cret-pass@{uuid}:6379/0")
    );

    // A field of another type is refused.
    let r = h
        .call(
            "POST",
            "/api/v1/databases/redis",
            W,
            Some(json!({"server_uuid": f.server, "project_uuid": f.project, "environment_name": "production", "postgres_user": "x"})),
        )
        .await;
    r.assert(
        422,
        r#"{"message":"Validation failed.","errors":{"postgres_user":["This field is not allowed."]}}"#,
    );

    let r = h
        .call(
            "POST",
            &format!("/api/v1/databases/{uuid}/start"),
            ROOT,
            None,
        )
        .await;
    r.assert(200, r#"{"message":"Database starting request queued."}"#);
    wait_status(h, &uuid, "running:healthy").await;
    let all = scripts(&f.dir);
    let start = all
        .iter()
        .find(|s| s.contains("docker compose -f"))
        .expect("start script");
    use base64::Engine as _;
    let compose = start
        .lines()
        .find(|l| l.contains("docker-compose.yml > /dev/null"))
        .and_then(|l| l.split('\'').nth(1))
        .map(|b| {
            String::from_utf8(base64::engine::general_purpose::STANDARD.decode(b).unwrap()).unwrap()
        })
        .expect("compose file");
    assert!(
        compose.contains(r#"command: "redis-server --requirepass 's3cret-pass' --appendonly yes""#),
        "{compose}"
    );
    assert!(
        compose.contains("- REDIS_PASSWORD=s3cret-pass"),
        "{compose}"
    );
    assert!(compose.contains("- REDIS_USERNAME=default"), "{compose}");
    // The start job saves the hash after the status, so `running:healthy` can be seen first.
    let mut hash: Option<String> = None;
    for _ in 0..100 {
        hash = sqlx::query_scalar("SELECT config_hash FROM standalone_redis WHERE uuid = $1")
            .bind(&uuid)
            .fetch_one(h.tdb.db.pool())
            .await
            .unwrap();
        if hash.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(hash.is_some());
    f.h.finish().await;
}
