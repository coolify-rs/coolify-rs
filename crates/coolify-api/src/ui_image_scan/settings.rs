//! `App\Livewire\ImageScan\ScanSettings`: the scan rules of one level (instance, team, server, or
//! application), saved on change, and the ignores of that level. A null value inherits from the
//! level above. Databases and services have no rules of their own: their scope lists only their
//! ignores (a service with the ignores of its applications and databases). The scope is reloaded
//! and authorized on every request.
//!
//! The team scope is the session team (the team settings page opens the session team).

use std::collections::HashMap;

use axum::extract::{Path, State};
use axum::response::IntoResponse;
use axum::routing::{delete, get};
use axum::{Extension, Json, Router};
use coolify_db::applications as apps;
use coolify_db::image_scans::{self, ImageScanIgnoreRow, ImageScanPolicyRow, ScanOwner};
use coolify_db::servers::{self, ColumnValue, ServerRow};
use coolify_domain::grants::AccessLevel;
use coolify_jobs::image_scan::policy::{self, GateMode, OnScanError, PolicyScope, ResolvedLevels};
use coolify_jobs::image_scan::resource_scans;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{forbidden, internal, jobs, not_found, toast_error, toast_success};
use crate::audit::{self, Meta};
use crate::auth::AuthCtx;
use crate::response::{ApiError, ApiResult, ok};
use crate::routes::vulnerabilities::can_access;
use crate::state::AppState;

const SCOPES: [&str; 6] = [
    "instance",
    "team",
    "server",
    "application",
    "database",
    "service",
];

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/ui/image-scan/settings/{scope}", get(render).patch(update))
        .route(
            "/ui/image-scan/settings/{scope}/{uuid}",
            get(render).patch(update),
        )
        .route(
            "/ui/image-scan/settings/{scope}/ignores/{ignore}",
            delete(remove_ignore),
        )
        .route(
            "/ui/image-scan/settings/{scope}/{uuid}/ignores/{ignore}",
            delete(remove_ignore),
        )
}

/// The loaded scope (`loadScope()`): `None` is the instance level.
enum Scope {
    Instance,
    Team(i64),
    Server(ServerRow),
    /// An application, database, or service.
    Resource(ScanOwner),
}

impl Scope {
    fn team_id(&self) -> Option<i64> {
        match self {
            Scope::Instance => None,
            Scope::Team(id) => Some(*id),
            Scope::Server(s) => Some(s.team_id),
            Scope::Resource(r) => r.team_id,
        }
    }

    fn application(&self) -> Option<&ScanOwner> {
        match self {
            Scope::Resource(r) if r.is_application() => Some(r),
            _ => None,
        }
    }

    fn policy_scope(&self) -> Option<PolicyScope<'_>> {
        match self {
            Scope::Instance => None,
            Scope::Team(id) => Some(PolicyScope::Team(*id)),
            Scope::Server(s) => Some(PolicyScope::Server {
                id: s.id,
                team_id: s.team_id,
            }),
            Scope::Resource(r) => Some(PolicyScope::Application(r)),
        }
    }
}

fn is_ignore_only(scope_type: &str) -> bool {
    matches!(scope_type, "database" | "service")
}

fn is_resource_scope(scope: &Scope) -> bool {
    matches!(scope, Scope::Resource(_))
}

async fn instance_admin(state: &AppState, ctx: &AuthCtx) -> Result<bool, ApiError> {
    Ok(coolify_db::instance_settings::is_instance_admin(&state.db, ctx.user_id).await?)
}

/// `loadScope()`.
async fn load_scope(
    state: &AppState,
    ctx: &AuthCtx,
    scope_type: &str,
    uuid: Option<&str>,
) -> Result<Scope, ApiError> {
    if !SCOPES.contains(&scope_type) {
        return Err(not_found());
    }
    let uuid = uuid.unwrap_or_default();
    match scope_type {
        "instance" => {
            if !instance_admin(state, ctx).await? {
                return Err(forbidden());
            }
            Ok(Scope::Instance)
        }
        "team" => Ok(Scope::Team(ctx.team_id)),
        "server" => servers::find(&state.db, ctx.team_id, uuid)
            .await?
            .map(Scope::Server)
            .ok_or_else(not_found),
        kind => {
            let resource = image_scans::find_resource(&state.db, kind, uuid, &[ctx.team_id])
                .await?
                .ok_or_else(not_found)?;
            if !can_access(ctx, &resource, AccessLevel::Read) {
                return Err(forbidden());
            }
            Ok(Scope::Resource(resource))
        }
    }
}

/// `canEdit($scope)`.
async fn can_edit(state: &AppState, ctx: &AuthCtx, scope: &Scope) -> Result<bool, ApiError> {
    Ok(match scope {
        Scope::Instance => instance_admin(state, ctx).await?,
        // `isAdminOfTeam()`; `ServerPolicy::update`.
        Scope::Team(_) | Scope::Server(_) => ctx.is_team_admin(),
        Scope::Resource(r) => can_access(ctx, r, AccessLevel::Write),
    })
}

/// `LEVEL_NAMES`.
fn level_name(source: &str) -> &'static str {
    match source {
        "instance" => "Coolify",
        "team" => "team",
        _ => "server",
    }
}

/// `inheritLabels($scope)`: the inherit option label per rule field.
fn inherit_labels(levels: &ResolvedLevels) -> Value {
    let label = |source: &str, value: &str| format!("Use {} default ({value})", level_name(source));
    json!({
        "gate_mode": label(levels.gate_mode.source, levels.gate_mode.value.label()),
        "on_scan_error": label(levels.on_scan_error.source, levels.on_scan_error.value.label()),
        "count_medium": label(
            levels.count_medium.source,
            if levels.count_medium.value { "Yes" } else { "No" },
        ),
    })
}

/// `boolToOption()`.
fn bool_option(value: Option<bool>) -> &'static str {
    match value {
        Some(true) => "1",
        Some(false) => "0",
        None => "",
    }
}

/// The ignores of a resource: of an application or database itself, of the applications and
/// databases of a service, and of the service.
async fn resource_owner_keys(
    state: &AppState,
    resource: &ScanOwner,
) -> Result<Vec<(String, i64)>, ApiError> {
    let mut keys: Vec<(String, i64)> = image_scans::owners(&state.db, resource)
        .await?
        .iter()
        .map(|o| (o.morph.to_owned(), o.id))
        .collect();
    if resource.is_service() {
        keys.push((resource.morph.to_owned(), resource.id));
    }
    Ok(keys)
}

fn is_resource_ignore(ignore: &ImageScanIgnoreRow, keys: &[(String, i64)]) -> bool {
    match (&ignore.resource_type, ignore.resource_id) {
        (Some(t), Some(id)) => keys.iter().any(|(kt, kid)| kt == t && *kid == id),
        _ => false,
    }
}

/// `ignores($scope)`: newest first; on a resource page its own ignores first, then the team-wide
/// ones.
async fn ignores(state: &AppState, scope: &Scope) -> Result<Vec<ImageScanIgnoreRow>, ApiError> {
    let newest_first = |list: &mut Vec<ImageScanIgnoreRow>| {
        list.sort_by(|a, b| b.created_at.cmp(&a.created_at).then(b.id.cmp(&a.id)));
    };
    match scope {
        Scope::Team(team_id) => {
            let mut list: Vec<ImageScanIgnoreRow> =
                image_scans::ignores_of_team(&state.db, *team_id)
                    .await?
                    .into_iter()
                    .filter(|i| i.resource_id.is_none())
                    .collect();
            newest_first(&mut list);
            Ok(list)
        }
        Scope::Resource(resource) => {
            let Some(team_id) = resource.team_id else {
                return Ok(Vec::new());
            };
            let keys = resource_owner_keys(state, resource).await?;
            let mut list: Vec<ImageScanIgnoreRow> =
                image_scans::ignores_of_team(&state.db, team_id)
                    .await?
                    .into_iter()
                    .filter(|i| i.resource_id.is_none() || is_resource_ignore(i, &keys))
                    .collect();
            newest_first(&mut list);
            // `orderByRaw('resource_id is null')`: false (own ignores) first; the sort is stable.
            list.sort_by_key(|i| i.resource_id.is_none());
            Ok(list)
        }
        _ => Ok(Vec::new()),
    }
}

/// `ownerNames()`: for a service, the name of the application or database each ignore was set
/// for, by ignore uuid.
async fn owner_names(
    state: &AppState,
    scope: &Scope,
    ignores: &[ImageScanIgnoreRow],
) -> Result<HashMap<String, String>, ApiError> {
    let Scope::Resource(resource) = scope else {
        return Ok(HashMap::new());
    };
    if !resource.is_service() {
        return Ok(HashMap::new());
    }
    let owners = image_scans::owners(&state.db, resource).await?;
    Ok(ignores
        .iter()
        .filter_map(|ignore| {
            let owner = owners.iter().find(|o| {
                ignore.resource_type.as_deref() == Some(o.morph) && ignore.resource_id == Some(o.id)
            })?;
            let human = owner.string("human_name");
            let name = if human.is_empty() {
                owner.name()
            } else {
                human
            };
            Some((ignore.uuid.clone(), name))
        })
        .collect())
}

#[derive(Debug, Deserialize)]
struct ScopePath {
    scope: String,
    uuid: Option<String>,
}

/// `render()`.
async fn render(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(path): Path<ScopePath>,
) -> ApiResult {
    let scope = load_scope(&state, &ctx, &path.scope, path.uuid.as_deref()).await?;
    let ignore_only = is_ignore_only(&path.scope);
    let can_edit = can_edit(&state, &ctx, &scope).await?;

    let (values, inherit) = if ignore_only {
        (Value::Null, Value::Null)
    } else {
        let row: Option<ImageScanPolicyRow> = policy::for_scope(&state.db, scope.policy_scope())
            .await
            .map_err(internal)?;
        let inherit = match scope.policy_scope() {
            None => Value::Null,
            Some(p) => inherit_labels(
                &policy::inherited_for(&state.db, p)
                    .await
                    .map_err(internal)?,
            ),
        };
        let team_scans_new = match &scope {
            Scope::Team(id) => image_scans::team_scans_new_servers(&state.db, *id).await?,
            _ => false,
        };
        let app = scope.application();
        (
            json!({
                "gate_mode": row.as_ref().and_then(|r| r.gate_mode.clone()).unwrap_or_default(),
                "on_scan_error": row.as_ref().and_then(|r| r.on_scan_error.clone()).unwrap_or_default(),
                "count_medium": bool_option(row.as_ref().and_then(|r| r.count_medium)),
                "app_scan_enabled": match app {
                    Some(a) => bool_option(Some(a.is_image_scan_enabled())),
                    None => "1",
                },
                "builds_by_team": match app {
                    Some(a) => bool_option(a.image_builds_by_team()),
                    None => "",
                },
                "scan_new_servers": bool_option(Some(team_scans_new)),
            }),
            inherit,
        )
    };

    let ignores = ignores(&state, &scope).await?;
    let names = owner_names(&state, &scope, &ignores).await?;
    // `showsTeamSettingsLink()`: the resource is in the session team, and the user can open
    // team-wide pages.
    let shows_team_settings_link = is_resource_scope(&scope)
        && scope.team_id() == Some(ctx.team_id)
        && ctx.covers_whole_team();
    let app = scope.application();
    Ok(ok(json!({
        "scope_type": path.scope,
        "can_edit": can_edit,
        "values": values,
        "inherit_labels": inherit,
        "gate_mode_options": GateMode::ALL.iter().map(|m| {
            let mut o = json!({"value": m.value(), "label": m.label()});
            if let (Some(d), Value::Object(map)) = (m.description(), &mut o) {
                map.insert("description".to_owned(), json!(d));
            }
            o
        }).collect::<Vec<_>>(),
        "on_scan_error_options": OnScanError::ALL.iter().map(|o| json!({"value": o.value(), "label": o.label()})).collect::<Vec<_>>(),
        "count_medium_options": [{"value": "1", "label": "Yes"}, {"value": "0", "label": "No"}],
        "app_scan_on": app.is_none_or(ScanOwner::is_image_scan_enabled),
        "is_docker_image": app.is_some_and(|a| a.build_pack() == Some("dockerimage")),
        "shows_ignores": matches!(scope, Scope::Team(_)) || is_resource_scope(&scope),
        "shows_team_settings_link": shows_team_settings_link,
        "ignores": ignores.iter().map(|i| json!({
            "uuid": i.uuid,
            "vulnerability_id": i.vulnerability_id,
            "package": i.package,
            "reason": i.reason,
            "expires": i.expires_at.map(|t| t.format("%Y-%m-%d").to_string()),
            "team_wide": i.resource_id.is_none(),
            "set_for": names.get(&i.uuid),
        })).collect::<Vec<_>>(),
    }))
    .into_response())
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct UpdateInput {
    field: String,
    value: Option<String>,
}

fn audit_context(scope: &Scope, path: &ScopePath, field: &str) -> Value {
    json!({
        "team_id": scope.team_id(),
        "scope": path.scope,
        "scope_uuid": match scope {
            Scope::Team(id) => Some(id.to_string()),
            _ => path.uuid.clone(),
        },
        "changed_fields": [field],
    })
}

/// `ImageScanPolicy::validationRules()` of one rule, required at the instance level: the first
/// error of the field.
fn rule_error(field: &str, value: Option<&str>, required: bool) -> Option<String> {
    let attribute = field.replace('_', " ");
    let Some(value) = value.filter(|v| !v.is_empty()) else {
        return required.then(|| format!("The {attribute} field is required."));
    };
    let valid = match field {
        "gate_mode" => GateMode::from_value(value).is_some(),
        "on_scan_error" => OnScanError::from_value(value).is_some(),
        _ => matches!(value, "1" | "0"),
    };
    if valid {
        None
    } else if field == "count_medium" {
        Some(format!("The {attribute} field must be true or false."))
    } else {
        Some(format!("The selected {attribute} is invalid."))
    }
}

/// `writeStatuses` after "Count medium problems" changed (`refreshStatuses()`).
async fn refresh_statuses(state: &AppState, scope: &Scope) -> Result<(), ApiError> {
    let jobs = jobs(state)?;
    if let Some(app) = scope.application() {
        let scans = resource_scans::current(&state.db, app)
            .await
            .map_err(internal)?;
        return resource_scans::refresh_summaries(jobs, scans)
            .await
            .map_err(internal);
    }
    let team_ids: Vec<i64> = match scope {
        Scope::Instance => sqlx::query_scalar(
            "SELECT DISTINCT team_id FROM servers WHERE id IN (SELECT server_id FROM image_scans)",
        )
        .fetch_all(state.db.pool())
        .await
        .map_err(internal)?,
        other => other.team_id().into_iter().collect(),
    };
    for team_id in team_ids {
        jobs.dispatch_refresh_image_scan_summaries(team_id);
    }
    Ok(())
}

/// `updatedGateMode`, `updatedOnScanError`, `updatedCountMedium`, `updatedAppScanEnabled`,
/// `updatedBuildsByTeam`, and `updatedScanNewServers`.
async fn update(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(path): Path<ScopePath>,
    meta: Meta,
    Json(input): Json<UpdateInput>,
) -> ApiResult {
    let scope = load_scope(&state, &ctx, &path.scope, path.uuid.as_deref()).await?;
    if !can_edit(&state, &ctx, &scope).await? {
        return Err(forbidden());
    }
    let value = input.value.as_deref();
    let now = state.now();
    let field = input.field.as_str();
    let audit_field = match field {
        "gate_mode" | "on_scan_error" | "count_medium" => {
            if is_ignore_only(&path.scope) {
                return Err(not_found());
            }
            if let Some(message) = rule_error(field, value, matches!(scope, Scope::Instance)) {
                return toast_error(message);
            }
            let row = policy::for_scope(&state.db, scope.policy_scope())
                .await
                .map_err(internal)?;
            if field == "gate_mode"
                && scope
                    .application()
                    .is_some_and(|a| !a.is_image_scan_enabled())
            {
                return toast_error("Turn on the scan for this app first.");
            }
            let value = value.filter(|v| !v.is_empty());
            let mut gate_mode = row.as_ref().and_then(|r| r.gate_mode.clone());
            let mut on_scan_error = row.as_ref().and_then(|r| r.on_scan_error.clone());
            let mut count_medium = row.as_ref().and_then(|r| r.count_medium);
            match field {
                "gate_mode" => gate_mode = value.map(str::to_owned),
                "on_scan_error" => on_scan_error = value.map(str::to_owned),
                _ => count_medium = value.map(|v| v == "1"),
            }
            image_scans::save_policy(
                &state.db,
                scope.policy_scope().map(|p| p.key()),
                gate_mode.as_deref(),
                on_scan_error.as_deref(),
                count_medium,
                now,
            )
            .await?;
            if field == "count_medium" {
                refresh_statuses(&state, &scope).await?;
            }
            field
        }
        "app_scan_enabled" => {
            let app = scope.application().ok_or_else(not_found)?;
            let Some(v) = value.filter(|v| matches!(*v, "1" | "0")) else {
                return toast_error(if value.is_none_or(str::is_empty) {
                    "The app scan enabled field is required."
                } else {
                    "The selected app scan enabled is invalid."
                });
            };
            apps::update_settings(
                &state.db,
                app.id,
                &[("is_image_scan_enabled", ColumnValue::Bool(v == "1"))],
                now,
            )
            .await?;
            "is_image_scan_enabled"
        }
        "builds_by_team" => {
            let app = scope.application().ok_or_else(not_found)?;
            let v = value.filter(|v| !v.is_empty());
            if v.is_some_and(|v| !matches!(v, "1" | "0")) {
                return toast_error("The selected builds by team is invalid.");
            }
            state.db.ensure_writable()?;
            sqlx::query(
                "UPDATE application_settings SET image_builds_by_team = $2, updated_at = $3 \
                 WHERE application_id = $1",
            )
            .bind(app.id)
            .bind(v.map(|v| v == "1"))
            .bind(now)
            .execute(state.db.pool())
            .await
            .map_err(internal)?;
            "image_builds_by_team"
        }
        "scan_new_servers" => {
            let Scope::Team(team_id) = scope else {
                return Err(not_found());
            };
            let Some(v) = value.filter(|v| matches!(*v, "1" | "0")) else {
                return toast_error("The selected scan new servers is invalid.");
            };
            coolify_db::teams::set_scan_images_on_new_servers(&state.db, team_id, v == "1", now)
                .await?;
            "scan_images_on_new_servers"
        }
        _ => return Err(not_found()),
    };
    audit::log(
        &state,
        &ctx,
        &meta,
        "ui.image_scan.settings_updated",
        audit_context(&scope, &path, audit_field),
        "info",
    )
    .await;
    toast_success("Saved.")
}

#[derive(Debug, Deserialize)]
struct IgnorePath {
    scope: String,
    uuid: Option<String>,
    ignore: String,
}

/// `removeIgnore($uuid)`: on a resource page only the ignores of that resource; team-wide
/// ignores are removed on the team settings.
async fn remove_ignore(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(path): Path<IgnorePath>,
    meta: Meta,
) -> ApiResult {
    let jobs = jobs(&state)?;
    let scope = load_scope(&state, &ctx, &path.scope, path.uuid.as_deref()).await?;
    let team_id = scope.team_id().ok_or_else(not_found)?;
    let ignore = image_scans::find_ignore(&state.db, team_id, &path.ignore)
        .await?
        .ok_or_else(not_found)?;
    let allowed = match &scope {
        Scope::Team(_) => ignore.resource_id.is_none(),
        Scope::Resource(resource) => {
            let keys = resource_owner_keys(&state, resource).await?;
            is_resource_ignore(&ignore, &keys)
        }
        _ => false,
    };
    if !allowed {
        return Err(not_found());
    }
    if !can_edit(&state, &ctx, &scope).await? {
        return Err(forbidden());
    }
    image_scans::delete_ignore(&state.db, ignore.id).await?;
    if ignore.resource_id.is_none() {
        jobs.dispatch_refresh_image_scan_summaries(ignore.team_id);
    } else if let Scope::Resource(resource) = &scope {
        let scans = resource_scans::current(&state.db, resource)
            .await
            .map_err(internal)?;
        resource_scans::refresh_summaries(jobs, scans)
            .await
            .map_err(internal)?;
    }
    audit::log(
        &state,
        &ctx,
        &meta,
        "ui.image_scan.ignore_removed",
        json!({
            "team_id": ignore.team_id,
            "scope": path.scope,
            "scope_uuid": match scope {
                Scope::Team(id) => Some(id.to_string()),
                _ => path.uuid.clone(),
            },
            "vulnerability_id": ignore.vulnerability_id,
            "package": ignore.package,
        }),
        "info",
    )
    .await;
    toast_success("Removed.")
}

#[cfg(test)]
mod tests {
    use super::*;
    use coolify_jobs::image_scan::policy::Sourced;

    #[test]
    fn rule_errors_read_like_laravel() {
        assert_eq!(rule_error("gate_mode", Some("on"), false), None);
        assert_eq!(rule_error("gate_mode", None, false), None);
        assert_eq!(
            rule_error("gate_mode", Some(""), true).as_deref(),
            Some("The gate mode field is required.")
        );
        assert_eq!(
            rule_error("on_scan_error", Some("x"), false).as_deref(),
            Some("The selected on scan error is invalid.")
        );
        assert_eq!(
            rule_error("count_medium", Some("yes"), false).as_deref(),
            Some("The count medium field must be true or false.")
        );
    }

    #[test]
    fn inherit_labels_name_the_level() {
        let levels = ResolvedLevels {
            gate_mode: Sourced {
                value: GateMode::On,
                source: "team",
            },
            on_scan_error: Sourced {
                value: OnScanError::Deploy,
                source: "instance",
            },
            count_medium: Sourced {
                value: false,
                source: "server",
            },
        };
        let labels = inherit_labels(&levels);
        assert_eq!(labels["gate_mode"], "Use team default (On)");
        assert_eq!(
            labels["on_scan_error"],
            "Use Coolify default (Deploy anyway)"
        );
        assert_eq!(labels["count_medium"], "Use server default (No)");
        assert_eq!(bool_option(None), "");
        assert_eq!(bool_option(Some(true)), "1");
    }
}
