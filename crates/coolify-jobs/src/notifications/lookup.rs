//! The plain data of the notifications, read from the rows the jobs hold ids of.

use coolify_compat::Encrypter;
use coolify_db::Db;

use super::messages::{
    CommitDetails, DatabaseBackup, Deployment, Notification, RestartKind, RestartLimit, ServerRef,
    Task, TaskResource, blocked_titles, git_commit_link,
};

/// `environment.project.*`.
#[derive(Debug, Clone, Default)]
pub struct EnvironmentInfo {
    pub environment_name: String,
    pub environment_uuid: String,
    pub project_name: String,
    pub project_uuid: String,
    pub team_id: Option<i64>,
}

/// The environment of a resource.
pub async fn environment(db: &Db, environment_id: i64) -> Option<EnvironmentInfo> {
    let row: Option<(String, String, String, String, Option<i64>)> = sqlx::query_as(
        "SELECT e.name::text, e.uuid::text, p.name::text, p.uuid::text, p.team_id \
         FROM environments e JOIN projects p ON p.id = e.project_id WHERE e.id = $1",
    )
    .bind(environment_id)
    .fetch_optional(db.pool())
    .await
    .ok()
    .flatten();
    row.map(|r| EnvironmentInfo {
        environment_name: r.0,
        environment_uuid: r.1,
        project_name: r.2,
        project_uuid: r.3,
        team_id: r.4,
    })
}

/// `$server->team` and the plain server data.
pub async fn server(db: &Db, server_id: i64) -> Option<(i64, ServerRef)> {
    let row: Option<(Option<i64>, String, String)> =
        sqlx::query_as("SELECT team_id, name::text, uuid::text FROM servers WHERE id = $1")
            .bind(server_id)
            .fetch_optional(db.pool())
            .await
            .ok()
            .flatten();
    let (team, name, uuid) = row?;
    Some((
        team?,
        ServerRef {
            id: server_id,
            name,
            uuid,
        },
    ))
}

/// `$application->environment->project->team?->notify(new DeploymentSuccess|DeploymentFailed(...))`
/// (`sendDeploymentNotification`). Previews and additional servers are not deployed by coolify-rs.
pub async fn notify_deployment(
    db: &Db,
    encrypter: Option<&Encrypter>,
    application_id: i64,
    queue_id: i64,
    commit: &str,
    success: bool,
) {
    type ApplicationColumns = (
        String,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<i64>,
        i64,
    );
    let app: Option<ApplicationColumns> = sqlx::query_as(
        "SELECT name::text, uuid::text, fqdn::text, git_repository::text, git_branch::text, \
         source_type::text, source_id, environment_id FROM applications WHERE id = $1",
    )
    .bind(application_id)
    .fetch_optional(db.pool())
    .await
    .ok()
    .flatten();
    let Some((name, uuid, fqdn, repo, branch, source_type, source_id, environment_id)) = app else {
        return;
    };
    let Some(env) = environment(db, environment_id).await else {
        return;
    };
    let Some(team_id) = env.team_id else {
        return;
    };
    #[allow(clippy::type_complexity)]
    let queue: Option<(String, Option<String>, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT deployment_uuid::text, commit_message::text, parent_deployment_uuid::text, \
         scan_gate_result::text FROM application_deployment_queues WHERE id = $1",
    )
    .bind(queue_id)
    .fetch_optional(db.pool())
    .await
    .ok()
    .flatten();
    let Some((deployment_uuid, commit_message, parent, scan_gate_result)) = queue else {
        return;
    };
    // `DeploymentFailed`: the headline of the image scan gate when it stopped the deployment.
    // The scan engine writes `scan_gate_result`.
    let gate: Option<serde_json::Value> = scan_gate_result
        .as_deref()
        .and_then(|text| serde_json::from_str(text).ok());
    let (blocked_title, blocked_chat_title) = if success {
        (None, None)
    } else {
        blocked_titles(gate.as_ref())
    };
    // `sendMultiServerDeploymentNotification`: the main deployment reports for all servers.
    if parent.is_some_and(|p| !p.trim().is_empty()) {
        return;
    }
    let html_url = match (source_type.as_deref(), source_id) {
        (Some("App\\Models\\GithubApp"), Some(id)) => source_html_url(db, "github_apps", id).await,
        (Some("App\\Models\\GitlabApp"), Some(id)) => source_html_url(db, "gitlab_apps", id).await,
        _ => None,
    };
    let details = CommitDetails::resolve(
        Some(commit),
        commit_message.as_deref(),
        branch.as_deref(),
        false,
        |c| git_commit_link(html_url.as_deref(), repo.as_deref(), branch.as_deref(), c),
    );
    let d = Deployment {
        application_name: name,
        application_uuid: uuid,
        deployment_uuid,
        project_name: env.project_name,
        project_uuid: env.project_uuid,
        environment_name: env.environment_name,
        environment_uuid: env.environment_uuid,
        fqdn,
        preview: None,
        commit: Some(details),
        blocked_title,
        blocked_chat_title,
    };
    let n = if success {
        Notification::DeploymentSuccess(d)
    } else {
        Notification::DeploymentFailed(d)
    };
    super::notify(db, encrypter, team_id, n).await;
}

async fn source_html_url(db: &Db, table: &str, id: i64) -> Option<String> {
    let sql = format!("SELECT html_url::text FROM {table} WHERE id = $1");
    sqlx::query_scalar(&sql)
        .bind(id)
        .fetch_optional(db.pool())
        .await
        .ok()
        .flatten()
}

/// The database part of the backup notifications.
pub async fn database_backup(
    db: &Db,
    name: &str,
    uuid: &str,
    environment_id: Option<i64>,
    database_name: Option<&str>,
    frequency: &str,
) -> DatabaseBackup {
    let env = match environment_id {
        Some(id) => environment(db, id).await,
        None => None,
    };
    DatabaseBackup {
        name: name.to_owned(),
        uuid: uuid.to_owned(),
        project_uuid: env.as_ref().map(|e| e.project_uuid.clone()),
        environment_uuid: env.as_ref().map(|e| e.environment_uuid.clone()),
        database_name: database_name.map(str::to_owned),
        frequency: frequency.to_owned(),
        backup_id: 0,
        service_uuid: None,
        is_instance_database: false,
    }
}

/// [`database_backup`] for the database of the backup schedule `backup_id`: with the schedule
/// (the throttle subject of its failures) and what `link()` reads (the service of a service
/// database, `isInstanceDatabase()` of the instance database).
pub async fn scheduled_database_backup(
    db: &Db,
    backup_id: i64,
    database: &coolify_db::databases::DatabaseRow,
    database_name: Option<&str>,
    frequency: &str,
) -> DatabaseBackup {
    let mut backup = database_backup(
        db,
        database.str("name").unwrap_or_default(),
        &database.uuid,
        database.int("environment_id"),
        database_name,
        frequency,
    )
    .await;
    backup.backup_id = backup_id;
    if database.is_service() {
        backup.service_uuid = database.str("__service_uuid").map(str::to_owned);
    } else {
        // `StandalonePostgresql::isInstanceDatabase()`: `$this->id === 0`.
        backup.is_instance_database =
            database.kind == coolify_db::databases::Kind::Postgresql && database.id == 0;
    }
    backup
}

/// `$backup->s3->uuid`.
pub async fn s3_uuid(db: &Db, s3_storage_id: Option<i64>) -> Option<String> {
    let id = s3_storage_id?;
    sqlx::query_scalar("SELECT uuid::text FROM s3_storages WHERE id = $1")
        .bind(id)
        .fetch_optional(db.pool())
        .await
        .ok()
        .flatten()
}

/// `TaskSuccess` / `TaskFailed` of a task of an application or a service.
pub async fn scheduled_task(
    db: &Db,
    name: &str,
    uuid: &str,
    output: &str,
    resource_uuid: Option<&str>,
    environment_id: Option<i64>,
    is_application: bool,
) -> Task {
    let env = match environment_id {
        Some(id) => environment(db, id).await,
        None => None,
    };
    Task {
        name: name.to_owned(),
        uuid: uuid.to_owned(),
        output: output.to_owned(),
        resource: resource_uuid.map(|resource_uuid| TaskResource {
            is_application,
            uuid: resource_uuid.to_owned(),
            project_uuid: env.as_ref().map(|e| e.project_uuid.clone()),
            environment_uuid: env.as_ref().map(|e| e.environment_uuid.clone()),
        }),
        // The failure streak belongs to `TaskFailed`; the job fills it in.
        id: 0,
        disabled: false,
        disabled_reason: None,
        consecutive_failures: 0,
        failing_since: None,
    }
}

/// `new RestartLimitReached($application)` and `new RestartLimitReached($preview)`.
#[allow(clippy::too_many_arguments)]
pub async fn application_restart_limit(
    db: &Db,
    environment_id: i64,
    kind: RestartKind,
    resource_name: String,
    resource_uuid: &str,
    application_uuid: &str,
    fqdn: Option<&str>,
    restart_count: i64,
    max_restart_count: i64,
) -> Option<(i64, Notification)> {
    let env = environment(db, environment_id).await?;
    Some((
        env.team_id?,
        Notification::RestartLimitReached(RestartLimit {
            kind,
            resource_name,
            resource_uuid: resource_uuid.to_owned(),
            url_uuid: application_uuid.to_owned(),
            project_name: env.project_name,
            project_uuid: env.project_uuid,
            environment_name: env.environment_name,
            environment_uuid: env.environment_uuid,
            fqdn: fqdn.map(str::to_owned),
            restart_count,
            max_restart_count,
        }),
    ))
}

/// `new RestartLimitReached($subResource)` for a part of a service.
#[allow(clippy::too_many_arguments)]
pub async fn service_part_restart_limit(
    db: &Db,
    environment_id: i64,
    resource_name: &str,
    resource_uuid: &str,
    service_uuid: &str,
    fqdn: Option<&str>,
    restart_count: i64,
    max_restart_count: i64,
) -> Option<(i64, Notification)> {
    let env = environment(db, environment_id).await?;
    Some((
        env.team_id?,
        Notification::RestartLimitReached(RestartLimit {
            kind: RestartKind::Service,
            resource_name: resource_name.to_owned(),
            resource_uuid: resource_uuid.to_owned(),
            url_uuid: service_uuid.to_owned(),
            project_name: env.project_name,
            project_uuid: env.project_uuid,
            environment_name: env.environment_name,
            environment_uuid: env.environment_uuid,
            fqdn: fqdn.map(str::to_owned),
            restart_count,
            max_restart_count,
        }),
    ))
}

/// `$server->unreachable_notification_sent`.
pub async fn unreachable_notification_sent(db: &Db, server_id: i64) -> bool {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM notification_throttles WHERE notifiable_type = 'App\\Models\\Server' \
         AND notifiable_id = $1 AND notification = 'App\\Notifications\\Server\\Unreachable')",
    )
    .bind(server_id)
    .fetch_one(db.pool())
    .await
    .unwrap_or(false)
}

/// `ServerReachabilityChanged::dispatch($server)`: `Server::isReachableChanged`.
pub async fn server_reachability_changed(
    db: &Db,
    encrypter: Option<&Encrypter>,
    server_id: i64,
    unreachable_threshold: i64,
) {
    let Some((team_id, server)) = server(db, server_id).await else {
        return;
    };
    let row: Option<(Option<bool>, Option<i32>)> = sqlx::query_as(
        "SELECT st.is_reachable, s.unreachable_count FROM servers s \
         JOIN server_settings st ON st.server_id = s.id WHERE s.id = $1",
    )
    .bind(server_id)
    .fetch_optional(db.pool())
    .await
    .ok()
    .flatten();
    let Some((reachable, count)) = row else {
        return;
    };
    let sent = unreachable_notification_sent(db, server_id).await;
    if reachable == Some(true) {
        if sent {
            super::send_reachable_notification(db, encrypter, team_id, server).await;
        }
        return;
    }
    if i64::from(count.unwrap_or(0)) >= unreachable_threshold && !sent {
        super::send_unreachable_notification(db, encrypter, team_id, server).await;
    }
}
