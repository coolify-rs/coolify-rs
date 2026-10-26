//! Differential test: the static route parser must reproduce Laravel's own
//! `route:list` for the same upstream version (method, uri, action, middleware).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;

use coolify_port::inventory::build_route_inventory;
use serde::Deserialize;

#[derive(Deserialize)]
struct Listed {
    method: String,
    uri: String,
    action: String,
    middleware: Vec<String>,
}

#[test]
fn parser_matches_artisan_route_list_v4_5_0() {
    let inv = build_route_inventory(
        "v4.5.0",
        include_str!("fixtures/v4.5.0/api.php"),
        include_str!("fixtures/v4.5.0/Kernel.php"),
    );
    let listed: Vec<Listed> =
        serde_json::from_str(include_str!("fixtures/v4.5.0/route-list.json")).unwrap();

    let norm = |methods: &str| -> String {
        let mut m: Vec<&str> = methods.split('|').filter(|m| *m != "HEAD").collect();
        m.sort();
        m.join("|")
    };
    let expected: BTreeMap<String, (String, Vec<String>)> = listed
        .iter()
        // `--path=api` is a substring filter; keep only routes from routes/api.php.
        .filter(|l| l.uri.starts_with("api/"))
        .map(|l| {
            (
                format!("{} {}", norm(&l.method), l.uri),
                (l.action.clone(), l.middleware.clone()),
            )
        })
        .collect();
    let actual: BTreeMap<String, (String, Vec<String>)> = inv
        .routes
        .iter()
        .map(|r| {
            let mut m = r.methods.clone();
            m.sort();
            (
                format!("{} {}", m.join("|"), r.uri),
                (r.action.clone(), r.middleware.clone()),
            )
        })
        .collect();

    let missing: Vec<_> = expected
        .keys()
        .filter(|k| !actual.contains_key(*k))
        .collect();
    let extra: Vec<_> = actual
        .keys()
        .filter(|k| !expected.contains_key(*k))
        .collect();
    assert!(
        missing.is_empty(),
        "routes missed by the parser: {missing:#?}"
    );
    assert!(
        extra.is_empty(),
        "routes invented by the parser: {extra:#?}"
    );

    let mut mismatches = Vec::new();
    for (k, (action, mw)) in &expected {
        let (a_action, a_mw) = &actual[k];
        // Laravel reports closures as "Closure".
        if action != a_action {
            mismatches.push(format!("{k}: action {action} vs {a_action}"));
        }
        if mw != a_mw {
            mismatches.push(format!("{k}: middleware {mw:?} vs {a_mw:?}"));
        }
    }
    assert!(
        mismatches.is_empty(),
        "{} mismatches:\n{}",
        mismatches.len(),
        mismatches.join("\n")
    );
    assert_eq!(actual.len(), expected.len());
}
