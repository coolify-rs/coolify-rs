//! `POST /api/v1/servers/{uuid}/validate` and the `ValidateServer` job.
//!
//! Offline tests drive the job through a scripted stand-in for `ssh`. The live
//! matrix runs against the SSH test host (scripts/testing-host-up.sh) when
//! `COOLIFY_RS_TEST_SSH=host:port` and `COOLIFY_RS_TEST_SSH_KEY=<path>` are set.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

use common::Harness;
use coolify_remote::{Remote, RemoteConfig};
use serde_json::json;

const W: Option<&str> = Some("acme_owner_write");
/// DELETE routes need the `delete` ability since 4.6.0.
const DEL: Option<&str> = Some("acme_owner_delete");

/// Answers validation's commands; the SSH user picks the scenario.
const FAKE_SSH: &str = r#"#!/bin/sh
user=""; prev=""
for a in "$@"; do [ "$prev" = "-l" ] && user="$a"; prev="$a"; done
script=$(cat)
[ "$user" = refused ] && { echo "ssh: connect to host 203.0.113.9 port 22: Connection refused" >&2; exit 255; }
case "$script" in
  *"ls /"*) echo bin ;;
  *"cat /etc/os-release"*) if [ "$user" = gentoo ]; then echo ID=gentoo; else printf 'ID=ubuntu\nID_LIKE=debian\n'; fi ;;
  *"command -v jq"*) [ "$user" = nojq ] && exit 1; echo /usr/bin/jq ;;
  *"command -v "*) echo /usr/bin/tool ;;
  *"docker version --format json"*) echo '{"Server":{"Version":"27.5.1"}}' ;;
  *"docker compose version --short"*) echo v2.30.0 ;;
  *) : ;;
esac
"#;

fn remote_with(ssh: Option<&str>) -> (tempfile::TempDir, Remote) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let mut cfg = RemoteConfig::new(root.join("data"));
    if let Some(script) = ssh {
        let path = root.join("fake-ssh");
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        cfg.ssh_binary = path;
    }
    (dir, Remote::new(cfg).unwrap())
}

async fn add_key(h: &Harness, pem: &str) -> String {
    let r = h
        .call(
            "POST",
            "/api/v1/security/keys",
            W,
            Some(json!({"name": "key", "private_key": pem})),
        )
        .await;
    assert_eq!(r.status, 201, "{}", r.body);
    r.json()["uuid"].as_str().unwrap().to_string()
}

async fn add_server(h: &Harness, key: &str, ip: &str, user: &str) -> (i64, String) {
    let r = h
        .call(
            "POST",
            "/api/v1/servers",
            W,
            Some(json!({"name": user, "ip": ip, "private_key_uuid": key, "user": user})),
        )
        .await;
    assert_eq!(r.status, 201, "{}", r.body);
    let uuid = r.json()["uuid"].as_str().unwrap().to_string();
    let id: i64 = sqlx::query_scalar("SELECT id FROM servers WHERE uuid = $1")
        .bind(&uuid)
        .fetch_one(h.tdb.db.pool())
        .await
        .unwrap();
    (id, uuid)
}

async fn validate_and_wait(h: &Harness, id: i64, uuid: &str) {
    let r = h
        .call("POST", &format!("/api/v1/servers/{uuid}/validate"), W, None)
        .await;
    r.assert(201, r#"{"message":"Validation started."}"#);
    let jobs = h.state.jobs.as_ref().unwrap();
    for _ in 0..300 {
        if jobs.progress(id).is_some_and(|p| !p.running) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("validation did not finish");
}

#[derive(Debug, PartialEq)]
struct Outcome {
    logs: Option<String>,
    reachable: bool,
    usable: bool,
    docker: Option<String>,
    compose: Option<String>,
}

async fn outcome(h: &Harness, id: i64) -> Outcome {
    let (logs, reachable, usable, docker, compose): (Option<String>, bool, bool, Option<String>, Option<String>) =
        sqlx::query_as(
            "SELECT s.validation_logs, ss.is_reachable, ss.is_usable, ss.docker_version, ss.compose_version \
             FROM servers s JOIN server_settings ss ON ss.server_id = s.id WHERE s.id = $1",
        )
        .bind(id)
        .fetch_one(h.tdb.db.pool())
        .await
        .unwrap();
    Outcome {
        logs,
        reachable,
        usable,
        docker,
        compose,
    }
}

const DOCS: &str = r#"<a target="_blank" class="text-black underline dark:text-white" href="https://docs.docker.com/engine/install/#server" rel="noreferrer noopener">documentation</a>"#;

fn unreachable(error: &str) -> String {
    format!(
        "Server is not reachable. Please validate your configuration and connection.<br>Check this <a target=\"_blank\" class=\"text-black underline dark:text-white\" href=\"https://coolify.io/docs/knowledge-base/server/openssh\" rel=\"noreferrer noopener\">documentation</a> for further help. <br><br><div class=\"text-error\">Error: {error}</div>"
    )
}

#[tokio::test]
async fn endpoint_rules() {
    let h = Harness::new().await;
    let pem = coolify_compat::sshkey::generate(coolify_compat::sshkey::KeyKind::Ed25519).unwrap();
    let key = add_key(&h, &pem).await;
    let (_, uuid) = add_server(&h, &key, "203.0.113.20", "root").await;
    let path = format!("/api/v1/servers/{uuid}/validate");

    h.call("POST", "/api/v1/servers/nope/validate", W, None)
        .await
        .assert(404, r#"{"message":"Server not found."}"#);
    h.call("GET", &path, W, None).await.assert(
        405,
        r#"{"message":"This endpoint has changed to a POST request."}"#,
    );
    let r = h
        .call("POST", &path, Some("acme_member_write_legacy"), None)
        .await;
    assert_eq!(r.status, 403, "{}", r.body);
    h.call("POST", &path, W, Some(json!({"install": "maybe"})))
        .await
        .assert(422, r#"{"message":"Validation failed.","errors":{"install":["The install field must be true or false."]}}"#);
    h.call("POST", &path, W, Some(json!({"install": true}))).await.assert(
        403,
        r#"{"message":"Missing required permissions: deploy. Installing the server runs commands on it and starts the proxy, so it needs a token with the deploy permission."}"#,
    );
    assert_eq!(
        h.call(
            "POST",
            &path,
            Some("acme_owner_root"),
            Some(json!({"install": true}))
        )
        .await
        .status,
        501
    );
    // Without remote execution configured.
    assert_eq!(
        h.call("POST", &path, W, Some(json!({"install": false})))
            .await
            .status,
        501
    );
    h.finish().await;
}

#[tokio::test]
async fn job_follows_upstream_with_scripted_ssh() {
    let (_dir, remote) = remote_with(Some(FAKE_SSH));
    let h = Harness::with_remote(remote).await;
    let pem = coolify_compat::sshkey::generate(coolify_compat::sshkey::KeyKind::Ed25519).unwrap();
    let key = add_key(&h, &pem).await;

    let (id, uuid) = add_server(&h, &key, "203.0.113.21", "root").await;
    validate_and_wait(&h, id, &uuid).await;
    assert_eq!(
        outcome(&h, id).await,
        Outcome {
            logs: None,
            reachable: true,
            usable: true,
            docker: Some("27.5.1".into()),
            compose: Some("2.30.0".into())
        }
    );
    let progress = h.state.jobs.as_ref().unwrap().progress(id).unwrap();
    assert_eq!(
        (progress.docker_version, progress.error),
        (Some(true), None)
    );

    let (id, uuid) = add_server(&h, &key, "203.0.113.22", "refused").await;
    validate_and_wait(&h, id, &uuid).await;
    let o = outcome(&h, id).await;
    assert_eq!(
        o.logs,
        Some(unreachable(
            "ssh: connect to host 203.0.113.9 port 22: Connection refused"
        ))
    );
    assert!(!o.reachable);

    let (id, uuid) = add_server(&h, &key, "203.0.113.23", "nojq").await;
    validate_and_wait(&h, id, &uuid).await;
    assert_eq!(
        outcome(&h, id).await.logs.as_deref(),
        Some(
            "Prerequisites (jq) are not installed. Please install them before continuing or use the validation with installation endpoint."
        )
    );

    let (id, uuid) = add_server(&h, &key, "203.0.113.24", "gentoo").await;
    validate_and_wait(&h, id, &uuid).await;
    assert_eq!(
        outcome(&h, id).await.logs,
        Some(format!(
            "Server OS type is not supported. Please install Docker manually before continuing: {DOCS}."
        ))
    );

    // A placeholder IP is skipped without touching the reachability flag.
    let (id, uuid) = add_server(&h, &key, "203.0.113.25", "root").await;
    sqlx::query("UPDATE servers SET ip = '1.2.3.4' WHERE id = $1")
        .bind(id)
        .execute(h.tdb.db.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE server_settings SET is_reachable = true WHERE server_id = $1")
        .bind(id)
        .execute(h.tdb.db.pool())
        .await
        .unwrap();
    validate_and_wait(&h, id, &uuid).await;
    let o = outcome(&h, id).await;
    assert_eq!(o.logs, Some(unreachable("Server skipped.")));
    assert!(o.reachable);
    h.finish().await;
}

#[tokio::test]
async fn changing_the_address_drops_the_pinned_host_key() {
    let (dir, remote) = remote_with(None);
    let h = Harness::with_remote(remote).await;
    let pem = coolify_compat::sshkey::generate(coolify_compat::sshkey::KeyKind::Ed25519).unwrap();
    let key = add_key(&h, &pem).await;
    let (_, uuid) = add_server(&h, &key, "203.0.113.30", "root").await;
    let pin = dir
        .path()
        .canonicalize()
        .unwrap()
        .join("data/ssh/known_hosts")
        .join(&uuid);

    std::fs::write(&pin, "x").unwrap();
    let r = h
        .call(
            "PATCH",
            &format!("/api/v1/servers/{uuid}"),
            W,
            Some(json!({"name": "renamed"})),
        )
        .await;
    assert_eq!(r.status, 201, "{}", r.body);
    assert!(pin.exists(), "a rename keeps the pin");
    // v4.7.5: the pinned key is in `server_settings`, with a known_hosts file named by its hash.
    sqlx::query(
        "UPDATE server_settings SET ssh_host_key = 'ssh-ed25519 AAAA', ssh_host_key_source = 'tofu', \
         ssh_host_key_pending = 'ssh-ed25519 BBBB' WHERE server_id = (SELECT id FROM servers WHERE uuid = $1)",
    )
    .bind(&uuid)
    .execute(h.tdb.db.pool())
    .await
    .unwrap();
    let hashed = pin.with_file_name(format!("known_hosts_{uuid}_0123456789abcdef"));
    std::fs::write(&hashed, "x").unwrap();
    let r = h
        .call(
            "PATCH",
            &format!("/api/v1/servers/{uuid}"),
            W,
            Some(json!({"description": "same address"})),
        )
        .await;
    assert_eq!(r.status, 201, "{}", r.body);
    async fn pinned(h: &Harness, uuid: String) -> (Option<String>, Option<String>, Option<String>) {
        sqlx::query_as(
            "SELECT ssh_host_key, ssh_host_key_source::text, ssh_host_key_pending FROM server_settings \
             WHERE server_id = (SELECT id FROM servers WHERE uuid = $1)",
        )
        .bind(uuid)
        .fetch_one(h.tdb.db.pool())
        .await
        .unwrap()
    }
    assert_eq!(
        pinned(&h, uuid.clone()).await.0.as_deref(),
        Some("ssh-ed25519 AAAA"),
        "another change keeps the pinned key"
    );
    let r = h
        .call(
            "PATCH",
            &format!("/api/v1/servers/{uuid}"),
            W,
            Some(json!({"port": 2200})),
        )
        .await;
    assert_eq!(r.status, 201, "{}", r.body);
    assert!(!pin.exists());
    // `Server::saved` -> `forgetSshHostKey('address_changed')`: the pinned and the pending key go.
    assert_eq!(pinned(&h, uuid.clone()).await, (None, None, None));
    assert!(!hashed.exists());
    let forgotten: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_events WHERE event = 'server.ssh_host_key.forgotten'",
    )
    .fetch_one(h.tdb.db.pool())
    .await
    .unwrap();
    assert_eq!(forgotten, 1);

    std::fs::write(&pin, "x").unwrap();
    let r = h
        .call("DELETE", &format!("/api/v1/servers/{uuid}"), DEL, None)
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert!(!pin.exists());
    h.finish().await;
}

#[tokio::test]
async fn live_matrix_against_the_test_host() {
    let (Ok(addr), Ok(key_path)) = (
        std::env::var("COOLIFY_RS_TEST_SSH"),
        std::env::var("COOLIFY_RS_TEST_SSH_KEY"),
    ) else {
        return;
    };
    let (host, port) = addr.rsplit_once(':').unwrap();
    let (_dir, remote) = remote_with(None);
    let h = Harness::with_remote(remote).await;
    let key = add_key(&h, &std::fs::read_to_string(key_path).unwrap()).await;

    let ok = |docker: &str| Outcome {
        logs: None,
        reachable: true,
        usable: true,
        docker: Some(docker.into()),
        compose: Some("2.38.2".into()),
    };
    let cases: Vec<(&str, Outcome)> = vec![
        ("root", ok("28.0.1")),
        ("deploy", ok("28.0.1")),
        (
            "nojq",
            Outcome {
                logs: Some("Prerequisites (jq) are not installed. Please install them before continuing or use the validation with installation endpoint.".into()),
                reachable: true,
                usable: false,
                docker: None,
                compose: None,
            },
        ),
        (
            "nodocker",
            Outcome {
                logs: Some(format!("Docker Engine is not installed. Please install Docker manually before continuing: {DOCS}.")),
                reachable: true,
                usable: false,
                docker: None,
                compose: None,
            },
        ),
        (
            "olddocker",
            Outcome {
                logs: Some("Docker Engine 20.10.7 is installed, but Coolify needs version 24 or newer. Please upgrade Docker: <a target=\"_blank\" class=\"underline\" href=\"https://docs.docker.com/engine/install/#server\" rel=\"noreferrer noopener\">documentation</a>.".into()),
                reachable: true,
                usable: false,
                docker: Some("20.10.7".into()),
                compose: Some("2.38.2".into()),
            },
        ),
        (
            "badjson",
            Outcome {
                logs: Some("Coolify could not read the Docker Engine version. Run \"sudo docker version --format json\" on the server as the badjson user. It must exit without an error and print only JSON. Text from shell startup files (for example ~/.bashrc) breaks the output.".into()),
                reachable: true,
                usable: false,
                docker: None,
                compose: Some("2.38.2".into()),
            },
        ),
        (
            "gentoo",
            Outcome {
                logs: Some(format!("Server OS type is not supported. Please install Docker manually before continuing: {DOCS}.")),
                reachable: true,
                usable: false,
                docker: None,
                compose: None,
            },
        ),
    ];
    for (i, (user, expected)) in cases.into_iter().enumerate() {
        let (id, uuid) = add_server(&h, &key, &format!("203.0.113.{}", 100 + i), user).await;
        // The API refuses duplicate addresses, so point each server at the host afterwards.
        sqlx::query("UPDATE servers SET ip = $1, port = $2 WHERE id = $3")
            .bind(host)
            .bind(port.parse::<i32>().unwrap())
            .bind(id)
            .execute(h.tdb.db.pool())
            .await
            .unwrap();
        validate_and_wait(&h, id, &uuid).await;
        assert_eq!(outcome(&h, id).await, expected, "user {user}");
    }

    // Wrong port: one attempt, OpenSSH's message, escaped.
    let (id, uuid) = add_server(&h, &key, "203.0.113.120", "root").await;
    sqlx::query("UPDATE servers SET ip = '127.0.0.1', port = 1 WHERE id = $1")
        .bind(id)
        .execute(h.tdb.db.pool())
        .await
        .unwrap();
    validate_and_wait(&h, id, &uuid).await;
    assert_eq!(
        outcome(&h, id).await.logs,
        Some(unreachable(
            "ssh: connect to host 127.0.0.1 port 1: Connection refused"
        ))
    );
    h.finish().await;
}
