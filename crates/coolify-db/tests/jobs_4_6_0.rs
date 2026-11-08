//! Queries changed by the Coolify v4.6.0 port of the jobs area. Every database here is created by
//! the test itself on the dev Postgres (`COOLIFY_RS_TEST_DATABASE_URL`) and dropped at the end.
#![cfg(feature = "testing")]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use coolify_db::testing::TestDb;
use coolify_db::{Db, DbConfig};
use sqlx::Executor;

const SEED: &str = r#"
INSERT INTO instance_settings (id) VALUES (0);
INSERT INTO teams (id, name) VALUES (1, 'team');
INSERT INTO private_keys (id, uuid, team_id, name, private_key) VALUES (1, 'key', 1, 'k', 'x');
INSERT INTO servers (id, uuid, name, ip, team_id, private_key_id) VALUES
  (1, 's1', 's1', '10.0.0.1', 1, 1),
  (2, 's2', 's2', '10.0.0.2', 1, 1),
  (3, 's3', 's3', '10.0.0.3', 1, 1),
  (4, 's4', 's4', '10.0.0.4', 1, 1);
INSERT INTO github_apps (id, uuid, name, api_url, html_url, team_id) VALUES (1, 'gh', 'gh', 'https://api.github.com', 'https://github.com', 1);
INSERT INTO github_runner_configs (id, uuid, server_id, github_app_id, labels, is_enabled) VALUES
  (1, 'c1', 1, 1, '[]', true),
  (2, 'c2', 2, 1, '[]', false),
  (3, 'c3', 3, 1, '[]', false);
INSERT INTO github_runner_executions (id, uuid, github_app_id, server_id, status, trigger_workflow_job_id) VALUES
  (1, 'e1', 1, 1, 'running', 1),
  (2, 'e2', 1, 2, 'idle', 2),
  (3, 'e3', 1, 3, 'completed', 3),
  (4, 'e4', 1, 4, 'provisioning', 4);
INSERT INTO scheduled_tasks (id, uuid, name, command, frequency, enabled, team_id, created_at, updated_at)
  VALUES (1, 'task', 't', 'echo x', '* * * * *', true, 1, now(), now());
"#;

/// `ReconcileGithubRunnersJob::serversToReconcile()`: enabled runners, or executions that still
/// occupy a runner; each server once.
#[tokio::test]
async fn runner_servers_come_from_a_union_of_server_ids() {
    let tdb = TestDb::new(SEED).await;
    let ids = coolify_db::github_runners::reconcile_server_ids(&tdb.db)
        .await
        .unwrap();
    assert_eq!(ids, vec![1, 2, 4]);
    tdb.drop_db().await;
}

/// `ServerManagerJob` loads servers in batches by id, with settings but without
/// `patch_check_result` (`ServerSetting::withoutPatchCheckResult()`).
#[tokio::test]
async fn server_batches_leave_out_the_patch_check_result() {
    let tdb = TestDb::new(SEED).await;
    tdb.db
        .pool()
        .execute(
            "INSERT INTO server_settings (server_id, docker_cleanup_frequency, patch_check_result) \
             VALUES (2, '0 * * * *', '{\"updates\": [1, 2, 3]}')",
        )
        .await
        .unwrap();
    assert_eq!(
        coolify_db::servers::ids(&tdb.db).await.unwrap(),
        vec![1, 2, 3, 4]
    );
    let rows = coolify_db::servers::batch_without_patch_check_result(&tdb.db, &[4, 2])
        .await
        .unwrap();
    assert_eq!(rows.iter().map(|r| r.id).collect::<Vec<_>>(), vec![2, 4]);
    let settings = rows[0].settings.as_ref().unwrap();
    assert_eq!(settings["docker_cleanup_frequency"], "0 * * * *");
    assert!(settings.contains_key("server_id"));
    assert!(!settings.contains_key("patch_check_result"));
    assert!(rows[1].settings.is_none());

    let all = coolify_db::servers::all_without_patch_check_result(&tdb.db)
        .await
        .unwrap();
    assert_eq!(all.len(), 4);
    assert!(all.iter().all(|r| {
        r.settings
            .as_ref()
            .is_none_or(|s| !s.contains_key("patch_check_result"))
    }));
    // The full load still has it.
    let full = coolify_db::servers::all(&tdb.db).await.unwrap();
    assert!(full.iter().any(|r| {
        r.settings
            .as_ref()
            .is_some_and(|s| s.contains_key("patch_check_result"))
    }));
    tdb.drop_db().await;
}

/// `RecordsWorkerHost`: on a v4.6.0 schema the execution stores the host; on v4.5.3 it has no
/// such column and the insert still works.
#[tokio::test]
async fn executions_record_the_worker_host_when_the_column_exists() {
    let tdb = TestDb::new(SEED).await;
    let now = chrono::Utc::now().naive_utc();
    let old = coolify_db::scheduled_tasks::start_execution(&tdb.db, "old", 1, 0, now)
        .await
        .unwrap();
    assert!(old > 0);

    tdb.db
        .pool()
        .execute(
            "ALTER TABLE scheduled_task_executions ADD COLUMN worker_host varchar(255); \
             ALTER TABLE scheduled_database_backup_executions ADD COLUMN worker_host varchar(255); \
             ALTER TABLE scheduled_volume_backup_executions ADD COLUMN worker_host varchar(255);",
        )
        .await
        .unwrap();
    // The columns are discovered when the handle connects.
    let admin = std::env::var("COOLIFY_RS_TEST_DATABASE_URL").unwrap();
    let url = format!("{}/{}", &admin[..admin.rfind('/').unwrap()], tdb.name);
    let db = Db::connect(&DbConfig {
        url,
        max_connections: 2,
        acquire_timeout: std::time::Duration::from_secs(10),
        allow_unknown_schema: false,
    })
    .await
    .unwrap();
    assert!(
        db.schema()
            .has_column("scheduled_task_executions", "worker_host")
    );

    let new = coolify_db::scheduled_tasks::start_execution(&db, "new", 1, 0, now)
        .await
        .unwrap();
    coolify_db::scheduled_tasks::record_missed_execution(&db, 1, "missed", now)
        .await
        .unwrap();
    let host = coolify_db::worker_host::worker_host();
    let hosts: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT uuid, worker_host FROM scheduled_task_executions ORDER BY id")
            .fetch_all(db.pool())
            .await
            .unwrap();
    assert_eq!(hosts.len(), 3);
    assert_eq!(hosts[0], ("old".to_owned(), None));
    assert_eq!(hosts[1], ("new".to_owned(), Some(host.to_owned())));
    assert_eq!(hosts[2].1.as_deref(), Some(host));
    assert!(new > old);
    db.pool().close().await;
    tdb.drop_db().await;
}

/// `CleanupHelperContainersJob`: only helper containers named after a deployment that is not
/// queued or in progress are removed; backups, imports and file browsers keep theirs.
#[tokio::test]
async fn only_finished_deployment_helpers_are_cleaned_up() {
    let tdb = TestDb::new(SEED).await;
    tdb.db
        .pool()
        .execute(
            "INSERT INTO application_deployment_queues (application_id, deployment_uuid, status) VALUES \
             ('1', 'done', 'finished'), ('1', 'failed', 'failed'), ('1', 'running', 'in_progress'), ('1', 'waiting', 'queued')",
        )
        .await
        .unwrap();
    let names: Vec<String> = ["done", "failed", "running", "waiting", "backup-helper"]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    let mut finished = coolify_db::instance_jobs::finished_deployment_uuids(&tdb.db, &names)
        .await
        .unwrap();
    finished.sort();
    assert_eq!(finished, vec!["done".to_owned(), "failed".to_owned()]);
    tdb.drop_db().await;
}
