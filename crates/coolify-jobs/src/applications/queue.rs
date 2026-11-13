//! `queue_application_deployment()` and the helpers that start queued deployments
//! (bootstrap/helpers/applications.php). One coolify-rs process runs the jobs, so a
//! process-wide lock stands in for upstream's row locks.

use chrono::Utc;
use coolify_db::applications::{self as apps, ApplicationRow, NewDeployment};
use coolify_db::deployment_queue::{self as dq, BuildSlots, NewerDeployment, Superseded};
use coolify_db::servers;
use rand::seq::SliceRandom as _;
use serde_json::Value;

use crate::Jobs;
use crate::server::ServerInfo;

static QUEUE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// What `queue_application_deployment` answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Queued {
    Queued { deployment_uuid: String },
    Skipped { deployment_uuid: String },
    QueueFull,
}

impl Queued {
    pub fn message(&self) -> &'static str {
        match self {
            Queued::Queued { .. } => "Deployment queued.",
            Queued::Skipped { .. } => "Deployment already queued for this commit.",
            Queued::QueueFull => {
                "Deployment queue is full. Please wait for existing deployments to complete."
            }
        }
    }
}

/// The options of `queue_application_deployment(...)`.
#[derive(Debug, Clone, Default)]
pub struct QueueOptions {
    pub commit: Option<String>,
    pub force_rebuild: bool,
    pub is_webhook: bool,
    pub is_api: bool,
    pub restart_only: bool,
    pub no_questions_asked: bool,
    pub rollback: bool,
    pub docker_registry_image_tag: Option<String>,
    /// `$pull_request_id` (0: the application itself).
    pub pull_request_id: i64,
    /// `$git_type`.
    pub git_type: Option<String>,
    /// `$server`, instead of the server of the application's destination.
    pub server_id: Option<i64>,
    /// `$destination`, instead of the application's destination.
    pub destination_id: Option<i64>,
    /// `$only_this_server`.
    pub only_this_server: bool,
    /// `$parent_deployment_uuid`.
    pub parent_deployment_uuid: Option<String>,
    /// `$is_mcp`: queued by an MCP tool. coolify-rs has no MCP server; the column is kept for
    /// deployments that another deployment of an MCP caller queues (additional servers).
    pub is_mcp: bool,
    /// `$cancel_previous`: which older deployments an API deployment cancels.
    pub cancel_previous: Option<CancelPreviousDeployments>,
    /// `$scan_gate_bypassed_by`: the user who chose "Deploy anyway" for an image the deploy gate
    /// blocked.
    pub scan_gate_bypassed_by: Option<i64>,
    /// `$scan_gate_pinned_image`: "Deploy anyway" of a Docker image starts the blocked image by
    /// this repo digest.
    pub scan_gate_pinned_image: Option<String>,
}

/// `App\Enums\CancelPreviousDeployments`: which older deployments a new deployment cancels. A
/// building deployment is cancelled only before it starts to replace containers, and cancelling
/// builds also cancels queued deployments.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CancelPreviousDeployments {
    #[default]
    None,
    Queued,
    Building,
}

impl CancelPreviousDeployments {
    /// `CancelPreviousDeployments::tryFrom($value)`.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "none" => Some(Self::None),
            "queued" => Some(Self::Queued),
            "building" => Some(Self::Building),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Queued => "queued",
            Self::Building => "building",
        }
    }
}

/// The flags of the new deployment that `cancel_mode()` reads.
#[derive(Debug, Clone, Copy, Default)]
pub struct SupersedingDeployment {
    pub is_webhook: bool,
    pub is_api: bool,
    pub is_mcp: bool,
    pub rollback: bool,
    pub restart_only: bool,
}

/// The `$mode` of `cancel_superseded_deployments()`: what the new deployment may cancel.
/// - A webhook deployment replaces older webhook deployments, as the application settings allow.
/// - An API or MCP deployment replaces older webhook, API and MCP deployments, as its caller asks.
///   Without `cancel_previous` nothing is cancelled, so callers that wait for their deployment
///   keep working.
/// - Restarts and rollbacks never cancel others.
pub fn cancel_mode(
    new: &SupersedingDeployment,
    auto_cancel_queued: bool,
    auto_cancel_building: bool,
    cancel_previous: Option<CancelPreviousDeployments>,
) -> CancelPreviousDeployments {
    if new.rollback || new.restart_only {
        return CancelPreviousDeployments::None;
    }
    if new.is_webhook && auto_cancel_building {
        CancelPreviousDeployments::Building
    } else if new.is_webhook && auto_cancel_queued {
        CancelPreviousDeployments::Queued
    } else if new.is_api || new.is_mcp {
        cancel_previous.unwrap_or_default()
    } else {
        CancelPreviousDeployments::None
    }
}

/// `least_busy_build_server($buildServers, $onlyWithFreeSlot)`: the build server with the fewest
/// running builds, at random among equals. With `only_with_free_slot`, a server that already runs
/// as many builds as its "Concurrent builds" setting allows is skipped, and `None` means that all
/// servers are full.
pub fn pick_least_busy<R: rand::Rng + ?Sized>(
    mut slots: Vec<BuildSlots>,
    only_with_free_slot: bool,
    rng: &mut R,
) -> Option<i64> {
    slots.retain(|s| !only_with_free_slot || s.builds < s.concurrent_builds);
    slots.shuffle(rng);
    // Stable, so the shuffle decides among equals.
    slots.sort_by_key(|s| s.builds);
    slots.first().map(|s| s.server_id)
}

/// `Application::mustBuildOnBuildServer($server)`: a deployments-only server cannot build this
/// application, so a build server must build it. Docker image applications build nothing, and
/// Compose applications do not use build servers.
pub fn must_build_on_build_server(a: &ApplicationRow, server: &ServerInfo) -> bool {
    !server.can_build() && !matches!(a.str("build_pack"), Some("dockerimage" | "dockercompose"))
}

/// `scan_gate_result` of a new deployment: `['repo_digest' => $scan_gate_pinned_image]` when
/// "Deploy anyway" pins the blocked image, else null.
pub fn scan_gate_result(pinned_image: Option<&str>) -> Option<Value> {
    pinned_image.map(|digest| serde_json::json!({ "repo_digest": digest }))
}

/// `validateGitRef($commit, 'deployment commit')`.
pub fn validate_commit(commit: &str) -> Result<String, String> {
    validate_git_ref(commit, "deployment commit")
}

/// `validateGitRef($input, $context)`.
pub fn validate_git_ref(commit: &str, context: &str) -> Result<String, String> {
    let c = commit.trim();
    if c.is_empty() || c == "HEAD" {
        return Ok(c.to_owned());
    }
    if c.starts_with('-') {
        return Err(format!("Invalid {context}: must not start with a hyphen."));
    }
    let mut chars = c.chars();
    let first_ok = chars.next().is_some_and(|ch| ch.is_ascii_alphanumeric());
    if !first_ok || !chars.all(|ch| ch.is_ascii_alphanumeric() || "._-/".contains(ch)) {
        return Err(format!(
            "Invalid {context}: contains disallowed characters. Only alphanumeric characters, dots, hyphens, underscores, and slashes are allowed."
        ));
    }
    Ok(c.to_owned())
}

impl Jobs {
    /// `queue_application_deployment($application, $deployment_uuid, ...)`.
    pub async fn queue_application_deployment(
        &self,
        a: &ApplicationRow,
        deployment_uuid: &str,
        o: QueueOptions,
    ) -> Result<Queued, String> {
        let commit = validate_commit(
            o.commit
                .as_deref()
                .filter(|c| !c.is_empty())
                .unwrap_or_else(|| {
                    a.str("git_commit_sha")
                        .filter(|c| !c.is_empty())
                        .unwrap_or("HEAD")
                }),
        )?;
        let db = &self.0.db;
        let server_id = o
            .server_id
            .or_else(|| a.server_id())
            .ok_or("The application has no destination.")?;
        let server = servers::find_by_id(db, server_id)
            .await
            .map_err(|e| e.to_string())?
            .ok_or("Server not found.")?;
        let settings = server.settings.clone().unwrap_or_default();
        let env_uuid = a
            .environment
            .get("uuid")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let project_uuid = a
            .project
            .get("uuid")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let started;
        let queue_id;
        let superseded: Superseded;
        {
            let _guard = QUEUE_LOCK.lock().await;
            let limit = settings
                .get("deployment_queue_limit")
                .and_then(Value::as_i64)
                .unwrap_or(25);
            let queued = apps::queued_on_server(db, server_id)
                .await
                .map_err(|e| e.to_string())?;
            if i64::try_from(queued.len()).unwrap_or(i64::MAX) >= limit {
                return Ok(Queued::QueueFull);
            }
            let open = apps::deployments_with_status(db, a.id, &["in_progress", "queued"])
                .await
                .map_err(|e| e.to_string())?;
            if let Some(existing) = open.iter().find(|d| {
                d.str("commit") == Some(commit.as_str())
                    && d.int("pull_request_id").unwrap_or(0) == o.pull_request_id
                    && d.str("docker_registry_image_tag") == o.docker_registry_image_tag.as_deref()
            }) && !(o.force_rebuild || o.rollback || o.no_questions_asked)
            {
                return Ok(Queued::Skipped {
                    deployment_uuid: existing.uuid.clone(),
                });
            }
            let new = NewDeployment {
                application_id: a.id,
                application_name: a.str("name").unwrap_or_default().to_owned(),
                server_id,
                server_name: server
                    .record
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                destination_id: o
                    .destination_id
                    .unwrap_or_else(|| a.int("destination_id").unwrap_or_default()),
                deployment_uuid: deployment_uuid.to_owned(),
                deployment_url: format!(
                    "/project/{project_uuid}/environment/{env_uuid}/application/{}/deployment/{deployment_uuid}",
                    a.uuid
                ),
                pull_request_id: o.pull_request_id,
                force_rebuild: o.force_rebuild,
                commit: commit.clone(),
                status: "queued".into(),
                is_webhook: o.is_webhook,
                // MCP tools also call the REST API controllers; such a deployment is not an API one.
                is_api: o.is_api && !o.is_mcp,
                is_mcp: o.is_mcp,
                restart_only: o.restart_only,
                rollback: o.rollback,
                only_this_server: o.only_this_server,
                git_type: o.git_type.clone(),
                commit_message: None,
                docker_registry_image_tag: o.docker_registry_image_tag.clone(),
                build_server_id: None,
                parent_deployment_uuid: o.parent_deployment_uuid.clone(),
                scan_gate_bypassed_by: o.scan_gate_bypassed_by,
                // "Deploy anyway" of a Docker image starts the blocked image by its digest.
                scan_gate_result: scan_gate_result(o.scan_gate_pinned_image.as_deref()),
            };
            let now = Utc::now().naive_utc();
            queue_id = apps::insert_deployment(db, &new, now)
                .await
                .map_err(|e| e.to_string())?;
            superseded = self
                .cancel_superseded_deployments(
                    a,
                    &NewerDeployment {
                        id: queue_id,
                        application_id: a.id,
                        pull_request_id: o.pull_request_id,
                        server_id,
                    },
                    &SupersedingDeployment {
                        is_webhook: new.is_webhook,
                        is_api: new.is_api,
                        is_mcp: new.is_mcp,
                        rollback: new.rollback,
                        restart_only: new.restart_only,
                    },
                    o.cancel_previous,
                )
                .await
                .map_err(|e| e.to_string())?;
            // Decided under the lock, so concurrent requests cannot both see an idle queue. After a
            // cancelled build, `CancelSupersededDeploymentJob` starts it once the build is stopped.
            let mut reserved = None;
            started = superseded.building.is_empty()
                && (o.no_questions_asked || {
                    let (ok, build_server_id) =
                        self.next_queuable(server_id, a.id, o.pull_request_id).await;
                    reserved = build_server_id;
                    ok
                });
            if started {
                dq::admit(db, queue_id, reserved, now)
                    .await
                    .map_err(|e| e.to_string())?;
            }
        }
        for (building, old) in superseded
            .queued
            .iter()
            .map(|d| (false, d))
            .chain(superseded.building.iter().map(|d| (true, d)))
        {
            let entry = apps::LogEntry {
                command: None,
                output: format!(
                    "Deployment cancelled: the newer deployment {deployment_uuid} (commit {commit}) replaces it."
                ),
                kind: "stderr",
                timestamp: Utc::now().format("%Y-%m-%dT%H:%M:%S%.6fZ").to_string(),
                hidden: false,
                batch: 1,
            };
            let _ = apps::append_log(db, old.id, &[entry], Utc::now().naive_utc()).await;
            // The cancel is a query update that fires no model event.
            self.sync_github_deployment(old.id);
            if building {
                self.dispatch_cancel_superseded_deployment(old.clone());
            }
        }
        if started {
            // `$deployment->update(['status' => 'in_progress'])` fires the model event that
            // syncs the GitHub Deployment.
            self.sync_github_deployment(queue_id);
            self.dispatch_deployment(queue_id);
        }
        Ok(Queued::Queued {
            deployment_uuid: deployment_uuid.to_owned(),
        })
    }

    /// `cancel_superseded_deployments($deployment, $application, $cancelPrevious)`: a new
    /// deployment can replace older deployments of the same application, pull request and server.
    /// A running deployment is cancelled only before it starts to deploy containers, so a cancel
    /// never leaves an application half deployed. Manual, restart and rollback deployments are
    /// never cancelled. Call it under the queue lock, after the new deployment is created.
    async fn cancel_superseded_deployments(
        &self,
        a: &ApplicationRow,
        new: &NewerDeployment,
        flags: &SupersedingDeployment,
        cancel_previous: Option<CancelPreviousDeployments>,
    ) -> coolify_db::DbResult<Superseded> {
        let mode = cancel_mode(
            flags,
            a.setting_bool("is_auto_cancel_queued_deployments_enabled"),
            a.setting_bool("is_auto_cancel_building_deployments_enabled"),
            cancel_previous,
        );
        if mode == CancelPreviousDeployments::None {
            return Ok(Superseded::default());
        }
        dq::cancel_superseded(
            &self.0.db,
            new,
            flags.is_webhook,
            mode == CancelPreviousDeployments::Building,
            Utc::now().naive_utc(),
        )
        .await
    }

    /// `CancelSupersededDeploymentJob`: stops the build of a running deployment that a newer
    /// deployment replaced, then starts the queued deployments. It runs in the background,
    /// because one push can replace builds of many applications and the webhook request must
    /// answer fast.
    fn dispatch_cancel_superseded_deployment(&self, old: apps::Deployment) {
        let jobs = self.clone();
        tokio::spawn(async move {
            // `CleanupCancelledDeployment::run($this->deployment, $this->teamId)`.
            jobs.cleanup_cancelled_deployment(&old).await;
            if let Some(server_id) = old.int("server_id")
                && matches!(
                    apps::find_by_id(&jobs.0.db, old.application_id).await,
                    Ok(Some(_))
                )
            {
                jobs.queue_next_deployment(old.application_id, server_id)
                    .await;
            }
        });
    }

    /// `Application::buildServerCandidates($server)`: the build servers that can build a
    /// deployment of this application on `server`. It is empty when the deployment builds on
    /// `server`, or when `server` is an additional server that pulls the image.
    pub(crate) async fn build_server_candidates(
        &self,
        a: &ApplicationRow,
        server: &ServerInfo,
    ) -> Vec<i64> {
        let db = &self.0.db;
        let has_image = a
            .str("docker_registry_image_name")
            .is_some_and(|n| !n.is_empty());
        let pulls_image = has_image
            && coolify_db::resource_ops::additional_server_ids(db, a.id)
                .await
                .unwrap_or_default()
                .contains(&server.id);
        if pulls_image {
            return Vec::new();
        }
        if !must_build_on_build_server(a, server) && !a.setting_bool("is_build_server_enabled") {
            return Vec::new();
        }
        apps::build_server_ids(db, a.team_id, server.id)
            .await
            .unwrap_or_default()
    }

    /// `least_busy_build_server($buildServers, $onlyWithFreeSlot)` over server ids.
    pub(crate) async fn least_busy_build_server(
        &self,
        ids: &[i64],
        only_with_free_slot: bool,
    ) -> Option<i64> {
        let slots = dq::build_slots(&self.0.db, ids).await.ok()?;
        pick_least_busy(slots, only_with_free_slot, &mut rand::thread_rng())
    }

    /// `next_queuable($deployment)`: whether a queued deployment can start now. No build of the
    /// application may be in progress for the same pull request (a production deployment and a
    /// preview run side by side). Each deployment holds one build slot of the server that builds
    /// it: its deployment server, or a build server of the team. When it builds on a build server,
    /// the least busy one with a free slot is reserved: the second value, which the caller saves
    /// in `build_server_id` with the status change. Call it under the queue lock.
    async fn next_queuable(
        &self,
        server_id: i64,
        application_id: i64,
        pull_request_id: i64,
    ) -> (bool, Option<i64>) {
        let db = &self.0.db;
        let in_progress = apps::deployments_with_status(db, application_id, &["in_progress"])
            .await
            .unwrap_or_default();
        if in_progress
            .iter()
            .any(|d| d.int("pull_request_id").unwrap_or(0) == pull_request_id)
        {
            return (false, None);
        }
        // A deleted server keeps its queued deployments; they cannot run anymore.
        let Ok(Some(server)) = servers::find_by_id(db, server_id).await else {
            return (false, None);
        };
        let Ok(Some(application)) = apps::find_by_id(db, application_id).await else {
            return (false, None);
        };
        let build_servers = self
            .build_server_candidates(&application, &ServerInfo::from_row(&server))
            .await;
        if build_servers.is_empty() {
            let concurrent = server
                .settings
                .as_ref()
                .and_then(|s| s.get("concurrent_builds"))
                .and_then(Value::as_i64)
                .unwrap_or(2);
            let free = dq::active_builds_on_server(db, server_id)
                .await
                .unwrap_or(i64::MAX)
                < concurrent;
            return (free, None);
        }
        match self.least_busy_build_server(&build_servers, true).await {
            Some(id) => (true, Some(id)),
            None => (false, None),
        }
    }

    /// `LoadComposeFile::dispatch($application)`.
    pub fn spawn_load_compose_file(&self, application_id: i64) {
        let jobs = self.clone();
        tokio::spawn(async move {
            if let Err(e) =
                crate::compose_parser::load_compose_file(&jobs, application_id, false).await
            {
                tracing::warn!(
                    application_id,
                    error = e.message(),
                    "LoadComposeFile failed"
                );
            }
        });
    }

    /// `force_start_deployment($deployment)`: starts a queued deployment right away, ignoring the
    /// per-application and per-server limits. False when it is no longer queued, so a repeated
    /// force start never runs the same deployment twice.
    pub async fn force_start_deployment(&self, queue_id: i64) -> bool {
        {
            let _guard = QUEUE_LOCK.lock().await;
            let Ok(Some(current)) = apps::deployment_by_id(&self.0.db, queue_id).await else {
                return false;
            };
            if current.status != "queued" {
                return false;
            }
            if apps::update_deployment(
                &self.0.db,
                queue_id,
                &[(
                    "status",
                    coolify_db::servers::ColumnValue::Text(Some("in_progress".into())),
                )],
                Utc::now().naive_utc(),
            )
            .await
            .is_err()
            {
                return false;
            }
        }
        self.dispatch_deployment(queue_id);
        true
    }

    /// `ApplicationDeploymentJob::dispatch(...)`: runs on the deployment workers.
    fn dispatch_deployment(&self, queue_id: i64) {
        let jobs = self.clone();
        tokio::spawn(async move { jobs.run_application_deployment(queue_id).await });
    }

    /// `queue_next_deployment($application, $finishedServerId)`: starts queued deployments of
    /// the servers involved, oldest first.
    pub async fn queue_next_deployment(&self, application_id: i64, finished_server_id: i64) {
        let db = &self.0.db;
        let mut server_ids = vec![finished_server_id];
        let application = apps::find_by_id(db, application_id).await.ok().flatten();
        if let Some(id) = application.as_ref().and_then(ApplicationRow::server_id)
            && !server_ids.contains(&id)
        {
            server_ids.push(id);
        }
        // A build on a build server frees a slot that deployments on other servers of the team can wait for.
        let mut team_server_ids = Vec::new();
        if let Some(a) = &application
            && let Some(primary_id) = a.server_id()
            && let Ok(Some(primary)) = servers::find_by_id(db, primary_id).await
            && !self
                .build_server_candidates(a, &ServerInfo::from_row(&primary))
                .await
                .is_empty()
        {
            team_server_ids = dq::team_server_ids(db, primary.team_id)
                .await
                .unwrap_or_default();
        }
        for d in apps::deployments_with_status(db, application_id, &["queued"])
            .await
            .unwrap_or_default()
        {
            if let Some(id) = d.int("server_id")
                && !server_ids.contains(&id)
            {
                server_ids.push(id);
            }
        }
        for id in team_server_ids {
            if !server_ids.contains(&id) {
                server_ids.push(id);
            }
        }
        let mut queued = Vec::new();
        for id in server_ids {
            queued.extend(apps::queued_on_server(db, id).await.unwrap_or_default());
        }
        queued.sort_by(|a, b| {
            a.str("created_at")
                .cmp(&b.str("created_at"))
                .then(a.id.cmp(&b.id))
        });
        for d in queued {
            let _guard = QUEUE_LOCK.lock().await;
            let Ok(Some(fresh)) = apps::deployment_by_id(db, d.id).await else {
                continue;
            };
            if fresh.status != "queued" {
                continue;
            }
            let server_id = fresh.int("server_id").unwrap_or_default();
            let pull_request_id = fresh.int("pull_request_id").unwrap_or(0);
            let (ok, build_server_id) = self
                .next_queuable(server_id, fresh.application_id, pull_request_id)
                .await;
            if !ok {
                continue;
            }
            let _ = dq::admit(db, d.id, build_server_id, Utc::now().naive_utc()).await;
            // `$current->update(['status' => 'in_progress'])`: the model event syncs GitHub.
            self.sync_github_deployment(d.id);
            self.dispatch_deployment(d.id);
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]
mod tests {
    use super::*;

    fn webhook() -> SupersedingDeployment {
        SupersedingDeployment {
            is_webhook: true,
            ..Default::default()
        }
    }

    /// "Deploy anyway" of a Docker image stores the digest of the blocked image.
    #[test]
    fn deploy_anyway_stores_the_pinned_digest_as_the_gate_result() {
        assert_eq!(
            scan_gate_result(Some("nginx@sha256:abc")),
            Some(serde_json::json!({"repo_digest": "nginx@sha256:abc"}))
        );
        assert_eq!(scan_gate_result(None), None);
    }

    /// `cancel_superseded_deployments()`: the mode of a webhook deployment comes from the
    /// application settings, building before queued.
    #[test]
    fn a_webhook_deployment_cancels_what_the_application_settings_allow() {
        use CancelPreviousDeployments as C;
        assert_eq!(cancel_mode(&webhook(), true, false, None), C::Queued);
        assert_eq!(cancel_mode(&webhook(), true, true, None), C::Building);
        assert_eq!(cancel_mode(&webhook(), false, true, None), C::Building);
        assert_eq!(cancel_mode(&webhook(), false, false, None), C::None);
        // The caller's wish does not apply to webhooks.
        assert_eq!(
            cancel_mode(&webhook(), false, false, Some(C::Building)),
            C::None
        );
    }

    /// An API or MCP deployment cancels only what its caller asks for; a manual one nothing.
    #[test]
    fn an_api_deployment_cancels_only_on_request() {
        use CancelPreviousDeployments as C;
        let api = SupersedingDeployment {
            is_api: true,
            ..Default::default()
        };
        let mcp = SupersedingDeployment {
            is_mcp: true,
            ..Default::default()
        };
        assert_eq!(cancel_mode(&api, true, true, None), C::None);
        assert_eq!(cancel_mode(&api, false, false, Some(C::Queued)), C::Queued);
        assert_eq!(
            cancel_mode(&mcp, false, false, Some(C::Building)),
            C::Building
        );
        let manual = SupersedingDeployment::default();
        assert_eq!(cancel_mode(&manual, true, true, Some(C::Building)), C::None);
    }

    /// Restarts and rollbacks never cancel other deployments.
    #[test]
    fn restarts_and_rollbacks_cancel_nothing() {
        use CancelPreviousDeployments as C;
        let rollback = SupersedingDeployment {
            rollback: true,
            ..webhook()
        };
        let restart = SupersedingDeployment {
            restart_only: true,
            is_api: true,
            ..Default::default()
        };
        assert_eq!(cancel_mode(&rollback, true, true, None), C::None);
        assert_eq!(
            cancel_mode(&restart, true, true, Some(C::Building)),
            C::None
        );
    }

    #[test]
    fn cancel_previous_values() {
        use CancelPreviousDeployments as C;
        for mode in [C::None, C::Queued, C::Building] {
            assert_eq!(C::parse(mode.as_str()), Some(mode));
        }
        assert_eq!(C::parse("all"), None);
    }

    fn slots(server_id: i64, builds: i64, concurrent_builds: i64) -> BuildSlots {
        BuildSlots {
            server_id,
            builds,
            concurrent_builds,
        }
    }

    /// `least_busy_build_server()`: the fewest running builds win; a full server is skipped
    /// unless the caller takes any server.
    #[test]
    fn the_least_busy_build_server_with_a_free_slot_is_reserved() {
        let mut rng = rand::thread_rng();
        let all = vec![
            slots(1, 2, 2),
            slots(2, 1, 2),
            slots(3, 0, 1),
            slots(4, 3, 8),
        ];
        assert_eq!(pick_least_busy(all, true, &mut rng), Some(3));
        let full = vec![slots(1, 2, 2), slots(2, 5, 1)];
        assert_eq!(pick_least_busy(full.clone(), true, &mut rng), None);
        assert_eq!(pick_least_busy(full, false, &mut rng), Some(1));
        assert_eq!(pick_least_busy(Vec::new(), false, &mut rng), None);
        // Equals are picked at random: both show up over many draws.
        let equal = vec![slots(1, 0, 2), slots(2, 0, 2), slots(3, 1, 2)];
        let mut seen = std::collections::HashSet::new();
        for _ in 0..200 {
            seen.insert(pick_least_busy(equal.clone(), true, &mut rng).unwrap());
        }
        assert_eq!(seen, std::collections::HashSet::from([1, 2]));
    }
}
