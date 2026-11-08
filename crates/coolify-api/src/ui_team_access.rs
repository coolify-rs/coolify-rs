//! `App\Livewire\Team\Member\Access` (upstream 4.6.0): the access grants (`team_user.access`) of a
//! team member with the `member` role, and of an invited member before the invitation is accepted.
//!
//! A row of the editor has one or more targets (`team`, or `<scope>:<uuid>` of a project,
//! environment or resource of the team) and the levels that apply to each of them. Saving stores
//! one grant per target.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::middleware::from_fn_with_state;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use coolify_db::access as acc;
use coolify_domain::TeamId;
use coolify_domain::grants::{AccessLevel, AccessScope, Grant};
use rand::Rng;
use serde_json::{Map, Value, json};

use crate::audit::Meta;
use crate::auth::AuthCtx;
use crate::response::{ApiError, ApiResult, PhpJson, ok};
use crate::state::AppState;
use crate::ui_notifications::{toast_error, unauthorized};

pub fn routes(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/ui/team/access-options", get(options))
        .route(
            "/ui/team/members/{id}/access",
            get(member_access).post(save_member_access),
        )
        .route("/ui/team/invitation-access", post(invitation_access))
        .route_layer(from_fn_with_state(state, crate::session::ui_guard))
}

/// `AccessLevel->name`: the keys of a row's `levels`.
fn level_name(level: AccessLevel) -> &'static str {
    match level {
        AccessLevel::Read => "Read",
        AccessLevel::ReadSensitive => "ReadSensitive",
        AccessLevel::Deploy => "Deploy",
        AccessLevel::Write => "Write",
        AccessLevel::Delete => "Delete",
        AccessLevel::Root => "Root",
    }
}

/// `team`, or `<scope>:<uuid>`.
pub(crate) fn target_of(g: &Grant) -> String {
    match g.scope {
        AccessScope::Team => "team".into(),
        scope => format!(
            "{}:{}",
            scope.as_str(),
            g.uuid.as_deref().unwrap_or_default()
        ),
    }
}

/// `Access::summary`: a short description of stored access for the members list.
pub(crate) fn summary(stored: Option<&str>) -> String {
    let Some(stored) = stored else {
        return "Team-wide read (default)".into();
    };
    let grants: Vec<Grant> = Grant::list_from_str(stored)
        .into_iter()
        .filter(|g| !g.levels.is_empty())
        .collect();
    summary_of(&grants)
}

pub(crate) fn summary_of(grants: &[Grant]) -> String {
    let grants: Vec<&Grant> = grants.iter().filter(|g| !g.levels.is_empty()).collect();
    if grants.is_empty() {
        return "No access".into();
    }
    let mut parts: Vec<String> = Vec::new();
    for g in &grants {
        if g.scope == AccessScope::Team {
            parts.push(format!(
                "Team: {}",
                g.levels
                    .iter()
                    .map(|l| l.label().to_lowercase())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }
    for scope in [
        AccessScope::Project,
        AccessScope::Environment,
        AccessScope::Resource,
    ] {
        let n = grants.iter().filter(|g| g.scope == scope).count();
        if n > 0 {
            let word = scope.as_str();
            parts.push(if n == 1 {
                format!("1 {word}")
            } else {
                format!("{n} {word}s")
            });
        }
    }
    parts.join(" + ")
}

/// `SORT_NATURAL | SORT_FLAG_CASE`.
fn natural_case(a: &str, b: &str) -> Ordering {
    let (a, b) = (a.to_lowercase(), b.to_lowercase());
    let (mut x, mut y) = (a.as_bytes(), b.as_bytes());
    loop {
        match (x.first(), y.first()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(p), Some(q)) if p.is_ascii_digit() && q.is_ascii_digit() => {
                let run = |s: &[u8]| s.iter().take_while(|c| c.is_ascii_digit()).count();
                let (n, m) = (run(x), run(y));
                let value = |s: &[u8]| {
                    std::str::from_utf8(s)
                        .ok()
                        .and_then(|t| t.parse::<u128>().ok())
                        .unwrap_or(0)
                };
                let o = value(&x[..n]).cmp(&value(&y[..m]));
                if o.is_ne() {
                    return o;
                }
                x = &x[n..];
                y = &y[m..];
            }
            (Some(p), Some(q)) => {
                if p != q {
                    return p.cmp(q);
                }
                x = &x[1..];
                y = &y[1..];
            }
        }
    }
}

/// `scopeOptionsFor`: every project, environment and resource of the team. The queries skip the
/// visibility of the request: only owners and admins open the editor.
async fn scope_options(state: &AppState, team_id: TeamId) -> Result<Vec<Value>, ApiError> {
    let db = &state.db;
    let projects: Vec<(i64, String, String)> =
        sqlx::query_as("SELECT id, uuid, name FROM projects WHERE team_id = $1 ORDER BY name")
            .bind(team_id)
            .fetch_all(db.pool())
            .await
            .map_err(coolify_db::DbError::from)?;
    let ids: Vec<i64> = projects.iter().map(|p| p.0).collect();
    let environments: Vec<(i64, String, String, i64)> = sqlx::query_as(
        "SELECT id, uuid, name, project_id FROM environments WHERE project_id = ANY($1)",
    )
    .bind(&ids)
    .fetch_all(db.pool())
    .await
    .map_err(coolify_db::DbError::from)?;
    let path_of = |env: i64| -> String {
        let Some(e) = environments.iter().find(|e| e.0 == env) else {
            return String::new();
        };
        let project = projects
            .iter()
            .find(|p| p.0 == e.3)
            .map(|p| p.2.as_str())
            .unwrap_or_default();
        format!("{project} / {}", e.2)
    };
    let env_ids: Vec<i64> = environments.iter().map(|e| e.0).collect();
    let mut resources: Vec<(String, String)> = Vec::new();
    for table in acc::RESOURCE_TABLES {
        let rows: Vec<(String, String, i64)> = sqlx::query_as(&format!(
            "SELECT uuid, name, environment_id FROM {table} \
             WHERE deleted_at IS NULL AND environment_id = ANY($1)"
        ))
        .bind(&env_ids)
        .fetch_all(db.pool())
        .await
        .map_err(coolify_db::DbError::from)?;
        for (uuid, name, env) in rows {
            resources.push((
                format!("resource:{uuid}"),
                format!("{} / {name}", path_of(env)),
            ));
        }
    }
    resources.sort_by(|a, b| natural_case(&a.1, &b.1));
    let mut envs: Vec<(String, String)> = environments
        .iter()
        .map(|e| (format!("environment:{}", e.1), path_of(e.0)))
        .collect();
    envs.sort_by(|a, b| natural_case(&a.1, &b.1));

    let mut out = vec![
        json!({"value": "__team", "label": "Team", "header": true}),
        json!({"value": "team", "label": "Whole team"}),
    ];
    if !projects.is_empty() {
        out.push(json!({"value": "__projects", "label": "Projects", "header": true}));
    }
    out.extend(
        projects
            .iter()
            .map(|p| json!({"value": format!("project:{}", p.1), "label": p.2})),
    );
    if !envs.is_empty() {
        out.push(json!({"value": "__environments", "label": "Environments", "header": true}));
    }
    out.extend(envs.iter().map(|(v, l)| json!({"value": v, "label": l})));
    if !resources.is_empty() {
        out.push(json!({"value": "__resources", "label": "Resources", "header": true}));
    }
    out.extend(
        resources
            .iter()
            .map(|(v, l)| json!({"value": v, "label": l})),
    );
    Ok(out)
}

/// `Access::validTargets`.
pub(crate) async fn valid_targets(
    state: &AppState,
    team_id: TeamId,
) -> Result<Vec<String>, ApiError> {
    Ok(scope_options(state, team_id)
        .await?
        .iter()
        .filter(|o| o.get("header").is_none())
        .filter_map(|o| o.get("value").and_then(Value::as_str).map(str::to_owned))
        .collect())
}

fn random_key() -> String {
    rand::thread_rng()
        .sample_iter(&rand::distributions::Alphanumeric)
        .take(10)
        .map(char::from)
        .collect()
}

/// `row()`.
fn row(targets: Vec<String>, levels: &[AccessLevel]) -> Value {
    let mut map = Map::new();
    for l in AccessLevel::ALL {
        map.insert(level_name(l).into(), json!(levels.contains(&l)));
    }
    json!({"key": random_key(), "targets": targets, "levels": map})
}

/// `rowsFrom`: grants with the same levels share one row.
fn rows_from(grants: &[Grant]) -> Vec<Value> {
    let mut groups: Vec<(String, Vec<String>, Vec<AccessLevel>)> = Vec::new();
    for g in grants {
        let key = g
            .levels
            .iter()
            .map(|l| level_name(*l))
            .collect::<Vec<_>>()
            .join(",");
        let target = target_of(g);
        match groups.iter_mut().find(|(k, _, _)| *k == key) {
            Some((_, targets, _)) => {
                if !targets.contains(&target) {
                    targets.push(target);
                }
            }
            None => groups.push((key, vec![target], g.levels.clone())),
        }
    }
    groups
        .into_iter()
        .map(|(_, targets, levels)| row(targets, &levels))
        .collect()
}

/// `presetGrants`: the presets of the editor, as rows (`__project__` stands for the project).
fn presets() -> Value {
    let preset = |grants: Vec<Grant>| rows_from(&grants).into_iter().next().unwrap_or(Value::Null);
    json!({
        "viewer": preset(vec![Grant::team(vec![AccessLevel::Read])]),
        "developer": preset(vec![Grant {
            scope: AccessScope::Project,
            uuid: Some("__project__".into()),
            levels: vec![AccessLevel::Write, AccessLevel::Deploy],
        }]),
        "operator": preset(vec![Grant::team(vec![
            AccessLevel::Write,
            AccessLevel::Deploy,
            AccessLevel::Delete,
        ])]),
        "full": preset(vec![Grant::team(vec![AccessLevel::Root])]),
    })
}

fn levels() -> Value {
    Value::Array(
        AccessLevel::ALL
            .iter()
            .map(|l| {
                json!({
                    "name": level_name(*l),
                    "label": l.label(),
                    "description": l.description(),
                    "includes": l.includes().iter().map(|i| level_name(*i)).collect::<Vec<_>>(),
                })
            })
            .collect(),
    )
}

/// What the editor renders besides its rows: levels, presets and the scope picker.
async fn options(State(state): State<AppState>, Extension(ctx): Extension<AuthCtx>) -> ApiResult {
    // `manageMembers` / `manageInvitations`.
    if !ctx.is_team_admin() {
        return Ok(unauthorized());
    }
    let scope_options = scope_options(&state, ctx.team_id).await?;
    let projects: Vec<&Value> = scope_options
        .iter()
        .filter(|o| {
            o.get("value")
                .and_then(Value::as_str)
                .is_some_and(|v| v.starts_with("project:"))
        })
        .collect();
    Ok(ok(json!({
        "scope_options": scope_options,
        "project_options": projects,
        "levels": levels(),
        "presets": presets(),
        "supported": acc::has_member_access_column(&state.db),
    }))
    .into_response())
}

/// `member()`: a member of the team with the `member` role, else 404.
async fn member(
    state: &AppState,
    ctx: &AuthCtx,
    id: i64,
) -> Result<Option<coolify_db::teams::MemberRow>, ApiError> {
    Ok(coolify_db::teams::member_rows(&state.db, ctx.team_id)
        .await?
        .into_iter()
        .find(|m| m.id == id && m.role == "member"))
}

/// `openFor($memberId)`.
async fn member_access(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(id): Path<i64>,
) -> ApiResult {
    if !ctx.is_team_admin() {
        return Ok(unauthorized());
    }
    let Some(m) = member(&state, &ctx, id).await? else {
        return Err(ApiError::Message(StatusCode::NOT_FOUND, "Not Found"));
    };
    let grants = match m.access.as_deref() {
        None => acc::default_member_grants(),
        Some(stored) => Grant::list_from_str(stored),
    };
    Ok(ok(json!({
        "title": format!("Access for {}", m.name),
        "summary": summary(m.access.as_deref()),
        "rows": rows_from(&grants),
    }))
    .into_response())
}

/// `validatedGrants`: errors are keyed `grants.<row>.targets` and `grants.<row>.levels`.
async fn validated_grants(
    state: &AppState,
    team_id: TeamId,
    body: &Value,
) -> Result<Result<Vec<Grant>, BTreeMap<String, String>>, ApiError> {
    let valid = valid_targets(state, team_id).await?;
    let rows = body
        .get("rows")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut grants = Vec::new();
    let mut errors: BTreeMap<String, String> = BTreeMap::new();
    for (index, row) in rows.iter().enumerate() {
        let mut targets: Vec<String> = Vec::new();
        let raw_targets = match row.get("targets") {
            Some(Value::Array(a)) => a.clone(),
            Some(Value::Null) | None => Vec::new(),
            Some(other) => vec![other.clone()],
        };
        for t in raw_targets {
            let t = match t {
                Value::String(s) => s,
                other => other.to_string(),
            };
            if !targets.contains(&t) {
                targets.push(t);
            }
        }
        let levels: Vec<AccessLevel> = AccessLevel::ALL
            .into_iter()
            .filter(|l| {
                row.get("levels")
                    .and_then(|m| m.get(level_name(*l)))
                    .is_some_and(crate::serialize::php_truthy)
            })
            .collect();
        let mut bad = false;
        if targets.is_empty() || targets.iter().any(|t| !valid.contains(t)) {
            errors.insert(
                format!("grants.{index}.targets"),
                "Choose at least one scope in this team.".into(),
            );
            bad = true;
        }
        if levels.is_empty() {
            errors.insert(
                format!("grants.{index}.levels"),
                "Choose at least one level.".into(),
            );
            bad = true;
        }
        if bad {
            continue;
        }
        for t in targets {
            let (scope, uuid) = if t == "team" {
                (AccessScope::Team, None)
            } else {
                let (s, u) = t.split_once(':').unwrap_or_default();
                let Some(scope) = AccessScope::parse(s) else {
                    continue;
                };
                (scope, Some(u.to_owned()))
            };
            grants.push(Grant {
                scope,
                uuid,
                levels: levels.clone(),
            });
        }
    }
    Ok(if errors.is_empty() {
        Ok(grants)
    } else {
        Err(errors)
    })
}

fn validation_response(errors: BTreeMap<String, String>) -> axum::response::Response {
    let first = errors.values().next().cloned().unwrap_or_default();
    let map: Map<String, Value> = errors.into_iter().map(|(k, v)| (k, json!([v]))).collect();
    PhpJson(
        StatusCode::UNPROCESSABLE_ENTITY,
        json!({"message": first, "errors": map}),
    )
    .into_response()
}

/// `save()` for a member: stores the grants and reloads the page.
async fn save_member_access(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path(id): Path<i64>,
    Json(body): Json<Value>,
) -> ApiResult {
    if !ctx.is_team_admin() {
        return Ok(unauthorized());
    }
    let Some(m) = member(&state, &ctx, id).await? else {
        return Err(ApiError::Message(StatusCode::NOT_FOUND, "Not Found"));
    };
    let grants = match validated_grants(&state, ctx.team_id, &body).await? {
        Ok(g) => g,
        Err(errors) => return Ok(validation_response(errors)),
    };
    if !acc::has_member_access_column(&state.db) {
        return Ok(toast_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Member access needs the database schema of Coolify 4.6.0.",
        ));
    }
    let stored = Grant::list_to_json(&grants);
    coolify_db::teams::set_member_access(&state.db, ctx.team_id, m.id, Some(&stored)).await?;
    crate::audit::log(
        &state,
        &ctx,
        &meta,
        "ui.team_member.access_updated",
        json!({
            "team_id": ctx.team_id,
            "member_id": m.id,
            "member_name": m.name,
            "member_email": m.email,
            "access": grants.iter().map(Grant::to_value).collect::<Vec<_>>(),
        }),
        "info",
    )
    .await;
    Ok(ok(json!({"message": "Access saved."})).into_response())
}

/// `save()` for an invitation: returns the grants to the invite form and stores nothing.
async fn invitation_access(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Json(body): Json<Value>,
) -> ApiResult {
    if !ctx.is_team_admin() {
        return Ok(unauthorized());
    }
    let grants = match validated_grants(&state, ctx.team_id, &body).await? {
        Ok(g) => g,
        Err(errors) => return Ok(validation_response(errors)),
    };
    Ok(ok(json!({
        "access": grants.iter().map(Grant::to_value).collect::<Vec<_>>(),
        "summary": summary_of(&grants),
    }))
    .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summaries_match_upstream() {
        assert_eq!(summary(None), "Team-wide read (default)");
        assert_eq!(summary(Some("[]")), "No access");
        assert_eq!(
            summary(Some(
                r#"[{"scope":"team","uuid":null,"levels":["read","deploy"]},{"scope":"project","uuid":"a","levels":["write"]},{"scope":"project","uuid":"b","levels":["write"]},{"scope":"resource","uuid":"c","levels":["read"]}]"#
            )),
            "Team: read, deploy + 2 projects + 1 resource"
        );
        assert_eq!(
            summary(Some(
                r#"[{"scope":"team","uuid":null,"levels":["read:sensitive"]}]"#
            )),
            "Team: read secrets"
        );
    }

    #[test]
    fn rows_group_grants_with_the_same_levels() {
        let grants = vec![
            Grant {
                scope: AccessScope::Project,
                uuid: Some("a".into()),
                levels: vec![AccessLevel::Write],
            },
            Grant::team(vec![AccessLevel::Read]),
            Grant {
                scope: AccessScope::Resource,
                uuid: Some("r".into()),
                levels: vec![AccessLevel::Write],
            },
        ];
        let rows = rows_from(&grants);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["targets"], json!(["project:a", "resource:r"]));
        assert_eq!(rows[0]["levels"]["Write"], json!(true));
        assert_eq!(rows[0]["levels"]["Read"], json!(false));
        assert_eq!(rows[1]["targets"], json!(["team"]));
    }
}
