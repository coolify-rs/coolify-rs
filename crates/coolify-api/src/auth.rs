//! The upstream API middleware chain.
//!
//! Main `/api/v1` group (`routes/api.php:65`), in order:
//! `auth:sanctum` → `api.token.team` → `ApiAllowed` (API enabled, then
//! `ApiIpAllowed`) → `api.sensitive` → per-route `api.ability:*`.
//!
//! `/api/v1/disable` group (`routes/api.php:56`): `api.ip.allowed` →
//! `auth:sanctum` → `api.token.team` → `api.ability:write`.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderMap, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use coolify_compat::parse_bearer_token;
use coolify_db::{auth, instance};
use coolify_domain::access::{AbilityCheck, check_any};
use coolify_domain::{Abilities, Ability, Role, TeamId};

use crate::ip::api_ip_allowed;
use crate::response::ApiError;
use crate::state::AppState;

/// Authenticated request context, inserted as a request extension.
#[derive(Debug, Clone)]
pub struct AuthCtx {
    pub token_id: i64,
    pub user_id: i64,
    /// The token's team: every query is scoped to it.
    pub team_id: TeamId,
    /// Raw `team_id` column; upstream compares it to the string `'0'`.
    pub team_id_raw: String,
    pub role: Role,
    pub abilities: Abilities,
    /// `api.sensitive`: (root | read:sensitive) and admin of the token team.
    pub can_read_sensitive: bool,
}

impl AuthCtx {
    pub fn is_team_admin(&self) -> bool {
        self.role.is_admin()
    }
}

pub(crate) fn client_ip(state: &AppState, req: &Request) -> IpAddr {
    let peer = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0)
        .unwrap_or_else(|| SocketAddr::from((Ipv4Addr::LOCALHOST, 0)));
    state.trusted_proxies.client_ip(peer, req.headers())
}

/// Laravel `Request::bearerToken`.
fn bearer(headers: &HeaderMap) -> Option<String> {
    let v = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let pos = v.to_ascii_lowercase().find("bearer ")?;
    let token = v[pos + 7..].trim();
    (!token.is_empty()).then(|| token.split(',').next().unwrap_or(token).trim().to_string())
}

/// `auth:sanctum` + `api.token.team`.
async fn authenticate(state: &AppState, headers: &HeaderMap) -> Result<AuthCtx, ApiError> {
    let raw = bearer(headers).ok_or(ApiError::Unauthenticated)?;
    let parsed = parse_bearer_token(&raw).ok_or(ApiError::Unauthenticated)?;
    let now = state.now();
    let token = auth::find_token(&state.db, &parsed, now)
        .await?
        .ok_or(ApiError::Unauthenticated)?;
    if state.db.ensure_writable().is_ok()
        && let Err(e) = auth::touch_last_used(&state.db, token.id, now).await
    {
        tracing::warn!(error = %e, "could not update token last_used_at");
    }

    let team_id = token.team_id().ok_or(ApiError::InvalidToken)?;
    let role = auth::membership_role(&state.db, team_id, token.user_id)
        .await?
        .ok_or(ApiError::InvalidToken)?;
    let abilities = Abilities::from_column(token.abilities_raw.as_deref());
    if !role.is_admin() && !abilities.elevated().is_empty() {
        return Err(ApiError::MissingTeamRole);
    }
    let can_read_sensitive =
        (abilities.can(Ability::Root) || abilities.can(Ability::ReadSensitive)) && role.is_admin();
    Ok(AuthCtx {
        token_id: token.id,
        user_id: token.user_id,
        team_id,
        team_id_raw: token.team_id_raw,
        role,
        abilities,
        can_read_sensitive,
    })
}

async fn ip_check(state: &AppState, client: IpAddr) -> Result<(), ApiError> {
    let settings = instance::api_settings(&state.db).await?;
    if api_ip_allowed(settings.allowed_ips.as_deref(), client) {
        Ok(())
    } else {
        Err(ApiError::IpNotAllowed)
    }
}

/// `Handler::unauthenticated` for `api/*`: `auditLog('api.auth.unauthenticated', ...)`, recorded once
/// per source and reason in the window of `auditFailureDeduplicationWindow` (300 seconds).
async fn audit_unauthenticated(state: &AppState, client: IpAddr, headers: &HeaderMap) {
    if crate::rules::throttle(
        &format!("audit-failure-dedup:api.auth.unauthenticated:{client}"),
        1,
        std::time::Duration::from_secs(300),
    )
    .is_some()
    {
        return;
    }
    let meta = crate::audit::Meta {
        ip: Some(client.to_string()),
        user_agent: Some(
            headers
                .get(axum::http::header::USER_AGENT)
                .and_then(|v| v.to_str().ok())
                .map(|s| s.chars().take(200).collect::<String>())
                .unwrap_or_default(),
        ),
    };
    crate::audit::log_system(
        state,
        None,
        "api.auth.unauthenticated",
        serde_json::json!({"reason": "Unauthenticated.", "guards": ["sanctum"]}),
        "warning",
        &meta,
    )
    .await;
}

/// Main group guard.
pub async fn main_guard(State(state): State<AppState>, mut req: Request, next: Next) -> Response {
    let headers = req.headers().clone();
    let client = client_ip(&state, &req);
    let result = async {
        let ctx = authenticate(&state, &headers).await?;
        let settings = instance::api_settings(&state.db).await?;
        if !settings.is_api_enabled {
            return Err(ApiError::ApiDisabled);
        }
        if !api_ip_allowed(settings.allowed_ips.as_deref(), client) {
            return Err(ApiError::IpNotAllowed);
        }
        Ok(ctx)
    }
    .await;
    match result {
        Ok(ctx) => {
            tracing::Span::current().record("team_id", ctx.team_id);
            req.extensions_mut().insert(ctx);
            req.extensions_mut().insert(state);
            req.extensions_mut().insert(ClientAddr(client));
            next.run(req).await
        }
        Err(e) => {
            if matches!(e, ApiError::Unauthenticated) {
                audit_unauthenticated(&state, client, &headers).await;
            }
            e.into_response()
        }
    }
}

/// `/disable` group guard (IP check first, no "API enabled" check).
pub async fn disable_guard(
    State(state): State<AppState>,
    mut req: Request,
    next: Next,
) -> Response {
    let headers = req.headers().clone();
    let client = client_ip(&state, &req);
    let result = async {
        ip_check(&state, client).await?;
        authenticate(&state, &headers).await
    }
    .await;
    match result {
        Ok(ctx) => {
            req.extensions_mut().insert(ctx);
            next.run(req).await
        }
        Err(e) => e.into_response(),
    }
}

/// The request's address, for what runs after the guard.
#[derive(Debug, Clone, Copy)]
struct ClientAddr(IpAddr);

/// `api.ability:<list>`; several entries in `all_of` model stacked middlewares. A refusal writes
/// `api.auth.ability_denied` (`ApiAbility::handle`).
pub async fn require_abilities(
    all_of: &'static [&'static [Ability]],
    req: Request,
    next: Next,
) -> Response {
    let Some(ctx) = req.extensions().get::<AuthCtx>() else {
        return ApiError::Unauthenticated.into_response();
    };
    for any_of in all_of {
        if let AbilityCheck::Missing(missing) = check_any(&ctx.abilities, any_of) {
            if let Some(state) = req.extensions().get::<AppState>() {
                let meta = crate::audit::Meta {
                    ip: req
                        .extensions()
                        .get::<ClientAddr>()
                        .map(|c| c.0.to_string()),
                    user_agent: Some(
                        req.headers()
                            .get(axum::http::header::USER_AGENT)
                            .and_then(|v| v.to_str().ok())
                            .map(|s| s.chars().take(200).collect::<String>())
                            .unwrap_or_default(),
                    ),
                };
                crate::audit::log(
                    state,
                    ctx,
                    &meta,
                    "api.auth.ability_denied",
                    serde_json::json!({
                        "required_abilities": missing.iter().map(|a| a.as_str()).collect::<Vec<_>>(),
                        "token_id": ctx.token_id,
                        "reason": "Invalid ability provided.",
                    }),
                    "warning",
                )
                .await;
            }
            return ApiError::MissingAbilities(missing.iter().map(|a| a.as_str()).collect())
                .into_response();
        }
    }
    next.run(req).await
}
