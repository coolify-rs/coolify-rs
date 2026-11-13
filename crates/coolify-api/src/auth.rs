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
use coolify_domain::grants::{Access, AccessLevel, Grant, Target};
use coolify_domain::{Abilities, Ability, Role, TeamId};
use std::sync::Arc;

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
    /// `AccessResolver::layersFor` of the actor in the team: member grants, then token scopes.
    pub access: Arc<Access>,
}

impl AuthCtx {
    pub fn is_team_admin(&self) -> bool {
        self.role.is_admin()
    }

    /// `User::canAccess($level, $target)`.
    pub fn can(&self, level: AccessLevel, target: &Target) -> bool {
        self.access.allows(level, target)
    }

    /// `canAccess` on an application, service or standalone database of an environment.
    pub fn can_resource(
        &self,
        level: AccessLevel,
        environment_id: Option<i64>,
        uuid: &str,
    ) -> bool {
        self.can(level, &Target::resource(environment_id, uuid))
    }

    /// `canAccess` on an application.
    pub fn can_app(
        &self,
        level: AccessLevel,
        a: &coolify_db::applications::ApplicationRow,
    ) -> bool {
        self.can_resource(level, a.int("environment_id"), &a.uuid)
    }

    /// `canAccess` on a standalone database (`DatabasePolicy::allows`: the instance database,
    /// id 0 of the root team, needs an owner or admin of the root team). It has no environment,
    /// so since 4.7.5 the check also uses the root team grants and the API token limits
    /// (`AccessResolver::allowsTeamWide`): a token limited to some projects cannot touch it.
    pub fn can_db(&self, level: AccessLevel, d: &coolify_db::databases::DatabaseRow) -> bool {
        if d.id == 0 && d.team_id == coolify_domain::ROOT_TEAM_ID {
            return self.can_instance_database(level);
        }
        self.can_resource(
            level,
            d.raw
                .get("environment_id")
                .and_then(serde_json::Value::as_i64),
            d.access_uuid(),
        )
    }

    /// `DatabasePolicy::allows` for the instance database (coolify-db):
    /// `isAdminOfTeam(0) && allowsTeamWide($user, $level, 0)`.
    pub fn can_instance_database(&self, level: AccessLevel) -> bool {
        self.team_id == coolify_domain::ROOT_TEAM_ID
            && self.is_team_admin()
            && self.allows_team_wide(level)
    }

    /// `AccessResolver::coversWholeTeam`.
    pub fn covers_whole_team(&self) -> bool {
        self.access.covers_whole_team()
    }

    /// `canReadServerSecretsFromApi()` (`ApiSensitiveData`, `can_read_server_sensitive`): server
    /// secrets (Sentinel token, log drain keys, proxy configuration) are team-level, so a token
    /// limited to some projects, environments or resources can read the secrets of those
    /// resources, but not of the servers they run on.
    pub fn can_read_server_sensitive(&self) -> bool {
        self.can_read_sensitive && self.covers_whole_team()
    }

    /// `AccessResolver::allowsTeamWide`.
    pub fn allows_team_wide(&self, level: AccessLevel) -> bool {
        self.access.allows_team_wide(level)
    }

    /// `ResourceCreatePolicy::createAny`: write on the environment, or on the whole team without one.
    pub fn can_create_in(&self, environment_id: Option<i64>) -> bool {
        match environment_id {
            Some(e) => self.can(AccessLevel::Write, &Target::environment(e)),
            None => self.allows_team_wide(AccessLevel::Write),
        }
    }

    /// `ResourceCreatePolicy::useTeamCredentials`: write on the whole team.
    pub fn can_use_team_credentials(&self) -> bool {
        self.allows_team_wide(AccessLevel::Write)
    }

    /// `$this->authorize('useTeamCredentials')`: since 4.7.5 the policy answers with its own
    /// message instead of the generic "This action is unauthorized.".
    pub fn authorize_team_credentials(&self) -> Result<(), ApiError> {
        if self.can_use_team_credentials() {
            Ok(())
        } else {
            Err(policy_denied(USE_TEAM_CREDENTIALS_DENIED_MESSAGE))
        }
    }
}

/// `Handler::render` for an `AuthorizationException` whose policy answered
/// `Response::deny($message)`: 403 `{"message": $message, "error": "Unauthorized"}`.
pub fn policy_denied(message: &str) -> ApiError {
    ApiError::Status(
        axum::http::StatusCode::FORBIDDEN,
        serde_json::json!({"message": message, "error": "Unauthorized"}),
    )
}

/// `ResourceCreatePolicy::useTeamCredentials` deny message.
pub const USE_TEAM_CREDENTIALS_DENIED_MESSAGE: &str = "You need write access on the whole team to use team credentials, such as private keys, git sources, and S3 storages.";

/// `ServerPolicy::placeResource` deny messages.
pub const PLACE_ON_COOLIFY_HOST_DENIED_MESSAGE: &str =
    "Only an admin can place resources on the server that runs Coolify.";
pub const PLACE_ON_SERVER_DENIED_MESSAGE: &str = "You need write access on the whole team, or on a resource that already runs on this server, to place resources on it.";

/// `EnsureTokenBelongsToCurrentTeamMember::EXCEEDS_ACCESS_MESSAGE`.
pub const EXCEEDS_ACCESS_MESSAGE: &str =
    "This API token has more access than you have in this team. Revoke it and create a new token.";

/// `ApiAbility::SCOPED_ACCESS_ROUTES`: the `/api/v1/` segments a token or member limited to some
/// projects, environments or resources can use. Everything else is team-level.
pub const SCOPED_ACCESS_ROUTES: &[&str] = &[
    "applications",
    "services",
    "databases",
    "projects",
    "deploy",
    "deployments",
    "resources",
    "version",
    "vulnerabilities",
];

pub const SCOPED_ACCESS_DENIED_MESSAGE: &str = "This API token is limited to selected projects, environments, or resources, so it cannot use this endpoint.";

/// `EnsureTeamWideAccess`.
pub const TEAM_WIDE_DENIED_MESSAGE: &str = "Your access in this team is limited to some projects.";

/// `EnsureTokenBelongsToCurrentTeamMember::tokenWithinCreatorAccess`: a member's token may hold
/// only access levels (no `write:sensitive`, no `*`), and the member must hold each of them on
/// every token scope. Owners and admins hold every level.
pub async fn token_within_creator_access(
    db: &coolify_db::Db,
    team_id: TeamId,
    member: Option<Vec<Grant>>,
    abilities: &Abilities,
    token_grants: Option<Vec<Grant>>,
) -> Result<bool, coolify_db::DbError> {
    if member.is_none() {
        return Ok(true);
    }
    let raw = abilities.as_slice();
    let levels = AccessLevel::parse_many(raw.iter().map(String::as_str));
    let mut unique: Vec<&String> = Vec::new();
    for a in raw {
        if !unique.contains(&a) {
            unique.push(a);
        }
    }
    if levels.len() != unique.len() {
        return Ok(false);
    }
    let grants = token_grants.unwrap_or_else(|| vec![Grant::team(levels)]);
    coolify_db::access::user_holds(db, team_id, member, &grants).await
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
pub(crate) fn bearer(headers: &HeaderMap) -> Option<String> {
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
    let membership = coolify_db::access::membership(&state.db, team_id, token.user_id)
        .await?
        .ok_or(ApiError::InvalidToken)?;
    let role = Role::parse(&membership.0).ok_or(ApiError::InvalidToken)?;
    let member = coolify_db::access::member_grants_of(Some((
        membership.0.as_str(),
        membership.1.as_deref(),
    )));
    let abilities = Abilities::from_column(token.abilities_raw.as_deref());
    let token_grants =
        coolify_db::access::token_grants(token.scopes_raw.as_deref(), abilities.as_slice());
    let within = token_within_creator_access(
        &state.db,
        team_id,
        member.clone(),
        &abilities,
        token_grants.clone(),
    )
    .await?;
    let access = coolify_db::access::actor(&state.db, team_id, member, token_grants).await?;
    let can_read_sensitive =
        (abilities.can(Ability::Root) || abilities.can(Ability::ReadSensitive)) && role.is_admin();
    let ctx = AuthCtx {
        token_id: token.id,
        user_id: token.user_id,
        team_id,
        team_id_raw: token.team_id_raw,
        role,
        abilities,
        can_read_sensitive,
        access: Arc::new(access),
    };
    if !within {
        return Err(ApiError::TokenExceedsAccess(Box::new(ctx)));
    }
    Ok(ctx)
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
            let access = ctx.access.clone();
            let audit_ctx = ctx.clone();
            req.extensions_mut().insert(ctx);
            req.extensions_mut().insert(state.clone());
            req.extensions_mut().insert(ClientAddr(client));
            // `$request->route()?->getName() ?? $request->path()`: API routes have no names.
            let route = req.uri().path().trim_start_matches('/').to_owned();
            let meta = request_meta(&req);
            let response = coolify_db::access::scope(access, next.run(req)).await;
            // `Handler::render()`: `auditLog('api.auth.policy_denied', ...)` for an
            // `AuthorizationException` of an API route.
            if let Some(crate::response::PolicyDenied(reason)) = response
                .extensions()
                .get::<crate::response::PolicyDenied>()
                .cloned()
            {
                crate::audit::log(
                    &state,
                    &audit_ctx,
                    &meta,
                    "api.auth.policy_denied",
                    serde_json::json!({"reason": reason, "route": route}),
                    "warning",
                )
                .await;
            }
            response
        }
        Err(ApiError::TokenExceedsAccess(ctx)) => {
            audit_token_exceeds_access(&state, &ctx, client, &headers).await;
            ApiError::TokenExceedsAccess(ctx).into_response()
        }
        Err(e) => {
            if matches!(e, ApiError::Unauthenticated) {
                audit_unauthenticated(&state, client, &headers).await;
            }
            e.into_response()
        }
    }
}

/// `auditLog('api.auth.token_exceeds_access', ...)` of `EnsureTokenBelongsToCurrentTeamMember`.
async fn audit_token_exceeds_access(
    state: &AppState,
    ctx: &AuthCtx,
    client: IpAddr,
    headers: &HeaderMap,
) {
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
    crate::audit::log(
        state,
        ctx,
        &meta,
        "api.auth.token_exceeds_access",
        serde_json::json!({
            "token_id": ctx.token_id,
            "team_id": ctx.team_id,
            "abilities": ctx.abilities.as_slice(),
        }),
        "warning",
    )
    .await;
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
            let access = ctx.access.clone();
            req.extensions_mut().insert(ctx);
            coolify_db::access::scope(access, next.run(req)).await
        }
        Err(ApiError::TokenExceedsAccess(ctx)) => {
            audit_token_exceeds_access(&state, &ctx, client, &headers).await;
            ApiError::TokenExceedsAccess(ctx).into_response()
        }
        Err(e) => e.into_response(),
    }
}

/// The request's address, for what runs after the guard.
#[derive(Debug, Clone, Copy)]
struct ClientAddr(IpAddr);

fn request_meta(req: &Request) -> crate::audit::Meta {
    crate::audit::Meta {
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
    }
}

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
    // `ApiAbility::handle`: a token or member limited to some projects, environments or
    // resources can use only the project and resource endpoints.
    if let Some(rest) = req.uri().path().strip_prefix("/api/v1/") {
        let segment = rest.split('/').next().unwrap_or_default();
        if !SCOPED_ACCESS_ROUTES.contains(&segment) && !ctx.covers_whole_team() {
            if let Some(state) = req.extensions().get::<AppState>()
                && crate::audit::first_denial_in_window(
                    state,
                    "api.auth.scope_denied",
                    ctx.token_id,
                    None,
                    &[],
                )
            {
                let meta = request_meta(&req);
                crate::audit::log(
                    state,
                    ctx,
                    &meta,
                    "api.auth.scope_denied",
                    serde_json::json!({
                        "token_id": ctx.token_id,
                        "path": req.uri().path().trim_start_matches('/'),
                    }),
                    "warning",
                )
                .await;
            }
            return ApiError::Message(
                axum::http::StatusCode::FORBIDDEN,
                SCOPED_ACCESS_DENIED_MESSAGE,
            )
            .into_response();
        }
    }
    for any_of in all_of {
        if let AbilityCheck::Missing(missing) = check_any(&ctx.abilities, any_of) {
            let required: Vec<&str> = missing.iter().map(|a| a.as_str()).collect();
            if let Some(state) = req.extensions().get::<AppState>()
                && crate::audit::first_denial_in_window(
                    state,
                    "api.auth.ability_denied",
                    ctx.token_id,
                    Some("Invalid ability provided."),
                    &required,
                )
            {
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

#[cfg(test)]
mod tests {
    use super::*;
    use coolify_domain::grants::{AccessGrants, expand};

    fn ctx(team_id: TeamId, role: Role, layers: Vec<AccessGrants>, sensitive: bool) -> AuthCtx {
        AuthCtx {
            token_id: 1,
            user_id: 1,
            team_id,
            team_id_raw: team_id.to_string(),
            role,
            abilities: Abilities::default(),
            can_read_sensitive: sensitive,
            access: Arc::new(Access {
                layers,
                environment_projects: [(70, 7)].into_iter().collect(),
            }),
        }
    }

    fn project_layer(levels: &[AccessLevel]) -> AccessGrants {
        let mut layer = AccessGrants::default();
        layer.project_levels.insert(7, expand(levels));
        layer.project_environment_ids = vec![70];
        layer
    }

    fn team_layer(levels: &[AccessLevel]) -> AccessGrants {
        AccessGrants {
            team_levels: expand(levels),
            ..Default::default()
        }
    }

    /// `ApiSensitiveData` (4.7.5): `can_read_server_sensitive` needs a token that covers the team.
    #[test]
    fn server_secrets_need_a_team_wide_token() {
        let team_wide = ctx(1, Role::Owner, vec![], true);
        assert!(team_wide.can_read_server_sensitive());
        let scoped = ctx(
            1,
            Role::Owner,
            vec![project_layer(&[AccessLevel::Root])],
            true,
        );
        assert!(scoped.can_read_sensitive);
        assert!(!scoped.can_read_server_sensitive());
        let no_sensitive = ctx(1, Role::Owner, vec![], false);
        assert!(!no_sensitive.can_read_server_sensitive());
    }

    /// `DatabasePolicy::allows` (4.7.5) for the instance database.
    #[test]
    fn the_instance_database_needs_a_root_team_admin_and_a_team_wide_level() {
        let owner = ctx(0, Role::Owner, vec![], false);
        assert!(owner.can_instance_database(AccessLevel::Write));
        // A token limited to a project of the root team.
        let scoped = ctx(
            0,
            Role::Owner,
            vec![project_layer(&[AccessLevel::Root])],
            false,
        );
        assert!(!scoped.can_instance_database(AccessLevel::Read));
        // A team-wide read token reads, but does not write.
        let read = ctx(
            0,
            Role::Owner,
            vec![team_layer(&[AccessLevel::Read])],
            false,
        );
        assert!(read.can_instance_database(AccessLevel::Read));
        assert!(!read.can_instance_database(AccessLevel::Write));
        // Members of the root team and admins of other teams never.
        let member = ctx(
            0,
            Role::Member,
            vec![team_layer(&[AccessLevel::Root])],
            false,
        );
        assert!(!member.can_instance_database(AccessLevel::Read));
        let other = ctx(1, Role::Owner, vec![], false);
        assert!(!other.can_instance_database(AccessLevel::Read));
    }

    #[test]
    fn team_credentials_denial_carries_the_policy_message() {
        let scoped = ctx(
            1,
            Role::Owner,
            vec![project_layer(&[AccessLevel::Root])],
            false,
        );
        let Err(ApiError::Status(status, body)) = scoped.authorize_team_credentials() else {
            panic!("expected a denial");
        };
        assert_eq!(status, axum::http::StatusCode::FORBIDDEN);
        assert_eq!(
            body,
            serde_json::json!({
                "message": "You need write access on the whole team to use team credentials, such as private keys, git sources, and S3 storages.",
                "error": "Unauthorized",
            })
        );
        assert!(
            ctx(
                1,
                Role::Member,
                vec![team_layer(&[AccessLevel::Write])],
                false
            )
            .authorize_team_credentials()
            .is_ok()
        );
    }

    #[test]
    fn vulnerabilities_are_open_to_scoped_tokens() {
        assert!(SCOPED_ACCESS_ROUTES.contains(&"vulnerabilities"));
    }
}
