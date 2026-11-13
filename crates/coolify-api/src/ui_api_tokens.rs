//! `App\Livewire\Security\ApiTokens` and `App\Livewire\Forms\ApiTokenForm` (upstream 4.6.0):
//! tokens limited to projects, environments or resources, edited in place, paused, resumed and
//! rolled. A member can never give a token more access than the member has.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::middleware::from_fn_with_state;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use chrono::NaiveDateTime;
use coolify_db::access as acc;
use coolify_domain::grants::{AccessGrants, AccessLevel, AccessScope, Grant};
use rand::Rng;
use regex::Regex;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::sync::LazyLock;

use crate::audit::Meta;
use crate::auth::AuthCtx;
use crate::response::{ApiResult, ok};
use crate::state::AppState;
use crate::ui_notifications::{blank, toast_error, validation_failed};

pub fn routes(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/ui/security/api-tokens", get(index).post(create))
        .route(
            "/ui/security/api-tokens/{id}",
            axum::routing::patch(update).delete(revoke),
        )
        .route("/ui/security/api-tokens/{id}/paused", post(set_paused))
        .route("/ui/security/api-tokens/{id}/roll", post(roll))
        .route_layer(from_fn_with_state(state, crate::session::ui_guard))
}

/// `$expirationOptions`.
const EXPIRY_DAYS: [i64; 5] = [7, 30, 60, 90, 365];
/// `ApiTokenForm::KEEP_EXPIRY`.
const KEEP_EXPIRY: &str = "keep";
const ABILITIES: [&str; 7] = [
    "read",
    "read:sensitive",
    "write",
    "write:sensitive",
    "deploy",
    "delete",
    "root",
];
const NOT_FOUND: &str = "Not found.";

static SCOPE_VALUE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(project|environment|resource):[A-Za-z0-9_-]+$")
        .unwrap_or_else(|_| unreachable!())
});

fn iso8601(t: NaiveDateTime) -> String {
    format!("{}+00:00", t.format("%Y-%m-%dT%H:%M:%S"))
}

fn stamp(t: Option<NaiveDateTime>) -> Value {
    t.map_or(Value::Null, |t| {
        json!(t.format("%Y-%m-%dT%H:%M:%S").to_string())
    })
}

/// `hash('crc32b', ...)`.
fn crc32b(data: &[u8]) -> String {
    let mut crc: u32 = !0;
    for b in data {
        crc ^= u32::from(*b);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    format!("{:08x}", !crc)
}

/// `PersonalAccessToken::generateSecret`: the plain secret and its stored SHA-256.
fn generate_secret() -> (String, String) {
    let entropy: String = rand::thread_rng()
        .sample_iter(&rand::distributions::Alphanumeric)
        .take(40)
        .map(char::from)
        .collect();
    let plain = format!("{entropy}{}", crc32b(entropy.as_bytes()));
    let hash = Sha256::digest(plain.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    (plain, hash)
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

#[derive(Debug, Clone, sqlx::FromRow)]
struct TokenRow {
    id: i64,
    name: String,
    team_id: String,
    abilities: Option<String>,
    scopes: Option<String>,
    paused_at: Option<NaiveDateTime>,
    last_used_at: Option<NaiveDateTime>,
    created_at: Option<NaiveDateTime>,
    expires_at: Option<NaiveDateTime>,
    tokenable_id: i64,
}

impl TokenRow {
    fn abilities(&self) -> Vec<String> {
        self.abilities
            .as_deref()
            .and_then(|a| serde_json::from_str::<Vec<String>>(a).ok())
            .unwrap_or_default()
    }

    /// `$token->scopes`: `None` when the token is not limited, else the readable scopes
    /// (`Grant::listFrom($token->scopes)`, 4.7.5: an item that is not a scope is left out, so the
    /// page opens when a token has unreadable scopes).
    fn scopes(&self) -> Option<Vec<Value>> {
        readable_scopes(self.scopes.as_deref())
    }
}

fn columns(state: &AppState) -> String {
    let scopes = if acc::has_token_scopes_column(&state.db) {
        "scopes::text"
    } else {
        "NULL::text"
    };
    let paused = if acc::has_token_paused_column(&state.db) {
        "paused_at"
    } else {
        "NULL::timestamp"
    };
    format!(
        "id, name, team_id, abilities, {scopes} AS scopes, {paused} AS paused_at, last_used_at, \
         created_at, expires_at, tokenable_id"
    )
}

/// `auth()->user()->tokens()->where('team_id', currentTeam()->id)->latest()`.
async fn own_tokens(state: &AppState, ctx: &AuthCtx) -> Result<Vec<TokenRow>, coolify_db::DbError> {
    Ok(sqlx::query_as(&format!(
        "SELECT {} FROM personal_access_tokens \
         WHERE tokenable_type = 'App\\Models\\User' AND tokenable_id = $1 AND team_id = $2 \
         ORDER BY created_at DESC, id DESC",
        columns(state)
    ))
    .bind(ctx.user_id)
    .bind(ctx.team_id.to_string())
    .fetch_all(state.db.pool())
    .await?)
}

/// `findOwnToken`.
async fn find_own(
    state: &AppState,
    ctx: &AuthCtx,
    id: i64,
) -> Result<Option<TokenRow>, coolify_db::DbError> {
    Ok(sqlx::query_as(&format!(
        "SELECT {} FROM personal_access_tokens \
         WHERE tokenable_type = 'App\\Models\\User' AND tokenable_id = $1 AND team_id = $2 AND id = $3",
        columns(state)
    ))
    .bind(ctx.user_id)
    .bind(ctx.team_id.to_string())
    .bind(id)
    .fetch_optional(state.db.pool())
    .await?)
}

/// One option of `scopeOptions`.
#[derive(Debug, Clone)]
struct ScopeOption {
    value: String,
    label: String,
    header: bool,
    parents: Vec<String>,
}

impl ScopeOption {
    fn to_value(&self) -> Value {
        let mut m = Map::new();
        m.insert("value".into(), json!(self.value));
        m.insert("label".into(), json!(self.label));
        if self.header {
            m.insert("header".into(), json!(true));
        } else if !self.parents.is_empty() || !self.value.starts_with("project:") {
            m.insert("parents".into(), json!(self.parents));
        }
        Value::Object(m)
    }
}

fn header(value: &str, label: &str) -> ScopeOption {
    ScopeOption {
        value: value.into(),
        label: label.into(),
        header: true,
        parents: Vec::new(),
    }
}

/// `scopeOptions()`: the projects, environments and resources of the team the user can see. A
/// project or environment is an option only when the member has a grant on all of it (3fac00e61).
async fn scope_options(
    state: &AppState,
    ctx: &AuthCtx,
) -> Result<Vec<ScopeOption>, coolify_db::DbError> {
    let db = &state.db;
    let member = acc::member_grants(db, ctx.team_id, ctx.user_id).await?;
    let member_layer: Option<AccessGrants> = match &member {
        Some(g) => Some(acc::resolve(db, ctx.team_id, g).await?),
        None => None,
    };
    let projects: Vec<(i64, String, String)> =
        sqlx::query_as("SELECT id, uuid, name FROM projects WHERE team_id = $1 ORDER BY name")
            .bind(ctx.team_id)
            .fetch_all(db.pool())
            .await?;
    let projects: Vec<(i64, String, String)> = projects
        .into_iter()
        .filter(|p| acc::project_visible(p.0))
        .collect();
    let project_ids: Vec<i64> = projects.iter().map(|p| p.0).collect();
    let environments: Vec<(i64, String, String, i64)> = sqlx::query_as(
        "SELECT id, uuid, name, project_id FROM environments WHERE project_id = ANY($1)",
    )
    .bind(&project_ids)
    .fetch_all(db.pool())
    .await?;
    let environments: Vec<(i64, String, String, i64)> = environments
        .into_iter()
        .filter(|e| acc::environment_visible(e.0))
        .collect();
    let project_of = |id: i64| projects.iter().find(|p| p.0 == id);
    let path_of = |e: &(i64, String, String, i64)| {
        format!(
            "{} / {}",
            project_of(e.3).map(|p| p.2.as_str()).unwrap_or_default(),
            e.2
        )
    };
    let parents_of = |e: &(i64, String, String, i64)| {
        vec![
            format!(
                "project:{}",
                project_of(e.3).map(|p| p.1.as_str()).unwrap_or_default()
            ),
            format!("environment:{}", e.1),
        ]
    };

    let env_ids: Vec<i64> = environments.iter().map(|e| e.0).collect();
    let mut resources: Vec<ScopeOption> = Vec::new();
    for table in acc::RESOURCE_TABLES {
        let rows: Vec<(String, String, i64)> = sqlx::query_as(&format!(
            "SELECT uuid, name, environment_id FROM {table} \
             WHERE deleted_at IS NULL AND environment_id = ANY($1)"
        ))
        .bind(&env_ids)
        .fetch_all(db.pool())
        .await?;
        for (uuid, name, env) in rows {
            if !acc::resource_visible(Some(env), &uuid) {
                continue;
            }
            let Some(e) = environments.iter().find(|e| e.0 == env) else {
                continue;
            };
            resources.push(ScopeOption {
                value: format!("resource:{uuid}"),
                label: format!("{} / {name}", path_of(e)),
                header: false,
                parents: parents_of(e),
            });
        }
    }
    resources.sort_by(|a, b| natural_case(&a.label, &b.label));

    let selectable_projects: Vec<&(i64, String, String)> = projects
        .iter()
        .filter(|p| {
            member_layer
                .as_ref()
                .is_none_or(|l| l.allows(AccessLevel::Read, Some(p.0), None, None))
        })
        .collect();
    let mut selectable_envs: Vec<ScopeOption> = environments
        .iter()
        .filter(|e| {
            member_layer
                .as_ref()
                .is_none_or(|l| l.allows(AccessLevel::Read, Some(e.3), Some(e.0), None))
        })
        .map(|e| ScopeOption {
            value: format!("environment:{}", e.1),
            label: path_of(e),
            header: false,
            parents: vec![parents_of(e)[0].clone()],
        })
        .collect();
    selectable_envs.sort_by(|a, b| natural_case(&a.label, &b.label));

    let mut out = Vec::new();
    if !selectable_projects.is_empty() {
        out.push(header("__projects", "Projects"));
    }
    out.extend(selectable_projects.iter().map(|p| ScopeOption {
        value: format!("project:{}", p.1),
        label: p.2.clone(),
        header: false,
        parents: Vec::new(),
    }));
    if !selectable_envs.is_empty() {
        out.push(header("__environments", "Environments"));
    }
    out.extend(selectable_envs);
    if !resources.is_empty() {
        out.push(header("__resources", "Resources"));
    }
    out.extend(resources);
    Ok(out)
}

/// `ApiTokenPolicy::holdsSomewhere`: some grant of the member includes the level.
fn holds_somewhere(member: Option<&Vec<Grant>>, level: AccessLevel) -> bool {
    member.is_some_and(|grants| {
        grants
            .iter()
            .any(|g| g.levels.iter().any(|l| l.includes().contains(&level)))
    })
}

/// `ApiTokenForm::valuesOf`: what the edit modal opens with.
fn values_of(t: &TokenRow, now: NaiveDateTime) -> Value {
    let mut permissions = t.abilities();
    if t.abilities.is_none() {
        permissions = vec!["read".into()];
    }
    permissions.sort();
    let current = t
        .expires_at
        .map_or("Never".to_owned(), |e| e.format("%Y-%m-%d").to_string());
    let keep = if t.expires_at.is_some_and(|e| e < now) {
        format!("Keep current (expired {current})")
    } else {
        format!("Keep current ({current})")
    };
    let scopes = t.scopes();
    json!({
        "id": t.id,
        "description": t.name,
        "permissions": permissions,
        "scopeMode": if scopes.is_none() { "team" } else { "selected" },
        "selectedScopes": scopes.unwrap_or_default().iter().map(|s| format!(
            "{}:{}",
            s.get("scope").and_then(Value::as_str).unwrap_or_default(),
            s.get("uuid").and_then(Value::as_str).unwrap_or_default()
        )).collect::<Vec<_>>(),
        "expiresInDays": KEEP_EXPIRY,
        "keepLabel": keep,
    })
}

/// `GET`: the page (`mount`, `render`).
async fn index(State(state): State<AppState>, Extension(ctx): Extension<AuthCtx>) -> ApiResult {
    let now = state.now();
    let tokens = own_tokens(&state, &ctx).await?;
    let options = scope_options(&state, &ctx).await?;
    let labels: BTreeMap<String, String> = options
        .iter()
        .filter(|o| !o.header)
        .map(|o| (o.value.clone(), o.label.clone()))
        .collect();
    let member = acc::member_grants(&state.db, ctx.team_id, ctx.user_id).await?;
    let admin = ctx.is_team_admin();
    let can = |level: AccessLevel| admin || holds_somewhere(member.as_ref(), level);
    let api_enabled: bool =
        sqlx::query_scalar("SELECT is_api_enabled FROM instance_settings WHERE id = 0")
            .fetch_optional(state.db.pool())
            .await
            .map_err(coolify_db::DbError::from)?
            .unwrap_or(true);
    let instance_admin =
        coolify_db::instance_settings::is_instance_admin(&state.db, ctx.user_id).await?;
    let rows: Vec<Value> = tokens
        .iter()
        .map(|t| {
            json!({
                "id": t.id,
                "name": t.name,
                "abilities": t.abilities(),
                "scopes": t.scopes(),
                "paused": t.paused_at.is_some(),
                "last_used_at": stamp(t.last_used_at),
                "created_at": stamp(t.created_at),
                "expires_at": stamp(t.expires_at),
                "own": t.tokenable_id == ctx.user_id,
                "edit": values_of(t, now),
            })
        })
        .collect();
    Ok(ok(json!({
        "tokens": rows,
        "api_enabled": api_enabled,
        "instance_admin": instance_admin,
        "can_use": {
            "root": can(AccessLevel::Root),
            "write": can(AccessLevel::Write),
            "deploy": can(AccessLevel::Deploy),
            "sensitive": can(AccessLevel::ReadSensitive),
            "delete": can(AccessLevel::Delete),
            "team_scope": ctx.covers_whole_team(),
        },
        "scope_options": options.iter().map(ScopeOption::to_value).collect::<Vec<_>>(),
        "scope_labels": labels,
        "scopes_supported": acc::has_token_scopes_column(&state.db),
        "pause_supported": acc::has_token_paused_column(&state.db),
    }))
    .into_response())
}

/// The expiry choice of the form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expiry {
    Never,
    /// `ApiTokenForm::KEEP_EXPIRY`, in the edit form only.
    Keep,
    Days(i64),
}

/// What `validateTokenForm` returns.
struct Validated {
    description: String,
    permissions: Vec<String>,
    /// `None` = every resource of the team.
    scopes: Option<Vec<Grant>>,
    expiry: Expiry,
}

fn strings(v: Option<&Value>) -> Option<Vec<Value>> {
    match v {
        Some(Value::Array(a)) => Some(a.clone()),
        Some(Value::Object(o)) => Some(o.values().cloned().collect()),
        _ => None,
    }
}

/// `validateTokenForm`: the form rules, then the access checks. Never trusts the page's
/// `canUse*` flags: the access of the user in the team is checked again.
async fn validate_form(
    state: &AppState,
    ctx: &AuthCtx,
    body: &Value,
    editing: bool,
) -> Result<Validated, Response> {
    let mut errors: Vec<String> = Vec::new();
    let description = body
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    if blank(body.get("description")) {
        errors.push("The description field is required.".into());
    } else {
        let n = description.chars().count();
        if n < 3 {
            errors.push("The description field must be at least 3 characters.".into());
        }
        if n > 255 {
            errors.push("The description field must not be greater than 255 characters.".into());
        }
    }
    let mut expiry = Expiry::Never;
    if let Some(v) = body.get("expires_in_days").filter(|v| !blank(Some(v))) {
        let text = match v {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        if editing && text == KEEP_EXPIRY {
            expiry = Expiry::Keep;
        } else {
            match text.parse::<i64>() {
                Ok(d) if EXPIRY_DAYS.contains(&d) => expiry = Expiry::Days(d),
                _ => errors.push("The selected expires in days is invalid.".into()),
            }
        }
    }
    let permissions = strings(body.get("permissions"));
    let mut perms: Vec<String> = Vec::new();
    match &permissions {
        None => errors.push("The permissions field is required.".into()),
        Some(list) if list.is_empty() => {
            errors.push("The permissions field is required.".into());
        }
        Some(list) => {
            for (i, p) in list.iter().enumerate() {
                match p {
                    Value::Null => errors.push(format!("The permissions.{i} field is required.")),
                    Value::String(s) if s.trim().is_empty() => {
                        errors.push(format!("The permissions.{i} field is required."));
                    }
                    Value::String(s) if ABILITIES.contains(&s.as_str()) => perms.push(s.clone()),
                    Value::String(_) => {
                        errors.push(format!("The selected permissions.{i} is invalid."));
                    }
                    _ => {
                        errors.push(format!("The permissions.{i} field must be a string."));
                        errors.push(format!("The selected permissions.{i} is invalid."));
                    }
                }
            }
        }
    }
    let scope_mode = body
        .get("scope_mode")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if blank(body.get("scope_mode")) {
        errors.push("The scope mode field is required.".into());
    } else if !["team", "selected"].contains(&scope_mode) {
        errors.push("The selected scope mode is invalid.".into());
    }
    let mut selected: Vec<String> = Vec::new();
    if scope_mode == "selected" {
        match strings(body.get("selected_scopes")) {
            Some(list) if !list.is_empty() => {
                for (i, v) in list.iter().enumerate() {
                    match v.as_str() {
                        Some(s) if SCOPE_VALUE.is_match(s) => selected.push(s.to_owned()),
                        Some(_) => {
                            errors.push(format!("The selected scopes.{i} field format is invalid."))
                        }
                        None => {
                            errors.push(format!("The selected scopes.{i} field must be a string."))
                        }
                    }
                }
            }
            _ => errors.push("Select at least one project, environment, or resource.".into()),
        }
    }
    if let Some(r) = validation_failed(&errors) {
        return Err(r);
    }
    let fail = |m: &str| toast_error(StatusCode::UNPROCESSABLE_ENTITY, m);
    let db_fail = |_e: coolify_db::DbError| {
        toast_error(StatusCode::INTERNAL_SERVER_ERROR, "Something went wrong.")
    };

    // `withoutCoveredScopes`: a selected project covers its environments and resources.
    let options = scope_options(state, ctx).await.map_err(db_fail)?;
    let mut unique: Vec<String> = Vec::new();
    for s in selected {
        if !unique.contains(&s) {
            unique.push(s);
        }
    }
    let kept: Vec<String> = unique
        .iter()
        .filter(|v| {
            let parents = options
                .iter()
                .find(|o| &&o.value == v)
                .map(|o| o.parents.clone())
                .unwrap_or_default();
            !parents.iter().any(|p| unique.contains(p))
        })
        .cloned()
        .collect();

    let scopes = if scope_mode == "selected" {
        let mut out = Vec::new();
        for value in &kept {
            let (scope, uuid) = value.split_once(':').unwrap_or_default();
            let Some(scope) = AccessScope::parse(scope) else {
                return Err(fail(
                    "A selected project, environment, or resource does not exist in this team.",
                ));
            };
            let probe = Grant {
                scope,
                uuid: Some(uuid.to_owned()),
                levels: vec![AccessLevel::Read],
            };
            let layer = acc::resolve(&state.db, ctx.team_id, std::slice::from_ref(&probe))
                .await
                .map_err(db_fail)?;
            let exists = match scope {
                AccessScope::Project => !layer.visible_project_ids().is_empty(),
                AccessScope::Environment => !layer.full_environment_ids().is_empty(),
                _ => !layer.resource_uuids().is_empty(),
            };
            if !exists {
                return Err(fail(
                    "A selected project, environment, or resource does not exist in this team.",
                ));
            }
            out.push(Grant {
                scope,
                uuid: Some(uuid.to_owned()),
                levels: Vec::new(),
            });
        }
        Some(out)
    } else {
        None
    };

    if perms.iter().any(|p| p == "write:sensitive") && !ctx.is_team_admin() {
        return Err(fail(
            "You do not have permission to give tokens write:sensitive permissions.",
        ));
    }
    let levels = AccessLevel::parse_many(perms.iter().map(String::as_str));
    let grants: Vec<Grant> = match &scopes {
        None => vec![Grant::team(levels)],
        Some(list) => list
            .iter()
            .map(|g| Grant {
                levels: levels.clone(),
                ..g.clone()
            })
            .collect(),
    };
    let member = acc::member_grants(&state.db, ctx.team_id, ctx.user_id)
        .await
        .map_err(db_fail)?;
    if !acc::user_holds(&state.db, ctx.team_id, member, &grants)
        .await
        .map_err(db_fail)?
    {
        return Err(fail(
            "A token cannot have more access than you have. Choose fewer permissions or narrower projects, environments, or resources.",
        ));
    }
    if scopes.is_some() && !acc::has_token_scopes_column(&state.db) {
        return Err(fail(
            "Limiting a token to projects needs the database schema of Coolify 4.6.0.",
        ));
    }
    Ok(Validated {
        description,
        permissions: perms,
        scopes,
        expiry,
    })
}

/// The scopes of a stored `scopes` column as `{"scope", "uuid"}` items. NULL (SQL or JSON) is a
/// token for the whole team; anything else limits the token, readable or not.
pub(crate) fn readable_scopes(stored: Option<&str>) -> Option<Vec<Value>> {
    let value: Value = serde_json::from_str(stored?).ok()?;
    if value.is_null() {
        return None;
    }
    Some(
        Grant::list_from(&value)
            .iter()
            .map(|g| json!({"scope": g.scope.as_str(), "uuid": g.uuid}))
            .collect(),
    )
}

/// Stored `scopes`: `[{"scope": ..., "uuid": ...}]`, or NULL for the whole team.
fn stored_scopes(scopes: &Option<Vec<Grant>>) -> Value {
    match scopes {
        None => Value::Null,
        Some(list) => Value::Array(
            list.iter()
                .map(|g| json!({"scope": g.scope.as_str(), "uuid": g.uuid}))
                .collect(),
        ),
    }
}

fn php_json(v: &Value) -> String {
    coolify_domain::grants::php_json_encode(v)
}

/// `addNewToken`.
async fn create(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Json(body): Json<Value>,
) -> ApiResult {
    let v = match validate_form(&state, &ctx, &body, false).await {
        Ok(v) => v,
        Err(r) => return Ok(r),
    };
    let now = state.now();
    let expires = match v.expiry {
        Expiry::Days(d) => Some(now + chrono::Duration::days(d)),
        Expiry::Never | Expiry::Keep => None,
    };
    let (plain, hash) = generate_secret();
    let scopes = stored_scopes(&v.scopes);
    // One transaction, so a token never exists team-wide when saving its scopes fails.
    let mut tx = state
        .db
        .pool()
        .begin()
        .await
        .map_err(coolify_db::DbError::from)?;
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO personal_access_tokens (tokenable_type, tokenable_id, name, token, abilities, expires_at, team_id, created_at, updated_at) \
         VALUES ('App\\Models\\User', $1, $2, $3, $4, $5, $6, $7, $7) RETURNING id",
    )
    .bind(ctx.user_id)
    .bind(&v.description)
    .bind(hash)
    .bind(php_json(&json!(v.permissions)))
    .bind(expires)
    .bind(ctx.team_id.to_string())
    .bind(now)
    .fetch_one(&mut *tx)
    .await
    .map_err(coolify_db::DbError::from)?;
    if acc::has_token_scopes_column(&state.db) {
        sqlx::query(
            "UPDATE personal_access_tokens SET scopes = $2::json, updated_at = $3 WHERE id = $1",
        )
        .bind(id)
        .bind((!scopes.is_null()).then(|| php_json(&scopes)))
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(coolify_db::DbError::from)?;
    }
    tx.commit().await.map_err(coolify_db::DbError::from)?;
    crate::audit::log(
        &state,
        &ctx,
        &meta,
        "ui.api_token.created",
        json!({
            "team_id": ctx.team_id,
            "api_token_name": v.description,
            "abilities": v.permissions,
            "scopes": scopes,
            "expires_at": expires.map(iso8601),
        }),
        "info",
    )
    .await;
    // The numeric prefix stays: Sanctum looks tokens up by it.
    Ok(ok(json!({"token": format!("{id}|{plain}")})).into_response())
}

/// `updateToken`: the token value does not change; the next request with it uses the new access.
async fn update(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path(id): Path<i64>,
    Json(body): Json<Value>,
) -> ApiResult {
    let Some(token) = find_own(&state, &ctx, id).await? else {
        return Ok(toast_error(StatusCode::NOT_FOUND, NOT_FOUND));
    };
    let v = match validate_form(&state, &ctx, &body, true).await {
        Ok(v) => v,
        Err(r) => return Ok(r),
    };
    let now = state.now();
    let before = json!({
        "abilities": token.abilities(),
        "scopes": token.scopes(),
        "expires_at": token.expires_at.map(iso8601),
    });
    let scopes = stored_scopes(&v.scopes);
    // "Never" removes the expiry, "keep" keeps it; a new expiry needs a new warning.
    let keep = v.expiry == Expiry::Keep;
    let new_expiry = match v.expiry {
        Expiry::Keep => token.expires_at,
        Expiry::Days(d) => Some(now + chrono::Duration::days(d)),
        Expiry::Never => None,
    };
    let scopes_sql = if acc::has_token_scopes_column(&state.db) {
        ", scopes = $5::json"
    } else {
        ""
    };
    let expiry_sql = if keep {
        ""
    } else {
        ", expires_at = $4, api_token_expiration_warning_sent_at = NULL"
    };
    sqlx::query(&format!(
        "UPDATE personal_access_tokens SET name = $2, abilities = $3, updated_at = $6{expiry_sql}{scopes_sql} \
         WHERE id = $1"
    ))
    .bind(token.id)
    .bind(&v.description)
    .bind(php_json(&json!(v.permissions)))
    .bind(new_expiry)
    .bind((!scopes.is_null()).then(|| php_json(&scopes)))
    .bind(now)
    .execute(state.db.pool())
    .await
    .map_err(coolify_db::DbError::from)?;
    crate::audit::log(
        &state,
        &ctx,
        &meta,
        "ui.api_token.updated",
        json!({
            "team_id": ctx.team_id,
            "api_token_name": v.description,
            "before": before,
            "after": {
                "abilities": v.permissions,
                "scopes": scopes,
                "expires_at": new_expiry.map(iso8601),
            },
        }),
        "info",
    )
    .await;
    Ok(ok(json!({"message": "API token updated."})).into_response())
}

/// `setPaused`: the caller sends the wanted state, so a stale page cannot resume a token that
/// another page paused.
async fn set_paused(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path(id): Path<i64>,
    Json(body): Json<Value>,
) -> ApiResult {
    let paused = body.get("paused").and_then(Value::as_bool).unwrap_or(true);
    let Some(token) = find_own(&state, &ctx, id).await? else {
        return Ok(toast_error(StatusCode::NOT_FOUND, NOT_FOUND));
    };
    if !acc::has_token_paused_column(&state.db) {
        return Ok(toast_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Pausing a token needs the database schema of Coolify 4.6.0.",
        ));
    }
    if token.paused_at.is_some() != paused {
        let now = state.now();
        sqlx::query(
            "UPDATE personal_access_tokens SET paused_at = $2, updated_at = $3 WHERE id = $1",
        )
        .bind(token.id)
        .bind(paused.then_some(now))
        .bind(now)
        .execute(state.db.pool())
        .await
        .map_err(coolify_db::DbError::from)?;
        crate::audit::log(
            &state,
            &ctx,
            &meta,
            if paused {
                "ui.api_token.paused"
            } else {
                "ui.api_token.resumed"
            },
            json!({"team_id": ctx.team_id, "api_token_name": token.name}),
            "info",
        )
        .await;
    }
    Ok(
        ok(json!({"message": if paused { "API token paused." } else { "API token resumed." }}))
            .into_response(),
    )
}

/// `roll`: a new secret replaces the token's; abilities, scopes and expiry stay.
async fn roll(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path(id): Path<i64>,
) -> ApiResult {
    let Some(token) = find_own(&state, &ctx, id).await? else {
        return Ok(toast_error(StatusCode::NOT_FOUND, NOT_FOUND));
    };
    let (plain, hash) = generate_secret();
    sqlx::query(
        "UPDATE personal_access_tokens SET token = $2, last_used_at = NULL, updated_at = $3 WHERE id = $1",
    )
    .bind(token.id)
    .bind(hash)
    .bind(state.now())
    .execute(state.db.pool())
    .await
    .map_err(coolify_db::DbError::from)?;
    crate::audit::log(
        &state,
        &ctx,
        &meta,
        "ui.api_token.rolled",
        json!({"team_id": token.team_id, "api_token_name": token.name}),
        "info",
    )
    .await;
    Ok(ok(json!({"token": format!("{}|{plain}", token.id)})).into_response())
}

/// `revoke`.
async fn revoke(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path(id): Path<i64>,
) -> ApiResult {
    let Some(token) = find_own(&state, &ctx, id).await? else {
        return Ok(toast_error(StatusCode::NOT_FOUND, NOT_FOUND));
    };
    sqlx::query("DELETE FROM personal_access_tokens WHERE id = $1")
        .bind(token.id)
        .execute(state.db.pool())
        .await
        .map_err(coolify_db::DbError::from)?;
    crate::audit::log(
        &state,
        &ctx,
        &meta,
        "ui.api_token.revoked",
        json!({"team_id": ctx.team_id, "api_token_name": token.name}),
        "info",
    )
    .await;
    Ok(ok(json!({})).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `Grant::listFrom($token->scopes)` (4.7.5): the page opens when a token has unreadable
    /// scopes.
    #[test]
    fn unreadable_token_scopes_are_left_out() {
        assert_eq!(readable_scopes(None), None);
        assert_eq!(readable_scopes(Some("null")), None);
        assert_eq!(
            readable_scopes(Some(
                r#"[{"scope":"project","uuid":"p-one"},"junk",null,{"scope":"nope","uuid":"x"},{"scope":"resource"},{"scope":"environment","uuid":"e-one"}]"#
            )),
            Some(vec![
                json!({"scope": "project", "uuid": "p-one"}),
                json!({"scope": "environment", "uuid": "e-one"}),
            ])
        );
        // A value that is not a list still limits the token: no readable scope, not the whole team.
        assert_eq!(readable_scopes(Some(r#""broken""#)), Some(vec![]));
        assert_eq!(readable_scopes(Some("[]")), Some(vec![]));
    }

    #[test]
    fn crc32b_matches_php() {
        // Checked with PHP `hash("crc32b", ...)`.
        assert_eq!(
            crc32b(b"The quick brown fox jumped over the lazy dog."),
            "82a34642"
        );
    }

    #[test]
    fn natural_case_sorts_like_php() {
        let mut v = vec!["b10", "B2", "a"];
        v.sort_by(|a, b| natural_case(a, b));
        assert_eq!(v, vec!["a", "B2", "b10"]);
    }

    #[test]
    fn scope_values() {
        assert!(SCOPE_VALUE.is_match("project:abc_1-2"));
        assert!(!SCOPE_VALUE.is_match("team:abc"));
        assert!(!SCOPE_VALUE.is_match("project:a b"));
    }

    #[test]
    fn holds_somewhere_reads_levels() {
        let member = vec![Grant {
            scope: AccessScope::Project,
            uuid: Some("p".into()),
            levels: vec![AccessLevel::Write],
        }];
        assert!(holds_somewhere(Some(&member), AccessLevel::Deploy));
        assert!(!holds_somewhere(Some(&member), AccessLevel::Delete));
        assert!(!holds_somewhere(None, AccessLevel::Read));
    }
}
