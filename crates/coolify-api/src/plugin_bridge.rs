//! The REST API behind the plugin capabilities `resources` and `deploy` (ADR 0011).
//!
//! A plugin's read runs the API's own handler, with the context a token of the call's team would have with
//! only `read`: a member's role and no `read:sensitive`. So team scoping, the serializers and secret
//! redaction are the API's, byte for byte, and a change to any of them reaches plugins too. A plugin's
//! deployment runs `POST /api/v1/deploy`'s handler, with the same checks, for one application of the call's
//! team, on the authority the host chose: the route's caller, or the team admin who granted `deploy`.
//!
//! The bridge holds the state weakly: the state owns the plugin host, which owns the bridge.

use std::sync::{Arc, Weak};

use axum::Extension;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use coolify_domain::{Abilities, Ability, Role};
use coolify_plugins::data::BoxFuture;
use coolify_plugins::{
    Actor, DeployRequest, Deployer, ResourceKind, ResourceQuery, ResourceSource,
};

use crate::auth::AuthCtx;
use crate::response::ApiResult;
use crate::routes::{applications, databases, projects, servers, services};
use crate::state::{AppState, Inner};

/// Connects the state's plugin host, if it has one, to the API, and gives it the `APP_KEY` encrypter for
/// plugins' secret settings (ADR 0014).
pub(crate) fn connect(state: &AppState) {
    if let Some(host) = &state.plugins {
        let bridge = Arc::new(Bridge(state.downgrade()));
        host.connect(bridge.clone(), bridge);
        if let Some(e) = &state.encrypter {
            host.use_encrypter(e.clone());
        }
    }
}

struct Bridge(Weak<Inner>);

impl Bridge {
    fn state(&self) -> Result<AppState, String> {
        AppState::upgrade(&self.0).ok_or_else(|| "coolify-rs is shutting down".to_owned())
    }
}

/// What the API knows about a request with a token of `team_id` holding only `read`. No user: no read
/// handler looks at it, and none matches a real one.
fn reader(team_id: i64) -> AuthCtx {
    AuthCtx {
        token_id: 0,
        user_id: -1,
        team_id,
        team_id_raw: team_id.to_string(),
        role: Role::Member,
        abilities: Abilities::new(vec![Ability::Read.as_str().to_owned()]),
        can_read_sensitive: false,
    }
}

/// The handler's status and body.
async fn answer(result: ApiResult) -> (StatusCode, String) {
    let response: Response = match result {
        Ok(r) => r,
        Err(e) => e.into_response(),
    };
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default();
    (status, body)
}

fn message(status: StatusCode, body: &str) -> String {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("message").and_then(|m| m.as_str()).map(str::to_owned))
        .unwrap_or_else(|| status.to_string())
}

async fn read(
    state: AppState,
    team_id: i64,
    query: &ResourceQuery,
) -> Result<Option<String>, String> {
    use ResourceKind as K;
    let s = || State(state.clone());
    let ctx = || Extension(reader(team_id));
    let result = match query {
        ResourceQuery::All(K::Projects) => projects::list(s(), ctx()).await,
        ResourceQuery::All(K::Servers) => servers::list(s(), ctx()).await,
        ResourceQuery::All(K::Applications) => {
            applications::list(s(), ctx(), Uri::from_static("/api/v1/applications")).await
        }
        ResourceQuery::All(K::Services) => services::list(s(), ctx()).await,
        ResourceQuery::All(K::Databases) => databases::list(s(), ctx()).await,
        ResourceQuery::One(K::Projects, uuid) => {
            projects::show(s(), ctx(), Path(uuid.clone())).await
        }
        ResourceQuery::One(K::Servers, uuid) => {
            let uri =
                Uri::try_from(format!("/api/v1/servers/{uuid}")).map_err(|e| e.to_string())?;
            servers::show(s(), ctx(), Path(uuid.clone()), uri).await
        }
        ResourceQuery::One(K::Applications, uuid) => {
            applications::show(s(), ctx(), Path(uuid.clone())).await
        }
        ResourceQuery::One(K::Services, uuid) => {
            services::show(s(), ctx(), Path(uuid.clone())).await
        }
        ResourceQuery::One(K::Databases, uuid) => {
            databases::show(s(), ctx(), Path(uuid.clone())).await
        }
        ResourceQuery::Environments(uuid) => {
            projects::environments(s(), ctx(), Path(uuid.clone())).await
        }
    };
    match answer(result).await {
        (StatusCode::OK, body) => Ok(Some(body)),
        (StatusCode::NOT_FOUND, _) => Ok(None),
        (status, body) => {
            tracing::warn!(%status, ?query, "a plugin's resource read failed");
            Err(message(status, &body))
        }
    }
}

impl ResourceSource for Bridge {
    fn read<'a>(
        &'a self,
        team_id: i64,
        query: &'a ResourceQuery,
    ) -> BoxFuture<'a, Result<Option<String>, String>> {
        Box::pin(async move { read(self.state()?, team_id, query).await })
    }
}

async fn deploy(state: AppState, r: &DeployRequest) -> Result<String, String> {
    let unavailable = |e: coolify_db::DbError| {
        tracing::warn!(plugin = %r.plugin, error = %e, "a plugin's deployment failed");
        "deployments are unavailable".to_owned()
    };
    // Applications only, of the call's team; the API's endpoint would also start services and databases.
    if coolify_db::applications::find_in_team(&state.db, r.team_id, &r.uuid)
        .await
        .map_err(unavailable)?
        .is_none()
    {
        return Err("Application not found.".into());
    }
    let (user_id, abilities) = match &r.actor {
        Actor::Caller { user_id, abilities } => (*user_id, abilities.clone()),
        Actor::Grantor { user_id } => (*user_id, vec![Ability::Deploy.as_str().to_owned()]),
    };
    let role = coolify_db::auth::membership_role(&state.db, r.team_id, user_id)
        .await
        .map_err(unavailable)?;
    let role = match (&r.actor, role) {
        (Actor::Grantor { .. }, Some(role)) if role.is_admin() => role,
        (Actor::Grantor { .. }, _) => {
            return Err(
                "the team admin who granted deploy to this plugin is no longer an admin of the team"
                    .into(),
            );
        }
        (Actor::Caller { .. }, Some(role)) => role,
        (Actor::Caller { .. }, None) => return Err("Missing required team role.".into()),
    };
    let abilities = Abilities::new(abilities);
    // `api.ability:deploy` of the route.
    if !(abilities.can(Ability::Deploy) || abilities.can(Ability::Root)) {
        return Err("Missing required permissions: deploy".into());
    }
    let ctx = AuthCtx {
        token_id: 0,
        user_id,
        team_id: r.team_id,
        team_id_raw: r.team_id.to_string(),
        role,
        abilities,
        can_read_sensitive: false,
    };
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    let body = serde_json::json!({"uuid": r.uuid, "force": r.force}).to_string();
    let meta = crate::audit::Meta {
        ip: None,
        user_agent: Some(format!("coolify-rs plugin {}", r.plugin)),
    };
    let result = applications::deploy(
        State(state),
        Extension(ctx),
        meta,
        headers,
        Uri::from_static("/api/v1/deploy"),
        Bytes::from(body),
    )
    .await;
    match answer(result).await {
        (StatusCode::OK, body) => Ok(body),
        (status, body) => Err(message(status, &body)),
    }
}

impl Deployer for Bridge {
    fn deploy<'a>(&'a self, request: &'a DeployRequest) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async move { deploy(self.state()?, request).await })
    }
}
