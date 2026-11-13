//! `ScheduledTasksController`. The controller's private methods are generic over the resource
//! ([`Resource`]); the services routes only need their own lookup and thin handlers.

use axum::Extension;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, Uri};
use coolify_compat::cron::validate_cron_expression;
use coolify_db::applications as apps;
use coolify_db::records::{Record, laravel_datetime};
use coolify_db::scheduled_tasks::{self as tasks, NewTask, Owner, TaskChanges};
use serde_json::{Map, Value, json};

use crate::auth::AuthCtx;
use crate::input::{Input, validated_json};
use crate::response::{ApiError, ApiResult, PhpJson, created, ok};
use crate::routes::application_fields::save_error;
use crate::routes::application_ops::NOT_FOUND;
use crate::routes::applications::field;
use crate::routes::databases::input_bool;
use crate::routes::storages::can_deploy;
use crate::serialize::serialize_record;
use crate::state::AppState;
use crate::validation::{CUSTOM_API_MESSAGES, Rule, validate};
use coolify_domain::grants::AccessLevel;

const TASK_NOT_FOUND: ApiError =
    ApiError::Message(StatusCode::NOT_FOUND, "Scheduled task not found.");
const ALLOWED: &[&str] = &[
    "name",
    "command",
    "frequency",
    "container",
    "timeout",
    "use_shell",
    "enabled",
];
/// `enabledTaskForbiddenResponse()`.
const ENABLED_FORBIDDEN: &str = "Missing required permissions: deploy. An enabled scheduled task runs commands in the resource containers, so it needs a token with the deploy permission.";

/// The application or service a task belongs to.
#[derive(Debug, Clone)]
pub struct Resource {
    pub owner: Owner,
    /// `'application'` or `'service'`, for the audit log.
    pub kind: &'static str,
    pub uuid: String,
    /// The resource's environment, for its access grants.
    pub environment_id: Option<i64>,
}

/// `resolveApplication()`.
async fn application(state: &AppState, ctx: &AuthCtx, uuid: &str) -> Result<Resource, ApiError> {
    let a = apps::find_in_team(&state.db, ctx.team_id, uuid)
        .await?
        .ok_or(NOT_FOUND)?;
    Ok(Resource {
        owner: Owner::Application(a.id),
        kind: "application",
        environment_id: a.int("environment_id"),
        uuid: a.uuid,
    })
}

/// `$task` with `removeSensitiveData()` and `serializeApiResponse()`.
fn present_task(ctx: &AuthCtx, raw: Record) -> Value {
    let mut r = present(raw, &["created_at", "updated_at", "failing_since"]);
    for hidden in ["id", "team_id", "application_id", "service_id"] {
        r.shift_remove(hidden);
    }
    if !ctx.can_read_sensitive {
        r.shift_remove("command");
    }
    Value::Object(serialize_record(r))
}

/// `$execution` with `makeHidden()` and `serializeApiResponse()`.
fn present_execution(ctx: &AuthCtx, raw: Record) -> Value {
    let mut r = present(
        raw,
        &["created_at", "updated_at", "started_at", "finished_at"],
    );
    // `worker_host`: `RecordsWorkerHost` hides it (Coolify v4.6.0).
    for hidden in ["id", "scheduled_task_id", "worker_host"] {
        r.shift_remove(hidden);
    }
    if !ctx.can_read_sensitive {
        r.shift_remove("message");
    }
    Value::Object(serialize_record(r))
}

fn present(raw: Record, datetimes: &[&str]) -> Map<String, Value> {
    raw.into_iter()
        .map(|(k, v)| {
            let v = match v {
                Value::String(s) if datetimes.contains(&k.as_str()) => {
                    Value::String(laravel_datetime(&s))
                }
                // The `duration` cast is `float`.
                Value::Number(n) if k == "duration" => n
                    .as_f64()
                    .and_then(serde_json::Number::from_f64)
                    .map_or(Value::Null, Value::Number),
                other => other,
            };
            (k, v)
        })
        .collect()
}

fn id_of(task: &Record) -> i64 {
    task.get("id").and_then(Value::as_i64).unwrap_or_default()
}

fn name_of(task: &Record) -> &str {
    task.get("name").and_then(Value::as_str).unwrap_or_default()
}

fn uuid_of(task: &Record) -> &str {
    task.get("uuid").and_then(Value::as_str).unwrap_or_default()
}

/// `$this->authorize('update' | 'delete', $resource)`: the grant level on the resource.
fn authorize(ctx: &AuthCtx, resource: &Resource, level: AccessLevel) -> Result<(), ApiError> {
    if ctx.can_resource(level, resource.environment_id, &resource.uuid) {
        Ok(())
    } else {
        Err(ApiError::Unauthorized)
    }
}

/// `deployAbilityForbiddenResponse()` when the task will be enabled.
fn enabled_task_forbidden(ctx: &AuthCtx, enabled: bool) -> Result<(), ApiError> {
    if enabled && !can_deploy(ctx) {
        return Err(ApiError::Message(StatusCode::FORBIDDEN, ENABLED_FORBIDDEN));
    }
    Ok(())
}

/// `validate_cron_expression($request->frequency)` with its controller response.
fn check_frequency(input: &Input) -> Result<(), ApiError> {
    let frequency = input.str("frequency").unwrap_or_default();
    if validate_cron_expression(frequency) {
        return Ok(());
    }
    Err(ApiError::Validation(
        [(
            "frequency".to_owned(),
            json!(["Invalid cron expression or frequency format."]),
        )]
        .into_iter()
        .collect(),
    ))
}

/// `strip_tags()` of the `name` setter (`HasSafeStringAttribute`).
fn safe_name(name: &str) -> String {
    coolify_domain::naming::strip_tags(Some(name))
}

/// The cast `timeout` reads as an integer; the rule accepted `"10"`.
fn int_value(v: Option<&Value>) -> Option<i64> {
    match v {
        Some(Value::Number(n)) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        Some(Value::String(s)) => s.trim().parse().ok(),
        Some(Value::Bool(b)) => Some(i64::from(*b)),
        _ => None,
    }
}

/// `listTasks()`.
pub async fn list_tasks(
    state: &AppState,
    ctx: &AuthCtx,
    resource: &Resource,
) -> ApiResult<PhpJson> {
    let rows = tasks::list(&state.db, resource.owner).await?;
    Ok(ok(Value::Array(
        rows.into_iter().map(|r| present_task(ctx, r)).collect(),
    )))
}

/// `createTask()`.
pub async fn create_task(
    state: &AppState,
    ctx: &AuthCtx,
    resource: &Resource,
    headers: &HeaderMap,
    uri: &Uri,
    body: &[u8],
) -> ApiResult<PhpJson> {
    authorize(ctx, resource, AccessLevel::Write)?;
    let input = validated_json(headers, uri, body)?;
    validate(
        &input,
        &[
            field("name", vec![Rule::Required, Rule::String, Rule::Max(255)]),
            field("command", vec![Rule::Required, Rule::String]),
            field("frequency", vec![Rule::Required, Rule::String]),
            field("container", vec![Rule::String, Rule::Nullable]),
            field(
                "timeout",
                vec![Rule::Integer, Rule::MinNum(1), Rule::MaxNum(36000)],
            ),
            field("use_shell", vec![Rule::Boolean]),
            field("enabled", vec![Rule::Boolean]),
        ],
        CUSTOM_API_MESSAGES,
        ALLOWED,
    )?;
    check_frequency(&input)?;
    let enabled = if input.has("enabled") {
        input_bool(&input, "enabled")
    } else {
        true
    };
    enabled_task_forbidden(ctx, enabled)?;

    let name = safe_name(input.str("name").unwrap_or_default());
    let task = tasks::create(
        &state.db,
        resource.owner,
        &NewTask {
            name: &name,
            command: input.str("command").unwrap_or_default(),
            frequency: input.str("frequency").unwrap_or_default(),
            container: input.str("container"),
            timeout: if input.has("timeout") {
                int_value(input.get("timeout")).unwrap_or(300)
            } else {
                300
            },
            // `$request->has('use_shell') ? $request->boolean('use_shell') : true`.
            use_shell: !input.has("use_shell") || input_bool(&input, "use_shell"),
            enabled,
            team_id: ctx.team_id,
        },
        state.now(),
    )
    .await
    .map_err(save_error)?;
    // The new model holds what `createTask()` set, not the column defaults of the failure streak.
    let mut task = task;
    for unset in ["consecutive_failures", "failing_since", "disabled_reason"] {
        task.shift_remove(unset);
    }
    tracing::info!(target: "audit", event = "api.scheduled_task.created", team_id = ctx.team_id, task_uuid = uuid_of(&task), task_name = name_of(&task), resource_type = resource.kind, resource_uuid = %resource.uuid, "scheduled task created");
    Ok(created(present_task(ctx, task)))
}

/// `updateTask()`.
pub async fn update_task(
    state: &AppState,
    ctx: &AuthCtx,
    resource: &Resource,
    task_uuid: &str,
    headers: &HeaderMap,
    uri: &Uri,
    body: &[u8],
) -> ApiResult<PhpJson> {
    authorize(ctx, resource, AccessLevel::Write)?;
    let input = validated_json(headers, uri, body)?;
    validate(
        &input,
        &[
            field("name", vec![Rule::String, Rule::Max(255)]),
            field("command", vec![Rule::String]),
            field("frequency", vec![Rule::String]),
            field("container", vec![Rule::String, Rule::Nullable]),
            field(
                "timeout",
                vec![Rule::Integer, Rule::MinNum(1), Rule::MaxNum(36000)],
            ),
            field("use_shell", vec![Rule::Boolean]),
            field("enabled", vec![Rule::Boolean]),
        ],
        CUSTOM_API_MESSAGES,
        ALLOWED,
    )?;
    if input.has("frequency") {
        check_frequency(&input)?;
    }
    let task = tasks::find(&state.db, resource.owner, task_uuid)
        .await?
        .ok_or(TASK_NOT_FOUND)?;
    let enabled = if input.has("enabled") {
        input_bool(&input, "enabled")
    } else {
        task.get("enabled")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    };
    enabled_task_forbidden(ctx, enabled)?;

    let changes = TaskChanges {
        name: input
            .has("name")
            .then(|| safe_name(input.str("name").unwrap_or_default())),
        command: input.str("command").map(str::to_owned),
        frequency: input.str("frequency").map(str::to_owned),
        container: input
            .has("container")
            .then(|| input.str("container").map(str::to_owned)),
        timeout: input
            .has("timeout")
            .then(|| int_value(input.get("timeout")))
            .flatten(),
        use_shell: input
            .has("use_shell")
            .then(|| input_bool(&input, "use_shell")),
        enabled: input.has("enabled").then_some(enabled),
    };
    let updated = tasks::update(&state.db, &task, &changes, state.now())
        .await
        .map_err(save_error)?;
    // `auditChangedFields($task)`: nothing is logged when the request changed nothing.
    let changed = changed_fields(&task, &updated);
    if changed.is_empty() {
        return Ok(ok(present_task(ctx, updated)));
    }
    tracing::info!(target: "audit", event = "api.scheduled_task.updated", team_id = ctx.team_id, task_uuid = uuid_of(&updated), task_name = name_of(&updated), resource_type = resource.kind, resource_uuid = %resource.uuid, changed_fields = ?changed, "scheduled task updated");
    Ok(ok(present_task(ctx, updated)))
}

/// `auditChangedFields($task)` after `fill($request->only($allowedFields))`: the allowed fields
/// whose value differs from the stored one, in the order of `$allowedFields`.
fn changed_fields(before: &Record, after: &Record) -> Vec<&'static str> {
    ALLOWED
        .iter()
        .copied()
        .filter(|k| before.get(*k) != after.get(*k))
        .collect()
}

/// `deleteTask()`.
pub async fn delete_task(
    state: &AppState,
    ctx: &AuthCtx,
    resource: &Resource,
    task_uuid: &str,
) -> ApiResult<PhpJson> {
    authorize(ctx, resource, AccessLevel::Delete)?;
    let task = tasks::find(&state.db, resource.owner, task_uuid)
        .await?
        .ok_or(TASK_NOT_FOUND)?;
    tasks::delete(&state.db, id_of(&task)).await?;
    tracing::info!(target: "audit", event = "api.scheduled_task.deleted", team_id = ctx.team_id, task_uuid = uuid_of(&task), task_name = name_of(&task), resource_type = resource.kind, resource_uuid = %resource.uuid, "scheduled task deleted");
    Ok(ok(json!({"message": "Scheduled task deleted."})))
}

/// `getExecutions()`.
pub async fn get_executions(
    state: &AppState,
    ctx: &AuthCtx,
    resource: &Resource,
    task_uuid: &str,
) -> ApiResult<PhpJson> {
    let task = tasks::find(&state.db, resource.owner, task_uuid)
        .await?
        .ok_or(TASK_NOT_FOUND)?;
    let rows = tasks::executions(&state.db, id_of(&task)).await?;
    Ok(ok(Value::Array(
        rows.into_iter()
            .map(|r| present_execution(ctx, r))
            .collect(),
    )))
}

/// `executeTask()`.
pub async fn execute_task(
    state: &AppState,
    ctx: &AuthCtx,
    resource: &Resource,
    task_uuid: &str,
) -> ApiResult<PhpJson> {
    authorize(ctx, resource, AccessLevel::Write)?;
    let task = tasks::find(&state.db, resource.owner, task_uuid)
        .await?
        .ok_or(TASK_NOT_FOUND)?;
    let jobs = state.jobs.as_ref().ok_or(ApiError::Internal)?;
    jobs.dispatch_scheduled_task(id_of(&task));
    tracing::info!(target: "audit", event = "api.scheduled_task.executed", team_id = ctx.team_id, task_uuid = uuid_of(&task), task_name = name_of(&task), resource_type = resource.kind, resource_uuid = %resource.uuid, "scheduled task execution queued");
    Ok(ok(json!({"message": "Scheduled task execution queued."})))
}

// ---------------------------------------------------------------------------
// Application routes

/// `GET /applications/{uuid}/scheduled-tasks`.
pub async fn application_list(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult<PhpJson> {
    let r = application(&state, &ctx, &uuid).await?;
    list_tasks(&state, &ctx, &r).await
}

/// `POST /applications/{uuid}/scheduled-tasks`.
pub async fn application_create(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult<PhpJson> {
    let r = application(&state, &ctx, &uuid).await?;
    create_task(&state, &ctx, &r, &headers, &uri, &body).await
}

/// `PATCH /applications/{uuid}/scheduled-tasks/{task_uuid}`.
pub async fn application_update(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((uuid, task_uuid)): Path<(String, String)>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult<PhpJson> {
    let r = application(&state, &ctx, &uuid).await?;
    update_task(&state, &ctx, &r, &task_uuid, &headers, &uri, &body).await
}

/// `DELETE /applications/{uuid}/scheduled-tasks/{task_uuid}`.
pub async fn application_delete(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((uuid, task_uuid)): Path<(String, String)>,
) -> ApiResult<PhpJson> {
    let r = application(&state, &ctx, &uuid).await?;
    delete_task(&state, &ctx, &r, &task_uuid).await
}

/// `GET /applications/{uuid}/scheduled-tasks/{task_uuid}/executions`.
pub async fn application_executions(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((uuid, task_uuid)): Path<(String, String)>,
) -> ApiResult<PhpJson> {
    let r = application(&state, &ctx, &uuid).await?;
    get_executions(&state, &ctx, &r, &task_uuid).await
}

/// `POST /applications/{uuid}/scheduled-tasks/{task_uuid}/execute`.
pub async fn application_execute(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((uuid, task_uuid)): Path<(String, String)>,
) -> ApiResult<PhpJson> {
    let r = application(&state, &ctx, &uuid).await?;
    execute_task(&state, &ctx, &r, &task_uuid).await
}
