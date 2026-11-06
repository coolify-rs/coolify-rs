//! Plugin endpoints (ADR 0010). Coolify has no plugins, so these are coolify-rs's own: none is in the upstream
//! route inventory, and none changes a route Coolify defines.
//!
//! - `GET /api/v1/plugins`, `GET /api/v1/plugins/{publisher}/{name}`: what is installed and whether the
//!   token's team enabled it.
//! - `POST /api/v1/plugins`: install or update (instance admins; only with
//!   `COOLIFY_RS_ALLOW_UNVERIFIED_PLUGINS` until the registry exists). The request must repeat the capability
//!   list as `grant`, so a client cannot grant what it never showed.
//! - `PATCH` and `DELETE /api/v1/plugins/{publisher}/{name}`: switch on or off, uninstall (instance admins).
//! - `POST /api/v1/plugins/{publisher}/{name}/enable` and `/disable`: for the token's team (team admins).
//! - `/api/v1/ext/{publisher}/{name}/...`: the plugin's own routes, any method.
//!
//! A plugin route never sees the caller's token or cookies. Its response can't set cookies or headers of
//! its own, and is served so a browser will not run it: HTML and SVG become downloads, with `nosniff` and a
//! sandboxing content security policy.

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, RawQuery, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::{from_fn, from_fn_with_state};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post};
use axum::{Extension, Json, Router};
use base64::Engine as _;
use coolify_domain::Ability;
use coolify_plugins::{CallError, Caller, Host, Installed, Manifest, Request};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::auth::AuthCtx;
use crate::response::{ApiError, ApiResult, created, ok};
use crate::state::AppState;

const READ: &[&[Ability]] = &[&[Ability::Read]];
const WRITE: &[&[Ability]] = &[&[Ability::Write]];

/// Request headers a plugin may see. Never `authorization` or `cookie`.
const FORWARDED_HEADERS: &[&str] = &[
    "accept",
    "accept-language",
    "content-type",
    "if-modified-since",
    "if-none-match",
    "user-agent",
];
/// Response headers a plugin may set.
const RETURNED_HEADERS: &[&str] = &["cache-control", "content-type", "etag", "last-modified"];
/// Content types served as they are; anything else becomes a download.
const INLINE_TYPES: &[&str] = &["application/json", "text/plain", "text/csv"];

pub fn routes(state: AppState) -> Router<AppState> {
    let guard = from_fn_with_state(state.clone(), crate::auth::main_guard);
    let read = || from_fn(|req, next| crate::auth::require_abilities(READ, req, next));
    let write = || from_fn(|req, next| crate::auth::require_abilities(WRITE, req, next));
    Router::new()
        .route("/api/v1/plugins", get(list).route_layer(read()))
        .route(
            "/api/v1/plugins",
            post(install)
                .route_layer(write())
                .layer(DefaultBodyLimit::max(32 * 1024 * 1024)),
        )
        .route(
            "/api/v1/plugins/{publisher}/{name}",
            get(show).route_layer(read()),
        )
        .route(
            "/api/v1/plugins/{publisher}/{name}",
            axum::routing::patch(update)
                .delete(uninstall)
                .route_layer(write()),
        )
        .route(
            "/api/v1/plugins/{publisher}/{name}/enable",
            post(enable).route_layer(write()),
        )
        .route(
            "/api/v1/plugins/{publisher}/{name}/disable",
            post(disable).route_layer(write()),
        )
        .route(
            "/api/v1/ext/{publisher}/{name}",
            any(ext_root)
                .layer::<_, std::convert::Infallible>(read())
                .layer(DefaultBodyLimit::max(1024 * 1024)),
        )
        .route(
            "/api/v1/ext/{publisher}/{name}/{*path}",
            any(ext)
                .layer::<_, std::convert::Infallible>(read())
                .layer(DefaultBodyLimit::max(1024 * 1024)),
        )
        .route_layer(guard)
}

fn host(state: &AppState) -> Result<&Host, ApiError> {
    state.plugins.as_ref().ok_or(ApiError::Status(
        StatusCode::NOT_FOUND,
        json!({"message": "Plugins are switched off on this instance."}),
    ))
}

fn instance_admin(ctx: &AuthCtx) -> Result<(), ApiError> {
    if ctx.team_id == 0 && ctx.is_team_admin() {
        Ok(())
    } else {
        Err(ApiError::Status(
            StatusCode::FORBIDDEN,
            json!({"message": "Only instance admins can manage plugins."}),
        ))
    }
}

fn not_found() -> ApiError {
    ApiError::Status(
        StatusCode::NOT_FOUND,
        json!({"message": "Plugin not found."}),
    )
}

fn internal(e: impl std::fmt::Display) -> ApiError {
    tracing::error!(error = %e, "plugin request failed");
    ApiError::Internal
}

fn present(p: &Installed, team_enabled: bool) -> Value {
    let m = &p.manifest;
    json!({
        "id": p.id,
        "name": m.name,
        "version": p.version,
        "description": m.description,
        "license": m.license,
        "source": m.source,
        "sha256": p.sha256,
        "origin": p.origin,
        "verified": p.verified,
        "enabled": p.enabled,
        "disabled_reason": p.disabled_reason,
        "capabilities": m.capability_summary(),
        "enabled_for_team": team_enabled,
    })
}

async fn list(State(state): State<AppState>, Extension(ctx): Extension<AuthCtx>) -> ApiResult {
    let host = host(&state)?;
    let mut items = Vec::new();
    for p in coolify_plugins::store::list(host.db())
        .await
        .map_err(internal)?
    {
        let on = coolify_plugins::store::enabled_for_team(host.db(), &p.id, ctx.team_id)
            .await
            .map_err(internal)?;
        items.push(present(&p, on));
    }
    Ok(ok(Value::Array(items)).into_response())
}

async fn find(host: &Host, id: &str) -> Result<Installed, ApiError> {
    coolify_plugins::store::find(host.db(), id)
        .await
        .map_err(internal)?
        .ok_or_else(not_found)
}

async fn show(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((publisher, name)): Path<(String, String)>,
) -> ApiResult {
    let host = host(&state)?;
    let id = format!("{publisher}/{name}");
    let p = find(host, &id).await?;
    let on = coolify_plugins::store::enabled_for_team(host.db(), &id, ctx.team_id)
        .await
        .map_err(internal)?;
    Ok(ok(present(&p, on)).into_response())
}

#[derive(Deserialize)]
struct InstallBody {
    /// `plugin.toml`.
    manifest: String,
    /// The component, base64.
    component: String,
    /// The capability list the admin approved; must equal the manifest's.
    #[serde(default)]
    grant: Option<Vec<String>>,
}

async fn install(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    body: Result<Json<InstallBody>, axum::extract::rejection::JsonRejection>,
) -> ApiResult {
    instance_admin(&ctx)?;
    let host = host(&state)?;
    if !state.allow_unverified_plugins {
        return Err(ApiError::Status(
            StatusCode::FORBIDDEN,
            json!({"message": "Installing plugins outside the registry is switched off. Set COOLIFY_RS_ALLOW_UNVERIFIED_PLUGINS=true to allow it."}),
        ));
    }
    let Json(body) = body.map_err(|e| {
        ApiError::Status(
            StatusCode::UNPROCESSABLE_ENTITY,
            json!({"message": e.body_text()}),
        )
    })?;
    let invalid =
        |m: String| ApiError::Status(StatusCode::UNPROCESSABLE_ENTITY, json!({"message": m}));
    let manifest = Manifest::parse(&body.manifest).map_err(|e| invalid(e.to_string()))?;
    let capabilities = manifest.capability_summary();
    if body.grant.as_ref() != Some(&capabilities) {
        return Err(ApiError::Status(
            StatusCode::UNPROCESSABLE_ENTITY,
            json!({
                "message": "Approve the plugin's capabilities by sending them back as grant.",
                "capabilities": capabilities,
            }),
        ));
    }
    let component = base64::engine::general_purpose::STANDARD
        .decode(body.component.trim())
        .map_err(|_| invalid("component must be base64".into()))?;
    let installed = host
        .install(manifest, &component, "upload", false, Some(ctx.user_id))
        .await
        .map_err(|e| match e {
            coolify_plugins::PluginError::Db(e) => internal(e),
            e => invalid(e.to_string()),
        })?;
    Ok(created(present(&installed, false)).into_response())
}

#[derive(Deserialize)]
struct UpdateBody {
    enabled: bool,
}

async fn update(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((publisher, name)): Path<(String, String)>,
    Json(body): Json<UpdateBody>,
) -> ApiResult {
    instance_admin(&ctx)?;
    let host = host(&state)?;
    let id = format!("{publisher}/{name}");
    find(host, &id).await?;
    host.set_enabled(&id, body.enabled, Some(ctx.user_id))
        .await
        .map_err(|e| match e {
            coolify_plugins::PluginError::Db(e) => internal(e),
            e => ApiError::Status(
                StatusCode::UNPROCESSABLE_ENTITY,
                json!({"message": e.to_string()}),
            ),
        })?;
    let p = find(host, &id).await?;
    let on = coolify_plugins::store::enabled_for_team(host.db(), &id, ctx.team_id)
        .await
        .map_err(internal)?;
    Ok(ok(present(&p, on)).into_response())
}

async fn uninstall(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((publisher, name)): Path<(String, String)>,
) -> ApiResult {
    instance_admin(&ctx)?;
    let host = host(&state)?;
    let id = format!("{publisher}/{name}");
    if !host
        .uninstall(&id, Some(ctx.user_id))
        .await
        .map_err(internal)?
    {
        return Err(not_found());
    }
    Ok(ok(json!({"message": "Plugin uninstalled."})).into_response())
}

async fn set_team(state: &AppState, ctx: &AuthCtx, id: &str, enabled: bool) -> ApiResult {
    if !ctx.is_team_admin() {
        return Err(ApiError::Status(
            StatusCode::FORBIDDEN,
            json!({"message": "Only team owners and admins can enable plugins."}),
        ));
    }
    let host = host(state)?;
    let p = find(host, id).await?;
    host.set_team(id, ctx.team_id, enabled, Some(ctx.user_id))
        .await
        .map_err(internal)?;
    Ok(ok(present(&p, enabled)).into_response())
}

async fn enable(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((publisher, name)): Path<(String, String)>,
) -> ApiResult {
    set_team(&state, &ctx, &format!("{publisher}/{name}"), true).await
}

async fn disable(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((publisher, name)): Path<(String, String)>,
) -> ApiResult {
    set_team(&state, &ctx, &format!("{publisher}/{name}"), false).await
}

async fn ext_root(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((publisher, name)): Path<(String, String)>,
    method: Method,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult {
    call(
        &state,
        &ctx,
        &format!("{publisher}/{name}"),
        "/".into(),
        method,
        query,
        &headers,
        body,
    )
    .await
}

async fn ext(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((publisher, name, path)): Path<(String, String, String)>,
    method: Method,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult {
    let path = format!("/{path}");
    call(
        &state,
        &ctx,
        &format!("{publisher}/{name}"),
        path,
        method,
        query,
        &headers,
        body,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn call(
    state: &AppState,
    ctx: &AuthCtx,
    id: &str,
    path: String,
    method: Method,
    query: Option<String>,
    headers: &HeaderMap,
    body: Bytes,
) -> ApiResult {
    let host = host(state)?;
    let request = Request {
        method: method.as_str().to_owned(),
        path,
        query: query.unwrap_or_default(),
        headers: headers
            .iter()
            .filter(|(k, _)| FORWARDED_HEADERS.contains(&k.as_str()))
            .filter_map(|(k, v)| Some((k.as_str().to_owned(), v.to_str().ok()?.to_owned())))
            .collect(),
        body: body.to_vec(),
    };
    let caller = Caller {
        team_id: ctx.team_id,
        user_id: ctx.user_id,
        abilities: ctx.abilities.as_slice().to_vec(),
    };
    let response = host
        .handle_request(id, caller, request)
        .await
        .map_err(|e| match e {
            CallError::Unavailable(_)
            | CallError::NotEnabledForTeam(_)
            | CallError::NotGranted(..) => ApiError::Status(
                StatusCode::NOT_FOUND,
                json!({"message": "Plugin not found, or not enabled for this team."}),
            ),
            CallError::Timeout(..) => ApiError::Status(
                StatusCode::GATEWAY_TIMEOUT,
                json!({"message": "The plugin did not answer in time."}),
            ),
            CallError::Trap(..) | CallError::Plugin(..) => {
                tracing::warn!(plugin = id, error = %e, "plugin route failed");
                ApiError::Status(
                    StatusCode::BAD_GATEWAY,
                    json!({"message": "The plugin failed."}),
                )
            }
            CallError::Store(e) => internal(e),
        })?;
    Ok(served(response))
}

/// The plugin's response, as safe to serve from coolify-rs's origin.
fn served(r: coolify_plugins::Response) -> Response {
    let status = StatusCode::from_u16(r.status)
        .ok()
        .filter(|s| (200..600).contains(&s.as_u16()))
        .unwrap_or(StatusCode::BAD_GATEWAY);
    let mut out = (status, r.body).into_response();
    let headers = out.headers_mut();
    for (name, value) in &r.headers {
        let lower = name.to_ascii_lowercase();
        if !RETURNED_HEADERS.contains(&lower.as_str()) {
            continue;
        }
        if let (Ok(name), Ok(value)) = (
            header::HeaderName::from_bytes(lower.as_bytes()),
            HeaderValue::from_str(value),
        ) {
            headers.insert(name, value);
        }
    }
    let inline = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|v| {
            v.split(';')
                .next()
                .unwrap_or_default()
                .trim()
                .to_ascii_lowercase()
        })
        .is_some_and(|t| INLINE_TYPES.contains(&t.as_str()));
    if !inline {
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/octet-stream"),
        );
        headers.insert(
            header::CONTENT_DISPOSITION,
            HeaderValue::from_static("attachment"),
        );
    }
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'none'; frame-ancestors 'none'; sandbox"),
    );
    headers.insert(
        header::HeaderName::from_static("cross-origin-resource-policy"),
        HeaderValue::from_static("same-origin"),
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(status: u16, headers: &[(&str, &str)]) -> coolify_plugins::Response {
        coolify_plugins::Response {
            status,
            headers: headers
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            body: b"<script>alert(1)</script>".to_vec(),
        }
    }

    #[test]
    fn plugin_responses_cannot_run_in_the_browser() {
        let r = served(response(
            200,
            &[
                ("Content-Type", "text/html"),
                ("Set-Cookie", "coolify_session=stolen"),
                ("Location", "https://evil.example"),
                ("Access-Control-Allow-Origin", "*"),
            ],
        ));
        let h = r.headers();
        assert_eq!(h["content-type"], "application/octet-stream");
        assert_eq!(h["content-disposition"], "attachment");
        assert_eq!(h["x-content-type-options"], "nosniff");
        assert!(
            h["content-security-policy"]
                .to_str()
                .unwrap()
                .contains("sandbox")
        );
        assert!(h.get("set-cookie").is_none());
        assert!(h.get("location").is_none());
        assert!(h.get("access-control-allow-origin").is_none());

        let json = served(response(
            201,
            &[("content-type", "application/json; charset=utf-8")],
        ));
        assert_eq!(json.status(), 201);
        assert_eq!(
            json.headers()["content-type"],
            "application/json; charset=utf-8"
        );
        assert!(json.headers().get("content-disposition").is_none());

        assert_eq!(served(response(101, &[])).status(), StatusCode::BAD_GATEWAY);
        assert_eq!(served(response(999, &[])).status(), StatusCode::BAD_GATEWAY);
    }
}
