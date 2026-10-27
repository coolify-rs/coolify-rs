//! `yaml::dump` against output of symfony/yaml in the upstream reference image.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use coolify_compat::yaml::{Key, PhpValue, dump};
use serde_json::Value;

#[test]
fn scalars_match_symfony() {
    for line in include_str!("fixtures/symfony-yaml-scalars.jsonl").lines() {
        let pair: Vec<Value> = serde_json::from_str(line).unwrap();
        let input = match &pair[0] {
            Value::String(s) => PhpValue::Str(s.clone()),
            Value::Number(n) => PhpValue::Float(n.as_f64().unwrap()),
            other => panic!("{other}"),
        };
        assert_eq!(dump(&input, 0), pair[1].as_str().unwrap(), "{line}");
    }
}

#[test]
fn structure_matches_symfony() {
    let doc = PhpValue::map([
        ("a", PhpValue::list([])),
        ("b", PhpValue::map([("x", PhpValue::list([]))])),
        (
            "c",
            PhpValue::list([PhpValue::map([("type", PhpValue::from("bind"))])]),
        ),
        ("d", PhpValue::Null),
        (
            "e",
            PhpValue::Array(vec![(Key::Int(1), PhpValue::from("x"))]),
        ),
        ("f", PhpValue::map([("k", PhpValue::strings(["l1", "l2"]))])),
    ]);
    assert_eq!(
        dump(&doc, 10),
        "a: {}\nb:\n    x: {}\nc:\n    -\n        type: bind\nd: null\ne:\n    1: x\nf:\n    k:\n        - l1\n        - l2\n"
    );
}
