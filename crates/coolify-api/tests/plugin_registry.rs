//! Plugin registry endpoints end to end (ADR 0010, section 7): the catalogue, refresh, install with an
//! explicit grant, updates that wait for approval, and revocation, against a signed index served locally.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use chrono::Utc;
use common::Harness;
use coolify_plugins::registry::index::{Build, Entry, FORMAT, Index, Release, Revoked};
use coolify_plugins::registry::signing::{self, SecretKeyFile};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const ID: &str = "coolify-rs/deploy-stats";
const ADMIN: Option<&str> = Some("root_root");
const CATALOG: &str = "/api/v1/plugins/catalog";
const REFRESH: &str = "/api/v1/plugins/catalog/refresh";
const INSTALL: &str = "/api/v1/plugins/catalog/install";
const UPDATE: &str = "/api/v1/plugins/coolify-rs/deploy-stats/update";

fn plugins() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugins")
}

fn component() -> &'static [u8] {
    static WASM: OnceLock<Vec<u8>> = OnceLock::new();
    WASM.get_or_init(|| {
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let status = std::process::Command::new(cargo)
            .args(["build", "-q", "--release", "--target", "wasm32-wasip2"])
            .current_dir(plugins())
            .status()
            .unwrap();
        assert!(status.success(), "building plugins/ failed");
        std::fs::read(plugins().join("target/wasm32-wasip2/release/deploy_stats.wasm")).unwrap()
    })
}

fn dev_key() -> SecretKeyFile {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../coolify-plugins/tests/fixtures/registry-dev.key");
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

/// A registry served from memory over plain HTTP on a local port.
#[derive(Clone)]
struct Registry {
    files: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    port: u16,
}

impl Registry {
    async fn start() -> Registry {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let files: Arc<Mutex<HashMap<String, Vec<u8>>>> = Arc::default();
        let served = files.clone();
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                let files = served.clone();
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 4096];
                    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                        match sock.read(&mut chunk).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => buf.extend_from_slice(&chunk[..n]),
                        }
                    }
                    let head = String::from_utf8_lossy(&buf);
                    let path = head.split_whitespace().nth(1).unwrap_or("/").to_owned();
                    let body = files.lock().unwrap().get(&path).cloned();
                    let (status, body) = match body {
                        Some(b) => ("200 OK", b),
                        None => ("404 Not Found", b"not found".to_vec()),
                    };
                    let head = format!(
                        "HTTP/1.1 {status}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = sock.write_all(head.as_bytes()).await;
                    let _ = sock.write_all(&body).await;
                });
            }
        });
        Registry { files, port }
    }

    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }

    fn put(&self, path: &str, bytes: Vec<u8>) {
        self.files.lock().unwrap().insert(path.to_owned(), bytes);
    }

    fn publish(&self, versions: Vec<Release>, revoked: Option<&str>) {
        static TICK: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);
        let n = TICK.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let index = Index {
            format: FORMAT,
            generated_at: Utc::now() + chrono::Duration::seconds(n),
            plugins: vec![Entry {
                id: ID.into(),
                revoked: revoked.map(|r| Revoked { reason: r.into() }),
                versions,
            }],
        };
        let bytes = index.to_bytes();
        self.put(
            "/dist/index.json.sig",
            signing::sign(&bytes, &dev_key()).unwrap(),
        );
        self.put("/dist/index.json", bytes);
    }

    fn release(&self, version: &str, events: &[&str]) -> Release {
        let path = format!("/c/{version}.wasm");
        self.put(&path, component().to_vec());
        Release {
            version: version.into(),
            source: "https://github.com/coolify-rs/coolify-rs".into(),
            commit: "0123456789abcdef0123456789abcdef01234567".into(),
            toolchain: "rust 1.99.1".into(),
            builder: format!("docker.io/library/rust:1.99.1@sha256:{}", "a".repeat(64)),
            build: Build {
                dir: "plugins".into(),
                command: "cargo build --release --locked --target wasm32-wasip2".into(),
                artifact: "target/wasm32-wasip2/release/deploy_stats.wasm".into(),
            },
            sha256: format!("{:x}", Sha256::digest(component())),
            url: self.url(&path),
            yanked: false,
            manifest: json!({
                "id": ID,
                "name": "Deploy stats",
                "version": version,
                "description": "Counts deployments.",
                "license": "Apache-2.0",
                "api": "0.1",
                "capabilities": {"events": events, "storage": true, "routes": true},
            }),
        }
    }
}

const V1: &[&str] = &["deployment.succeeded"];
const V2: &[&str] = &["deployment.succeeded", "deployment.failed"];

fn grant(events: &[&str]) -> Value {
    json!([
        format!("events: {}", events.join(", ")),
        "storage",
        "routes"
    ])
}

#[tokio::test]
async fn catalogue_and_install_need_an_instance_admin_and_a_grant() {
    let off = Harness::new().await;
    off.call("GET", CATALOG, ADMIN, None).await.assert(
        404,
        r#"{"message":"Plugins are switched off on this instance."}"#,
    );
    off.tdb.drop_db().await;

    let registry = Registry::start().await;
    let h = Harness::with_plugin_registry(false, registry.url("/dist")).await;
    h.call("GET", CATALOG, Some("acme_owner_read"), None)
        .await
        .assert(200, r#"{"index":null,"plugins":[]}"#);
    h.call("POST", INSTALL, ADMIN, Some(json!({"id": ID})))
        .await
        .assert(
            409,
            r#"{"message":"the plugin registry has not been fetched yet; refresh the catalogue first"}"#,
        );

    registry.publish(vec![registry.release("0.1.0", V1)], None);
    for token in ["acme_owner_root", "root_read"] {
        let r = h.call("POST", REFRESH, Some(token), None).await;
        assert_eq!(r.status, 403, "{token}: {}", r.body);
    }
    let r = h.call("POST", REFRESH, ADMIN, None).await;
    assert_eq!(r.status, 200, "{}", r.body);
    let refreshed = r.json();
    assert_eq!(refreshed["plugins"], 1);
    assert_eq!(refreshed["key_id"], signing::DEV_KEY_ID);
    assert_eq!(refreshed["revoked"], json!([]));

    // Any token with read sees the catalogue.
    let r = h.call("GET", CATALOG, Some("acme_member_read"), None).await;
    assert_eq!(r.status, 200, "{}", r.body);
    let c = r.json();
    assert_eq!(c["index"]["key_id"], signing::DEV_KEY_ID);
    let e = &c["plugins"][0];
    assert_eq!(e["id"], ID);
    assert_eq!(e["latest_version"], "0.1.0");
    assert_eq!(e["capabilities"], grant(V1));
    assert_eq!(e["verified"], true);
    assert_eq!(e["installed_version"], Value::Null);
    assert_eq!(e["update_available"], false);
    assert_eq!(e["pending_update"], Value::Null);
    assert_eq!(e["revoked"], Value::Null);
    assert_eq!(e["versions"][0]["version"], "0.1.0");
    assert_eq!(e["versions"][0]["yanked"], false);

    let r = h
        .call(
            "POST",
            INSTALL,
            Some("acme_owner_root"),
            Some(json!({"id": ID})),
        )
        .await;
    assert_eq!(r.status, 403, "{}", r.body);
    let r = h
        .call("POST", INSTALL, ADMIN, Some(json!({"id": ID})))
        .await;
    assert_eq!(r.status, 422, "{}", r.body);
    let need = r.json();
    assert_eq!(need["capabilities"], grant(V1));
    assert_eq!(need["version"], "0.1.0");
    assert!(
        coolify_plugins::store::find(&h.tdb.db, ID)
            .await
            .unwrap()
            .is_none()
    );

    let r = h
        .call(
            "POST",
            INSTALL,
            ADMIN,
            Some(json!({"id": ID, "grant": grant(V1)})),
        )
        .await;
    assert_eq!(r.status, 201, "{}", r.body);
    let p = r.json();
    assert_eq!(p["origin"], "registry");
    assert_eq!(p["verified"], true);
    assert_eq!(p["version"], "0.1.0");

    let r = h
        .call(
            "POST",
            INSTALL,
            ADMIN,
            Some(json!({"id": ID, "grant": grant(V1)})),
        )
        .await;
    assert_eq!(r.status, 409, "{}", r.body);
    let r = h
        .call("POST", INSTALL, ADMIN, Some(json!({"id": "acme/nothing"})))
        .await;
    assert_eq!(r.status, 404, "{}", r.body);
    let c = h.call("GET", CATALOG, ADMIN, None).await.json();
    assert_eq!(c["plugins"][0]["installed_version"], "0.1.0");
    h.tdb.drop_db().await;
}

#[tokio::test]
async fn updates_wait_for_approval_and_revocation_switches_off() {
    let registry = Registry::start().await;
    let h = Harness::with_plugin_registry(false, registry.url("/dist")).await;
    let v1 = registry.release("0.1.0", V1);
    registry.publish(vec![v1.clone()], None);
    h.call("POST", REFRESH, ADMIN, None).await;
    let r = h
        .call(
            "POST",
            INSTALL,
            ADMIN,
            Some(json!({"id": ID, "grant": grant(V1)})),
        )
        .await;
    assert_eq!(r.status, 201, "{}", r.body);

    let r = h.call("POST", UPDATE, ADMIN, None).await;
    assert_eq!(r.status, 409, "{}", r.body);

    // A version that asks for one more event kind.
    let v2 = registry.release("0.2.0", V2);
    registry.publish(vec![v1.clone(), v2.clone()], None);
    let refreshed = h.call("POST", REFRESH, ADMIN, None).await.json();
    assert_eq!(refreshed["pending_updates"], json!([ID]));
    let e = &h.call("GET", CATALOG, ADMIN, None).await.json()["plugins"][0];
    assert_eq!(e["update_available"], true);
    assert_eq!(e["update_requires_approval"], true);
    assert_eq!(e["pending_update"]["version"], "0.2.0");
    assert_eq!(
        e["pending_update"]["added"],
        json!(["events: deployment.failed"])
    );
    assert_eq!(e["pending_update"]["capabilities"], grant(V2));

    let r = h.call("POST", UPDATE, Some("acme_owner_root"), None).await;
    assert_eq!(r.status, 403, "{}", r.body);
    let r = h.call("POST", UPDATE, ADMIN, Some(json!({}))).await;
    assert_eq!(r.status, 422, "{}", r.body);
    let need = r.json();
    assert_eq!(need["added"], json!(["events: deployment.failed"]));
    assert_eq!(need["capabilities"], grant(V2));
    let installed = h
        .call(
            "GET",
            "/api/v1/plugins/coolify-rs/deploy-stats",
            ADMIN,
            None,
        )
        .await;
    assert_eq!(installed.json()["version"], "0.1.0");

    let r = h
        .call("POST", UPDATE, ADMIN, Some(json!({"grant": grant(V2)})))
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.json()["version"], "0.2.0");
    assert_eq!(r.json()["capabilities"], grant(V2));
    let e = &h.call("GET", CATALOG, ADMIN, None).await.json()["plugins"][0];
    assert_eq!(e["pending_update"], Value::Null);
    assert_eq!(e["installed_version"], "0.2.0");

    // Revoked: switched off with the reason, and it stays off.
    registry.publish(vec![v1, v2], Some("leaks deployment URLs"));
    let refreshed = h.call("POST", REFRESH, ADMIN, None).await.json();
    assert_eq!(refreshed["revoked"], json!([ID]));
    let p = h
        .call(
            "GET",
            "/api/v1/plugins/coolify-rs/deploy-stats",
            ADMIN,
            None,
        )
        .await
        .json();
    assert_eq!(p["enabled"], false);
    assert_eq!(
        p["disabled_reason"],
        "revoked by the plugin registry: leaks deployment URLs"
    );
    let r = h
        .call(
            "PATCH",
            "/api/v1/plugins/coolify-rs/deploy-stats",
            ADMIN,
            Some(json!({"enabled": true})),
        )
        .await;
    assert_eq!(r.status, 422, "{}", r.body);
    assert!(r.body.contains("revoked"), "{}", r.body);
    let e = &h.call("GET", CATALOG, ADMIN, None).await.json()["plugins"][0];
    assert_eq!(e["revoked"]["reason"], "leaks deployment URLs");
    let r = h
        .call("POST", UPDATE, ADMIN, Some(json!({"grant": grant(V2)})))
        .await;
    assert_eq!(r.status, 422, "{}", r.body);
    assert_eq!(r.json()["reason"], "leaks deployment URLs");
    h.tdb.drop_db().await;
}

#[tokio::test]
async fn tampered_downloads_are_bad_gateway() {
    let registry = Registry::start().await;
    let h = Harness::with_plugin_registry(false, registry.url("/dist")).await;
    let mut v1 = registry.release("0.1.0", V1);
    v1.sha256 = "0".repeat(64);
    registry.publish(vec![v1], None);
    h.call("POST", REFRESH, ADMIN, None).await;
    let r = h
        .call(
            "POST",
            INSTALL,
            ADMIN,
            Some(json!({"id": ID, "grant": grant(V1)})),
        )
        .await;
    assert_eq!(r.status, 502, "{}", r.body);
    assert!(r.body.contains("does not match the registry"), "{}", r.body);

    // An index with a broken signature is refused, and the catalogue keeps the last good one.
    registry.put("/dist/index.json.sig", b"{}".to_vec());
    let r = h.call("POST", REFRESH, ADMIN, None).await;
    assert_eq!(r.status, 502, "{}", r.body);
    let c = h.call("GET", CATALOG, ADMIN, None).await.json();
    assert_eq!(c["plugins"][0]["id"], ID);
    h.tdb.drop_db().await;
}
