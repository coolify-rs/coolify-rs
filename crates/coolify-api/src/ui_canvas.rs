//! The `/canvas/api` routes of `routes/web.php`: `EnvironmentCanvasController`,
//! `ProjectCanvasController`, `ServerCanvasController`, `SharedVariableCanvasController` and
//! `TagCanvasController`, called by the canvases in `frontend/src/upstream/js/canvas` with the
//! browser session. Members hold the `read` ability only, so `$this->authorize('update', ...)`
//! after `api.ability:write` never refuses here, and `canEdit` is the same as being an admin.
//! Also the page shells of `App\Livewire\DeployCanvas` and the other canvas components.

mod connect;
pub mod graph;

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::middleware::from_fn;
use axum::middleware::from_fn_with_state;
use axum::response::{IntoResponse, Response};
use axum::routing::{MethodRouter, delete, get, patch, post};
use axum::{Extension, Router};
use coolify_db::canvas as db;
use coolify_db::records::Record;
use coolify_domain::Ability;
use coolify_jobs::server::ServerInfo;
use coolify_jobs::server_ops::Srv;
use serde_json::{Map, Value, json};

use crate::auth::AuthCtx;
use crate::input::{Input, validated_json};
use crate::response::{ApiError, ApiResult, PhpJson, ok};
use crate::state::AppState;
use crate::ui_server_metrics as metrics;
use graph::{int, text};

const MAX_POSITIONS: usize = 1000;
const MAX_COORDINATE: i64 = 100_000;

/// The routes of the `canvas/api` group served by existing API controllers.
const BRIDGED: &[(&str, &str)] = &[
    ("GET", "projects"),
    ("POST", "projects"),
    ("GET", "projects/{uuid}/environments"),
    ("POST", "projects/{uuid}/environments"),
    ("GET", "servers"),
    ("GET", "servers/{server_uuid}/destinations"),
    ("GET", "github-apps"),
    ("GET", "github-apps/{github_app_id}/repositories"),
    (
        "GET",
        "github-apps/{github_app_id}/repositories/{owner}/{repo}/branches",
    ),
    ("GET", "security/keys"),
    ("POST", "applications/public"),
    ("POST", "applications/private-github-app"),
    ("POST", "applications/private-deploy-key"),
    ("POST", "applications/dockerfile"),
    ("POST", "applications/dockerimage"),
    ("GET", "applications/{uuid}"),
    ("PATCH", "applications/{uuid}"),
    ("PATCH", "applications/{uuid}/envs/bulk"),
    ("POST", "services"),
    ("PATCH", "services/{uuid}/envs/bulk"),
    ("POST", "databases/postgresql"),
    ("POST", "databases/mysql"),
    ("POST", "databases/mariadb"),
    ("POST", "databases/mongodb"),
    ("POST", "databases/redis"),
    ("POST", "databases/clickhouse"),
    ("POST", "databases/dragonfly"),
    ("POST", "databases/keydb"),
    ("POST", "deploy"),
    ("GET", "deployments/{uuid}"),
];

/// The `/canvas/api` path of an API route, when the canvas group has it.
pub fn bridge_path(method: &str, uri: &str) -> Option<String> {
    let rest = uri.strip_prefix("api/v1/")?;
    BRIDGED
        .iter()
        .any(|(m, u)| *m == method && *u == rest)
        .then(|| format!("canvas/api/{rest}"))
}

fn with_ability(
    ability: &'static [&'static [Ability]],
    router: MethodRouter<AppState>,
) -> MethodRouter<AppState> {
    router.route_layer(from_fn(move |req, next| {
        crate::auth::require_abilities(ability, req, next)
    }))
}

const READ: &[&[Ability]] = &[&[Ability::Read]];
const WRITE: &[&[Ability]] = &[&[Ability::Write]];

pub fn routes(state: AppState) -> Router<AppState> {
    let r = |m: MethodRouter<AppState>| with_ability(READ, m);
    let w = |m: MethodRouter<AppState>| with_ability(WRITE, m);
    let canvas = Router::new()
        .route("/canvas/api/projects/canvas", r(get(projects_canvas)))
        .route(
            "/canvas/api/projects/canvas/positions",
            w(patch(project_positions)),
        )
        .route("/canvas/api/projects/{uuid}/canvas", r(get(project_canvas)))
        .route(
            "/canvas/api/projects/{uuid}/environments/{environment}/canvas",
            r(get(environment_canvas)),
        )
        .route(
            "/canvas/api/projects/{uuid}/environments/{environment}/canvas/positions",
            w(patch(environment_positions)),
        )
        .route(
            "/canvas/api/projects/{uuid}/environments/{environment}/canvas/links",
            w(post(store_link)),
        )
        .route(
            "/canvas/api/projects/{uuid}/environments/{environment}/canvas/links/{link_uuid}",
            w(patch(update_link)).merge(w(delete(destroy_link))),
        )
        .route("/canvas/api/tags/canvas", r(get(connect::tags_canvas)))
        .route(
            "/canvas/api/tags/canvas/links",
            w(post(connect::tags_store_link)),
        )
        .route(
            "/canvas/api/tags/canvas/links/{link_id}",
            w(delete(connect::tags_destroy_link)),
        )
        .route(
            "/canvas/api/shared-variables/team/canvas",
            r(get(connect::team_show)),
        )
        .route(
            "/canvas/api/shared-variables/team/canvas/links",
            w(post(connect::team_store_link)),
        )
        .route(
            "/canvas/api/shared-variables/team/canvas/links/{link_id}",
            w(delete(connect::team_destroy_link)),
        )
        .route(
            "/canvas/api/shared-variables/{scope}/{uuid}/canvas",
            r(get(connect::show)),
        )
        .route(
            "/canvas/api/shared-variables/{scope}/{uuid}/canvas/links",
            w(post(connect::store_link)),
        )
        .route(
            "/canvas/api/shared-variables/{scope}/{uuid}/canvas/links/{link_id}",
            w(delete(connect::destroy_link)),
        )
        .route("/canvas/api/servers/canvas", r(get(servers_canvas)))
        .route(
            "/canvas/api/servers/canvas/positions",
            w(patch(server_positions)),
        )
        .route("/canvas/api/dashboard/canvas", r(get(dashboard_canvas)))
        .route(
            "/canvas/api/dashboard/canvas/positions",
            w(patch(server_positions)),
        )
        .route("/canvas/api/servers/{uuid}/canvas", r(get(server_canvas)))
        .route(
            "/canvas/api/servers/{uuid}/canvas/containers",
            r(get(server_containers)),
        )
        .route(
            "/canvas/api/servers/{uuid}/canvas/usage",
            r(get(server_usage)),
        )
        .route_layer(from_fn_with_state(
            state.clone(),
            crate::session::canvas_guard,
        ));
    let shells = Router::new()
        .route("/ui/canvas/shell", get(shell))
        .route("/ui/canvas/deploy", get(deploy_shell))
        .route(
            "/ui/projects/{uuid}/icon",
            post(upload_project_icon).delete(remove_project_icon),
        )
        .route("/project/{uuid}/icon", get(project_icon))
        .route_layer(from_fn_with_state(state, crate::session::ui_guard));
    canvas.merge(shells)
}

/// `url('/')` of the request.
pub(crate) fn base(headers: &HeaderMap) -> String {
    let host = headers
        .get(axum::http::header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("localhost");
    let scheme = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("http");
    format!("{scheme}://{host}")
}

fn message(status: StatusCode, m: &str) -> Response {
    PhpJson(status, json!({"message": m})).into_response()
}

fn validation(errors: Map<String, Value>) -> Response {
    PhpJson(
        StatusCode::UNPROCESSABLE_ENTITY,
        json!({"message": "Validation failed.", "errors": errors}),
    )
    .into_response()
}

fn add_error(errors: &mut Map<String, Value>, key: &str, m: String) {
    if let Some(Value::Array(list)) = errors.get_mut(key) {
        list.push(json!(m));
    } else {
        errors.insert(key.to_owned(), json!([m]));
    }
}

/// `Rule required`: null, blank strings and empty arrays fail.
fn is_empty_value(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => true,
        Some(Value::String(s)) => s.trim().is_empty(),
        Some(Value::Array(a)) => a.is_empty(),
        Some(Value::Object(m)) => m.is_empty(),
        Some(_) => false,
    }
}

/// `filter_var($value, FILTER_VALIDATE_INT)`.
fn php_int(v: &Value) -> Option<i64> {
    match v {
        Value::Bool(true) => Some(1),
        Value::Number(n) => n.as_i64().or_else(|| {
            n.as_f64()
                .filter(|f| f.fract() == 0.0 && f.abs() < 1e15)
                .map(|f| f as i64)
        }),
        Value::String(s) => {
            let t = s.trim();
            let digits = t.strip_prefix(['-', '+']).unwrap_or(t);
            let clean = !digits.is_empty()
                && digits.bytes().all(|b| b.is_ascii_digit())
                && (digits == "0" || !digits.starts_with('0'));
            if clean { t.parse().ok() } else { None }
        }
        _ => None,
    }
}

/// `['required', 'integer', 'between:-100000,100000']`.
fn check_coordinate(errors: &mut Map<String, Value>, attribute: &str, v: Option<&Value>) {
    if is_empty_value(v) {
        add_error(
            errors,
            attribute,
            format!("The {attribute} field is required."),
        );
        return;
    }
    let v = v.unwrap_or(&Value::Null);
    let int = php_int(v);
    if int.is_none() {
        add_error(
            errors,
            attribute,
            format!("The {attribute} field must be an integer."),
        );
    }
    let within = match (int, v) {
        (Some(i), _) => i.abs() <= MAX_COORDINATE,
        (None, Value::Number(n)) => n.as_f64().is_some_and(|f| f.abs() <= MAX_COORDINATE as f64),
        (None, Value::String(s)) => s
            .trim()
            .parse::<f64>()
            .map_or(true, |f| f.abs() <= MAX_COORDINATE as f64),
        _ => true,
    };
    if !within {
        add_error(
            errors,
            attribute,
            format!(
                "The {attribute} field must be between -{MAX_COORDINATE} and {MAX_COORDINATE}."
            ),
        );
    }
}

fn entries(v: &Value) -> Vec<(String, &Value)> {
    match v {
        Value::Object(m) => m.iter().map(|(k, v)| (k.clone(), v)).collect(),
        Value::Array(a) => a
            .iter()
            .enumerate()
            .map(|(i, v)| (i.to_string(), v))
            .collect(),
        _ => Vec::new(),
    }
}

/// The wildcard rules `positions.*`, `positions.*.x` and `positions.*.y`, in Laravel's order.
fn check_positions(errors: &mut Map<String, Value>, items: &[(String, &Value)]) {
    let is_list = |v: &Value| matches!(v, Value::Array(_) | Value::Object(_));
    for (key, item) in items {
        let attribute = format!("positions.{key}");
        if is_empty_value(Some(item)) {
            add_error(
                errors,
                &attribute,
                format!("The {attribute} field is required."),
            );
        } else if !is_list(item) {
            add_error(
                errors,
                &attribute,
                format!("The {attribute} field must be an array."),
            );
        }
    }
    for axis in ["x", "y"] {
        for (key, item) in items {
            check_coordinate(errors, &format!("positions.{key}.{axis}"), item.get(axis));
        }
    }
}

fn input(headers: &HeaderMap, uri: &Uri, body: &[u8]) -> Result<Input, ApiError> {
    validated_json(headers, uri, body)
}

/// `updatePositions` of the project and server canvas controllers.
async fn zone_positions(
    state: &AppState,
    headers: &HeaderMap,
    uri: &Uri,
    body: &[u8],
    lookup: impl AsyncFnOnce(&[String]) -> Result<Vec<Record>, ApiError>,
    table: &'static str,
) -> ApiResult {
    let data = input(headers, uri, body)?;
    let mut errors = Map::new();
    let positions = data.get("positions");
    if is_empty_value(positions) {
        add_error(
            &mut errors,
            "positions",
            "The positions field is required.".into(),
        );
    } else if !matches!(positions, Some(Value::Array(_) | Value::Object(_))) {
        add_error(
            &mut errors,
            "positions",
            "The positions field must be an array.".into(),
        );
    }
    let items = positions.map(entries).unwrap_or_default();
    if items.len() > MAX_POSITIONS {
        add_error(
            &mut errors,
            "positions",
            format!("The positions field must not have more than {MAX_POSITIONS} items."),
        );
    }
    check_positions(&mut errors, &items);
    if !errors.is_empty() {
        return Ok(validation(errors));
    }
    let keys: Vec<String> = items.iter().map(|(k, _)| k.clone()).collect();
    let rows = lookup(&keys).await?;
    let now = state.now();
    let mut saved = Map::new();
    for row in rows {
        let uuid = text(&row, "uuid").to_owned();
        let Some((_, item)) = items.iter().find(|(k, _)| *k == uuid) else {
            continue;
        };
        let position = json!({
            "x": item.get("x").and_then(php_int).unwrap_or(0),
            "y": item.get("y").and_then(php_int).unwrap_or(0),
        });
        db::save_zone_position(&state.db, table, int(&row, "id"), &position, now).await?;
        saved.insert(uuid, position);
    }
    Ok(ok(json!({"positions": saved})).into_response())
}

async fn projects_canvas(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    headers: HeaderMap,
) -> ApiResult {
    let p = graph::projects_payload(
        &state,
        ctx.team_id,
        ctx.can_read_sensitive,
        ctx.is_team_admin(),
        &base(&headers),
    )
    .await?;
    Ok(ok(p).into_response())
}

async fn project_canvas(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    headers: HeaderMap,
    Path(uuid): Path<String>,
) -> ApiResult {
    let Some(project) = db::project_by_uuid(&state.db, ctx.team_id, &uuid).await? else {
        return Ok(message(StatusCode::NOT_FOUND, "Project not found."));
    };
    let p = graph::project_payload(
        &state,
        &project,
        ctx.can_read_sensitive,
        ctx.is_team_admin(),
        &base(&headers),
    )
    .await?;
    Ok(ok(p).into_response())
}

async fn project_positions(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let team = ctx.team_id;
    let projects = db::team_projects(&state.db, team).await?;
    zone_positions(
        &state,
        &headers,
        &uri,
        &body,
        async |keys| {
            Ok(projects
                .into_iter()
                .filter(|p| keys.iter().any(|k| k == text(p, "uuid")))
                .collect())
        },
        "projects",
    )
    .await
}

async fn server_positions(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let servers = db::team_servers(&state.db, ctx.team_id).await?;
    zone_positions(
        &state,
        &headers,
        &uri,
        &body,
        async |keys| {
            Ok(servers
                .into_iter()
                .filter(|s| keys.iter().any(|k| k == text(s, "uuid")))
                .collect())
        },
        "servers",
    )
    .await
}

/// `EnvironmentCanvasController::findEnvironment`.
async fn find_environment(
    state: &AppState,
    ctx: &AuthCtx,
    uuid: &str,
    environment: &str,
) -> Result<Result<(Record, Record), Response>, ApiError> {
    let Some(project) = db::project_by_uuid(&state.db, ctx.team_id, uuid).await? else {
        return Ok(Err(message(StatusCode::NOT_FOUND, "Project not found.")));
    };
    let Some(env) = db::find_environment(&state.db, int(&project, "id"), environment).await? else {
        return Ok(Err(message(
            StatusCode::NOT_FOUND,
            "Environment not found.",
        )));
    };
    Ok(Ok((project, env)))
}

async fn environment_canvas(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    headers: HeaderMap,
    Path((uuid, environment)): Path<(String, String)>,
) -> ApiResult {
    let (project, env) = match find_environment(&state, &ctx, &uuid, &environment).await? {
        Ok(found) => found,
        Err(r) => return Ok(r),
    };
    let p = graph::environment_payload(
        &state,
        &project,
        &env,
        ctx.can_read_sensitive,
        ctx.is_team_admin(),
        &base(&headers),
    )
    .await?;
    Ok(ok(p).into_response())
}

/// `validationFailedResponse`: the validator errors, then the fields that are not allowed.
fn with_extra_fields(
    mut errors: Map<String, Value>,
    data: &Input,
    allowed: &[&str],
) -> Option<Response> {
    for key in data.keys() {
        if !allowed.contains(&key.as_str()) {
            add_error(&mut errors, key, "This field is not allowed.".into());
        }
    }
    (!errors.is_empty()).then(|| validation(errors))
}

/// `EnvironmentCanvas::resourceIndex`: morph type and id per uuid.
async fn resource_index(
    state: &AppState,
    env: &Record,
) -> Result<HashMap<String, (&'static str, i64)>, ApiError> {
    let list = db::resources(&state.db, &[int(env, "id")]).await?;
    Ok(list
        .iter()
        .map(|r| (r.uuid().to_owned(), (r.morph, r.id())))
        .collect())
}

async fn environment_positions(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    headers: HeaderMap,
    uri: Uri,
    Path((uuid, environment)): Path<(String, String)>,
    body: Bytes,
) -> ApiResult {
    let (_, env) = match find_environment(&state, &ctx, &uuid, &environment).await? {
        Ok(found) => found,
        Err(r) => return Ok(r),
    };
    let data = input(&headers, &uri, &body)?;
    let mut errors = Map::new();
    let positions = data.get("positions");
    match positions {
        None => add_error(
            &mut errors,
            "positions",
            "The positions field must be present.".into(),
        ),
        Some(Value::Array(_) | Value::Object(_)) => {}
        Some(_) => add_error(
            &mut errors,
            "positions",
            "The positions field must be an array.".into(),
        ),
    }
    let items = positions.map(entries).unwrap_or_default();
    if items.len() > MAX_POSITIONS {
        add_error(
            &mut errors,
            "positions",
            format!("The positions field must not have more than {MAX_POSITIONS} items."),
        );
    }
    if let Some(r) = with_extra_fields(errors, &data, &["positions"]) {
        return Ok(r);
    }
    let index = resource_index(&state, &env).await?;
    let known: Vec<(String, &Value)> = items
        .into_iter()
        .filter(|(k, _)| index.contains_key(k))
        .collect();
    let mut errors = Map::new();
    check_positions(&mut errors, &known);
    if !errors.is_empty() {
        return Ok(validation(errors));
    }
    let uuids: std::collections::HashSet<&str> = index.keys().map(String::as_str).collect();
    let saved = db::merge_positions(
        &state.db,
        int(&env, "id"),
        |current| {
            let mut merged = match current {
                Some(Value::Object(m)) => m,
                _ => Map::new(),
            };
            for (k, v) in &known {
                merged.insert(k.clone(), (*v).clone());
            }
            graph::php_map(graph::filter_positions(
                Some(&Value::Object(merged)),
                &uuids,
            ))
        },
        state.now(),
    )
    .await?;
    let saved = match saved {
        Value::Object(m) => m,
        _ => Map::new(),
    };
    Ok(ok(json!({"positions": saved})).into_response())
}

/// `['nullable', 'string', 'max:64']`, with `present` when `present`.
fn check_label(errors: &mut Map<String, Value>, data: &Input, present: bool) {
    match data.get("label") {
        None if present => add_error(errors, "label", "The label field must be present.".into()),
        None | Some(Value::Null) => {}
        Some(Value::String(s)) if s.chars().count() > 64 => add_error(
            errors,
            "label",
            "The label field must not be greater than 64 characters.".into(),
        ),
        Some(Value::String(_)) => {}
        Some(_) => add_error(errors, "label", "The label field must be a string.".into()),
    }
}

/// `['required', 'string', 'max:255']`.
pub(super) fn check_end(errors: &mut Map<String, Value>, data: &Input, field: &str) -> bool {
    let v = data.get(field);
    if is_empty_value(v) {
        add_error(errors, field, format!("The {field} field is required."));
        return false;
    }
    match v {
        Some(Value::String(s)) if s.chars().count() > 255 => {
            add_error(
                errors,
                field,
                format!("The {field} field must not be greater than 255 characters."),
            );
            false
        }
        Some(Value::String(_)) => true,
        _ => {
            add_error(
                errors,
                field,
                format!("The {field} field must be a string."),
            );
            false
        }
    }
}

/// `normalizeLabel`.
fn normalize_label(data: &Input) -> Option<String> {
    let label = match data.get("label") {
        Some(Value::String(s)) => s.trim().to_owned(),
        _ => String::new(),
    };
    (!label.is_empty()).then_some(label)
}

async fn store_link(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    headers: HeaderMap,
    uri: Uri,
    Path((uuid, environment)): Path<(String, String)>,
    body: Bytes,
) -> ApiResult {
    let (_, env) = match find_environment(&state, &ctx, &uuid, &environment).await? {
        Ok(found) => found,
        Err(r) => return Ok(r),
    };
    let data = input(&headers, &uri, &body)?;
    let mut errors = Map::new();
    check_end(&mut errors, &data, "source");
    if check_end(&mut errors, &data, "target") && data.get("target") == data.get("source") {
        add_error(
            &mut errors,
            "target",
            "The target field and source must be different.".into(),
        );
    }
    check_label(&mut errors, &data, false);
    if let Some(r) = with_extra_fields(errors, &data, &["source", "target", "label"]) {
        return Ok(r);
    }
    let index = resource_index(&state, &env).await?;
    let source_uuid = data.str("source").unwrap_or_default();
    let target_uuid = data.str("target").unwrap_or_default();
    let source = index.get(source_uuid);
    let target = index.get(target_uuid);
    let mut errors = Map::new();
    if source.is_none() {
        add_error(
            &mut errors,
            "source",
            "The source must be a resource of this environment.".into(),
        );
    }
    if target.is_none() {
        add_error(
            &mut errors,
            "target",
            "The target must be a resource of this environment.".into(),
        );
    }
    let (Some(source), Some(target)) = (source, target) else {
        return Ok(validation(errors));
    };
    let ends = db::LinkEnds {
        environment_id: int(&env, "id"),
        source_type: source.0,
        source_id: source.1,
        target_type: target.0,
        target_id: target.1,
    };
    let duplicate = || {
        let mut errors = Map::new();
        add_error(
            &mut errors,
            "target",
            "These resources are already linked.".into(),
        );
        validation(errors)
    };
    if db::link_exists(&state.db, &ends).await? {
        return Ok(duplicate());
    }
    let label = normalize_label(&data);
    let Some(link_uuid) =
        db::create_link(&state.db, &ends, label.as_deref(), ctx.user_id, state.now()).await?
    else {
        return Ok(duplicate());
    };
    Ok(PhpJson(
        StatusCode::CREATED,
        json!({"id": link_uuid, "source": source_uuid, "target": target_uuid, "label": label}),
    )
    .into_response())
}

async fn update_link(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    headers: HeaderMap,
    uri: Uri,
    Path((uuid, environment, link_uuid)): Path<(String, String, String)>,
    body: Bytes,
) -> ApiResult {
    let (_, env) = match find_environment(&state, &ctx, &uuid, &environment).await? {
        Ok(found) => found,
        Err(r) => return Ok(r),
    };
    let data = input(&headers, &uri, &body)?;
    let mut errors = Map::new();
    check_label(&mut errors, &data, true);
    if let Some(r) = with_extra_fields(errors, &data, &["label"]) {
        return Ok(r);
    }
    let Some(link) = db::find_link(&state.db, int(&env, "id"), &link_uuid).await? else {
        return Ok(message(StatusCode::NOT_FOUND, "Link not found."));
    };
    let index = resource_index(&state, &env).await?;
    let uuid_of = |t: &str, id: i64| {
        index
            .iter()
            .find(|(_, (morph, rid))| *morph == t && *rid == id)
            .map(|(u, _)| u.clone())
    };
    let source = uuid_of(text(&link, "source_type"), int(&link, "source_id"));
    let target = uuid_of(text(&link, "target_type"), int(&link, "target_id"));
    let (Some(source), Some(target)) = (source, target) else {
        return Ok(message(StatusCode::NOT_FOUND, "Link not found."));
    };
    let label = normalize_label(&data);
    db::update_link_label(&state.db, int(&link, "id"), label.as_deref(), state.now()).await?;
    Ok(
        ok(json!({"id": link_uuid, "source": source, "target": target, "label": label}))
            .into_response(),
    )
}

async fn destroy_link(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((uuid, environment, link_uuid)): Path<(String, String, String)>,
) -> ApiResult {
    let (_, env) = match find_environment(&state, &ctx, &uuid, &environment).await? {
        Ok(found) => found,
        Err(r) => return Ok(r),
    };
    let Some(link) = db::find_link(&state.db, int(&env, "id"), &link_uuid).await? else {
        return Ok(message(StatusCode::NOT_FOUND, "Link not found."));
    };
    db::delete_link(&state.db, int(&link, "id")).await?;
    Ok(ok(json!({"message": "Link deleted."})).into_response())
}

async fn servers_canvas(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    headers: HeaderMap,
) -> ApiResult {
    let servers = db::team_servers(&state.db, ctx.team_id).await?;
    let p = graph::server_payload(
        &state,
        ctx.team_id,
        &servers,
        false,
        ctx.can_read_sensitive,
        ctx.is_team_admin(),
        &base(&headers),
    )
    .await?;
    Ok(ok(p).into_response())
}

async fn dashboard_canvas(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    headers: HeaderMap,
) -> ApiResult {
    let servers = db::team_servers(&state.db, ctx.team_id).await?;
    let p = graph::dashboard_payload(
        &state,
        ctx.team_id,
        &servers,
        ctx.can_read_sensitive,
        ctx.is_team_admin(),
        &base(&headers),
    )
    .await?;
    Ok(ok(p).into_response())
}

async fn server_canvas(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    headers: HeaderMap,
    Path(uuid): Path<String>,
) -> ApiResult {
    let Some(server) = db::server_by_uuid(&state.db, ctx.team_id, &uuid).await? else {
        return Ok(message(StatusCode::NOT_FOUND, "Server not found."));
    };
    let p = graph::server_payload(
        &state,
        ctx.team_id,
        &[server],
        true,
        ctx.can_read_sensitive,
        ctx.is_team_admin(),
        &base(&headers),
    )
    .await?;
    Ok(ok(p).into_response())
}

/// `ServerCanvas::unmanagedContainerNodes`, through `$server->loadUnmanagedContainers()`.
async fn server_containers(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    headers: HeaderMap,
    Path(uuid): Path<String>,
) -> ApiResult {
    let Some(server) = coolify_db::servers::find(&state.db, ctx.team_id, &uuid).await? else {
        return Ok(message(StatusCode::NOT_FOUND, "Server not found."));
    };
    let base = base(&headers);
    let mut nodes: Vec<(String, Value)> = Vec::new();
    if ServerInfo::from_row(&server).is_functional()
        && let Some(jobs) = state.jobs.as_ref()
    {
        let out = jobs
            .instant_remote(
                &server,
                &["docker ps -a --format '{{json .}}'".to_string()],
                true,
                "server.canvas.containers",
            )
            .await
            .ok()
            .flatten()
            .unwrap_or_default();
        let link = format!("{base}/server/{uuid}/resources");
        for line in out.lines() {
            let Ok(c) = serde_json::from_str::<Value>(line.trim()) else {
                continue;
            };
            let field = |k: &str| c.get(k).and_then(Value::as_str).unwrap_or_default();
            if field("Labels").contains("coolify.managed") || field("ID").is_empty() {
                continue;
            }
            let state_text = c.get("State").and_then(Value::as_str).unwrap_or("unknown");
            let status = field("Status");
            let health = if status.contains("(healthy)") {
                Some("healthy")
            } else if status.contains("(unhealthy)") {
                Some("unhealthy")
            } else {
                None
            };
            let joined = match health {
                Some(h) if !state_text.is_empty() => format!("{state_text}:{h}"),
                Some(h) => h.to_owned(),
                None => state_text.to_owned(),
            };
            nodes.push((
                field("Names").to_owned(),
                json!({
                    "id": format!("container:{}", field("ID")),
                    "type": "container",
                    "name": field("Names"),
                    "status": joined,
                    "zone": uuid,
                    "extra_zones": [],
                    "urls": [],
                    "subtitle": field("Image"),
                    "children": [],
                    "link": link,
                    "context": "Unmanaged containers",
                    "context_link": link,
                }),
            ));
        }
    }
    nodes.sort_by(|a, b| natural(&a.0, &b.0));
    Ok(ok(json!({"nodes": nodes.into_iter().map(|(_, n)| n).collect::<Vec<_>>()})).into_response())
}

/// `SORT_NATURAL` for container names.
fn natural(a: &str, b: &str) -> std::cmp::Ordering {
    let (mut x, mut y) = (a.as_bytes(), b.as_bytes());
    loop {
        match (x.first(), y.first()) {
            (None, None) => return std::cmp::Ordering::Equal,
            (None, Some(_)) => return std::cmp::Ordering::Less,
            (Some(_), None) => return std::cmp::Ordering::Greater,
            (Some(p), Some(q)) if p.is_ascii_digit() && q.is_ascii_digit() => {
                let run = |s: &[u8]| s.iter().take_while(|b| b.is_ascii_digit()).count();
                let (n, m) = (run(x), run(y));
                let value = |s: &[u8]| {
                    std::str::from_utf8(s)
                        .ok()
                        .and_then(|t| t.parse::<u128>().ok())
                        .unwrap_or(0)
                };
                let ordering = value(&x[..n]).cmp(&value(&y[..m]));
                if ordering.is_ne() {
                    return ordering;
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

static USAGE_CACHE: LazyLock<Mutex<HashMap<i64, (Instant, Value)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// `ServerCanvas::usage`: the latest Sentinel values, cached for 30 seconds.
async fn server_usage(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let Some(server) = coolify_db::servers::find(&state.db, ctx.team_id, &uuid).await? else {
        return Ok(message(StatusCode::NOT_FOUND, "Server not found."));
    };
    let enabled = server
        .settings
        .as_ref()
        .and_then(|s| s.get("is_metrics_enabled"))
        .is_some_and(crate::serialize::php_truthy);
    if !enabled {
        return Ok(ok(json!({"cpu": null, "memory": null, "disk": null})).into_response());
    }
    if let Ok(cache) = USAGE_CACHE.lock()
        && let Some((at, value)) = cache.get(&server.id)
        && at.elapsed() < Duration::from_secs(30)
    {
        return Ok(ok(value.clone()).into_response());
    }
    let id = server.id;
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    let srv = Srv::from_row(jobs, server);
    // `$server->prefetchMetrics(['cpu', 'memory', 'disk'], 1)`.
    let answers = metrics::prefetch(&srv, &["cpu", "memory", "disk"], None, 1).await;
    let latest = |kind: &str, field: &str, root_only: bool| -> Value {
        let Ok(rows) = metrics::rows(answers.get(kind).and_then(Option::as_ref)) else {
            return Value::Null;
        };
        let rows: Vec<Value> = if root_only && rows.iter().any(|r| r.get("mount").is_some()) {
            rows.into_iter()
                .filter(|r| r.get("mount").and_then(Value::as_str) == Some("/"))
                .collect()
        } else {
            rows
        };
        metrics::map_rows(&rows, field, 1)
            .last()
            .map_or(Value::Null, |(_, v)| json!((v * 10.0).round() / 10.0))
    };
    let usage = json!({
        "cpu": latest("cpu", "percent", false),
        "memory": latest("memory", "usedPercent", false),
        "disk": latest("disk", "usedPercent", true),
    });
    if let Ok(mut cache) = USAGE_CACHE.lock() {
        cache.insert(id, (Instant::now(), usage.clone()));
    }
    Ok(ok(usage).into_response())
}

/// What the canvas page shells need: the team of the page, the user, the owner the page shows and
/// what the user may do, with the 404 of `firstOrFail` in `mount`.
async fn shell(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult {
    let mut out = Map::new();
    out.insert("team_id".into(), json!(ctx.team_id));
    out.insert("user_id".into(), json!(ctx.user_id));
    out.insert("is_admin".into(), json!(ctx.is_team_admin()));
    let not_found = || ApiError::Message(StatusCode::NOT_FOUND, "Not found.");
    if let Some(uuid) = q.get("project") {
        let project = db::project_by_uuid(&state.db, ctx.team_id, uuid)
            .await?
            .ok_or_else(not_found)?;
        if let Some(env) = q.get("environment") {
            let environments = db::project_environments(&state.db, &[int(&project, "id")]).await?;
            let environment = environments
                .iter()
                .find(|e| text(e, "uuid") == env)
                .ok_or_else(not_found)?;
            out.insert("environment".into(), db::brief(environment));
        }
        out.insert("project".into(), db::brief(&project));
    }
    if let Some(uuid) = q.get("server") {
        let server = db::server_by_uuid(&state.db, ctx.team_id, uuid)
            .await?
            .ok_or_else(not_found)?;
        out.insert("server".into(), db::brief(&server));
    }
    Ok(ok(Value::Object(out)).into_response())
}

/// `DeployCanvas::render`: the service templates and logos of the Service step.
async fn deploy_shell(Extension(ctx): Extension<AuthCtx>) -> ApiResult {
    let templates = coolify_jobs::services::templates::get_service_templates();
    let mut names: Vec<(String, String)> = templates
        .keys()
        .map(|k| (k.clone(), coolify_db::audit::headline(k)))
        .collect();
    names.sort_by_key(|(_, n)| n.to_lowercase());
    let mut logos = Map::new();
    for (key, template) in templates {
        let logo = template.get("logo").and_then(Value::as_str);
        if let Some(l) = logo.filter(|l| !l.is_empty() && !l.ends_with("default.webp")) {
            let (src, cdn, _) = crate::ui_services::logo_urls(Some(l));
            logos.insert(key.clone(), json!([src, cdn]));
        }
    }
    Ok(ok(json!({
        "team_id": ctx.team_id,
        "user_id": ctx.user_id,
        "can_add_projects": ctx.is_team_admin(),
        "service_templates": names.iter().map(|(k, n)| json!([k, n])).collect::<Vec<_>>(),
        "service_logos": logos,
    }))
    .into_response())
}

/// `ProjectIconController`.
async fn project_icon(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let project = db::project_by_uuid(&state.db, ctx.team_id, &uuid)
        .await?
        .ok_or(ApiError::Message(StatusCode::NOT_FOUND, "Not Found"))?;
    let local = matches!(
        project.get("icon_storage_type").and_then(Value::as_str),
        None | Some("local")
    );
    crate::ui_teams::image_response(project.get("icon_path").and_then(Value::as_str), local).await
}

/// `Project\Edit::uploadIcon`, `ProjectIconStorageService::storeProject`.
async fn upload_project_icon(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    body: Bytes,
) -> ApiResult {
    let project = db::project_by_uuid(&state.db, ctx.team_id, &uuid)
        .await?
        .ok_or(ApiError::Message(
            StatusCode::NOT_FOUND,
            "Project not found.",
        ))?;
    if !ctx.is_team_admin() {
        return Ok(crate::ui_notifications::unauthorized());
    }
    let contents = match crate::ui_teams::prepare_avatar(&body) {
        Ok(c) => c,
        Err(errors) => {
            let m = errors.join(" ").replace("avatar", "icon");
            return Ok(message(StatusCode::UNPROCESSABLE_ENTITY, &m));
        }
    };
    // The uuid is a path segment, so it must not leave the project-icons directory.
    if uuid.is_empty()
        || !uuid
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Ok(message(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Unable to store the project icon: the project UUID is not valid.",
        ));
    }
    let path = format!("project-icons/{uuid}/icon.jpg");
    if crate::ui_teams::write_image(&path, &contents)
        .await
        .is_err()
    {
        return Ok(message(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Unable to store the project icon.",
        ));
    }
    db::set_project_icon(&state.db, int(&project, "id"), Some(&path), state.now()).await?;
    Ok(ok(json!({"message": "Project icon updated."})).into_response())
}

/// `Project\Edit::removeIcon`, `ProjectIconStorageService::deleteProject`.
async fn remove_project_icon(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let project = db::project_by_uuid(&state.db, ctx.team_id, &uuid)
        .await?
        .ok_or(ApiError::Message(
            StatusCode::NOT_FOUND,
            "Project not found.",
        ))?;
    if !ctx.is_team_admin() {
        return Ok(crate::ui_notifications::unauthorized());
    }
    if let Some(path) = project.get("icon_path").and_then(Value::as_str)
        && !path.contains("..")
    {
        let _ = tokio::fs::remove_file(crate::ui_teams::images_dir().join(path)).await;
    }
    db::set_project_icon(&state.db, int(&project, "id"), None, state.now()).await?;
    Ok(ok(json!({"message": "Project icon removed."})).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_bridged_route_exists_in_the_api() {
        for (method, uri) in BRIDGED {
            let full = format!("api/v1/{uri}");
            assert!(
                crate::routes::ROUTES
                    .iter()
                    .any(|r| r.method == *method && r.uri == full),
                "{method} {full}"
            );
        }
    }
}
