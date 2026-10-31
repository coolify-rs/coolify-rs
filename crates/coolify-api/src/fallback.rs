//! Requests that match no ported route.
//!
//! * `/api/*` and `/ui/api/*`: upstream's catch-all (`routes/api.php:462`,
//!   404 JSON), or 501 when upstream *does* have the route but coolify-rs has
//!   not ported it, so clients can tell "not ported" from "does not exist".
//! * Anything else: the web UI (static files from `ui_dir`, with
//!   `index.html` as the single-page-app fallback).

use std::sync::LazyLock;

use axum::extract::{Request, State};
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use tower::ServiceExt;
use tower_http::services::{ServeDir, ServeFile};

use crate::response::ApiError;
use crate::state::AppState;

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
    let path = path.strip_prefix("ui/").unwrap_or(path);
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

fn is_api_path(path: &str) -> bool {
    path == "/api" || path.starts_with("/api/") || path.starts_with("/ui/")
}

/// Content Security Policy for the UI: same-origin only, no inline scripts. Monaco
/// (components/forms/monaco-editor.blade.php) starts its web worker from a blob URL. Forms may
/// post elsewhere for the GitHub App manifest flow, which posts to the provider's site (any
/// GitHub Enterprise host); the page gets there by client-side navigation, so this holds for all.
/// The new server page (livewire/server/create.blade.php) shows Vultr's logo from vultr.com. The
/// analytics views show `countryFlagUrl()` from flagcdn.com and `refererFaviconUrl()` from DuckDuckGo.
const CSP: &str = "default-src 'self'; script-src 'self'; worker-src 'self' blob:; style-src 'self' 'unsafe-inline'; \
                   img-src 'self' data: https://www.vultr.com https://flagcdn.com https://icons.duckduckgo.com; connect-src 'self'; font-src 'self' data:; \
                   frame-ancestors 'none'; base-uri 'self'; form-action 'self' https: http:";

pub async fn fallback(State(state): State<AppState>, req: Request) -> Response {
    let path = req.uri().path().to_string();
    if is_api_path(&path) {
        return if upstream_has_route(req.method().as_str(), &path) {
            ApiError::NotPorted.into_response()
        } else {
            ApiError::RouteNotFound.into_response()
        };
    }
    let Some(dir) = state.ui_dir.clone() else {
        return ApiError::RouteNotFound.into_response();
    };
    if !matches!(*req.method(), Method::GET | Method::HEAD) {
        return (StatusCode::METHOD_NOT_ALLOWED, "").into_response();
    }
    let index = dir.join("index.html");
    let svc = ServeDir::new(&dir)
        .append_index_html_on_directories(true)
        .fallback(ServeFile::new(&index));
    let mut res = match svc.oneshot(req).await {
        Ok(r) => r.into_response(),
        Err(_) => return ApiError::Internal.into_response(),
    };
    let h = res.headers_mut();
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CSP),
    );
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("same-origin"),
    );
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    // Hashed assets can be cached forever; the HTML shell must not be.
    let cache = if path.starts_with("/assets/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn knows_upstream_routes() {
        assert!(upstream_has_route("GET", "/api/v1/servers"));
        assert!(upstream_has_route("GET", "/ui/api/v1/servers"));
        assert!(upstream_has_route("POST", "/api/v1/servers/abc/validate"));
        assert!(!upstream_has_route("DELETE", "/api/v1/nope"));
        assert!(!upstream_has_route("GET", "/api/v1/nope"));
        assert!(!upstream_has_route("PUT", "/api/v1/servers"));
    }

    #[test]
    fn api_paths() {
        assert!(is_api_path("/api/v1/x"));
        assert!(is_api_path("/ui/session"));
        assert!(!is_api_path("/projects"));
        assert!(!is_api_path("/apis"));
    }
}
