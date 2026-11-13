//! `SafeExternalUrl::forGitSource()`: a git source of the root team may point at a private
//! address; a source of another team only at one of the instance's allowed internal targets.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::Harness;
use serde_json::json;

#[tokio::test]
async fn private_source_urls_are_for_root_team_sources_or_allowed_targets() {
    let h = Harness::new().await;
    let body = || json!({"html_url": "http://10.20.30.40", "api_url": "http://10.20.30.40/api/v3"});
    let url_errors = |token: &'static str| {
        let h = &h;
        async move {
            let r = h
                .call("POST", "/api/v1/github-apps", Some(token), Some(body()))
                .await;
            // The other required fields are missing either way.
            assert_eq!(r.status, 422, "{}", r.body);
            let errors = r.json()["errors"].clone();
            (
                errors.get("html_url").cloned(),
                errors.get("api_url").cloned(),
            )
        }
    };
    // Another team: refused, with the way to allow the target.
    let (html, api) = url_errors("acme_owner_root").await;
    let html = html.expect("html_url error").to_string();
    assert!(
        html.contains("points to a local or private address"),
        "{html}"
    );
    assert!(api.is_some());
    // The root team: allowed, as upstream allows it on a self-hosted instance.
    assert_eq!(url_errors("root_root").await, (None, None));
    // An allowed internal target opens it for every team.
    sqlx::query(
        "UPDATE instance_settings SET webhook_allowed_internal_hosts = '[\"10.20.30.40\"]'",
    )
    .execute(h.tdb.db.pool())
    .await
    .unwrap();
    assert_eq!(url_errors("acme_owner_root").await, (None, None));
    h.finish().await;
}
