//! Build servers and additional servers of `ApplicationDeploymentJob`: `selectBuildServer()`,
//! `deploy_to_additional_destinations()`, `ensureRegistryImageForMultipleServers()` and the
//! combined multi-server notification.

use std::collections::HashSet;
use std::sync::{LazyLock, Mutex};

use coolify_db::application_ops as ops;
use coolify_db::applications as apps;
use coolify_db::servers;
use coolify_remote::shell::quote;
use rand::seq::SliceRandom as _;
use serde_json::Value;

use super::{Cmd, Fail, Run, Step, in_docker, now};
use crate::applications::queue::{QueueOptions, Queued};
use crate::server::ServerInfo;

/// `Cache::put("multi-server-deployment-queued:{uuid}")`: in-process, a restart fails running jobs.
static MULTI_QUEUED: LazyLock<Mutex<HashSet<String>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));
/// `Cache::add("multi-server-deployment-notified:{uuid}")`.
static MULTI_NOTIFIED: LazyLock<Mutex<HashSet<String>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

pub(super) fn multi_server_mark_queued(parent: &str) {
    if let Ok(mut set) = MULTI_QUEUED.lock() {
        set.insert(parent.to_owned());
    }
}

impl Run {
    /// `$this->server = $this->mainServer`.
    pub(super) fn to_main_server(&mut self) {
        self.server = self.main_server.clone();
        self.server_name = self.main_name.clone();
        self.target = std::sync::Arc::clone(&self.main_target);
    }

    /// `$this->server = $this->build_server`.
    pub(super) fn to_build_server(&mut self) {
        if let Some((server, name, target)) = &self.build_server {
            self.server = server.clone();
            self.server_name = name.clone();
            self.target = std::sync::Arc::clone(target);
        }
    }

    pub(super) fn on_build_server(&self) -> bool {
        self.use_build_server && self.server.id != self.main_server.id
    }

    /// `if ($this->use_build_server) { $this->server = $this->build_server; }`.
    pub(super) fn move_to_build_server(&mut self) {
        if self.use_build_server {
            self.to_build_server();
        }
    }

    /// `$this->server->canBuildApplications()`.
    pub(super) fn server_can_build(&self) -> bool {
        can_build(&self.server)
    }

    /// `create_workdir()`.
    pub(super) async fn create_workdir(&mut self) -> Step {
        let cfg = format!("mkdir -p {}", self.cfg);
        if self.use_build_server {
            self.to_main_server();
            self.exec(vec![Cmd::new(cfg.clone())]).await?;
            self.to_build_server();
        }
        let mkdir = in_docker(&self.dep, &format!("mkdir -p {}", self.workdir));
        self.exec(vec![Cmd::new(mkdir), Cmd::new(cfg)]).await
    }

    /// `selectBuildServer()`.
    pub(super) async fn select_build_server(&mut self) -> Step {
        let has_image = self
            .app
            .str("docker_registry_image_name")
            .is_some_and(|n| !n.is_empty());
        let build_pack = self.build_pack();
        if self.is_additional && has_image {
            // The main deployment already built the image (also for a forced rebuild).
            if build_pack != "dockercompose" || !self.server_can_build() {
                self.b.force_rebuild = false;
            }
            self.info(
                "Additional server: pulls the image from the registry, no build server needed.",
            )
            .await;
            return Ok(());
        }
        let must_build_elsewhere = !self.server_can_build()
            && !matches!(build_pack.as_str(), "dockerimage" | "dockercompose");
        if !must_build_elsewhere && !self.app.setting_bool("is_build_server_enabled") {
            return Ok(());
        }
        let name = self.server_name.clone();
        if must_build_elsewhere && !self.b.restart_only && !has_image {
            return Err(Fail::new(format!(
                "The deployment server ({name}) is set to deployments only, so this application is built on a build server. Set a Docker image name in the application's General settings so the deployment server can pull the built image."
            )));
        }
        let db = self.jobs.0.db.clone();
        let ids = apps::build_server_ids(&db, self.app.team_id, self.server.id)
            .await
            .map_err(|e| Fail::new(e.to_string()))?;
        let Some(id) = ids.choose(&mut rand::thread_rng()).copied() else {
            if must_build_elsewhere && !self.b.restart_only {
                return Err(Fail::new(format!(
                    "The deployment server ({name}) is set to deployments only, and no usable build server was found. Add a build server or change the server role."
                )));
            }
            let fallback = apps::build_server_fallback_enabled(&db, self.app.team_id)
                .await
                .map_err(|e| Fail::new(e.to_string()))?;
            if !fallback && !self.b.restart_only {
                return Err(Fail::new(
                    "No available dedicated build server was found. Enable a usable build server for this team or allow fallback to the deployment server in the team settings.",
                ));
            }
            self.info("No suitable build server found. Using the deployment server.")
                .await;
            return Ok(());
        };
        let row = servers::find_by_id(&db, id)
            .await
            .map_err(|e| Fail::new(e.to_string()))?
            .ok_or_else(|| Fail::new("Server not found."))?;
        let server = ServerInfo::from_row(&row);
        let target = std::sync::Arc::new(server.target(&self.jobs).await.map_err(Fail::new)?);
        let build_name = row
            .record
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let _ = apps::update_deployment(
            &db,
            self.queue_id,
            &[("build_server_id", servers::ColumnValue::Int(id))],
            now(),
        )
        .await;
        self.info(&format!("Found a suitable build server ({build_name})."))
            .await;
        self.build_server = Some((server, build_name, target));
        self.use_build_server = true;
        Ok(())
    }

    /// `ensureRegistryImageForMultipleServers()`, `warnAboutVolumesOnMultipleServers()` and
    /// `warnAboutMixedProxiesOnMultipleServers()`.
    pub(super) async fn check_multiple_servers(&mut self) -> Step {
        if self.b.is_preview() {
            return Ok(());
        }
        let db = self.jobs.0.db.clone();
        let additional = ops::additional_networks(&db, self.app.id)
            .await
            .map_err(|e| Fail::new(e.to_string()))?;
        if additional.is_empty() {
            return Ok(());
        }
        if self.build_pack() != "dockercompose"
            && !self
                .app
                .str("docker_registry_image_name")
                .is_some_and(|n| !n.is_empty())
        {
            return Err(Fail::new(
                "Before deploying to multiple servers, you must set a Docker image in the General tab. More information: https://coolify.io/docs/knowledge-base/server/multiple-servers",
            ));
        }
        if !apps::persistent_volumes(&db, self.app.id)
            .await
            .map_err(|e| Fail::new(e.to_string()))?
            .is_empty()
        {
            self.log(
                "Warning: This application uses multiple servers and has persistent storage. Volumes are not shared between servers, so each server has its own data.",
                "stderr",
                false,
            )
            .await;
        }
        // `proxyMismatchReason()`.
        let primary = match self.app.server_id() {
            Some(id) => servers::find_by_id(&db, id).await.ok().flatten(),
            None => None,
        };
        let primary_proxy = primary.as_ref().and_then(proxy_type);
        for destination in additional {
            let Some(row) = servers::find_by_id(&db, destination.server_id)
                .await
                .ok()
                .flatten()
            else {
                continue;
            };
            let proxy = proxy_type(&row);
            let routing = |p: Option<&str>| matches!(p, Some("TRAEFIK" | "CADDY"));
            if routing(primary_proxy.as_deref())
                && routing(proxy.as_deref())
                && primary_proxy != proxy
            {
                let name = row
                    .record
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                self.log(
                    &format!(
                        "Warning: The primary server uses {} and {name} uses {}. All servers of an application must use the same proxy, because the proxy labels are generated for one proxy. The domains of this application will not work through the proxy on {name}.",
                        primary_proxy.as_deref().unwrap_or_default(),
                        proxy.as_deref().unwrap_or_default(),
                    ),
                    "stderr",
                    false,
                )
                .await;
            }
        }
        Ok(())
    }

    /// `pull_image_for_additional_server()`.
    pub(super) async fn pull_image_for_additional_server(&mut self) -> Result<bool, Fail> {
        if !self.is_additional
            || !self
                .app
                .str("docker_registry_image_name")
                .is_some_and(|n| !n.is_empty())
        {
            return Ok(false);
        }
        let img = self.b.production_image.clone();
        let image = quote(&img);
        self.info(&format!(
            "Pulling image ({img}) that the main server pushed. Build step skipped."
        ))
        .await;
        let prefix = docker_config_prefix(&self.server);
        self.exec(vec![
            Cmd::new(format!("{prefix}docker pull {image}")).hidden(),
            Cmd::new(format!("docker images -q {image} 2>/dev/null"))
                .hidden()
                .save("local_image_found", false),
        ])
        .await?;
        if self
            .saved
            .get("local_image_found")
            .is_none_or(|v| v.trim().is_empty())
        {
            let mut message = format!(
                "Image ({img}) not found in the registry; the main server must push it first."
            );
            if !self.server_can_build() {
                message.push_str(&format!(
                    " The server ({}) is set to deployments only and cannot build it.",
                    self.server_name
                ));
            }
            return Err(Fail::new(message));
        }
        Ok(true)
    }

    /// `deploy_to_additional_destinations()`: how many deployments were queued.
    pub(super) async fn deploy_to_additional_destinations(&mut self) -> usize {
        let db = self.jobs.0.db.clone();
        let destinations = ops::additional_networks(&db, self.app.id)
            .await
            .unwrap_or_default();
        if destinations.is_empty() || self.b.is_preview() {
            return 0;
        }
        if self.server.is_swarm() {
            self.info("Additional destinations are not supported in swarm mode.")
                .await;
            return 0;
        }
        if destinations.iter().any(|d| d.id == self.destination_id) {
            return 0;
        }
        let tag = self.b.preview_tag.clone();
        let mut queued = 0;
        for d in destinations {
            let Some(row) = servers::find_by_id(&db, d.server_id).await.ok().flatten() else {
                continue;
            };
            let name = row
                .record
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let server = ServerInfo::from_row(&row);
            if server.team_id != self.main_server.team_id {
                self.info(&format!(
                    "Skipping deployment to {name}. Not in the same team?!"
                ))
                .await;
                continue;
            }
            let deployment_uuid = coolify_domain::new_public_id();
            let result = self
                .jobs
                .queue_application_deployment(
                    &self.app,
                    &deployment_uuid,
                    QueueOptions {
                        commit: Some(self.commit.clone()),
                        no_questions_asked: true,
                        server_id: Some(d.server_id),
                        destination_id: Some(d.id),
                        rollback: self.b.rollback,
                        docker_registry_image_tag: tag.clone(),
                        parent_deployment_uuid: Some(self.dep.clone()),
                        ..Default::default()
                    },
                )
                .await;
            match result {
                Err(e) => {
                    self.log(
                        &format!("Deployment to {name} could not be started: {e}"),
                        "stderr",
                        false,
                    )
                    .await;
                }
                Ok(Queued::Queued { .. }) => {
                    queued += 1;
                    let project = self
                        .app
                        .project
                        .get("uuid")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let environment = self
                        .app
                        .environment
                        .get("uuid")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let base = crate::notifications::Ctx::load(&db).await.base_url;
                    let url = format!(
                        "{base}/project/{project}/environment/{environment}/application/{}/deployment/{deployment_uuid}",
                        self.app.uuid
                    );
                    self.info(&format!("Deployment to {name}. Logs: {url}"))
                        .await;
                }
                Ok(other) => {
                    self.log(
                        &format!("Deployment to {name} was not queued: {}", other.message()),
                        "stderr",
                        false,
                    )
                    .await;
                }
            }
        }
        queued
    }

    /// `sendMultiServerDeploymentNotification($parentDeploymentUuid)`.
    pub(super) async fn send_multi_server_notification(&self, parent: &str) {
        let queued = MULTI_QUEUED.lock().is_ok_and(|s| s.contains(parent));
        if !queued {
            return;
        }
        let db = &self.jobs.0.db;
        let Ok(children) = apps::child_deployments(db, parent).await else {
            return;
        };
        let ended = |s: &str| matches!(s, "finished" | "failed" | "cancelled-by-user");
        if children.iter().any(|(_, status)| !ended(status)) {
            return;
        }
        let first_time = MULTI_NOTIFIED
            .lock()
            .is_ok_and(|mut s| s.insert(parent.to_owned()));
        if !first_time {
            return;
        }
        let unsuccessful = children.iter().find(|(_, s)| s != "finished");
        let (uuid, success) = unsuccessful.map_or((parent, true), |(u, _)| (u.as_str(), false));
        let Ok(Some(row)) = apps::deployment_by_uuid(db, uuid).await else {
            return;
        };
        crate::notifications::lookup::notify_deployment(
            db,
            self.jobs.0.encrypter.as_ref(),
            self.app.id,
            row.id,
            &self.commit,
            success,
        )
        .await;
    }
}

/// `effectiveServerRole()->canBuild()`.
pub(crate) fn can_build(server: &ServerInfo) -> bool {
    server.settings.get("server_role").and_then(Value::as_str) != Some("deployment")
}

/// `serverDockerConfigPrefix()`.
pub(super) fn docker_config_prefix(server: &ServerInfo) -> &'static str {
    if server.is_non_root() {
        "DOCKER_CONTEXT=default DOCKER_CONFIG=\"$(find \"$HOME/.docker/config.json\" -maxdepth 0 -exec dirname {} \\; 2>/dev/null)\" "
    } else {
        ""
    }
}

/// `$server->proxyType()`.
fn proxy_type(row: &servers::ServerRow) -> Option<String> {
    row.record
        .get("proxy")
        .and_then(|p| p.get("type"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}
