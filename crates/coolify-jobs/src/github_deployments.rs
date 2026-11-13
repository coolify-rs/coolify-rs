//! `SyncGithubDeploymentJob`: mirrors the status of a deployment to a GitHub Deployment, so GitHub
//! shows the branch or pull request as deployed. Each run sends the current status, so a late or
//! repeated run converges to the same result.
//!
//! Upstream queues the job on the `default` queue (a slow GitHub must not delay deployments);
//! here it runs on its own tokio task. The entry points are
//! [`Jobs::sync_github_deployment`] (`SyncGithubDeploymentJob::dispatchFor($deployment)`, to be
//! called whenever the status of a deployment changed) and
//! [`Jobs::deactivate_github_deployments_of_pull_request`]
//! (`SyncGithubDeploymentJob::deactivatePullRequest($application, $pullRequestId)`).

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use coolify_db::applications::{self as apps, ApplicationRow, Deployment};
use coolify_db::records::Record;
use coolify_db::servers::ColumnValue;
use serde_json::{Map, Value, json};

use crate::Jobs;
use crate::github;

const GITHUB_MORPH: &str = r"App\Models\GithubApp";

/// `HTTP_TIMEOUT_SECONDS`: seconds that one request waits for GitHub.
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);

/// `$maxExceptions`: API errors are tried three times.
const MAX_EXCEPTIONS: u32 = 3;

/// `$backoff`.
const BACKOFF: Duration = Duration::from_secs(10);

type LockMap = HashMap<i64, Arc<tokio::sync::Mutex<()>>>;

/// `WithoutOverlapping("github-deployment:{$id}")`: all syncs of one deployment share this lock,
/// and a sync reads the state while it holds the lock. Thus a deactivation cannot happen between
/// that read and the requests.
static LOCKS: LazyLock<Mutex<LockMap>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn lock_of(deployment_queue_id: i64) -> Arc<tokio::sync::Mutex<()>> {
    LOCKS
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .entry(deployment_queue_id)
        .or_default()
        .clone()
}

fn forget_lock(deployment_queue_id: i64, lock: Arc<tokio::sync::Mutex<()>>) {
    let mut locks = LOCKS.lock().unwrap_or_else(|p| p.into_inner());
    // The map and this caller hold it: nobody waits for it.
    if Arc::strong_count(&lock) <= 2 {
        locks.remove(&deployment_queue_id);
    }
}

/// `githubState($status)`: the GitHub deployment state of an `ApplicationDeploymentStatus`.
pub fn github_state(status: &str) -> Option<&'static str> {
    match status {
        "in_progress" => Some("in_progress"),
        "finished" => Some("success"),
        "failed" => Some("failure"),
        "cancelled-by-user" => Some("error"),
        _ => None,
    }
}

/// `description($state)`.
pub fn description(state: &str) -> &'static str {
    match state {
        "in_progress" => "Deploying with Coolify.",
        "success" => "Deployed with Coolify.",
        "failure" => "The Coolify deployment failed.",
        "error" => "The Coolify deployment was cancelled.",
        _ => "The pull request was closed.",
    }
}

/// What one run of `handle()` does, decided from the row alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Nothing to send.
    Skip,
    /// Keep the `inactive` marker, so a late sync does not create a GitHub Deployment.
    MarkInactive,
    /// Create the GitHub Deployment, then send the state.
    CreateAndSend(&'static str),
    /// Send the state to the GitHub Deployment that exists.
    Send(&'static str),
}

/// The decisions of `SyncGithubDeploymentJob::handle()` after the source check.
pub fn next_step(
    status: &str,
    github_deployment_id: Option<i64>,
    github_deployment_state: Option<&str>,
    horizon_job_worker: Option<&str>,
    deactivate: bool,
) -> Step {
    // Inactive is the end state: a closed pull request must not show as deployed again.
    if github_deployment_state == Some("inactive") {
        return Step::Skip;
    }
    let state = if deactivate {
        Some("inactive")
    } else {
        github_state(status)
    };
    let Some(state) = state else {
        return Step::Skip;
    };
    if Some(state) == github_deployment_state {
        return Step::Skip;
    }
    if github_deployment_id.is_some_and(|id| id != 0) {
        return Step::Send(state);
    }
    if state == "inactive" {
        return Step::MarkInactive;
    }
    // The deployment ended before GitHub knew about it. The deployment job sets
    // horizon_job_worker together with IN_PROGRESS, so a failure without it never started.
    let never_started = horizon_job_worker.is_none_or(|w| w.trim().is_empty());
    if state == "error" || (state == "failure" && never_started) {
        return Step::Skip;
    }
    Step::CreateAndSend(state)
}

/// `Str::limit($value, $limit, '')` for the texts sent here.
fn limit(value: &str, max: usize) -> String {
    value.chars().take(max).collect()
}

/// The body of `POST repos/{repo}/deployments` (`createGithubDeployment()`).
pub fn deployment_body(application_name: &str, pull_request_id: i64, git_ref: &str) -> Value {
    let is_preview = pull_request_id != 0;
    let environment = if is_preview {
        format!("{application_name} (preview)")
    } else {
        application_name.to_owned()
    };
    let description = if is_preview {
        format!("Coolify preview for PR #{pull_request_id}")
    } else {
        "Coolify deployment".to_owned()
    };
    json!({
        "ref": git_ref,
        "environment": limit(&environment, 255),
        "description": limit(&description, 140),
        // GitHub would merge the default branch into the ref, or wait for all commit checks.
        "auto_merge": false,
        "required_contexts": [],
        "transient_environment": is_preview,
        "production_environment": !is_preview,
    })
}

/// The body of `POST repos/{repo}/deployments/{id}/statuses`: `array_filter()` drops the
/// environment URL unless the deployment succeeded and has one.
pub fn status_body(state: &str, log_url: &str, environment_url: Option<&str>) -> Value {
    let mut body = Map::new();
    body.insert("state".into(), json!(state));
    body.insert("description".into(), json!(description(state)));
    if !log_url.is_empty() {
        body.insert("log_url".into(), json!(log_url));
    }
    if state == "success"
        && let Some(url) = environment_url.filter(|u| !u.is_empty())
    {
        body.insert("environment_url".into(), json!(url));
    }
    Value::Object(body)
}

/// The deployed commit, when the deployment names one.
fn deployed_commit(commit: Option<&str>) -> Option<&str> {
    commit.filter(|c| !c.trim().is_empty() && *c != "HEAD")
}

/// `ref()` of a pull request deployment. GitHub links a deployment to a pull request only when
/// its ref is the head branch. A fork branch is not in the base repository, so a fork preview
/// uses the head commit. GitHub resolves a branch to its current commit, so the branch is used
/// only while it still points to the deployed commit.
pub fn pull_request_ref(commit: Option<&str>, pull_request: &Value, repository: &str) -> String {
    let commit = deployed_commit(commit);
    let text = |path: &str| crate::git_http::path_str(pull_request, path);
    let head_repository = text("head.repo.full_name").unwrap_or_default();
    let base_repository = text("base.repo.full_name").unwrap_or(repository);
    let head_branch = text("head.ref").filter(|b| !b.trim().is_empty());
    let head_commit = text("head.sha").unwrap_or_default();
    let is_deployed_commit = commit.is_none_or(|c| c.eq_ignore_ascii_case(head_commit));
    if let Some(branch) = head_branch
        && head_repository.eq_ignore_ascii_case(base_repository)
        && is_deployed_commit
    {
        return branch.to_owned();
    }
    commit.unwrap_or(head_commit).to_owned()
}

/// `ref()` of a deployment of the branch: the branch can move past the deployed commit before
/// the GitHub deployment is created.
pub fn branch_ref(commit: Option<&str>, git_branch: &str) -> String {
    deployed_commit(commit).unwrap_or(git_branch).to_owned()
}

/// The first domain of a `docker_compose_domains` value.
fn first_compose_domain(domains: Option<&str>) -> Option<String> {
    let parsed: Value = serde_json::from_str(domains?).ok()?;
    let items: Vec<&Value> = match &parsed {
        Value::Object(m) => m.values().collect(),
        Value::Array(a) => a.iter().collect(),
        _ => Vec::new(),
    };
    items
        .into_iter()
        .filter_map(|service| service.get("domain").and_then(Value::as_str))
        .find(|d| !d.is_empty())
        .map(str::to_owned)
}

/// `environmentUrl()`: the first URL of the application or of its preview, when it is one.
pub fn environment_url(
    build_pack: Option<&str>,
    fqdn: Option<&str>,
    docker_compose_domains: Option<&str>,
) -> Option<String> {
    let domains = if build_pack == Some("dockercompose") {
        first_compose_domain(docker_compose_domains)
    } else {
        fqdn.map(str::to_owned)
    };
    let url = domains
        .as_deref()
        .unwrap_or_default()
        .split(',')
        .next()
        .unwrap_or_default()
        .trim()
        .to_owned();
    (url.starts_with("http://") || url.starts_with("https://")).then_some(url)
}

/// `logUrl()`.
fn log_url(base_url: &str, a: &ApplicationRow, deployment_uuid: &str) -> String {
    let uuid_of = |r: &Record| {
        r.get("uuid")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    format!(
        "{base_url}/project/{}/environment/{}/application/{}/deployment/{deployment_uuid}",
        uuid_of(&a.project),
        uuid_of(&a.environment),
        a.uuid
    )
}

/// `$response['data']->get('id')` and friends: the exception of a failed answer.
fn checked(answer: github::ApiAnswer) -> Result<Value, String> {
    answer.successful()?;
    Ok(answer.data)
}

impl Jobs {
    /// The GitHub App of an application when it can write deployments
    /// (`$source instanceof GithubApp && $source->canSyncDeployments()`).
    async fn deployments_source(&self, a: &ApplicationRow) -> Option<Record> {
        if a.str("source_type") != Some(GITHUB_MORPH) {
            return None;
        }
        let source =
            coolify_db::git_sources::source_row(&self.0.db, "github_apps", a.int("source_id")?)
                .await
                .ok()
                .flatten()?;
        github::can_sync_deployments(&source).then_some(source)
    }

    /// `SyncGithubDeploymentJob::dispatchFor($deployment)`: queues a sync when the deployment
    /// belongs to a GitHub App that can write deployments. Call it after every change of the
    /// status of a row of `application_deployment_queues`. It never fails: a failed GitHub sync
    /// must not stop the status transition or the deployment queue.
    pub fn sync_github_deployment(&self, deployment_queue_id: i64) {
        let jobs = self.clone();
        tokio::spawn(async move {
            let Ok(Some(deployment)) =
                apps::deployment_by_id(&jobs.0.db, deployment_queue_id).await
            else {
                return;
            };
            // Deployments to additional servers are part of the main deployment.
            if deployment
                .str("parent_deployment_uuid")
                .is_some_and(|p| !p.trim().is_empty())
            {
                return;
            }
            // A soft-deleted application still needs its last status on GitHub.
            let Ok(Some(application)) =
                apps::find_with_trashed(&jobs.0.db, deployment.application_id).await
            else {
                return;
            };
            if jobs.deployments_source(&application).await.is_none() {
                return;
            }
            jobs.run_github_deployment_sync(deployment_queue_id, false)
                .await;
        });
    }

    /// `SyncGithubDeploymentJob::deactivatePullRequest($application, $pullRequestId)`: marks the
    /// GitHub Deployments of a closed pull request as inactive. This also covers deployments that
    /// GitHub does not know yet, so a late sync cannot create one.
    pub fn deactivate_github_deployments_of_pull_request(
        &self,
        application_id: i64,
        pull_request_id: i64,
    ) {
        let jobs = self.clone();
        tokio::spawn(async move {
            let ids: Vec<i64> = sqlx::query_scalar(
                "SELECT id FROM application_deployment_queues WHERE application_id = $1 AND pull_request_id = $2 \
                 AND (github_deployment_state IS NULL OR github_deployment_state != 'inactive') ORDER BY id",
            )
            .bind(application_id.to_string())
            .bind(pull_request_id as i32)
            .fetch_all(jobs.0.db.pool())
            .await
            .unwrap_or_default();
            for id in ids {
                let jobs = jobs.clone();
                tokio::spawn(async move {
                    jobs.run_github_deployment_sync(id, true).await;
                });
            }
        });
    }

    /// One queued `SyncGithubDeploymentJob`: tried again after an API error, three times at most.
    async fn run_github_deployment_sync(&self, deployment_queue_id: i64, deactivate: bool) {
        let lock = lock_of(deployment_queue_id);
        let mut last_error = None;
        for attempt in 1..=MAX_EXCEPTIONS {
            let result = {
                let _held = lock.lock().await;
                // `$timeout = 100`: longer than the worst case of all requests with their retries.
                tokio::time::timeout(
                    Duration::from_secs(100),
                    self.sync_github_deployment_once(deployment_queue_id, deactivate),
                )
                .await
                .unwrap_or_else(|_| Err("The GitHub deployment sync timed out.".to_owned()))
            };
            match result {
                Ok(()) => {
                    last_error = None;
                    break;
                }
                Err(e) => last_error = Some(e),
            }
            if attempt < MAX_EXCEPTIONS {
                tokio::time::sleep(BACKOFF).await;
            }
        }
        forget_lock(deployment_queue_id, lock);
        // `failed()`.
        if let Some(error) = last_error {
            tracing::warn!(
                deployment_queue_id,
                error = %error,
                "Failed to sync the GitHub deployment."
            );
        }
    }

    /// `SyncGithubDeploymentJob::handle()`.
    pub async fn sync_github_deployment_once(
        &self,
        deployment_queue_id: i64,
        deactivate: bool,
    ) -> Result<(), String> {
        let db = &self.0.db;
        let Some(deployment) = apps::deployment_by_id(db, deployment_queue_id)
            .await
            .map_err(|e| e.to_string())?
        else {
            return Ok(());
        };
        let Some(application) = apps::find_with_trashed(db, deployment.application_id)
            .await
            .map_err(|e| e.to_string())?
        else {
            return Ok(());
        };
        let Some(source) = self.deployments_source(&application).await else {
            return Ok(());
        };
        let step = next_step(
            &deployment.status,
            deployment.int("github_deployment_id"),
            deployment.str("github_deployment_state"),
            deployment.str("horizon_job_worker"),
            deactivate,
        );
        let now = Self::now();
        let state = match step {
            Step::Skip => return Ok(()),
            Step::MarkInactive => {
                return apps::update_deployment(
                    db,
                    deployment.id,
                    &[(
                        "github_deployment_state",
                        ColumnValue::Text(Some("inactive".into())),
                    )],
                    now,
                )
                .await
                .map_err(|e| e.to_string());
            }
            Step::CreateAndSend(state) | Step::Send(state) => state,
        };
        let repository =
            github::git_repository_slug(application.str("git_repository").unwrap_or_default());
        let github_deployment_id = match step {
            Step::Send(_) => deployment.int("github_deployment_id").unwrap_or_default(),
            _ => {
                let id = self
                    .create_github_deployment(&deployment, &application, &source, &repository)
                    .await?;
                // The id is a 64-bit number: GitHub's deployment ids do not fit a 32-bit integer.
                sqlx::query(
                    "UPDATE application_deployment_queues SET github_deployment_id = $1, updated_at = $2 WHERE id = $3",
                )
                .bind(id)
                .bind(now)
                .bind(deployment.id)
                .execute(db.pool())
                .await
                .map_err(|e| e.to_string())?;
                id
            }
        };
        let base_url = crate::notifications::Ctx::load(db).await.base_url;
        let environment_url = if state == "success" {
            self.github_environment_url(&deployment, &application).await
        } else {
            None
        };
        let body = status_body(
            state,
            &log_url(&base_url, &application, &deployment.uuid),
            environment_url.as_deref(),
        );
        let endpoint = format!("repos/{repository}/deployments/{github_deployment_id}/statuses");
        checked(
            github::api_request(
                db,
                self.0.encrypter.as_ref(),
                &source,
                reqwest::Method::POST,
                &endpoint,
                Some(&body),
                Some(HTTP_TIMEOUT),
            )
            .await?,
        )?;
        apps::update_deployment(
            db,
            deployment.id,
            &[(
                "github_deployment_state",
                ColumnValue::Text(Some(state.to_owned())),
            )],
            Self::now(),
        )
        .await
        .map_err(|e| e.to_string())
    }

    /// `createGithubDeployment()`: the id of the new GitHub Deployment.
    async fn create_github_deployment(
        &self,
        deployment: &Deployment,
        application: &ApplicationRow,
        source: &Record,
        repository: &str,
    ) -> Result<i64, String> {
        let db = &self.0.db;
        let enc = self.0.encrypter.as_ref();
        let pull_request_id = deployment.int("pull_request_id").unwrap_or(0);
        let commit = deployment.str("commit");
        let git_ref = if pull_request_id != 0 {
            let endpoint = format!("repos/{repository}/pulls/{pull_request_id}");
            let pull_request = checked(
                github::api_request(
                    db,
                    enc,
                    source,
                    reqwest::Method::GET,
                    &endpoint,
                    None,
                    Some(HTTP_TIMEOUT),
                )
                .await?,
            )?;
            pull_request_ref(commit, &pull_request, repository)
        } else {
            branch_ref(commit, application.str("git_branch").unwrap_or_default())
        };
        let body = deployment_body(
            application.str("name").unwrap_or_default(),
            pull_request_id,
            &git_ref,
        );
        let endpoint = format!("repos/{repository}/deployments");
        let created = checked(
            github::api_request(
                db,
                enc,
                source,
                reqwest::Method::POST,
                &endpoint,
                Some(&body),
                Some(HTTP_TIMEOUT),
            )
            .await?,
        )?;
        created
            .get("id")
            .and_then(Value::as_i64)
            .filter(|id| *id != 0)
            .ok_or_else(|| "GitHub did not return the id of the deployment.".to_owned())
    }

    /// `environmentUrl()`.
    async fn github_environment_url(
        &self,
        deployment: &Deployment,
        application: &ApplicationRow,
    ) -> Option<String> {
        let build_pack = application.str("build_pack");
        let pull_request_id = deployment.int("pull_request_id").unwrap_or(0);
        if pull_request_id == 0 {
            return environment_url(
                build_pack,
                application.str("fqdn"),
                application.str("docker_compose_domains"),
            );
        }
        let preview = self
            .find_preview(application.id, pull_request_id)
            .await
            .ok()
            .flatten()?;
        let text = |k: &str| preview.get(k).and_then(Value::as_str);
        environment_url(build_pack, text("fqdn"), text("docker_compose_domains"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deployment_statuses_map_to_github_states() {
        assert_eq!(github_state("in_progress"), Some("in_progress"));
        assert_eq!(github_state("finished"), Some("success"));
        assert_eq!(github_state("failed"), Some("failure"));
        assert_eq!(github_state("cancelled-by-user"), Some("error"));
        assert_eq!(github_state("queued"), None);
        assert_eq!(description("in_progress"), "Deploying with Coolify.");
        assert_eq!(description("success"), "Deployed with Coolify.");
        assert_eq!(description("failure"), "The Coolify deployment failed.");
        assert_eq!(
            description("error"),
            "The Coolify deployment was cancelled."
        );
        assert_eq!(description("inactive"), "The pull request was closed.");
    }

    #[test]
    fn a_started_deployment_creates_a_github_deployment() {
        assert_eq!(
            next_step("in_progress", None, None, Some("worker"), false),
            Step::CreateAndSend("in_progress")
        );
        // The next status goes to the deployment GitHub knows.
        assert_eq!(
            next_step(
                "finished",
                Some(7),
                Some("in_progress"),
                Some("worker"),
                false
            ),
            Step::Send("success")
        );
        // A repeated run sends nothing.
        assert_eq!(
            next_step("finished", Some(7), Some("success"), Some("worker"), false),
            Step::Skip
        );
        // A queued deployment has no GitHub state.
        assert_eq!(next_step("queued", None, None, None, false), Step::Skip);
    }

    #[test]
    fn deployments_that_ended_before_github_knew_them_are_not_created() {
        // Cancelled while queued.
        assert_eq!(
            next_step("cancelled-by-user", None, None, None, false),
            Step::Skip
        );
        // Failed before the deployment job started it.
        assert_eq!(next_step("failed", None, None, None, false), Step::Skip);
        assert_eq!(next_step("failed", None, None, Some(""), false), Step::Skip);
        // A started deployment whose in_progress sync was lost still reports its failure.
        assert_eq!(
            next_step("failed", None, None, Some("worker"), false),
            Step::CreateAndSend("failure")
        );
        assert_eq!(
            next_step("finished", None, None, None, false),
            Step::CreateAndSend("success")
        );
        // A cancel after GitHub knew the deployment is sent.
        assert_eq!(
            next_step(
                "cancelled-by-user",
                Some(7),
                Some("in_progress"),
                None,
                false
            ),
            Step::Send("error")
        );
    }

    #[test]
    fn inactive_is_the_end_state() {
        // A closed pull request: the deployment GitHub knows becomes inactive.
        assert_eq!(
            next_step("finished", Some(7), Some("success"), Some("worker"), true),
            Step::Send("inactive")
        );
        // GitHub does not know it yet: only the marker is kept.
        assert_eq!(
            next_step("queued", None, None, None, true),
            Step::MarkInactive
        );
        // A late sync of an inactive deployment does nothing, also a deactivation.
        assert_eq!(
            next_step("finished", None, Some("inactive"), Some("worker"), false),
            Step::Skip
        );
        assert_eq!(
            next_step("finished", Some(7), Some("inactive"), Some("worker"), true),
            Step::Skip
        );
    }

    #[test]
    fn deployment_bodies() {
        assert_eq!(
            deployment_body("web", 0, "abc123"),
            json!({
                "ref": "abc123", "environment": "web", "description": "Coolify deployment",
                "auto_merge": false, "required_contexts": [], "transient_environment": false,
                "production_environment": true,
            })
        );
        assert_eq!(
            deployment_body("web", 12, "feature"),
            json!({
                "ref": "feature", "environment": "web (preview)", "description": "Coolify preview for PR #12",
                "auto_merge": false, "required_contexts": [], "transient_environment": true,
                "production_environment": false,
            })
        );
        let long = "n".repeat(300);
        let body = deployment_body(&long, 0, "main");
        assert_eq!(body["environment"].as_str().map(str::len), Some(255));
    }

    #[test]
    fn status_bodies_drop_empty_values() {
        assert_eq!(
            status_body(
                "in_progress",
                "https://c.test/log",
                Some("https://app.test")
            ),
            json!({"state": "in_progress", "description": "Deploying with Coolify.", "log_url": "https://c.test/log"})
        );
        assert_eq!(
            status_body("success", "https://c.test/log", Some("https://app.test")),
            json!({"state": "success", "description": "Deployed with Coolify.", "log_url": "https://c.test/log",
                "environment_url": "https://app.test"})
        );
        assert_eq!(
            status_body("success", "https://c.test/log", None),
            json!({"state": "success", "description": "Deployed with Coolify.", "log_url": "https://c.test/log"})
        );
        assert_eq!(
            status_body("inactive", "https://c.test/log", None)["description"],
            "The pull request was closed."
        );
    }

    #[test]
    fn pull_request_refs() {
        let same_repository = json!({
            "head": {"ref": "feature", "sha": "ABCDEF", "repo": {"full_name": "Acme/Web"}},
            "base": {"repo": {"full_name": "acme/web"}},
        });
        // The head branch while it points to the deployed commit.
        assert_eq!(
            pull_request_ref(Some("abcdef"), &same_repository, "acme/web"),
            "feature"
        );
        assert_eq!(
            pull_request_ref(Some("HEAD"), &same_repository, "acme/web"),
            "feature"
        );
        assert_eq!(
            pull_request_ref(None, &same_repository, "acme/web"),
            "feature"
        );
        // The branch moved on: the deployed commit.
        assert_eq!(
            pull_request_ref(Some("111111"), &same_repository, "acme/web"),
            "111111"
        );
        // A fork branch is not in the base repository.
        let fork = json!({
            "head": {"ref": "feature", "sha": "abcdef", "repo": {"full_name": "someone/web"}},
            "base": {"repo": {"full_name": "acme/web"}},
        });
        assert_eq!(
            pull_request_ref(Some("abcdef"), &fork, "acme/web"),
            "abcdef"
        );
        assert_eq!(pull_request_ref(None, &fork, "acme/web"), "abcdef");
        // A deleted fork has no head repository.
        let deleted = json!({"head": {"ref": "feature", "sha": "abcdef", "repo": null}});
        assert_eq!(pull_request_ref(None, &deleted, "acme/web"), "abcdef");
    }

    #[test]
    fn branch_refs_prefer_the_deployed_commit() {
        assert_eq!(branch_ref(Some("abc123"), "main"), "abc123");
        assert_eq!(branch_ref(Some("HEAD"), "main"), "main");
        assert_eq!(branch_ref(Some(""), "main"), "main");
        assert_eq!(branch_ref(None, "main"), "main");
    }

    #[test]
    fn environment_urls() {
        assert_eq!(
            environment_url(None, Some("https://a.test,https://b.test"), None).as_deref(),
            Some("https://a.test")
        );
        assert_eq!(
            environment_url(Some("nixpacks"), Some(" http://a.test "), None).as_deref(),
            Some("http://a.test")
        );
        // A domain without a scheme is no URL for GitHub.
        assert_eq!(environment_url(None, Some("a.test"), None), None);
        assert_eq!(environment_url(None, None, None), None);
        let domains =
            json!({"db": {"domain": ""}, "web": {"domain": "https://w.test,https://x.test"}});
        assert_eq!(
            environment_url(
                Some("dockercompose"),
                Some("https://ignored.test"),
                Some(&domains.to_string())
            )
            .as_deref(),
            Some("https://w.test")
        );
        assert_eq!(environment_url(Some("dockercompose"), None, Some("")), None);
        assert_eq!(environment_url(Some("dockercompose"), None, None), None);
    }
}
