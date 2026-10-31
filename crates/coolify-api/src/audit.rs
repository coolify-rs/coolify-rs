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

/// `auditLog` with the row's team taken from the context, and an actor read before the request
/// removed the user (`array_key_exists('team_id', $context)` in `attributesFor`).
pub async fn log_as(
    state: &AppState,
    team_id: Option<i64>,
    actor: &Actor,
    event: &str,
    context: Value,
) {
    if let Err(e) = audit::record(
        &state.db,
        team_id,
        event,
        &context,
        "info",
        actor,
        None,
        state.now(),
    )
    .await
    {
        tracing::warn!(error = %e, event, "audit event persistence failed");
    }
}

pub async fn actor(state: &AppState, ctx: &AuthCtx, meta: &Meta) -> Actor {
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

/// `Auditable` on `Team` (`ui.team.created`, `updated`, `deleted`): `$old` and `$new` are the raw
/// rows; only `updated` compares them, and every other action lists the row's attributes.
pub async fn team_mutation(
    state: &AppState,
    actor: &Actor,
    team_id: i64,
    action: &str,
    old: Option<&serde_json::Map<String, Value>>,
    new: Option<&serde_json::Map<String, Value>>,
) {
    // `isExcludedAuditField` for the columns of `teams`.
    let excluded = |k: &str| {
        k == "id" || k.ends_with("_id") || k.ends_with("_at") || audit::is_sensitive_field(k)
    };
    let (Some(row), other) = (new.or(old), old) else {
        return;
    };
    let mut changes = serde_json::Map::new();
    let mut changed_fields = Vec::new();
    // On `created`, Eloquent holds only what was assigned: the `$attributes` defaults first.
    let assigned: serde_json::Map<String, Value> = if action == "created" {
        let personal = row.get("personal_team") == Some(&Value::Bool(true));
        let fields: &[&str] = if personal {
            &[
                "is_mcp_server_enabled",
                "is_build_server_fallback_enabled",
                "is_two_factor_required",
                "name",
                "personal_team",
                "show_boarding",
            ]
        } else {
            &[
                "is_mcp_server_enabled",
                "is_build_server_fallback_enabled",
                "is_two_factor_required",
                "name",
                "description",
                "personal_team",
            ]
        };
        fields
            .iter()
            .filter_map(|k| row.get(*k).map(|v| ((*k).to_string(), v.clone())))
            .collect()
    } else {
        row.clone()
    };
    for (k, v) in &assigned {
        if action == "updated" {
            let before = other.and_then(|o| o.get(k));
            if before == Some(v) || k == "updated_at" {
                continue;
            }
            changed_fields.push(k.clone());
        }
        if excluded(k) {
            continue;
        }
        let before = other.and_then(|o| o.get(k)).cloned().unwrap_or(Value::Null);
        let (old_v, new_v) = match action {
            "created" => (Value::Null, v.clone()),
            "deleted" => (before, Value::Null),
            _ => (before, v.clone()),
        };
        changes.insert(k.clone(), serde_json::json!({"old": old_v, "new": new_v}));
    }
    if action == "updated" && changed_fields.is_empty() {
        return;
    }
    let event = format!("ui.team.{action}");
    let context = serde_json::json!({
        "team_id": team_id,
        "team_uuid": null,
        "team_name": row.get("name"),
        "changed_fields": changed_fields,
    });
    let encrypted = state
        .encrypter
        .as_ref()
        .map(|e| e.encrypt_string(&coolify_compat::php_json::encode(&Value::Object(changes))));
    if let Err(e) = audit::record(
        &state.db,
        Some(team_id),
        &event,
        &context,
        "info",
        actor,
        encrypted.as_deref(),
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
