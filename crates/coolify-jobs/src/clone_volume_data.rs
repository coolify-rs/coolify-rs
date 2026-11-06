//! `CloneVolumeDataWithSourceStopped`: copies the volume data of a clone in one chain. A running
//! source is stopped before the copy and started again afterwards, also when a step fails. A
//! stopped source stays stopped. When a step fails, the team gets a notification.

use coolify_db::applications::ApplicationRow;
use coolify_db::databases::DatabaseRow;
use coolify_db::servers;
use coolify_db::services::{self as svc, ServiceRow, SubKind};
use coolify_remote::RunOptions;
use coolify_remote::shell::quote;

use crate::Jobs;
use crate::applications::queue::{QueueOptions, Queued};
use crate::compose_parser::php;
use crate::server::ServerInfo;
use coolify_compat::yaml::PhpValue;

/// The resource whose volumes are copied.
#[derive(Debug, Clone)]
pub enum CloneSource {
    Application(ApplicationRow),
    Service(ServiceRow),
    Database(DatabaseRow),
}

/// One `VolumeCloneJob`.
#[derive(Debug, Clone)]
pub struct VolumeCopy {
    pub source_server: i64,
    pub target_server: i64,
    pub source_volume: String,
    /// The id of the new `LocalPersistentVolume` (a database clone does not pass it).
    pub volume_id: Option<i64>,
    pub target_volume: String,
}

/// Why a step of the chain failed.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Failure {
    /// `STOP_FAILED` (`partly: false`) or `APPLICATION_PARTLY_STOPPED`: no volume data was copied.
    Stop { partly: bool, message: String },
    /// A copy or the start afterwards failed.
    Step(String),
}

impl Failure {
    fn message(&self) -> &str {
        match self {
            Self::Stop { message, .. } | Self::Step(message) => message,
        }
    }
}

impl CloneSource {
    fn name(&self) -> &str {
        match self {
            Self::Application(a) => a.str("name").unwrap_or_default(),
            Self::Service(s) => s.name(),
            Self::Database(d) => d.str("name").unwrap_or_default(),
        }
    }

    fn uuid(&self) -> &str {
        match self {
            Self::Application(a) => &a.uuid,
            Self::Service(s) => &s.uuid,
            Self::Database(d) => &d.uuid,
        }
    }

    fn team_id(&self) -> i64 {
        match self {
            Self::Application(a) => a.team_id,
            Self::Service(s) => s.team_id,
            Self::Database(d) => d.team_id,
        }
    }

    fn server_id(&self) -> Option<i64> {
        match self {
            Self::Application(a) => a.server_id(),
            Self::Service(s) => s.server_id(),
            Self::Database(d) => d.server_id(),
        }
    }
}

/// `CloneVolumeDataWithSourceStopped::sourceAbility()`: every one is an admin ability.
pub fn source_ability(source: &CloneSource) -> &'static str {
    match source {
        CloneSource::Application(_) => "deploy",
        CloneSource::Service(_) => "stop",
        CloneSource::Database(_) => "manage",
    }
}

/// `filled($source->status) && ! $source->isExited()`: a service is exited when its status
/// contains `exited`, the others when it starts with it.
fn is_running_status(is_service: bool, status: Option<&str>) -> bool {
    let Some(status) = status.filter(|s| !s.trim().is_empty()) else {
        return false;
    };
    if is_service {
        !status.contains("exited")
    } else {
        !status.starts_with("exited")
    }
}

/// The sentence of `recoverSource()`.
fn copy_failed_message(name: &str) -> String {
    format!(
        "Copying the volume data of '{name}' to its clone failed. The clone may have no or partial data."
    )
}

pub use crate::notifications::php::clone_resource_name;

/// `remove_iip($e->getMessage())`.
fn remove_iip(text: &str) -> String {
    coolify_compat::redact::redact_text(text, &[], &[])
}

impl Jobs {
    /// `CloneVolumeDataWithSourceStopped::run($source, $volumeJobs)`: reads whether the source runs
    /// now, then runs the chain in the background.
    pub async fn clone_volume_data_with_source_stopped(
        &self,
        source: CloneSource,
        copies: Vec<VolumeCopy>,
    ) {
        if copies.is_empty() {
            return;
        }
        let status = self.clone_source_status(&source).await;
        let was_running =
            is_running_status(matches!(source, CloneSource::Service(_)), status.as_deref());
        let jobs = self.clone();
        tokio::spawn(async move {
            jobs.run_clone_chain(&source, &copies, was_running).await;
        });
    }

    /// `$source->status`.
    async fn clone_source_status(&self, source: &CloneSource) -> Option<String> {
        let db = &self.0.db;
        match source {
            CloneSource::Application(a) => a.str("status").map(str::to_owned),
            CloneSource::Database(d) => d.str("status").map(str::to_owned),
            CloneSource::Service(s) => {
                if svc::has_running_start(db, &s.uuid).await.unwrap_or(false) {
                    return Some("starting:unhealthy".into());
                }
                let mut subs = svc::subs(db, SubKind::Application, s.id)
                    .await
                    .unwrap_or_default();
                subs.extend(
                    svc::subs(db, SubKind::Database, s.id)
                        .await
                        .unwrap_or_default(),
                );
                crate::services::status::container_status_of(&subs)
            }
        }
    }

    /// The chain: stop (when running), one copy per volume, start (when it ran), with `catch`.
    async fn run_clone_chain(
        &self,
        source: &CloneSource,
        copies: &[VolumeCopy],
        was_running: bool,
    ) {
        let result = async {
            if was_running {
                self.stop_clone_source(source).await?;
            }
            for c in copies {
                self.clone_volume_data(
                    c.source_server,
                    c.target_server,
                    &c.source_volume,
                    c.volume_id,
                    &c.target_volume,
                )
                .await
                .map_err(Failure::Step)?;
            }
            if was_running {
                self.start_clone_source(source)
                    .await
                    .map_err(Failure::Step)?;
            }
            Ok::<(), Failure>(())
        }
        .await;
        if let Err(e) = result {
            self.recover_clone_source(source, was_running, &e).await;
        }
    }

    /// `stopSource()`: fails, so that no copy runs, when the stop reports an error or a container
    /// of the source still runs.
    async fn stop_clone_source(&self, source: &CloneSource) -> Result<(), Failure> {
        // A partly stopped application is redeployed, so remember which of its containers ran.
        let running_before = match source {
            CloneSource::Application(_) => self.clone_source_containers(source).await.ok(),
            _ => None,
        };
        let result: Option<String> = match source {
            CloneSource::Application(a) => self.stop_application(a, false, true, true).await,
            CloneSource::Service(s) => match self.service_server(s).await {
                Ok(server) if server.is_functional() => {
                    self.stop_service(s, false).await;
                    None
                }
                _ => Some("Server is not functional".into()),
            },
            CloneSource::Database(d) => match self.server_for(d).await {
                Ok(server) if server.is_functional() => {
                    self.stop_database(d, &server, false).await;
                    None
                }
                _ => Some("Server is not functional".into()),
            },
        };
        if let Some(message) = result.filter(|m| !m.is_empty()) {
            let partly = self
                .failed_stop_is_partly(source, running_before.as_deref(), None)
                .await;
            return Err(Failure::Stop { partly, message });
        }
        let running = match self.clone_source_containers(source).await {
            Ok(r) => r,
            Err(e) => {
                return Err(Failure::Stop {
                    partly: false,
                    message: format!("Could not check that the containers stopped: {e}"),
                });
            }
        };
        if !running.is_empty() {
            let partly = self
                .failed_stop_is_partly(source, running_before.as_deref(), Some(&running))
                .await;
            return Err(Failure::Stop {
                partly,
                message: format!("These containers still run: {}", running.join(", ")),
            });
        }
        Ok(())
    }

    /// `failedStopCode()`: an application whose stop took down some of its containers, while
    /// others still run, is partly stopped. Unknown containers before the stop count as partly.
    async fn failed_stop_is_partly(
        &self,
        source: &CloneSource,
        before: Option<&[String]>,
        after: Option<&[String]>,
    ) -> bool {
        if !matches!(source, CloneSource::Application(_)) {
            return false;
        }
        let after: Vec<String> = match after {
            Some(a) => a.to_vec(),
            None => self
                .clone_source_containers(source)
                .await
                .unwrap_or_default(),
        };
        if after.is_empty() {
            return false;
        }
        before.is_none_or(|before| before.iter().any(|b| !after.contains(b)))
    }

    /// `runningContainers()`: the names of the running containers of the source on its main
    /// server. A swarm stack is removed asynchronously, so it is not checked.
    async fn clone_source_containers(&self, source: &CloneSource) -> Result<Vec<String>, String> {
        let server_id = source.server_id().ok_or("The resource has no server.")?;
        let row = servers::find_by_id(&self.0.db, server_id)
            .await
            .map_err(|e| e.to_string())?
            .ok_or("Server not found.")?;
        let server = ServerInfo::from_row(&row);
        if server.is_swarm() {
            return Ok(Vec::new());
        }
        let (kind, extra): (&str, &[&str]) = match source {
            CloneSource::Application(_) => ("application", &["label=coolify.pullRequestId=0"]),
            CloneSource::Service(_) => ("service", &[]),
            CloneSource::Database(_) => ("database", &[]),
        };
        let commands = docker_ps_by_owner_names(kind, source.uuid(), extra);
        let refs: Vec<&str> = commands.iter().map(String::as_str).collect();
        let target = server.target(self).await?;
        let out = self
            .remote()
            .run(&target, &refs, RunOptions::op("clone.running_containers"))
            .await
            .map_err(|e| e.message)?
            .unwrap_or_default();
        let mut names: Vec<String> = Vec::new();
        for name in out.lines().map(str::trim).filter(|n| !n.is_empty()) {
            if !names.iter().any(|n| n == name) {
                names.push(name.to_owned());
            }
        }
        Ok(names)
    }

    /// `startSource()`.
    async fn start_clone_source(&self, source: &CloneSource) -> Result<(), String> {
        match source {
            CloneSource::Application(a) => {
                let deployment = coolify_domain::new_public_id();
                self.queue_application_deployment(
                    a,
                    &deployment,
                    QueueOptions {
                        no_questions_asked: true,
                        ..Default::default()
                    },
                )
                .await
                .map(|_| ())
            }
            CloneSource::Service(s) => self.start_service(s.id, false, false).await,
            CloneSource::Database(d) => {
                self.start_database(d, None).await;
                Ok(())
            }
        }
    }

    /// `recoverSource()`.
    async fn recover_clone_source(&self, source: &CloneSource, was_running: bool, e: &Failure) {
        if let Failure::Stop { partly, .. } = e {
            self.recover_from_failed_stop(source, *partly, e.message())
                .await;
            return;
        }
        let name = source.name();
        let mut message = copy_failed_message(name);
        if was_running {
            match self.start_clone_source(source).await {
                Ok(()) => message.push_str(&format!(" Coolify started '{name}' again.")),
                Err(start) => {
                    tracing::error!(source = %name, error = %start, "Failed to start the source again after a failed volume clone");
                    message.push_str(&format!(
                        " Coolify could not start '{name}' again. Start it manually."
                    ));
                }
            }
        }
        self.notify_clone_failure(source, &message, e.message());
    }

    /// `recoverFromFailedStop()`: no data was copied.
    async fn recover_from_failed_stop(&self, source: &CloneSource, partly: bool, error: &str) {
        let name = source.name();
        let mut message = format!(
            "Coolify did not copy the volume data of '{name}' to its clone, because stopping '{name}' failed. The clone has no copied data."
        );
        let still_running = self
            .clone_source_containers(source)
            .await
            .is_ok_and(|r| !r.is_empty());
        match source {
            CloneSource::Service(s) if still_running => {
                match self.start_stopped_service_containers(s).await {
                    Ok(()) => message.push_str(&format!(
                        " Some containers of '{name}' were still running, so Coolify started its stopped containers again."
                    )),
                    Err(start) => {
                        tracing::error!(source = %name, error = %start, "Failed to start the stopped containers after a failed stop for a volume clone");
                        message.push_str(&format!(
                            " Some containers of '{name}' are still running. Coolify could not start its stopped containers again. Start '{name}' manually."
                        ));
                    }
                }
            }
            CloneSource::Application(a) if still_running && partly => {
                message.push(' ');
                message.push_str(&self.redeploy_partly_stopped_application(a).await);
            }
            _ if still_running => {
                message.push_str(&format!(" '{name}' is still running."));
            }
            _ => match self.start_clone_source(source).await {
                Ok(()) => message.push_str(&format!(" Coolify started '{name}' again.")),
                Err(start) => {
                    tracing::error!(source = %name, error = %start, "Failed to start the source again after a failed stop for a volume clone");
                    message.push_str(&format!(
                        " Coolify could not start '{name}' again. Start it manually."
                    ));
                }
            },
        }
        self.notify_clone_failure(source, &message, error);
    }

    /// `$source->team()?->notify(new GeneralNotification($message.' Error: '.remove_iip(...)))`.
    fn notify_clone_failure(&self, source: &CloneSource, message: &str, error: &str) {
        crate::notifications::spawn_team_general(
            &self.0.db,
            self.0.encrypter.as_ref(),
            source.team_id(),
            format!("{message} Error: {}", remove_iip(error)),
            false,
        );
    }

    /// `redeployPartlyStoppedApplication()`: a deployment of the commit of the last successful
    /// deployment, so the running containers are replaced only by the same version.
    async fn redeploy_partly_stopped_application(&self, a: &ApplicationRow) -> String {
        let name = a.str("name").unwrap_or_default();
        let commit: Option<String> = sqlx::query_scalar(
            "SELECT \"commit\" FROM application_deployment_queues WHERE application_id = $1 \
             AND status = 'finished' AND pull_request_id = 0 AND \"commit\" IS NOT NULL \
             AND \"commit\" <> 'HEAD' ORDER BY updated_at DESC, id DESC LIMIT 1",
        )
        .bind(a.id.to_string())
        .fetch_optional(self.0.db.pool())
        .await
        .ok()
        .flatten();
        let deployment = coolify_domain::new_public_id();
        let queued = self
            .queue_application_deployment(
                a,
                &deployment,
                QueueOptions {
                    commit: commit.clone(),
                    no_questions_asked: true,
                    ..Default::default()
                },
            )
            .await;
        let failed = match queued {
            Ok(Queued::Queued { .. }) => None,
            Ok(other) => Some(other.message().to_owned()),
            Err(e) => Some(e),
        };
        if let Some(e) = failed {
            tracing::error!(application = %a.uuid, error = %e, "Failed to queue a redeploy after a failed stop for a volume clone");
            return format!(
                "Some containers of '{name}' stopped and others are still running. Coolify could not queue a redeploy of '{name}' to bring back its stopped containers. Redeploy it manually."
            );
        }
        let sentence = format!(
            "Some containers of '{name}' stopped and others were still running, so Coolify started a redeploy of '{name}' to bring back its stopped containers"
        );
        match commit {
            None => format!(
                "{sentence}. Coolify could not find the commit that ran before, so the redeploy uses the configured commit or image tag."
            ),
            Some(c) => format!("{sentence} with the commit that ran before ({c})."),
        }
    }

    /// `startStoppedServiceContainers()`: creates and starts the containers that the stop removed,
    /// with the Compose file of the last start. Running containers are not recreated.
    async fn start_stopped_service_containers(&self, s: &ServiceRow) -> Result<(), String> {
        let server = self.service_server(s).await?;
        let target = server.target(self).await?;
        let dir = crate::services::workdir(&s.uuid);
        let compose = format!(
            "{}docker compose --project-directory {dir} -f {dir}/docker-compose.yml --project-name {}",
            crate::services::docker_config_prefix(&server),
            s.uuid
        );
        let mut commands: Vec<String> = Vec::new();
        if s.bool("connect_to_docker_network") {
            // Like StartService: connect new containers to the predefined network before they start.
            commands.push(format!(
                "{compose} up -d --no-recreate --no-start < /dev/null"
            ));
            let network = self.service_network(s).await;
            if let Ok(yaml) = crate::compose_parser::validate::parse_docker_compose_yaml(
                s.str("docker_compose").unwrap_or_default(),
            ) && let Some(PhpValue::Array(services)) = php::get(&yaml, "services")
            {
                for (name, config) in services {
                    if let Some(c) = crate::services::predefined_network_connect_command(
                        &s.uuid,
                        &network,
                        &name.as_string(),
                        Some(config),
                    ) {
                        commands.push(c);
                    }
                }
            }
            commands.push(format!("{compose} start < /dev/null"));
        } else {
            commands.push(format!("{compose} up -d --no-recreate < /dev/null"));
        }
        let refs: Vec<&str> = commands.iter().map(String::as_str).collect();
        self.remote()
            .run(&target, &refs, RunOptions::op("clone.start_stopped"))
            .await
            .map(|_| ())
            .map_err(|e| e.message)
    }
}

/// `dockerPsByOwnerCommands($type, $uuid, $filters, '{{.Names}}', all: false)`.
fn docker_ps_by_owner_names(kind: &str, uuid: &str, extra: &[&str]) -> Vec<String> {
    let filters = |values: &[String]| {
        values
            .iter()
            .map(|f| format!("--filter {}", quote(f)))
            .collect::<Vec<_>>()
            .join(" ")
    };
    let format = format!("--format {}", quote("{{.Names}}"));
    let extra: Vec<String> = extra.iter().map(|f| (*f).to_owned()).collect();
    let with = |base: Vec<String>| {
        let mut all = base;
        all.extend(extra.iter().cloned());
        format!("docker ps {} {format}", filters(&all))
    };
    let mut commands = vec![
        with(vec![format!("label=coolify.{kind}Uuid={uuid}")]),
        with(vec![
            format!("label=coolify.{kind}Id"),
            format!("label=com.docker.compose.project={uuid}"),
        ]),
    ];
    if kind == "application" {
        commands.push(with(vec![
            "label=coolify.applicationId".into(),
            format!("name={uuid}"),
        ]));
    }
    commands
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_commands_match_upstream() {
        let c = docker_ps_by_owner_names("application", "abc", &["label=coolify.pullRequestId=0"]);
        assert_eq!(
            c,
            [
                "docker ps --filter 'label=coolify.applicationUuid=abc' --filter 'label=coolify.pullRequestId=0' --format '{{.Names}}'",
                "docker ps --filter 'label=coolify.applicationId' --filter 'label=com.docker.compose.project=abc' --filter 'label=coolify.pullRequestId=0' --format '{{.Names}}'",
                "docker ps --filter 'label=coolify.applicationId' --filter 'name=abc' --filter 'label=coolify.pullRequestId=0' --format '{{.Names}}'",
            ]
        );
        let d = docker_ps_by_owner_names("database", "xyz", &[]);
        assert_eq!(d.len(), 2);
        assert_eq!(
            d[0],
            "docker ps --filter 'label=coolify.databaseUuid=xyz' --format '{{.Names}}'"
        );
    }

    #[test]
    fn running_status_follows_is_exited() {
        assert!(!is_running_status(true, None));
        assert!(!is_running_status(true, Some("")));
        assert!(!is_running_status(true, Some("degraded:exited")));
        assert!(is_running_status(true, Some("running:healthy")));
        assert!(!is_running_status(false, Some("exited:unhealthy")));
        assert!(is_running_status(false, Some("running:healthy")));
        assert!(is_running_status(false, Some("degraded:exited")));
    }

    #[test]
    fn clone_names_do_not_grow() {
        let uuid = "abcdefghijklmnopqrstuvwx";
        assert_eq!(
            clone_resource_name("orders", uuid),
            format!("orders-clone-{uuid}")
        );
        assert_eq!(
            clone_resource_name("orders-clone-k0w4gc8ssoc4o8sc-clone-1234567", uuid),
            format!("orders-clone-{uuid}")
        );
        // A suffix shorter than seven characters is part of the name.
        assert_eq!(
            clone_resource_name("orders-clone-abc", uuid),
            format!("orders-clone-abc-clone-{uuid}")
        );
        let long = "x".repeat(300);
        let name = clone_resource_name(&long, uuid);
        assert_eq!(name.chars().count(), 255);
        assert!(name.ends_with(&format!("-clone-{uuid}")));
        // `rtrim` after the cut.
        let spaced = format!("{} {}", "a".repeat(223), "b".repeat(40));
        assert_eq!(
            clone_resource_name(&spaced, uuid),
            format!("{}-clone-{uuid}", "a".repeat(223))
        );
    }

    #[test]
    fn messages_match_upstream() {
        assert_eq!(
            copy_failed_message("db"),
            "Copying the volume data of 'db' to its clone failed. The clone may have no or partial data."
        );
    }
}
