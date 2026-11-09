//! The registry client against a local registry: a signed index and components served over HTTP (local
//! addresses are only allowed in tests). Covers installing, integrity, signatures, yanking, revocation,
//! updates that need approval, and the conformance check.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use chrono::{Duration as Days, Utc};
use coolify_db::testing::TestDb;
use coolify_plugins::egress::Egress;
use coolify_plugins::registry::index::{Build, Entry, FORMAT, Index, Release, Revoked};
use coolify_plugins::registry::signing::{self, SecretKeyFile, TrustedKey};
use coolify_plugins::registry::{self, Config, RegistryError, conformance};
use coolify_plugins::{CallError, Caller, Host, Limits, Request, Settings};
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const ID: &str = "coolify-rs/deploy-stats";

fn plugins_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugins")
}

fn wasm(name: &str) -> Vec<u8> {
    static BUILT: OnceLock<()> = OnceLock::new();
    BUILT.get_or_init(|| {
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let status = std::process::Command::new(cargo)
            .args(["build", "-q", "--release", "--target", "wasm32-wasip2"])
            .current_dir(plugins_dir())
            .status()
            .unwrap_or_else(|e| panic!("cargo: {e}"));
        assert!(status.success(), "building plugins/ failed");
    });
    let path = plugins_dir().join(format!("target/wasm32-wasip2/release/{name}.wasm"));
    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn dev_secret() -> SecretKeyFile {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/registry-dev.key");
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

/// A registry served from memory over plain HTTP on a local port.
#[derive(Clone)]
struct Server {
    files: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    port: u16,
}

impl Server {
    async fn start() -> Server {
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
        Server { files, port }
    }

    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }

    fn put(&self, path: &str, bytes: Vec<u8>) {
        self.files.lock().unwrap().insert(path.to_owned(), bytes);
    }

    /// Publishes `index`, signed with `key`, as the registry's CI does.
    fn publish(&self, index: &Index, key: &SecretKeyFile) {
        let bytes = index.to_bytes();
        let sig = signing::sign(&bytes, key).unwrap();
        self.put("/dist/index.json", bytes);
        self.put("/dist/index.json.sig", sig);
    }

    /// Serves `component` and returns the release that names it.
    fn release(&self, version: &str, component: &[u8], caps: serde_json::Value) -> Release {
        let path = format!("/components/deploy-stats-{version}.wasm");
        self.put(&path, component.to_vec());
        Release {
            version: version.into(),
            source: "https://github.com/coolify-rs/coolify-rs".into(),
            commit: "0123456789abcdef0123456789abcdef01234567".into(),
            toolchain: "rust 1.99.1".into(),
            builder: format!("docker.io/library/rust:1.99.1@sha256:{}", "a".repeat(64)),
            build: Build {
                dir: "plugins".into(),
                command: "cargo build --release --locked --target wasm32-wasip2 -p deploy-stats"
                    .into(),
                artifact: "target/wasm32-wasip2/release/deploy_stats.wasm".into(),
            },
            sha256: hex::encode(Sha256::digest(component)),
            url: self.url(&path),
            yanked: false,
            manifest: json!({
                "id": ID,
                "name": "Deploy stats",
                "version": version,
                "description": "Counts deployments.",
                "license": "Apache-2.0",
                "api": "0.1",
                "capabilities": caps,
            }),
        }
    }
}

/// Each call is a second later, so successive indexes are never older than the last.
fn index(versions: Vec<Release>, revoked: Option<&str>) -> Index {
    static TICK: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);
    let n = TICK.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    Index {
        format: FORMAT,
        generated_at: Utc::now() + chrono::Duration::seconds(n),
        plugins: vec![Entry {
            id: ID.into(),
            revoked: revoked.map(|r| Revoked { reason: r.into() }),
            versions,
        }],
    }
}

fn caps_v1() -> serde_json::Value {
    json!({"events": ["deployment.succeeded"], "storage": true, "routes": true})
}

fn caps_v2() -> serde_json::Value {
    json!({"events": ["deployment.succeeded", "deployment.failed"], "storage": true, "routes": true})
}

fn grant(caps: &serde_json::Value) -> Vec<String> {
    let m: coolify_plugins::Capabilities = serde_json::from_value(caps.clone()).unwrap();
    coolify_plugins::Manifest {
        id: ID.into(),
        name: "x".into(),
        version: "0.0.0".into(),
        description: "x".into(),
        license: "MIT".into(),
        source: None,
        api: "0.1".into(),
        capabilities: m,
        settings: Vec::new(),
    }
    .capability_summary()
}

async fn host_with_keys(db: &TestDb, server: &Server, keys: Vec<TrustedKey>) -> Host {
    Host::start(
        db.db.clone(),
        Settings {
            egress: Egress {
                allow_local: true,
                ..Egress::default()
            },
            registry: Config {
                url: server.url("/dist"),
                keys,
            },
            ..Settings::default()
        },
    )
    .await
    .unwrap()
}

async fn host(db: &TestDb, server: &Server) -> Host {
    host_with_keys(db, server, vec![signing::development_key()]).await
}

fn stats() -> Request {
    Request {
        method: "GET".into(),
        path: "/stats".into(),
        query: String::new(),
        headers: vec![],
        body: vec![],
    }
}

fn caller() -> Caller {
    Caller {
        team_id: 1,
        user_id: 1,
        abilities: vec!["read".into()],
    }
}

#[test]
fn the_development_key_fixture_matches_the_public_key() {
    let public = signing::public_half(&dev_secret(), "2099-12-31".parse().unwrap()).unwrap();
    assert_eq!(public.trusted().unwrap(), signing::development_key());
}

#[tokio::test]
async fn installs_verified_components_from_a_signed_index() {
    let server = Server::start().await;
    let tdb = TestDb::new("").await;
    let host = host(&tdb, &server).await;
    let component = wasm("deploy_stats");

    let err = registry::install(&host, ID, None, None, Some(1))
        .await
        .unwrap_err();
    assert!(matches!(err, RegistryError::NoIndex), "{err}");

    server.publish(
        &index(vec![server.release("0.1.0", &component, caps_v1())], None),
        &dev_secret(),
    );
    let refreshed = registry::refresh(&host).await.unwrap();
    assert_eq!(refreshed.plugins, 1);
    assert_eq!(refreshed.key_id, signing::DEV_KEY_ID);

    // Without a grant nothing is downloaded, and the error lists what to approve.
    let err = registry::install(&host, ID, None, None, Some(1))
        .await
        .unwrap_err();
    match err {
        RegistryError::GrantRequired {
            version,
            capabilities,
            ..
        } => {
            assert_eq!(version, "0.1.0");
            assert_eq!(capabilities, grant(&caps_v1()));
        }
        e => panic!("{e}"),
    }
    let wrong = vec!["storage".to_owned()];
    assert!(matches!(
        registry::install(&host, ID, None, Some(&wrong), Some(1))
            .await
            .unwrap_err(),
        RegistryError::GrantRequired { .. }
    ));

    let installed = registry::install(&host, ID, None, Some(&grant(&caps_v1())), Some(1))
        .await
        .unwrap();
    assert_eq!(installed.origin, "registry");
    assert!(installed.verified);
    assert_eq!(installed.version, "0.1.0");
    host.set_team(ID, 1, true, Some(1)).await.unwrap();
    let r = host.handle_request(ID, caller(), stats()).await.unwrap();
    assert_eq!(r.status, 200);

    let again = registry::install(&host, ID, None, Some(&grant(&caps_v1())), Some(1)).await;
    assert!(matches!(
        again.unwrap_err(),
        RegistryError::AlreadyInstalled(_)
    ));
    let unknown = registry::install(&host, "acme/nothing", None, None, None).await;
    assert!(matches!(unknown.unwrap_err(), RegistryError::Unknown(_)));

    let catalog = registry::catalog(&host).await.unwrap();
    assert_eq!(catalog.index.unwrap().key_id, signing::DEV_KEY_ID);
    let e = &catalog.plugins[0];
    assert_eq!(e.installed_version.as_deref(), Some("0.1.0"));
    assert_eq!(e.latest_version.as_deref(), Some("0.1.0"));
    assert!(!e.update_available);
    tdb.drop_db().await;
}

#[tokio::test]
async fn components_must_match_the_index() {
    let server = Server::start().await;
    let tdb = TestDb::new("").await;
    let host = host(&tdb, &server).await;
    let component = wasm("deploy_stats");
    let release = server.release("0.1.0", &component, caps_v1());
    // The server now hands out other bytes than the index names.
    let mut swapped = component.clone();
    swapped.extend_from_slice(b"\0trailing");
    server.put("/components/deploy-stats-0.1.0.wasm", swapped);
    server.publish(&index(vec![release], None), &dev_secret());
    registry::refresh(&host).await.unwrap();
    let err = registry::install(&host, ID, None, Some(&grant(&caps_v1())), None)
        .await
        .unwrap_err();
    assert!(matches!(err, RegistryError::Integrity { .. }), "{err}");
    assert!(
        coolify_plugins::store::find(host.db(), ID)
            .await
            .unwrap()
            .is_none()
    );
    tdb.drop_db().await;
}

#[tokio::test]
async fn only_trusted_current_signatures_of_newer_indexes_are_accepted() {
    let server = Server::start().await;
    let tdb = TestDb::new("").await;
    let component = wasm("deploy_stats");
    let release = server.release("0.1.0", &component, caps_v1());

    // Tampered after signing.
    let first = index(vec![release.clone()], None);
    server.publish(&first, &dev_secret());
    let mut bytes = first.to_bytes();
    let at = bytes.len() - 3;
    bytes[at] = b' ';
    server.put("/dist/index.json", bytes);
    let host = host(&tdb, &server).await;
    let err = registry::refresh(&host).await.unwrap_err();
    assert!(err.to_string().contains("does not match"), "{err}");

    // Signed by a key nobody trusts.
    let (stranger, _) = signing::generate("stranger", "2099-01-01".parse().unwrap()).unwrap();
    server.publish(&first, &stranger);
    let err = registry::refresh(&host).await.unwrap_err();
    assert!(err.to_string().contains("does not trust"), "{err}");

    // Signed by a trusted key that has expired.
    let yesterday = (Utc::now() - Days::days(1)).date_naive();
    let (old, old_public) = signing::generate("old", yesterday).unwrap();
    let expired = host_with_keys(&tdb, &server, vec![old_public.trusted().unwrap()]).await;
    server.publish(&first, &old);
    let err = registry::refresh(&expired).await.unwrap_err();
    assert!(err.to_string().contains("expired"), "{err}");

    // A valid index, then an older one: the older one is refused.
    let newer = index(vec![release], None);
    server.publish(&newer, &dev_secret());
    registry::refresh(&host).await.unwrap();
    server.publish(&first, &dev_secret());
    let err = registry::refresh(&host).await.unwrap_err();
    assert!(err.to_string().contains("refusing to go back"), "{err}");
    tdb.drop_db().await;
}

#[tokio::test]
async fn yanked_versions_are_not_installed() {
    let server = Server::start().await;
    let tdb = TestDb::new("").await;
    let host = host(&tdb, &server).await;
    let component = wasm("deploy_stats");
    let mut yanked = server.release("0.2.0", &component, caps_v1());
    yanked.yanked = true;
    let good = server.release("0.1.0", &component, caps_v1());
    server.publish(&index(vec![good, yanked], None), &dev_secret());
    registry::refresh(&host).await.unwrap();

    let g = grant(&caps_v1());
    let err = registry::install(&host, ID, Some("0.2.0"), Some(&g), None)
        .await
        .unwrap_err();
    assert!(matches!(err, RegistryError::Yanked { .. }), "{err}");
    let err = registry::install(&host, ID, Some("9.9.9"), Some(&g), None)
        .await
        .unwrap_err();
    assert!(matches!(err, RegistryError::NoSuchVersion { .. }), "{err}");
    // The default skips the yanked version.
    let installed = registry::install(&host, ID, None, Some(&g), None)
        .await
        .unwrap();
    assert_eq!(installed.version, "0.1.0");
    let catalog = registry::catalog(&host).await.unwrap();
    assert!(catalog.plugins[0].versions[0].yanked);
    tdb.drop_db().await;
}

#[tokio::test]
async fn revocation_switches_plugins_off_everywhere() {
    let server = Server::start().await;
    let tdb = TestDb::new("").await;
    let host = host(&tdb, &server).await;
    let component = wasm("deploy_stats");
    let release = server.release("0.1.0", &component, caps_v1());
    server.publish(&index(vec![release.clone()], None), &dev_secret());
    registry::refresh(&host).await.unwrap();
    registry::install(&host, ID, None, Some(&grant(&caps_v1())), Some(1))
        .await
        .unwrap();
    host.set_team(ID, 1, true, Some(1)).await.unwrap();
    host.handle_request(ID, caller(), stats()).await.unwrap();

    server.publish(
        &index(vec![release.clone()], Some("sends tokens to a third party")),
        &dev_secret(),
    );
    let refreshed = registry::refresh(&host).await.unwrap();
    assert_eq!(refreshed.revoked, [ID]);

    let off = host
        .handle_request(ID, caller(), stats())
        .await
        .unwrap_err();
    assert!(matches!(off, CallError::Unavailable(_)), "{off}");
    let p = coolify_plugins::store::find(host.db(), ID)
        .await
        .unwrap()
        .unwrap();
    assert!(!p.enabled);
    assert_eq!(
        p.disabled_reason.as_deref(),
        Some("revoked by the plugin registry: sends tokens to a third party")
    );
    let audited: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM coolify_rs_plugins.audit WHERE plugin_id = $1 AND action = 'revoked'",
    )
    .bind(ID)
    .fetch_one(host.db().pool())
    .await
    .unwrap();
    assert_eq!(audited, 1);

    // It cannot be switched back on, reinstalled, uploaded again or updated while revoked.
    let err = host.set_enabled(ID, true, Some(1)).await.unwrap_err();
    assert!(err.to_string().contains("revoked"), "{err}");
    let manifest = release.manifest().unwrap();
    let err = host
        .install(manifest, &component, "upload", false, Some(1))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("revoked"), "{err}");
    let err = registry::update(&host, ID, None, None, Some(1))
        .await
        .unwrap_err();
    assert!(matches!(err, RegistryError::Revoked { .. }), "{err}");

    // A second refresh does not revoke it again; a lifted revocation lets the admin switch it on.
    let again = registry::refresh(&host).await.unwrap();
    assert!(again.revoked.is_empty());
    server.publish(&index(vec![release], None), &dev_secret());
    registry::refresh(&host).await.unwrap();
    host.set_enabled(ID, true, Some(1)).await.unwrap();
    host.handle_request(ID, caller(), stats()).await.unwrap();
    tdb.drop_db().await;
}

#[tokio::test]
async fn updates_that_ask_for_more_wait_for_approval() {
    let server = Server::start().await;
    let tdb = TestDb::new("").await;
    let host = host(&tdb, &server).await;
    let component = wasm("deploy_stats");
    let v010 = server.release("0.1.0", &component, caps_v1());
    server.publish(&index(vec![v010.clone()], None), &dev_secret());
    registry::refresh(&host).await.unwrap();
    registry::install(&host, ID, None, Some(&grant(&caps_v1())), Some(1))
        .await
        .unwrap();
    host.set_team(ID, 1, true, Some(1)).await.unwrap();

    // Same capabilities: the admin updates directly.
    let v011 = server.release("0.1.1", &component, caps_v1());
    server.publish(
        &index(vec![v010.clone(), v011.clone()], None),
        &dev_secret(),
    );
    let r = registry::refresh(&host).await.unwrap();
    assert!(r.pending_updates.is_empty());
    let catalog = registry::catalog(&host).await.unwrap();
    assert!(catalog.plugins[0].update_available);
    assert!(!catalog.plugins[0].update_requires_approval);
    let updated = registry::update(&host, ID, None, None, Some(1))
        .await
        .unwrap();
    assert_eq!(updated.version, "0.1.1");
    let err = registry::update(&host, ID, None, None, Some(1))
        .await
        .unwrap_err();
    assert!(matches!(err, RegistryError::UpToDate { .. }), "{err}");

    // More capabilities: a pending update, and the old version keeps running.
    let v020 = server.release("0.2.0", &component, caps_v2());
    server.publish(&index(vec![v010, v011, v020], None), &dev_secret());
    let r = registry::refresh(&host).await.unwrap();
    assert_eq!(r.pending_updates, [ID]);
    let catalog = registry::catalog(&host).await.unwrap();
    let e = &catalog.plugins[0];
    assert!(e.update_requires_approval);
    let pending = e.pending_update.as_ref().unwrap();
    assert_eq!(pending.version, "0.2.0");
    assert_eq!(pending.added, ["events: deployment.failed"]);

    let err = registry::update(&host, ID, None, None, Some(1))
        .await
        .unwrap_err();
    match err {
        RegistryError::GrantRequired {
            added,
            capabilities,
            ..
        } => {
            assert_eq!(added, ["events: deployment.failed"]);
            assert_eq!(capabilities, grant(&caps_v2()));
        }
        e => panic!("{e}"),
    }
    let p = coolify_plugins::store::find(host.db(), ID)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(p.version, "0.1.1");
    assert!(!p.granted.events.contains(&"deployment.failed".to_owned()));
    host.handle_request(ID, caller(), stats()).await.unwrap();

    // Approving with the new list installs it and clears the pending update.
    let updated = registry::update(&host, ID, None, Some(&grant(&caps_v2())), Some(1))
        .await
        .unwrap();
    assert_eq!(updated.version, "0.2.0");
    assert!(
        updated
            .granted
            .events
            .contains(&"deployment.failed".to_owned())
    );
    let catalog = registry::catalog(&host).await.unwrap();
    assert!(catalog.plugins[0].pending_update.is_none());
    assert!(host.handle_request(ID, caller(), stats()).await.is_ok());
    tdb.drop_db().await;
}

#[tokio::test]
async fn conformance_check_runs_without_a_database() {
    let manifest =
        std::fs::read_to_string(plugins_dir().join("examples/deploy-stats/plugin.toml")).unwrap();
    let report = conformance::check(&wasm("deploy_stats"), &manifest, Settings::default()).await;
    assert!(report.ok, "{report:#?}");
    let names: Vec<&str> = report.checks.iter().map(|c| c.name.as_str()).collect();
    assert!(names.contains(&"event deployment.succeeded"), "{names:?}");
    assert!(names.contains(&"event deployment.failed"), "{names:?}");
    assert!(
        names.iter().any(|n| n.starts_with("malformed event")),
        "{names:?}"
    );
    assert!(names.iter().any(|n| n.starts_with("request")), "{names:?}");
    assert!(report.max_memory > 0);
    assert!(
        report
            .imports
            .iter()
            .any(|i| i.starts_with("coolify-rs:plugin/storage"))
    );

    // Imports beyond the manifest fail before anything runs.
    let narrow = r#"
id = "test/hostile"
name = "Hostile"
version = "0.1.0"
description = "Asks for too little."
license = "MIT"
api = "0.1"
[capabilities]
events = ["deployment.succeeded"]
"#;
    let report = conformance::check(&wasm("hostile"), narrow, Settings::default()).await;
    assert!(!report.ok);
    assert!(
        report.errors[0].contains("coolify-rs:plugin/storage"),
        "{:?}",
        report.errors
    );

    // A plugin that cannot live within the limits fails.
    let tiny = Settings {
        limits: Limits {
            memory: 64 * 1024,
            ..Limits::default()
        },
        ..Settings::default()
    };
    let report = conformance::check(&wasm("deploy_stats"), &manifest, tiny).await;
    assert!(!report.ok, "{report:#?}");

    let report = conformance::check(b"not wasm", &manifest, Settings::default()).await;
    assert!(!report.ok);
    let report = conformance::check(&wasm("deploy_stats"), "id = 1", Settings::default()).await;
    assert!(!report.ok);
}

