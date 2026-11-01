//! `Webhook\Bitbucket::manual()`.

use std::net::SocketAddr;

use axum::body::Bytes;
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, Uri};
use axum::response::Response;
use serde_json::{Value, json};

use super::payload::{self as p, InvalidPayload, json_response};
use super::{
    audit_failure, audit_queued, client_address, ensure_preview, header, manual_applications,
    manual_secret, name_of, pr_public, queue_deployment, server_error, server_functional,
};
use crate::state::AppState;

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

pub async fn manual(
    State(state): State<AppState>,
    peer: Option<axum::Extension<ConnectInfo<SocketAddr>>>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> Response {
    match inner(&state, peer.as_ref().map(|e| &e.0), &headers, &uri, &body).await {
        Ok(r) => r,
        Err(Failure::Invalid(e)) => json_response(&json!({"status": "failed", "message": e.0})),
        Err(Failure::Other(e)) => server_error(&e),
    }
}

async fn inner(
    state: &AppState,
    peer: Option<&ConnectInfo<SocketAddr>>,
    headers: &HeaderMap,
    uri: &Uri,
    body: &Bytes,
) -> Result<Response, Failure> {
    let payload = p::collect(headers, uri, body);
    let token = header(headers, "x-hub-signature");
    let event = header(headers, "x-event-key");
    const HANDLED: &[&str] = &[
        "repo:push",
        "pullrequest:updated",
        "pullrequest:created",
        "pullrequest:rejected",
        "pullrequest:fulfilled",
    ];
    if !HANDLED.contains(&event.as_str()) {
        return Ok(json_response(
            &json!({"status": "failed", "message": "Nothing to do. Event not handled."}),
        ));
    }
    let no_branch = || {
        json_response(
            &json!({"status": "failed", "message": "Nothing to do. No branch found in the request."}),
        )
    };
    let raw_name = p::get(&payload, "repository.full_name").cloned();
    let branch;
    let commit;
    let mut skip_commits = false;
    let mut pull_request = None;
    if event == "repo:push" {
        branch = p::string_of(p::get(&payload, "push.changes.0.new.name"));
        commit = p::commit_sha(&payload, "push.changes.0.new.target.hash")?;
        let messages: Vec<String> = p::get(&payload, "push.changes")
            .and_then(Value::as_array)
            .map(|changes| {
                changes
                    .iter()
                    .flat_map(|c| p::commit_messages(c, "commits"))
                    .collect()
            })
            .unwrap_or_default();
        skip_commits = p::should_skip_deploy(&messages);
    } else {
        branch = p::string_of(p::get(&payload, "pullrequest.destination.branch.name"));
        let id = p::pull_request_id(&payload, "pullrequest.id")?;
        let html = p::payload_url(&payload, "pullrequest.links.html.href", false)?;
        let title = p::payload_string(&payload, "pullrequest.title", false)?;
        commit = p::commit_sha(&payload, "pullrequest.source.commit.hash")?;
        pull_request = Some((id, html, p::should_skip_deploy_any(&[title.as_deref()])));
    }
    let Some(branch) = branch else {
        return Ok(no_branch());
    };
    let Some(full_name) = p::repository_full_name(raw_name.as_ref()) else {
        return Ok(json_response(
            &json!({"status": "failed", "message": "Nothing to do. Invalid repository."}),
        ));
    };
    let key = p::failure_key(
        "bitbucket",
        &client_address(peer),
        &full_name,
        Some(&branch),
    );
    let attempt = p::signed_attempt(body, &token);
    let closing = event == "pullrequest:rejected" || event == "pullrequest:fulfilled";
    let filter = (!closing).then_some(branch.as_str());
    let applications = manual_applications(state, filter, &full_name).await?;
    if applications.is_empty() {
        return Ok(p::unauthenticated_response(&key, &attempt));
    }
    let mut out: Vec<Value> = Vec::new();
    for a in applications {
        let Some(secret) = manual_secret(state, &a, "manual_webhook_secret_bitbucket") else {
            audit_failure("bitbucket", "webhook_secret_missing", &a.uuid);
            out.push(p::failure_payload());
            continue;
        };
        let Some(hash) = token.strip_prefix("sha256=") else {
            audit_failure("bitbucket", "malformed_signature", &a.uuid);
            out.push(p::failure_payload());
            continue;
        };
        if !p::hash_equals(hash, &p::hmac_hex(body, &secret)) {
            audit_failure("bitbucket", "invalid_signature", &a.uuid);
            out.push(p::failure_payload());
            continue;
        }
        crate::server_transfer::checklist::record_webhook("application", &a.uuid);
        if !server_functional(state, &a).await {
            out.push(json!({"application": name_of(&a), "status": "failed", "message": "Server is not functional."}));
            continue;
        }
        if event == "repo:push" {
            if !a.setting_bool("is_auto_deploy_enabled") {
                out.push(json!({"application": name_of(&a), "status": "failed", "message": "Auto deployment disabled."}));
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
                    audit_queued("bitbucket", "manual", &a, result.deployment_uuid.as_deref());
                    out.push(json!({"application": name_of(&a), "status": "success", "message": "Deployment queued."}));
                }
            }
        }
        if let Some((id, html, skip)) = &pull_request {
            if event == "pullrequest:created" || event == "pullrequest:updated" {
                if !a.setting_bool("is_preview_deployments_enabled") {
                    out.push(json!({"application": name_of(&a), "status": "failed", "message": "Preview deployments disabled."}));
                    continue;
                }
                if !p::preview_repository_trusted(
                    p::get(&payload, "pullrequest.source.repository.uuid"),
                    p::get(&payload, "pullrequest.destination.repository.uuid"),
                    p::get(&payload, "repository.uuid"),
                    pr_public(&a),
                ) {
                    continue;
                }
                if *skip {
                    out.push(json!({"application": name_of(&a), "status": "skipped", "message": "PR title contains [skip cd] or [skip ci]. Skipping preview deployment."}));
                    continue;
                }
                ensure_preview(state, &a, *id, html.as_deref(), "bitbucket").await?;
                let result =
                    queue_deployment(state, &a, commit.as_deref(), *id, Some("bitbucket")).await?;
                if result.status == "queue_full" {
                    return Ok(p::queue_full(result.message));
                }
                if result.status == "skipped" {
                    out.push(json!({"application": name_of(&a), "status": "skipped", "message": result.message}));
                } else {
                    out.push(json!({"application": name_of(&a), "status": "success", "message": "Preview deployment queued."}));
                }
            } else {
                let found = match state.jobs.as_ref() {
                    Some(jobs) => jobs.find_preview(a.id, *id).await?,
                    None => None,
                };
                if let (Some(found), Some(jobs)) = (found, state.jobs.as_ref()) {
                    jobs.cleanup_preview(&a, *id, &found).await;
                    out.push(json!({"application": name_of(&a), "status": "success", "message": "Preview deployment closed."}));
                } else {
                    out.push(json!({"application": name_of(&a), "status": "failed", "message": "No preview deployment found."}));
                }
            }
        }
    }
    Ok(p::manual_response(out, &key, &attempt))
}
