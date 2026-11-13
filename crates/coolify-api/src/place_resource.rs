//! `ServerPolicy::placeResource` (upstream 4.7.5, 8011d0d6a): whether the actor can place a
//! resource on a server: create, clone, or migrate one there, or add the server to an application.
//!
//! The Coolify host needs a team admin, which is an instance admin for the localhost server.
//! Other servers need team-wide write, or write on a resource that already runs there
//! (`AccessResolver::allowsOnServer`).

use coolify_db::servers::ServerRow;
use coolify_domain::grants::AccessLevel;
use serde_json::Value;

use crate::auth::{
    AuthCtx, PLACE_ON_COOLIFY_HOST_DENIED_MESSAGE, PLACE_ON_SERVER_DENIED_MESSAGE, policy_denied,
};
use crate::response::ApiError;
use crate::state::AppState;

/// `Server::isLocalhost()`: `$this->ip === 'host.docker.internal' || $this->id === 0`.
pub(crate) fn is_localhost(server: &ServerRow) -> bool {
    server.id == 0
        || server
            .record
            .get("ip")
            .and_then(Value::as_str)
            .map(crate::routes::servers::ip_accessor)
            .as_deref()
            == Some("host.docker.internal")
}

/// The decision of `placeResource` without the database: `on_server` is
/// `AccessResolver::allowsOnServer($user, AccessLevel::Write, $server)`, asked only when needed.
/// `Err` carries the deny message of the policy.
pub(crate) fn decide(
    team_wide_write: bool,
    is_admin: bool,
    is_localhost: bool,
    on_server: impl FnOnce() -> bool,
) -> Result<(), &'static str> {
    if is_localhost {
        return if team_wide_write && is_admin {
            Ok(())
        } else {
            Err(PLACE_ON_COOLIFY_HOST_DENIED_MESSAGE)
        };
    }
    if team_wide_write || on_server() {
        Ok(())
    } else {
        Err(PLACE_ON_SERVER_DENIED_MESSAGE)
    }
}

/// `$this->authorize('placeResource', $server)` / `Gate::authorize('placeResource', $server)`.
/// The server is one of the actor's team (the finders scope by team).
pub(crate) async fn authorize(
    state: &AppState,
    ctx: &AuthCtx,
    server: &ServerRow,
) -> Result<(), ApiError> {
    check(state, ctx, server).await?.map_err(policy_denied)
}

/// `Gate::allows('placeResource', $server)`: the same decision without the deny response.
pub(crate) async fn allows(
    state: &AppState,
    ctx: &AuthCtx,
    server: &ServerRow,
) -> Result<bool, ApiError> {
    Ok(check(state, ctx, server).await?.is_ok())
}

/// The ids of the team's servers that pass `Gate::allows('placeResource', $server)`, for the
/// lists that filter servers with it (`Select::loadServers()`, `GlobalSearch::loadServers()`,
/// `Destination`, `CloneMe`, `ResourceOperations`).
pub(crate) async fn allowed_server_ids(
    state: &AppState,
    ctx: &AuthCtx,
) -> Result<std::collections::HashSet<i64>, ApiError> {
    let mut ids = std::collections::HashSet::new();
    for server in coolify_db::servers::list(&state.db, ctx.team_id).await? {
        if allows(state, ctx, &server).await? {
            ids.insert(server.id);
        }
    }
    Ok(ids)
}

async fn check(
    state: &AppState,
    ctx: &AuthCtx,
    server: &ServerRow,
) -> Result<Result<(), &'static str>, ApiError> {
    let team_wide_write = ctx.allows_team_wide(AccessLevel::Write);
    let localhost = is_localhost(server);
    let on_server = if team_wide_write || localhost {
        false
    } else {
        coolify_db::access::allows_on_server(
            &state.db,
            &ctx.access,
            server.team_id,
            AccessLevel::Write,
            server.id,
        )
        .await?
    };
    Ok(decide(
        team_wide_write,
        ctx.is_team_admin(),
        localhost,
        || on_server,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_coolify_host_needs_a_team_admin_with_team_wide_write() {
        assert_eq!(decide(true, true, true, || false), Ok(()));
        // A member with team-wide write is not enough on the host.
        assert_eq!(
            decide(true, false, true, || true),
            Err(PLACE_ON_COOLIFY_HOST_DENIED_MESSAGE)
        );
        // An admin whose token is limited to some projects is not enough either.
        assert_eq!(
            decide(false, true, true, || true),
            Err(PLACE_ON_COOLIFY_HOST_DENIED_MESSAGE)
        );
    }

    #[test]
    fn other_servers_need_team_wide_write_or_write_on_a_resource_there() {
        assert_eq!(decide(true, false, false, || false), Ok(()));
        assert_eq!(decide(false, false, false, || true), Ok(()));
        assert_eq!(
            decide(false, false, false, || false),
            Err(PLACE_ON_SERVER_DENIED_MESSAGE)
        );
    }

    #[test]
    fn deny_messages_match_upstream() {
        assert_eq!(
            PLACE_ON_COOLIFY_HOST_DENIED_MESSAGE,
            "Only an admin can place resources on the server that runs Coolify."
        );
        assert_eq!(
            PLACE_ON_SERVER_DENIED_MESSAGE,
            "You need write access on the whole team, or on a resource that already runs on this server, to place resources on it."
        );
    }
}
