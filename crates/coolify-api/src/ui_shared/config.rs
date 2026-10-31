//! `Project\Shared\{ResourceLimits, HealthChecks, Tags, Webhooks}`.

use std::sync::LazyLock;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use coolify_db::database_parts as parts;
use coolify_db::servers::ColumnValue as C;
use regex::Regex;
use serde_json::{Map, Value, json};

use super::envs::snake;
use super::{Class, Res, find};
use crate::audit::Meta;
use crate::auth::AuthCtx;
use crate::response::{ApiError, ApiResult, PhpJson, ok};
use crate::state::AppState;
use crate::ui_notifications::{toast_error, unauthorized};

type P = Path<(String, String)>;

pub fn routes() -> Router<AppState> {
    let b = "/ui/resources/{kind}/{uuid}";
    Router::new()
        .route(&format!("{b}/limits"), get(limits).post(save_limits))
        .route(&format!("{b}/healthcheck"), get(health).post(save_health))
        .route(&format!("{b}/healthcheck/toggle"), post(toggle_health))
        .route(&format!("{b}/tags"), get(tags).post(add_tags))
        .route(
            &format!("{b}/tags/{{id}}"),
            post(add_tag).delete(remove_tag),
        )
        .route(&format!("{b}/webhooks"), get(webhooks).post(save_webhooks))
        .route(&format!("{b}/webhooks/generate"), post(generate_secret))
        .route(&format!("{b}/metrics"), get(metrics))
}

/// `AuditEvent::isSensitiveField`, for the columns these sections write.
fn sensitive(field: &str) -> bool {
    field.contains("secret")
        || matches!(field, "health_check_command" | "health_check_response_text")
}

/// `$resource->save()` with the `Auditable` row: the changed columns in table order.
pub async fn save_model(
    state: &AppState,
    ctx: &AuthCtx,
    meta: &Meta,
    res: &Res,
    values: Vec<(&'static str, C)>,
    shown: &Map<String, Value>,
) -> Result<(), ApiError> {
    let changed: Vec<&'static str> = values.iter().map(|(k, _)| *k).collect();
    parts::update_row(&state.db, res.table, res.id, &values, state.now()).await?;
    if changed.is_empty() {
        return Ok(());
    }
    let resource = res.morph.rsplit('\\').next().map(snake).unwrap_or_default();
    let mut ordered: Vec<&str> = res
        .raw
        .keys()
        .map(String::as_str)
        .filter(|k| changed.contains(k))
        .collect();
    for c in &changed {
        if !ordered.contains(c) {
            ordered.push(c);
        }
    }
    let mut changes = Map::new();
    for f in &ordered {
        if sensitive(f) {
            continue;
        }
        changes.insert(
            (*f).to_owned(),
            json!({"old": res.raw.get(*f), "new": shown.get(*f)}),
        );
    }
    crate::audit::mutation(
        state,
        ctx,
        meta,
        &resource,
        "updated",
        json!({
            "team_id": res.team_id,
            format!("{resource}_uuid"): res.uuid,
            format!("{resource}_name"): res.name,
            "changed_fields": ordered,
        }),
        Value::Object(changes),
    )
    .await;
    Ok(())
}

fn text_of(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// What `Project\Shared\Metrics` decides its states from.
async fn metrics(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid)): P,
) -> ApiResult {
    let res = find(&state, &ctx, &kind, &uuid).await?;
    let server = match res.server_id {
        Some(id) => coolify_db::servers::find_by_id(&state.db, id).await?,
        None => None,
    };
    let setting = |k: &str| {
        server
            .as_ref()
            .and_then(|s| s.settings.as_ref())
            .and_then(|s| s.get(k))
            .and_then(Value::as_bool)
            .unwrap_or(false)
    };
    Ok(ok(json!({
        "is_application": res.is_application(),
        "build_pack": res.str("build_pack"),
        "metrics_enabled": setting("is_metrics_enabled"),
        "running": res.str("status").unwrap_or_default().contains("running"),
        "server_uuid": server.as_ref().map(|s| s.uuid.clone()),
    }))
    .into_response())
}

// ---------------------------------------------------------------------------
// Resource limits
// ---------------------------------------------------------------------------

async fn limits(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid)): P,
) -> ApiResult {
    let res = find(&state, &ctx, &kind, &uuid).await?;
    Ok(ok(json!({
        "can_update": ctx.is_team_admin(),
        "limitsCpus": res.raw.get("limits_cpus"),
        "limitsCpuset": res.raw.get("limits_cpuset"),
        "limitsCpuShares": res.raw.get("limits_cpu_shares"),
        "limitsMemory": res.raw.get("limits_memory"),
        "limitsMemorySwap": res.raw.get("limits_memory_swap"),
        "limitsMemorySwappiness": res.raw.get("limits_memory_swappiness"),
        "limitsMemoryReservation": res.raw.get("limits_memory_reservation"),
    }))
    .into_response())
}

static MEMORY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(0|\d+[bBkKmMgG])\n?\z").unwrap_or_else(|_| unreachable!()));
static CPUS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\d*\.?\d+\n?\z").unwrap_or_else(|_| unreachable!()));
static CPUSET: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\d+([,-]\d+)*\n?\z").unwrap_or_else(|_| unreachable!()));

/// `ResourceLimits::submit`.
async fn save_limits(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path((kind, uuid)): P,
    Json(body): Json<Value>,
) -> ApiResult {
    let res = find(&state, &ctx, &kind, &uuid).await?;
    if !ctx.is_team_admin() {
        return Ok(unauthorized());
    }
    let field = |k: &str| body.get(k).and_then(text_of).unwrap_or_default();
    let or_zero = |v: String| if v.is_empty() { "0".to_owned() } else { v };
    let memory = or_zero(field("limitsMemory"));
    let swap = or_zero(field("limitsMemorySwap"));
    let reservation = or_zero(field("limitsMemoryReservation"));
    let cpus = or_zero(field("limitsCpus"));
    let cpuset = field("limitsCpuset");
    let swappiness = {
        let v = field("limitsMemorySwappiness");
        if v.is_empty() { "60".to_owned() } else { v }
    };
    let shares = {
        let v = field("limitsCpuShares");
        if v.is_empty() { "1024".to_owned() } else { v }
    };
    // The catch dispatches one toast per message, in the order of `$rules`.
    let mut errors: Vec<&str> = Vec::new();
    if !MEMORY.is_match(&memory) {
        errors.push("Maximum Memory Limit must be a number followed by a unit (b, k, m, g). Example: 256m, 1g. Use 0 for unlimited.");
    }
    if !MEMORY.is_match(&swap) {
        errors.push("Maximum Swap Limit must be a number followed by a unit (b, k, m, g). Example: 256m, 1g. Use 0 for unlimited.");
    }
    match swappiness.parse::<i64>() {
        Err(_) => errors.push("Swappiness must be a whole number between 0 and 100."),
        Ok(n) if !(0..=100).contains(&n) => errors.push("Swappiness must be between 0 and 100."),
        Ok(_) => {}
    }
    if !MEMORY.is_match(&reservation) {
        errors.push("Soft Memory Limit must be a number followed by a unit (b, k, m, g). Example: 256m, 1g. Use 0 for unlimited.");
    }
    if !CPUS.is_match(&cpus) {
        errors.push("Number of CPUs must be a number (integer or decimal). Example: 0.5, 2.");
    }
    if !cpuset.is_empty() && !CPUSET.is_match(&cpuset) {
        errors.push(
            "CPU sets must be a comma-separated list of CPU numbers or ranges. Example: 0-2 or 0,1,3.",
        );
    }
    match shares.parse::<i64>() {
        Err(_) => errors.push("CPU Weight must be a whole number."),
        Ok(n) if n < 0 => errors.push("CPU Weight must be a positive number."),
        Ok(_) => {}
    }
    if !errors.is_empty() {
        return Ok(PhpJson(
            StatusCode::UNPROCESSABLE_ENTITY,
            json!({"message": errors[0], "errors": {"toasts": errors}}),
        )
        .into_response());
    }
    let swappiness: i64 = swappiness.parse().unwrap_or(60);
    let shares: i64 = shares.parse().unwrap_or(1024);
    let current_text = |k: &str| res.raw.get(k).and_then(Value::as_str).map(str::to_owned);
    let current_int = |k: &str| res.raw.get(k).and_then(Value::as_i64);
    let mut values: Vec<(&'static str, C)> = Vec::new();
    let mut shown = Map::new();
    for (k, v) in [
        ("limits_memory", Some(memory)),
        ("limits_memory_swap", Some(swap)),
        ("limits_memory_reservation", Some(reservation)),
        ("limits_cpus", Some(cpus)),
        ("limits_cpuset", (!cpuset.is_empty()).then_some(cpuset)),
    ] {
        if current_text(k) != v {
            shown.insert(k.to_owned(), json!(v));
            values.push((k, C::Text(v)));
        }
    }
    for (k, v) in [
        ("limits_memory_swappiness", swappiness),
        ("limits_cpu_shares", shares),
    ] {
        if current_int(k) != Some(v) {
            shown.insert(k.to_owned(), json!(v));
            values.push((k, C::Int(v)));
        }
    }
    save_model(&state, &ctx, &meta, &res, values, &shown).await?;
    Ok(ok(json!({"message": "Resource limits updated."})).into_response())
}

// ---------------------------------------------------------------------------
// Health checks
// ---------------------------------------------------------------------------

async fn health(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid)): P,
) -> ApiResult {
    let res = find(&state, &ctx, &kind, &uuid).await?;
    let mut m = Map::new();
    for k in [
        "health_check_enabled",
        "health_check_type",
        "health_check_command",
        "health_check_method",
        "health_check_scheme",
        "health_check_host",
        "health_check_port",
        "health_check_path",
        "health_check_return_code",
        "health_check_response_text",
        "health_check_interval",
        "health_check_timeout",
        "health_check_retries",
        "health_check_start_period",
        "custom_healthcheck_found",
    ] {
        m.insert(k.to_owned(), res.raw.get(k).cloned().unwrap_or(Value::Null));
    }
    m.insert("can_update".into(), json!(ctx.is_team_admin()));
    Ok(ok(Value::Object(m)).into_response())
}

static HC_COMMAND: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[a-zA-Z0-9 \-_./:=@,+]+\n?\z").unwrap_or_else(|_| unreachable!())
});
static HC_PATH: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-zA-Z0-9/\-_.~%,;]+\n?\z").unwrap_or_else(|_| unreachable!()));
static HC_HOST: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-zA-Z0-9.\-_]+\n?\z").unwrap_or_else(|_| unreachable!()));

fn valid_host(v: &str) -> bool {
    HC_HOST.is_match(v)
        || v.trim_matches(|c| c == '[' || c == ']')
            .parse::<std::net::Ipv6Addr>()
            .is_ok()
}

/// `HealthChecks::rules()` with Laravel's default messages.
fn health_errors(body: &Value) -> Map<String, Value> {
    let mut e = Map::new();
    let s = |k: &str| body.get(k).and_then(text_of);
    let mut put = |k: &str, m: String| {
        e.insert(k.to_owned(), json!([m]));
    };
    if let Some(c) = s("health_check_command").filter(|c| !c.is_empty()) {
        if c.chars().count() > 1000 {
            put(
                "health_check_command",
                "The health check command field must not be greater than 1000 characters.".into(),
            );
        } else if !HC_COMMAND.is_match(&c) {
            put(
                "health_check_command",
                "The health check command field format is invalid.".into(),
            );
        }
    }
    match s("health_check_path").filter(|c| !c.is_empty()) {
        None => put(
            "health_check_path",
            "The health check path field is required.".into(),
        ),
        Some(p) if !HC_PATH.is_match(&p) => put(
            "health_check_path",
            "The health check path field format is invalid.".into(),
        ),
        _ => {}
    }
    if let Some(p) = s("health_check_port").filter(|c| !c.is_empty()) {
        match p.parse::<i64>() {
            Err(_) => put(
                "health_check_port",
                "The health check port field must be an integer.".into(),
            ),
            Ok(n) if n < 1 => put(
                "health_check_port",
                "The health check port field must be at least 1.".into(),
            ),
            Ok(n) if n > 65535 => put(
                "health_check_port",
                "The health check port field must not be greater than 65535.".into(),
            ),
            _ => {}
        }
    }
    match s("health_check_host").filter(|c| !c.is_empty()) {
        None => put(
            "health_check_host",
            "The health check host field is required.".into(),
        ),
        Some(h) if !valid_host(&h) => put(
            "health_check_host",
            "The health check host must be a hostname, an IPv4 address, or an IPv6 address.".into(),
        ),
        _ => {}
    }
    for (k, label, min) in [
        ("health_check_interval", "health check interval", true),
        ("health_check_timeout", "health check timeout", true),
        ("health_check_retries", "health check retries", true),
        (
            "health_check_start_period",
            "health check start period",
            false,
        ),
        (
            "health_check_return_code",
            "health check return code",
            false,
        ),
    ] {
        match s(k).unwrap_or_default().parse::<i64>() {
            Err(_) => put(k, format!("The {label} field must be an integer.")),
            Ok(n) if min && n < 1 => put(k, format!("The {label} field must be at least 1.")),
            _ => {}
        }
    }
    e
}

fn health_values(res: &Res, body: &Value) -> (Vec<(&'static str, C)>, Map<String, Value>) {
    let mut values: Vec<(&'static str, C)> = Vec::new();
    let mut shown = Map::new();
    for k in [
        "health_check_type",
        "health_check_method",
        "health_check_scheme",
        "health_check_host",
        "health_check_path",
    ] {
        let new = body.get(k).and_then(text_of);
        if res.raw.get(k).and_then(Value::as_str) != new.as_deref() {
            shown.insert(k.to_owned(), json!(new));
            values.push((k, C::Text(new)));
        }
    }
    for k in [
        "health_check_command",
        "health_check_port",
        "health_check_response_text",
    ] {
        let new = body.get(k).and_then(text_of).filter(|v| !v.is_empty());
        let cur = res.raw.get(k).and_then(text_of);
        if cur != new {
            shown.insert(k.to_owned(), json!(new));
            values.push((k, C::Text(new)));
        }
    }
    for k in [
        "health_check_return_code",
        "health_check_interval",
        "health_check_timeout",
        "health_check_retries",
        "health_check_start_period",
    ] {
        let new = body
            .get(k)
            .and_then(text_of)
            .and_then(|v| v.parse::<i64>().ok());
        if let Some(n) = new
            && res.raw.get(k).and_then(Value::as_i64) != Some(n)
        {
            shown.insert(k.to_owned(), json!(n));
            values.push((k, C::Int(n)));
        }
    }
    (values, shown)
}

/// `HealthChecks::submit`.
async fn save_health(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path((kind, uuid)): P,
    Json(body): Json<Value>,
) -> ApiResult {
    let res = find(&state, &ctx, &kind, &uuid).await?;
    if !ctx.is_team_admin() {
        return Ok(unauthorized());
    }
    let errors = health_errors(&body);
    if let Some(first) = errors
        .values()
        .next()
        .and_then(|v| v.get(0))
        .and_then(Value::as_str)
    {
        // `handleError`: the toast, with the count of other messages.
        let count = errors.len() - 1;
        let message = match count {
            0 => first.to_owned(),
            1 => format!("{first} (and 1 more error)"),
            n => format!("{first} (and {n} more errors)"),
        };
        return Ok(toast_error(StatusCode::UNPROCESSABLE_ENTITY, message));
    }
    let (values, shown) = health_values(&res, &body);
    save_model(&state, &ctx, &meta, &res, values, &shown).await?;
    Ok(ok(json!({"message": "Healthcheck updated."})).into_response())
}

/// `HealthChecks::toggleHealthcheck`.
async fn toggle_health(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path((kind, uuid)): P,
    Json(body): Json<Value>,
) -> ApiResult {
    let res = find(&state, &ctx, &kind, &uuid).await?;
    if !ctx.is_team_admin() {
        return Ok(unauthorized());
    }
    let was_enabled = res.bool("health_check_enabled");
    let enabled = !was_enabled;
    let (mut values, mut shown) = health_values(&res, &body);
    shown.insert("health_check_enabled".into(), json!(enabled));
    values.push(("health_check_enabled", C::Bool(enabled)));
    save_model(&state, &ctx, &meta, &res, values, &shown).await?;
    let status = res.str("status").unwrap_or_default();
    let running = match res.class {
        Class::Service => status.contains("running"),
        _ => status.starts_with("running"),
    };
    if enabled && running {
        return Ok(ok(json!({"info": "Healthcheck has been enabled. A restart is required to apply the new settings."})).into_response());
    }
    let word = if enabled { "enabled" } else { "disabled" };
    Ok(ok(json!({"message": format!("Healthcheck {word}.")})).into_response())
}

// ---------------------------------------------------------------------------
// Tags
// ---------------------------------------------------------------------------

async fn team_tags(state: &AppState, team_id: i64) -> Result<Vec<Value>, ApiError> {
    let rows: Vec<(i64, String)> =
        sqlx::query_as("SELECT id, name FROM tags WHERE team_id = $1 ORDER BY name")
            .bind(team_id)
            .fetch_all(state.db.pool())
            .await
            .map_err(coolify_db::DbError::from)?;
    Ok(rows
        .into_iter()
        .map(|(id, name)| json!({"id": id, "name": name}))
        .collect())
}

async fn tags(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid)): P,
) -> ApiResult {
    let res = find(&state, &ctx, &kind, &uuid).await?;
    let assigned = parts::tags_of(&state.db, &res.morph, res.id).await?;
    let ids: Vec<i64> = assigned
        .iter()
        .filter_map(|t| t.get("id").and_then(Value::as_i64))
        .collect();
    let available: Vec<Value> = team_tags(&state, res.team_id)
        .await?
        .into_iter()
        .filter(|t| {
            !t.get("id")
                .and_then(Value::as_i64)
                .is_some_and(|i| ids.contains(&i))
        })
        .collect();
    let assigned: Vec<Value> = assigned
        .iter()
        .map(|t| json!({"id": t.get("id"), "name": t.get("name")}))
        .collect();
    Ok(ok(json!({"can_update": ctx.is_team_admin(), "assigned": assigned, "available": available})).into_response())
}

/// `Tags::submit`.
async fn add_tags(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path((kind, uuid)): P,
    Json(body): Json<Value>,
) -> ApiResult {
    let res = find(&state, &ctx, &kind, &uuid).await?;
    if !ctx.is_team_admin() {
        return Ok(unauthorized());
    }
    let input = body
        .get("newTags")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if input.trim().is_empty() {
        return Ok(toast_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "The new tags field is required.",
        ));
    }
    if input.chars().count() < 2 {
        return Ok(toast_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "The new tags field must be at least 2 characters.",
        ));
    }
    let mut toasts: Vec<Value> = Vec::new();
    for raw in input.trim().split(' ') {
        let tag = coolify_domain::naming::strip_tags(Some(raw));
        if tag.len() < 2 {
            toasts.push(json!({"title": "Invalid tag.", "message": format!("Tag <span class='dark:text-warning'>{tag}</span> is invalid. Min length is 2.")}));
            continue;
        }
        let exists = parts::tags_of(&state.db, &res.morph, res.id)
            .await?
            .iter()
            .any(|t| t.get("name").and_then(Value::as_str) == Some(tag.as_str()));
        if exists {
            toasts.push(json!({"title": "Duplicate tags.", "message": format!("Tag <span class='dark:text-warning'>{tag}</span> already added.")}));
            continue;
        }
        let known = team_tags(&state, res.team_id)
            .await?
            .iter()
            .any(|t| t.get("name").and_then(Value::as_str) == Some(tag.as_str()));
        parts::attach_tags(
            &state.db,
            res.team_id,
            &res.morph,
            res.id,
            std::slice::from_ref(&tag),
            state.now(),
        )
        .await?;
        if !known {
            crate::audit::mutation(
                &state,
                &ctx,
                &meta,
                "tag",
                "created",
                json!({"team_id": res.team_id, "tag_uuid": null, "tag_name": tag, "changed_fields": []}),
                json!({"name": {"old": null, "new": tag}}),
            )
            .await;
        }
    }
    Ok(ok(json!({"toasts": toasts})).into_response())
}

/// `Tags::addTag`.
async fn add_tag(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid, id)): Path<(String, String, i64)>,
) -> ApiResult {
    let res = find(&state, &ctx, &kind, &uuid).await?;
    if !ctx.is_team_admin() {
        return Ok(unauthorized());
    }
    let tag = team_tags(&state, res.team_id)
        .await?
        .into_iter()
        .find(|t| t.get("id").and_then(Value::as_i64) == Some(id));
    let Some(tag) = tag else {
        return Ok(toast_error(StatusCode::NOT_FOUND, "Tag not found."));
    };
    if parts::has_tag(&state.db, &res.morph, res.id, id).await? {
        return Ok(toast_error(StatusCode::CONFLICT, "Duplicate tags."));
    }
    let name = tag
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    parts::attach_tags(
        &state.db,
        res.team_id,
        &res.morph,
        res.id,
        &[name],
        state.now(),
    )
    .await?;
    Ok(ok(json!({"message": "Tag added."})).into_response())
}

/// `Tags::deleteTag`: the tag stays.
async fn remove_tag(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid, id)): Path<(String, String, i64)>,
) -> ApiResult {
    let res = find(&state, &ctx, &kind, &uuid).await?;
    if !ctx.is_team_admin() {
        return Ok(unauthorized());
    }
    parts::detach_tag(&state.db, &res.morph, res.id, id).await?;
    Ok(ok(json!({"message": "Tag removed."})).into_response())
}

// ---------------------------------------------------------------------------
// Webhooks
// ---------------------------------------------------------------------------

const PROVIDERS: [(&str, &str, &str); 4] = [
    (
        "github",
        "manual_webhook_secret_github",
        "GitHub webhook secret",
    ),
    (
        "gitlab",
        "manual_webhook_secret_gitlab",
        "GitLab webhook secret",
    ),
    (
        "bitbucket",
        "manual_webhook_secret_bitbucket",
        "Bitbucket webhook secret",
    ),
    (
        "gitea",
        "manual_webhook_secret_gitea",
        "Gitea webhook secret",
    ),
];
const SECRET_MIN_LENGTH: usize = 16;

fn secret_of(state: &AppState, res: &Res, column: &str) -> Option<String> {
    let cipher = res.str(column).filter(|c| !c.is_empty())?;
    state.encrypter.as_ref()?.decrypt_string(cipher).ok()
}

/// `$application->gitWebhook` for a repository without a source.
fn git_webhook(res: &Res) -> Option<String> {
    let repo = res.str("git_repository")?;
    let https = crate::git_urls::ssh_git_url_to_https(repo)
        .map(|u| u.strip_suffix(".git").map(str::to_owned).unwrap_or(u));
    Some(match https {
        Some(u) => format!("{u}/settings/hooks"),
        None => repo.to_owned(),
    })
}

async fn webhooks(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid)): P,
) -> ApiResult {
    let res = find(&state, &ctx, &kind, &uuid).await?;
    let base = coolify_jobs::notifications::Ctx::load(&state.db)
        .await
        .base_url;
    let can = ctx.is_team_admin();
    let source_id = res.int("source_id");
    let no_source = source_id.is_none() || source_id == Some(0);
    let manual = |t: &str| {
        (no_source && res.is_application())
            .then(|| format!("{base}/webhooks/source/{t}/events/manual"))
    };
    let source_type = res.str("source_type").unwrap_or_default();
    let mut secrets = Map::new();
    for (provider, column, _) in PROVIDERS {
        if can {
            secrets.insert(provider.to_owned(), json!(secret_of(&state, &res, column)));
        }
    }
    let api_enabled: bool =
        sqlx::query_scalar("SELECT is_api_enabled FROM instance_settings WHERE id = 0")
            .fetch_one(state.db.pool())
            .await
            .map_err(coolify_db::DbError::from)?;
    Ok(ok(json!({
        "is_application": res.is_application(),
        "git_based": res.git_based(),
        "can_update": can,
        "api_enabled": api_enabled,
        "deploy_webhook": format!("{base}/api/v1/deploy?uuid={}&force=false", res.uuid),
        "manual": {"github": manual("github"), "gitlab": manual("gitlab"), "bitbucket": manual("bitbucket"), "gitea": manual("gitea")},
        "secrets": secrets,
        "git_webhook": if res.is_application() { git_webhook(&res) } else { None },
        "gitlab_app_webhook": source_type.ends_with("GitlabApp").then(|| format!("{base}/webhooks/source/gitlab/events")),
        "gitlab_source_uuid": Value::Null,
    }))
    .into_response())
}

/// `Webhooks::submit`.
async fn save_webhooks(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path((kind, uuid)): P,
    Json(body): Json<Value>,
) -> ApiResult {
    let res = find(&state, &ctx, &kind, &uuid).await?;
    if !ctx.is_team_admin() {
        return Ok(unauthorized());
    }
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let mut errors = Map::new();
    let mut values: Vec<(&'static str, C)> = Vec::new();
    for (provider, column, label) in PROVIDERS {
        let new = body
            .get(provider)
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty());
        let current = secret_of(&state, &res, column);
        if new != current.as_deref() {
            if let Some(n) = new
                && n.chars().count() < SECRET_MIN_LENGTH
            {
                errors.insert(
                    provider.to_owned(),
                    json!([format!(
                        "The {label} must be at least {SECRET_MIN_LENGTH} characters."
                    )]),
                );
                continue;
            }
            values.push((column, C::Text(new.map(|n| enc.encrypt_string(n)))));
        }
    }
    if let Some(first) = errors
        .values()
        .next()
        .and_then(|v| v.get(0))
        .and_then(Value::as_str)
    {
        return Ok(PhpJson(
            StatusCode::UNPROCESSABLE_ENTITY,
            json!({"message": first, "errors": errors}),
        )
        .into_response());
    }
    save_model(&state, &ctx, &meta, &res, values, &Map::new()).await?;
    Ok(ok(json!({"message": "Webhook secrets saved."})).into_response())
}

/// `Webhooks::generateSecret`.
async fn generate_secret(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path((kind, uuid)): P,
    Json(body): Json<Value>,
) -> ApiResult {
    let res = find(&state, &ctx, &kind, &uuid).await?;
    if !ctx.is_team_admin() {
        return Ok(unauthorized());
    }
    let provider = body
        .get("provider")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let Some((_, column, label)) = PROVIDERS.iter().find(|(p, _, _)| *p == provider) else {
        return Ok(toast_error(
            StatusCode::BAD_REQUEST,
            "Unknown webhook provider.",
        ));
    };
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let secret = coolify_jobs::compose_parser::php::str_random(40);
    save_model(
        &state,
        &ctx,
        &meta,
        &res,
        vec![(column, C::Text(Some(enc.encrypt_string(&secret))))],
        &Map::new(),
    )
    .await?;
    Ok(
        ok(json!({"message": format!("New {label} generated and saved."), "secret": secret}))
            .into_response(),
    )
}
