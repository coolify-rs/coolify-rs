//! `App\Jobs\NotifyResourceStoppedJob`: sends the "resource stopped" notification
//! (`Application\StatusChanged`) when a status check finds that a resource stopped without a
//! stop from Coolify (for example a crash or `docker kill`).
//!
//! The job runs after a delay and checks the resource again. A resource that is up again by then
//! (Docker restarted it, or a deployment replaced its container) sends no notification.
//!
//! # Interface
//!
//! ```ignore
//! // Before Coolify stops or recreates the containers of a resource (`StopApplication`,
//! // `StopDatabase`, `StopService`, ...), so that the stop sends no notification:
//! resource_stopped::expect_stop(resource_stopped::APPLICATION, application_id, stop_grace_seconds);
//!
//! // Around a status check (`GetContainersStatus`, `PushServerUpdateJob`):
//! let up = resource_stopped::up_resources(rows.iter().map(|r| (morph, r.id, r.status.as_deref())));
//! // ... the check writes the new statuses ...
//! resource_stopped::dispatch_for_stopped(&jobs, &up).await;
//! ```

use std::time::Duration;

use coolify_db::Db;

use super::lookup;
use super::messages::{Notification, RestartKind};
use super::php::php_trim;
use crate::Jobs;
use crate::server_ops::MARKS;

/// `NotifyResourceStoppedJob::CONFIRMATION_DELAY_SECONDS`.
pub const CONFIRMATION_DELAY_SECONDS: u64 = 120;

/// `EXPECTED_STOP_SECONDS`: how long a stop that Coolify starts sends no notification, in
/// addition to the stop timeout.
const EXPECTED_STOP_SECONDS: u64 = 300;

/// The default `$stopTimeoutSeconds` of `expectStop()`.
pub const DEFAULT_STOP_TIMEOUT_SECONDS: u64 = 30;

pub const APPLICATION: &str = "App\\Models\\Application";
pub const APPLICATION_PREVIEW: &str = "App\\Models\\ApplicationPreview";
pub const SERVICE_APPLICATION: &str = "App\\Models\\ServiceApplication";
pub const SERVICE_DATABASE: &str = "App\\Models\\ServiceDatabase";

/// A resource a status check looks at: its morph class and id.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Resource {
    pub morph: String,
    pub id: i64,
}

/// Where a resource lives and how `StatusChanged` links to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Shape {
    table: &'static str,
    kind: RestartKind,
}

/// The table and link type of a morph class; `None` for a class that has no status.
fn shape(morph: &str) -> Option<Shape> {
    let (table, kind) = match morph {
        APPLICATION => ("applications", RestartKind::Application),
        APPLICATION_PREVIEW => ("application_previews", RestartKind::Preview),
        SERVICE_APPLICATION => ("service_applications", RestartKind::Service),
        SERVICE_DATABASE => ("service_databases", RestartKind::Service),
        "App\\Models\\StandalonePostgresql" => ("standalone_postgresqls", RestartKind::Database),
        "App\\Models\\StandaloneRedis" => ("standalone_redis", RestartKind::Database),
        "App\\Models\\StandaloneMongodb" => ("standalone_mongodbs", RestartKind::Database),
        "App\\Models\\StandaloneMysql" => ("standalone_mysqls", RestartKind::Database),
        "App\\Models\\StandaloneMariadb" => ("standalone_mariadbs", RestartKind::Database),
        "App\\Models\\StandaloneKeydb" => ("standalone_keydbs", RestartKind::Database),
        "App\\Models\\StandaloneDragonfly" => ("standalone_dragonflies", RestartKind::Database),
        "App\\Models\\StandaloneClickhouse" => ("standalone_clickhouses", RestartKind::Database),
        "App\\Models\\StandaloneSqlite" => ("standalone_sqlites", RestartKind::Database),
        _ => return None,
    };
    Some(Shape { table, kind })
}

/// `isExited($status)`: `str($status)->startsWith('exited')`.
pub fn is_exited(status: Option<&str>) -> bool {
    status.is_some_and(|s| s.starts_with("exited"))
}

/// `expectedStopKey($resource)`.
fn expected_stop_key(morph: &str, id: i64) -> String {
    format!("resource-stop-expected:{morph}:{id}")
}

/// `expectStop($resource, $stopTimeoutSeconds)`: call before Coolify stops or recreates the
/// containers of a resource, so that the stop sends no notification.
pub fn expect_stop(morph: &str, id: i64, stop_timeout_seconds: u64) {
    MARKS.put(
        expected_stop_key(morph, id),
        "1".to_owned(),
        Duration::from_secs(stop_timeout_seconds + EXPECTED_STOP_SECONDS),
    );
}

/// `stopIsExpected($resource)`.
pub fn stop_is_expected(morph: &str, id: i64) -> bool {
    MARKS.has(&expected_stop_key(morph, id))
}

/// `upResourceIds($resources)`: which resources are up, read before a status check changes
/// their status. Each item is the morph class, the id and the status of a resource.
pub fn up_resources<'a>(
    resources: impl IntoIterator<Item = (&'a str, i64, Option<&'a str>)>,
) -> Vec<Resource> {
    resources
        .into_iter()
        .filter(|(_, _, status)| {
            status.is_some_and(|s| !php_trim(s).is_empty()) && !is_exited(*status)
        })
        .map(|(morph, id, _)| Resource {
            morph: morph.to_owned(),
            id,
        })
        .collect()
}

/// `dispatchForStopped($upResourceIds)`: after a status check, queues a notification for each
/// resource that was up before the check and is exited now.
pub async fn dispatch_for_stopped(jobs: &Jobs, up: &[Resource]) {
    for resource in up {
        let Some(shape) = shape(&resource.morph) else {
            continue;
        };
        let exited = match current_status(&jobs.0.db, shape.table, resource.id).await {
            Ok(status) => is_exited(status.as_deref()),
            Err(e) => {
                tracing::warn!(error = %e, resource = %resource.morph, "could not read the status of a resource");
                false
            }
        };
        if !exited || stop_is_expected(&resource.morph, resource.id) {
            continue;
        }
        dispatch(jobs, resource.clone());
    }
}

/// `self::dispatch($resource)`: the job runs after `CONFIRMATION_DELAY_SECONDS`, once.
pub fn dispatch(jobs: &Jobs, resource: Resource) {
    let jobs = jobs.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(CONFIRMATION_DELAY_SECONDS)).await;
        handle(&jobs, &resource).await;
    });
}

/// `$resource::query()->whereKey($id)->value('status')`; soft deleted rows are gone.
async fn current_status(db: &Db, table: &str, id: i64) -> Result<Option<String>, sqlx::Error> {
    let sql = format!(
        "SELECT x.status::text FROM {table} x WHERE x.id = $1 AND (to_jsonb(x)->>'deleted_at') IS NULL"
    );
    let row: Option<Option<String>> = sqlx::query_scalar(&sql)
        .bind(id)
        .fetch_optional(db.pool())
        .await?;
    Ok(row.flatten())
}

/// The attributes of a resource that `handle()` and `StatusChanged` read.
#[derive(Debug, Clone, Default, PartialEq, sqlx::FromRow)]
struct StoppedRow {
    uuid: Option<String>,
    status: Option<String>,
    restart_limit_reached: bool,
    fqdn: Option<String>,
    name: Option<String>,
    environment_id: Option<i64>,
    application_id: Option<i64>,
    service_id: Option<i64>,
    pull_request_id: Option<i64>,
}

async fn load(db: &Db, table: &str, id: i64) -> Result<Option<StoppedRow>, sqlx::Error> {
    // Not every table has every column (`restart_limit_reached`, `fqdn`, the parent ids).
    let sql = format!(
        "SELECT x.uuid::text AS uuid, x.status::text AS status, \
         COALESCE((to_jsonb(x)->>'restart_limit_reached')::boolean, false) AS restart_limit_reached, \
         to_jsonb(x)->>'fqdn' AS fqdn, to_jsonb(x)->>'name' AS name, \
         (to_jsonb(x)->>'environment_id')::bigint AS environment_id, \
         (to_jsonb(x)->>'application_id')::bigint AS application_id, \
         (to_jsonb(x)->>'service_id')::bigint AS service_id, \
         (to_jsonb(x)->>'pull_request_id')::bigint AS pull_request_id \
         FROM {table} x WHERE x.id = $1 AND (to_jsonb(x)->>'deleted_at') IS NULL"
    );
    sqlx::query_as(&sql)
        .bind(id)
        .fetch_optional(db.pool())
        .await
}

/// `isDeploying($resource)`: a queued or running deployment of the application or preview.
async fn is_deploying(
    db: &Db,
    application_id: i64,
    pull_request_id: i64,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM application_deployment_queues WHERE application_id = $1 \
         AND pull_request_id = $2 AND status IN ('queued', 'in_progress'))",
    )
    .bind(application_id.to_string())
    .bind(pull_request_id)
    .fetch_one(db.pool())
    .await
}

/// The parent of a preview (its application) or of a service part (its service): name, uuid
/// and environment.
async fn parent(
    db: &Db,
    table: &str,
    id: i64,
) -> Result<Option<(Option<String>, String, Option<i64>)>, sqlx::Error> {
    let sql = format!(
        "SELECT name::text, uuid::text, environment_id FROM {table} WHERE id = $1 AND deleted_at IS NULL"
    );
    sqlx::query_as(&sql)
        .bind(id)
        .fetch_optional(db.pool())
        .await
}

/// What `StatusChanged` needs besides the row: the name, the uuid of the link, the environment.
#[derive(Debug, Clone, PartialEq)]
struct Target {
    name: String,
    url_uuid: Option<String>,
    environment_id: Option<i64>,
}

/// `new StatusChanged($resource)`: the name and link of each resource type. `parent` is the
/// application of a preview or the service of a part: `(name, uuid, environment_id)`.
fn target(
    kind: RestartKind,
    row: &StoppedRow,
    parent: Option<&(Option<String>, String, Option<i64>)>,
) -> Target {
    match kind {
        RestartKind::Preview => Target {
            // `data_get($resource, 'application.name').' PR #'.$resource->pull_request_id`.
            name: format!(
                "{} PR #{}",
                parent.and_then(|p| p.0.clone()).unwrap_or_default(),
                row.pull_request_id.unwrap_or_default()
            ),
            url_uuid: Some(parent.map(|p| p.1.clone()).unwrap_or_default()),
            environment_id: parent.and_then(|p| p.2),
        },
        RestartKind::Service => Target {
            name: row.name.clone().unwrap_or_default(),
            url_uuid: Some(parent.map(|p| p.1.clone()).unwrap_or_default()),
            environment_id: parent.and_then(|p| p.2),
        },
        RestartKind::Application | RestartKind::Database => Target {
            name: row.name.clone().unwrap_or_default(),
            url_uuid: None,
            environment_id: row.environment_id,
        },
    }
}

/// The first checks of `handle()` on the reloaded row: the resource is still exited, did not
/// reach its restart limit (that has its own notification) and its stop is not expected.
fn still_stopped(row: &StoppedRow, stop_is_expected: bool) -> bool {
    is_exited(row.status.as_deref()) && !row.restart_limit_reached && !stop_is_expected
}

/// `handle()`: checks the resource again and notifies its team.
pub async fn handle(jobs: &Jobs, resource: &Resource) {
    if let Err(e) = handle_inner(jobs, resource).await {
        tracing::warn!(error = %e, resource = %resource.morph, id = resource.id, "NotifyResourceStoppedJob failed");
    }
}

async fn handle_inner(jobs: &Jobs, resource: &Resource) -> Result<(), sqlx::Error> {
    let db = &jobs.0.db;
    let Some(shape) = shape(&resource.morph) else {
        return Ok(());
    };
    let Some(row) = load(db, shape.table, resource.id).await? else {
        return Ok(());
    };
    if !still_stopped(&row, stop_is_expected(&resource.morph, resource.id)) {
        return Ok(());
    }
    let deploying = match shape.kind {
        RestartKind::Application => is_deploying(db, resource.id, 0).await?,
        RestartKind::Preview => match row.application_id {
            Some(application_id) => {
                is_deploying(db, application_id, row.pull_request_id.unwrap_or_default()).await?
            }
            None => false,
        },
        _ => false,
    };
    if deploying {
        return Ok(());
    }
    let parent = match shape.kind {
        RestartKind::Preview => match row.application_id {
            Some(id) => parent(db, "applications", id).await?,
            None => None,
        },
        RestartKind::Service => match row.service_id {
            Some(id) => parent(db, "services", id).await?,
            None => None,
        },
        _ => None,
    };
    let target = target(shape.kind, &row, parent.as_ref());
    let environment = match target.environment_id {
        Some(id) => lookup::environment(db, id).await,
        None => None,
    };
    // `$resource->team()`: the team of the project. Without one there is nobody to notify.
    let Some(team_id) = environment.as_ref().and_then(|e| e.team_id) else {
        return Ok(());
    };
    let notification = Notification::StatusChanged {
        name: target.name,
        uuid: row.uuid.clone().unwrap_or_default(),
        project_name: environment.as_ref().map(|e| e.project_name.clone()),
        project_uuid: environment.as_ref().map(|e| e.project_uuid.clone()),
        environment_name: environment.as_ref().map(|e| e.environment_name.clone()),
        environment_uuid: environment.as_ref().map(|e| e.environment_uuid.clone()),
        fqdn: row.fqdn.clone(),
        kind: shape.kind,
        url_uuid: target.url_uuid,
        throttle: Some((resource.morph.clone(), resource.id)),
    };
    super::notify(db, jobs.0.encrypter.as_ref(), team_id, notification).await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exited_statuses() {
        assert!(is_exited(Some("exited")));
        assert!(is_exited(Some("exited:unhealthy")));
        assert!(!is_exited(Some("running:healthy")));
        assert!(!is_exited(Some("restarting")));
        assert!(!is_exited(Some("")));
        assert!(!is_exited(None));
    }

    #[test]
    fn only_resources_that_are_up_are_remembered() {
        let up = up_resources([
            (APPLICATION, 1, Some("running:healthy")),
            (APPLICATION, 2, Some("exited:unhealthy")),
            (APPLICATION, 3, None),
            (APPLICATION, 4, Some("")),
            (SERVICE_DATABASE, 5, Some("restarting:unknown")),
            (
                "App\\Models\\StandalonePostgresql",
                6,
                Some("degraded:unhealthy"),
            ),
        ]);
        let ids: Vec<(&str, i64)> = up.iter().map(|r| (r.morph.as_str(), r.id)).collect();
        assert_eq!(
            ids,
            [
                (APPLICATION, 1),
                (SERVICE_DATABASE, 5),
                ("App\\Models\\StandalonePostgresql", 6),
            ]
        );
    }

    #[test]
    fn a_stop_from_coolify_is_expected_for_that_resource_only() {
        assert!(!stop_is_expected(APPLICATION, 987_001));
        expect_stop(APPLICATION, 987_001, DEFAULT_STOP_TIMEOUT_SECONDS);
        assert!(stop_is_expected(APPLICATION, 987_001));
        assert!(!stop_is_expected(APPLICATION, 987_002));
        assert!(!stop_is_expected(APPLICATION_PREVIEW, 987_001));
        assert_eq!(
            expected_stop_key(SERVICE_APPLICATION, 7),
            "resource-stop-expected:App\\Models\\ServiceApplication:7"
        );
    }

    #[test]
    fn every_resource_type_has_a_table_and_a_link() {
        assert_eq!(
            shape(APPLICATION),
            Some(Shape {
                table: "applications",
                kind: RestartKind::Application
            })
        );
        assert_eq!(
            shape(APPLICATION_PREVIEW).map(|s| s.kind),
            Some(RestartKind::Preview)
        );
        assert_eq!(
            shape(SERVICE_APPLICATION).map(|s| s.table),
            Some("service_applications")
        );
        assert_eq!(
            shape(SERVICE_DATABASE).map(|s| s.kind),
            Some(RestartKind::Service)
        );
        assert_eq!(
            shape("App\\Models\\StandaloneRedis"),
            Some(Shape {
                table: "standalone_redis",
                kind: RestartKind::Database
            })
        );
        assert_eq!(shape("App\\Models\\Server"), None);
    }

    fn row(status: &str) -> StoppedRow {
        StoppedRow {
            uuid: Some("res-uuid".into()),
            status: Some(status.into()),
            name: Some("web".into()),
            environment_id: Some(4),
            ..StoppedRow::default()
        }
    }

    #[test]
    fn a_resource_that_is_up_again_sends_nothing() {
        assert!(still_stopped(&row("exited:unhealthy"), false));
        // Docker restarted it, or a deployment replaced its container.
        assert!(!still_stopped(&row("running:healthy"), false));
        // Coolify stopped it.
        assert!(!still_stopped(&row("exited"), true));
        // The restart limit has its own notification.
        let mut limited = row("exited");
        limited.restart_limit_reached = true;
        assert!(!still_stopped(&limited, false));
    }

    #[test]
    fn the_notification_names_and_links_each_resource_type() {
        let application = target(RestartKind::Application, &row("exited"), None);
        assert_eq!(
            application,
            Target {
                name: "web".into(),
                url_uuid: None,
                environment_id: Some(4)
            }
        );

        let mut preview = row("exited");
        preview.name = None;
        preview.environment_id = None;
        preview.pull_request_id = Some(12);
        let app = (Some("shop".to_owned()), "app-uuid".to_owned(), Some(9));
        assert_eq!(
            target(RestartKind::Preview, &preview, Some(&app)),
            Target {
                name: "shop PR #12".into(),
                url_uuid: Some("app-uuid".into()),
                environment_id: Some(9)
            }
        );

        let service = (Some("stack".to_owned()), "svc-uuid".to_owned(), Some(3));
        let mut part = row("exited");
        part.environment_id = None;
        assert_eq!(
            target(RestartKind::Service, &part, Some(&service)),
            Target {
                name: "web".into(),
                url_uuid: Some("svc-uuid".into()),
                environment_id: Some(3)
            }
        );

        let database = target(RestartKind::Database, &row("exited"), None);
        assert_eq!(database.url_uuid, None);
        assert_eq!(database.environment_id, Some(4));
    }
}
