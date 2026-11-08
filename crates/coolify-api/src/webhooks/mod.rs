//! `routes/webhooks.php`, mounted under `/webhooks`, and what the handlers of
//! `Webhook\{Github,Gitlab,Gitea,Bitbucket}` share.

mod bitbucket;
mod gitea;
mod github;
mod gitlab;
mod payload;

use std::net::SocketAddr;

use axum::Router;
use axum::extract::ConnectInfo;
use axum::http::HeaderMap;
use axum::response::Response;
use axum::routing::{get, post};
use coolify_db::applications::{self as apps, ApplicationRow};
use coolify_jobs::applications::queue::{QueueOptions, Queued};
use coolify_jobs::server::ServerInfo;
use serde_json::{Value, json};

use crate::state::AppState;
use crate::ui_sources as s;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/webhooks/source/github/redirect", get(s::github_redirect))
        .route("/webhooks/source/github/install", get(s::github_install))
        .route("/webhooks/source/gitlab/redirect", get(s::gitlab_redirect))
        .route("/webhooks/source/gitea/redirect", get(s::gitea_redirect))
        .route("/webhooks/source/github/events", post(github::normal))
        .route(
            "/webhooks/source/github/events/manual",
            post(github::manual),
        )
        .route("/webhooks/source/gitlab/events", post(gitlab::normal))
        .route(
            "/webhooks/source/gitlab/events/manual",
            post(gitlab::manual),
        )
        .route(
            "/webhooks/source/bitbucket/events/manual",
            post(bitbucket::manual),
        )
        .route("/webhooks/source/gitea/events", post(gitea::normal))
        .route("/webhooks/source/gitea/events/manual", post(gitea::manual))
        .route(
            "/webhooks/payments/stripe/events",
            post(crate::ui_subscription::stripe_events),
        )
}

/// `$request->server('REMOTE_ADDR')`.
fn client_address(peer: Option<&ConnectInfo<SocketAddr>>) -> String {
    peer.map_or_else(|| "127.0.0.1".to_owned(), |c| c.0.ip().to_string())
}

/// `auditLogWebhookFailure($provider, $reason, $context)`.
pub(crate) fn audit_failure(provider: &str, reason: &str, detail: &str) {
    tracing::warn!(target: "audit", event = %format!("webhook.{provider}.signature_failed"), reason = %reason, detail = %detail, "webhook signature failed");
}

/// `handleError($e)` outside Livewire: the exception handler's answer.
fn server_error(message: &str) -> Response {
    tracing::error!(error = %message, "webhook failed");
    use axum::response::IntoResponse;
    (
        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        axum::Json(json!({"message": "Server Error"})),
    )
        .into_response()
}

/// The applications with the given ids, in the order given.
async fn load_apps(state: &AppState, ids: Vec<i64>) -> Result<Vec<ApplicationRow>, String> {
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        if let Some(a) = apps::find_by_id(&state.db, id)
            .await
            .map_err(|e| e.to_string())?
        {
            out.push(a);
        }
    }
    Ok(out)
}

/// `Application::query()` with the source and branch filters of the handlers.
async fn application_ids(
    state: &AppState,
    source: Option<(i64, &str)>,
    project_id: Option<i64>,
    branch: Option<&str>,
) -> Result<Vec<i64>, String> {
    query_application_ids(state, source, project_id, branch, None).await
}

/// `manualWebhookApplications()`'s SQL prefilter: every repository form that
/// `manualWebhookRepositoryMatches()` accepts contains the repository name, so the database
/// discards unrelated applications that share the branch. The full match stays in Rust.
fn repository_pattern(full_name: &str) -> String {
    format!("%{}%", full_name.to_lowercase().replace('_', "\\_"))
}

async fn query_application_ids(
    state: &AppState,
    source: Option<(i64, &str)>,
    project_id: Option<i64>,
    branch: Option<&str>,
    repository: Option<&str>,
) -> Result<Vec<i64>, String> {
    let mut sql = String::from("SELECT id FROM applications WHERE deleted_at IS NULL");
    let mut n = 0;
    let mut next = || {
        n += 1;
        n
    };
    if source.is_some() {
        let (a, b) = (next(), next());
        sql.push_str(&format!(" AND source_id = ${a} AND source_type = ${b}"));
    }
    if project_id.is_some() {
        sql.push_str(&format!(" AND repository_project_id = ${}", next()));
    }
    if branch.is_some() {
        sql.push_str(&format!(" AND git_branch = ${}", next()));
    }
    if repository.is_some() {
        sql.push_str(&format!(
            " AND LOWER(git_repository) LIKE ${} ESCAPE '\\'",
            next()
        ));
    }
    let mut q = sqlx::query_scalar::<_, i64>(&sql);
    if let Some((id, morph)) = source {
        q = q.bind(id).bind(morph);
    }
    if let Some(p) = project_id {
        q = q.bind(p as i32);
    }
    if let Some(b) = branch {
        q = q.bind(b);
    }
    if let Some(r) = repository {
        q = q.bind(repository_pattern(r));
    }
    q.fetch_all(state.db.pool())
        .await
        .map_err(|e| e.to_string())
}

/// `manualWebhookApplications($query, $fullName)`.
async fn manual_applications(
    state: &AppState,
    branch: Option<&str>,
    full_name: &str,
) -> Result<Vec<ApplicationRow>, String> {
    let ids = query_application_ids(state, None, None, branch, Some(full_name)).await?;
    Ok(load_apps(state, ids)
        .await?
        .into_iter()
        .filter(|a| payload::repository_matches(a.str("git_repository"), full_name))
        .collect())
}

/// `$application->destination->server->isFunctional()`.
async fn server_functional(state: &AppState, a: &ApplicationRow) -> bool {
    let Some(id) = a.server_id() else {
        return false;
    };
    match coolify_db::servers::find_by_id(&state.db, id).await {
        Ok(Some(row)) => ServerInfo::from_row(&row).is_functional(),
        _ => false,
    }
}

/// `$applications->groupBy(fn ($app) => $app->destination->server_id)`, flattened.
fn grouped_by_server(applications: Vec<ApplicationRow>) -> Vec<ApplicationRow> {
    let mut order: Vec<Option<i64>> = Vec::new();
    for a in &applications {
        let s = a.server_id();
        if !order.contains(&s) {
            order.push(s);
        }
    }
    let mut out = Vec::with_capacity(applications.len());
    for s in order {
        out.extend(applications.iter().filter(|a| a.server_id() == s).cloned());
    }
    out
}

/// A decrypted `manual_webhook_secret_*` column; the attribute cast leaves a plain value as is.
fn manual_secret(state: &AppState, a: &ApplicationRow, column: &str) -> Option<String> {
    let raw = a.str(column).filter(|s| !s.is_empty())?;
    let plain = state
        .encrypter
        .as_ref()
        .and_then(|e| {
            e.decrypt_string(raw)
                .or_else(|_| e.decrypt_maybe_serialized(raw))
                .ok()
        })
        .unwrap_or_else(|| raw.to_owned());
    (!plain.is_empty() && plain != "0").then_some(plain)
}

/// What `queue_application_deployment()` returned, as the handlers read it.
struct QueueResult {
    status: &'static str,
    message: &'static str,
    deployment_uuid: Option<String>,
}

/// `queue_application_deployment(application: ..., is_webhook: true, ...)`.
async fn queue_deployment(
    state: &AppState,
    a: &ApplicationRow,
    commit: Option<&str>,
    pull_request_id: i64,
    git_type: Option<&str>,
) -> Result<QueueResult, String> {
    let jobs = state
        .jobs
        .as_ref()
        .ok_or("Background jobs are not available.")?;
    let uuid = coolify_domain::new_public_id();
    let answer = jobs
        .queue_application_deployment(
            a,
            &uuid,
            QueueOptions {
                commit: Some(commit.unwrap_or("HEAD").to_owned()),
                is_webhook: true,
                pull_request_id,
                git_type: git_type.map(str::to_owned),
                ..Default::default()
            },
        )
        .await?;
    let message = answer.message();
    Ok(match answer {
        Queued::Queued { deployment_uuid } => QueueResult {
            status: "queued",
            message,
            deployment_uuid: Some(deployment_uuid),
        },
        Queued::Skipped { deployment_uuid } => QueueResult {
            status: "skipped",
            message,
            deployment_uuid: Some(deployment_uuid),
        },
        Queued::QueueFull => QueueResult {
            status: "queue_full",
            message,
            deployment_uuid: None,
        },
    })
}

/// `ApplicationPreview::create([...])` when the pull request has no preview yet.
async fn ensure_preview(
    state: &AppState,
    a: &ApplicationRow,
    pull_request_id: i64,
    html_url: Option<&str>,
    git_type: &str,
) -> Result<(), String> {
    let jobs = state
        .jobs
        .as_ref()
        .ok_or("Background jobs are not available.")?;
    if jobs.find_preview(a.id, pull_request_id).await?.is_none() {
        jobs.create_preview(a, pull_request_id, html_url.unwrap_or_default(), git_type)
            .await?;
    }
    Ok(())
}

/// The audit line of a queued webhook deployment.
fn audit_queued(provider: &str, mode: &str, a: &ApplicationRow, deployment: Option<&str>) {
    tracing::info!(target: "audit", event = "webhook.deployment.queued", provider = %provider, mode = %mode, application_uuid = %a.uuid, deployment_uuid = %deployment.unwrap_or_default(), "webhook deployment queued");
}

fn name_of(a: &ApplicationRow) -> Value {
    json!(a.str("name"))
}

fn pr_public(a: &ApplicationRow) -> bool {
    a.setting_bool("is_pr_deployments_public_enabled")
}

/// `X-…` header helper used by the handlers.
fn header(headers: &HeaderMap, name: &str) -> String {
    payload::header_str(headers, name)
}

#[cfg(test)]
mod tests {
    use super::{payload, repository_pattern};

    #[test]
    fn the_repository_prefilter_keeps_every_accepted_form() {
        assert_eq!(repository_pattern("Acme/My_Repo"), "%acme/my\\_repo%");
        // Every form `manualWebhookRepositoryMatches()` accepts contains the lowercase name.
        for url in [
            "https://github.com/Acme/My_Repo.git",
            "git@github.com:acme/my_repo.git",
            "acme/my_repo",
        ] {
            if payload::repository_matches(Some(url), "Acme/My_Repo") {
                assert!(url.to_lowercase().contains("acme/my_repo"), "{url}");
            }
        }
    }
}
