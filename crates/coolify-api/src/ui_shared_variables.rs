//! Page endpoints of `App\Livewire\SharedVariables\*` with the shared-variable behaviour of
//! `Project\Shared\EnvironmentVariable\{Add, Show}`.

use std::collections::{HashMap, HashSet};

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::middleware::from_fn_with_state;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use coolify_db::records::Record;
use coolify_db::servers::ColumnValue as C;
use coolify_db::shared_variables::{self as sv, NewShared, Scope};
use regex::Regex;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::audit::Meta;
use crate::auth::AuthCtx;
use crate::response::{ApiError, ApiResult, PhpJson, ok};
use crate::state::AppState;
use crate::ui_notifications::{toast_error, unauthorized};
use crate::ui_shared::dev_view;

const BUILT_IN: &[&str] = &["COOLIFY_SERVER_UUID", "COOLIFY_SERVER_NAME"];

pub fn routes(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/ui/shared-variables/overview", get(overview))
        .route("/ui/shared-variables/owners", get(owners))
        .route("/ui/shared-variables/scope", get(scope_data))
        .route("/ui/shared-variables/scope/add", post(add))
        .route("/ui/shared-variables/scope/bulk", post(bulk))
        .route(
            "/ui/shared-variables/variable/{id}",
            get(value).patch(update).delete(destroy),
        )
        .route("/ui/shared-variables/variable/{id}/lock", post(lock))
        .route_layer(from_fn_with_state(state, crate::session::ui_guard))
}

#[derive(Debug, Deserialize)]
struct ScopeQuery {
    kind: String,
    uuid: Option<String>,
    env: Option<String>,
}

struct Resolved {
    scope: Scope,
    title: String,
    /// The project and environment for the breadcrumb.
    project_uuid: Option<String>,
    environment_uuid: Option<String>,
}

/// The `mount()` of the scope pages: the owner in the session team.
async fn resolve(
    state: &AppState,
    ctx: &AuthCtx,
    q: &ScopeQuery,
) -> Result<Option<Resolved>, ApiError> {
    let uuid = q.uuid.as_deref().unwrap_or_default();
    Ok(match q.kind.as_str() {
        "team" => Some(Resolved {
            scope: Scope::Team,
            title: "Team variables".to_owned(),
            project_uuid: None,
            environment_uuid: None,
        }),
        "project" => coolify_db::projects::find(&state.db, ctx.team_id, uuid)
            .await?
            .map(|p| Resolved {
                scope: Scope::Project(p.id),
                title: p.name,
                project_uuid: Some(p.uuid),
                environment_uuid: None,
            }),
        "environment" => {
            let Some(p) = coolify_db::projects::find(&state.db, ctx.team_id, uuid).await? else {
                return Ok(None);
            };
            coolify_db::projects::find_environment(
                &state.db,
                p.id,
                q.env.as_deref().unwrap_or_default(),
            )
            .await?
            .map(|e| Resolved {
                scope: Scope::Environment(e.id),
                title: format!("{} / {}", p.name, e.name),
                project_uuid: Some(p.uuid.clone()),
                environment_uuid: Some(e.uuid),
            })
        }
        "server" => coolify_db::servers::find(&state.db, ctx.team_id, uuid)
            .await?
            .map(|s| Resolved {
                scope: Scope::Server(s.id),
                title: text(&s.record, "name").unwrap_or_default().to_owned(),
                project_uuid: None,
                environment_uuid: None,
            }),
        _ => None,
    })
}

fn text<'a>(r: &'a Record, k: &str) -> Option<&'a str> {
    r.get(k).and_then(Value::as_str)
}

fn flag(r: &Record, k: &str) -> bool {
    r.get(k).and_then(Value::as_bool).unwrap_or(false)
}

fn decrypt(state: &AppState, r: &Record) -> Result<Option<String>, ApiError> {
    let Some(c) = text(r, "value") else {
        return Ok(None);
    };
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    enc.decrypt_string(c)
        .map(Some)
        .map_err(|_| ApiError::Internal)
}

/// `getDevView` / `formatEnvironmentVariables`, sorted by key.
fn dev_view(state: &AppState, ctx: &AuthCtx, rows: &[Record]) -> Result<String, ApiError> {
    let mut sorted: Vec<&Record> = rows
        .iter()
        .filter(|r| !BUILT_IN.contains(&text(r, "key").unwrap_or_default()) || !is_server(r))
        .collect();
    sorted.sort_by_key(|r| text(r, "key").unwrap_or_default().to_owned());
    let mut lines = Vec::new();
    let can_view = ctx.is_team_admin();
    for r in sorted {
        // `formatEnvVariableForDevView`.
        let value = if can_view { decrypt(state, r)? } else { None };
        let flags = [("literal", flag(r, "is_literal"))];
        let v = dev_view::DevVariable {
            key: text(r, "key").unwrap_or_default(),
            value: value.as_deref(),
            is_shown_once: flag(r, "is_shown_once"),
            is_multiline: flag(r, "is_multiline"),
            flags: &flags,
        };
        lines.push(dev_view::format_variable(
            &v,
            can_view,
            dev_view::SHARED_FLAGS,
        ));
    }
    Ok(lines.join("\n"))
}

fn is_server(r: &Record) -> bool {
    text(r, "type") == Some("server")
}

async fn scope_data(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Query(q): Query<ScopeQuery>,
) -> ApiResult {
    let Some(res) = resolve(&state, &ctx, &q).await? else {
        return Ok(ok(json!({"found": false})).into_response());
    };
    let rows = sv::list(&state.db, ctx.team_id, res.scope).await?;
    let mut variables = Vec::new();
    for r in &rows {
        let read_only = is_server(r) && BUILT_IN.contains(&text(r, "key").unwrap_or_default());
        variables.push(json!({
            "id": r.get("id"),
            "key": r.get("key"),
            "comment": r.get("comment"),
            "is_multiline": flag(r, "is_multiline"),
            "is_literal": flag(r, "is_literal"),
            "is_shown_once": flag(r, "is_shown_once"),
            "read_only": read_only,
            "value": if read_only { decrypt(&state, r)? } else { None },
        }));
    }
    Ok(ok(json!({
        "found": true,
        "title": res.title,
        "project_uuid": res.project_uuid,
        "environment_uuid": res.environment_uuid,
        "can_update": ctx.is_team_admin(),
        "variables": variables,
        "dev_view": dev_view(&state, &ctx, &rows)?,
    }))
    .into_response())
}

fn field_errors(errors: Map<String, Value>) -> Response {
    let first = errors
        .values()
        .next()
        .and_then(|v| v.get(0))
        .and_then(Value::as_str)
        .unwrap_or("The given data was invalid.")
        .to_owned();
    let count: usize = errors
        .values()
        .map(|v| v.as_array().map_or(0, Vec::len))
        .sum();
    let message = match count.saturating_sub(1) {
        0 => first,
        1 => format!("{first} (and 1 more error)"),
        n => format!("{first} (and {n} more errors)"),
    };
    PhpJson(
        StatusCode::UNPROCESSABLE_ENTITY,
        json!({"message": message, "errors": errors}),
    )
    .into_response()
}

fn key_pattern() -> Result<Regex, ApiError> {
    Regex::new(r"\A[A-Za-z_][A-Za-z0-9_.]*\z").map_err(|_| ApiError::Internal)
}

/// `ValidationPatterns::environmentVariableKeyRules` with its messages, and the comment rule.
fn validate_fields(key: &str, comment: Option<&str>) -> Result<Map<String, Value>, ApiError> {
    let mut errors = Map::new();
    if key.is_empty() {
        errors.insert("key".into(), json!(["The key field is required."]));
    } else {
        let mut list = Vec::new();
        if key.chars().count() > 255 {
            list.push("The key may not be greater than 255 characters.");
        }
        if !key_pattern()?.is_match(key) {
            list.push(
                "The key must start with a letter or underscore and may only contain letters, numbers, underscores, and dots.",
            );
        }
        if !list.is_empty() {
            errors.insert("key".into(), json!(list));
        }
    }
    if comment.is_some_and(|c| c.chars().count() > 256) {
        errors.insert(
            "comment".into(),
            json!(["The comment field must not be greater than 256 characters."]),
        );
    }
    Ok(errors)
}

/// `Auditable` on `SharedEnvironmentVariable::create`.
async fn audit_created(
    state: &AppState,
    ctx: &AuthCtx,
    meta: &Meta,
    scope: Scope,
    n: &NewShared<'_>,
) {
    crate::audit::mutation(
        state,
        ctx,
        meta,
        "shared_environment_variable",
        "created",
        json!({
            "team_id": ctx.team_id,
            "shared_environment_variable_uuid": null,
            "shared_environment_variable_name": n.key,
            "changed_fields": [],
        }),
        json!({
            "key": {"old": null, "new": n.key},
            "is_literal": {"old": null, "new": n.is_literal},
            "is_multiline": {"old": null, "new": n.is_multiline},
            "is_shown_once": {"old": null, "new": n.is_shown_once},
            "comment": {"old": null, "new": n.comment},
            "type": {"old": null, "new": scope.kind()},
        }),
    )
    .await;
}

/// `Add::submit` and the page's `saveKey`.
async fn add(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Query(q): Query<ScopeQuery>,
    Json(body): Json<Value>,
) -> ApiResult {
    let Some(res) = resolve(&state, &ctx, &q).await? else {
        return Ok(toast_error(StatusCode::NOT_FOUND, "Not found."));
    };
    let key = body
        .get("key")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_owned();
    let comment = body.get("comment").and_then(Value::as_str);
    let errors = validate_fields(&key, comment)?;
    if !errors.is_empty() {
        return Ok(field_errors(errors));
    }
    if !ctx.is_team_admin() {
        return Ok(unauthorized());
    }
    if sv::key_exists(&state.db, ctx.team_id, res.scope, &key, None).await? {
        return Ok(toast_error(
            StatusCode::CONFLICT,
            "Variable already exists.",
        ));
    }
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let plain = body
        .get("value")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let cipher = enc.encrypt_string(plain);
    let n = NewShared {
        team_id: ctx.team_id,
        key: &key,
        value_encrypted: Some(&cipher),
        is_literal: false,
        is_multiline: body
            .get("is_multiline")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        is_shown_once: false,
        comment,
    };
    sv::create(&state.db, res.scope, &n, state.now()).await?;
    audit_created(&state, &ctx, &meta, res.scope, &n).await;
    Ok(ok(json!({})).into_response())
}

/// `submit` of a scope page: `handleBulkSubmit`.
async fn bulk(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Query(q): Query<ScopeQuery>,
    Json(body): Json<Value>,
) -> ApiResult {
    let Some(res) = resolve(&state, &ctx, &q).await? else {
        return Ok(toast_error(StatusCode::NOT_FOUND, "Not found."));
    };
    if !ctx.is_team_admin() {
        return Ok(unauthorized());
    }
    let contents = body
        .get("variables")
        .and_then(Value::as_str)
        .unwrap_or_default();
    // `parseEnvDevView`, before the save, so that an invalid "# coolify:" line changes nothing.
    let parsed = match dev_view::parse(contents, dev_view::SHARED_FLAGS) {
        Ok(p) => p,
        Err(m) => return Ok(toast_error(StatusCode::UNPROCESSABLE_ENTITY, m)),
    };
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let pattern = key_pattern()?;
    let rows = sv::list(&state.db, ctx.team_id, res.scope).await?;
    let wanted: HashSet<&str> = parsed.iter().map(|p| p.key.as_str()).collect();
    // The model's `key` mutator refuses an invalid key; the transaction leaves everything as it was.
    if parsed.iter().any(|p| {
        !rows.iter().any(|r| text(r, "key") == Some(p.key.as_str()))
            && !pattern.is_match(p.key.trim())
    }) {
        return Ok(toast_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "The key must start with a letter or underscore and may only contain letters, numbers, underscores, and dots.",
        ));
    }
    let mut changed = false;
    // `deleteRemovedVariables` deletes through the query builder: no model events.
    for r in &rows {
        let key = text(r, "key").unwrap_or_default();
        if is_server(r) && BUILT_IN.contains(&key) {
            continue;
        }
        if !wanted.contains(key) {
            let id = r.get("id").and_then(Value::as_i64).unwrap_or(0);
            coolify_db::server_ops::delete_shared_env(&state.db, id).await?;
            changed = true;
        }
    }
    for p in &parsed {
        let (key, plain) = (&p.key, &p.value);
        match rows.iter().find(|r| text(r, "key") == Some(key)) {
            Some(r) => {
                let current = decrypt(&state, r)?;
                // A value the view could not show is not overwritten by its placeholder.
                if flag(r, "is_shown_once")
                    || flag(r, "is_multiline")
                    || dev_view::format_value(current.as_deref()).is_none()
                {
                    continue;
                }
                let mut columns: Vec<(&'static str, C)> = Vec::new();
                let mut fields: Vec<&str> = Vec::new();
                let mut audit_changes = Map::new();
                if current.as_deref() != Some(plain.as_str()) {
                    columns.push(("value", C::Text(Some(enc.encrypt_string(plain)))));
                    fields.push("value");
                }
                // `$found->fill($data['settings'])`.
                if let Some(literal) = p.setting("is_literal")
                    && literal != flag(r, "is_literal")
                {
                    columns.push(("is_literal", C::Bool(literal)));
                    fields.push("is_literal");
                    audit_changes.insert(
                        "is_literal".into(),
                        json!({"old": flag(r, "is_literal"), "new": literal}),
                    );
                }
                if !columns.is_empty() {
                    let id = r.get("id").and_then(Value::as_i64).unwrap_or(0);
                    coolify_db::server_ops::update_shared_env(&state.db, id, &columns, state.now())
                        .await?;
                    crate::audit::mutation(
                        &state,
                        &ctx,
                        &meta,
                        "shared_environment_variable",
                        "updated",
                        json!({
                            "team_id": ctx.team_id,
                            "shared_environment_variable_uuid": null,
                            "shared_environment_variable_name": key,
                            "changed_fields": fields,
                        }),
                        Value::Object(audit_changes),
                    )
                    .await;
                    changed = true;
                }
            }
            None => {
                let key = key.trim();
                let cipher = enc.encrypt_string(plain);
                let n = NewShared {
                    team_id: ctx.team_id,
                    key,
                    value_encrypted: Some(&cipher),
                    is_literal: p.setting("is_literal").unwrap_or(false),
                    is_multiline: false,
                    is_shown_once: false,
                    comment: None,
                };
                sv::create(&state.db, res.scope, &n, state.now()).await?;
                audit_created(&state, &ctx, &meta, res.scope, &n).await;
                changed = true;
            }
        }
    }
    Ok(ok(json!({"message": changed.then_some("Environment variables updated.")})).into_response())
}

/// A variable of the session team, as `$this->env`.
async fn find_any(state: &AppState, ctx: &AuthCtx, id: i64) -> Result<Option<Record>, ApiError> {
    let text: Option<String> = sqlx::query_scalar(
        "SELECT row_to_json(e)::text FROM shared_environment_variables e WHERE team_id = $1 AND id = $2",
    )
    .bind(ctx.team_id)
    .bind(id)
    .fetch_optional(state.db.pool())
    .await
    .map_err(|_| ApiError::Internal)?;
    text.map(|t| coolify_db::records::parse_row(&t).map_err(|_| ApiError::Internal))
        .transpose()
}

/// `loadValues` / `copyValue`.
async fn value(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(id): Path<i64>,
) -> ApiResult {
    let Some(r) = find_any(&state, &ctx, id).await? else {
        return Ok(toast_error(StatusCode::NOT_FOUND, "Not found."));
    };
    if !ctx.is_team_admin() {
        return Ok(unauthorized());
    }
    let value = if flag(&r, "is_shown_once") {
        None
    } else {
        decrypt(&state, &r)?
    };
    Ok(ok(json!({"value": value})).into_response())
}

/// `Show::submit`.
async fn update(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path(id): Path<i64>,
    Json(body): Json<Value>,
) -> ApiResult {
    let Some(current) = find_any(&state, &ctx, id).await? else {
        return Ok(toast_error(StatusCode::NOT_FOUND, "Not found."));
    };
    if !ctx.is_team_admin() {
        return Ok(unauthorized());
    }
    let key = body
        .get("key")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_owned();
    let comment = body
        .get("comment")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let errors = validate_fields(&key, comment.as_deref())?;
    if !errors.is_empty() {
        return Ok(field_errors(errors));
    }
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let locked = flag(&current, "is_shown_once");
    let mut changes: Vec<(&'static str, C)> = Vec::new();
    let mut audit_changes = Map::new();
    let mut changed: Vec<&'static str> = Vec::new();
    let old_key = text(&current, "key").unwrap_or_default().to_owned();
    if key != old_key {
        audit_changes.insert("key".into(), json!({"old": old_key, "new": key}));
        changed.push("key");
        changes.push(("key", C::Text(Some(key.clone()))));
    }
    // The form never holds a locked value, so keep the stored one and keep the lock.
    if !locked {
        let new = body.get("value").and_then(Value::as_str);
        if new != decrypt(&state, &current)?.as_deref() {
            changed.push("value");
            changes.push(("value", C::Text(new.map(|v| enc.encrypt_string(v)))));
        }
    }
    let multiline = body
        .get("is_multiline")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if multiline != flag(&current, "is_multiline") {
        audit_changes.insert(
            "is_multiline".into(),
            json!({"old": flag(&current, "is_multiline"), "new": multiline}),
        );
        changed.push("is_multiline");
        changes.push(("is_multiline", C::Bool(multiline)));
    }
    if text(&current, "comment") != comment.as_deref() {
        audit_changes.insert(
            "comment".into(),
            json!({"old": current.get("comment"), "new": comment}),
        );
        changed.push("comment");
        changes.push(("comment", C::Text(comment.clone())));
    }
    if !changes.is_empty() {
        coolify_db::server_ops::update_shared_env(&state.db, id, &changes, state.now()).await?;
        crate::audit::mutation(
            &state,
            &ctx,
            &meta,
            "shared_environment_variable",
            "updated",
            json!({
                "team_id": ctx.team_id,
                "shared_environment_variable_uuid": null,
                "shared_environment_variable_name": key,
                "changed_fields": changed,
            }),
            Value::Object(audit_changes),
        )
        .await;
    }
    Ok(ok(json!({"message": "Environment variable updated."})).into_response())
}

/// `Show::lock`.
async fn lock(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path(id): Path<i64>,
) -> ApiResult {
    let Some(current) = find_any(&state, &ctx, id).await? else {
        return Ok(toast_error(StatusCode::NOT_FOUND, "Not found."));
    };
    if !ctx.is_team_admin() {
        return Ok(unauthorized());
    }
    coolify_db::server_ops::update_shared_env(
        &state.db,
        id,
        &[("is_shown_once", C::Bool(true))],
        state.now(),
    )
    .await?;
    crate::audit::mutation(
        &state,
        &ctx,
        &meta,
        "shared_environment_variable",
        "updated",
        json!({
            "team_id": ctx.team_id,
            "shared_environment_variable_uuid": null,
            "shared_environment_variable_name": current.get("key"),
            "changed_fields": ["is_shown_once"],
        }),
        json!({"is_shown_once": {"old": false, "new": true}}),
    )
    .await;
    Ok(ok(json!({})).into_response())
}

/// `Show::delete`.
async fn destroy(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path(id): Path<i64>,
) -> ApiResult {
    let Some(current) = find_any(&state, &ctx, id).await? else {
        return Ok(toast_error(StatusCode::NOT_FOUND, "Not found."));
    };
    if !ctx.is_team_admin() {
        return Ok(unauthorized());
    }
    coolify_db::server_ops::delete_shared_env(&state.db, id).await?;
    let key = current.get("key").cloned().unwrap_or(Value::Null);
    crate::audit::mutation(
        &state,
        &ctx,
        &meta,
        "shared_environment_variable",
        "deleted",
        json!({
            "team_id": ctx.team_id,
            "shared_environment_variable_uuid": null,
            "shared_environment_variable_name": key,
            "changed_fields": [],
        }),
        json!({
            "key": {"old": key, "new": null},
            "is_literal": {"old": current.get("is_literal"), "new": null},
            "is_multiline": {"old": current.get("is_multiline"), "new": null},
            "is_shown_once": {"old": current.get("is_shown_once"), "new": null},
            "comment": {"old": current.get("comment"), "new": null},
            "type": {"old": current.get("type"), "new": null},
        }),
    )
    .await;
    Ok(ok(json!({"message": "Environment variable deleted successfully."})).into_response())
}

/// The list pages: projects with their environments, and servers.
async fn owners(State(state): State<AppState>, Extension(ctx): Extension<AuthCtx>) -> ApiResult {
    let projects: Vec<String> = sqlx::query_scalar(
        "SELECT json_build_object('uuid', p.uuid, 'name', p.name, 'description', p.description, \
           'variables', (SELECT count(*) FROM shared_environment_variables v WHERE v.project_id = p.id AND v.type = 'project'), \
           'environments', COALESCE((SELECT json_agg(json_build_object('uuid', e.uuid, 'name', e.name, 'description', e.description)) FROM environments e WHERE e.project_id = p.id), '[]'::json))::text \
         FROM projects p WHERE p.team_id = $1 ORDER BY lower(p.name)",
    )
    .bind(ctx.team_id)
    .fetch_all(state.db.pool())
    .await
    .map_err(|_| ApiError::Internal)?;
    let projects: Vec<Value> = projects
        .iter()
        .filter_map(|t| serde_json::from_str(t).ok())
        .collect();
    let rows = coolify_db::servers::list(&state.db, ctx.team_id).await?;
    let servers: Vec<Value> = rows
        .iter()
        .map(|s| {
            let settings = s.settings.clone().unwrap_or_default();
            let on = |k: &str| flag(&settings, k);
            // `Server::isFunctional()`.
            let transferred = s
                .record
                .get("server_metadata")
                .and_then(|m| m.pointer("/transfer/status"))
                .and_then(Value::as_str)
                == Some("transferred");
            json!({
                "uuid": s.uuid,
                "name": s.record.get("name"),
                "description": s.record.get("description"),
                "ip": s.record.get("ip"),
                "functional": on("is_reachable") && on("is_usable") && !on("force_disabled") && !transferred,
            })
        })
        .collect();
    Ok(ok(json!({"projects": projects, "servers": servers})).into_response())
}

/// `SharedVariableCanvas::overview`.
async fn overview(State(state): State<AppState>, Extension(ctx): Extension<AuthCtx>) -> ApiResult {
    let team = ctx.team_id;
    let vars: Vec<String> = sqlx::query_scalar(
        "SELECT json_build_object('id', v.id, 'key', v.key, 'type', v.type, 'team_id', v.team_id, \
           'project_id', v.project_id, 'environment_id', v.environment_id, 'server_id', v.server_id, 'updated_at', v.updated_at, \
           'project', p.name, 'project_uuid', p.uuid, 'environment', e.name, 'environment_uuid', e.uuid, \
           'environment_project', ep.name, 'environment_project_uuid', ep.uuid, 'server', s.name, 'server_uuid', s.uuid)::text \
         FROM shared_environment_variables v \
         LEFT JOIN projects p ON p.id = v.project_id \
         LEFT JOIN environments e ON e.id = v.environment_id \
         LEFT JOIN projects ep ON ep.id = e.project_id \
         LEFT JOIN servers s ON s.id = v.server_id \
         WHERE v.team_id = $1 AND NOT (v.type = 'server' AND v.key IN ('COOLIFY_SERVER_UUID', 'COOLIFY_SERVER_NAME'))",
    )
    .bind(team)
    .fetch_all(state.db.pool())
    .await
    .map_err(|_| ApiError::Internal)?;
    let vars: Vec<Value> = vars
        .iter()
        .filter_map(|t| serde_json::from_str(t).ok())
        .collect();
    let s = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).map(str::to_owned);
    let n = |v: &Value, k: &str| v.get(k).and_then(Value::as_i64);

    let mut scopes = Map::new();
    for scope in ["team", "project", "environment", "server"] {
        let of: Vec<&Value> = vars
            .iter()
            .filter(|v| v.get("type").and_then(Value::as_str) == Some(scope))
            .collect();
        let column = if scope == "team" {
            "team_id".to_owned()
        } else {
            format!("{scope}_id")
        };
        let owners: HashSet<Option<i64>> = of.iter().map(|v| n(v, &column)).collect();
        scopes.insert(
            scope.into(),
            json!({"variables": of.len(), "owners": owners.len()}),
        );
    }

    let mut id_by_reference: HashMap<String, i64> = HashMap::new();
    for v in &vars {
        let kind = s(v, "type").unwrap_or_default();
        let owner = if kind == "team" {
            n(v, "team_id")
        } else {
            n(v, &format!("{kind}_id"))
        };
        id_by_reference.insert(
            format!(
                "{kind}|{}|{}",
                owner.unwrap_or_default(),
                s(v, "key").unwrap_or_default()
            ),
            n(v, "id").unwrap_or_default(),
        );
    }

    // Every resource variable of the team with the owners its references resolve against.
    let used_rows = coolify_db::tag_pages::resource_variables(&state.db, team).await?;
    let enc = state.encrypter.as_ref();
    let pattern = Regex::new(r"\{\{\s*(team|project|environment|server)\.(.+?)\s*\}\}")
        .map_err(|_| ApiError::Internal)?;
    let mut used_by: HashMap<i64, HashSet<String>> = HashMap::new();
    let mut missing: HashSet<String> = HashSet::new();
    for row in used_rows {
        let plain = row
            .value
            .as_deref()
            .and_then(|c| enc.and_then(|e| e.decrypt_string(c).ok()));
        let Some(plain) = plain else { continue };
        for cap in pattern.captures_iter(&plain) {
            let owner = match &cap[1] {
                "team" => Some(team),
                "project" => Some(row.project_id),
                "environment" => Some(row.environment_id),
                _ => row.server_id,
            };
            let reference = format!(
                "{}|{}|{}",
                &cap[1],
                owner.map_or(String::new(), |o| o.to_string()),
                &cap[2]
            );
            if let Some(id) = id_by_reference.get(&reference) {
                used_by
                    .entry(*id)
                    .or_default()
                    .insert(row.resource_uuid.clone());
            } else {
                missing.insert(reference);
            }
        }
    }

    let mut rows: Vec<Value> = vars
        .iter()
        .map(|v| {
            let kind = s(v, "type").unwrap_or_default();
            let id = n(v, "id").unwrap_or_default();
            let select = format!("?select=variable%3A{id}");
            let (owner, link) = match kind.as_str() {
                "team" => (
                    Some("Team".to_owned()),
                    format!("/shared-variables/team/canvas{select}"),
                ),
                "project" => (
                    s(v, "project"),
                    format!(
                        "/shared-variables/project/{}/canvas{select}",
                        s(v, "project_uuid").unwrap_or_default()
                    ),
                ),
                "environment" => (
                    Some(format!(
                        "{} / {}",
                        s(v, "environment_project").unwrap_or_default(),
                        s(v, "environment").unwrap_or_default()
                    )),
                    format!(
                        "/shared-variables/environments/project/{}/environment/{}/canvas{select}",
                        s(v, "environment_project_uuid").unwrap_or_default(),
                        s(v, "environment_uuid").unwrap_or_default()
                    ),
                ),
                _ => (
                    s(v, "server"),
                    format!(
                        "/shared-variables/server/{}/canvas{select}",
                        s(v, "server_uuid").unwrap_or_default()
                    ),
                ),
            };
            json!({
                "key": v.get("key"),
                "scope": kind,
                "owner": owner,
                "link": link,
                "resources": used_by.get(&id).map_or(0, HashSet::len),
                "updated_at": v.get("updated_at"),
            })
        })
        .collect();
    let resources = |v: &Value| v.get("resources").and_then(Value::as_u64).unwrap_or(0);
    let updated = |v: &Value| {
        v.get("updated_at")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    rows.sort_by_key(|v| std::cmp::Reverse(updated(v)));
    let recent: Vec<Value> = rows.iter().take(6).cloned().collect();
    let mut used: Vec<Value> = rows.iter().filter(|v| resources(v) > 0).cloned().collect();
    used.sort_by_key(|v| std::cmp::Reverse(resources(v)));
    used.truncate(6);
    let mut unused: Vec<Value> = rows.iter().filter(|v| resources(v) == 0).cloned().collect();
    unused.sort_by_key(|v| s(v, "key"));
    Ok(ok(json!({
        "scopes": scopes,
        "recent": recent,
        "used": used,
        "unused": unused,
        "missing": missing.len(),
    }))
    .into_response())
}
