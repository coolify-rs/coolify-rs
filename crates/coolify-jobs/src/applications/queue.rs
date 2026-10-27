//! `queue_application_deployment()` and the helpers that start queued deployments
//! (bootstrap/helpers/applications.php). One coolify-rs process runs the jobs, so a
//! process-wide lock stands in for upstream's row locks.

use chrono::Utc;
use coolify_db::applications::{self as apps, ApplicationRow, NewDeployment};
use coolify_db::servers;
use serde_json::Value;

use crate::Jobs;

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
}

/// `validateGitRef($commit, 'deployment commit')`.
pub fn validate_commit(commit: &str) -> Result<String, String> {
    let c = commit.trim();
    if c.is_empty() || c == "HEAD" {
        return Ok(c.to_owned());
    }
    if c.starts_with('-') {
        return Err("Invalid deployment commit: must not start with a hyphen.".into());
    }
    let mut chars = c.chars();
    let first_ok = chars.next().is_some_and(|ch| ch.is_ascii_alphanumeric());
    if !first_ok || !chars.all(|ch| ch.is_ascii_alphanumeric() || "._-/".contains(ch)) {
        return Err("Invalid deployment commit: contains disallowed characters. Only alphanumeric characters, dots, hyphens, underscores, and slashes are allowed.".into());
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
        let server_id = a.server_id().ok_or("The application has no destination.")?;
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
                    && d.int("pull_request_id").unwrap_or(0) == 0
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
                destination_id: a.int("destination_id").unwrap_or_default(),
                deployment_uuid: deployment_uuid.to_owned(),
                deployment_url: format!(
                    "/project/{project_uuid}/environment/{env_uuid}/application/{}/deployment/{deployment_uuid}",
                    a.uuid
                ),
                pull_request_id: 0,
                force_rebuild: o.force_rebuild,
                commit: commit.clone(),
                status: "queued".into(),
                is_webhook: o.is_webhook,
                is_api: o.is_api,
                restart_only: o.restart_only,
                rollback: o.rollback,
                only_this_server: false,
                git_type: None,
                commit_message: None,
                docker_registry_image_tag: o.docker_registry_image_tag.clone(),
                build_server_id: None,
            };
            let now = Utc::now().naive_utc();
            queue_id = apps::insert_deployment(db, &new, now)
                .await
                .map_err(|e| e.to_string())?;
            started = o.no_questions_asked || self.next_queuable(server_id, a.id).await;
            if started {
                apps::update_deployment(
                    db,
                    queue_id,
                    &[(
                        "status",
                        coolify_db::servers::ColumnValue::Text(Some("in_progress".into())),
                    )],
                    now,
                )
                .await
                .map_err(|e| e.to_string())?;
            }
        }
        if started {
            self.dispatch_deployment(queue_id);
        }
        Ok(Queued::Queued {
            deployment_uuid: deployment_uuid.to_owned(),
        })
    }

    /// `next_queuable()`: no build of the application in progress and a free build slot.
    async fn next_queuable(&self, server_id: i64, application_id: i64) -> bool {
        let db = &self.0.db;
        let in_progress = apps::deployments_with_status(db, application_id, &["in_progress"])
            .await
            .unwrap_or_default();
        if in_progress
            .iter()
            .any(|d| d.int("pull_request_id").unwrap_or(0) == 0)
        {
            return false;
        }
        let Ok(Some(server)) = servers::find_by_id(db, server_id).await else {
            return false;
        };
        let concurrent = server
            .settings
            .as_ref()
            .and_then(|s| s.get("concurrent_builds"))
            .and_then(Value::as_i64)
            .unwrap_or(2);
        apps::in_progress_on_server(db, server_id)
            .await
            .unwrap_or(i64::MAX)
            < concurrent
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
        if let Ok(Some(a)) = apps::find_by_id(db, application_id).await
            && let Some(id) = a.server_id()
            && !server_ids.contains(&id)
        {
            server_ids.push(id);
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
            if !self.next_queuable(server_id, fresh.application_id).await {
                continue;
            }
            let _ = apps::update_deployment(
                db,
                d.id,
                &[(
                    "status",
                    coolify_db::servers::ColumnValue::Text(Some("in_progress".into())),
                )],
                Utc::now().naive_utc(),
            )
            .await;
            self.dispatch_deployment(d.id);
        }
    }
}
