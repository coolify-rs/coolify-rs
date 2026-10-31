//! `auditLog()` for request handlers: fills the actor from the token or session
//! and the request's address and user agent, then writes the `audit_events` row.
//! A failure is logged and never fails the request, as upstream's deferred write.

use axum::extract::{ConnectInfo, FromRequestParts};
use axum::http::header;
use axum::http::request::Parts;
use coolify_db::audit::{self, Actor};
use serde_json::Value;
use std::convert::Infallible;
use std::net::{Ipv4Addr, SocketAddr};

use crate::auth::AuthCtx;
use crate::state::AppState;

/// The request details an audit row stores.
#[derive(Debug, Clone, Default)]
pub struct Meta {
    pub ip: Option<String>,
    pub user_agent: Option<String>,
}

impl FromRequestParts<AppState> for Meta {
    type Rejection = Infallible;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let peer = parts
            .extensions
            .get::<ConnectInfo<SocketAddr>>()
            .map(|c| c.0)
            .unwrap_or_else(|| SocketAddr::from((Ipv4Addr::LOCALHOST, 0)));
        let ip = state.trusted_proxies.client_ip(peer, &parts.headers);
        let ua = parts
            .headers
            .get(header::USER_AGENT)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.chars().take(200).collect::<String>())
            .unwrap_or_default();
        Ok(Self {
            ip: Some(ip.to_string()),
            user_agent: Some(ua),
        })
    }
}

async fn actor(state: &AppState, ctx: &AuthCtx, meta: &Meta) -> Actor {
    let user = audit::actor_user(&state.db, ctx.user_id)
        .await
        .ok()
        .flatten();
    let token_id = (ctx.token_id != 0).then_some(ctx.token_id);
    let token_name = match token_id {
        Some(id) => audit::token_name(&state.db, id).await.ok().flatten(),
        None => None,
    };
    Actor {
        user_id: Some(ctx.user_id),
        name: user.as_ref().map(|u| u.0.clone()),
        email: user.map(|u| u.1),
        token_id,
        token_name,
        ip: meta.ip.clone(),
        user_agent: meta.user_agent.clone(),
    }
}

/// `auditLog($event, $context, $level)` inside an authenticated request.
pub async fn log(
    state: &AppState,
    ctx: &AuthCtx,
    meta: &Meta,
    event: &str,
    context: Value,
    level: &str,
) {
    let actor = actor(state, ctx, meta).await;
    let team = Some(ctx.team_id);
    if let Err(e) = audit::record(
        &state.db,
        team,
        event,
        &context,
        level,
        &actor,
        None,
        state.now(),
    )
    .await
    {
        tracing::warn!(error = %e, event, "audit event persistence failed");
    }
}

/// `Auditable::recordAuditMutation`: `{api|ui}.{resource}.{action}` with `changes`.
pub async fn mutation(
    state: &AppState,
    ctx: &AuthCtx,
    meta: &Meta,
    resource: &str,
    action: &str,
    context: Value,
    changes: Value,
) {
    let source = if ctx.token_id != 0 { "api" } else { "ui" };
    let event = format!("{source}.{resource}.{action}");
    let changes = state
        .encrypter
        .as_ref()
        .map(|e| e.encrypt_string(&coolify_compat::php_json::encode(&changes)));
    let actor = actor(state, ctx, meta).await;
    if let Err(e) = audit::record(
        &state.db,
        Some(ctx.team_id),
        &event,
        &context,
        "info",
        &actor,
        changes.as_deref(),
        state.now(),
    )
    .await
    {
        tracing::warn!(error = %e, event, "audit event persistence failed");
    }
}

/// `auditLog` without a session or token: scheduler, jobs, webhooks and failed logins.
pub async fn log_system(
    state: &AppState,
    team_id: Option<i64>,
    event: &str,
    context: Value,
    level: &str,
    meta: &Meta,
) {
    let actor = Actor {
        ip: meta.ip.clone(),
        user_agent: meta.user_agent.clone(),
        ..Actor::default()
    };
    if let Err(e) = audit::record(
        &state.db,
        team_id,
        event,
        &context,
        level,
        &actor,
        None,
        state.now(),
    )
    .await
    {
        tracing::warn!(error = %e, event, "audit event persistence failed");
    }
}
