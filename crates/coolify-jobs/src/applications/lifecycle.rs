//! `StopApplication` and `DeleteResourceJob` for applications on one server.

use chrono::Utc;
use coolify_db::applications::{self as apps, ApplicationRow};
use coolify_db::servers::{self, ColumnValue};
use coolify_remote::RunOptions;
use coolify_remote::shell::quote;
use serde_json::Value;

use crate::Jobs;
use crate::server::ServerInfo;

impl Jobs {
    /// The application's containers on its server (`getCurrentApplicationContainerStatus`).
    async fn application_containers(
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
    /// the error message when something failed (nothing is written then).
    pub async fn stop_application(
        &self,
        a: &ApplicationRow,
        previews: bool,
        reset_restart_count: bool,
        remove: bool,
    ) -> Option<String> {
        let db = &self.0.db;
        let server_id = a.server_id()?;
        let row = servers::find_by_id(db, server_id).await.ok().flatten()?;
        let server = ServerInfo::from_row(&row);
        let name = row
            .record
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let mut errors: Vec<String> = Vec::new();
        if !server.is_functional() {
            errors.push(format!("Server {name} is not functional."));
        } else {
            match server.target(self).await {
                Err(e) => errors.push(e),
                Ok(t) => match self.application_containers(&t, &a.uuid, previews).await {
                    Err(e) => errors.push(e),
                    Ok(containers) => {
                        let grace = a
                            .setting_int("stop_grace_period")
                            .filter(|g| (1..=3600).contains(g))
                            .unwrap_or(30);
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
                                .run(&t, &refs, RunOptions::op("application.stop"))
                                .await
                            {
                                errors.push(e.message);
                            }
                        }
                    }
                },
            }
        }
        if !errors.is_empty() {
            let message = errors.join(" ");
            tracing::warn!(application = %a.uuid, error = %message, "Failed to stop application");
            return Some(message);
        }
        let mut values = vec![
            ("status", ColumnValue::Text(Some("exited:unhealthy".into()))),
            ("container_present", ColumnValue::Bool(!remove)),
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

    /// `StopApplication::dispatch(...)` from the API and the UI.
    pub fn spawn_application_stop(&self, a: ApplicationRow) {
        let jobs = self.clone();
        tokio::spawn(async move {
            jobs.stop_application(&a, false, true, true).await;
        });
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
            let _ = jobs.stop_application(&a, true, true, true).await;
            if let Some(server_id) = a.server_id()
                && let Ok(Some(row)) = servers::find_by_id(&db, server_id).await
                && let Ok(t) = ServerInfo::from_row(&row).target(&jobs).await
            {
                let mut cmds: Vec<String> = Vec::new();
                if delete_configurations {
                    cmds.push(format!("rm -rf /data/coolify/applications/{}", a.uuid));
                }
                if delete_volumes {
                    for (name, _, host) in apps::persistent_volumes(&db, a.id)
                        .await
                        .unwrap_or_default()
                    {
                        if host.as_deref().is_none_or(str::is_empty) {
                            cmds.push(format!("docker volume rm -f {}", quote(&name)));
                        }
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
            if let Err(e) = apps::force_delete(&db, a.id, Utc::now().naive_utc()).await {
                tracing::warn!(application = %a.uuid, error = %e, "application rows could not be deleted");
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
