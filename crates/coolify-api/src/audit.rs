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
        from_context: false,
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

/// PHP `! $changes` for the `changes` of a model mutation: null or an empty array.
pub(crate) fn changes_are_empty(changes: &Value) -> bool {
    match changes {
        Value::Null => true,
        Value::Array(a) => a.is_empty(),
        Value::Object(o) => o.is_empty(),
        _ => false,
    }
}

/// `auditFailureDeduplicationWindow()` for a refused API request (4.7.5): `api.auth.ability_denied`
/// and `api.auth.scope_denied` are recorded once per token (and required abilities, and reason)
/// every 300 seconds, like `api.auth.unauthenticated` is per source address. True when this
/// failure is the first of its window and gets a row.
pub(crate) fn first_denial_in_window(
    state: &AppState,
    event: &str,
    token_id: i64,
    reason: Option<&str>,
    required_abilities: &[&str],
) -> bool {
    // The token identifies the client, so a token used from many addresses gets one row. Token
    // ids are per database, so the database is part of the key (upstream's cache is per instance).
    let options = state.db.pool().connect_options();
    let key = denial_key(
        options.get_database().unwrap_or_default(),
        event,
        token_id,
        reason,
        required_abilities,
    );
    crate::rules::throttle(&key, 1, std::time::Duration::from_secs(300)).is_none()
}

/// The fingerprint of `shouldRecordAuditEvent()` for a refused API request: the event, the token,
/// the reason and the required abilities. The path is not part of it.
pub(crate) fn denial_key(
    database: &str,
    event: &str,
    token_id: i64,
    reason: Option<&str>,
    required_abilities: &[&str],
) -> String {
    format!(
        "audit-failure-dedup:{database}:{event}:{token_id}:{}:{}",
        reason.unwrap_or_default(),
        required_abilities.join(",")
    )
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
    // 4.7.5 (b7d0e6be2): `recordWithChanges($event, $context, $changes ?: null)`: no empty
    // `changes` value.
    let changes = if changes_are_empty(&changes) {
        None
    } else {
        state
            .encrypter
            .as_ref()
            .map(|e| e.encrypt_string(&coolify_compat::php_json::encode(&changes)))
    };
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// `recordModelMutation` (4.7.5): `$changes ?: null`.
    #[test]
    fn empty_changes_are_not_stored() {
        assert!(changes_are_empty(&json!({})));
        assert!(changes_are_empty(&json!([])));
        assert!(changes_are_empty(&Value::Null));
        assert!(!changes_are_empty(
            &json!({"name": {"old": "a", "new": "b"}})
        ));
    }

    /// `shouldRecordAuditEvent` (4.7.5): the token, the reason and the required abilities make
    /// the fingerprint of a refused API request.
    #[test]
    fn denial_fingerprints() {
        let a = denial_key("db", "api.auth.ability_denied", 7, Some("r"), &["write"]);
        assert_eq!(
            a,
            denial_key("db", "api.auth.ability_denied", 7, Some("r"), &["write"])
        );
        assert_ne!(
            a,
            denial_key("db", "api.auth.ability_denied", 8, Some("r"), &["write"])
        );
        assert_ne!(
            a,
            denial_key("db", "api.auth.ability_denied", 7, Some("r"), &["deploy"])
        );
        assert_ne!(
            a,
            denial_key("db", "api.auth.scope_denied", 7, Some("r"), &["write"])
        );
        assert_ne!(
            a,
            denial_key("other", "api.auth.ability_denied", 7, Some("r"), &["write"])
        );
    }
}
