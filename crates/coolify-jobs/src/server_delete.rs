//! Deleting a server: `ServersController::delete_server` and `Server\\Delete::delete` after their checks,
//! the `deleted` hook and the queued `DeleteResourceJob`s and `DeleteServer`.

use coolify_db::servers::ServerRow;
use coolify_db::{
    applications as apps, databases as dbs, github_runners, servers, services as svc,
};

use crate::Jobs;
use crate::cloud::{self, DeleteFrom};

impl Jobs {
    /// Returns once the server is soft deleted; the rest runs behind the response, like upstream's queue.
    ///
    /// The server is already soft deleted when the resource jobs run, so `destination.server` is empty for
    /// them: they remove the rows and leave what runs on the server. The reference does the same
    /// (`compat/diff/server_delete_diff.py`).
    pub async fn delete_server(
        &self,
        row: ServerRow,
        force: bool,
        from: DeleteFrom,
    ) -> coolify_db::DbResult<()> {
        let db = &self.0.db;
        let mut handles = Vec::new();
        // `$server->detachAdditionalApplications()` (v4.7.5): applications that use the server
        // only as an additional destination stay; they lose this server. Their containers on
        // this server are stopped and removed (best effort).
        let detached = servers::additional_applications(db, row.id).await?;
        let detached_ids: Vec<i64> = detached.iter().map(|a| a.id).collect();
        let on_server = if force || !detached.is_empty() {
            apps::list_for_server(db, row.id).await?
        } else {
            Vec::new()
        };
        for application in on_server.iter().filter(|a| detached_ids.contains(&a.id)) {
            if let Some(error) = self.stop_application_on(application, row.id).await {
                tracing::warn!(
                    application_id = application.id,
                    server_id = row.id,
                    error = %error,
                    "Could not stop the containers of an application on a deleted server."
                );
            }
        }
        servers::detach_additional_applications(db, row.id, &detached_ids).await?;
        // `$server->primaryResources()`.
        let (applications, databases, services) = if force {
            (
                on_server
                    .into_iter()
                    .filter(|a| !detached_ids.contains(&a.id))
                    .collect(),
                dbs::list_for_server(db, row.id).await?,
                svc::list_for_server(db, row.id).await?,
            )
        } else {
            (Vec::new(), Vec::new(), Vec::new())
        };
        servers::soft_delete(db, row.id, Self::now()).await?;
        self.fail_queued_deployments(row.id).await;
        // `DeleteResourceJob::dispatch($resource)`: the default arguments.
        for a in applications {
            handles.push(self.spawn_application_delete(a, true, true, true));
        }
        for d in databases {
            handles.push(self.spawn_database_delete(d, true, true, true));
        }
        for s in services {
            handles.push(self.spawn_service_delete(s, true, true, true));
        }
        // Nothing to wait for or call: the rows go before the response.
        if handles.is_empty() && !from.any() {
            self.finish_delete_server(&row, from).await;
            return Ok(());
        }
        let jobs = self.clone();
        tokio::spawn(async move {
            for h in handles {
                let _ = h.await;
            }
            jobs.finish_delete_server(&row, from).await;
        });
        Ok(())
    }

    /// `DeleteServer::handle`.
    async fn finish_delete_server(&self, row: &ServerRow, from: DeleteFrom) {
        // v4.7.5 `stopSentinel()`: a Sentinel that keeps running would push to Coolify for a
        // server that no longer exists.
        if !from.hetzner && !from.vultr && !from.digitalocean {
            self.stop_sentinel_before_delete(row).await;
        }
        if let Err(e) = cloud::delete_from_providers(self, row, from).await {
            tracing::error!(server_uuid = %row.uuid, error = %e, "DeleteServer failed; the server stays soft deleted");
            return;
        }
        // `GithubRunnerExecution::deleteAndDeregister`; the rows go with `forceDelete()`.
        for e in github_runners::occupying_on_server(&self.0.db, row.id)
            .await
            .unwrap_or_default()
        {
            if let Some(runner_id) = e.runner_id {
                self.spawn_deregister_runner(e.github_app_id, runner_id);
            }
        }
        if let Err(e) = servers::force_delete(&self.0.db, row.id).await {
            tracing::error!(server_uuid = %row.uuid, error = %e, "Failed to force delete server from Coolify");
        }
        self.remote().forget_host_key(&row.uuid);
    }

    /// `DeleteServer::stopSentinel()`.
    async fn stop_sentinel_before_delete(&self, row: &ServerRow) {
        let info = crate::server::ServerInfo::from_row(row);
        if info.transferred
            || info.setting_bool("is_sentinel_enabled") != Some(true)
            || !info.is_functional()
        {
            return;
        }
        let Ok(target) = info.target(self).await else {
            return;
        };
        // `instant_remote_process([...], $server, false, timeout: 30, maxAttempts: 1)`.
        if let Err(e) = self
            .remote()
            .run(
                &target,
                &["docker rm -f coolify-sentinel"],
                coolify_remote::RunOptions::op("server.delete.stop_sentinel")
                    .attempts(1)
                    .timeout(std::time::Duration::from_secs(30)),
            )
            .await
        {
            tracing::warn!(
                server_id = row.id,
                error = %e.message,
                "Failed to stop Sentinel before deleting the server"
            );
        }
    }

    /// `Server::failQueuedDeployments`.
    async fn fail_queued_deployments(&self, server_id: i64) {
        let db = &self.0.db;
        let Ok(queued) = apps::queued_on_server(db, server_id).await else {
            return;
        };
        for d in queued {
            let updated = sqlx::query(
                "UPDATE application_deployment_queues SET status = 'failed', updated_at = $2 WHERE id = $1 AND status = 'queued'",
            )
            .bind(d.id)
            .bind(Self::now())
            .execute(db.pool())
            .await
            .map(|r| r.rows_affected())
            .unwrap_or(0);
            if updated > 0 {
                let now = Self::now();
                let entry = apps::LogEntry {
                    command: None,
                    output: "The server was deleted.".to_owned(),
                    kind: "stderr",
                    timestamp: now.format("%Y-%m-%dT%H:%M:%S%.6fZ").to_string(),
                    hidden: false,
                    batch: 1,
                };
                let _ = apps::append_log(db, d.id, &[entry], now).await;
            }
        }
    }
}
