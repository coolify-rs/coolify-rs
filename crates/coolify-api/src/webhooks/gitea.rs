//! `Webhook\Gitea::normal()` and `manual()`.

use std::net::SocketAddr;

use axum::body::Bytes;
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use coolify_db::applications::ApplicationRow;
use serde_json::{Value, json};

use super::payload::{self as p, InvalidPayload, json_response, text_response};
use super::{
    audit_failure, audit_queued, client_address, ensure_preview, header, load_apps,
    manual_applications, manual_secret, name_of, pr_public, queue_deployment, server_error,
    server_functional,
};
use crate::state::AppState;

const GITEA_APP: &str = r"App\Models\GiteaApp";

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

/// `readPullRequestPayload()`.
struct PullRequest {
    action: Option<String>,
    pull_request_id: i64,
    html_url: Option<String>,
    commit: Option<String>,
    skip_deploy: bool,
}

fn read_pull_request(payload: &Value) -> Result<PullRequest, InvalidPayload> {
    let title = p::payload_string(payload, "pull_request.title", false)?;
    Ok(PullRequest {
        action: p::payload_string(payload, "action", false)?,
        pull_request_id: p::pull_request_id(payload, "number")?,
        html_url: p::payload_url(payload, "pull_request.html_url", false)?,
        commit: match p::commit_sha(payload, "pull_request.head.sha")? {
            Some(c) => Some(c),
            None => p::commit_sha(payload, "after")?,
        },
        skip_deploy: p::should_skip_deploy_any(&[title.as_deref()]),
    })
}

fn trusted(payload: &Value, a: &ApplicationRow) -> bool {
    p::preview_repository_trusted(
        p::get(payload, "pull_request.head.repo.id"),
        p::get(payload, "pull_request.base.repo.id"),
        p::get(payload, "repository.id"),
        pr_public(a),
    )
}

/// `GiteaApp::findBySignature($payload, $signature)`.
async fn find_by_signature(
    state: &AppState,
    body: &[u8],
    signature: &str,
) -> Result<Option<i64>, String> {
    let rows: Vec<(i64, String)> = sqlx::query_as(
        "SELECT id, webhook_secret FROM gitea_apps WHERE webhook_secret IS NOT NULL",
    )
    .fetch_all(state.db.pool())
    .await
    .map_err(|e| e.to_string())?;
    let Some(enc) = state.encrypter.as_ref() else {
        return Ok(None);
    };
    for (id, stored) in rows {
        if let Ok(secret) = enc.decrypt_maybe_serialized(&stored)
            && !secret.is_empty()
            && p::hash_equals(&p::hmac_hex(body, &secret), signature)
        {
            return Ok(Some(id));
        }
    }
    Ok(None)
}

fn unauthorized(message: &str) -> Response {
    (
        StatusCode::UNAUTHORIZED,
        axum::Json(json!({"status": "failed", "message": message})),
    )
        .into_response()
}

/// `Webhook\Gitea::normal()`.
pub async fn normal(
    State(state): State<AppState>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> Response {
    let event = header(&headers, "x-gitea-event").to_lowercase();
    if event == "ping" {
        return text_response("pong");
    }
    match normal_inner(&state, &headers, &uri, &body, &event).await {
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
    event: &str,
) -> Result<Response, Failure> {
    if event != "push" && event != "pull_request" {
        return Ok(json_response(
            &json!({"status": "failed", "message": "Event not allowed. Only push and pull_request events are allowed."}),
        ));
    }
    let mut signature = header(headers, "x-gitea-signature");
    if signature.is_empty() {
        signature = p::after_sha256(&header(headers, "x-hub-signature-256")).to_owned();
    }
    if signature.is_empty() {
        audit_failure("gitea", "webhook_signature_missing", event);
        return Ok(unauthorized("Missing webhook signature."));
    }
    let Some(source_id) = find_by_signature(state, body, &signature).await? else {
        audit_failure("gitea", "invalid_signature", event);
        return Ok(unauthorized("Invalid webhook signature."));
    };
    let mut payload = p::collect(headers, uri, body);
    if !p::is_json(headers) {
        payload = p::form_payload(&payload);
    }
    let repository_id = p::database_id(&payload, "repository.id", true)?;
    let mut out: Vec<Value> = Vec::new();
    if event == "push" {
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
        let files = p::changed_files(&payload);
        let skip = p::should_skip_deploy(&p::commit_messages(&payload, "commits"));
        let ids = super::application_ids(
            state,
            Some((source_id, GITEA_APP)),
            repository_id,
            Some(&branch),
        )
        .await?;
        for a in load_apps(state, ids).await? {
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
            audit_queued("gitea", "app", &a, result.deployment_uuid.as_deref());
            out.push(json!({"application": name_of(&a), "status": result.status, "message": result.message}));
        }
    }
    if event == "pull_request" {
        let branch = p::string_of(p::get(&payload, "pull_request.head.ref"));
        let base = p::string_of(p::get(&payload, "pull_request.base.ref"));
        let (Some(_), Some(base)) = (branch, base) else {
            return Ok(json_response(
                &json!({"status": "failed", "message": "No branch found in the request."}),
            ));
        };
        let pr = read_pull_request(&payload)?;
        let action = pr.action.clone().unwrap_or_default();
        let ids = super::application_ids(
            state,
            Some((source_id, GITEA_APP)),
            repository_id,
            Some(&base),
        )
        .await?;
        for a in load_apps(state, ids).await? {
            if !server_functional(state, &a).await {
                out.push(json!({"application": name_of(&a), "status": "failed", "message": "Server is not functional"}));
                continue;
            }
            if matches!(action.as_str(), "opened" | "synchronized" | "reopened") {
                if !a.setting_bool("is_preview_deployments_enabled") {
                    out.push(json!({"application": name_of(&a), "status": "failed", "message": "Preview deployments disabled"}));
                    continue;
                }
                if !trusted(&payload, &a) {
                    continue;
                }
                if pr.skip_deploy {
                    out.push(json!({"application": name_of(&a), "status": "skipped", "message": "PR title contains [skip cd] or [skip ci]."}));
                    continue;
                }
                ensure_preview(
                    state,
                    &a,
                    pr.pull_request_id,
                    pr.html_url.as_deref(),
                    "gitea",
                )
                .await?;
                let result = queue_deployment(
                    state,
                    &a,
                    pr.commit.as_deref(),
                    pr.pull_request_id,
                    Some("gitea"),
                )
                .await?;
                if result.status == "queue_full" {
                    return Ok(p::queue_full(result.message));
                }
                out.push(json!({"application": name_of(&a), "status": result.status, "message": result.message}));
            } else if action == "closed"
                && let Some(jobs) = state.jobs.as_ref()
                && let Some(found) = jobs.find_preview(a.id, pr.pull_request_id).await?
            {
                jobs.cleanup_preview(&a, pr.pull_request_id, &found).await;
                out.push(json!({"application": name_of(&a), "status": "success", "message": "Preview deployment closed."}));
            }
        }
    }
    Ok(json_response(&Value::Array(out)))
}

/// `Webhook\Gitea::manual()`.
pub async fn manual(
    State(state): State<AppState>,
    peer: Option<axum::Extension<ConnectInfo<SocketAddr>>>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> Response {
    let event = header(&headers, "x-gitea-event").to_lowercase();
    if event == "ping" {
        return text_response("pong");
    }
    if event != "push" && event != "pull_request" {
        return text_response(format!("Nothing to do. Event '{event}' is not supported."));
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
    if header(headers, "content-type") != "application/json" {
        payload = p::form_payload(&payload);
    }
    let raw_name = p::get(&payload, "repository.full_name").cloned();
    let mut branch = None;
    let mut base = None;
    let mut commit = None;
    let mut files = None;
    let mut skip_commits = false;
    let mut pr = None;
    if event == "push" {
        branch = p::push_branch(p::get(&payload, "ref"));
        commit = p::commit_sha(&payload, "after")?;
        files = p::changed_files(&payload);
        skip_commits = p::should_skip_deploy(&p::commit_messages(&payload, "commits"));
    } else {
        branch = p::string_of(p::get(&payload, "pull_request.head.ref")).or(branch);
        base = p::string_of(p::get(&payload, "pull_request.base.ref"));
        pr = Some(read_pull_request(&payload)?);
    }
    if branch.is_none() || (event == "pull_request" && base.is_none()) {
        return Ok(text_response(
            "Nothing to do. No branch found in the request.",
        ));
    }
    if event == "push" && p::is_branch_deletion(&payload) {
        return Ok(text_response("Nothing to do. Branch deleted."));
    }
    let Some(full_name) = p::repository_full_name(raw_name.as_ref()) else {
        return Ok(text_response("Nothing to do. Invalid repository."));
    };
    let matched = if event == "pull_request" {
        base.clone()
    } else {
        branch.clone()
    };
    let key = p::failure_key(
        "gitea",
        &client_address(peer),
        &full_name,
        matched.as_deref(),
    );
    let attempt = p::signed_attempt(body, &signature);
    let action = pr
        .as_ref()
        .and_then(|m| m.action.clone())
        .unwrap_or_default();
    let filter = if event == "push" {
        branch.clone()
    } else if action == "closed" {
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
        let Some(secret) = manual_secret(state, &a, "manual_webhook_secret_gitea") else {
            audit_failure("gitea", "webhook_secret_missing", &a.uuid);
            out.push(p::failure_payload());
            continue;
        };
        if !p::hash_equals(&signature, &p::hmac_hex(body, &secret)) {
            audit_failure("gitea", "invalid_signature", &a.uuid);
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
                    audit_queued("gitea", "manual", &a, result.deployment_uuid.as_deref());
                    out.push(json!({"status": "success", "message": "Deployment queued.", "application_uuid": a.uuid, "application_name": name_of(&a)}));
                }
            }
        }
        if let Some(m) = &pr {
            if matches!(action.as_str(), "opened" | "synchronized" | "reopened") {
                if !a.setting_bool("is_preview_deployments_enabled") {
                    out.push(json!({"application": name_of(&a), "status": "failed", "message": "Preview deployments disabled."}));
                } else {
                    if !trusted(&payload, &a) {
                        continue;
                    }
                    if m.skip_deploy {
                        out.push(json!({"application": name_of(&a), "status": "skipped", "message": "PR title contains [skip cd] or [skip ci]. Skipping preview deployment."}));
                        continue;
                    }
                    ensure_preview(state, &a, m.pull_request_id, m.html_url.as_deref(), "gitea")
                        .await?;
                    let result = queue_deployment(
                        state,
                        &a,
                        m.commit.as_deref(),
                        m.pull_request_id,
                        Some("gitea"),
                    )
                    .await?;
                    if result.status == "queue_full" {
                        return Ok(p::queue_full(result.message));
                    }
                    if result.status == "skipped" {
                        out.push(json!({"application": name_of(&a), "status": "skipped", "message": result.message}));
                    } else {
                        out.push(json!({"application": name_of(&a), "status": "success", "message": "Preview deployment queued."}));
                    }
                }
            }
            if action == "closed" {
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
            }
        }
    }
    Ok(p::manual_response(out, &key, &attempt))
}
