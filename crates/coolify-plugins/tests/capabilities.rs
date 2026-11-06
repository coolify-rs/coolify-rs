//! The data capabilities of ADR 0011 (`resources`, `deploy`) against the real host, with recording stand-ins
//! for the REST API: imports need their grant, the team always comes from the call, deploy needs the
//! team's consent and is rate limited. Also: 0.1.0 components still load on the 0.1.1 host, and the
//! portability check of ADR 0012. `crates/coolify-api/tests/plugin_capabilities.rs` runs the same plugins
//! against the real API code.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use coolify_db::testing::TestDb;
use coolify_plugins::data::BoxFuture;
use coolify_plugins::egress::Egress;
use coolify_plugins::{
    Actor, CallError, Caller, Capabilities, DeployRequest, Deployer, Event, Host, Limits, Manifest,
    Request, ResourceKind, ResourceQuery, ResourceSource, Settings,
};
use serde_json::{Value, json};

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
        settings: Vec::new(),
    }
}

/// What the hostile plugin imports, with `resources` limited to `kinds`.
fn hostile(kinds: &[&str], deploy: bool) -> Capabilities {
    Capabilities {
        events: vec!["deployment.succeeded".into()],
        storage: true,
        routes: true,
        http: vec!["localhost".into()],
        resources: kinds.iter().map(|k| (*k).to_owned()).collect(),
        deploy,
        ..Default::default()
    }
}

fn caller(team_id: i64) -> Caller {
    Caller {
        team_id,
        user_id: 7,
        abilities: vec!["read".into(), "deploy".into()],
    }
}

fn request(method: &str, path: &str, query: &str) -> Request {
    Request {
        method: method.into(),
        path: path.into(),
        query: query.into(),
        headers: vec![],
        body: vec![],
    }
}

fn body(r: &coolify_plugins::Response) -> Value {
    serde_json::from_slice(&r.body).unwrap_or_default()
}

/// Stands in for the REST API: answers with the team and query it was asked for, and records deployments.
#[derive(Default)]
struct Recorder {
    reads: Mutex<Vec<(i64, ResourceQuery)>>,
    deploys: Mutex<Vec<DeployRequest>>,
}

impl ResourceSource for Recorder {
    fn read<'a>(
        &'a self,
        team_id: i64,
        query: &'a ResourceQuery,
    ) -> BoxFuture<'a, Result<Option<String>, String>> {
        Box::pin(async move {
            self.reads.lock().unwrap().push((team_id, query.clone()));
            Ok(match query {
                ResourceQuery::One(_, uuid) if uuid == "missing" => None,
                q => Some(json!({"team": team_id, "query": format!("{q:?}")}).to_string()),
            })
        })
    }
}

impl Deployer for Recorder {
    fn deploy<'a>(&'a self, request: &'a DeployRequest) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            self.deploys.lock().unwrap().push(request.clone());
            Ok(json!({"deployments": [{"resource_uuid": request.uuid}]}).to_string())
        })
    }
}

async fn host(db: &TestDb, limits: Limits) -> (Host, Arc<Recorder>) {
    let host = Host::start(
        db.db.clone(),
        Settings {
            limits,
            egress: Egress::default(),
            ..Settings::default()
        },
    )
    .await
    .unwrap_or_else(|e| panic!("start: {e}"));
    let recorder = Arc::new(Recorder::default());
    host.connect(recorder.clone(), recorder.clone());
    (host, recorder)
}

#[tokio::test]
async fn data_capabilities_need_their_grant() {
    let tdb = TestDb::new("").await;
    let (host, _) = host(&tdb, Limits::default()).await;
    let cases: [(&[&str], bool, &str); 2] = [
        (&[], true, "coolify-rs:plugin/resources@0.1."),
        (&["applications"], false, "coolify-rs:plugin/deploy@0.1."),
    ];
    for (kinds, deploy, refused) in cases {
        let err = host
            .install(
                manifest("test/hostile", hostile(kinds, deploy)),
                &wasm("hostile"),
                "file",
                false,
                None,
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains(refused), "{err}");
    }
    // The example asks for exactly what it imports.
    let text =
        std::fs::read_to_string(plugins_dir().join("examples/inventory/plugin.toml")).unwrap();
    let m = Manifest::parse(&text).unwrap();
    assert_eq!(
        m.capability_summary(),
        ["routes", "resources: applications"]
    );
    let mut without = m.clone();
    without.capabilities.resources.clear();
    assert!(
        host.install(without, &wasm("inventory"), "file", false, None)
            .await
            .unwrap_err()
            .to_string()
            .contains("coolify-rs:plugin/resources@0.1.")
    );
    host.install(m, &wasm("inventory"), "file", false, None)
        .await
        .unwrap();

    // A plugin that owns rows of Coolify's tables stays until they are handed back (ADR 0012).
    let id = "coolify-rs/inventory";
    coolify_plugins::store::mark_owned(host.db(), "applications", 9, id, "")
        .await
        .unwrap();
    let err = host.uninstall(id, None).await.unwrap_err().to_string();
    assert!(err.contains("owns 1 rows"), "{err}");
    sqlx::query("DELETE FROM coolify_rs_plugins.owned_resources")
        .execute(host.db().pool())
        .await
        .unwrap();
    assert!(host.uninstall(id, None).await.unwrap());
    tdb.drop_db().await;
}

#[tokio::test]
async fn resources_are_read_for_the_calling_team_and_granted_kinds_only() {
    let tdb = TestDb::new("").await;
    let (host, recorder) = host(&tdb, Limits::default()).await;
    let id = "test/hostile";
    host.install(
        manifest(id, hostile(&["applications"], true)),
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
    let get = |team, path: &str| host.handle_request(id, caller(team), request("GET", path, ""));

    for team in [1, 2] {
        let r = get(team, "/resources/applications").await.unwrap();
        assert_eq!(r.status, 200);
        assert_eq!(body(&r)["team"], team, "{:?}", body(&r));
    }
    let r = get(2, "/resources/applications/abc123").await.unwrap();
    assert_eq!(body(&r)["team"], 2);
    assert_eq!(
        get(1, "/resources/applications/missing")
            .await
            .unwrap()
            .status,
        404
    );
    // Not a uuid: not found, and the API is never asked.
    assert_eq!(
        get(1, "/resources/applications/a%27%3B")
            .await
            .unwrap()
            .status,
        404
    );

    for path in [
        "/resources/servers",
        "/resources/databases/abc",
        "/environments/abc",
    ] {
        let r = get(1, path).await.unwrap();
        assert_eq!(r.status, 422, "{path}");
        assert!(
            body(&r)["error"]
                .as_str()
                .unwrap()
                .contains("not granted resources"),
            "{path}: {:?}",
            body(&r)
        );
    }
    assert_eq!(
        *recorder.reads.lock().unwrap(),
        [
            (1, ResourceQuery::All(ResourceKind::Applications)),
            (2, ResourceQuery::All(ResourceKind::Applications)),
            (
                2,
                ResourceQuery::One(ResourceKind::Applications, "abc123".into())
            ),
            (
                1,
                ResourceQuery::One(ResourceKind::Applications, "missing".into())
            ),
        ]
    );

    // A team that did not enable the plugin gets nothing at all.
    let off = host
        .handle_request(id, caller(3), request("GET", "/resources/applications", ""))
        .await
        .unwrap_err();
    assert!(matches!(off, CallError::NotEnabledForTeam(_)), "{off}");
    tdb.drop_db().await;
}

#[tokio::test]
async fn deploy_needs_the_teams_consent_and_is_rate_limited() {
    let tdb = TestDb::new("").await;
    let limits = Limits {
        deploy_limit: 3,
        deploy_window: Duration::from_secs(3600),
        ..Limits::default()
    };
    let (host, recorder) = host(&tdb, limits).await;
    let id = "test/hostile";
    host.install(
        manifest(id, hostile(&["applications"], true)),
        &wasm("hostile"),
        "file",
        false,
        None,
    )
    .await
    .unwrap();
    let deploy = |team, uuid: &str| {
        host.handle_request(
            id,
            caller(team),
            request("POST", "/deploy", &format!("uuid={uuid}&force=1")),
        )
    };

    // Consent needs the plugin enabled for the team first.
    let err = host.set_team_deploy(id, 1, true, 5).await.unwrap_err();
    assert!(err.to_string().contains("not enabled for team 1"), "{err}");
    host.set_team(id, 1, true, Some(5)).await.unwrap();
    let r = deploy(1, "app1").await.unwrap();
    assert_eq!(r.status, 422);
    assert!(
        body(&r)["error"]
            .as_str()
            .unwrap()
            .contains("not granted deploy"),
        "{:?}",
        body(&r)
    );
    assert!(recorder.deploys.lock().unwrap().is_empty());

    host.set_team_deploy(id, 1, true, 5).await.unwrap();
    // A route deploys on the caller's authority, an event on the authority of whoever granted it.
    let r = deploy(1, "app1").await.unwrap();
    assert_eq!(r.status, 200, "{:?}", body(&r));
    assert_eq!(body(&r)["deployments"][0]["resource_uuid"], "app1");
    host.call_event(
        id,
        Event {
            kind: "deployment.succeeded".into(),
            version: 1,
            team_id: 1,
            payload: json!({"do": "deploy", "uuid": "app2"}).to_string(),
        },
    )
    .await
    .unwrap();
    assert_eq!(
        *recorder.deploys.lock().unwrap(),
        [
            DeployRequest {
                plugin: id.into(),
                team_id: 1,
                actor: Actor::Caller {
                    user_id: 7,
                    abilities: vec!["read".into(), "deploy".into()],
                },
                uuid: "app1".into(),
                force: true,
            },
            DeployRequest {
                plugin: id.into(),
                team_id: 1,
                actor: Actor::Grantor { user_id: 5 },
                uuid: "app2".into(),
                force: false,
            },
        ]
    );

    // The third is the last one the team's plugins get in the window; other teams have their own.
    assert_eq!(deploy(1, "app3").await.unwrap().status, 200);
    let r = deploy(1, "app4").await.unwrap();
    assert_eq!(r.status, 422);
    assert!(
        body(&r)["error"]
            .as_str()
            .unwrap()
            .contains("at most 3 deployments in 3600 seconds"),
        "{:?}",
        body(&r)
    );
    host.set_team(id, 2, true, Some(8)).await.unwrap();
    host.set_team_deploy(id, 2, true, 8).await.unwrap();
    assert_eq!(deploy(2, "other").await.unwrap().status, 200);
    let teams: Vec<i64> = recorder
        .deploys
        .lock()
        .unwrap()
        .iter()
        .map(|d| d.team_id)
        .collect();
    assert_eq!(teams, [1, 1, 1, 2]);

    // Every deployment is in the audit log with the plugin, the team and what came back.
    let rows: Vec<(String, i64, Value)> = sqlx::query_as(
        "SELECT plugin_id, team_id, detail FROM coolify_rs_plugins.audit WHERE action = 'deploy' ORDER BY id",
    )
    .fetch_all(host.db().pool())
    .await
    .unwrap();
    assert_eq!(rows.len(), 4);
    assert!(rows.iter().all(|(p, ..)| p == id));
    assert_eq!(rows[1].2["on_behalf_of"], "grantor");
    assert_eq!(rows[1].2["application_uuid"], "app2");
    assert_eq!(rows[1].2["ok"], true);
    assert_eq!(rows[3].1, 2);
    let granted: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM coolify_rs_plugins.audit WHERE action = 'team_deploy_granted'",
    )
    .fetch_one(host.db().pool())
    .await
    .unwrap();
    assert_eq!(granted, 2);

    // Withdrawn, the plugin is refused again; disabling and re-enabling forgets the consent.
    host.set_team_deploy(id, 2, false, 8).await.unwrap();
    assert_eq!(deploy(2, "other").await.unwrap().status, 422);
    host.set_team(id, 1, false, Some(5)).await.unwrap();
    host.set_team(id, 1, true, Some(5)).await.unwrap();
    assert!(
        coolify_plugins::store::deploy_grantor(host.db(), id, 1)
            .await
            .unwrap()
            .is_none()
    );
    tdb.drop_db().await;
}

#[tokio::test]
async fn deploy_consent_needs_the_capability() {
    let tdb = TestDb::new("").await;
    let (host, _) = host(&tdb, Limits::default()).await;
    let text =
        std::fs::read_to_string(plugins_dir().join("examples/inventory/plugin.toml")).unwrap();
    let m = Manifest::parse(&text).unwrap();
    let id = m.id.clone();
    host.install(m, &wasm("inventory"), "file", false, None)
        .await
        .unwrap();
    host.set_team(&id, 1, true, Some(1)).await.unwrap();
    let err = host.set_team_deploy(&id, 1, true, 1).await.unwrap_err();
    assert!(
        err.to_string()
            .contains("does not hold the deploy capability"),
        "{err}"
    );

    // A version without `deploy` drops the consent: asking for it again later needs the team again.
    let id = "test/hostile";
    let hostile_manifest = manifest(id, hostile(&["applications"], true));
    host.install(
        hostile_manifest.clone(),
        &wasm("hostile"),
        "file",
        false,
        None,
    )
    .await
    .unwrap();
    host.set_team(id, 1, true, Some(1)).await.unwrap();
    host.set_team_deploy(id, 1, true, 1).await.unwrap();
    let lean = Capabilities {
        routes: true,
        resources: vec!["applications".into()],
        ..Default::default()
    };
    host.install(manifest(id, lean), &wasm("inventory"), "file", false, None)
        .await
        .unwrap();
    host.install(hostile_manifest, &wasm("hostile"), "file", false, None)
        .await
        .unwrap();
    assert!(
        coolify_plugins::store::enabled_for_team(host.db(), id, 1)
            .await
            .unwrap()
    );
    assert_eq!(
        coolify_plugins::store::deploy_grantor(host.db(), id, 1)
            .await
            .unwrap(),
        None
    );
    tdb.drop_db().await;
}

/// `coolify-rs:plugin@0.1.1` components, built before `config` existed, load and run on the 0.1.2 host, with
/// `resources` (which 0.1.1 added) answered as before.
#[tokio::test]
async fn components_built_for_0_1_1_still_load() {
    let tdb = TestDb::new("").await;
    let (host, recorder) = host(&tdb, Limits::default()).await;
    let id = "test/compat-0-1-1";
    let caps = Capabilities {
        events: vec!["deployment.failed".into()],
        storage: true,
        routes: true,
        resources: vec!["applications".into()],
        ..Default::default()
    };
    let wasm = wasm("compat_0_1_1");
    let imports = imports_of(&wasm);
    assert!(
        imports
            .iter()
            .any(|i| i == "coolify-rs:plugin/resources@0.1.1"),
        "{imports:?}"
    );
    host.install(manifest(id, caps), &wasm, "file", false, None)
        .await
        .unwrap();
    host.set_team(id, 4, true, None).await.unwrap();
    host.call_event(
        id,
        Event {
            kind: "deployment.failed".into(),
            version: 1,
            team_id: 4,
            payload: "{}".into(),
        },
    )
    .await
    .unwrap();
    let r = host
        .handle_request(id, caller(4), request("GET", "/", ""))
        .await
        .unwrap();
    let text = String::from_utf8_lossy(&r.body).into_owned();
    assert!(text.starts_with("4 deployment.failed {"), "{text}");
    assert_eq!(
        recorder.reads.lock().unwrap().as_slice(),
        [(4, ResourceQuery::All(ResourceKind::Applications))]
    );
    let (again, _) = self::host(&tdb, Limits::default()).await;
    assert_eq!(again.loaded_ids(), [id]);
    tdb.drop_db().await;
}

/// The component's imports, as the host's check sees them.
fn imports_of(wasm: &[u8]) -> Vec<String> {
    let host = Host::detached(Settings::default()).unwrap();
    let caps = Capabilities {
        events: vec!["deployment.failed".into()],
        storage: true,
        routes: true,
        resources: vec!["applications".into()],
        ..Default::default()
    };
    host.load_detached(manifest("test/imports", caps), wasm)
        .unwrap()
}

/// `coolify-rs:plugin@0.1.0` components, built before `resources` and `deploy` existed, load and run on the
/// current host: wasmtime resolves `@0.1.0` imports to the current 0.1.x interfaces.
#[tokio::test]
async fn components_built_for_0_1_0_still_load() {
    let tdb = TestDb::new("").await;
    let (host, _) = host(&tdb, Limits::default()).await;
    let id = "test/compat";
    let caps = Capabilities {
        events: vec!["deployment.failed".into()],
        storage: true,
        routes: true,
        ..Default::default()
    };
    host.install(
        manifest(id, caps),
        &wasm("compat_0_1_0"),
        "file",
        false,
        None,
    )
    .await
    .unwrap();
    host.set_team(id, 4, true, None).await.unwrap();
    host.call_event(
        id,
        Event {
            kind: "deployment.failed".into(),
            version: 1,
            team_id: 4,
            payload: "{}".into(),
        },
    )
    .await
    .unwrap();
    let r = host
        .handle_request(id, caller(4), request("GET", "/", ""))
        .await
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&r.body), "4 deployment.failed");
    // It still loads when the host starts again.
    let (again, _) = self::host(&tdb, Limits::default()).await;
    assert_eq!(again.loaded_ids(), [id]);
    tdb.drop_db().await;
}

#[tokio::test]
async fn portability_check_lists_owned_rows_and_unknown_values() {
    let tdb = TestDb::new(
        "INSERT INTO applications (id, uuid, name, git_repository, git_branch, build_pack, environment_id) VALUES \
         (1, 'a1', 'web', 'r', 'main', 'nixpacks', 1), (2, 'a2', 'img', 'r', 'main', 'dockerimage', 1)",
    )
    .await;
    let db = &tdb.db;

    // Before plugins ever ran: no schema, nothing owned.
    let r = coolify_plugins::portable::check(db).await.unwrap();
    assert!(r.portable, "{r:?}");
    assert!(!r.plugin_schema);

    coolify_plugins::store::migrate(db).await.unwrap();
    let r = coolify_plugins::portable::check(db).await.unwrap();
    assert!(r.portable && r.plugin_schema, "{r:?}");
    assert!(r.human().contains("Portable"), "{}", r.human());

    // A provider plugin's build pack, marked as its own, and one nobody marked.
    sqlx::query(
        "INSERT INTO applications (id, uuid, name, git_repository, git_branch, build_pack, environment_id) VALUES \
         (3, 'a3', 'go', 'r', 'main', 'acme-go', 1), (4, 'a4', 'rb', 'r', 'main', 'acme-ruby', 1)",
    )
    .execute(db.pool())
    .await
    .unwrap();
    coolify_plugins::store::mark_owned(db, "applications", 3, "acme/go-pack", "build_pack acme-go")
        .await
        .unwrap();
    let r = coolify_plugins::portable::check(db).await.unwrap();
    assert!(!r.portable);
    assert_eq!(r.owned_resources.len(), 1);
    assert_eq!(r.owned_resources[0].plugin_id, "acme/go-pack");
    let unknown: Vec<(i64, &str, Option<&str>)> = r
        .unknown_values
        .iter()
        .map(|u| (u.row_id, u.value.as_str(), u.owner.as_deref()))
        .collect();
    assert_eq!(
        unknown,
        [(3, "acme-go", Some("acme/go-pack")), (4, "acme-ruby", None)]
    );
    let text = r.human();
    assert!(text.contains("Not portable"), "{text}");
    assert!(text.contains("not marked by any plugin"), "{text}");
    tdb.drop_db().await;
}
