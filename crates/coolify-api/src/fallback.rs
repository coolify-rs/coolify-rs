//! Requests that match no ported route.
//!
//! Upstream answers unknown `/api/*` requests with its catch-all
//! (`routes/api.php:462`, 404 JSON). Requests that match a route upstream
//! *does* have but coolify-rs has not ported yet get an explicit 501 instead,
//! so clients can tell "not ported" apart from "does not exist".

use std::sync::LazyLock;

use axum::extract::Request;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use crate::response::ApiError;

const INVENTORY: &str = include_str!("../../../compat/api/upstream-routes.json");

#[derive(Deserialize)]
struct Inventory {
    routes: Vec<InvRoute>,
}

#[derive(Deserialize)]
struct InvRoute {
    methods: Vec<String>,
    uri: String,
    action: String,
}

struct Pattern {
    methods: Vec<String>,
    segments: Vec<String>,
}

static UPSTREAM: LazyLock<Vec<Pattern>> = LazyLock::new(|| {
    let inv: Inventory = serde_json::from_str(INVENTORY).unwrap_or(Inventory { routes: vec![] });
    inv.routes
        .into_iter()
        // The catch-all itself is the 404 handler, not a route to port.
        .filter(|r| !(r.action == "Closure" && r.uri.ends_with("{any}")))
        .map(|r| Pattern {
            methods: r.methods,
            segments: r.uri.split('/').map(str::to_string).collect(),
        })
        .collect()
});

pub fn upstream_has_route(method: &str, path: &str) -> bool {
    let path = path.trim_matches('/');
    let segs: Vec<&str> = path.split('/').collect();
    let method = if method == "HEAD" { "GET" } else { method };
    UPSTREAM.iter().any(|p| {
        p.methods.iter().any(|m| m == method)
            && p.segments.len() == segs.len()
            && p.segments
                .iter()
                .zip(&segs)
                .all(|(pat, seg)| (pat.starts_with('{') && !seg.is_empty()) || pat == seg)
    })
}

pub async fn fallback(req: Request) -> Response {
    if upstream_has_route(req.method().as_str(), req.uri().path()) {
        ApiError::NotPorted.into_response()
    } else {
        ApiError::RouteNotFound.into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn knows_upstream_routes() {
        assert!(upstream_has_route("GET", "/api/v1/servers"));
        assert!(upstream_has_route("POST", "/api/v1/servers/abc/validate"));
        assert!(!upstream_has_route("DELETE", "/api/v1/nope"));
        assert!(!upstream_has_route("GET", "/api/v1/nope"));
        assert!(!upstream_has_route("PUT", "/api/v1/servers"));
    }
}
