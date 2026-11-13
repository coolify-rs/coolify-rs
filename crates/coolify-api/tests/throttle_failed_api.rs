//! `ThrottleFailedApiRequests`: API clients that keep sending failing requests get 429.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use axum::body::Body;
use axum::http::Request;
use common::{Harness, VERSION};
use coolify_api::ip::TrustedProxies;
use coolify_api::state::{AppState, Options};
use http_body_util::BodyExt as _;
use tower::ServiceExt as _;

/// A router over the harness database with the given limit of failed requests.
fn app(h: &Harness, limit: i64) -> axum::Router {
    let state = AppState::with_options(
        h.tdb.db.clone(),
        VERSION.into(),
        TrustedProxies::default(),
        Options {
            encrypter: h.state.encrypter.clone(),
            failed_api_request_limit: Some(limit),
            ..Options::default()
        },
    );
    coolify_api::router(state)
}

async fn send(
    app: &axum::Router,
    path: &str,
    bearer: Option<&str>,
) -> (u16, Option<String>, String) {
    let mut request = Request::builder().method("GET").uri(path);
    if let Some(token) = bearer {
        request = request.header("Authorization", format!("Bearer {token}"));
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status().as_u16();
    let retry_after = response
        .headers()
        .get("retry-after")
        .map(|v| v.to_str().unwrap().to_owned());
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        retry_after,
        String::from_utf8(bytes.to_vec()).unwrap(),
    )
}

#[tokio::test]
async fn a_token_that_keeps_failing_gets_429() {
    let h = Harness::new().await;
    let app = app(&h, 3);
    let bad = "999999|not-a-token";

    // Two failures stay under the limit.
    for _ in 0..2 {
        assert_eq!(send(&app, "/api/v1/servers", Some(bad)).await.0, 401);
    }
    // The third reaches it: this request still gets its own answer...
    assert_eq!(send(&app, "/api/v1/servers", Some(bad)).await.0, 401);
    // ...and every next request of the client is refused, also to routes that would answer.
    let (status, retry_after, body) = send(&app, "/api/v1/version", Some(bad)).await;
    assert_eq!(status, 429);
    let seconds: u64 = retry_after.unwrap().parse().unwrap();
    assert!((1..=60).contains(&seconds), "{seconds}");
    assert_eq!(
        body,
        format!(r#"{{"message":"Too many failed API requests. Retry after {seconds} seconds."}}"#)
    );

    // Another token is another client.
    let good = h.token("acme_owner_root").to_owned();
    assert_eq!(send(&app, "/api/v1/servers", Some(&good)).await.0, 200);
    // The health checks are never throttled.
    assert_ne!(send(&app, "/api/health", Some(bad)).await.0, 429);
    // Pages outside the API are not counted or refused.
    assert_ne!(send(&app, "/ui/session", Some(bad)).await.0, 429);
    h.finish().await;
}

#[tokio::test]
async fn a_missing_resource_counts_only_without_a_token() {
    let h = Harness::new().await;
    let app = app(&h, 2);
    let good = h.token("acme_owner_root").to_owned();

    // A client with a token often checks if a resource exists.
    for _ in 0..5 {
        assert_eq!(
            send(&app, "/api/v1/servers/does-not-exist", Some(&good))
                .await
                .0,
            404
        );
    }
    assert_eq!(send(&app, "/api/v1/servers", Some(&good)).await.0, 200);

    // Without a token the source address is the client, and unknown API routes count too.
    for _ in 0..2 {
        assert_eq!(send(&app, "/api/v1/no-such-route", None).await.0, 404);
    }
    assert_eq!(send(&app, "/api/v1/no-such-route", None).await.0, 429);
    // The token is still its own client.
    assert_eq!(send(&app, "/api/v1/servers", Some(&good)).await.0, 200);
    h.finish().await;
}

#[tokio::test]
async fn zero_turns_the_throttle_off() {
    let h = Harness::new().await;
    let app = app(&h, 0);
    for _ in 0..10 {
        assert_eq!(
            send(&app, "/api/v1/servers", Some("999999|not-a-token"))
                .await
                .0,
            401
        );
    }
    h.finish().await;
}
