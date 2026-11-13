//! `Webhook\Github::normal()` and `manual()` (with the GitHub Actions runners), and
//! `ProcessGithubPullRequestWebhook`.

use std::net::SocketAddr;

use axum::body::Bytes;
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, Uri};
use axum::response::Response;
use coolify_db::applications::ApplicationRow;
use coolify_db::records::Record;
use coolify_jobs::applications::previews::PullRequestStatus;
use serde_json::{Value, json};

use super::payload::{self as p, InvalidPayload, json_response, text_response};
use super::{
    audit_failure, audit_queued, client_address, ensure_preview, grouped_by_server, header,
    load_apps, manual_applications, manual_secret, name_of, queue_deployment, server_error,
    server_functional,
};
use crate::state::AppState;

const GITHUB_APP: &str = r"App\Models\GithubApp";

/// `readPullRequestPayload()`.
#[derive(Debug)]
struct PullRequest {
    action: String,
    pull_request_id: i64,
    html_url: String,
    title: Option<String>,
    branch: Option<String>,
    base_branch: Option<String>,
    before_sha: Option<String>,
    after_sha: Option<String>,
    commit_sha: String,
    is_fork: bool,
}

/// `isForkPullRequest()`.
fn is_fork(payload: &Value) -> Result<bool, InvalidPayload> {
    let head = p::payload_id(payload, "pull_request.head.repo.id", false, i64::MAX)?;
    let base = p::payload_id(payload, "pull_request.base.repo.id", false, i64::MAX)?;
    if let (Some(h), Some(b)) = (head, base) {
        return Ok(h != b);
    }
    if p::get(payload, "pull_request.head.repo.fork") == Some(&Value::Bool(true)) {
        return Ok(true);
    }
    let names = (
        p::get(payload, "pull_request.head.repo.full_name").and_then(Value::as_str),
        p::get(payload, "pull_request.base.repo.full_name").and_then(Value::as_str),
    );
    if let (Some(h), Some(b)) = names {
        return Ok(h.to_lowercase() != b.to_lowercase());
    }
    Ok(true)
}

fn read_pull_request(payload: &Value) -> Result<PullRequest, InvalidPayload> {
    let head = p::commit_sha(payload, "pull_request.head.sha")?;
    Ok(PullRequest {
        action: p::payload_string(payload, "action", true)?.unwrap_or_default(),
        pull_request_id: p::pull_request_id(payload, "number")?,
        html_url: p::payload_url(payload, "pull_request.html_url", true)?.unwrap_or_default(),
        title: p::payload_string(payload, "pull_request.title", false)?,
        branch: p::string_of(p::get(payload, "pull_request.head.ref")),
        base_branch: p::string_of(p::get(payload, "pull_request.base.ref")),
        before_sha: p::commit_sha(payload, "before")?,
        after_sha: p::commit_sha(payload, "after")?.or_else(|| head.clone()),
        commit_sha: head.unwrap_or_else(|| "HEAD".to_owned()),
        is_fork: is_fork(payload)?,
    })
}

/// `ProcessGithubPullRequestWebhook::handle()`.
async fn process_pull_request(
    state: AppState,
    a: ApplicationRow,
    source: Option<Record>,
    pr: PullRequest,
    full_name: String,
) {
    let Some(jobs) = state.jobs.as_ref() else {
        return;
    };
    if pr.action == "closed" || pr.action == "close" {
        if let Ok(Some(found)) = jobs.find_preview(a.id, pr.pull_request_id).await {
            jobs.update_pull_request(&a, &found, PullRequestStatus::Closed, None)
                .await;
            jobs.cleanup_preview(&a, pr.pull_request_id, &found).await;
        }
        return;
    }
    if !matches!(pr.action.as_str(), "opened" | "synchronize" | "reopened") {
        return;
    }
    if !a.setting_bool("is_preview_deployments_enabled") {
        return;
    }
    let enc = state.encrypter.as_ref();
    let head_message = match (source.as_ref(), enc) {
        (Some(s), Some(e)) => {
            coolify_jobs::github::commit_message(&state.db, e, s, &full_name, &pr.commit_sha).await
        }
        _ => None,
    };
    if p::should_skip_deploy_any(&[pr.title.as_deref(), head_message.as_deref()]) {
        return;
    }
    if pr.is_fork && !a.setting_bool("is_pr_deployments_public_enabled") {
        tracing::info!(application = %a.uuid, pull_request_id = pr.pull_request_id, action = %pr.action, "Skipped GitHub pull request preview deployment from a fork because public PR deployments are disabled.");
        return;
    }
    // `null`: the full list is unknown. Manual webhooks have no App: an empty list.
    let mut changed: Option<Vec<String>> = Some(Vec::new());
    if let (Some(s), Some(e)) = (source.as_ref(), enc) {
        if pr.action == "synchronize" {
            if let (Some(before), Some(after)) = (&pr.before_sha, &pr.after_sha) {
                changed = coolify_jobs::github::commit_range_files(
                    &state.db, e, s, &full_name, before, after,
                )
                .await;
            }
        } else {
            changed = coolify_jobs::github::pull_request_files(
                &state.db,
                e,
                s,
                &full_name,
                pr.pull_request_id,
            )
            .await;
        }
    }
    if a.str("watch_paths").is_some_and(|w| !w.trim().is_empty()) {
        match &changed {
            None => {
                tracing::warn!(application = %a.uuid, pull_request_id = pr.pull_request_id, "Could not get the full list of changed files from GitHub. Deploying the pull request preview without the watch paths check.");
            }
            Some(files) if !p::matches_watch_paths(&a, Some(files)) => {
                tracing::info!(application = %a.uuid, pull_request_id = pr.pull_request_id, "Skipped GitHub pull request preview deployment because no changed file matches the watch paths.");
                return;
            }
            Some(_) => {}
        }
    }
    if let Err(e) =
        ensure_preview(&state, &a, pr.pull_request_id, Some(&pr.html_url), "github").await
    {
        tracing::error!(error = %e, "preview could not be created");
        return;
    }
    if let Err(e) = queue_deployment(
        &state,
        &a,
        Some(&pr.commit_sha),
        pr.pull_request_id,
        Some("github"),
    )
    .await
    {
        tracing::error!(error = %e, "preview deployment could not be queued");
    }
}

/// `Webhook\Github::manual()`.
pub async fn manual(
    State(state): State<AppState>,
    peer: Option<axum::Extension<ConnectInfo<SocketAddr>>>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> Response {
    let event = header(&headers, "x-github-event").to_lowercase();
    if event == "ping" {
        return text_response("pong");
    }
    match manual_inner(
        &state,
        peer.as_ref().map(|e| &e.0),
        &headers,
        &uri,
        &body,
        &event,
    )
    .await
    {
        Ok(r) => r,
        Err(Failure::Invalid(e)) => text_response(e.0),
        Err(Failure::Other(e)) => server_error(&e),
    }
}

enum Failure {
    Invalid(InvalidPayload),
    Other(String),
}

impl From<InvalidPayload> for Failure {
    fn from(e: InvalidPayload) -> Self {
        Self::Invalid(e)
    }
}

impl From<String> for Failure {
    fn from(e: String) -> Self {
        Self::Other(e)
    }
}

async fn manual_inner(
    state: &AppState,
    peer: Option<&ConnectInfo<SocketAddr>>,
    headers: &HeaderMap,
    uri: &Uri,
    body: &Bytes,
    event: &str,
) -> Result<Response, Failure> {
    let signature = header(headers, "x-hub-signature-256");
    let signature = p::after_sha256(&signature).to_owned();
    let mut payload = p::collect(headers, uri, body);
    // `$content_type !== 'application/json'`: the exact header value.
    if header(headers, "content-type") != "application/json" {
        payload = p::form_payload(&payload);
    }
    let mut branch = None;
    let mut full_name_raw = None;
    let mut commit = None;
    let mut files = None;
    let mut skip_commits = false;
    let mut pr = None;
    if event == "push" {
        branch = p::push_branch(p::get(&payload, "ref"));
        full_name_raw = p::get(&payload, "repository.full_name").cloned();
        commit = p::commit_sha(&payload, "after")?;
        files = p::changed_files(&payload);
        skip_commits = p::should_skip_deploy(&p::commit_messages(&payload, "commits"));
    }
    if event == "pull_request" {
        let read = read_pull_request(&payload)?;
        full_name_raw = p::get(&payload, "repository.full_name").cloned();
        branch.clone_from(&read.branch);
        pr = Some(read);
    }
    if event != "push" && event != "pull_request" {
        return Ok(text_response(format!(
            "Nothing to do. Event '{event}' is not supported."
        )));
    }
    let base_missing = pr
        .as_ref()
        .is_some_and(|r| r.action != "closed" && r.base_branch.is_none());
    if branch.is_none() || base_missing {
        return Ok(text_response(
            "Nothing to do. No branch found in the request.",
        ));
    }
    if event == "push" && p::is_branch_deletion(&payload) {
        return Ok(text_response("Nothing to do. Branch deleted."));
    }
    let Some(full_name) = p::repository_full_name(full_name_raw.as_ref()) else {
        return Ok(text_response("Nothing to do. Invalid repository."));
    };
    let matched: Option<String> = match &pr {
        None => branch.clone(),
        Some(r) if r.action == "closed" => None,
        Some(r) => r.base_branch.clone(),
    };
    let key = p::failure_key(
        "github",
        &client_address(peer),
        &full_name,
        matched.as_deref(),
    );
    let attempt = p::signed_attempt(body, &signature);
    let applications = manual_applications(state, matched.as_deref(), &full_name).await?;
    if applications.is_empty() {
        return Ok(p::unauthenticated_response(&key, &attempt));
    }
    let mut out: Vec<Value> = Vec::new();
    for a in grouped_by_server(applications) {
        let Some(secret) = manual_secret(state, &a, "manual_webhook_secret_github") else {
            audit_failure("github", "webhook_secret_missing", &a.uuid);
            out.push(p::failure_payload());
            continue;
        };
        if !p::hash_equals(&signature, &p::hmac_hex(body, &secret)) {
            audit_failure("github", "invalid_signature", &a.uuid);
            out.push(p::failure_payload());
            continue;
        }
        crate::server_transfer::checklist::record_webhook("application", &a.uuid);
        if !server_functional(state, &a).await {
            out.push(json!({"application": name_of(&a), "status": "failed", "message": "Server is not functional."}));
            continue;
        }
        if event == "push" {
            if !a.setting_bool("is_auto_deploy_enabled") {
                out.push(json!({"status": "failed", "message": "Deployments disabled.", "application_uuid": a.uuid, "application_name": name_of(&a)}));
            } else if !p::matches_watch_paths(&a, files.as_deref()) {
                out.push(json!({
                    "status": "failed",
                    "message": "Changed files do not match watch paths. Ignoring deployment.",
                    "application_uuid": a.uuid,
                    "application_name": name_of(&a),
                    "details": {"changed_files": files, "watch_paths": p::watch_path_list(&a)},
                }));
            } else if skip_commits {
                out.push(json!({"application": name_of(&a), "status": "skipped", "message": "All commits contain [skip cd] or [skip ci]. Skipping deployment.", "application_uuid": a.uuid, "application_name": name_of(&a)}));
                continue;
            } else {
                let result = queue_deployment(state, &a, commit.as_deref(), 0, None).await?;
                if result.status == "queue_full" {
                    return Ok(p::queue_full(result.message));
                }
                if result.status == "skipped" {
                    out.push(json!({"application": name_of(&a), "status": "skipped", "message": result.message}));
                } else {
                    audit_queued("github", "manual", &a, result.deployment_uuid.as_deref());
                    out.push(json!({"application": name_of(&a), "status": "success", "message": "Deployment queued.", "application_uuid": a.uuid, "application_name": name_of(&a), "deployment_uuid": result.deployment_uuid}));
                }
            }
        }
        if let Some(r) = &pr {
            if !a.setting_bool("is_preview_deployments_enabled") && r.action != "closed" {
                out.push(json!({"application": name_of(&a), "status": "failed", "message": "Preview deployments disabled."}));
                continue;
            }
            spawn_process(state, &a, None, r, &full_name);
            out.push(json!({"application": name_of(&a), "status": "queued", "message": "PR webhook received, processing queued."}));
        }
    }
    Ok(p::manual_response(out, &key, &attempt))
}

/// `ProcessGithubPullRequestWebhook::dispatch(...)`.
fn spawn_process(
    state: &AppState,
    a: &ApplicationRow,
    source: Option<Record>,
    r: &PullRequest,
    full_name: &str,
) {
    let copy = PullRequest {
        action: r.action.clone(),
        pull_request_id: r.pull_request_id,
        html_url: r.html_url.clone(),
        title: r.title.clone(),
        branch: r.branch.clone(),
        base_branch: r.base_branch.clone(),
        before_sha: r.before_sha.clone(),
        after_sha: r.after_sha.clone(),
        commit_sha: r.commit_sha.clone(),
        is_fork: r.is_fork,
    };
    tokio::spawn(process_pull_request(
        state.clone(),
        a.clone(),
        source,
        copy,
        full_name.to_owned(),
    ));
}

async fn find_app(
    state: &AppState,
    target: &str,
    installation: Option<i64>,
) -> Result<Option<Record>, String> {
    // PostgreSQL refuses anything else for the integer column, upstream answers with a 500.
    let app_id = target
        .parse::<i32>()
        .map_err(|_| format!("invalid app id {target}"))?;
    if let Some(i) = installation {
        let row: Option<String> = sqlx::query_scalar(
            "SELECT row_to_json(s)::text FROM github_apps s WHERE app_id = $1 AND installation_id = $2 LIMIT 1",
        )
        .bind(app_id)
        .bind(i as i32)
        .fetch_optional(state.db.pool())
        .await
        .map_err(|e| e.to_string())?;
        if let Some(t) = row {
            return coolify_db::records::parse_row(&t)
                .map(Some)
                .map_err(|e| e.to_string());
        }
    }
    let row: Option<String> = sqlx::query_scalar(
        "SELECT row_to_json(s)::text FROM github_apps s WHERE app_id = $1 LIMIT 1",
    )
    .bind(app_id)
    .fetch_optional(state.db.pool())
    .await
    .map_err(|e| e.to_string())?;
    row.map(|t| coolify_db::records::parse_row(&t).map_err(|e| e.to_string()))
        .transpose()
}

/// `GithubAppPermissionJob::dispatch($github_app)`.
fn spawn_permissions(state: &AppState, source: Record) {
    let state = state.clone();
    tokio::spawn(async move {
        let Some(enc) = state.encrypter.as_ref() else {
            return;
        };
        let _ = coolify_jobs::github::sync_permissions(&state.db, enc, &source, state.now()).await;
    });
}

/// `Webhook\Github::normal()`.
pub async fn normal(
    State(state): State<AppState>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> Response {
    let event = header(&headers, "x-github-event").to_lowercase();
    if event == "ping" {
        return text_response("pong");
    }
    match normal_inner(&state, &headers, &uri, &body, &event).await {
        Ok(r) => r,
        Err(Failure::Invalid(e)) => text_response(e.0),
        Err(Failure::Other(e)) => server_error(&e),
    }
}

async fn normal_inner(
    state: &AppState,
    headers: &HeaderMap,
    uri: &Uri,
    body: &Bytes,
    event: &str,
) -> Result<Response, Failure> {
    let payload = p::collect(headers, uri, body);
    let target = header(headers, "x-github-hook-installation-target-id");
    let signature = header(headers, "x-hub-signature-256");
    let signature = p::after_sha256(&signature).to_owned();
    let installation = match p::get(&payload, "installation.id") {
        Some(Value::Number(n)) => n.as_i64(),
        Some(Value::String(s)) => s.trim().parse().ok(),
        _ => None,
    };
    let Some(source) = find_app(state, &target, installation).await? else {
        return Ok(text_response("Nothing to do. No GitHub App found."));
    };
    let secret = source
        .get("webhook_secret")
        .and_then(Value::as_str)
        .filter(|c| !c.is_empty())
        .and_then(|c| {
            state
                .encrypter
                .as_ref()
                .and_then(|e| e.decrypt_maybe_serialized(c).ok())
        })
        .filter(|s| !s.is_empty() && s != "0");
    let Some(secret) = secret else {
        audit_failure("github", "webhook_secret_missing", &target);
        return Ok(text_response("Invalid signature."));
    };
    if !p::hash_equals(&signature, &p::hmac_hex(body, &secret)) {
        audit_failure("github", "invalid_signature", &target);
        return Ok(text_response("Invalid signature."));
    }
    crate::server_transfer::checklist::record_webhook(
        "github_app",
        source
            .get("uuid")
            .and_then(Value::as_str)
            .unwrap_or_default(),
    );
    if event == "installation" || event == "installation_repositories" {
        if p::get(&payload, "action").and_then(Value::as_str) == Some("new_permissions_accepted") {
            spawn_permissions(state, source);
        }
        return Ok(text_response("cool"));
    }
    if event == "workflow_job" {
        return Ok(text_response(
            handle_workflow_job(state, &source, &payload).await?,
        ));
    }
    let mut id = None;
    let mut branch = None;
    let mut commit = None;
    let mut files = None;
    let mut skip_commits = false;
    let mut pr = None;
    let mut full_name = String::new();
    if event == "push" {
        id = p::database_id(&payload, "repository.id", false)?;
        branch = p::push_branch(p::get(&payload, "ref"));
        commit = p::commit_sha(&payload, "after")?;
        files = p::changed_files(&payload);
        skip_commits = p::should_skip_deploy(&p::commit_messages(&payload, "commits"));
    }
    if event == "pull_request" {
        id = p::database_id(&payload, "repository.id", false)?;
        let read = read_pull_request(&payload)?;
        branch.clone_from(&read.branch);
        let Some(name) = p::repository_full_name(p::get(&payload, "repository.full_name")) else {
            return Ok(text_response("Nothing to do. Invalid repository."));
        };
        full_name = name;
        pr = Some(read);
    }
    if event != "push" && event != "pull_request" {
        return Ok(text_response(format!(
            "Nothing to do. Event '{event}' is not supported."
        )));
    }
    let base_missing = pr
        .as_ref()
        .is_some_and(|r| r.action != "closed" && r.base_branch.is_none());
    let (Some(id), Some(branch_name)) = (id, branch.clone()) else {
        return Ok(text_response("Nothing to do. No id or branch found."));
    };
    if base_missing {
        return Ok(text_response("Nothing to do. No id or branch found."));
    }
    if event == "push" && p::is_branch_deletion(&payload) {
        return Ok(text_response("Nothing to do. Branch deleted."));
    }
    let source_id = source.get("id").and_then(Value::as_i64).unwrap_or(-1);
    let is_public = source.get("is_public").and_then(Value::as_bool) == Some(true);
    let filter_branch: Option<String> = match &pr {
        None => Some(branch_name.clone()),
        Some(r) if r.action == "closed" => None,
        Some(r) => r.base_branch.clone(),
    };
    let ids = if is_public {
        Vec::new()
    } else {
        super::application_ids(
            state,
            Some((source_id, GITHUB_APP)),
            Some(id),
            filter_branch.as_deref(),
        )
        .await?
    };
    let applications = load_apps(state, ids).await?;
    if applications.is_empty() {
        let shown = if pr.is_some() {
            pr.as_ref()
                .and_then(|r| r.base_branch.clone())
                .unwrap_or_default()
        } else {
            branch_name
        };
        return Ok(text_response(format!(
            "Nothing to do. No applications found with branch '{shown}'."
        )));
    }
    if event == "push"
        && applications
            .iter()
            .any(|a| a.str("watch_paths").is_some_and(|w| !w.is_empty()))
    {
        // githubPushComparedFiles(): the net change of the push, when GitHub can tell.
        let before = p::commit_sha(&payload, "before")?;
        let after = p::commit_sha(&payload, "after")?;
        let name = p::repository_full_name(p::get(&payload, "repository.full_name"));
        if let (Some(before), Some(after), Some(name), Some(enc)) =
            (before, after, name, state.encrypter.as_ref())
            && !before.bytes().all(|b| b == b'0')
            && let Some(compared) = coolify_jobs::github::commit_range_files(
                &state.db, enc, &source, &name, &before, &after,
            )
            .await
        {
            files = Some(compared);
        }
    }
    let mut out: Vec<Value> = Vec::new();
    for a in grouped_by_server(applications) {
        if !server_functional(state, &a).await {
            out.push(json!({"status": "failed", "message": "Server is not functional.", "application_uuid": a.uuid, "application_name": name_of(&a)}));
            continue;
        }
        if event == "push" {
            if !a.setting_bool("is_auto_deploy_enabled") {
                out.push(json!({"status": "failed", "message": "Deployments disabled.", "application_uuid": a.uuid, "application_name": name_of(&a)}));
            } else if !p::matches_watch_paths(&a, files.as_deref()) {
                out.push(json!({
                    "status": "failed",
                    "message": "Changed files do not match watch paths. Ignoring deployment.",
                    "application_uuid": a.uuid,
                    "application_name": name_of(&a),
                    "details": {"changed_files": files, "watch_paths": p::watch_path_list(&a)},
                }));
            } else if skip_commits {
                out.push(json!({"application": name_of(&a), "status": "skipped", "message": "All commits contain [skip cd] or [skip ci]. Skipping deployment.", "application_uuid": a.uuid, "application_name": name_of(&a)}));
                continue;
            } else {
                let result = queue_deployment(state, &a, commit.as_deref(), 0, None).await?;
                if result.status == "queue_full" {
                    return Ok(p::queue_full(result.message));
                }
                if result.status != "skipped" {
                    audit_queued("github", "app", &a, result.deployment_uuid.as_deref());
                }
                out.push(json!({"status": result.status, "message": result.message, "application_uuid": a.uuid, "application_name": name_of(&a), "deployment_uuid": result.deployment_uuid}));
            }
        }
        if let Some(r) = &pr {
            if !a.setting_bool("is_preview_deployments_enabled") && r.action != "closed" {
                out.push(json!({"application": name_of(&a), "status": "failed", "message": "Preview deployments disabled."}));
                continue;
            }
            spawn_process(state, &a, Some(source.clone()), r, &full_name);
            out.push(json!({"application": name_of(&a), "status": "queued", "message": "PR webhook received, processing queued."}));
        }
    }
    Ok(json_response(&Value::Array(out)))
}

/// `limitRunnerColumn`.
fn limit_runner_column(v: Option<&Value>) -> Option<String> {
    let text = match v? {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(true) => "1".to_owned(),
        Value::Bool(false) => String::new(),
        _ => return None,
    };
    Some(text.chars().take(255).collect())
}

/// `isPullRequestWorkflowJob`.
async fn is_pull_request_workflow_job(
    state: &AppState,
    source: &coolify_db::records::Record,
    payload: &Value,
) -> bool {
    let run_id = p::get(payload, "workflow_job.run_id")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let repository = p::get(payload, "repository.full_name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if run_id <= 0 || !repository.contains('/') {
        return false;
    }
    let Some(enc) = state.encrypter.as_ref() else {
        return false;
    };
    match coolify_jobs::server_ops::github_runner_jobs::workflow_run_event(
        &state.db, enc, source, repository, run_id,
    )
    .await
    {
        Ok(event) => coolify_jobs::server_ops::github_runner_jobs::PULL_REQUEST_EVENTS
            .contains(&event.as_str()),
        Err(_) => false,
    }
}

/// `handleWorkflowJob`: the response text.
async fn handle_workflow_job(
    state: &AppState,
    source: &coolify_db::records::Record,
    payload: &Value,
) -> Result<&'static str, Failure> {
    use coolify_db::github_runners as store;
    use coolify_jobs::server_ops::github_runner_jobs::{is_active, matches_labels};

    let action = p::get(payload, "action")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let job_id = p::get(payload, "workflow_job.id")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let runner_name = p::get(payload, "workflow_job.runner_name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if job_id <= 0 {
        return Ok("Nothing to do. No workflow job found.");
    }
    let app_id = source.get("id").and_then(Value::as_i64).unwrap_or(0);
    let own = source.get("installation_id").and_then(Value::as_i64);
    let got = p::get(payload, "installation.id").and_then(|v| {
        v.as_i64()
            .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
    });
    if own.is_none() || got.is_none() || own != got {
        return Ok("Nothing to do. The job belongs to another installation of the GitHub App.");
    }
    let details = store::JobDetails {
        workflow_job_id: job_id,
        workflow_job_html_url: p::get(payload, "workflow_job.html_url")
            .and_then(Value::as_str)
            .filter(|u| !u.is_empty() && u.chars().count() <= 255)
            .map(str::to_owned),
        workflow_name: limit_runner_column(p::get(payload, "workflow_job.workflow_name"))
            .filter(|s| !s.is_empty()),
        job_name: limit_runner_column(p::get(payload, "workflow_job.name"))
            .filter(|s| !s.is_empty()),
        repository_full_name: limit_runner_column(p::get(payload, "repository.full_name"))
            .filter(|s| !s.is_empty()),
    };
    let now = state.now();
    let jobs = state.jobs.as_ref();
    if action == "queued" {
        let labels: Vec<String> = p::get(payload, "workflow_job.labels")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|l| l.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        let matching: Vec<_> = store::enabled_configs(&state.db, app_id)
            .await?
            .into_iter()
            .filter(|c| matches_labels(&c.labels, &labels))
            .collect();
        if matching.is_empty() {
            return Ok("Nothing to do. No runner configuration matches the job labels.");
        }
        let is_pull_request = matching.iter().any(|c| !c.allow_pull_requests)
            && is_pull_request_workflow_job(state, source, payload).await;
        if is_pull_request && !matching.iter().any(|c| c.allow_pull_requests) {
            return Ok("Nothing to do. The runner configuration does not allow pull request jobs.");
        }
        let (id, created) = store::create_or_first(
            &state.db,
            app_id,
            job_id,
            &details,
            &labels,
            is_pull_request,
            now,
        )
        .await?;
        if created && let Some(j) = jobs {
            j.spawn_provision_runner(id, None);
        }
        return Ok("Runner queued.");
    }
    let execution = if runner_name.is_empty() {
        None
    } else {
        store::by_runner_name(&state.db, app_id, runner_name).await?
    };
    if (action == "in_progress" || action == "completed")
        && let Some(e) = &execution
        && (e.status == "provisioning" || e.status == "idle")
    {
        if let Some(j) = jobs
            && let Some(replacement) = j.assign_runner_job(e, job_id, &details).await
        {
            j.spawn_provision_runner(replacement, None);
        }
        if action == "in_progress" {
            return Ok("Runner marked running.");
        }
    }
    if action == "completed" {
        if let Some(e) = &execution {
            let conclusion = limit_runner_column(p::get(payload, "workflow_job.conclusion"));
            store::set_conclusion(&state.db, e.id, &details, conclusion.as_deref(), now).await?;
            let fresh = store::execution_by_id(&state.db, e.id).await?;
            let status = fresh.map(|f| f.status).unwrap_or_default();
            if is_active(&status) || status == "failed" {
                store::finish_execution(&state.db, e.id, "completed", "", now).await?;
                store::clear_error(&state.db, e.id).await?;
                if let Some(j) = jobs {
                    j.spawn_cleanup_runner(e.id);
                }
            }
            return Ok("Runner cleanup queued.");
        }
        if let Some(q) = store::by_trigger(&state.db, app_id, job_id).await?
            && q.status == "queued"
        {
            store::finish_execution(
                &state.db,
                q.id,
                "cancelled",
                "The job ended before a runner started.",
                now,
            )
            .await?;
        }
        return Ok("Queued runner cancelled.");
    }
    Ok("Nothing to do.")
}

impl From<coolify_db::DbError> for Failure {
    fn from(e: coolify_db::DbError) -> Self {
        Self::Other(e.to_string())
    }
}
