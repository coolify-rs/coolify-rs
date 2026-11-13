//! `AccessResolver` (upstream 4.6.0): the access grants of members and API tokens.
//!
//! Members keep their grants on `team_user.access`, tokens on `personal_access_tokens.scopes`
//! (both JSON, added by `add_access_grants_to_team_members_and_tokens`). Before that migration
//! neither column exists: every member then has the default grant (team-wide read) and no token
//! is scoped, which is the behaviour of earlier releases.
//!
//! `VisibleThroughAccessGrants` hides the projects, environments and resources an actor has no
//! grant for. Here the actor of a request is a task-local ([`scope`]), set by the request guards;
//! the finders and lists the API and the UI read through call [`project_visible`],
//! [`environment_visible`] and [`resource_visible`]. Background jobs run outside a request and
//! see every row, like upstream's queue workers.

use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::sync::Arc;

use coolify_domain::grants::{Access, AccessGrants, AccessLevel, AccessScope, Grant, expand};
use coolify_domain::{Role, TeamId};
use serde_json::Value;

use crate::{Db, DbResult};

/// Tables of `AccessResolver::RESOURCE_MODELS`.
pub const RESOURCE_TABLES: [&str; 11] = [
    "applications",
    "services",
    "standalone_postgresqls",
    "standalone_mysqls",
    "standalone_mariadbs",
    "standalone_mongodbs",
    "standalone_redis",
    "standalone_keydbs",
    "standalone_dragonflies",
    "standalone_clickhouses",
    "standalone_sqlites",
];

pub fn has_member_access_column(db: &Db) -> bool {
    db.schema().has_column("team_user", "access")
}

pub fn has_token_scopes_column(db: &Db) -> bool {
    db.schema().has_column("personal_access_tokens", "scopes")
}

pub fn has_token_paused_column(db: &Db) -> bool {
    db.schema()
        .has_column("personal_access_tokens", "paused_at")
}

pub fn has_invitation_access_column(db: &Db) -> bool {
    db.schema().has_column("team_invitations", "access")
}

/// `AccessResolver::defaultMemberGrants`: a member without stored grants reads the whole team.
pub fn default_member_grants() -> Vec<Grant> {
    vec![Grant::team(vec![AccessLevel::Read])]
}

/// `team_user` of the user in the team: role and the raw `access` column.
pub async fn membership(
    db: &Db,
    team_id: TeamId,
    user_id: i64,
) -> DbResult<Option<(String, Option<String>)>> {
    let access = if has_member_access_column(db) {
        "access::text"
    } else {
        "NULL::text"
    };
    Ok(sqlx::query_as(&format!(
        "SELECT role, {access} FROM team_user WHERE team_id = $1 AND user_id = $2 LIMIT 1"
    ))
    .bind(team_id)
    .bind(user_id)
    .fetch_optional(db.pool())
    .await?)
}

/// `memberGrants` from a membership row: `None` for owners and admins (no limits), the stored
/// grants or the default for members, and no grants at all for non-members.
pub fn member_grants_of(membership: Option<(&str, Option<&str>)>) -> Option<Vec<Grant>> {
    let Some((role, access)) = membership else {
        return Some(Vec::new());
    };
    if role == "admin" || role == "owner" {
        return None;
    }
    Some(match access {
        None => default_member_grants(),
        Some(stored) => Grant::list_from_str(stored),
    })
}

pub async fn member_grants(db: &Db, team_id: TeamId, user_id: i64) -> DbResult<Option<Vec<Grant>>> {
    let m = membership(db, team_id, user_id).await?;
    Ok(member_grants_of(
        m.as_ref().map(|(r, a)| (r.as_str(), a.as_deref())),
    ))
}

/// `AccessResolver::tokenGrants`: `None` when the token is not limited to scopes. Each scope
/// gets the token's abilities as levels.
pub fn token_grants(scopes_raw: Option<&str>, abilities: &[String]) -> Option<Vec<Grant>> {
    let raw = scopes_raw?;
    let value: Value = serde_json::from_str(raw).ok()?;
    if value.is_null() {
        return None;
    }
    let levels = AccessLevel::parse_many(abilities.iter().map(String::as_str));
    Some(
        Grant::list_from(&value)
            .into_iter()
            .map(|g| Grant {
                levels: levels.clone(),
                ..g
            })
            .collect(),
    )
}

/// `AccessResolver::resolve`: grant UUIDs to ids inside the team. Grants that point outside the
/// team are dropped.
pub async fn resolve(db: &Db, team_id: TeamId, grants: &[Grant]) -> DbResult<AccessGrants> {
    let mut out = AccessGrants::default();
    let mut by_scope: BTreeMap<AccessScope, BTreeMap<String, coolify_domain::grants::LevelSet>> =
        BTreeMap::new();
    for g in grants {
        if g.levels.is_empty() {
            continue;
        }
        if g.scope == AccessScope::Team {
            out.team_levels.extend(expand(&g.levels));
            continue;
        }
        let Some(uuid) = g.uuid.clone() else { continue };
        by_scope
            .entry(g.scope)
            .or_default()
            .entry(uuid)
            .or_default()
            .extend(expand(&g.levels));
    }

    if let Some(projects) = by_scope.get(&AccessScope::Project) {
        let uuids: Vec<String> = projects.keys().cloned().collect();
        let rows: Vec<(i64, String)> =
            sqlx::query_as("SELECT id, uuid FROM projects WHERE team_id = $1 AND uuid = ANY($2)")
                .bind(team_id)
                .bind(&uuids)
                .fetch_all(db.pool())
                .await?;
        for (id, uuid) in rows {
            if let Some(levels) = projects.get(&uuid) {
                out.project_levels.insert(id, levels.clone());
            }
        }
        let ids: Vec<i64> = out.project_levels.keys().copied().collect();
        out.project_environment_ids =
            sqlx::query_scalar("SELECT id FROM environments WHERE project_id = ANY($1)")
                .bind(&ids)
                .fetch_all(db.pool())
                .await?;
    }

    if let Some(envs) = by_scope.get(&AccessScope::Environment) {
        let uuids: Vec<String> = envs.keys().cloned().collect();
        let rows: Vec<(i64, String, i64)> = sqlx::query_as(
            "SELECT environments.id, environments.uuid, environments.project_id FROM environments \
             JOIN projects ON projects.id = environments.project_id \
             WHERE projects.team_id = $1 AND environments.uuid = ANY($2)",
        )
        .bind(team_id)
        .bind(&uuids)
        .fetch_all(db.pool())
        .await?;
        for (id, uuid, project_id) in rows {
            if let Some(levels) = envs.get(&uuid) {
                out.environment_levels.insert(id, levels.clone());
                out.environment_projects.insert(id, project_id);
            }
        }
    }

    if let Some(resources) = by_scope.get(&AccessScope::Resource) {
        let uuids: Vec<String> = resources.keys().cloned().collect();
        let team_envs = team_environment_projects(db, team_id).await?;
        let env_ids: Vec<i64> = team_envs.keys().copied().collect();
        for table in RESOURCE_TABLES {
            let rows: Vec<(String, i64)> = sqlx::query_as(&format!(
                "SELECT uuid, environment_id FROM {table} WHERE deleted_at IS NULL \
                 AND uuid = ANY($1) AND environment_id = ANY($2)"
            ))
            .bind(&uuids)
            .bind(&env_ids)
            .fetch_all(db.pool())
            .await?;
            for (uuid, env) in rows {
                if let Some(levels) = resources.get(&uuid) {
                    out.resource_levels.insert(uuid.clone(), levels.clone());
                    out.resource_environments.insert(uuid, env);
                    if let Some(p) = team_envs.get(&env) {
                        out.environment_projects.insert(env, *p);
                    }
                }
            }
        }
    }
    Ok(out)
}

/// Environment id => project id of every environment of the team.
pub async fn team_environment_projects(db: &Db, team_id: TeamId) -> DbResult<HashMap<i64, i64>> {
    let rows: Vec<(i64, i64)> = sqlx::query_as(
        "SELECT environments.id, environments.project_id FROM environments \
         JOIN projects ON projects.id = environments.project_id WHERE projects.team_id = $1",
    )
    .bind(team_id)
    .fetch_all(db.pool())
    .await?;
    Ok(rows.into_iter().collect())
}

/// `layersFor`: the member layer (unless owner/admin) and the token layer (when scoped).
pub async fn actor(
    db: &Db,
    team_id: TeamId,
    member: Option<Vec<Grant>>,
    token: Option<Vec<Grant>>,
) -> DbResult<Access> {
    let mut layers = Vec::new();
    for grants in [member, token].into_iter().flatten() {
        layers.push(resolve_cached(db, team_id, &grants).await?);
    }
    let mut access = Access {
        layers,
        environment_projects: HashMap::new(),
    };
    if access.needs_locations() {
        access.environment_projects = team_environment_projects(db, team_id).await?;
    }
    Ok(access)
}

/// Team-only grants need no query; skip the database for them.
async fn resolve_cached(db: &Db, team_id: TeamId, grants: &[Grant]) -> DbResult<AccessGrants> {
    if grants.iter().all(|g| g.scope == AccessScope::Team) {
        let mut out = AccessGrants::default();
        for g in grants {
            out.team_levels.extend(expand(&g.levels));
        }
        return Ok(out);
    }
    resolve(db, team_id, grants).await
}

/// The access of a member (or owner/admin) of the team, with the token scopes when given.
pub async fn for_user(
    db: &Db,
    team_id: TeamId,
    user_id: i64,
    token: Option<Vec<Grant>>,
) -> DbResult<Access> {
    let member = member_grants(db, team_id, user_id).await?;
    actor(db, team_id, member, token).await
}

/// `AccessResolver::userHolds`: the member holds every level of every grant, at that scope or
/// above it. A token can never get more access than the user who owns it.
pub async fn user_holds(
    db: &Db,
    team_id: TeamId,
    member: Option<Vec<Grant>>,
    grants: &[Grant],
) -> DbResult<bool> {
    let Some(member) = member else {
        return Ok(true);
    };
    let layer = resolve(db, team_id, &member).await?;
    for grant in grants {
        let Some((p, e, r)) = locate_scope(db, team_id, grant).await? else {
            return Ok(false);
        };
        for level in &grant.levels {
            if !layer.allows(*level, p, e, r.as_deref()) {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

/// `AccessResolver::allowsOnServer` (4.7.5): true when the actor has the level on at least one
/// resource that runs on the server, as its main destination or as an additional server of an
/// application. An actor without layers (owner or admin with a team-wide token) is allowed.
/// The caller has already checked that the server belongs to `team_id`.
pub async fn allows_on_server(
    db: &Db,
    access: &Access,
    team_id: TeamId,
    level: AccessLevel,
    server_id: i64,
) -> DbResult<bool> {
    if access.layers.is_empty() {
        return Ok(true);
    }
    let environment_projects = team_environment_projects(db, team_id).await?;
    if environment_projects.is_empty() {
        return Ok(false);
    }
    let env_ids: Vec<i64> = environment_projects.keys().copied().collect();
    for table in RESOURCE_TABLES {
        // `orWhereHas('additional_servers', ...)`: only applications have additional servers.
        let additional = if table == "applications" {
            " OR EXISTS (SELECT 1 FROM additional_destinations ad \
               WHERE ad.application_id = r.id AND ad.server_id = $2)"
        } else {
            ""
        };
        let rows: Vec<(String, i64)> = sqlx::query_as(&format!(
            "SELECT r.uuid, r.environment_id FROM {table} r WHERE r.deleted_at IS NULL \
             AND r.environment_id = ANY($1) AND (\
               (r.destination_type = 'App\\Models\\StandaloneDocker' AND EXISTS (\
                 SELECT 1 FROM standalone_dockers d WHERE d.id = r.destination_id AND d.server_id = $2)) \
               OR (r.destination_type = 'App\\Models\\SwarmDocker' AND EXISTS (\
                 SELECT 1 FROM swarm_dockers d WHERE d.id = r.destination_id AND d.server_id = $2))\
               {additional})"
        ))
        .bind(&env_ids)
        .bind(server_id)
        .fetch_all(db.pool())
        .await?;
        for (uuid, environment_id) in rows {
            let Some(project_id) = environment_projects.get(&environment_id).copied() else {
                continue;
            };
            if access
                .layers
                .iter()
                .all(|l| l.allows(level, Some(project_id), Some(environment_id), Some(&uuid)))
            {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

type ScopeLocation = (Option<i64>, Option<i64>, Option<String>);

/// `locateScope`.
async fn locate_scope(db: &Db, team_id: TeamId, grant: &Grant) -> DbResult<Option<ScopeLocation>> {
    if grant.scope == AccessScope::Team {
        return Ok(Some((None, None, None)));
    }
    let probe = Grant {
        levels: vec![AccessLevel::Read],
        ..grant.clone()
    };
    let layer = resolve(db, team_id, std::slice::from_ref(&probe)).await?;
    let project = layer.visible_project_ids().first().copied();
    Ok(match grant.scope {
        AccessScope::Team => Some((None, None, None)),
        AccessScope::Project => project.map(|p| (Some(p), None, None)),
        AccessScope::Environment => layer
            .full_environment_ids()
            .first()
            .map(|e| (project, Some(*e), None)),
        AccessScope::Resource => {
            if layer.resource_uuids().is_empty() {
                None
            } else {
                Some((
                    project,
                    layer.visible_environment_ids().first().copied(),
                    grant.uuid.clone(),
                ))
            }
        }
    })
}

/// Where a resource of an environment sits: `(team_id, project_id)` of the environment.
pub async fn locate_environment(db: &Db, environment_id: i64) -> DbResult<Option<(TeamId, i64)>> {
    Ok(sqlx::query_as(
        "SELECT projects.team_id, projects.id FROM environments \
         JOIN projects ON projects.id = environments.project_id WHERE environments.id = $1",
    )
    .bind(environment_id)
    .fetch_optional(db.pool())
    .await?)
}

/// Whether a role string is owner or admin.
pub fn is_admin_role(role: &str) -> bool {
    Role::parse(role).is_some_and(Role::is_admin)
}

tokio::task_local! {
    static VISIBILITY: Arc<Access>;
}

/// Runs a request with the access of its actor, so the finders hide what it cannot see.
pub async fn scope<F: Future>(access: Arc<Access>, f: F) -> F::Output {
    VISIBILITY.scope(access, f).await
}

/// `withoutVisibilityScope`: checks that must see every row of the team.
pub async fn unscoped<F: Future>(f: F) -> F::Output {
    VISIBILITY.scope(Arc::new(Access::unrestricted()), f).await
}

/// The access of the current request, if any.
pub fn current() -> Option<Arc<Access>> {
    VISIBILITY.try_with(Arc::clone).ok()
}

/// `VisibleThroughAccessGrants` for a project row.
pub fn project_visible(project_id: i64) -> bool {
    VISIBILITY
        .try_with(|a| a.project_visible(project_id))
        .unwrap_or(true)
}

/// `VisibleThroughAccessGrants` for an environment row.
pub fn environment_visible(environment_id: i64) -> bool {
    VISIBILITY
        .try_with(|a| a.environment_visible(environment_id))
        .unwrap_or(true)
}

/// `VisibleThroughAccessGrants` for an application, service or standalone database row.
pub fn resource_visible(environment_id: Option<i64>, uuid: &str) -> bool {
    VISIBILITY
        .try_with(|a| a.resource_visible(environment_id, uuid))
        .unwrap_or(true)
}

/// True when the current request may see every row (no task-local, or a team-wide actor).
pub fn sees_whole_team() -> bool {
    VISIBILITY
        .try_with(|a| a.covers_whole_team())
        .unwrap_or(true)
}
