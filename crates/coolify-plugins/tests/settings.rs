//! Per-team plugin settings (ADR 0014) against the real host: a plugin reads its own settings for the call's
//! team and nothing else, secrets are encrypted at rest with `APP_KEY`, `config` needs declared settings,
//! updates reconcile stored values, and uninstalling deletes them. `crates/coolify-api/tests/plugin_settings.rs`
//! covers the API on top.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::sync::OnceLock;

use coolify_compat::Encrypter;
use coolify_db::testing::TestDb;
use coolify_plugins::{Caller, Event, Host, Manifest, Request, Settings, UpdateError};
use serde_json::{Map, Value, json};

/// A throwaway key, the encrypter fixture's.
const APP_KEY: &str = "base64:hPAFGDXpvHW0ZnwoP/YKkC1WJ+9Bv2jZCwc1hGGaI3s=";
const ID: &str = "test/configured";
const OTHER: &str = "test/other";
const SECRET_A: &str = "tk_team_one_value";

fn plugins_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugins")
}

fn wasm() -> Vec<u8> {
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
    let path = plugins_dir().join("target/wasm32-wasip2/release/configured.wasm");
    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// `plugins/testing/configured/plugin.toml`, under `id`.
fn manifest(id: &str) -> Manifest {
    let text =
        std::fs::read_to_string(plugins_dir().join("testing/configured/plugin.toml")).unwrap();
    let mut m = Manifest::parse(&text).unwrap();
    m.id = id.into();
    m
}

async fn host(db: &TestDb, encrypted: bool) -> Host {
    let host = Host::start(db.db.clone(), Settings::default())
        .await
        .unwrap_or_else(|e| panic!("start: {e}"));
    if encrypted {
        host.use_encrypter(Encrypter::new(APP_KEY).unwrap());
    }
    host
}

fn changes(v: Value) -> Map<String, Value> {
    v.as_object().unwrap().clone()
}

/// What the plugin's `config.get(key)` answers for the team, through one of its routes.
async fn get(host: &Host, id: &str, team: i64, key: &str) -> Value {
    let r = host
        .handle_request(
            id,
            Caller {
                team_id: team,
                user_id: 1,
                abilities: vec!["read".into()],
            },
            Request {
                method: "GET".into(),
                path: format!("/config/{key}"),
                query: String::new(),
                headers: vec![],
                body: vec![],
            },
        )
        .await
        .unwrap();
    assert_eq!(r.status, 200);
    let v: Value = serde_json::from_slice(&r.body).unwrap();
    v["value"].clone()
}

async fn install(host: &Host, id: &str, teams: &[i64]) {
    host.install(manifest(id), &wasm(), "file", false, None)
        .await
        .unwrap();
    for t in teams {
        host.set_team(id, *t, true, None).await.unwrap();
    }
}

async fn stored(host: &Host, id: &str, team: i64, key: &str) -> Option<(String, bool)> {
    sqlx::query_as(
        "SELECT value, secret FROM coolify_rs_plugins.team_settings WHERE plugin_id = $1 AND team_id = $2 AND key = $3",
    )
    .bind(id)
    .bind(team)
    .bind(key)
    .fetch_optional(host.db().pool())
    .await
    .unwrap()
}

#[tokio::test]
async fn a_plugin_reads_its_own_settings_for_the_calls_team_only() {
    let tdb = TestDb::new("").await;
    let host = host(&tdb, true).await;
    install(&host, ID, &[1, 2, 3]).await;
    install(&host, OTHER, &[1]).await;

    host.update_team_config(
        ID,
        1,
        Some(10),
        &changes(json!({"topic": "alpha", "token": SECRET_A})),
    )
    .await
    .unwrap();
    host.update_team_config(ID, 2, Some(20), &changes(json!({"topic": "beta"})))
        .await
        .unwrap();
    host.update_team_config(OTHER, 1, Some(10), &changes(json!({"topic": "other"})))
        .await
        .unwrap();

    // Each call reads the settings of its own plugin and team.
    assert_eq!(get(&host, ID, 1, "topic").await, "alpha");
    assert_eq!(get(&host, ID, 2, "topic").await, "beta");
    assert_eq!(get(&host, OTHER, 1, "topic").await, "other");
    assert_eq!(get(&host, ID, 1, "token").await, SECRET_A);
    assert_eq!(get(&host, ID, 2, "token").await, Value::Null);
    assert_eq!(get(&host, OTHER, 1, "token").await, Value::Null);
    // Unset: none, required or not; the default when there is one; none for undeclared keys.
    assert_eq!(get(&host, ID, 3, "topic").await, Value::Null);
    assert_eq!(get(&host, ID, 3, "webhook").await, Value::Null);
    assert_eq!(get(&host, ID, 3, "server").await, "https://ntfy.sh");
    assert_eq!(get(&host, ID, 3, "priority").await, "default");
    assert_eq!(get(&host, ID, 1, "nope").await, Value::Null);
    assert_eq!(get(&host, ID, 1, "TOPIC").await, Value::Null);

    // Events are calls for the event's team too.
    host.call_event(
        ID,
        Event {
            kind: "deployment.succeeded".into(),
            version: 1,
            team_id: 2,
            payload: "{}".into(),
        },
    )
    .await
    .unwrap();
    let seen = coolify_plugins::store::kv_get(host.db(), ID, 2, "topic")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(seen, b"beta");

    // What the API shows: values of settings that are not secret, only `set` for secrets.
    let one = host.team_config(ID, 1).await.unwrap();
    assert!(!one.needs_configuration());
    let json = one.to_json();
    let token = json["settings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["key"] == "token")
        .unwrap();
    assert_eq!(token["set"], true);
    assert!(token.get("value").is_none(), "{token}");
    assert!(!json.to_string().contains(SECRET_A));
    let topic = &json["settings"][0];
    assert_eq!(
        (&topic["key"], &topic["value"]),
        (&json!("topic"), &json!("alpha"))
    );
    assert!(host.team_config(ID, 2).await.unwrap().needs_configuration());

    // Switching the plugin off for a team keeps its settings.
    host.set_team(ID, 1, false, None).await.unwrap();
    host.set_team(ID, 1, true, None).await.unwrap();
    assert_eq!(get(&host, ID, 1, "token").await, SECRET_A);
    tdb.drop_db().await;
}

#[tokio::test]
async fn secrets_are_encrypted_at_rest_and_audits_have_no_values() {
    let tdb = TestDb::new("").await;
    let host = host(&tdb, true).await;
    install(&host, ID, &[1]).await;
    let webhook = "https://chat.googleapis.com/v1/spaces/x/messages?key=k&token=t";
    host.update_team_config(
        ID,
        1,
        Some(10),
        &changes(json!({"topic": "alpha", "token": SECRET_A, "webhook": webhook})),
    )
    .await
    .unwrap();

    let e = Encrypter::new(APP_KEY).unwrap();
    for (key, plain) in [("token", SECRET_A), ("webhook", webhook)] {
        let (raw, secret) = stored(&host, ID, 1, key).await.unwrap();
        assert!(secret);
        assert_ne!(raw, plain);
        assert!(!raw.contains(plain));
        assert_eq!(e.decrypt_string(&raw).unwrap(), plain);
    }
    assert_eq!(
        stored(&host, ID, 1, "topic").await,
        Some(("alpha".into(), false))
    );

    let audit: Vec<(String, Value, Option<i64>)> = sqlx::query_as(
        "SELECT action, detail, user_id FROM coolify_rs_plugins.audit WHERE plugin_id = $1 AND team_id = 1 ORDER BY id",
    )
    .bind(ID)
    .fetch_all(host.db().pool())
    .await
    .unwrap();
    let last = audit.last().unwrap();
    assert_eq!(last.0, "team_settings_updated");
    assert_eq!(last.2, Some(10));
    assert_eq!(
        last.1,
        json!({"set": ["topic", "token", "webhook"], "cleared": []})
    );
    let everything: Vec<Value> = sqlx::query_scalar("SELECT detail FROM coolify_rs_plugins.audit")
        .fetch_all(host.db().pool())
        .await
        .unwrap();
    let text = serde_json::to_string(&everything).unwrap();
    assert!(
        !text.contains(SECRET_A) && !text.contains("alpha") && !text.contains("chat.googleapis")
    );

    // null and blank clear; clearing what is unset changes nothing.
    let (_, changed) = host
        .update_team_config(
            ID,
            1,
            Some(10),
            &changes(json!({"token": null, "topic": "  ", "webhook": null, "server": null})),
        )
        .await
        .unwrap();
    assert_eq!(changed.cleared, ["token", "topic", "webhook"]);
    assert!(stored(&host, ID, 1, "token").await.is_none());
    tdb.drop_db().await;
}

#[tokio::test]
async fn invalid_updates_change_nothing() {
    let tdb = TestDb::new("").await;
    let host = host(&tdb, true).await;
    install(&host, ID, &[1]).await;
    host.update_team_config(ID, 1, None, &changes(json!({"topic": "alpha"})))
        .await
        .unwrap();
    let cases = [
        (
            json!({"topic": "new", "server": "http://ntfy.sh"}),
            "server",
            "https://",
        ),
        (
            json!({"topic": "new", "webhook": "not a url"}),
            "webhook",
            "https://",
        ),
        (
            json!({"topic": "new", "priority": "urgent"}),
            "priority",
            "one of",
        ),
        (
            json!({"topic": "new", "nope": "x"}),
            "nope",
            "no such setting",
        ),
        (json!({"topic": 5}), "topic", "string or null"),
        (json!({"topic": "a\nb"}), "topic", "line breaks"),
        (json!({"token": "x".repeat(5000)}), "token", "at most"),
    ];
    for (body, key, want) in cases {
        let err = host
            .update_team_config(ID, 1, None, &changes(body.clone()))
            .await
            .unwrap_err();
        let UpdateError::Invalid(errors) = err else {
            panic!("{body}: {err}");
        };
        let msg = errors
            .get(key)
            .unwrap_or_else(|| panic!("{body}: {errors:?}"));
        assert!(msg.contains(want), "{body}: {msg}");
    }
    assert_eq!(get(&host, ID, 1, "topic").await, "alpha");

    // Without APP_KEY, secrets are refused rather than stored in clear.
    let plain = self::host(&tdb, false).await;
    let err = plain
        .update_team_config(
            ID,
            1,
            None,
            &changes(json!({"token": SECRET_A, "topic": "b"})),
        )
        .await
        .unwrap_err();
    let UpdateError::Invalid(errors) = err else {
        panic!("{err}");
    };
    assert!(errors["token"].contains("APP_KEY"), "{errors:?}");
    assert!(stored(&host, ID, 1, "token").await.is_none());
    tdb.drop_db().await;
}

#[tokio::test]
async fn config_needs_declared_settings() {
    let tdb = TestDb::new("").await;
    let host = host(&tdb, true).await;
    let mut m = manifest(ID);
    m.settings.clear();
    let err = host
        .install(m, &wasm(), "file", false, None)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("coolify-rs:plugin/config@0.1."), "{err}");
    assert!(
        coolify_plugins::store::find(host.db(), ID)
            .await
            .unwrap()
            .is_none()
    );

    // The conformance check's sandbox answers defaults only.
    let detached = Host::detached(Settings::default()).unwrap();
    detached.load_detached(manifest(ID), &wasm()).unwrap();
    let probe = |key: &'static str| {
        let detached = detached.clone();
        async move {
            let r = detached
                .probe_request(
                    ID,
                    Caller {
                        team_id: 1,
                        user_id: 1,
                        abilities: vec![],
                    },
                    Request {
                        method: "GET".into(),
                        path: format!("/config/{key}"),
                        query: String::new(),
                        headers: vec![],
                        body: vec![],
                    },
                )
                .await
                .result
                .unwrap();
            serde_json::from_slice::<Value>(&r.body).unwrap()["value"].clone()
        }
    };
    assert_eq!(probe("server").await, "https://ntfy.sh");
    assert_eq!(probe("topic").await, Value::Null);
    tdb.drop_db().await;
}

#[tokio::test]
async fn updates_reconcile_and_uninstall_deletes() {
    let tdb = TestDb::new("").await;
    let host = host(&tdb, true).await;
    install(&host, ID, &[1, 2]).await;
    install(&host, OTHER, &[1]).await;
    for team in [1, 2] {
        host.update_team_config(
            ID,
            team,
            None,
            &changes(json!({
                "topic": "alpha",
                "token": SECRET_A,
                "server": "https://ntfy.example.com",
                "priority": "high",
            })),
        )
        .await
        .unwrap();
    }
    host.update_team_config(OTHER, 1, None, &changes(json!({"topic": "kept"})))
        .await
        .unwrap();

    // Version 2: `topic` is gone, `server` turned secret, `token` is no longer secret, and `high` is no
    // longer a priority.
    let mut v2 = manifest(ID);
    v2.version = "0.2.0".into();
    v2.settings.retain(|s| s.key != "topic");
    for s in &mut v2.settings {
        match s.key.as_str() {
            "server" => {
                s.secret = true;
                s.default = None;
            }
            "token" => s.secret = false,
            "priority" => s.options = vec!["low".into(), "default".into()],
            _ => {}
        }
    }
    host.install(v2, &wasm(), "file", false, None)
        .await
        .unwrap();

    let e = Encrypter::new(APP_KEY).unwrap();
    for team in [1, 2] {
        assert!(stored(&host, ID, team, "topic").await.is_none());
        assert!(
            stored(&host, ID, team, "token").await.is_none(),
            "a former secret is never shown"
        );
        assert!(stored(&host, ID, team, "priority").await.is_none());
        let (raw, secret) = stored(&host, ID, team, "server").await.unwrap();
        assert!(secret);
        assert_eq!(e.decrypt_string(&raw).unwrap(), "https://ntfy.example.com");
        assert_eq!(
            get(&host, ID, team, "server").await,
            "https://ntfy.example.com"
        );
    }
    let audit: Value = sqlx::query_scalar(
        "SELECT detail FROM coolify_rs_plugins.audit WHERE plugin_id = $1 AND action = 'settings_reconciled'",
    )
    .bind(ID)
    .fetch_one(host.db().pool())
    .await
    .unwrap();
    assert_eq!(audit["dropped"].as_array().unwrap().len(), 6, "{audit}");
    assert_eq!(audit["encrypted"].as_array().unwrap().len(), 2, "{audit}");
    assert!(!audit.to_string().contains("ntfy.example.com"));

    // Uninstalling deletes the plugin's settings for every team, and only its own.
    assert!(host.uninstall(ID, None).await.unwrap());
    let left: Vec<(String, String)> = sqlx::query_as(
        "SELECT plugin_id, key FROM coolify_rs_plugins.team_settings ORDER BY plugin_id, key",
    )
    .fetch_all(host.db().pool())
    .await
    .unwrap();
    assert_eq!(left, [(OTHER.to_owned(), "topic".to_owned())]);
    tdb.drop_db().await;
}
