//! `$server->delete()` and the queued `DeleteServer` job, with the `DeleteResourceJob`s that
//! `ServersController::delete_server` and `Livewire\Server\Delete::delete` dispatch for `force`.

use coolify_db::applications as apps;
use coolify_db::databases as dbs;
use coolify_db::servers;
use coolify_db::services as svc;

use crate::Jobs;

impl Jobs {
    /// Upstream's jobs start after the soft delete: `destination.server` is then empty, so a
    /// resource loses only its rows ("A deleted server has nothing left to clean up"). Its containers
    /// stay on the host. `delete_from_provider` runs first and is the caller's decision.
    pub async fn delete_server(&self, server_id: i64, force: bool) -> coolify_db::DbResult<()> {
        let db = &self.0.db;
        let now = Self::now();
        let resources = if force {
            servers::defined_resources(db, server_id).await?
        } else {
            servers::DefinedResources::default()
        };
        servers::soft_delete(db, server_id, now).await?;
        // `static::deleted`: `failQueuedDeployments()`.
        servers::fail_queued_deployments(db, server_id, now).await?;
        for id in resources.applications {
            if let Some(a) = apps::find_by_id(db, id).await? {
                self.spawn_application_delete(a, true, true, true);
            }
        }
        for (kind, id) in resources.databases {
            if let Some(d) = dbs::find_by_id(db, kind, id).await? {
                self.spawn_database_delete(d, true, true, true);
            }
        }
        for id in resources.services {
            if let Some(s) = svc::find_by_id(db, id).await? {
                self.spawn_service_delete(s, true, true, true);
            }
        }
        // `static::forceDeleting`: `GithubRunnerExecution::deleteAndDeregister`.
        for (app, runner) in servers::occupying_runners(db, server_id).await? {
            self.spawn_deregister_runner(app, runner);
        }
        servers::delete(db, server_id, now).await
    }
}
