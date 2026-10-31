//! Session endpoints behind `Server\GithubRunners` and `Server\GithubRunnerExecutions`.
//!
//! Nothing here calls GitHub. Saving or enabling with a GitHub App that is ready for runners would call
//! `GithubRunnerApi::ensureRunnerGroup` upstream; this port answers with an error toast there, because neither the
//! API client nor provisioning is ported.

use axum::Extension;
use axum::Json;
use axum::Router;
use axum::extract::{Path, State};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use coolify_db::github_runners::{self as db, ConfigFields};
use coolify_db::servers::ServerRow;
use coolify_jobs::Jobs;
use coolify_jobs::server::ServerInfo;
use coolify_jobs::server_ops::{Srv, github_runners as runners};
use regex::Regex;
use serde_json::{Map, Value, json};

use crate::auth::AuthCtx;
use crate::input::Input;
use crate::response::{ApiError, ApiResult, ok};
use crate::routes::applications::docker_image_format;
use crate::routes::server_settings::{find_server, is_localhost};
use crate::state::AppState;
use crate::ui_server_destinations::toast_error;
use crate::ui_server_general::SERVER_PAGE;
use crate::validation::{Field, Rule, validate};

const UNAUTHORIZED: &str = "This action is unauthorized.";
const NEEDS_BUILD_ROLE: &str =
    "GitHub Actions runners need a server with the Builds only or Deployments and builds role.";
const DEFAULT_GROUP: &str = "Runners use the Default runner group of the organization. Check which repositories can use that group on GitHub.";
const NOT_READY: &str = "The GitHub App is not ready for runners. Coolify cannot take workflow jobs until you update its permissions on GitHub.";
/// `GithubRunners::RESERVED_LABELS`.
const RESERVED_LABELS: [&str; 7] = [
    "self-hosted",
    "linux",
    "x64",
    "arm64",
    "arm",
    "windows",
    "macos",
];

pub fn routes() -> Router<AppState> {
    let p = |s: &str| format!("{SERVER_PAGE}/github-runners{s}");
    Router::new()
        .route(&p(""), get(show).post(submit))
        .route(&p("/toggle"), post(toggle))
        .route(&p("/sysbox"), get(sysbox))
        .route(&p("/sysbox/install"), post(install_sysbox))
        .route(&p("/executions"), get(executions))
        .route(&p("/executions/{id}/cancel"), post(cancel))
}

fn role(s: &ServerRow) -> String {
    let settings = s.settings.clone().unwrap_or_default();
    match settings.get("server_role").and_then(Value::as_str) {
        Some(r) => r.to_string(),
        None if settings.get("is_build_server").and_then(Value::as_bool) == Some(true) => {
            "build".to_string()
        }
        None => "both".to_string(),
    }
}

/// `canBuildApplications`.
fn can_build(s: &ServerRow) -> bool {
    role(s) != "deployment"
}

fn str_of<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or_default()
}

/// `GithubApp::missingRunnerRequirements`.
fn missing_requirements(app: &Value) -> Vec<&'static str> {
    let mut missing = Vec::new();
    if str_of(app, "organization").trim().is_empty() {
        missing.push("An organization (runners are registered at organization level)");
    }
    if str_of(app, "organization_self_hosted_runners") != "write" {
        missing.push("Organization permission \"Self-hosted runners\": write");
    }
    if !matches!(str_of(app, "actions"), "read" | "write") {
        missing.push("Repository permission \"Actions\": read");
    }
    let events = app.get("webhook_events").and_then(Value::as_array);
    if !events.is_some_and(|e| e.iter().any(|x| x.as_str() == Some("workflow_job"))) {
        missing.push("Webhook event \"Workflow job\"");
    }
    missing
}

fn app_json(app: &Value) -> Value {
    json!({
        "id": app.get("id"),
        "uuid": app.get("uuid"),
        "name": app.get("name"),
        "organization": app.get("organization"),
        "html_url": app.get("html_url"),
        "missing": missing_requirements(app),
    })
}

/// `GithubRunners::mount`.
async fn show(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    let info = ServerInfo::from_row(&s);
    let apps = db::organization_apps(&state.db, info.team_id).await?;
    Ok(ok(json!({
        "is_localhost": is_localhost(&s),
        "can_build": can_build(&s),
        "is_build_server": role(&s) == "build",
        "apps": apps.iter().map(app_json).collect::<Vec<_>>(),
        "config": db::config(&state.db, s.id).await?,
    }))
    .into_response())
}

fn jobs(state: &AppState) -> Result<&Jobs, ApiError> {
    state.jobs.as_ref().ok_or(ApiError::NotPorted)
}

/// `parseLabels`.
fn parse_labels(labels: &str) -> Result<Vec<String>, ApiError> {
    let mut seen: Vec<String> = Vec::new();
    for label in labels.split(',') {
        let label = label.trim().to_lowercase();
        if !label.is_empty() && !seen.contains(&label) {
            seen.push(label);
        }
    }
    let error = |message: String| {
        let mut errors = Map::new();
        errors.insert("labels".into(), json!([message]));
        ApiError::Validation(errors)
    };
    let pattern = Regex::new(r"\A[a-z0-9][a-z0-9._-]{0,99}\z").map_err(|_| ApiError::Internal)?;
    for label in &seen {
        if !pattern.is_match(label) {
            return Err(error(format!(
                "The label \"{label}\" can only contain letters, numbers, dots, dashes, and underscores."
            )));
        }
    }
    let custom: Vec<String> = seen
        .into_iter()
        .filter(|l| !RESERVED_LABELS.contains(&l.as_str()))
        .collect();
    if custom.is_empty() {
        return Err(error(
            "Add at least one custom label, for example \"coolify\". Coolify only takes jobs that ask for a custom label."
                .to_string(),
        ));
    }
    Ok(custom)
}

fn cpu_rule(v: &Value) -> Option<&'static str> {
    let ok =
        Regex::new(r"\A\d+(\.\d+)?\z").is_ok_and(|p| v.as_str().is_some_and(|s| p.is_match(s)));
    (!ok).then_some("The :attribute field format is invalid.")
}

fn memory_rule(v: &Value) -> Option<&'static str> {
    let ok =
        Regex::new(r"(?i)\A\d+[bkmg]?\z").is_ok_and(|p| v.as_str().is_some_and(|s| p.is_match(s)));
    (!ok).then_some("The :attribute field format is invalid.")
}

const FIELDS: [&str; 12] = [
    "github_app_id",
    "labels",
    "max_runners",
    "docker_mode",
    "runner_image",
    "cpu_limit",
    "memory_limit",
    "capacity_wait_timeout",
    "idle_timeout",
    "job_timeout",
    "is_dedicated",
    "allow_pull_requests",
];

/// `GithubRunners::rules` with `validate()`.
fn check(input: &Input) -> Result<(), ApiError> {
    let int_range = |name: &'static str, max: i64| Field {
        name,
        rules: vec![
            Rule::Required,
            Rule::Integer,
            Rule::MinNum(1),
            Rule::MaxNum(max),
        ],
    };
    let fields = [
        Field {
            name: "github_app_id",
            rules: vec![Rule::Required, Rule::Integer],
        },
        Field {
            name: "labels",
            rules: vec![Rule::Required, Rule::String, Rule::Max(1000)],
        },
        int_range("max_runners", 32),
        Field {
            name: "docker_mode",
            rules: vec![Rule::Required, Rule::In(&["dind", "sysbox", "none"])],
        },
        Field {
            name: "runner_image",
            rules: vec![
                Rule::Nullable,
                Rule::String,
                Rule::Max(255),
                Rule::CustomOwned(docker_image_format),
            ],
        },
        Field {
            name: "cpu_limit",
            rules: vec![Rule::Nullable, Rule::String, Rule::Custom(cpu_rule)],
        },
        Field {
            name: "memory_limit",
            rules: vec![Rule::Nullable, Rule::String, Rule::Custom(memory_rule)],
        },
        int_range("capacity_wait_timeout", 1440),
        int_range("idle_timeout", 1440),
        int_range("job_timeout", 7200),
        Field {
            name: "is_dedicated",
            rules: vec![Rule::Required, Rule::Boolean],
        },
        Field {
            name: "allow_pull_requests",
            rules: vec![Rule::Required, Rule::Boolean],
        },
    ];
    validate(input, &fields, &[], &FIELDS)
}

fn number(input: &Input, k: &str) -> i64 {
    input
        .get(k)
        .and_then(|v| v.as_i64().or_else(|| v.as_str()?.parse().ok()))
        .unwrap_or_default()
}

/// `filled($value) ? $value : null`.
fn filled(input: &Input, k: &str) -> Option<String> {
    input
        .str(k)
        .filter(|v| !v.trim().is_empty())
        .map(str::to_string)
}

/// `prepareRunnerGroup`: its warning.
async fn prepare_group(state: &AppState, app: &Value) -> Result<Option<&'static str>, String> {
    if !missing_requirements(app).is_empty() {
        return Ok(Some(NOT_READY));
    }
    let id = app.get("id").and_then(Value::as_i64).unwrap_or_default();
    let source = coolify_db::git_sources::github_app(&state.db, id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or("GitHub App not found.")?;
    let enc = state
        .encrypter
        .as_ref()
        .ok_or("No application encryption key has been specified.")?;
    let (_, is_default) = coolify_jobs::server_ops::github_runner_jobs::ensure_runner_group(
        &state.db,
        enc,
        &source,
        state.now(),
    )
    .await?;
    Ok(is_default.then_some(DEFAULT_GROUP))
}

async fn do_submit(state: &AppState, s: &ServerRow, input: Input) -> ApiResult {
    check(&input)?;
    let team = ServerInfo::from_row(s).team_id;
    let apps = db::organization_apps(&state.db, team).await?;
    let app_id = number(&input, "github_app_id");
    let Some(app) = apps
        .iter()
        .find(|a| a.get("id").and_then(Value::as_i64) == Some(app_id))
    else {
        let mut errors = Map::new();
        errors.insert(
            "github_app_id".into(),
            json!(["The selected github app id is invalid."]),
        );
        return Err(ApiError::Validation(errors));
    };
    let labels = parse_labels(input.str("labels").unwrap_or_default())?;
    let mode = input.str("docker_mode").unwrap_or_default().to_string();
    if mode == "sysbox" {
        let srv = Srv::from_row(jobs(state)?, s.clone());
        if !runners::is_installed(&srv).await {
            let mut errors = Map::new();
            errors.insert(
                "docker_mode".into(),
                json!(["Sysbox is not installed on this server. Install it, then save again."]),
            );
            return Err(ApiError::Validation(errors));
        }
    }
    if !can_build(s) {
        return toast_error(NEEDS_BUILD_ROLE);
    }
    let existing = db::config(&state.db, s.id).await?;
    let enabled = existing
        .as_ref()
        .is_none_or(|c| c.get("is_enabled").and_then(Value::as_bool) != Some(false));
    let mut warning = None;
    if enabled {
        match prepare_group(state, app).await {
            Ok(w) => warning = w,
            Err(e) => return toast_error(e),
        }
    }
    let boolean = |k: &str| input.get(k).and_then(Value::as_bool).unwrap_or(false);
    let fields = ConfigFields {
        github_app_id: app_id,
        labels: serde_json::to_string(&labels).map_err(|_| ApiError::Internal)?,
        max_runners: number(&input, "max_runners"),
        docker_mode: mode,
        runner_image: filled(&input, "runner_image").map(|i| i.trim().to_string()),
        cpu_limit: filled(&input, "cpu_limit"),
        memory_limit: filled(&input, "memory_limit").map(|m| m.to_lowercase()),
        capacity_wait_timeout: number(&input, "capacity_wait_timeout"),
        idle_timeout: number(&input, "idle_timeout"),
        job_timeout: number(&input, "job_timeout"),
        is_dedicated: boolean("is_dedicated"),
        allow_pull_requests: boolean("allow_pull_requests"),
    };
    db::save_config(
        &state.db,
        &coolify_domain::new_public_id(),
        s.id,
        &fields,
        state.now(),
    )
    .await?;
    tracing::info!(target: "audit", event = if existing.is_some() { "ui.server.github_runners.updated" } else { "ui.server.github_runners.created" }, team_id = team, server_uuid = %s.uuid, "github runners saved");
    Ok(ok(json!({"message": "GitHub runner settings saved.", "warning": warning})).into_response())
}

/// `GithubRunners::submit`.
async fn submit(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    Json(body): Json<Map<String, Value>>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    if !ctx.is_team_admin() {
        return toast_error(UNAUTHORIZED);
    }
    do_submit(&state, &s, Input(body)).await
}

/// `GithubRunners::toggleEnabled`.
async fn toggle(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    Json(body): Json<Map<String, Value>>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    if !ctx.is_team_admin() {
        return toast_error(UNAUTHORIZED);
    }
    let Some(config) = db::config(&state.db, s.id).await? else {
        return do_submit(&state, &s, Input(body)).await;
    };
    let team = ServerInfo::from_row(&s).team_id;
    if config.get("is_enabled").and_then(Value::as_bool) == Some(true) {
        db::set_enabled(&state.db, s.id, false, state.now()).await?;
        tracing::info!(target: "audit", event = "ui.server.github_runners.disabled", team_id = team, server_uuid = %s.uuid, "github runners disabled");
        let config_id = config.get("id").and_then(Value::as_i64).unwrap_or_default();
        let j = jobs(&state)?;
        for (id, _uuid) in db::idle_executions(&state.db, config_id).await? {
            db::finish_execution(
                &state.db,
                id,
                "cancelled",
                "Runners were disabled on this server.",
                state.now(),
            )
            .await?;
            j.spawn_cleanup_runner(id);
        }
        return Ok(ok(
            json!({"message": "GitHub runners disabled. Running jobs continue until they finish."}),
        )
        .into_response());
    }
    if !can_build(&s) {
        return toast_error(NEEDS_BUILD_ROLE);
    }
    let app_id = config.get("github_app_id").and_then(Value::as_i64);
    let apps = db::organization_apps(&state.db, team).await?;
    let app = apps
        .iter()
        .find(|a| a.get("id").and_then(Value::as_i64) == app_id);
    let Some(app) = app else {
        return toast_error(UNAUTHORIZED);
    };
    let warning = match prepare_group(&state, app).await {
        Ok(w) => w,
        Err(e) => return toast_error(e),
    };
    db::set_enabled(&state.db, s.id, true, state.now()).await?;
    tracing::info!(target: "audit", event = "ui.server.github_runners.enabled", team_id = team, server_uuid = %s.uuid, "github runners enabled");
    Ok(ok(json!({"message": "GitHub runners enabled.", "warning": warning})).into_response())
}

/// `GithubRunners::checkSysbox`.
async fn sysbox(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    let srv = Srv::from_row(jobs(&state)?, s);
    Ok(ok(json!({"installed": runners::is_installed(&srv).await})).into_response())
}

/// `GithubRunners::installSysbox`.
async fn install_sysbox(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    if !ctx.is_team_admin() {
        return toast_error(UNAUTHORIZED);
    }
    let srv = Srv::from_row(jobs(&state)?, s);
    match runners::install_sysbox(&srv).await {
        Ok(activity) => Ok(ok(json!({"activity_id": activity})).into_response()),
        Err(e) => toast_error(e),
    }
}

async fn app_of(state: &AppState, s: &ServerRow) -> Result<Option<i64>, ApiError> {
    Ok(db::config(&state.db, s.id)
        .await?
        .and_then(|c| c.get("github_app_id").and_then(Value::as_i64)))
}

/// `GithubRunnerExecutions::executions`.
async fn executions(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    let app = app_of(&state, &s).await?;
    Ok(ok(json!({"executions": db::executions(&state.db, s.id, app).await?})).into_response())
}

/// `GithubRunnerExecutions::cancel`.
async fn cancel(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((uuid, id)): Path<(String, i64)>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    if !ctx.is_team_admin() {
        return toast_error(UNAUTHORIZED);
    }
    let app = app_of(&state, &s).await?;
    let Some((_uuid, status, server)) = db::execution(&state.db, s.id, app, id).await? else {
        return toast_error("Not found.");
    };
    if !matches!(
        status.as_str(),
        "queued" | "provisioning" | "idle" | "running"
    ) {
        return Ok(ok(json!({})).into_response());
    }
    db::finish_execution(
        &state.db,
        id,
        "cancelled",
        "Cancelled by a user.",
        state.now(),
    )
    .await?;
    if server.is_some() {
        jobs(&state)?.spawn_cleanup_runner(id);
    }
    Ok(ok(json!({"message": "Runner cancelled."})).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels() {
        let parsed = parse_labels("Coolify, linux, coolify, x_1").unwrap_or_default();
        assert_eq!(parsed, vec!["coolify".to_string(), "x_1".to_string()]);
        assert!(parse_labels("linux,x64").is_err());
        assert!(parse_labels("bad label").is_err());
    }
}
