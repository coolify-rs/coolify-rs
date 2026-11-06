//! Plugin pages of the web UI (ADR 0010, phase 3; ADR 0013). Coolify has no plugins, so none of this is a
//! port: it is coolify-rs's own, behind the session guard like every `/ui/...` endpoint (cookie, CSRF on
//! writes, forced reset and team two-factor gates). The rules match the bearer API in `routes/plugins.rs`:
//!
//! - Instance admins (owners and admins of the root team, `isInstanceAdmin()`) see every installed plugin,
//!   switch plugins on or off for the instance, uninstall them, and install uploads in development mode or
//!   from the registry catalogue.
//! - Team owners and admins switch a plugin on or off for their team; members see the list.
//! - Panels and `/ui/ext/...` reach a plugin only for the session's team, and only when that team enabled
//!   it.
//!
//! Endpoints:
//! - `GET /ui/plugins/nav`: whether the Settings and Team menus show a Plugins entry, and the panels of the
//!   session's team. 404 when plugins are switched off, so the UI adds nothing.
//! - `GET`, `POST /ui/plugins`: the settings page; install an upload (`{manifest, component, grant}`).
//! - `PATCH`, `DELETE /ui/plugins/{publisher}/{name}`: instance-wide switch; uninstall (password step).
//! - `GET /ui/plugins/team`, `POST /ui/plugins/{publisher}/{name}/team`: the team page and its switch.
//! - `GET /ui/plugins/catalog`, `POST /ui/plugins/catalog/refresh`, `POST /ui/plugins/catalog/install`,
//!   `POST /ui/plugins/{publisher}/{name}/update`: the registry catalogue, through [`catalog`].
//! - `GET /ui/plugins/{publisher}/{name}/panels/{slot}?panel=&resource=`: one panel's document, for a
//!   sandboxed frame (see [`panel`]).
//! - `/ui/ext/{publisher}/{name}/...`: the plugin's own routes for the session's team, any method, served
//!   with the same hardening as `/api/v1/ext` (`routes::plugins::served`). Panels reach these through the
//!   page's message bridge, never directly.

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, Query, RawQuery, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::from_fn_with_state;
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post};
use axum::{Extension, Router};
use base64::Engine as _;
use coolify_plugins::{Caller, Installed, Manifest, PanelError};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::auth::AuthCtx;
use crate::response::{ApiError, ApiResult, created, ok};
use crate::routes::plugins::{call, find, host, internal, not_found, present};
use crate::state::AppState;

pub fn routes(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/ui/plugins/nav", get(nav))
        .route(
            "/ui/plugins",
            get(settings_page)
                .post(upload)
                // A 20 MiB component is about 27 MiB of base64.
                .layer(DefaultBodyLimit::max(32 * 1024 * 1024)),
        )
        .route("/ui/plugins/team", get(team_page))
        .route("/ui/plugins/catalog", get(catalog_list))
        .route("/ui/plugins/catalog/refresh", post(catalog_refresh))
        .route("/ui/plugins/catalog/install", post(catalog_install))
        .route(
            "/ui/plugins/{publisher}/{name}",
            axum::routing::patch(set_enabled).delete(uninstall),
        )
        .route("/ui/plugins/{publisher}/{name}/team", post(set_team))
        .route(
            "/ui/plugins/{publisher}/{name}/update",
            post(catalog_update),
        )
        .route("/ui/plugins/{publisher}/{name}/panels/{slot}", get(panel))
        .route(
            "/ui/ext/{publisher}/{name}",
            any(ext_root).layer(DefaultBodyLimit::max(1024 * 1024)),
        )
        .route(
            "/ui/ext/{publisher}/{name}/{*path}",
            any(ext).layer(DefaultBodyLimit::max(1024 * 1024)),
        )
        .route_layer(from_fn_with_state(state, crate::session::ui_guard))
}

/// The policy of a panel document. The frame also carries `sandbox="allow-scripts"`; the header's
/// `sandbox` keeps the document in an opaque origin even if it is opened outside the frame.
pub const PANEL_CSP: &str = "sandbox allow-scripts; default-src 'none'; script-src 'unsafe-inline'; \
    style-src 'unsafe-inline'; img-src data:; connect-src 'none'; base-uri 'none'; form-action 'none'; \
    frame-ancestors 'self'";
/// The same policy as the document's first element, so it holds whatever the plugin's HTML declares.
pub const PANEL_CSP_META: &str = "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src data:; connect-src 'none'";

fn forbidden(message: &str) -> ApiError {
    ApiError::Status(StatusCode::FORBIDDEN, json!({ "message": message }))
}

fn invalid(message: impl Into<String>) -> ApiError {
    ApiError::Status(
        StatusCode::UNPROCESSABLE_ENTITY,
        json!({ "message": message.into() }),
    )
}

/// `isInstanceAdmin()`: owner or admin of the root team, whichever team the session is on. The bearer API
/// asks the same of a token, whose team must be the root team.
async fn instance_admin(state: &AppState, ctx: &AuthCtx) -> Result<(), ApiError> {
    if coolify_db::instance_settings::is_instance_admin(&state.db, ctx.user_id).await? {
        Ok(())
    } else {
        Err(forbidden("Only instance admins can manage plugins."))
    }
}

fn team_admin(ctx: &AuthCtx) -> Result<(), ApiError> {
    if ctx.is_team_admin() {
        Ok(())
    } else {
        Err(forbidden("Only team owners and admins can enable plugins."))
    }
}

fn plugin_error(e: coolify_plugins::PluginError) -> ApiError {
    match e {
        coolify_plugins::PluginError::Db(e) => internal(e),
        e => invalid(e.to_string()),
    }
}

/// An installed plugin for the settings page: the API's shape and its panels.
fn settings_item(p: &Installed, team_enabled: bool) -> Value {
    let mut v = present(p, team_enabled);
    v["panels"] = p
        .granted
        .ui
        .iter()
        .map(|x| json!({ "slot": x.slot, "title": x.title }))
        .collect();
    v
}

// ---- Navigation and panels ----

async fn nav(State(state): State<AppState>, Extension(ctx): Extension<AuthCtx>) -> ApiResult {
    let host = host(&state)?;
    let installed = coolify_plugins::store::list(host.db())
        .await
        .map_err(internal)?;
    let admin = coolify_db::instance_settings::is_instance_admin(&state.db, ctx.user_id).await?;
    let settings = admin
        && (!installed.is_empty()
            || state.allow_unverified_plugins
            || catalog(&state, &ctx, CatalogOp::Available).await.is_some());
    let panels: Vec<Value> = host
        .panels_for_team(ctx.team_id)
        .await
        .map_err(internal)?
        .into_iter()
        .map(|p| {
            json!({
                "plugin": p.plugin,
                "name": p.name,
                "verified": p.verified,
                "slot": p.panel.slot,
                "index": p.index,
                "title": p.panel.title,
            })
        })
        .collect();
    Ok(ok(json!({
        "settings": settings,
        "team": installed.iter().any(|p| p.enabled),
        "panels": panels,
    }))
    .into_response())
}

#[derive(Deserialize)]
struct PanelQuery {
    #[serde(default)]
    panel: Option<usize>,
    #[serde(default)]
    resource: Option<String>,
    /// The page's theme, so the document starts in the right colour scheme before the bridge says so.
    #[serde(default)]
    theme: Option<String>,
}

/// Whether the request is a frame of this origin loading the document. Browsers send `Sec-Fetch-*` on
/// secure origins; over plain HTTP they may not, and the document is still sandboxed by its policy.
fn framed_here(headers: &HeaderMap) -> bool {
    let is = |name: &str, want: &str| {
        headers
            .get(name)
            .is_none_or(|v| v.to_str().is_ok_and(|v| v.eq_ignore_ascii_case(want)))
    };
    is("sec-fetch-dest", "iframe") && is("sec-fetch-site", "same-origin")
}

fn hardened(status: StatusCode, content_type: &'static str, body: String) -> Response {
    let mut res = (status, body).into_response();
    let h = res.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(PANEL_CSP),
    );
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(
        header::X_FRAME_OPTIONS,
        HeaderValue::from_static("SAMEORIGIN"),
    );
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(
        header::HeaderName::from_static("cross-origin-resource-policy"),
        HeaderValue::from_static("same-origin"),
    );
    res
}

/// The panel's HTML as a document: our policy first, the page's colour scheme, then the plugin's markup
/// without its doctype.
pub fn panel_document(html: &str, dark: bool) -> String {
    let mut rest = html.trim_start();
    if rest
        .get(..9)
        .is_some_and(|s| s.eq_ignore_ascii_case("<!doctype"))
    {
        rest = rest.find('>').map_or("", |i| &rest[i + 1..]);
    }
    let scheme = if dark { "dark" } else { "light" };
    format!(
        "<!doctype html><meta charset=\"utf-8\"><meta http-equiv=\"Content-Security-Policy\" content=\"{PANEL_CSP_META}\"><meta name=\"color-scheme\" content=\"{scheme}\">{rest}"
    )
}

/// One panel, as the document of a sandboxed frame. The page lists panels from [`nav`] and points a frame
/// with `sandbox="allow-scripts"` here; the response's policy keeps the document in an opaque origin with
/// no network access, so it can't read cookies or the page, and talks to its plugin only through the
/// page's message bridge.
async fn panel(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((publisher, name, slot)): Path<(String, String, String)>,
    Query(q): Query<PanelQuery>,
    headers: HeaderMap,
) -> ApiResult {
    let host = host(&state)?;
    if !framed_here(&headers) {
        return Ok(hardened(
            StatusCode::FORBIDDEN,
            "text/plain; charset=utf-8",
            "Plugin panels are only shown inside coolify-rs.".into(),
        ));
    }
    if !coolify_plugins::UI_SLOTS.contains(&slot.as_str()) {
        return Err(not_found());
    }
    let caller = Caller {
        team_id: ctx.team_id,
        user_id: ctx.user_id,
        abilities: ctx.abilities.as_slice().to_vec(),
    };
    let id = format!("{publisher}/{name}");
    let result = host
        .panel(
            &id,
            caller,
            &slot,
            q.panel.unwrap_or(0),
            q.resource.as_deref().filter(|r| !r.is_empty()),
        )
        .await;
    let failed = |status: StatusCode, message: &str| {
        Ok(hardened(
            status,
            "text/plain; charset=utf-8",
            message.to_owned(),
        ))
    };
    match result {
        Ok(html) => Ok(hardened(
            StatusCode::OK,
            "text/html; charset=utf-8",
            panel_document(&html, q.theme.as_deref() == Some("dark")),
        )),
        Err(PanelError::NoPanel)
        | Err(PanelError::Call(
            coolify_plugins::CallError::Unavailable(_)
            | coolify_plugins::CallError::NotEnabledForTeam(_)
            | coolify_plugins::CallError::NotGranted(..),
        )) => failed(
            StatusCode::NOT_FOUND,
            "Plugin not found, or not enabled for this team.",
        ),
        Err(PanelError::BadResource) => failed(StatusCode::BAD_REQUEST, "Invalid resource."),
        Err(PanelError::Call(coolify_plugins::CallError::Timeout(..))) => failed(
            StatusCode::GATEWAY_TIMEOUT,
            "The plugin did not answer in time.",
        ),
        Err(PanelError::Call(coolify_plugins::CallError::Store(e))) => Err(internal(e)),
        Err(e) => {
            tracing::warn!(plugin = %id, error = %e, "plugin panel failed");
            failed(StatusCode::BAD_GATEWAY, "The plugin's panel failed.")
        }
    }
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
    let id = format!("{publisher}/{name}");
    call(&state, &ctx, &id, "/".into(), method, query, &headers, body).await
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
    let id = format!("{publisher}/{name}");
    let path = format!("/{path}");
    call(&state, &ctx, &id, path, method, query, &headers, body).await
}

// ---- Settings: installed plugins ----

async fn settings_page(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    headers: HeaderMap,
) -> ApiResult {
    let host = host(&state)?;
    instance_admin(&state, &ctx).await?;
    let mut items = Vec::new();
    for p in coolify_plugins::store::list(host.db())
        .await
        .map_err(internal)?
    {
        let on = coolify_plugins::store::enabled_for_team(host.db(), &p.id, ctx.team_id)
            .await
            .map_err(internal)?;
        items.push(settings_item(&p, on));
    }
    let (text, password) = crate::ui_teams::confirm_steps(&state, &headers, ctx.user_id).await?;
    Ok(ok(json!({
        "plugins": items,
        "allow_upload": state.allow_unverified_plugins,
        "confirm": {"text": text, "password": password},
    }))
    .into_response())
}

#[derive(Deserialize)]
struct UploadBody {
    manifest: String,
    component: String,
    #[serde(default)]
    grant: Option<Vec<String>>,
}

/// Installs an uploaded plugin, as `POST /api/v1/plugins` does: the first request answers the capability
/// list, and only a request that repeats it as `grant` installs.
async fn upload(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    body: Bytes,
) -> ApiResult {
    let host = host(&state)?;
    instance_admin(&state, &ctx).await?;
    if !state.allow_unverified_plugins {
        return Err(forbidden(
            "Installing plugins outside the registry is switched off. Set COOLIFY_RS_ALLOW_UNVERIFIED_PLUGINS=true to allow it.",
        ));
    }
    let body: UploadBody = serde_json::from_slice(&body)
        .map_err(|_| invalid("Choose a plugin.toml and a .wasm component."))?;
    let manifest = Manifest::parse(&body.manifest).map_err(|e| invalid(e.to_string()))?;
    let capabilities = manifest.capability_summary();
    if body.grant.as_ref() != Some(&capabilities) {
        return Err(ApiError::Status(
            StatusCode::UNPROCESSABLE_ENTITY,
            json!({
                "message": "Approve the plugin's capabilities by sending them back as grant.",
                "capabilities": capabilities,
                "plugin": {
                    "id": manifest.id,
                    "name": manifest.name,
                    "version": manifest.version,
                    "description": manifest.description,
                    "license": manifest.license,
                    "source": manifest.source,
                },
            }),
        ));
    }
    let component = base64::engine::general_purpose::STANDARD
        .decode(body.component.trim())
        .map_err(|_| invalid("component must be base64"))?;
    let installed = host
        .install(manifest, &component, "upload", false, Some(ctx.user_id))
        .await
        .map_err(plugin_error)?;
    Ok(created(settings_item(&installed, false)).into_response())
}

#[derive(Deserialize)]
struct EnabledBody {
    enabled: bool,
}

fn parse<T: serde::de::DeserializeOwned>(body: &Bytes) -> Result<T, ApiError> {
    serde_json::from_slice(body).map_err(|e| invalid(e.to_string()))
}

async fn set_enabled(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((publisher, name)): Path<(String, String)>,
    body: Bytes,
) -> ApiResult {
    let host = host(&state)?;
    instance_admin(&state, &ctx).await?;
    let body: EnabledBody = parse(&body)?;
    let id = format!("{publisher}/{name}");
    find(host, &id).await?;
    host.set_enabled(&id, body.enabled, Some(ctx.user_id))
        .await
        .map_err(plugin_error)?;
    let p = find(host, &id).await?;
    let on = coolify_plugins::store::enabled_for_team(host.db(), &id, ctx.team_id)
        .await
        .map_err(internal)?;
    Ok(ok(settings_item(&p, on)).into_response())
}

/// Uninstalls after the confirmation modal: the typed name is checked by the page, the password here.
async fn uninstall(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((publisher, name)): Path<(String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult {
    let host = host(&state)?;
    instance_admin(&state, &ctx).await?;
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    if !crate::ui_teams::session_password_ok(&state, &headers, ctx.user_id, &body).await? {
        return Err(ApiError::Status(
            StatusCode::UNPROCESSABLE_ENTITY,
            json!({"message": "The provided password is incorrect.", "errors": {"password": ["The provided password is incorrect."]}}),
        ));
    }
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

// ---- Team: per-team switch ----

async fn team_page(State(state): State<AppState>, Extension(ctx): Extension<AuthCtx>) -> ApiResult {
    let host = host(&state)?;
    let mut items = Vec::new();
    for p in coolify_plugins::store::list(host.db())
        .await
        .map_err(internal)?
    {
        if !p.enabled {
            continue;
        }
        let on = coolify_plugins::store::enabled_for_team(host.db(), &p.id, ctx.team_id)
            .await
            .map_err(internal)?;
        items.push(settings_item(&p, on));
    }
    Ok(ok(json!({"plugins": items, "can_manage": ctx.is_team_admin()})).into_response())
}

async fn set_team(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((publisher, name)): Path<(String, String)>,
    body: Bytes,
) -> ApiResult {
    let host = host(&state)?;
    team_admin(&ctx)?;
    let body: EnabledBody = parse(&body)?;
    let id = format!("{publisher}/{name}");
    let p = find(host, &id).await?;
    if body.enabled && !p.enabled {
        return Err(invalid("The plugin is switched off for this instance."));
    }
    host.set_team(&id, ctx.team_id, body.enabled, Some(ctx.user_id))
        .await
        .map_err(internal)?;
    Ok(ok(settings_item(&p, body.enabled)).into_response())
}

// ---- Settings: the registry catalogue ----

/// A request of the settings page to the registry catalogue.
#[derive(Debug)]
pub(crate) enum CatalogOp<'a> {
    /// Whether this instance has a catalogue at all.
    Available,
    /// `GET /api/v1/plugins/catalog`: `[{id, name, description, license, source, latest_version,
    /// capabilities, installed_version, update_available, pending_update, yanked, revoked}]`.
    List,
    /// `POST /api/v1/plugins/catalog/refresh`.
    Refresh,
    /// `POST /api/v1/plugins/catalog/install` with `{id, version?, grant?}`; without the grant it answers
    /// 422 `{message, capabilities}`.
    Install(&'a Value),
    /// `POST /api/v1/plugins/{publisher}/{name}/update` with `{version?, grant?}`.
    Update(&'a str, &'a Value),
}

/// The one place the web UI reaches the registry catalogue (ADR 0010, phase 2), as a status and a JSON
/// body in the shapes of the catalogue API. `None` means this instance has no catalogue: the endpoints
/// answer 404 and the page hides the section. The caller has already checked that `ctx` is an instance
/// admin.
pub(crate) async fn catalog(
    state: &AppState,
    ctx: &AuthCtx,
    op: CatalogOp<'_>,
) -> Option<Result<(StatusCode, Value), ApiError>> {
    // The registry client lands with phase 2; until then there is nothing to proxy.
    let _ = (state, ctx);
    match op {
        CatalogOp::Available | CatalogOp::List | CatalogOp::Refresh => None,
        CatalogOp::Install(body) => {
            let _ = body;
            None
        }
        CatalogOp::Update(id, body) => {
            let _ = (id, body);
            None
        }
    }
}

async fn proxied(state: &AppState, ctx: &AuthCtx, op: CatalogOp<'_>) -> ApiResult {
    host(state)?;
    instance_admin(state, ctx).await?;
    match catalog(state, ctx, op).await {
        None => Err(ApiError::Status(
            StatusCode::NOT_FOUND,
            json!({"message": "The plugin catalogue is not available on this instance."}),
        )),
        Some(Ok((status, body))) => Ok(crate::response::PhpJson(status, body).into_response()),
        Some(Err(e)) => Err(e),
    }
}

async fn catalog_list(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
) -> ApiResult {
    proxied(&state, &ctx, CatalogOp::List).await
}

async fn catalog_refresh(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
) -> ApiResult {
    proxied(&state, &ctx, CatalogOp::Refresh).await
}

async fn catalog_install(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    body: Bytes,
) -> ApiResult {
    let body: Value = parse(&body)?;
    proxied(&state, &ctx, CatalogOp::Install(&body)).await
}

async fn catalog_update(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((publisher, name)): Path<(String, String)>,
    body: Bytes,
) -> ApiResult {
    let body: Value = if body.is_empty() {
        json!({})
    } else {
        parse(&body)?
    };
    let id = format!("{publisher}/{name}");
    proxied(&state, &ctx, CatalogOp::Update(&id, &body)).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panel_documents_start_with_the_policy() {
        let doc = panel_document(
            "  <!DOCTYPE html><html><head><meta http-equiv=\"Content-Security-Policy\" content=\"default-src *\"></head><body>hi</body></html>",
            true,
        );
        assert!(doc.starts_with(&format!(
            "<!doctype html><meta charset=\"utf-8\"><meta http-equiv=\"Content-Security-Policy\" content=\"{PANEL_CSP_META}\"><meta name=\"color-scheme\" content=\"dark\"><html>"
        )));
        assert_eq!(
            doc.matches("<!doctype").count() + doc.matches("<!DOCTYPE").count(),
            1
        );
        assert!(panel_document("<p>x</p>", false).ends_with("<p>x</p>"));
        assert!(panel_document("<!doctype", false).ends_with("content=\"light\">"));
    }

    #[test]
    fn panels_load_only_in_a_frame_of_this_origin() {
        let mut h = HeaderMap::new();
        assert!(framed_here(&h), "plain HTTP sends no fetch metadata");
        h.insert("sec-fetch-dest", HeaderValue::from_static("iframe"));
        h.insert("sec-fetch-site", HeaderValue::from_static("same-origin"));
        assert!(framed_here(&h));
        h.insert("sec-fetch-dest", HeaderValue::from_static("document"));
        assert!(!framed_here(&h), "opened as a page");
        h.insert("sec-fetch-dest", HeaderValue::from_static("iframe"));
        h.insert("sec-fetch-site", HeaderValue::from_static("cross-site"));
        assert!(!framed_here(&h), "framed by another site");
    }
}
