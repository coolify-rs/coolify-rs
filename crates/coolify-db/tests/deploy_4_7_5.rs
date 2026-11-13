//! Queries of the Coolify v4.7.5 port of the deploy area: deployments that a newer deployment
//! replaces, build slots of build servers, the conditional status updates of the deployment job,
//! environment variable inserts that refuse a second row of a key, and service parts that are no
//! longer in the compose file. Every database here is created by the test itself on the dev
//! Postgres (`COOLIFY_RS_TEST_DATABASE_URL`) and dropped at the end. The tests need the columns of
//! the v4.7.5 migrations (`is_mcp`, `deploy_started_at`, `is_removed_from_compose`).
#![cfg(feature = "testing")]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use chrono::{NaiveDateTime, Utc};
use coolify_db::deployment_queue::{self as dq, NewerDeployment};
use coolify_db::testing::TestDb;
use sqlx::Executor;

const SEED: &str = r#"
INSERT INTO instance_settings (id) VALUES (0);
INSERT INTO teams (id, name) VALUES (1, 'team');
INSERT INTO private_keys (id, uuid, team_id, name, private_key) VALUES (1, 'key', 1, 'k', 'x');
INSERT INTO servers (id, uuid, name, ip, team_id, private_key_id) VALUES
  (1, 's1', 's1', '10.0.0.1', 1, 1),
  (2, 's2', 's2', '10.0.0.2', 1, 1),
  (3, 's3', 's3', '10.0.0.3', 1, 1);
INSERT INTO server_settings (server_id, concurrent_builds) VALUES (2, 1), (3, 2);
"#;

fn now() -> NaiveDateTime {
    Utc::now().naive_utc()
}

/// One queue row: `(uuid, status, flags as SQL, server_id, build_server_id, pull_request_id)`.
async fn deployment(
    tdb: &TestDb,
    uuid: &str,
    status: &str,
    flags: &str,
    server_id: i64,
    build_server_id: Option<i64>,
    pull_request_id: i64,
) -> i64 {
    let extra = if flags.is_empty() {
        String::new()
    } else {
        format!(", {flags}")
    };
    let columns: Vec<&str> = extra
        .split(',')
        .filter_map(|part| part.split('=').next())
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .collect();
    let values: Vec<&str> = extra
        .split(',')
        .filter_map(|part| part.split('=').nth(1))
        .map(str::trim)
        .collect();
    let (more_columns, more_values) = if columns.is_empty() {
        (String::new(), String::new())
    } else {
        (
            format!(", {}", columns.join(", ")),
            format!(", {}", values.join(", ")),
        )
    };
    let sql = format!(
        "INSERT INTO application_deployment_queues (application_id, deployment_uuid, status, server_id, build_server_id, pull_request_id{more_columns}) \
         VALUES ('7', '{uuid}', '{status}', {server_id}, {}, {pull_request_id}{more_values}) RETURNING id",
        build_server_id.map_or("NULL".to_owned(), |id| id.to_string())
    );
    sqlx::query_scalar(&sql)
        .fetch_one(tdb.db.pool())
        .await
        .unwrap()
}

async fn status_of(tdb: &TestDb, id: i64) -> String {
    sqlx::query_scalar("SELECT status FROM application_deployment_queues WHERE id = $1")
        .bind(id)
        .fetch_one(tdb.db.pool())
        .await
        .unwrap()
}

/// `cancel_superseded_deployments()`: a webhook deployment replaces the older queued webhook
/// deployments of the same application, pull request and server. Manual, API, restart and
/// rollback deployments, other pull requests and other servers stay.
#[tokio::test]
async fn a_webhook_deployment_cancels_older_queued_webhook_deployments() {
    let tdb = TestDb::new(SEED).await;
    let old = deployment(&tdb, "old", "queued", "is_webhook = true", 1, None, 0).await;
    let manual = deployment(&tdb, "manual", "queued", "", 1, None, 0).await;
    let api = deployment(&tdb, "api", "queued", "is_api = true", 1, None, 0).await;
    let rollback = deployment(
        &tdb,
        "rollback",
        "queued",
        "is_webhook = true, rollback = true",
        1,
        None,
        0,
    )
    .await;
    let restart = deployment(
        &tdb,
        "restart",
        "queued",
        "is_webhook = true, restart_only = true",
        1,
        None,
        0,
    )
    .await;
    let preview = deployment(&tdb, "preview", "queued", "is_webhook = true", 1, None, 4).await;
    let other_server = deployment(&tdb, "other", "queued", "is_webhook = true", 2, None, 0).await;
    let building = deployment(
        &tdb,
        "building",
        "in_progress",
        "is_webhook = true",
        1,
        None,
        0,
    )
    .await;
    let new = deployment(&tdb, "new", "queued", "is_webhook = true", 1, None, 0).await;

    let superseded = dq::cancel_superseded(
        &tdb.db,
        &NewerDeployment {
            id: new,
            application_id: 7,
            pull_request_id: 0,
            server_id: 1,
        },
        true,
        false,
        now(),
    )
    .await
    .unwrap();
    assert_eq!(
        superseded.queued.iter().map(|d| d.id).collect::<Vec<_>>(),
        vec![old]
    );
    assert!(superseded.building.is_empty());
    // The returned row has the status it had before the cancel.
    assert_eq!(superseded.queued[0].status, "queued");
    assert_eq!(status_of(&tdb, old).await, "cancelled-by-user");
    for kept in [manual, api, rollback, restart, preview, other_server, new] {
        assert_eq!(status_of(&tdb, kept).await, "queued", "deployment {kept}");
    }
    assert_eq!(status_of(&tdb, building).await, "in_progress");
    tdb.drop_db().await;
}

/// With `cancel_building`, a running deployment is cancelled only before it starts to deploy
/// containers. An API caller also replaces API and MCP deployments.
#[tokio::test]
async fn a_build_is_cancelled_only_before_its_deploy_phase() {
    let tdb = TestDb::new(SEED).await;
    let building = deployment(
        &tdb,
        "building",
        "in_progress",
        "is_webhook = true",
        1,
        None,
        0,
    )
    .await;
    let deploying = deployment(
        &tdb,
        "deploying",
        "in_progress",
        "is_api = true",
        1,
        None,
        0,
    )
    .await;
    let mcp = deployment(&tdb, "mcp", "queued", "is_mcp = true", 1, None, 0).await;
    let new = deployment(&tdb, "new", "queued", "is_api = true", 1, None, 0).await;
    // `startDeployPhase()` of the second one: from here on it is not cancelled.
    dq::start_deploy_phase(&tdb.db, deploying, now())
        .await
        .unwrap();
    let started: Option<NaiveDateTime> = sqlx::query_scalar(
        "SELECT deploy_started_at FROM application_deployment_queues WHERE id = $1",
    )
    .bind(deploying)
    .fetch_one(tdb.db.pool())
    .await
    .unwrap();
    assert!(started.is_some());

    let superseded = dq::cancel_superseded(
        &tdb.db,
        &NewerDeployment {
            id: new,
            application_id: 7,
            pull_request_id: 0,
            server_id: 1,
        },
        false,
        true,
        now(),
    )
    .await
    .unwrap();
    assert_eq!(
        superseded.building.iter().map(|d| d.id).collect::<Vec<_>>(),
        vec![building]
    );
    assert_eq!(
        superseded.queued.iter().map(|d| d.id).collect::<Vec<_>>(),
        vec![mcp]
    );
    assert_eq!(status_of(&tdb, deploying).await, "in_progress");
    assert_eq!(status_of(&tdb, building).await, "cancelled-by-user");

    // The deploy phase is not marked on a deployment that is not in progress.
    dq::start_deploy_phase(&tdb.db, building, now())
        .await
        .unwrap();
    let started: Option<NaiveDateTime> = sqlx::query_scalar(
        "SELECT deploy_started_at FROM application_deployment_queues WHERE id = $1",
    )
    .bind(building)
    .fetch_one(tdb.db.pool())
    .await
    .unwrap();
    assert!(started.is_none());
    tdb.drop_db().await;
}

/// `handle()`: the job moves to `in_progress` unless a cancel came first.
#[tokio::test]
async fn a_cancelled_deployment_is_not_started() {
    let tdb = TestDb::new(SEED).await;
    let queued = deployment(&tdb, "queued", "queued", "", 1, None, 0).await;
    let cancelled = deployment(&tdb, "cancelled", "cancelled-by-user", "", 1, None, 0).await;
    assert!(
        dq::start_unless_cancelled(&tdb.db, queued, "worker-1", now())
            .await
            .unwrap()
    );
    assert!(
        !dq::start_unless_cancelled(&tdb.db, cancelled, "worker-1", now())
            .await
            .unwrap()
    );
    assert_eq!(status_of(&tdb, queued).await, "in_progress");
    assert_eq!(status_of(&tdb, cancelled).await, "cancelled-by-user");
    let worker: Option<String> = sqlx::query_scalar(
        "SELECT horizon_job_worker FROM application_deployment_queues WHERE id = $1",
    )
    .bind(queued)
    .fetch_one(tdb.db.pool())
    .await
    .unwrap();
    assert_eq!(worker.as_deref(), Some("worker-1"));
    tdb.drop_db().await;
}

/// `active_builds_on_server()`: a deployment holds one build slot of the server that builds it:
/// its deployment server, or its build server.
#[tokio::test]
async fn builds_count_on_the_server_that_builds_them() {
    let tdb = TestDb::new(SEED).await;
    // Runs on server 1 and builds there.
    deployment(&tdb, "a", "in_progress", "", 1, None, 0).await;
    // Runs on server 1 and builds on build server 2.
    deployment(&tdb, "b", "in_progress", "", 1, Some(2), 0).await;
    // Queued: no slot yet.
    let queued = deployment(&tdb, "c", "queued", "", 1, Some(3), 0).await;
    assert_eq!(dq::active_builds_on_server(&tdb.db, 1).await.unwrap(), 1);
    assert_eq!(dq::active_builds_on_server(&tdb.db, 2).await.unwrap(), 1);
    assert_eq!(dq::active_builds_on_server(&tdb.db, 3).await.unwrap(), 0);

    let slots = dq::build_slots(&tdb.db, &[3, 2]).await.unwrap();
    assert_eq!(
        slots
            .iter()
            .map(|s| (s.server_id, s.builds, s.concurrent_builds))
            .collect::<Vec<_>>(),
        vec![(2, 1, 1), (3, 0, 2)]
    );
    assert!(dq::build_slots(&tdb.db, &[]).await.unwrap().is_empty());

    // Admission keeps the build server that `next_queuable()` reserved.
    dq::admit(&tdb.db, queued, None, now()).await.unwrap();
    assert_eq!(dq::active_builds_on_server(&tdb.db, 3).await.unwrap(), 1);
    assert_eq!(
        dq::team_server_ids(&tdb.db, 1).await.unwrap(),
        vec![1, 2, 3]
    );
    tdb.drop_db().await;
}

/// `GetRollbackImages::allows()`: only the commit of a successful production deployment that was
/// not a rollback counts.
#[tokio::test]
async fn only_successfully_deployed_commits_allow_a_rollback() {
    let tdb = TestDb::new(SEED).await;
    tdb.db
        .pool()
        .execute(
            "INSERT INTO application_deployment_queues (application_id, deployment_uuid, status, commit, pull_request_id, rollback) VALUES \
             ('7', 'd1', 'finished', 'aaa', 0, false), ('7', 'd2', 'failed', 'bbb', 0, false), \
             ('7', 'd3', 'finished', 'ccc', 5, false), ('7', 'd4', 'finished', 'ddd', 0, true), \
             ('8', 'd5', 'finished', 'eee', 0, false)",
        )
        .await
        .unwrap();
    assert!(dq::was_deployed(&tdb.db, 7, "aaa").await.unwrap());
    for commit in ["bbb", "ccc", "ddd", "eee", "zzz"] {
        assert!(
            !dq::was_deployed(&tdb.db, 7, commit).await.unwrap(),
            "{commit}"
        );
    }
    tdb.drop_db().await;
}

/// `EnvironmentVariable::performInsert()`: a second row of a key is refused for the same
/// resource and preview flag, and `firstOrCreate()` answers the existing row.
#[tokio::test]
async fn a_second_variable_with_the_same_key_is_not_inserted() {
    use coolify_db::compose::{NewEnv, env_insert};
    let tdb = TestDb::new(SEED).await;
    let new = |key: &str, is_preview: bool| NewEnv {
        key: key.to_owned(),
        value_encrypted: Some("cipher".into()),
        is_preview,
        is_required: false,
        is_multiline: false,
        is_literal: false,
        is_shown_once: false,
        is_shared: false,
        comment: None,
    };
    let morph = r"App\Models\Application";
    let first = env_insert(&tdb.db, morph, 7, &new("KEY", false), "4.7.5", now())
        .await
        .unwrap();
    let again = env_insert(&tdb.db, morph, 7, &new("KEY", false), "4.7.5", now())
        .await
        .unwrap();
    assert_eq!(first, again);
    // The preview copy and another resource are different variables.
    let preview = env_insert(&tdb.db, morph, 7, &new("KEY", true), "4.7.5", now())
        .await
        .unwrap();
    let other = env_insert(&tdb.db, morph, 8, &new("KEY", false), "4.7.5", now())
        .await
        .unwrap();
    assert_ne!(first, preview);
    assert_ne!(first, other);
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM environment_variables WHERE key = 'KEY'")
            .fetch_one(tdb.db.pool())
            .await
            .unwrap();
    assert_eq!(count, 3);
    tdb.drop_db().await;
}

/// The `saving` hook: a lock is permanent, but the other columns of a locked variable change.
#[tokio::test]
async fn a_locked_variable_stays_locked() {
    use coolify_db::servers::ColumnValue;
    let tdb = TestDb::new(SEED).await;
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO environment_variables (key, value, resourceable_type, resourceable_id, is_shown_once, uuid) \
         VALUES ('KEY', 'old', 'App\\Models\\Application', 7, true, 'env-1') RETURNING id",
    )
    .fetch_one(tdb.db.pool())
    .await
    .unwrap();
    let row = coolify_db::environment_variables::save_dirty(
        &tdb.db,
        id,
        &[
            ("is_shown_once", ColumnValue::Bool(false)),
            ("value", ColumnValue::Text(Some("new".into()))),
        ],
        now(),
    )
    .await
    .unwrap();
    assert_eq!(row["is_shown_once"], true);
    assert_eq!(row["value"], "new");
    tdb.drop_db().await;
}

/// `markServicePartsRemovedFromCompose()`: parts whose names are no longer in the compose file
/// are marked, a part that comes back is unmarked, and an empty compose file changes nothing.
#[tokio::test]
async fn service_parts_that_left_the_compose_file_are_marked() {
    let tdb = TestDb::new(SEED).await;
    tdb.db
        .pool()
        .execute(
            "INSERT INTO service_applications (id, uuid, name, service_id, is_removed_from_compose) VALUES \
               (1, 'a1', 'web', 5, false), (2, 'a2', 'worker', 5, false), (3, 'a3', 'back', 5, true), (4, 'a4', 'web', 6, false); \
             INSERT INTO service_databases (id, uuid, name, service_id, is_removed_from_compose) VALUES \
               (1, 'd1', 'db', 5, false), (2, 'd2', 'cache', 5, false)",
        )
        .await
        .unwrap();
    let names = vec!["web".to_owned(), "back".to_owned(), "db".to_owned()];
    coolify_db::service_parts::mark_removed_from_compose(&tdb.db, 5, &names, now())
        .await
        .unwrap();
    assert_eq!(
        coolify_db::service_parts::removed_from_compose_names(&tdb.db, 5)
            .await
            .unwrap(),
        vec!["worker".to_owned(), "cache".to_owned()]
    );
    // Another service is not touched.
    assert!(
        coolify_db::service_parts::removed_from_compose_names(&tdb.db, 6)
            .await
            .unwrap()
            .is_empty()
    );
    coolify_db::service_parts::mark_removed_from_compose(&tdb.db, 5, &[], now())
        .await
        .unwrap();
    assert_eq!(
        coolify_db::service_parts::removed_from_compose_names(&tdb.db, 5)
            .await
            .unwrap()
            .len(),
        2
    );
    tdb.drop_db().await;
}
