//! `Webhook\Gitlab::normal()` and `manual()`.

use std::net::SocketAddr;

use axum::body::Bytes;
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use coolify_db::applications::ApplicationRow;
use serde_json::{Value, json};

use super::payload::{self as p, InvalidPayload, json_response};
use super::{
    audit_failure, audit_queued, client_address, ensure_preview, header, load_apps,
    manual_applications, manual_secret, name_of, pr_public, queue_deployment, server_error,
    server_functional,
};
use crate::state::AppState;

const GITLAB_APP: &str = r"App\Models\GitlabApp";
const OPEN_ACTIONS: &[&str] = &[
    "open",
    "opened",
    "synchronize",
    "reopened",
    "reopen",
    "update",
];
const CLOSE_ACTIONS: &[&str] = &["closed", "close", "merge"];

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

/// `readMergeRequestPayload()`.
struct MergeRequest {
    action: Option<String>,
    pull_request_id: i64,
    html_url: Option<String>,
    commit: Option<String>,
    skip_deploy: bool,
}

fn read_merge_request(payload: &Value) -> Result<MergeRequest, InvalidPayload> {
    let title = p::payload_string(payload, "object_attributes.title", false)?;
    let message = p::get(payload, "object_attributes.last_commit.message").and_then(Value::as_str);
    Ok(MergeRequest {
        action: p::payload_string(payload, "object_attributes.action", false)?,
        pull_request_id: p::pull_request_id(payload, "object_attributes.iid")?,
        html_url: p::payload_url(payload, "object_attributes.url", false)?,
        commit: p::commit_sha(payload, "object_attributes.last_commit.id")?,
        skip_deploy: p::should_skip_deploy_any(&[title.as_deref(), message]),
    })
}

fn unauthorized(message: &str) -> Response {
    (
        StatusCode::UNAUTHORIZED,
        axum::Json(json!({"status": "failed", "message": message})),
    )
        .into_response()
}

fn trusted(payload: &Value, a: &ApplicationRow) -> bool {
    p::preview_repository_trusted(
        p::get(payload, "object_attributes.source_project_id"),
        p::get(payload, "object_attributes.target_project_id"),
        p::get(payload, "project.id"),
        pr_public(a),
    )
}

/// `normalizeSystemHookPayload()`.
fn normalize_system_hook(payload: &mut Value) {
    let Value::Object(map) = payload else {
        return;
    };
    if !map.contains_key("object_kind")
        && let Some(name) = map.get("event_name").cloned()
    {
        map.insert("object_kind".into(), name);
    }
    let project_id = map.get("project_id").cloned();
    if let (Some(Value::Object(project)), Some(id)) = (map.get_mut("project"), project_id)
        && !project.contains_key("id")
    {
        let mut with_id = serde_json::Map::new();
        with_id.insert("id".into(), id);
        with_id.extend(std::mem::take(project));
        *project = with_id;
    }
}

/// `GitlabApp::findByWebhookToken($token)`: the rows whose decrypted token matches.
async fn find_by_token(state: &AppState, token: &str) -> Result<Option<i64>, String> {
    if token.is_empty() {
        return Ok(None);
    }
    let rows: Vec<(i64, Option<String>)> =
        sqlx::query_as("SELECT id, webhook_token FROM gitlab_apps")
            .fetch_all(state.db.pool())
            .await
            .map_err(|e| e.to_string())?;
    for (id, stored) in rows {
        let Some(stored) = stored.filter(|s| !s.is_empty()) else {
            continue;
        };
        // Legacy rows hold the token in plain text.
        let plain = state
            .encrypter
            .as_ref()
            .and_then(|e| e.decrypt_string(&stored).ok())
            .unwrap_or(stored);
        if !plain.is_empty() && p::hash_equals(&plain, token) {
            return Ok(Some(id));
        }
    }
    Ok(None)
}

/// `Webhook\Gitlab::normal()`.
pub async fn normal(
    State(state): State<AppState>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> Response {
    match normal_inner(&state, &headers, &uri, &body).await {
        Ok(r) => r,
        Err(Failure::Invalid(e)) => json_response(&json!({"status": "failed", "message": e.0})),
        Err(Failure::Other(e)) => server_error(&e),
    }
}

async fn normal_inner(
    state: &AppState,
    headers: &HeaderMap,
    uri: &Uri,
    body: &Bytes,
) -> Result<Response, Failure> {
    let mut payload = p::collect(headers, uri, body);
    if header(headers, "x-gitlab-event") == "System Hook" {
        normalize_system_hook(&mut payload);
    }
    let token = header(headers, "x-gitlab-token");
    let kind = p::string_of(p::get(&payload, "object_kind"));
    if !matches!(kind.as_deref(), Some("push" | "merge_request")) {
        return Ok(json_response(
            &json!({"status": "failed", "message": "Event not allowed. Only push and merge_request events are allowed."}),
        ));
    }
    let kind = kind.unwrap_or_default();
    if token.is_empty() || token == "0" {
        audit_failure("gitlab", "webhook_token_missing", &kind);
        return Ok(unauthorized("Missing X-Gitlab-Token header."));
    }
    let Some(source_id) = find_by_token(state, &token).await? else {
        audit_failure("gitlab", "invalid_token", &kind);
        return Ok(unauthorized("Invalid webhook token."));
    };
    let project_id = p::database_id(&payload, "project.id", true)?;
    let mut out: Vec<Value> = Vec::new();
    if kind == "push" {
        let Some(branch) = p::push_branch(p::get(&payload, "ref")) else {
            return Ok(json_response(
                &json!({"status": "failed", "message": "No branch found in the request."}),
            ));
        };
        if p::is_branch_deletion(&payload) {
            return Ok(json_response(
                &json!({"status": "skipped", "message": "Nothing to do. Branch deleted."}),
            ));
        }
        let commit = p::commit_sha(&payload, "after")?;
        let ids = super::application_ids(
            state,
            Some((source_id, GITLAB_APP)),
            project_id,
            Some(&branch),
        )
        .await?;
        let applications = load_apps(state, ids).await?;
        let files = p::changed_files(&payload);
        let skip = p::should_skip_deploy(&p::commit_messages(&payload, "commits"));
        for a in applications {
            if !server_functional(state, &a).await {
                out.push(json!({"application": name_of(&a), "status": "failed", "message": "Server is not functional"}));
                continue;
            }
            if !a.setting_bool("is_auto_deploy_enabled") {
                out.push(json!({"application": name_of(&a), "status": "failed", "message": "Deployments disabled"}));
                continue;
            }
            if !p::matches_watch_paths(&a, files.as_deref()) {
                out.push(json!({"application": name_of(&a), "status": "failed", "message": "Changed files do not match watch paths."}));
                continue;
            }
            if skip {
                out.push(json!({"application": name_of(&a), "status": "skipped", "message": "All commits contain [skip cd] or [skip ci]."}));
                continue;
            }
            let result = queue_deployment(state, &a, commit.as_deref(), 0, None).await?;
            if result.status == "queue_full" {
                return Ok(p::queue_full(result.message));
            }
            audit_queued("gitlab", "app", &a, result.deployment_uuid.as_deref());
            out.push(json!({"application": name_of(&a), "status": result.status, "message": result.message}));
        }
    }
    if kind == "merge_request" {
        let branch = p::string_of(p::get(&payload, "object_attributes.source_branch"));
        let base = p::string_of(p::get(&payload, "object_attributes.target_branch"));
        if branch.is_none() || base.is_none() {
            return Ok(json_response(
                &json!({"status": "failed", "message": "No branch found in the request."}),
            ));
        }
        let mr = read_merge_request(&payload)?;
        let action = mr.action.clone().unwrap_or_default();
        let filter =
            (!CLOSE_ACTIONS.contains(&action.as_str())).then(|| base.clone().unwrap_or_default());
        let ids = super::application_ids(
            state,
            Some((source_id, GITLAB_APP)),
            project_id,
            filter.as_deref(),
        )
        .await?;
        for a in load_apps(state, ids).await? {
            if !server_functional(state, &a).await {
                out.push(json!({"application": name_of(&a), "status": "failed", "message": "Server is not functional"}));
                continue;
            }
            if OPEN_ACTIONS.contains(&action.as_str()) {
                if !a.setting_bool("is_preview_deployments_enabled") {
                    out.push(json!({"application": name_of(&a), "status": "failed", "message": "Preview deployments disabled"}));
                    continue;
                }
                if !trusted(&payload, &a) {
                    continue;
                }
                if mr.skip_deploy {
                    out.push(json!({"application": name_of(&a), "status": "skipped", "message": "PR title or latest commit contains [skip cd] or [skip ci]."}));
                    continue;
                }
                ensure_preview(
                    state,
                    &a,
                    mr.pull_request_id,
                    mr.html_url.as_deref(),
                    "gitlab",
                )
                .await?;
                let result = queue_deployment(
                    state,
                    &a,
                    mr.commit.as_deref(),
                    mr.pull_request_id,
                    Some("gitlab"),
                )
                .await?;
                if result.status == "queue_full" {
                    return Ok(p::queue_full(result.message));
                }
                out.push(json!({"application": name_of(&a), "status": result.status, "message": result.message}));
            } else if CLOSE_ACTIONS.contains(&action.as_str())
                && let Some(jobs) = state.jobs.as_ref()
                && let Some(found) = jobs.find_preview(a.id, mr.pull_request_id).await?
            {
                jobs.cleanup_preview(&a, mr.pull_request_id, &found).await;
                out.push(json!({"application": name_of(&a), "status": "success", "message": "Preview deployment closed."}));
            }
        }
    }
    Ok(json_response(&Value::Array(out)))
}

/// `Webhook\Gitlab::manual()`.
pub async fn manual(
    State(state): State<AppState>,
    peer: Option<axum::Extension<ConnectInfo<SocketAddr>>>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> Response {
    match manual_inner(&state, peer.as_ref().map(|e| &e.0), &headers, &uri, &body).await {
        Ok(r) => r,
        Err(Failure::Invalid(e)) => json_response(&json!([{"status": "failed", "message": e.0}])),
        Err(Failure::Other(e)) => server_error(&e),
    }
}

async fn manual_inner(
    state: &AppState,
    peer: Option<&ConnectInfo<SocketAddr>>,
    headers: &HeaderMap,
    uri: &Uri,
    body: &Bytes,
) -> Result<Response, Failure> {
    let payload = p::collect(headers, uri, body);
    let token = header(headers, "x-gitlab-token");
    let event = p::string_of(p::get(&payload, "object_kind")).unwrap_or_default();
    if !matches!(event.as_str(), "push" | "merge_request") {
        return Ok(json_response(
            &json!([{"status": "failed", "message": "Event not allowed. Only push and merge_request events are allowed."}]),
        ));
    }
    if token.is_empty() || token == "0" {
        audit_failure("gitlab", "webhook_token_missing", &event);
        return Ok(json_response(&json!([p::failure_payload()])));
    }
    let mut branch = None;
    let mut base = None;
    let mut commit = None;
    let mut files = None;
    let mut skip_commits = false;
    let mut mr = None;
    let raw_name = p::get(&payload, "project.path_with_namespace").cloned();
    if event == "push" {
        branch = p::push_branch(p::get(&payload, "ref"));
        if branch.is_none() {
            return Ok(json_response(
                &json!([{"status": "failed", "message": "Nothing to do. No branch found in the request."}]),
            ));
        }
        if p::is_branch_deletion(&payload) {
            return Ok(json_response(
                &json!([{"status": "skipped", "message": "Nothing to do. Branch deleted."}]),
            ));
        }
        commit = p::commit_sha(&payload, "after")?;
        files = p::changed_files(&payload);
        skip_commits = p::should_skip_deploy(&p::commit_messages(&payload, "commits"));
    }
    if event == "merge_request" {
        branch = p::string_of(p::get(&payload, "object_attributes.source_branch"));
        base = p::string_of(p::get(&payload, "object_attributes.target_branch"));
        if branch.is_none() || base.is_none() {
            return Ok(json_response(
                &json!([{"status": "failed", "message": "Nothing to do. No branch found in the request."}]),
            ));
        }
        mr = Some(read_merge_request(&payload)?);
    }
    let Some(full_name) = p::repository_full_name(raw_name.as_ref()) else {
        return Ok(json_response(
            &json!([{"status": "failed", "message": "Nothing to do. Invalid repository."}]),
        ));
    };
    let matched = if event == "merge_request" {
        base.clone()
    } else {
        branch.clone()
    };
    let key = p::failure_key(
        "gitlab",
        &client_address(peer),
        &full_name,
        matched.as_deref(),
    );
    let attempt = p::token_attempt(&token);
    let action = mr
        .as_ref()
        .and_then(|m| m.action.clone())
        .unwrap_or_default();
    let filter = if event == "push" {
        branch.clone()
    } else if CLOSE_ACTIONS.contains(&action.as_str()) {
        None
    } else {
        base.clone()
    };
    let applications = manual_applications(state, filter.as_deref(), &full_name).await?;
    if applications.is_empty() {
        return Ok(p::unauthenticated_response(&key, &attempt));
    }
    let mut out: Vec<Value> = Vec::new();
    for a in applications {
        let Some(secret) = manual_secret(state, &a, "manual_webhook_secret_gitlab") else {
            audit_failure("gitlab", "webhook_secret_missing", &a.uuid);
            out.push(p::failure_payload());
            continue;
        };
        if !p::hash_equals(&secret, &token) {
            audit_failure("gitlab", "invalid_signature", &a.uuid);
            out.push(p::failure_payload());
            continue;
        }
        if !server_functional(state, &a).await {
            out.push(json!({"application": name_of(&a), "status": "failed", "message": "Server is not functional"}));
            continue;
        }
        if event == "push" {
            if !a.setting_bool("is_auto_deploy_enabled") {
                out.push(json!({"status": "failed", "message": "Deployments disabled", "application_uuid": a.uuid, "application_name": name_of(&a)}));
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
                    out.push(json!({"status": result.status, "message": result.message, "application_uuid": a.uuid, "application_name": name_of(&a)}));
                } else {
                    audit_queued("gitlab", "manual", &a, result.deployment_uuid.as_deref());
                    out.push(json!({"status": "success", "message": "Deployment queued.", "application_uuid": a.uuid, "application_name": name_of(&a)}));
                }
            }
        }
        if let Some(m) = &mr {
            if OPEN_ACTIONS.contains(&action.as_str()) {
                if !a.setting_bool("is_preview_deployments_enabled") {
                    out.push(json!({"application": name_of(&a), "status": "failed", "message": "Preview deployments disabled"}));
                    continue;
                }
                if !trusted(&payload, &a) {
                    continue;
                }
                if m.skip_deploy {
                    out.push(json!({"application": name_of(&a), "status": "skipped", "message": "PR title or latest commit contains [skip cd] or [skip ci]. Skipping preview deployment."}));
                    continue;
                }
                ensure_preview(
                    state,
                    &a,
                    m.pull_request_id,
                    m.html_url.as_deref(),
                    "gitlab",
                )
                .await?;
                let result = queue_deployment(
                    state,
                    &a,
                    m.commit.as_deref(),
                    m.pull_request_id,
                    Some("gitlab"),
                )
                .await?;
                if result.status == "queue_full" {
                    return Ok(p::queue_full(result.message));
                }
                if result.status == "skipped" {
                    out.push(json!({"application": name_of(&a), "status": "skipped", "message": result.message}));
                } else {
                    out.push(json!({"application": name_of(&a), "status": "success", "message": "Preview Deployment queued"}));
                }
            } else if CLOSE_ACTIONS.contains(&action.as_str()) {
                let found = match state.jobs.as_ref() {
                    Some(jobs) => jobs.find_preview(a.id, m.pull_request_id).await?,
                    None => None,
                };
                if let (Some(found), Some(jobs)) = (found, state.jobs.as_ref()) {
                    jobs.cleanup_preview(&a, m.pull_request_id, &found).await;
                    out.push(json!({"application": name_of(&a), "status": "success", "message": "Preview deployment closed."}));
                } else {
                    out.push(json!({"application": name_of(&a), "status": "failed", "message": "No preview deployment found."}));
                }
            } else {
                out.push(json!({"application": name_of(&a), "status": "failed", "message": "No action found. Contact us for debugging."}));
            }
        }
    }
    Ok(p::manual_response(out, &key, &attempt))
}
