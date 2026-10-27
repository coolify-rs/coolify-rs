//! `ApplicationConfigurationSnapshot` and `ConfigurationDiffer` against values computed by
//! upstream (tinker on the reference instance, with the fixture's `app_key` as
//! `config('app.key')`) for git (static, Docker Compose, Dockerfile) and Docker image
//! applications. Inputs are what `snapshot::load_input` read from the same rows.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use coolify_compat::Encrypter;
use coolify_compat::php_json::encode;
use coolify_jobs::applications::snapshot::{self, SnapshotInput};
use serde_json::Value;

fn fixture() -> Value {
    serde_json::from_str(include_str!("fixtures/configuration_snapshots.json")).unwrap()
}

fn encrypter(fx: &Value) -> Encrypter {
    Encrypter::new(fx["app_key"].as_str().unwrap()).unwrap()
}

fn named<'a>(fx: &'a Value, name: &str) -> &'a Value {
    fx["snapshots"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == name)
        .map(|s| &s["snapshot"])
        .unwrap()
}

#[test]
fn snapshots_match_upstream_to_array_and_hash() {
    let fx = fixture();
    let enc = encrypter(&fx);
    for case in fx["snapshots"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let input: SnapshotInput = serde_json::from_value(case["input"].clone()).unwrap();
        let built = snapshot::build(&input, &enc);
        assert_eq!(
            encode(&built),
            encode(&case["snapshot"]),
            "toArray() of {name}"
        );
        assert_eq!(
            snapshot::hash_snapshot(&built),
            case["hash"].as_str().unwrap(),
            "hashSnapshot() of {name}"
        );
    }
}

#[test]
fn diffs_match_upstream() {
    let fx = fixture();
    let empty = Value::Array(vec![]);
    for case in fx["diffs"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let previous = case["previous"].as_str().map_or(&empty, |n| named(&fx, n));
        let current = named(&fx, case["current"].as_str().unwrap());
        let diff = snapshot::diff(previous, current);
        assert_eq!(encode(&diff.to_array()), encode(&case["diff"]), "{name}");
        assert_eq!(
            diff.requires_build(),
            case["diff"]["requires_build"].as_bool().unwrap()
        );
        assert_eq!(
            diff.is_changed(),
            case["diff"]["changed"].as_bool().unwrap()
        );
        assert!(!diff.is_legacy_fallback());
    }
}

#[test]
fn older_snapshots_drop_preview_volumes_and_ignore_new_static_keys() {
    let fx = fixture();
    let older = &fx["older_snapshot"];
    let uuid = older["uuid"].as_str().unwrap();
    let pruned = snapshot::without_preview_volumes(older["snapshot"].clone(), uuid);
    assert_eq!(encode(&pruned), encode(&older["pruned"]));
    let current = named(&fx, older["current"].as_str().unwrap());
    assert_eq!(
        encode(&snapshot::diff(&pruned, current).to_array()),
        encode(&older["diff"])
    );
}

#[test]
fn encrypted_array_cast_round_trips_and_reads_plaintext() {
    let fx = fixture();
    let enc = encrypter(&fx);
    let value = named(&fx, "static-1");
    let stored = snapshot::encode_encrypted_array(&enc, value);
    assert_eq!(
        snapshot::decode_encrypted_array(&enc, Some(&stored)).as_ref(),
        Some(value)
    );
    // Legacy plaintext JSON rows written before the column was encrypted.
    assert_eq!(
        snapshot::decode_encrypted_array(&enc, Some(r#"{"a":1}"#)),
        Some(serde_json::json!({"a": 1}))
    );
    assert_eq!(snapshot::decode_encrypted_array(&enc, Some("\"x\"")), None);
    assert_eq!(snapshot::decode_encrypted_array(&enc, Some("")), None);
    assert_eq!(snapshot::decode_encrypted_array(&enc, None), None);
}
