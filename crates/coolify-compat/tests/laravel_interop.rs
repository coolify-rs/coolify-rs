//! Decrypts payloads produced by upstream Laravel's own Encrypter.
//!
//! The fixture was generated inside the official coollabsio/coolify image
//! (see the `generator` field). The key is a throwaway test key.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use coolify_compat::Encrypter;
use serde::Deserialize;

#[derive(Deserialize)]
struct Fixture {
    app_key: String,
    cases: Vec<Case>,
}

#[derive(Deserialize)]
struct Case {
    kind: String,
    plaintext: String,
    payload: String,
}

fn fixture() -> Fixture {
    serde_json::from_str(include_str!("fixtures/laravel_encrypter.json")).unwrap()
}

#[test]
fn decrypts_every_laravel_payload() {
    let fx = fixture();
    let enc = Encrypter::new(&fx.app_key).unwrap();
    assert!(!fx.cases.is_empty());
    for case in &fx.cases {
        let got = match case.kind.as_str() {
            "encryptString" => enc.decrypt_string(&case.payload).unwrap(),
            "encrypt_serialized" => enc.decrypt_maybe_serialized(&case.payload).unwrap(),
            other => panic!("unknown fixture kind {other}"),
        };
        assert_eq!(got, case.plaintext, "kind={}", case.kind);
    }
}

#[test]
fn rust_payloads_have_laravel_shape() {
    let fx = fixture();
    let enc = Encrypter::new(&fx.app_key).unwrap();
    let payload = enc.encrypt_string("hello");
    use base64::Engine;
    let json = base64::engine::general_purpose::STANDARD
        .decode(payload)
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&json).unwrap();
    for field in ["iv", "value", "mac", "tag"] {
        assert!(v.get(field).is_some(), "missing {field}");
    }
    assert_eq!(v["mac"].as_str().unwrap().len(), 64);
}
