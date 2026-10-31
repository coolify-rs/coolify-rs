//! The scheduled jobs that act on resources and on servers: `cleanup:stucked-resources`,
//! `CleanupHelperContainersJob`, `CleanupOrphanedPreviewContainersJob`, `RegenerateSslCertJob`,
//! `ServerCloudProviderStatusCheckJob`, and the `save()` of `S3Storage::testConnection`.

use std::time::Duration;

use coolify_db::applications as apps;
use coolify_db::database_parts as parts;
use coolify_db::databases::{self as dbs, Kind};
use coolify_db::instance_jobs as queries;
use coolify_db::records::Record;
use coolify_db::servers::{self, ServerRow};
use coolify_db::services as svc;
use coolify_remote::RunOptions;
use coolify_remote::shell::quote;
use regex::Regex;
use serde_json::Value;

use crate::Jobs;
use crate::notifications::{self, Mail, Notification, SslResource, lookup, notify};
use crate::scheduler::allowed;
use crate::server::ServerInfo;

/// `CleanupHelperContainersJob::helperContainersCommand`.
const HELPER_CONTAINERS_COMMAND: &str = r#"docker container ps --format '{{json .}}' | jq -s 'map(select(.Image|test("(^|/)coollabsio/coolify-helper(:|$)")))'"#;

/// `ServerCloudProviderStatusCheckJob::$timeout`.
const PROVIDER_CHECK_TIMEOUT: Duration = Duration::from_secs(120);

impl Jobs {
    /// `ServerCloudProviderStatusCheckJob::dispatch($server)`. `Server::refresh*State` also keeps a
    /// transferred server away from the provider.
    pub(crate) fn spawn_cloud_provider_status_check(&self, server_id: i64) {
        let jobs = self.clone();
        tokio::spawn(async move {
            let Ok(Some(row)) = servers::find_by_id(&jobs.0.db, server_id).await else {
                return;
            };
            let check = crate::cloud::refresh_state(&jobs, &row);
            match tokio::time::timeout(PROVIDER_CHECK_TIMEOUT, check).await {
                Ok(Ok(_)) => {}
                Ok(Err(e)) => {
                    tracing::debug!(server_id, error = %e, "Cloud provider status check failed");
                }
                Err(_) => {
                    tracing::debug!(server_id, "Cloud provider status check timed out");
                }
            }
        });
    }

    /// `removeOldBackups($backup)` of `CleanupInstanceStuffsJob::enforceBackupRetention`.
    pub(crate) async fn enforce_backup_retention(&self, backup_id: i64) -> Result<(), String> {
        let db = &self.0.db;
        let Some(backup) = parts::backup_by_id(db, backup_id)
            .await
            .map_err(|e| e.to_string())?
        else {
            return Ok(());
        };
        let Some(database) = self.backup_database(&backup).await else {
            return Ok(());
        };
        let server = self.server_for(&database).await?;
        self.remove_old_backups(&backup, &database, &server).await
    }

    /// The `$storage->testConnection(shouldSave: true)` of `RevalidateUnusableS3StoragesJob`: a storage
    /// that still fails mails the team's admins once.
    pub(crate) async fn store_s3_connection_result(
        &self,
        row: &Record,
        result: Result<(), String>,
    ) -> Result<(), String> {
        let db = &self.0.db;
        let id = row.get("id").and_then(Value::as_i64).unwrap_or(-1);
        let mut email_sent = row.get("unusable_email_sent").and_then(Value::as_bool) == Some(true);
        match &result {
            Ok(()) => email_sent = false,
            Err(reason) if !email_sent => {
                let settings = coolify_db::instance_settings::row(db)
                    .await
                    .map_err(|e| e.to_string())?;
                let on = |k: &str| settings.get(k).and_then(Value::as_bool) == Some(true);
                if on("smtp_enabled") || on("resend_enabled") {
                    let team = row.get("team_id").and_then(Value::as_i64).unwrap_or(-1);
                    let base = notifications::Ctx::load(db).await.base_url;
                    let mail = Mail::S3ConnectionError {
                        name: row
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                        reason: reason.clone(),
                        url: format!(
                            "{base}/storages/{}",
                            row.get("uuid").and_then(Value::as_str).unwrap_or_default()
                        ),
                    };
                    let mut sent = true;
                    for address in queries::team_admin_emails(db, team)
                        .await
                        .map_err(|e| e.to_string())?
                    {
                        if let Err(e) = notifications::send_transactional(
                            db,
                            self.0.encrypter.as_ref(),
                            &address,
                            mail.clone(),
                        )
                        .await
                        {
                            sent = false;
                            tracing::warn!(error = %e, "Failed to send S3 connection error notification");
                            break;
                        }
                    }
                    email_sent = sent;
                }
            }
            Err(_) => {}
        }
        queries::save_s3_connection_result(db, id, result.is_ok(), email_sent, Jobs::now())
            .await
            .map_err(|e| e.to_string())
    }

    /// `RegenerateSslCertJob::handle`.
    pub async fn regenerate_ssl_certificates(
        &self,
        team_id: Option<i64>,
        server_id: Option<i64>,
        force_regeneration: bool,
    ) -> Result<(), String> {
        let db = &self.0.db;
        let rows =
            queries::certificates_to_regenerate(db, server_id, force_regeneration, Jobs::now())
                .await
                .map_err(|e| e.to_string())?;
        let mut regenerated: Vec<SslResource> = Vec::new();
        for certificate in rows {
            if !allowed(&format!("server:{}", certificate.server_id)) {
                continue;
            }
            match self.regenerate_one_certificate(&certificate).await {
                Ok(Some(resource)) => regenerated.push(resource),
                Ok(None) => {}
                Err(e) => tracing::error!(error = %e, "Failed to regenerate SSL certificate"),
            }
        }
        if !regenerated.is_empty()
            && let Some(team_id) = team_id
        {
            notify(
                db,
                self.0.encrypter.as_ref(),
                team_id,
                Notification::SslExpiration {
                    resources: regenerated,
                },
            )
            .await;
        }
        Ok(())
    }

    /// The loop of `handle()`. Only databases hold a certificate on a resource in coolify-rs;
    /// `generate_database_certificate` derives the names from the database, which is what
    /// `common_name` and `subject_alternative_names` hold.
    async fn regenerate_one_certificate(
        &self,
        c: &queries::CertificateToRegenerate,
    ) -> Result<Option<SslResource>, String> {
        let db = &self.0.db;
        let Some(row) = servers::find_by_id(db, c.server_id)
            .await
            .map_err(|e| e.to_string())?
        else {
            return Ok(None);
        };
        let server = ServerInfo::from_row(&row);
        if parts::ca_certificate(db, c.server_id)
            .await
            .map_err(|e| e.to_string())?
            .is_none()
        {
            tracing::error!(
                server_id = c.server_id,
                "No CA certificate found for server_id"
            );
            return Ok(None);
        }
        let ca = self.ensure_ca_certificate(&server).await?;
        let (Some(morph), Some(id)) = (c.resource_type.as_deref(), c.resource_id) else {
            return Ok(None);
        };
        let database = if morph == dbs::SERVICE_DATABASE_MORPH {
            dbs::service_database_by_id(db, id).await
        } else if let Some(kind) = Kind::ALL.into_iter().find(|k| k.morph() == morph) {
            dbs::find_by_id(db, kind, id).await
        } else {
            return Ok(None);
        }
        .map_err(|e| e.to_string())?;
        let Some(database) = database else {
            return Ok(None);
        };
        self.generate_database_certificate(&database, &server, &ca)
            .await?;
        let env = match database.int("environment_id") {
            Some(e) => lookup::environment(db, e).await,
            None => None,
        };
        Ok(Some(SslResource {
            name: database.str("name").unwrap_or_default().to_owned(),
            uuid: database.uuid.clone(),
            project_uuid: env.as_ref().map(|e| e.project_uuid.clone()),
            environment_uuid: env.as_ref().map(|e| e.environment_uuid.clone()),
        }))
    }

    /// `dispatch(new RegenerateSslCertJob(server_id: ..., force_regeneration: true))`.
    pub fn spawn_regenerate_ssl_certificates(&self, server_id: i64) {
        let jobs = self.clone();
        tokio::spawn(async move {
            if let Err(e) = jobs
                .regenerate_ssl_certificates(None, Some(server_id), true)
                .await
            {
                tracing::warn!(server_id, error = %e, "RegenerateSslCertJob failed");
            }
        });
    }

    /// `CleanupHelperContainersJob::handle`.
    pub async fn cleanup_helper_containers(&self, row: &ServerRow) -> Result<(), String> {
        let db = &self.0.db;
        let active = queries::active_deployment_uuids(db, row.id)
            .await
            .map_err(|e| e.to_string())?;
        let server = ServerInfo::from_row(row);
        let target = server.target(self).await?;
        let output = self
            .remote()
            .run_quiet(
                &target,
                &[HELPER_CONTAINERS_COMMAND],
                RunOptions::op("server.helper_containers").timeout(Duration::from_secs(30)),
            )
            .await
            .unwrap_or_default();
        let containers: Vec<Value> = serde_json::from_str(&output).unwrap_or_default();
        for c in containers {
            let name = c.get("Names").and_then(Value::as_str).unwrap_or_default();
            let id = c.get("ID").and_then(Value::as_str).unwrap_or_default();
            if id.is_empty() || active.iter().any(|d| name.contains(d.as_str())) {
                continue;
            }
            tracing::info!(
                container = name,
                id,
                "CleanupHelperContainersJob - Removing orphaned helper container"
            );
            let command = format!("docker container rm -f {id}");
            let _ = self
                .remote()
                .run_quiet(
                    &target,
                    &[command.as_str()],
                    RunOptions::op("server.helper_containers").timeout(Duration::from_secs(30)),
                )
                .await;
        }
        Ok(())
    }
}

/// `extractPullRequestId`.
fn pull_request_id(labels: &str) -> Option<i64> {
    static RE: std::sync::LazyLock<Option<Regex>> =
        std::sync::LazyLock::new(|| Regex::new(r"coolify\.pullRequestId=(\d+)").ok());
    RE.as_ref()?.captures(labels)?.get(1)?.as_str().parse().ok()
}

/// `coolify.applicationId=...` or `coolify.applicationUuid=...` of a `docker ps` label list.
fn label_value<'a>(labels: &'a str, key: &str) -> Option<&'a str> {
    labels.split(',').find_map(|part| {
        let (k, v) = part.split_once('=')?;
        (k == key && !v.is_empty()).then_some(v)
    })
}

/// `resolveContainerApplicationId(collect(), $labels)`.
async fn application_id(db: &coolify_db::Db, labels: &str) -> Option<i64> {
    if let Some(uuid) = label_value(labels, "coolify.applicationUuid") {
        return queries::application_id_with_trashed(db, uuid)
            .await
            .ok()
            .flatten();
    }
    label_value(labels, "coolify.applicationId")?.parse().ok()
}

/// `CleanupOrphanedPreviewContainersJob::handle`.
pub(crate) async fn cleanup_orphaned_preview_containers(jobs: &Jobs) -> Result<(), String> {
    let db = &jobs.0.db;
    for row in servers::all(db).await.map_err(|e| e.to_string())? {
        if !allowed(&format!("server:{}", row.id)) {
            continue;
        }
        let server = ServerInfo::from_row(&row);
        if server.has_placeholder_ip() || !server.is_functional() {
            continue;
        }
        let Ok(target) = server.target(jobs).await else {
            continue;
        };
        let output = jobs
            .remote()
            .run_quiet(
                &target,
                &["docker ps -a --filter 'label=coolify.pullRequestId' --format '{{json .}}'"],
                RunOptions::op("server.orphaned_previews").timeout(Duration::from_secs(60)),
            )
            .await
            .unwrap_or_default();
        for line in output.lines() {
            let Ok(container) = serde_json::from_str::<Value>(line.trim()) else {
                continue;
            };
            let labels = container
                .get("Labels")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let Some(pull_request) = pull_request_id(labels).filter(|id| *id > 0) else {
                continue;
            };
            let Some(application) = application_id(db, labels).await else {
                continue;
            };
            // A soft deleted preview is for `DeleteResourceJob`.
            if queries::preview_exists_with_trashed(db, application, pull_request)
                .await
                .unwrap_or(true)
            {
                continue;
            }
            let name = container
                .get("Names")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if name.is_empty() {
                tracing::warn!(server = %server.uuid, "CleanupOrphanedPreviewContainersJob - Cannot remove container: missing container name");
                continue;
            }
            tracing::info!(container = name, application_id = application, pull_request_id = pull_request, server = %server.uuid, "CleanupOrphanedPreviewContainersJob - Removing orphaned container");
            let command = format!("docker rm -f {}", quote(name));
            let _ = jobs
                .remote()
                .run_quiet(
                    &target,
                    &[command.as_str()],
                    RunOptions::op("server.orphaned_previews").timeout(Duration::from_secs(60)),
                )
                .await;
        }
    }
    Ok(())
}

/// A resource `cleanup:stucked-resources` hands to `DeleteResourceJob` with its default arguments.
#[derive(Debug, Clone, Copy)]
enum Stuck {
    Application(i64),
    Preview(i64),
    Database(Kind, i64),
    Service(i64),
}

async fn delete_stuck(jobs: &Jobs, stuck: Stuck) {
    let db = &jobs.0.db;
    match stuck {
        Stuck::Application(id) => match apps::find_with_trashed(db, id).await {
            Ok(Some(a)) => jobs.spawn_application_delete(a, true, true, true),
            // Without an environment there is no team or server to clean up on.
            Ok(None) => {
                let _ = apps::force_delete(db, id, Jobs::now()).await;
            }
            Err(e) => tracing::warn!(id, error = %e, "cleanup:stucked-resources"),
        },
        Stuck::Preview(id) => jobs.delete_stuck_preview(id).await,
        Stuck::Database(kind, id) => match dbs::find_with_trashed(db, kind, id).await {
            Ok(Some(d)) => jobs.spawn_database_delete(d, true, true, true),
            Ok(None) => {
                let sql = format!("DELETE FROM {} WHERE id = $1", kind.table());
                let _ = queries::delete_by_id(db, &sql, id).await;
            }
            Err(e) => tracing::warn!(id, error = %e, "cleanup:stucked-resources"),
        },
        Stuck::Service(id) => match svc::find_trashed(db, id).await {
            Ok(Some(s)) => jobs.spawn_service_delete(s, true, true, true),
            Ok(None) => {
                let _ = svc::force_delete(db, id).await;
            }
            Err(e) => tracing::warn!(id, error = %e, "cleanup:stucked-resources"),
        },
    }
}

impl Jobs {
    /// `DeleteResourceJob::dispatch($applicationPreview)`.
    async fn delete_stuck_preview(&self, id: i64) {
        let db = &self.0.db;
        let row: Option<(i64, i64)> = sqlx::query_as(
            "SELECT application_id, pull_request_id FROM application_previews WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(db.pool())
        .await
        .ok()
        .flatten();
        let Some((application_id, pull_request_id)) = row else {
            return;
        };
        match apps::find_by_id(db, application_id).await {
            Ok(Some(a)) => {
                self.cleanup_preview_deployment(&a, pull_request_id, id)
                    .await;
            }
            // The application is gone: nothing can be reached for this preview.
            _ => {
                let _ =
                    queries::delete_by_id(db, "DELETE FROM application_previews WHERE id = $1", id)
                        .await;
            }
        }
    }
}

/// Runs one query of the command; a failure is logged and the next one goes on, as its `try`s do.
async fn stuck_ids(jobs: &Jobs, sql: &str) -> Vec<i64> {
    match queries::ids(&jobs.0.db, sql).await {
        Ok(ids) => ids,
        Err(e) => {
            tracing::warn!(error = %e, "cleanup:stucked-resources");
            Vec::new()
        }
    }
}

/// The server chain of `destination.server`, as a condition on the alias `d`.
const WITHOUT_SERVER: &str = "NOT EXISTS (SELECT 1 FROM environments e WHERE e.id = d.environment_id) \
    OR NOT ( \
      (d.destination_type = 'App\\Models\\StandaloneDocker' AND EXISTS (SELECT 1 FROM standalone_dockers x JOIN servers s ON s.id = x.server_id WHERE x.id = d.destination_id)) \
      OR (d.destination_type = 'App\\Models\\SwarmDocker' AND EXISTS (SELECT 1 FROM swarm_dockers x JOIN servers s ON s.id = x.server_id WHERE x.id = d.destination_id)))";

/// `CleanupStuckedResources::cleanup_stucked_resources`. Nothing here runs for a key filter that does
/// not name it, because the whole job is `kernel:cleanup:stucked-resources`.
pub(crate) async fn cleanup_stucked_resources(jobs: &Jobs) {
    let db = &jobs.0.db;
    // Teams without members and servers go with what they own (`deleteOwnedRecords`).
    for id in stuck_ids(
        jobs,
        "SELECT t.id FROM teams t WHERE t.id <> 0 \
         AND NOT EXISTS (SELECT 1 FROM team_user u WHERE u.team_id = t.id) \
         AND NOT EXISTS (SELECT 1 FROM servers s WHERE s.team_id = t.id) ORDER BY t.id",
    )
    .await
    {
        if let Err(e) = queries::delete_team_and_owned_records(db, id).await {
            tracing::warn!(team_id = id, error = %e, "cleanup:stucked-resources");
        }
    }
    // Functional servers: `CleanupHelperContainersJob::dispatch($server)`.
    if let Ok(rows) = servers::all(db).await {
        for row in rows {
            if ServerInfo::from_row(&row).is_functional() {
                let jobs = jobs.clone();
                tokio::spawn(async move {
                    if let Err(e) = jobs.cleanup_helper_containers(&row).await {
                        tracing::warn!(error = %e, "CleanupHelperContainersJob failed");
                    }
                });
            }
        }
    }
    for id in stuck_ids(
        jobs,
        "SELECT id FROM servers WHERE deleted_at IS NOT NULL ORDER BY id",
    )
    .await
    {
        let _ = queries::delete_by_id(db, "DELETE FROM servers WHERE id = $1", id).await;
    }
    // Queue rows of an application that is gone (`Application::find` skips trashed ones).
    for id in stuck_ids(
        jobs,
        "SELECT q.id FROM application_deployment_queues q WHERE NOT EXISTS ( \
           SELECT 1 FROM applications a WHERE a.id::text = q.application_id AND a.deleted_at IS NULL) ORDER BY q.id",
    )
    .await
    {
        let _ = queries::delete_by_id(db, "DELETE FROM application_deployment_queues WHERE id = $1", id).await;
    }
    for id in stuck_ids(
        jobs,
        "SELECT id FROM applications WHERE deleted_at IS NOT NULL ORDER BY id",
    )
    .await
    {
        delete_stuck(jobs, Stuck::Application(id)).await;
    }
    for id in stuck_ids(
        jobs,
        "SELECT p.id FROM application_previews p WHERE p.deleted_at IS NOT NULL \
         OR NOT EXISTS (SELECT 1 FROM applications a WHERE a.id = p.application_id AND a.deleted_at IS NULL) ORDER BY p.id",
    )
    .await
    {
        delete_stuck(jobs, Stuck::Preview(id)).await;
    }
    for kind in Kind::ALL {
        let sql = format!(
            "SELECT id FROM {} WHERE deleted_at IS NOT NULL ORDER BY id",
            kind.table()
        );
        for id in stuck_ids(jobs, &sql).await {
            delete_stuck(jobs, Stuck::Database(kind, id)).await;
        }
    }
    for id in stuck_ids(
        jobs,
        "SELECT id FROM services WHERE deleted_at IS NOT NULL ORDER BY id",
    )
    .await
    {
        delete_stuck(jobs, Stuck::Service(id)).await;
    }
    for (select, delete) in [
        (
            "SELECT id FROM service_applications WHERE deleted_at IS NOT NULL ORDER BY id",
            "DELETE FROM service_applications WHERE id = $1",
        ),
        (
            "SELECT id FROM service_databases WHERE deleted_at IS NOT NULL ORDER BY id",
            "DELETE FROM service_databases WHERE id = $1",
        ),
        (
            "SELECT t.id FROM scheduled_tasks t WHERE (t.application_id IS NULL AND t.service_id IS NULL) \
             OR (t.application_id IS NOT NULL AND NOT EXISTS (SELECT 1 FROM applications a WHERE a.id = t.application_id AND a.deleted_at IS NULL)) \
             OR (t.service_id IS NOT NULL AND NOT EXISTS (SELECT 1 FROM services s WHERE s.id = t.service_id AND s.deleted_at IS NULL)) ORDER BY t.id",
            "DELETE FROM scheduled_tasks WHERE id = $1",
        ),
        (
            "SELECT x.id FROM service_applications x WHERE NOT EXISTS (SELECT 1 FROM services s WHERE s.id = x.service_id AND s.deleted_at IS NULL) ORDER BY x.id",
            "DELETE FROM service_applications WHERE id = $1",
        ),
        (
            "SELECT x.id FROM service_databases x WHERE NOT EXISTS (SELECT 1 FROM services s WHERE s.id = x.service_id AND s.deleted_at IS NULL) ORDER BY x.id",
            "DELETE FROM service_databases WHERE id = $1",
        ),
    ] {
        for id in stuck_ids(jobs, select).await {
            let _ = queries::delete_by_id(db, delete, id).await;
        }
    }
    // `$scheduled_backup->server()` is null.
    if let Ok(rows) =
        sqlx::query_scalar::<_, i64>("SELECT id FROM scheduled_database_backups ORDER BY id")
            .fetch_all(db.pool())
            .await
    {
        for id in rows {
            let Ok(Some(backup)) = parts::backup_by_id(db, id).await else {
                continue;
            };
            let server = match jobs.backup_database(&backup).await {
                Some(database) => jobs.server_for(&database).await.ok(),
                None => None,
            };
            if server.is_none() {
                let _ = parts::delete_backup(db, id).await;
            }
        }
    }
    // Resources that are not attached to an environment, a destination or a server.
    let unattached = |table: &str| {
        format!(
            "SELECT d.id FROM {table} d WHERE d.deleted_at IS NULL AND d.id <> 0 AND ({WITHOUT_SERVER}) ORDER BY d.id"
        )
    };
    for id in stuck_ids(jobs, &unattached("applications")).await {
        delete_stuck(jobs, Stuck::Application(id)).await;
    }
    for kind in [
        Kind::Postgresql,
        Kind::Redis,
        Kind::Mongodb,
        Kind::Mysql,
        Kind::Mariadb,
    ] {
        for id in stuck_ids(jobs, &unattached(kind.table())).await {
            delete_stuck(jobs, Stuck::Database(kind, id)).await;
        }
    }
    for id in stuck_ids(jobs, &unattached("services")).await {
        delete_stuck(jobs, Stuck::Service(id)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_of_docker_ps() {
        let labels = "coolify.applicationId=12,coolify.pullRequestId=34,other=x";
        assert_eq!(pull_request_id(labels), Some(34));
        assert_eq!(label_value(labels, "coolify.applicationId"), Some("12"));
        assert_eq!(label_value(labels, "coolify.applicationUuid"), None);
        assert_eq!(pull_request_id("a=b"), None);
    }
}
