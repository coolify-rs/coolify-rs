//! `Project\Shared\ScheduledTask\{All, Add, Show, Executions}` for applications and services.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Extension, Json, Router};
use coolify_compat::cron::validate_cron_expression;
use coolify_compat::yaml::PhpValue;
use coolify_db::records::Record;
use coolify_db::scheduled_tasks::{self as tasks, NewTask, Owner, TaskChanges};
use coolify_db::servers;
use coolify_db::services::{self as svc, SubKind};
use coolify_jobs::compose_parser::validate::parse_docker_compose_yaml;
use serde_json::{Map, Value, json};

use super::{Class, Res, find};
use crate::audit::Meta;
use crate::auth::AuthCtx;
use crate::input::Input;
use crate::response::{ApiError, ApiResult, ok};
use crate::routes::applications::field;
use crate::state::AppState;
use crate::validation::{Rule, validate};

const INVALID_CRON: &str = "Invalid Cron / Human expression.";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/ui/resources/{kind}/{uuid}/scheduled-tasks",
            get(index).post(add),
        )
        .route(
            "/ui/resources/{kind}/{uuid}/scheduled-tasks/{task}",
            post(save),
        )
        .route(
            "/ui/resources/{kind}/{uuid}/scheduled-tasks/{task}",
            delete(destroy),
        )
        .route(
            "/ui/resources/{kind}/{uuid}/scheduled-tasks/{task}/toggle",
            post(toggle),
        )
        .route(
            "/ui/resources/{kind}/{uuid}/scheduled-tasks/{task}/execute",
            post(execute),
        )
        .route(
            "/ui/resources/{kind}/{uuid}/scheduled-tasks/{task}/executions",
            get(executions),
        )
}

async fn resource(
    state: &AppState,
    ctx: &AuthCtx,
    kind: &str,
    uuid: &str,
) -> Result<(Res, Owner), ApiError> {
    let res = find(state, ctx, kind, uuid).await?;
    let owner = match res.class {
        Class::Application => Owner::Application(res.id),
        Class::Service => Owner::Service(res.id),
        _ => return Err(super::NOT_FOUND),
    };
    Ok((res, owner))
}

/// The toast of `handleError()`: Livewire's validation errors end up there too.
fn toast(message: &str) -> Response {
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        Json(json!({"message": message})),
    )
        .into_response()
}

/// `ValidationException::summarize()`.
fn summarize(errors: &Map<String, Value>) -> String {
    let messages: Vec<&str> = errors
        .values()
        .filter_map(Value::as_array)
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let first = messages.first().copied().unwrap_or_default();
    match messages.len() {
        0 | 1 => first.to_owned(),
        2 => format!("{first} (and 1 more error)"),
        n => format!("{first} (and {} more errors)", n - 1),
    }
}

/// `$this->validate()` in a `try`: the rules of `Add` and `Show`.
fn check(input: &Input) -> Option<Response> {
    let rules = [
        field("name", vec![Rule::Required, Rule::String]),
        field("command", vec![Rule::Required, Rule::String]),
        field("frequency", vec![Rule::Required, Rule::String]),
        field("container", vec![Rule::Nullable, Rule::String]),
        field(
            "timeout",
            vec![
                Rule::Required,
                Rule::Integer,
                Rule::MinNum(60),
                Rule::MaxNum(36000),
            ],
        ),
    ];
    match validate(
        input,
        &rules,
        &[],
        &["name", "command", "frequency", "container", "timeout"],
    ) {
        Err(ApiError::Validation(errors)) => Some(toast(&summarize(&errors))),
        _ => None,
    }
}

fn input_of(body: Value) -> Input {
    match body {
        // `required` fails on a blank string, which a bound field always sends.
        Value::Object(map) => Input(
            map.into_iter()
                .filter(|(_, v)| v.as_str().is_none_or(|t| !t.trim().is_empty()))
                .collect(),
        ),
        _ => Input(Map::new()),
    }
}

fn timeout_of(input: &Input) -> i64 {
    match input.get("timeout") {
        Some(Value::Number(n)) => n.as_i64().unwrap_or(300),
        Some(Value::String(s)) => s.trim().parse().unwrap_or(300),
        _ => 300,
    }
}

fn text<'a>(r: &'a Record, key: &str) -> Option<&'a str> {
    r.get(key).and_then(Value::as_str)
}

fn id_of(r: &Record) -> i64 {
    r.get("id").and_then(Value::as_i64).unwrap_or_default()
}

/// `getServerTimezone()`.
async fn server_timezone(state: &AppState, res: &Res) -> Result<String, ApiError> {
    let server = match res.server_id {
        Some(id) => servers::find_by_id(&state.db, id).await?,
        None => None,
    };
    Ok(server
        .as_ref()
        .and_then(|s| s.settings.as_ref())
        .and_then(|s| s.get("server_timezone"))
        .and_then(Value::as_str)
        .filter(|t| !t.is_empty())
        .unwrap_or("UTC")
        .to_owned())
}

/// `All::mount`: `$containerNames`.
async fn container_names(state: &AppState, res: &Res) -> Result<Vec<String>, ApiError> {
    if res.class == Class::Service {
        let mut names = Vec::new();
        for kind in [SubKind::Application, SubKind::Database] {
            names.extend(
                svc::subs(&state.db, kind, res.id)
                    .await?
                    .into_iter()
                    .map(|s| s.name),
            );
        }
        return Ok(names);
    }
    if res.str("build_pack") != Some("dockercompose") {
        return Ok(Vec::new());
    }
    let parsed = res
        .str("docker_compose_raw")
        .and_then(|raw| parse_docker_compose_yaml(raw).ok());
    let Some(PhpValue::Array(items)) = parsed else {
        return Ok(Vec::new());
    };
    let services = items.iter().find(|(k, _)| k.as_string() == "services");
    Ok(match services {
        Some((_, PhpValue::Array(services))) => {
            services.iter().map(|(k, _)| k.as_string()).collect()
        }
        _ => Vec::new(),
    })
}

/// `$resource->isRunning()`.
async fn is_running(state: &AppState, ctx: &AuthCtx, res: &Res) -> Result<bool, ApiError> {
    if res.class == Class::Service {
        let Some(s) = svc::find_in_team(&state.db, ctx.team_id, &res.uuid).await? else {
            return Ok(false);
        };
        let status = crate::routes::services::status_of(state, &s).await?;
        return Ok(status.unwrap_or_default().contains("running"));
    }
    Ok(res.str("status").unwrap_or_default().starts_with("running"))
}

fn task_json(ctx: &AuthCtx, task: &Record, latest: Option<&Record>) -> Value {
    json!({
        "id": id_of(task),
        "uuid": text(task, "uuid"),
        "name": text(task, "name"),
        // `syncData()`.
        "command": if ctx.is_team_admin() {
            text(task, "command").unwrap_or_default()
        } else {
            "Hidden (only admins can view)"
        },
        "frequency": text(task, "frequency"),
        "container": text(task, "container"),
        "timeout": task.get("timeout").and_then(Value::as_i64).unwrap_or(300),
        "enabled": task.get("enabled").and_then(Value::as_bool).unwrap_or(false),
        // `$task->latest_log`.
        "latest_status": latest.and_then(|e| text(e, "status")),
    })
}

/// `taskAuditContext()`.
fn audit_context(res: &Res, task: &Record) -> Map<String, Value> {
    let Value::Object(map) = json!({
        "team_id": res.team_id,
        "scheduled_task_uuid": text(task, "uuid"),
        "scheduled_task_name": text(task, "name"),
        "resource_type": res.type_name,
        "resource_uuid": res.uuid,
        "resource_name": res.name,
    }) else {
        return Map::new();
    };
    map
}

/// `auditTaskUpdated()`: only the names, because the command can contain secrets.
async fn audit_updated(
    state: &AppState,
    ctx: &AuthCtx,
    meta: &Meta,
    res: &Res,
    task: &Record,
    changed: Vec<&str>,
) {
    if changed.is_empty() {
        return;
    }
    let mut context = audit_context(res, task);
    context.insert("changed_fields".to_owned(), json!(changed));
    crate::audit::log(
        state,
        ctx,
        meta,
        "ui.scheduled_task.updated",
        Value::Object(context),
        "info",
    )
    .await;
}

/// `All::render`.
async fn index(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid)): Path<(String, String)>,
) -> ApiResult {
    let (res, owner) = resource(&state, &ctx, &kind, &uuid).await?;
    let mut rows = Vec::new();
    for task in tasks::list(&state.db, owner).await? {
        let latest = tasks::executions(&state.db, id_of(&task)).await?;
        rows.push(task_json(&ctx, &task, latest.first()));
    }
    Ok(ok(json!({
        "can_update": ctx.is_team_admin(),
        "type": res.type_name,
        "server_timezone": server_timezone(&state, &res).await?,
        "is_running": is_running(&state, &ctx, &res).await?,
        "container_names": container_names(&state, &res).await?,
        "tasks": rows,
    }))
    .into_response())
}

/// `Add::submit`.
async fn add(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path((kind, uuid)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> ApiResult {
    let (res, owner) = resource(&state, &ctx, &kind, &uuid).await?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let input = input_of(body);
    if let Some(answer) = check(&input) {
        return Ok(answer);
    }
    let frequency = input.str("frequency").unwrap_or_default();
    if !validate_cron_expression(frequency) {
        return Ok(toast(INVALID_CRON));
    }
    let container = input.str("container").unwrap_or_default();
    // `$this->subServiceName` does not exist on the component: the access throws into the `catch`.
    if res.class == Class::Service && (container.is_empty() || container == "null") {
        return Ok(toast(
            "Property [$subServiceName] not found on component: [project.shared.scheduled-task.add]",
        ));
    }
    let name = coolify_domain::naming::strip_tags(input.str("name"));
    let task = tasks::create(
        &state.db,
        owner,
        &NewTask {
            name: &name,
            command: input.str("command").unwrap_or_default(),
            frequency,
            container: Some(container),
            timeout: timeout_of(&input),
            enabled: true,
            team_id: res.team_id,
        },
        state.now(),
    )
    .await?;
    crate::audit::log(
        &state,
        &ctx,
        &meta,
        "ui.scheduled_task.created",
        Value::Object(audit_context(&res, &task)),
        "info",
    )
    .await;
    Ok(ok(json!({"message": "Scheduled task added."})).into_response())
}

async fn owned_task(state: &AppState, owner: Owner, uuid: &str) -> Result<Record, ApiError> {
    tasks::find(&state.db, owner, uuid)
        .await?
        .ok_or(ApiError::Message(
            StatusCode::NOT_FOUND,
            "Scheduled task not found.",
        ))
}

/// `Show::submit` (`syncData(true)`).
async fn save(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path((kind, uuid, task)): Path<(String, String, String)>,
    Json(body): Json<Value>,
) -> ApiResult {
    let (res, owner) = resource(&state, &ctx, &kind, &uuid).await?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let task = owned_task(&state, owner, &task).await?;
    let input = input_of(body);
    if let Some(answer) = check(&input) {
        return Ok(answer);
    }
    if !validate_cron_expression(input.str("frequency").unwrap_or_default()) {
        return Ok(toast(INVALID_CRON));
    }
    let changes = TaskChanges {
        name: Some(coolify_domain::naming::strip_tags(
            input.str("name").map(str::trim),
        )),
        command: input.str("command").map(|c| c.trim().to_owned()),
        frequency: input.str("frequency").map(|f| f.trim().to_owned()),
        container: Some(Some(
            input.str("container").unwrap_or_default().trim().to_owned(),
        )),
        timeout: Some(timeout_of(&input)),
        enabled: None,
    };
    // `auditChangedFields()`: a loose comparison, so null and '' are the same.
    let same = |key: &str, new: &Option<String>| {
        text(&task, key).unwrap_or_default() == new.as_deref().unwrap_or_default()
    };
    let mut changed = Vec::new();
    if !same("name", &changes.name) {
        changed.push("name");
    }
    if !same("command", &changes.command) {
        changed.push("command");
    }
    if !same("frequency", &changes.frequency) {
        changed.push("frequency");
    }
    if !same("container", &changes.container.clone().flatten()) {
        changed.push("container");
    }
    if task.get("timeout").and_then(Value::as_i64) != changes.timeout {
        changed.push("timeout");
    }
    let updated = tasks::update(&state.db, &task, &changes, state.now()).await?;
    audit_updated(&state, &ctx, &meta, &res, &updated, changed).await;
    Ok(ok(json!({"message": "Scheduled task updated."})).into_response())
}

/// `Show::toggleEnabled`.
async fn toggle(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path((kind, uuid, task)): Path<(String, String, String)>,
) -> ApiResult {
    let (res, owner) = resource(&state, &ctx, &kind, &uuid).await?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let task = owned_task(&state, owner, &task).await?;
    let enabled = !task
        .get("enabled")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let changes = TaskChanges {
        name: None,
        command: None,
        frequency: None,
        container: None,
        timeout: None,
        enabled: Some(enabled),
    };
    let updated = tasks::update(&state.db, &task, &changes, state.now()).await?;
    audit_updated(&state, &ctx, &meta, &res, &updated, vec!["enabled"]).await;
    Ok(ok(json!({
        "message": if enabled { "Scheduled task enabled." } else { "Scheduled task disabled." },
    }))
    .into_response())
}

/// `Show::executeNow`.
async fn execute(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path((kind, uuid, task)): Path<(String, String, String)>,
) -> ApiResult {
    let (res, owner) = resource(&state, &ctx, &kind, &uuid).await?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let task = owned_task(&state, owner, &task).await?;
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    jobs.dispatch_scheduled_task(id_of(&task));
    crate::audit::log(
        &state,
        &ctx,
        &meta,
        "ui.scheduled_task.executed",
        json!({
            "team_id": res.team_id,
            "resource_uuid": res.uuid,
            "resource_name": res.name,
            "scheduled_task_uuid": text(&task, "uuid"),
            "scheduled_task_name": text(&task, "name"),
        }),
        "info",
    )
    .await;
    Ok(ok(json!({"message": "Scheduled task executed."})).into_response())
}

/// `Show::delete`.
async fn destroy(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path((kind, uuid, task)): Path<(String, String, String)>,
) -> ApiResult {
    let (res, owner) = resource(&state, &ctx, &kind, &uuid).await?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let task = owned_task(&state, owner, &task).await?;
    tasks::delete(&state.db, id_of(&task)).await?;
    crate::audit::log(
        &state,
        &ctx,
        &meta,
        "ui.scheduled_task.deleted",
        Value::Object(audit_context(&res, &task)),
        "info",
    )
    .await;
    Ok(ok(json!({"message": "Scheduled task deleted."})).into_response())
}

/// `Executions::refreshExecutions`: the last 20. Output can contain secrets, so `canReadOutput()`
/// is checked here and not in the page.
async fn executions(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid, task)): Path<(String, String, String)>,
) -> ApiResult {
    let (_, owner) = resource(&state, &ctx, &kind, &uuid).await?;
    let task = owned_task(&state, owner, &task).await?;
    let rows: Vec<Value> = tasks::executions(&state.db, id_of(&task))
        .await?
        .iter()
        .take(20)
        .map(|e| {
            json!({
                "id": id_of(e),
                "status": text(e, "status"),
                "created_at": text(e, "created_at"),
                "finished_at": text(e, "finished_at"),
                "message": if ctx.is_team_admin() { text(e, "message") } else { None },
            })
        })
        .collect();
    Ok(ok(json!({
        "can_read_output": ctx.is_team_admin(),
        "executions": rows,
    }))
    .into_response())
}
