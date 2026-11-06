//! Real components against the real host: events, routes, storage, team isolation, and every limit of the
//! sandbox, using the example plugin and `plugins/testing/hostile`. The plugins are built from source with
//! the pinned toolchain (`wasm32-wasip2`) the first time a test needs them.

use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

use coolify_db::testing::TestDb;
use coolify_plugins::egress::Egress;
use coolify_plugins::{
    CallError, Caller, Capabilities, Event, Host, Limits, Manifest, Request, Settings,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

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

fn manifest(id: &str, caps: Capabilities) -> Manifest {
    Manifest {
        id: id.into(),
        name: "Test".into(),
        version: "0.1.0".into(),
        description: "A test plugin.".into(),
        license: "Apache-2.0".into(),
        source: None,
        api: coolify_plugins::API_VERSION.into(),
        capabilities: caps,
    }
}

fn hostile_caps() -> Capabilities {
    Capabilities {
        events: vec!["deployment.succeeded".into()],
        storage: true,
        routes: true,
        http: vec!["localhost".into()],
        resources: coolify_plugins::manifest::RESOURCE_KINDS
            .iter()
            .map(|k| (*k).to_owned())
            .collect(),
        deploy: true,
    }
}

fn event(team_id: i64, payload: serde_json::Value) -> Event {
    Event {
        kind: "deployment.succeeded".into(),
        version: 1,
        team_id,
        payload: payload.to_string(),
    }
}

fn caller(team_id: i64) -> Caller {
    Caller {
        team_id,
        user_id: 7,
        abilities: vec!["read".into()],
    }
}

fn get(path: &str) -> Request {
    Request {
        method: "GET".into(),
        path: path.into(),
        query: String::new(),
        headers: vec![],
        body: vec![],
    }
}

async fn host(db: &TestDb, limits: Limits, egress: Egress) -> Host {
    Host::start(db.db.clone(), Settings { limits, egress })
        .await
        .unwrap_or_else(|e| panic!("start: {e}"))
}

fn body(r: &coolify_plugins::Response) -> serde_json::Value {
    serde_json::from_slice(&r.body).unwrap_or_default()
}

#[tokio::test]
async fn delivers_events_and_requests_only_for_enabled_teams() {
    let tdb = TestDb::new("").await;
    let host = host(&tdb, Limits::default(), Egress::default()).await;
    let text =
        std::fs::read_to_string(plugins_dir().join("examples/deploy-stats/plugin.toml")).unwrap();
    let m = Manifest::parse(&text).unwrap();
    let id = m.id.clone();
    host.install(m, &wasm("deploy_stats"), "file", false, Some(1))
        .await
        .unwrap();
    host.set_team(&id, 1, true, Some(1)).await.unwrap();

    host.call_event(
        &id,
        event(1, serde_json::json!({"application_name": "web"})),
    )
    .await
    .unwrap();
    let err = host
        .call_event(&id, event(2, serde_json::json!({})))
        .await
        .unwrap_err();
    assert!(matches!(err, CallError::NotEnabledForTeam(_)), "{err}");

    // `dispatch` runs in the background and only delivers subscribed kinds.
    host.dispatch(
        1,
        "deployment.failed",
        serde_json::json!({"application_name": "api"}),
    );
    host.dispatch(1, "backup.succeeded", serde_json::json!({}));
    let mut stats = serde_json::Value::Null;
    for _ in 0..100 {
        stats = body(
            &host
                .handle_request(&id, caller(1), get("/stats"))
                .await
                .unwrap(),
        );
        if stats["failed"] == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(stats["succeeded"], 1, "{stats}");
    assert_eq!(stats["failed"], 1, "{stats}");
    assert_eq!(stats["last"]["application"], "api", "{stats}");

    let other = host
        .handle_request(&id, caller(2), get("/stats"))
        .await
        .unwrap_err();
    assert!(matches!(other, CallError::NotEnabledForTeam(_)), "{other}");
    let missing = host
        .handle_request(&id, caller(1), get("/nope"))
        .await
        .unwrap();
    assert_eq!(missing.status, 404);

    // Switched off instance-wide, nothing runs; back on, it does again.
    host.set_enabled(&id, false, Some(1)).await.unwrap();
    let off = host
        .handle_request(&id, caller(1), get("/stats"))
        .await
        .unwrap_err();
    assert!(matches!(off, CallError::Unavailable(_)), "{off}");
    host.set_enabled(&id, true, Some(1)).await.unwrap();
    host.handle_request(&id, caller(1), get("/stats"))
        .await
        .unwrap();

    // A restarted host loads it from the database.
    let again = self::host(&tdb, Limits::default(), Egress::default()).await;
    assert_eq!(again.loaded_ids(), std::slice::from_ref(&id));

    assert!(host.uninstall(&id, Some(1)).await.unwrap());
    tdb.drop_db().await;
}

#[tokio::test]
async fn refuses_components_that_import_more_than_granted() {
    let tdb = TestDb::new("").await;
    let host = host(&tdb, Limits::default(), Egress::default()).await;
    let m = manifest(
        "test/hostile",
        Capabilities {
            events: vec!["deployment.succeeded".into()],
            ..Default::default()
        },
    );
    let err = host
        .install(m, &wasm("hostile"), "file", false, None)
        .await
        .unwrap_err()
        .to_string();
    for interface in ["storage", "http", "resources", "deploy"] {
        assert!(
            err.contains(&format!("coolify-rs:plugin/{interface}@0.1.")),
            "{err}"
        );
    }
    assert!(
        coolify_plugins::store::find(host.db(), "test/hostile")
            .await
            .unwrap()
            .is_none()
    );

    let garbage = host
        .install(
            manifest("test/garbage", Capabilities::default()),
            b"\0asm not really",
            "file",
            false,
            None,
        )
        .await
        .unwrap_err();
    assert!(
        garbage.to_string().contains("invalid component"),
        "{garbage}"
    );
    tdb.drop_db().await;
}

#[tokio::test]
async fn sandbox_limits_hold() {
    let tdb = TestDb::new("").await;
    let limits = Limits {
        memory: 16 * 1024 * 1024,
        event_deadline: Duration::from_millis(500),
        storage_quota: 4096,
        max_failures: 3,
        ..Limits::default()
    };
    let host = host(&tdb, limits, Egress::default()).await;
    let id = "test/hostile";
    host.install(
        manifest(id, hostile_caps()),
        &wasm("hostile"),
        "file",
        false,
        None,
    )
    .await
    .unwrap();
    host.set_team(id, 1, true, None).await.unwrap();
    let run = |what: serde_json::Value| host.call_event(id, event(1, what));

    // No files and no environment, even though the component imports WASI filesystem.
    run(serde_json::json!({"do": "read-file"})).await.unwrap();
    run(serde_json::json!({"do": "read-env"})).await.unwrap();
    let keys = body(&host.handle_request(id, caller(1), get("/")).await.unwrap());
    assert_eq!(keys["keys"], serde_json::json!(["env", "file"]), "{keys}");

    // Storage quota and value checks are errors for the plugin, not failures of the sandbox.
    let over = run(serde_json::json!({"do": "store", "key": "big", "size": 5000}))
        .await
        .unwrap_err();
    assert!(over.to_string().contains("limited to 4096 bytes"), "{over}");

    // A guest that never yields is stopped at the deadline.
    let started = std::time::Instant::now();
    let looped = run(serde_json::json!({"do": "loop"})).await.unwrap_err();
    assert!(matches!(looped, CallError::Timeout(..)), "{looped}");
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "{:?}",
        started.elapsed()
    );

    // Memory is capped, and a panic is a trap.
    let oom = run(serde_json::json!({"do": "alloc"})).await.unwrap_err();
    assert!(matches!(oom, CallError::Trap(..)), "{oom}");
    let panicked = run(serde_json::json!({"do": "panic"})).await.unwrap_err();
    assert!(matches!(panicked, CallError::Trap(..)), "{panicked}");

    // That was the third failure in a row: the plugin is switched off, with the reason recorded.
    let off = run(serde_json::json!({"do": "ok"})).await.unwrap_err();
    assert!(matches!(off, CallError::Unavailable(_)), "{off}");
    let p = coolify_plugins::store::find(host.db(), id)
        .await
        .unwrap()
        .unwrap();
    assert!(!p.enabled);
    assert!(
        p.disabled_reason
            .unwrap_or_default()
            .contains("3 failures in a row")
    );
    tdb.drop_db().await;
}

#[tokio::test]
async fn teams_cannot_see_each_others_data() {
    let tdb = TestDb::new("").await;
    let host = host(&tdb, Limits::default(), Egress::default()).await;
    let id = "test/hostile";
    host.install(
        manifest(id, hostile_caps()),
        &wasm("hostile"),
        "file",
        false,
        None,
    )
    .await
    .unwrap();
    for team in [1, 2] {
        host.set_team(id, team, true, None).await.unwrap();
    }
    let store = |team, key: &str| {
        host.call_event(
            id,
            event(
                team,
                serde_json::json!({"do": "store", "key": key, "size": 3}),
            ),
        )
    };
    store(1, "team-one").await.unwrap();
    store(2, "team-two").await.unwrap();
    let seen = body(
        &host
            .handle_request(id, caller(1), get("/x?y=1"))
            .await
            .unwrap(),
    );
    assert_eq!(seen["keys"], serde_json::json!(["team-one"]), "{seen}");
    assert_eq!(seen["team_id"], 1);
    assert_eq!(seen["abilities"], serde_json::json!(["read"]));
    let seen = body(&host.handle_request(id, caller(2), get("/")).await.unwrap());
    assert_eq!(seen["keys"], serde_json::json!(["team-two"]), "{seen}");
    tdb.drop_db().await;
}

#[tokio::test]
async fn http_reaches_only_manifest_hosts() {
    // A local server stands in for the plugin's API; local addresses are only allowed in tests.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            let mut buf = [0u8; 4096];
            let _ = sock.read(&mut buf).await;
            let _ = sock
                .write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-length: 5\r\nconnection: close\r\n\r\nhello",
                )
                .await;
        }
    });
    let tdb = TestDb::new("").await;
    let egress = Egress {
        allow_local: true,
        ..Egress::default()
    };
    let host = host(&tdb, Limits::default(), egress).await;
    let id = "test/hostile";
    host.install(
        manifest(id, hostile_caps()),
        &wasm("hostile"),
        "file",
        false,
        None,
    )
    .await
    .unwrap();
    host.set_team(id, 1, true, None).await.unwrap();
    let fetch =
        |url: String| host.call_event(id, event(1, serde_json::json!({"do": "fetch", "url": url})));

    fetch(format!("http://localhost:{port}/hook"))
        .await
        .unwrap();
    let stored = coolify_plugins::store::kv_get(host.db(), id, 1, "fetched")
        .await
        .unwrap();
    assert_eq!(stored.as_deref(), Some(&b"hello"[..]));

    let refused = fetch(format!("http://127.0.0.1:{port}/hook"))
        .await
        .unwrap_err()
        .to_string();
    assert!(
        refused.contains("not in the plugin's manifest"),
        "{refused}"
    );
    tdb.drop_db().await;
}
