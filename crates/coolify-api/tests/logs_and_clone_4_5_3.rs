//! Coolify 4.5.3 against a scripted stand-in for `ssh`: the logs routes read stopped containers,
//! and a database clone with volume data stops a running source only, once, around the copies.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

use common::Harness;
use coolify_remote::{Remote, RemoteConfig};
use serde_json::json;

const ROOT: Option<&str> = Some("acme_owner_root");
const W: Option<&str> = Some("acme_owner_write");

/// Records each script. Every container is a stopped one; `{{.Names}}` (the running containers
/// check of a clone) lists none.
const FAKE_SSH: &str = r#"#!/bin/sh
script=$(cat)
dir=$(dirname "$0")
n=$(ls "$dir" | grep -c '^script')
printf '%s' "$script" > "$dir/script$n"
case "$script" in
  *"docker logs"*) echo "last words of a stopped container" ;;
  *"{{.Names}}"*) ;;
  *"docker ps -a"*) printf '{"ID":"c0ffee","Names":"stopped-one","State":"exited","Labels":"coolify.applicationId=1,com.docker.compose.service=web"}\n' ;;
  *"docker inspect"*) printf '{"Name":"/stopped-one","State":{"Status":"exited"}}\n' ;;
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
    let uuid = |r: common::Resp| {
        assert!(r.status.is_success(), "{}", r.body);
        r.json()["uuid"].as_str().unwrap().to_owned()
    };
    let pem = coolify_compat::sshkey::generate(coolify_compat::sshkey::KeyKind::Ed25519).unwrap();
    let key = uuid(
        h.call(
            "POST",
            "/api/v1/security/keys",
            W,
            Some(json!({"name": "k", "private_key": pem})),
        )
        .await,
    );
    let server = uuid(
        h.call(
            "POST",
            "/api/v1/servers",
            W,
            Some(json!({"name": "s", "ip": "203.0.113.70", "private_key_uuid": key})),
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
    let project = uuid(
        h.call("POST", "/api/v1/projects", W, Some(json!({"name": "Shop"})))
            .await,
    );
    Fixture {
        h,
        dir: bin,
        _tmp: tmp,
        project,
        server,
    }
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

async fn postgres(f: &Fixture) -> String {
    let r = f
        .h
        .call(
            "POST",
            "/api/v1/databases/postgresql",
            W,
            Some(json!({"server_uuid": f.server, "project_uuid": f.project, "environment_name": "production"})),
        )
        .await;
    assert_eq!(r.status, 201, "{}", r.body);
    r.json()["uuid"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn logs_of_stopped_containers() {
    let f = fixture().await;
    let h = &f.h;
    let db = postgres(&f).await;
    h.get(
        &format!("/api/v1/databases/{db}/logs"),
        "acme_owner_sensitive",
    )
    .await
    .assert(200, r#"{"logs":"last words of a stopped container"}"#);
    let app = h
        .call(
            "POST",
            "/api/v1/applications/dockerimage",
            W,
            Some(json!({
                "server_uuid": f.server,
                "project_uuid": f.project,
                "environment_name": "production",
                "docker_registry_image_name": "nginx",
                "ports_exposes": "80",
                "instant_deploy": false,
            })),
        )
        .await
        .json()["uuid"]
        .as_str()
        .unwrap()
        .to_owned();
    h.get(
        &format!("/api/v1/applications/{app}/logs"),
        "acme_owner_sensitive",
    )
    .await
    .assert(200, r#"{"logs":"last words of a stopped container"}"#);
    h.get(
        &format!("/api/v1/applications/{app}/logs?service_name=web"),
        "acme_owner_sensitive",
    )
    .await
    .assert(200, r#"{"logs":"last words of a stopped container"}"#);
    h.get(
        &format!("/api/v1/applications/{app}/logs?service_name=api"),
        "acme_owner_sensitive",
    )
    .await
    .assert(
        404,
        r#"{"message":"No container found for service_name 'api'."}"#,
    );
    // No inspect of the container state any more.
    assert!(!scripts(&f.dir).iter().any(|s| s.contains("docker inspect")));
    f.h.finish().await;
}

async fn destination_uuid(f: &Fixture) -> String {
    sqlx::query_scalar(
        "SELECT d.uuid FROM standalone_dockers d JOIN servers s ON s.id = d.server_id WHERE s.uuid = $1",
    )
    .bind(&f.server)
    .fetch_one(f.h.tdb.db.pool())
    .await
    .unwrap()
}

async fn wait_for(dir: &std::path::Path, needle: &str) -> Vec<String> {
    for _ in 0..100 {
        let all = scripts(dir);
        if all.iter().any(|s| s.contains(needle)) {
            return all;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("no script with {needle}: {:#?}", scripts(dir));
}

#[tokio::test]
async fn clone_with_volume_data_stops_only_a_running_source() {
    let f = fixture().await;
    let h = &f.h;
    let db = postgres(&f).await;
    let destination = destination_uuid(&f).await;
    let clone =
        |name: &str| json!({"destination_uuid": destination, "name": name, "clone_volumes": true});

    // A stopped source stays stopped: only the copy runs.
    let r = h
        .call(
            "POST",
            &format!("/api/v1/databases/{db}/clone"),
            ROOT,
            Some(clone("copy-1")),
        )
        .await;
    assert_eq!(r.status, 201, "{}", r.body);
    let all = wait_for(&f.dir, "cp -a /source/. /target/").await;
    assert!(!all.iter().any(|s| s.contains("docker stop")), "{all:#?}");
    assert!(
        !all.iter().any(|s| s.contains("docker compose")),
        "{all:#?}"
    );

    // A running source is stopped once, copied, then started again.
    for e in std::fs::read_dir(&f.dir).unwrap().flatten() {
        if e.file_name().to_string_lossy().starts_with("script") {
            std::fs::remove_file(e.path()).unwrap();
        }
    }
    sqlx::query("UPDATE standalone_postgresqls SET status = 'running:healthy' WHERE uuid = $1")
        .bind(&db)
        .execute(h.tdb.db.pool())
        .await
        .unwrap();
    let r = h
        .call(
            "POST",
            &format!("/api/v1/databases/{db}/clone"),
            ROOT,
            Some(clone("copy-2")),
        )
        .await;
    assert_eq!(r.status, 201, "{}", r.body);
    let all = wait_for(&f.dir, "docker compose").await;
    let at = |needle: &str| {
        all.iter()
            .position(|s| s.contains(needle))
            .unwrap_or_else(|| panic!("no {needle}: {all:#?}"))
    };
    let stop = at("docker stop");
    let check = at("{{.Names}}");
    let copy = at("cp -a /source/. /target/");
    let start = at("docker compose");
    assert!(stop < check && check < copy && copy < start, "{all:#?}");
    assert!(all[check].contains(&format!("label=coolify.databaseUuid={db}")));
    // The start of the database stops a leftover container itself; the clone stops once.
    let stops = all
        .iter()
        .filter(|s| s.contains("docker stop") && !s.contains("docker compose"));
    assert_eq!(stops.count(), 1, "{all:#?}");
    f.h.finish().await;
}
