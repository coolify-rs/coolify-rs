//! `StopApplication` and `DeleteResourceJob` for applications on one server.

use chrono::Utc;
use coolify_db::applications::{self as apps, ApplicationRow};
use coolify_db::resource_ops as ops;
use coolify_db::servers::{self, ColumnValue};
use coolify_remote::RunOptions;
use coolify_remote::shell::quote;
use serde_json::Value;

use crate::Jobs;
use crate::server::ServerInfo;

impl Jobs {
    /// The application's containers on its server (`getCurrentApplicationContainerStatus`).
    pub async fn application_containers(
        &self,
        target: &coolify_remote::Target,
        uuid: &str,
        previews: bool,
    ) -> Result<Vec<Value>, String> {
        let lines = [
            format!(
                "docker ps -a --filter 'label=coolify.applicationUuid={uuid}' --format '{{{{json .}}}}'"
            ),
            format!(
                "docker ps -a --filter 'label=coolify.applicationId' --filter 'label=com.docker.compose.project={uuid}' --format '{{{{json .}}}}'"
            ),
            format!(
                "docker ps -a --filter 'label=coolify.applicationId' --filter 'name={uuid}' --format '{{{{json .}}}}'"
            ),
        ];
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let out = self
            .remote()
            .run(target, &refs, RunOptions::op("application.containers"))
            .await
            .map_err(|e| e.message)?
            .unwrap_or_default();
        let mut ids: Vec<String> = Vec::new();
        let mut containers = Vec::new();
        for line in out.lines() {
            let Ok(v) = serde_json::from_str::<Value>(line.trim()) else {
                continue;
            };
            let id = v
                .get("ID")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            if ids.contains(&id) {
                continue;
            }
            ids.push(id);
            let labels = v.get("Labels").and_then(Value::as_str).unwrap_or_default();
            if previews
                || !labels.contains("coolify.pullRequestId=")
                || labels.contains("coolify.pullRequestId=0")
            {
                containers.push(v);
            }
        }
        Ok(containers)
    }

    /// `StopApplication::handle($application, $previewDeployments, ..., $resetRestartCount, $removeContainers)`:
    /// the error message when something failed (nothing is written then). Runs on the main server and
    /// on every additional server.
    pub async fn stop_application(
        &self,
        a: &ApplicationRow,
        previews: bool,
        reset_restart_count: bool,
        remove: bool,
    ) -> Option<String> {
        let db = &self.0.db;
        let server_id = a.server_id()?;
        let mut server_ids = vec![server_id];
        server_ids.extend(
            ops::additional_server_ids(db, a.id)
                .await
                .unwrap_or_default()
                .into_iter()
                .filter(|id| *id != server_id),
        );
        let mut container_present = !remove;
        let mut errors: Vec<String> = Vec::new();
        for id in server_ids {
            let Some(row) = servers::find_by_id(db, id).await.ok().flatten() else {
                continue;
            };
            let server = ServerInfo::from_row(&row);
            if !server.is_functional() {
                let name = row
                    .record
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                errors.push(format!("Server {name} is not functional."));
                continue;
            }
            match server.target(self).await {
                Err(e) => errors.push(e),
                Ok(t) if server.is_swarm() => {
                    container_present = false;
                    let stack = format!("docker stack rm {}", quote(&a.uuid));
                    if let Err(e) = self
                        .remote()
                        .run(&t, &[&stack], RunOptions::op("application.stop"))
                        .await
                    {
                        errors.push(e.message);
                    }
                }
                Ok(t) => {
                    errors.extend(
                        self.stop_containers_on(a, &server, &t, previews, remove)
                            .await,
                    );
                }
            }
        }
        if !errors.is_empty() {
            let message = errors.join(" ");
            tracing::warn!(application = %a.uuid, error = %message, "Failed to stop application");
            return Some(message);
        }
        let mut values = vec![
            ("status", ColumnValue::Text(Some("exited:unhealthy".into()))),
            ("container_present", ColumnValue::Bool(container_present)),
        ];
        if reset_restart_count {
            values.extend([
                ("restart_count", ColumnValue::Int(0)),
                ("last_restart_at", ColumnValue::Text(None)),
                ("last_restart_type", ColumnValue::Text(None)),
                ("restart_limit_reached", ColumnValue::Bool(false)),
            ]);
        }
        let _ = apps::update_if_changed(db, a.id, &values, Utc::now().naive_utc()).await;
        None
    }

    /// `StopApplicationOneServer::run`: the error message when something failed.
    pub async fn stop_application_on(&self, a: &ApplicationRow, server_id: i64) -> Option<String> {
        let db = &self.0.db;
        let row = servers::find_by_id(db, server_id).await.ok().flatten()?;
        let server = ServerInfo::from_row(&row);
        if !server.is_functional() {
            return Some("Server is not functional".to_owned());
        }
        let t = match server.target(self).await {
            Ok(t) => t,
            Err(e) => return Some(e),
        };
        let errors = self.stop_containers_on(a, &server, &t, false, true).await;
        (!errors.is_empty()).then(|| errors.join(" "))
    }

    /// The loop over `getCurrentApplicationContainerStatus()` of `StopApplication`.
    async fn stop_containers_on(
        &self,
        a: &ApplicationRow,
        server: &ServerInfo,
        t: &coolify_remote::Target,
        previews: bool,
        remove: bool,
    ) -> Vec<String> {
        let containers = match self.application_containers(t, &a.uuid, previews).await {
            Ok(c) => c,
            Err(e) => return vec![e],
        };
        let grace = a
            .setting_int("stop_grace_period")
            .filter(|g| (1..=3600).contains(g))
            .unwrap_or(30);
        let mut errors = Vec::new();
        for c in containers {
            let Some(n) = c.get("Names").and_then(Value::as_str) else {
                continue;
            };
            let mut cmds = vec![crate::databases::docker_stop_command(
                u32::try_from(grace).unwrap_or(30),
                &quote(n),
                server.docker_version(),
            )];
            if remove {
                cmds.push(format!("docker rm -f {}", quote(n)));
            }
            let refs: Vec<&str> = cmds.iter().map(String::as_str).collect();
            if let Err(e) = self
                .remote()
                .run(t, &refs, RunOptions::op("application.stop"))
                .await
            {
                errors.push(e.message);
            }
        }
        errors
    }

    /// `StopApplication::dispatch(...)` from the API and the UI.
    pub fn spawn_application_stop(&self, a: ApplicationRow) {
        let jobs = self.clone();
        tokio::spawn(async move {
            jobs.stop_application(&a, false, true, true).await;
        });
    }

    /// `StopApplicationPreview::handle`.
    pub async fn stop_application_preview(
        &self,
        a: &ApplicationRow,
        preview_id: i64,
        pull_request_id: i64,
        reset_restart_count: bool,
        remove_container: bool,
    ) {
        let db = &self.0.db;
        let now = Self::now();
        if let Some(server_id) = a.server_id()
            && let Ok(Some(row)) = servers::find_by_id(db, server_id).await
            && let server = ServerInfo::from_row(&row)
            && let Ok(t) = server.target(self).await
        {
            let label = format!("coolify.pullRequestId={pull_request_id}");
            let containers = self
                .application_containers(&t, &a.uuid, true)
                .await
                .unwrap_or_default();
            let grace = a
                .setting_int("stop_grace_period")
                .filter(|g| (1..=3600).contains(g))
                .unwrap_or(30);
            for c in containers {
                let labels = c.get("Labels").and_then(Value::as_str).unwrap_or_default();
                if !labels.split(',').any(|l| l == label) {
                    continue;
                }
                let Some(n) = c.get("Names").and_then(Value::as_str) else {
                    continue;
                };
                let mut cmds = vec![crate::databases::docker_stop_command(
                    u32::try_from(grace).unwrap_or(30),
                    &quote(n),
                    server.docker_version(),
                )];
                if remove_container {
                    cmds.push(format!("docker rm -f {}", quote(n)));
                }
                let refs: Vec<&str> = cmds.iter().map(String::as_str).collect();
                let _ = self
                    .remote()
                    .run_quiet(&t, &refs, RunOptions::op("application.stop"))
                    .await;
            }
        }
        let _ = coolify_db::application_ops::update_preview(
            db,
            preview_id,
            &[("status", ColumnValue::Text(Some("exited".into())))],
            now,
        )
        .await;
        if reset_restart_count {
            let _ =
                coolify_db::application_ops::reset_preview_restart_limit(db, preview_id, now).await;
        }
    }

    /// `DeleteResourceJob` for an application (already soft deleted).
    pub fn spawn_application_delete(
        &self,
        a: ApplicationRow,
        delete_volumes: bool,
        delete_configurations: bool,
        delete_networks: bool,
    ) {
        let jobs = self.clone();
        tokio::spawn(async move {
            let db = jobs.0.db.clone();
            if let Some(e) = jobs.stop_application(&a, true, true, true).await
                && e != "Server is not functional"
                && let Some(server_id) = a.server_id()
            {
                jobs.notify_remote_cleanup_failed(
                    a.str("name").unwrap_or_default(),
                    a.int("environment_id"),
                    server_id,
                    &e,
                )
                .await;
            }
            if let Some(server_id) = a.server_id()
                && let Ok(Some(row)) = servers::find_by_id(&db, server_id).await
                && let Ok(t) = ServerInfo::from_row(&row).target(&jobs).await
            {
                let mut cmds: Vec<String> = Vec::new();
                if delete_configurations {
                    cmds.push(format!("rm -rf /data/coolify/applications/{}", a.uuid));
                }
                if delete_volumes {
                    // `deleteVolumes()`: skips volumes shared with a SQLite database.
                    for name in ops::own_volume_names(&db, a.id).await.unwrap_or_default() {
                        cmds.push(format!("docker volume rm -f {}", quote(&name)));
                    }
                }
                if delete_networks {
                    cmds.push(format!(
                        "docker network disconnect {} coolify-proxy",
                        a.uuid
                    ));
                    cmds.push(format!("docker network rm {}", a.uuid));
                }
                // One SSH call each, errors ignored.
                for cmd in cmds {
                    let _ = jobs
                        .remote()
                        .run_quiet(&t, &[&cmd], RunOptions::op("application.delete"))
                        .await;
                }
            }
            // `deleteScheduledVolumeBackups()`: failures are logged, the local rows still go.
            let server = match a.server_id() {
                Some(id) => servers::find_by_id(&db, id)
                    .await
                    .ok()
                    .flatten()
                    .map(|r| ServerInfo::from_row(&r)),
                None => None,
            };
            for backup in ops::volume_backup_schedules(&db, a.id)
                .await
                .unwrap_or_default()
            {
                if let Err(e) = jobs
                    .delete_volume_schedule(&backup, server.as_ref(), true, true)
                    .await
                {
                    tracing::warn!(application = %a.uuid, error = %e, "Remote backup cleanup failed while deleting resource");
                }
            }
            // `$resource->maintenanceServers()` before the rows go.
            let maintenance_servers = if a.bool("is_maintenance_enabled") {
                let mut ids: Vec<i64> = a.server_id().into_iter().collect();
                ids.extend(
                    ops::additional_server_ids(&db, a.id)
                        .await
                        .unwrap_or_default(),
                );
                ids
            } else {
                Vec::new()
            };
            if let Err(e) = apps::force_delete(&db, a.id, Utc::now().naive_utc()).await {
                tracing::warn!(application = %a.uuid, error = %e, "application rows could not be deleted");
            }
            if let Err(e) =
                crate::server_ops::proxy::sync_maintenance_pages(&jobs, &maintenance_servers).await
            {
                tracing::warn!(application = %a.uuid, error = %e, "Maintenance page cleanup failed while deleting resource.");
            }
        });
    }
}

impl Jobs {
    /// `CleanupCancelledDeployment::run`: removes the helper container of a cancelled deployment.
    pub async fn cleanup_cancelled_deployment(&self, d: &apps::Deployment) {
        let db = &self.0.db;
        let server_id = d.int("server_id").unwrap_or_default();
        let message = async {
            let row = servers::find_by_id(db, server_id).await.ok().flatten()?;
            let t = ServerInfo::from_row(&row).target(self).await.ok()?;
            let out = self
                .remote()
                .run_quiet(&t, &[&format!("docker ps -a --filter name={} --format '{{{{.Names}}}}'", d.uuid)], RunOptions::op("deployment.cancel"))
                .await
                .unwrap_or_default();
            if out.lines().any(|l| l.trim() == d.uuid) {
                let _ = self.remote().run_quiet(&t, &[&format!("docker rm -f {}", d.uuid)], RunOptions::op("deployment.cancel")).await;
                Some("Deployment container stopped.")
            } else {
                Some("Deployment container not yet started. Will be cancelled when job checks status.")
            }
        }
        .await;
        let now = Utc::now().naive_utc();
        if let Some(m) = message {
            let entry = apps::LogEntry {
                command: None,
                output: m.to_owned(),
                kind: "stdout",
                timestamp: Utc::now().format("%Y-%m-%dT%H:%M:%S%.6fZ").to_string(),
                hidden: false,
                batch: 1,
            };
            let _ = apps::append_log(db, d.id, &[entry], now).await;
        }
        let _ = apps::update_deployment(
            db,
            d.id,
            &[("current_process_id", ColumnValue::Text(None))],
            now,
        )
        .await;
    }
}

impl Jobs {
    /// `InternalAccess::loadCurrentInternalHostname`: the first running container, else the first one.
    pub async fn application_container_name(&self, a: &ApplicationRow) -> Option<String> {
        let row = servers::find_by_id(&self.0.db, a.server_id()?)
            .await
            .ok()
            .flatten()?;
        let t = ServerInfo::from_row(&row).target(self).await.ok()?;
        let containers = self.application_containers(&t, &a.uuid, false).await.ok()?;
        let running = containers
            .iter()
            .find(|c| c.get("State").and_then(Value::as_str) == Some("running"));
        running
            .or(containers.first())
            .and_then(|c| c.get("Names").and_then(Value::as_str))
            .map(str::to_owned)
    }
}
