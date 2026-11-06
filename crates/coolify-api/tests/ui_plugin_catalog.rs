//! The web UI's registry catalogue (`/ui/plugins/catalog...`, ADR 0010 section 7, ADR 0013): the same calls and
//! answers as the bearer API, behind the session and its CSRF token, against a signed index served locally.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::Utc;
use common::Harness;
use coolify_plugins::registry::index::{Build, Entry, FORMAT, Index, Release, Revoked};
use coolify_plugins::registry::signing::{self, SecretKeyFile};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tower::ServiceExt;

const ID: &str = "coolify-rs/deploy-stats";
const CATALOG: &str = "/ui/plugins/catalog";
const REFRESH: &str = "/ui/plugins/catalog/refresh";
const INSTALL: &str = "/ui/plugins/catalog/install";
const UPDATE: &str = "/ui/plugins/coolify-rs/deploy-stats/update";

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

/// A registry served from memory over plain HTTP on a local port, as in `plugin_registry.rs`.
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

/// A signed-in browser.
struct Browser {
    cookie: String,
    csrf: String,
}

impl Browser {
    async fn login(h: &Harness, email: &str) -> Self {
        let (cookie, csrf) = h.ui_login(email).await;
        Self { cookie, csrf }
    }

    async fn send(
        &self,
        h: &Harness,
        method: &str,
        path: &str,
        body: Option<Value>,
        csrf: bool,
    ) -> (StatusCode, Value) {
        let mut b = Request::builder()
            .method(method)
            .uri(path)
            .header("cookie", &self.cookie);
        if csrf {
            b = b.header("x-csrf-token", &self.csrf);
        }
        let req = match body {
            Some(v) => b
                .header("content-type", "application/json")
                .body(Body::from(v.to_string()))
                .unwrap(),
            None => b.body(Body::empty()).unwrap(),
        };
        let res = h.app.clone().oneshot(req).await.unwrap();
        let status = res.status();
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        let v = serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into()));
        (status, v)
    }

    async fn get(&self, h: &Harness, path: &str) -> (StatusCode, Value) {
        self.send(h, "GET", path, None, false).await
    }

    async fn post(&self, h: &Harness, path: &str, body: Value) -> (StatusCode, Value) {
        self.send(h, "POST", path, Some(body), true).await
    }
}

#[tokio::test]
async fn catalogue_install_update_and_revocation_through_the_session() {
    let registry = Registry::start().await;
    let h = Harness::with_plugin_registry(false, registry.url("/dist")).await;
    let root = Browser::login(&h, "root@example.test").await;
    let alice = Browser::login(&h, "alice@example.test").await;

    // Instance admins only; before the first refresh the page shows an empty catalogue with Refresh.
    let (s, nav) = root.get(&h, "/ui/plugins/nav").await;
    assert_eq!((s, &nav["settings"]), (StatusCode::OK, &json!(true)));
    assert_eq!(
        root.get(&h, CATALOG).await,
        (StatusCode::OK, json!({"index": null, "plugins": []}))
    );
    let (s, body) = alice.get(&h, CATALOG).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{body}");
    let (s, body) = root.post(&h, INSTALL, json!({"id": ID})).await;
    assert_eq!(s, StatusCode::CONFLICT, "{body}");
    assert!(
        body["message"].as_str().unwrap().contains("refresh"),
        "{body}"
    );

    // Refresh: CSRF, admins only, then the verified index.
    registry.publish(vec![registry.release("0.1.0", V1)], None);
    let (s, _) = root.send(&h, "POST", REFRESH, None, false).await;
    assert_eq!(s.as_u16(), 419);
    let (s, _) = alice.post(&h, REFRESH, json!({})).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, refreshed) = root.post(&h, REFRESH, json!({})).await;
    assert_eq!(s, StatusCode::OK, "{refreshed}");
    assert_eq!(refreshed["plugins"], 1);
    assert_eq!(refreshed["key_id"], signing::DEV_KEY_ID);
    let (_, c) = root.get(&h, CATALOG).await;
    assert_eq!(c["index"]["key_id"], signing::DEV_KEY_ID);
    let e = &c["plugins"][0];
    assert_eq!(
        (
            &e["id"],
            &e["latest_version"],
            &e["capabilities"],
            &e["installed_version"]
        ),
        (&json!(ID), &json!("0.1.0"), &grant(V1), &Value::Null)
    );

    // Install asks for the grant first, then installs a verified plugin.
    let (s, need) = root.post(&h, INSTALL, json!({"id": ID})).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{need}");
    assert_eq!(need["capabilities"], grant(V1));
    assert_eq!(need["version"], "0.1.0");
    assert_eq!(
        need["added"].as_array().unwrap().len(),
        3,
        "on install, everything is new"
    );
    let (s, p) = root
        .post(
            &h,
            INSTALL,
            json!({"id": ID, "version": "0.1.0", "grant": grant(V1)}),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{p}");
    assert_eq!(
        (&p["origin"], &p["verified"], &p["version"]),
        (&json!("registry"), &json!(true), &json!("0.1.0"))
    );
    assert_eq!(p["panels"], json!([]));
    let (s, again) = root
        .post(&h, INSTALL, json!({"id": ID, "grant": grant(V1)}))
        .await;
    assert_eq!(s, StatusCode::CONFLICT, "{again}");
    let (_, settings) = root.get(&h, "/ui/plugins").await;
    assert_eq!(settings["plugins"][0]["verified"], true);

    // An update that asks for more waits for approval of exactly what it adds.
    registry.publish(
        vec![registry.release("0.1.0", V1), registry.release("0.2.0", V2)],
        None,
    );
    let (_, refreshed) = root.post(&h, REFRESH, json!({})).await;
    assert_eq!(refreshed["pending_updates"], json!([ID]));
    let (_, c) = root.get(&h, CATALOG).await;
    assert_eq!(
        c["plugins"][0]["pending_update"]["added"],
        json!(["events: deployment.failed"])
    );
    let (s, need) = root.send(&h, "POST", UPDATE, None, true).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{need}");
    assert_eq!(need["added"], json!(["events: deployment.failed"]));
    assert_eq!(
        alice.post(&h, UPDATE, json!({})).await.0,
        StatusCode::FORBIDDEN
    );
    let (s, p) = root
        .post(&h, UPDATE, json!({"version": "0.2.0", "grant": grant(V2)}))
        .await;
    assert_eq!(s, StatusCode::OK, "{p}");
    assert_eq!(p["version"], "0.2.0");

    // Revocation switches it off with the registry's reason, which the settings page shows.
    registry.publish(
        vec![registry.release("0.1.0", V1), registry.release("0.2.0", V2)],
        Some("Sends data to a third party."),
    );
    let (_, refreshed) = root.post(&h, REFRESH, json!({})).await;
    assert_eq!(refreshed["revoked"], json!([ID]));
    let (_, settings) = root.get(&h, "/ui/plugins").await;
    let p = &settings["plugins"][0];
    assert_eq!(p["enabled"], false);
    assert!(
        p["disabled_reason"]
            .as_str()
            .unwrap()
            .contains("Sends data to a third party."),
        "{p}"
    );
    let (s, body) = root
        .send(
            &h,
            "PATCH",
            "/ui/plugins/coolify-rs/deploy-stats",
            Some(json!({"enabled": true})),
            true,
        )
        .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    h.finish().await;
}
