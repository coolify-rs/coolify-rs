//! The Kernel's jobs and the sources of the scheduler on a private test database, with notifications
//! going to a local endpoint and nothing reaching a server.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{TimeZone, Utc};
use coolify_compat::Encrypter;
use coolify_db::Db;
use coolify_db::notification_settings as ns;
use coolify_db::testing::TestDb;
use coolify_jobs::Jobs;
use coolify_jobs::kernel::{self, KernelJob};
use coolify_jobs::schedule_sources::{
    DatabaseBackupSource, DockerCleanupSource, VolumeBackupSource,
};
use coolify_jobs::scheduled_task::ScheduledTaskSource;
use coolify_jobs::scheduler::Scheduler;
use coolify_jobs::server_ops::push_update::push_server_update;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const KEY: &str = "base64:AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=";

const SEED: &str = r#"
INSERT INTO instance_settings (id, public_ipv4) VALUES (0, '203.0.113.5');
INSERT INTO teams (id, name, personal_team) VALUES (0, 'root', false), (1, 'acme', false), (2, 'empty', false);
INSERT INTO users (id, name, email) VALUES (1, 'a', 'owner@acme.test');
INSERT INTO team_user (team_id, user_id, role) VALUES (1, 1, 'owner');
INSERT INTO private_keys (id, uuid, name, private_key, team_id) VALUES (1, 'k', 'k', 'x', 1);
INSERT INTO servers (id, uuid, name, ip, team_id, private_key_id) VALUES (1, 'srv', 'web-1', '10.0.0.1', 1, 1);
INSERT INTO server_settings (server_id, is_reachable, is_usable, docker_cleanup_frequency) VALUES (1, false, false, '* * * * *');
INSERT INTO standalone_dockers (id, uuid, name, network, server_id) VALUES (1, 'd1', 'd', 'coolify', 1);
INSERT INTO projects (id, uuid, name, team_id) VALUES (1, 'p-uuid', 'Proj', 1);
INSERT INTO environments (id, uuid, name, project_id) VALUES (1, 'e-uuid', 'production', 1);
INSERT INTO applications (id, uuid, name, environment_id, build_pack, git_branch, git_repository, status, destination_type, destination_id, max_restart_count, fqdn)
  VALUES (1, 'app-uuid', 'my-app', 1, 'dockerimage', 'main', 'r', 'running:healthy', 'App\Models\StandaloneDocker', 1, 3, 'https://a.test');
INSERT INTO application_settings (application_id) VALUES (1);
INSERT INTO standalone_postgresqls (id, uuid, name, postgres_password, environment_id, destination_type, destination_id, status)
  VALUES (1, 'pg-uuid', 'pg', 'x', 1, 'App\Models\StandaloneDocker', 1, 'running:healthy');
"#;

/// Answers 200 and records the bodies it got, per path.
#[derive(Clone, Default)]
struct Endpoint {
    bodies: Arc<Mutex<Vec<(String, Value)>>>,
    replies: Arc<Mutex<Vec<(String, String)>>>,
}

impl Endpoint {
    fn bodies(&self) -> Vec<(String, Value)> {
        self.bodies.lock().unwrap().clone()
    }

    fn reply(&self, path: &str, body: &str) {
        self.replies
            .lock()
            .unwrap()
            .push((path.to_owned(), body.to_owned()));
    }

    async fn wait_for(&self, count: usize) -> Vec<(String, Value)> {
        for _ in 0..100 {
            if self.bodies.lock().unwrap().len() >= count {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        self.bodies()
    }

    async fn start(&self) -> u16 {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let me = self.clone();
        tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let me = me.clone();
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 4096];
                    let (head_end, length) = loop {
                        let n = stream.read(&mut chunk).await.unwrap();
                        buf.extend_from_slice(&chunk[..n]);
                        let text = String::from_utf8_lossy(&buf).to_string();
                        if let Some(end) = text.find("\r\n\r\n") {
                            let length = text[..end]
                                .lines()
                                .find_map(|l| {
                                    l.to_ascii_lowercase()
                                        .strip_prefix("content-length:")
                                        .map(|v| v.trim().parse::<usize>().unwrap())
                                })
                                .unwrap_or(0);
                            break (end + 4, length);
                        }
                    };
                    while buf.len() < head_end + length {
                        let n = stream.read(&mut chunk).await.unwrap();
                        buf.extend_from_slice(&chunk[..n]);
                    }
                    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
                    let path = head.split_whitespace().nth(1).unwrap_or("").to_owned();
                    let body = serde_json::from_slice(&buf[head_end..head_end + length])
                        .unwrap_or(Value::Null);
                    me.bodies.lock().unwrap().push((path.clone(), body));
                    let answer = me
                        .replies
                        .lock()
                        .unwrap()
                        .iter()
                        .find(|(p, _)| *p == path)
                        .map_or("{}".to_owned(), |(_, b)| b.clone());
                    let reply = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
                        answer.len()
                    );
                    let _ = stream.write_all(reply.as_bytes()).await;
                });
            }
        });
        port
    }
}

async fn setup() -> (TestDb, Jobs, Endpoint, tempfile::TempDir) {
    let t = TestDb::new(SEED).await;
    let enc = Encrypter::new(KEY).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let endpoint = Endpoint::default();
    let port = endpoint.start().await;
    sqlx::query("UPDATE instance_settings SET webhook_allow_localhost = true, webhook_allowed_internal_hosts = '[\"127.0.0.1\"]' WHERE id = 0")
        .execute(t.db.pool())
        .await
        .unwrap();
    for ch in ns::ALL {
        ns::first_or_create(&t.db, ch, 1).await.unwrap();
    }
    let url = enc.encrypt_string(&format!("http://127.0.0.1:{port}/webhook/secret"));
    sqlx::query(
        "UPDATE webhook_notification_settings SET webhook_enabled = true, webhook_url = $1 WHERE team_id = 1",
    )
    .bind(url)
    .execute(t.db.pool())
    .await
    .unwrap();
    let remote =
        coolify_remote::Remote::new(coolify_remote::RemoteConfig::new(dir.path())).unwrap();
    let jobs = Jobs::new(t.db.clone(), remote, Some(enc));
    (t, jobs, endpoint, dir)
}

fn now() -> chrono::DateTime<Utc> {
    Utc::now()
}

async fn count(db: &Db, sql: &str) -> i64 {
    sqlx::query_scalar(sql).fetch_one(db.pool()).await.unwrap()
}

#[tokio::test]
async fn sources_record_a_skip_on_a_server_that_is_not_functional() {
    let (t, jobs, _endpoint, _dir) = setup().await;
    let db = &t.db;
    sqlx::query(
        "INSERT INTO scheduled_database_backups (id, uuid, team_id, frequency, enabled, database_id, database_type) \
         VALUES (1, 'b1', 1, '* * * * *', true, 1, 'App\\Models\\StandalonePostgresql')",
    )
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO local_persistent_volumes (id, uuid, name, mount_path, resource_type, resource_id) \
         VALUES (1, 'v1', 'data', '/data', 'App\\Models\\Application', 1)",
    )
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO scheduled_volume_backups (id, uuid, team_id, frequency, enabled, backupable_type, backupable_id) \
         VALUES (1, 'vb1', 1, '* * * * *', true, 'App\\Models\\LocalPersistentVolume', 1)",
    )
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO scheduled_tasks (id, uuid, name, command, frequency, enabled, team_id, application_id) \
         VALUES (1, 't1', 't', 'echo', '* * * * *', true, 1, 1)",
    )
    .execute(db.pool())
    .await
    .unwrap();
    let mut scheduler = Scheduler::new(jobs.clone());
    scheduler
        .register(DatabaseBackupSource)
        .register(ScheduledTaskSource)
        .register(VolumeBackupSource)
        .register(DockerCleanupSource);
    let at = now();
    let summary = scheduler.tick(at).await;
    assert_eq!(summary.dispatched, 0);
    assert_eq!(summary.skipped, 4, "{summary:?}");
    // A second tick in the same minute skips nothing new.
    assert_eq!(scheduler.tick(at).await.skipped, 0);
    for key in [
        "scheduled-backup:1",
        "scheduled-volume-backup:1",
        "scheduled-task:1",
        "docker-cleanup:1",
    ] {
        let states: i64 =
            sqlx::query_scalar("SELECT count(*) FROM scheduled_job_states WHERE schedule_key = $1")
                .bind(key)
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!(states, 1, "{key}");
    }
    assert_eq!(
        count(db, "SELECT count(*) FROM scheduled_job_deliveries").await,
        0
    );
}

#[tokio::test]
async fn a_schedule_without_its_resource_is_deleted() {
    let (t, jobs, _endpoint, _dir) = setup().await;
    let db = &t.db;
    sqlx::query(
        "INSERT INTO scheduled_database_backups (id, uuid, team_id, frequency, enabled, database_id, database_type) \
         VALUES (2, 'gone', 1, '* * * * *', true, 999, 'App\\Models\\StandalonePostgresql')",
    )
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO scheduled_volume_backups (id, uuid, team_id, frequency, enabled, backupable_type, backupable_id) \
         VALUES (2, 'gone', 1, '* * * * *', true, 'App\\Models\\LocalPersistentVolume', 999)",
    )
    .execute(db.pool())
    .await
    .unwrap();
    let mut scheduler = Scheduler::new(jobs);
    scheduler
        .register(DatabaseBackupSource)
        .register(VolumeBackupSource);
    scheduler.tick(now()).await;
    assert_eq!(
        count(db, "SELECT count(*) FROM scheduled_database_backups").await,
        0
    );
    assert_eq!(
        count(db, "SELECT count(*) FROM scheduled_volume_backups").await,
        0
    );
}

#[tokio::test]
async fn a_lost_occurrence_fails_and_notifies_once_an_hour() {
    let (t, jobs, endpoint, _dir) = setup().await;
    let db = &t.db;
    sqlx::query(
        "INSERT INTO scheduled_tasks (id, uuid, name, command, frequency, enabled, team_id, application_id) \
         VALUES (1, 't1', 'nightly', 'echo', '0 0 1 1 *', true, 1, 1)",
    )
    .execute(db.pool())
    .await
    .unwrap();
    // Two runs that never started, more than an hour old.
    for (id, minutes) in [(1, 120), (2, 90)] {
        sqlx::query(&format!(
            "INSERT INTO scheduled_job_deliveries (id, uuid, schedule_key, scheduled_for, job_type, resource_id, payload, status, created_at, updated_at, enqueued_at) \
             VALUES ({id}, 'u{id}', 'scheduled-task:1', now() - interval '{minutes} minutes', 'scheduled-task', 1, '[]', 'enqueued', now() - interval '{minutes} minutes', now() - interval '{minutes} minutes', now() - interval '{minutes} minutes')"
        ))
        .execute(db.pool())
        .await
        .unwrap();
    }
    let mut scheduler = Scheduler::new(jobs);
    scheduler.register(ScheduledTaskSource);
    scheduler.tick(now()).await;
    assert_eq!(
        count(
            db,
            "SELECT count(*) FROM scheduled_task_executions WHERE status = 'failed'"
        )
        .await,
        2
    );
    let bodies = endpoint.wait_for(1).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(endpoint.bodies().len(), 1, "{bodies:?}");
    assert_eq!(bodies[0].1["event"], "task_failed");
    assert_eq!(bodies[0].1["task_name"], "nightly");
}

#[tokio::test]
async fn api_tokens_about_to_expire_are_warned_about_once() {
    let (t, jobs, endpoint, _dir) = setup().await;
    let db = &t.db;
    for (id, name, hours) in [(1, "soon", 5), (2, "later", 48), (3, "past", -5)] {
        sqlx::query(&format!(
            "INSERT INTO personal_access_tokens (id, tokenable_type, tokenable_id, name, token, team_id, expires_at) \
             VALUES ({id}, 'App\\Models\\User', 1, '{name}', 'x{id}', '1', now() + interval '{hours} hours')"
        ))
        .execute(db.pool())
        .await
        .unwrap();
    }
    kernel::run_job(&jobs, KernelJob::ApiTokenExpirationWarning, now())
        .await
        .unwrap();
    kernel::run_job(&jobs, KernelJob::ApiTokenExpirationWarning, now())
        .await
        .unwrap();
    let bodies = endpoint.wait_for(1).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(endpoint.bodies().len(), 1, "{bodies:?}");
    assert!(bodies[0].1.to_string().contains("soon"));
    // `sanctum:prune-expired --hours=1` leaves the others.
    kernel::run_job(&jobs, KernelJob::PruneExpiredTokens, now())
        .await
        .unwrap();
    assert_eq!(
        count(
            db,
            "SELECT count(*) FROM personal_access_tokens WHERE name = 'past'"
        )
        .await,
        0
    );
    assert_eq!(
        count(db, "SELECT count(*) FROM personal_access_tokens").await,
        2
    );
}

#[tokio::test]
async fn missing_backups_notify_once_per_period() {
    let (t, jobs, endpoint, _dir) = setup().await;
    let db = &t.db;
    sqlx::query(
        "INSERT INTO scheduled_database_backups (id, uuid, team_id, frequency, enabled, database_id, database_type, missing_backup_notification_days, created_at) \
         VALUES (1, 'b1', 1, '0 0 * * *', true, 1, 'App\\Models\\StandalonePostgresql', 2, now() - interval '5 days'), \
                (2, 'b2', 1, '0 0 * * *', true, 1, 'App\\Models\\StandalonePostgresql', 7, now() - interval '5 days'), \
                (3, 'b3', 1, '0 0 * * *', true, 1, 'App\\Models\\StandalonePostgresql', 0, now() - interval '50 days')",
    )
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO local_persistent_volumes (id, uuid, name, mount_path, resource_type, resource_id) \
         VALUES (1, 'v1', 'data', '/data', 'App\\Models\\Application', 1)",
    )
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO scheduled_volume_backups (id, uuid, team_id, frequency, enabled, backupable_type, backupable_id, missing_backup_notification_days, created_at) \
         VALUES (1, 'vb1', 1, '0 0 * * *', true, 'App\\Models\\LocalPersistentVolume', 1, 2, now() - interval '5 days')",
    )
    .execute(db.pool())
    .await
    .unwrap();
    for _ in 0..2 {
        kernel::run_job(&jobs, KernelJob::CheckMissingDatabaseBackups, now())
            .await
            .unwrap();
        kernel::run_job(&jobs, KernelJob::CheckMissingVolumeBackups, now())
            .await
            .unwrap();
    }
    let bodies = endpoint.wait_for(2).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(endpoint.bodies().len(), 2, "{bodies:?}");
    let text = serde_json::to_string(&bodies).unwrap();
    assert!(text.contains("b1"), "{text}");
    assert!(!text.contains("b2") && !text.contains("b3"), "{text}");
    assert!(text.contains("data"), "{text}");
}

#[tokio::test]
async fn instance_cleanups_remove_only_what_upstream_removes() {
    let (t, jobs, _endpoint, _dir) = setup().await;
    let db = &t.db;
    sqlx::query(
        "INSERT INTO team_invitations (id, uuid, team_id, email, role, link, via, created_at, updated_at) VALUES \
         (1, 'old', 1, 'old@x.test', 'member', 'l', 'link', now() - interval '4 days', now()), \
         (2, 'new', 1, 'new@x.test', 'member', 'l', 'link', now(), now())",
    )
    .execute(db.pool())
    .await
    .unwrap();
    // Invited, never verified, must reset the password, alone in a personal team.
    sqlx::raw_sql(
        "INSERT INTO users (id, name, email, force_password_reset) VALUES (5, 'o', 'old@x.test', true); \
         INSERT INTO teams (id, name, personal_team) VALUES (5, 'o', true); \
         INSERT INTO team_user (team_id, user_id, role) VALUES (5, 5, 'owner');",
    )
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::raw_sql(
        "UPDATE users SET pending_email = 'p@x.test', email_change_code = '1', email_change_code_expires_at = now() - interval '1 minute' WHERE id = 1; \
         INSERT INTO notification_throttles (notifiable_type, notifiable_id, notification, sent_at) \
           VALUES ('App\\Models\\Server', 999, 'x', now()), ('App\\Models\\Server', 1, 'x', now());",
    )
    .execute(db.pool())
    .await
    .unwrap();
    kernel::run_job(&jobs, KernelJob::CleanupInstanceStuffs, now())
        .await
        .unwrap();
    assert_eq!(
        count(
            db,
            "SELECT count(*) FROM team_invitations WHERE uuid = 'old'"
        )
        .await,
        0
    );
    assert_eq!(
        count(
            db,
            "SELECT count(*) FROM team_invitations WHERE uuid = 'new'"
        )
        .await,
        1
    );
    assert_eq!(
        count(db, "SELECT count(*) FROM users WHERE id = 5").await,
        0
    );
    assert_eq!(
        count(
            db,
            "SELECT count(*) FROM users WHERE id = 1 AND pending_email IS NULL"
        )
        .await,
        1
    );
    assert_eq!(
        count(db, "SELECT count(*) FROM notification_throttles").await,
        1
    );

    // `cleanup:database --yes`.
    sqlx::raw_sql(
        "INSERT INTO application_deployment_queues (id, deployment_uuid, application_id, created_at) VALUES \
           (1, 'a', '1', now() - interval '61 days'), (2, 'b', '1', now()); \
         INSERT INTO sessions (id, last_activity, payload) VALUES ('old', 1, ''), ('new', 2147483647, '');",
    )
    .execute(db.pool())
    .await
    .unwrap();
    kernel::run_job(&jobs, KernelJob::CleanupDatabase, now())
        .await
        .unwrap();
    assert_eq!(
        count(db, "SELECT count(*) FROM application_deployment_queues").await,
        1
    );
    assert_eq!(count(db, "SELECT count(*) FROM sessions").await, 1);

    // `cleanup:unreachable-servers`.
    sqlx::raw_sql(
        "UPDATE servers SET unreachable_count = 3, updated_at = now() - interval '8 days' WHERE id = 1; \
         INSERT INTO notification_throttles (notifiable_type, notifiable_id, notification, sent_at) \
           VALUES ('App\\Models\\Server', 1, 'App\\Notifications\\Server\\Unreachable', now());",
    )
    .execute(db.pool())
    .await
    .unwrap();
    kernel::run_job(&jobs, KernelJob::CleanupUnreachableServers, now())
        .await
        .unwrap();
    assert_eq!(
        count(
            db,
            "SELECT count(*) FROM server_settings WHERE server_id = 1 AND force_disabled"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn stuck_resources_go_and_attached_ones_stay() {
    let (t, jobs, _endpoint, _dir) = setup().await;
    let db = &t.db;
    sqlx::raw_sql(
        "INSERT INTO application_deployment_queues (id, deployment_uuid, application_id) VALUES (1, 'gone', '999'), (2, 'here', '1'); \
         INSERT INTO scheduled_tasks (id, uuid, name, command, frequency, team_id) VALUES (1, 'orphan', 't', 'x', '* * * * *', 1); \
         INSERT INTO scheduled_tasks (id, uuid, name, command, frequency, team_id, application_id) VALUES (2, 'kept', 't', 'x', '* * * * *', 1, 1);",
    )
    .execute(db.pool())
    .await
    .unwrap();
    kernel::run_job(&jobs, KernelJob::CleanupStuckedResources, now())
        .await
        .unwrap();
    assert_eq!(
        count(db, "SELECT count(*) FROM teams WHERE id = 2").await,
        0
    );
    assert_eq!(
        count(db, "SELECT count(*) FROM teams WHERE id IN (0, 1)").await,
        2
    );
    assert_eq!(
        count(db, "SELECT count(*) FROM application_deployment_queues").await,
        1
    );
    assert_eq!(count(db, "SELECT count(*) FROM scheduled_tasks").await, 1);
    assert_eq!(
        count(db, "SELECT count(*) FROM applications WHERE id = 1").await,
        1
    );
    assert_eq!(
        count(
            db,
            "SELECT count(*) FROM standalone_postgresqls WHERE id = 1"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn certificates_near_their_end_are_selected() {
    let (t, _jobs, _endpoint, _dir) = setup().await;
    let db = &t.db;
    sqlx::query(
        "INSERT INTO ssl_certificates (id, ssl_certificate, ssl_private_key, server_id, valid_until, is_ca_certificate, common_name, resource_type, resource_id) VALUES \
         (1, 'x', 'x', 1, now() + interval '3 days', false, 'a', 'App\\Models\\StandalonePostgresql', 1), \
         (2, 'x', 'x', 1, now() + interval '30 days', false, 'b', 'App\\Models\\StandalonePostgresql', 1), \
         (3, 'x', 'x', 1, now() + interval '3 days', true, 'ca', NULL, NULL)",
    )
    .execute(db.pool())
    .await
    .unwrap();
    let now = Utc
        .timestamp_opt(Utc::now().timestamp(), 0)
        .unwrap()
        .naive_utc();
    let due = coolify_db::instance_jobs::certificates_to_regenerate(db, None, false, now)
        .await
        .unwrap();
    assert_eq!(due.iter().map(|c| c.id).collect::<Vec<_>>(), [1]);
    let forced = coolify_db::instance_jobs::certificates_to_regenerate(db, Some(1), true, now)
        .await
        .unwrap();
    assert_eq!(forced.iter().map(|c| c.id).collect::<Vec<_>>(), [1, 2]);
    assert!(
        coolify_db::instance_jobs::certificates_to_regenerate(db, Some(2), true, now)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn s3_storages_that_stay_unusable_keep_their_state() {
    let (t, jobs, _endpoint, _dir) = setup().await;
    let db = &t.db;
    let enc = Encrypter::new(KEY).unwrap();
    sqlx::query(
        "INSERT INTO s3_storages (id, uuid, name, region, key, secret, bucket, endpoint, team_id, is_usable) \
         VALUES (1, 's1', 's', 'r', $1, $1, 'bucket', 'http://127.0.0.1:1', 1, false)",
    )
    .bind(enc.encrypt_string("x"))
    .execute(db.pool())
    .await
    .unwrap();
    kernel::run_job(&jobs, KernelJob::RevalidateUnusableS3Storages, now())
        .await
        .unwrap();
    assert_eq!(
        count(
            db,
            "SELECT count(*) FROM s3_storages WHERE is_usable = false"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn a_sentinel_push_updates_statuses_and_the_restart_limit() {
    let (t, jobs, endpoint, _dir) = setup().await;
    let db = &t.db;
    let managed = |extra: Value| {
        let mut l = json!({"coolify.managed": "true"});
        l.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        l
    };
    // The application crashed three times, its limit; the database stopped.
    let data = json!({
        "containers": [
            {"name": "app", "state": "restarting", "restart_count": 3, "labels": managed(json!({
                "coolify.applicationId": "1", "coolify.applicationUuid": "app-uuid",
                "com.docker.compose.service": "web"}))},
        ],
    });
    push_server_update(&jobs, 1, data).await.unwrap();
    let row: (i32, bool) = sqlx::query_as(
        "SELECT restart_count, restart_limit_reached FROM applications WHERE id = 1",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(row, (3, true));
    assert!(
        sqlx::query_scalar::<_, String>("SELECT status FROM standalone_postgresqls WHERE id = 1")
            .fetch_one(db.pool())
            .await
            .unwrap()
            .starts_with("exited")
    );
    let bodies = endpoint.wait_for(1).await;
    assert!(
        serde_json::to_string(&bodies).unwrap().contains("my-app"),
        "{bodies:?}"
    );

    // An incomplete snapshot says nothing about what is missing from it.
    sqlx::raw_sql("UPDATE standalone_postgresqls SET status = 'running:healthy' WHERE id = 1")
        .execute(db.pool())
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_secs(1)).await;
    let incomplete = json!({
        "snapshot": {"complete": false},
        "containers": [{"name": "other", "state": "running", "labels": managed(json!({}))}],
    });
    // The lock of the job above is released when it returns.
    push_server_update(&jobs, 1, incomplete).await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT status FROM standalone_postgresqls WHERE id = 1")
            .fetch_one(db.pool())
            .await
            .unwrap(),
        "running:healthy"
    );
}

#[tokio::test]
async fn cdn_jobs_use_a_local_fake() {
    let endpoint = Endpoint::default();
    let port = endpoint.start().await;
    endpoint.reply(
        "/templates.json",
        r#"{"pocketbase": {"documentation": "x", "compose": "c"}}"#,
    );
    endpoint.reply("/bad.json", "[]");
    endpoint.reply(
        "/releases.json",
        r#"[{"tag_name": "v9", "name": "Nine", "body": "b", "draft": false, "published_at": "2026-09-05T10:00:00Z"}]"#,
    );
    endpoint.reply(
        "/versions.json",
        r#"{"coolify": {"v4": {"version": "9.9.9"}, "helper": {"version": "9.0.0"}}}"#,
    );
    let dir = tempfile::tempdir().unwrap();
    let base = format!("http://127.0.0.1:{port}");
    let t = TestDb::new(SEED).await;
    let remote =
        coolify_remote::Remote::new(coolify_remote::RemoteConfig::new(dir.path())).unwrap();
    let jobs = Jobs::new(t.db.clone(), remote, None);
    jobs.set_version("4.0.0".to_owned());

    // A bundle that is not a map keeps the current one.
    assert!(!kernel::fetch_templates_bundle(&format!("{base}/bad.json")).await);
    kernel::fetch_changelog(&format!("{base}/releases.json"), dir.path(), now()).await;
    let month: Value = serde_json::from_str(
        &std::fs::read_to_string(dir.path().join("changelogs/2026-09.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(month["entries"][0]["title"], "Nine");

    let versions =
        kernel::refresh_versions_from(&jobs, &format!("{base}/versions.json"), dir.path(), 1)
            .await
            .unwrap();
    assert_eq!(versions["coolify"]["v4"]["version"], "9.9.9");
    assert!(dir.path().join("versions.json").exists());
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            "SELECT helper_version FROM instance_settings WHERE id = 0"
        )
        .fetch_one(t.db.pool())
        .await
        .unwrap(),
        "9.0.0"
    );
    // A failing CDN leaves everything alone.
    assert!(
        kernel::refresh_versions_from(&jobs, "http://127.0.0.1:1/versions.json", dir.path(), 1)
            .await
            .is_none()
    );
    assert!(endpoint.bodies().iter().all(|(p, _)| p.starts_with('/')));
}

#[tokio::test]
async fn a_task_notifies_for_applications_and_services() {
    let (t, jobs, endpoint, _dir) = setup().await;
    let db = &t.db;
    sqlx::raw_sql(
        "INSERT INTO services (id, uuid, name, environment_id, destination_type, destination_id, server_id, docker_compose_raw) \
           VALUES (1, 'svc-uuid', 'svc', 1, 'App\\Models\\StandaloneDocker', 1, 1, 'services: {}'); \
         INSERT INTO scheduled_tasks (id, uuid, name, command, frequency, enabled, team_id, application_id) \
           VALUES (1, 'a-task', 'app-task', 'x', '* * * * *', true, 1, 1); \
         INSERT INTO scheduled_tasks (id, uuid, name, command, frequency, enabled, team_id, service_id) \
           VALUES (2, 's-task', 'svc-task', 'x', '* * * * *', true, 1, 1);",
    )
    .execute(db.pool())
    .await
    .unwrap();
    // The server is not reachable, so both runs fail and `failed()` tells the team.
    jobs.dispatch_scheduled_task(1);
    jobs.dispatch_scheduled_task(2);
    let bodies = endpoint.wait_for(2).await;
    let text = serde_json::to_string(&bodies).unwrap();
    assert!(
        text.contains("app-task") && text.contains("svc-task"),
        "{text}"
    );
    assert!(
        text.contains("/application/app-uuid/tasks/a-task"),
        "{text}"
    );
    assert!(text.contains("/service/svc-uuid/tasks/s-task"), "{text}");
    assert_eq!(
        count(
            db,
            "SELECT count(*) FROM scheduled_task_executions WHERE status = 'failed'"
        )
        .await,
        2
    );
}

#[tokio::test]
async fn regenerating_certificates_replaces_the_database_certificate() {
    let (t, jobs, endpoint, _dir) = setup().await;
    let db = &t.db;
    let enc = Encrypter::new(KEY).unwrap();
    let (certificate, key, until) =
        coolify_jobs::server_ops::certificate::new_ca_certificate("Coolify CA Certificate", 3650)
            .unwrap();
    sqlx::query(
        "INSERT INTO ssl_certificates (ssl_certificate, ssl_private_key, server_id, valid_until, is_ca_certificate, common_name, subject_alternative_names) \
         VALUES ($1, $2, 1, $3, true, 'Coolify CA Certificate', '[]')",
    )
    .bind(enc.encrypt_string(&certificate))
    .bind(enc.encrypt_string(&key))
    .bind(until)
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO ssl_certificates (ssl_certificate, ssl_private_key, server_id, valid_until, is_ca_certificate, common_name, subject_alternative_names, resource_type, resource_id) \
         VALUES ('old', 'old', 1, now() + interval '3 days', false, 'pg-uuid', '[]', 'App\\Models\\StandalonePostgresql', 1)",
    )
    .execute(db.pool())
    .await
    .unwrap();
    jobs.regenerate_ssl_certificates(Some(1), None, false)
        .await
        .unwrap();
    let (stored, days): (String, f64) = sqlx::query_as(
        "SELECT ssl_certificate, extract(epoch FROM valid_until - now())::float8 / 86400 FROM ssl_certificates \
         WHERE is_ca_certificate = false AND resource_id = 1",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_ne!(stored, "old");
    assert!(
        enc.decrypt_string(&stored)
            .unwrap()
            .contains("BEGIN CERTIFICATE")
    );
    assert!(days > 360.0, "{days}");
    assert_eq!(
        count(
            db,
            "SELECT count(*) FROM local_file_volumes WHERE resource_id = 1"
        )
        .await,
        2
    );
    // The team passed to the job hears about it, and the new certificate is not due again.
    let bodies = endpoint.wait_for(1).await;
    assert!(
        serde_json::to_string(&bodies).unwrap().contains("pg"),
        "{bodies:?}"
    );
    jobs.regenerate_ssl_certificates(None, None, false)
        .await
        .unwrap();
    assert_eq!(count(db, "SELECT count(*) FROM ssl_certificates").await, 2);
}

#[tokio::test]
async fn pull_request_comments_go_to_the_source_of_the_application() {
    use coolify_jobs::applications::previews::PullRequestStatus;
    use rsa::pkcs1::EncodeRsaPrivateKey;

    let (t, jobs, _endpoint, _dir) = setup().await;
    let db = &t.db;
    let enc = Encrypter::new(KEY).unwrap();
    let github = Endpoint::default();
    let port = github.start().await;
    github.reply(
        "/app/installations/5/access_tokens",
        r#"{"token": "fake-token"}"#,
    );
    github.reply("/repos/o/r/issues/7/comments", r#"{"id": 99}"#);
    let mut rng = rand::thread_rng();
    let key = rsa::RsaPrivateKey::new(&mut rng, 2048).unwrap();
    let pem = key.to_pkcs1_pem(rsa::pkcs1::LineEnding::LF).unwrap();
    sqlx::query("INSERT INTO private_keys (id, uuid, name, private_key, team_id) VALUES (2, 'gh', 'gh', $1, 1)")
        .bind(enc.encrypt_string(pem.as_str()))
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO github_apps (id, uuid, name, api_url, html_url, team_id, is_public, app_id, installation_id, private_key_id) \
         VALUES (1, 'gh', 'gh', $1, 'http://github.test', 1, false, 1, 5, 2)",
    )
    .bind(format!("http://127.0.0.1:{port}"))
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::raw_sql(
        "UPDATE applications SET source_type = 'App\\Models\\GithubApp', source_id = 1, git_repository = 'o/r' WHERE id = 1; \
         INSERT INTO application_previews (id, uuid, pull_request_id, pull_request_html_url, application_id, fqdn) \
           VALUES (1, 'pv', 7, 'http://github.test/o/r/pull/7', 1, 'https://pr.test');",
    )
    .execute(db.pool())
    .await
    .unwrap();
    let app = coolify_db::applications::find_by_id(db, 1)
        .await
        .unwrap()
        .unwrap();
    let preview = coolify_db::application_ops::preview(db, 1, 7)
        .await
        .unwrap()
        .unwrap();
    jobs.update_pull_request(&app, &preview, PullRequestStatus::Finished, Some("dep-1"))
        .await;
    let bodies = github.bodies();
    let comment = bodies
        .iter()
        .find(|(p, _)| p == "/repos/o/r/issues/7/comments")
        .unwrap_or_else(|| panic!("{bodies:?}"));
    let text = comment.1["body"].as_str().unwrap();
    assert!(text.starts_with("The preview deployment for **my-app** is ready. 🟢\n\n[Open Preview](https://pr.test) | [Open Build Logs]("), "{text}");
    assert!(
        text.contains("/project/p-uuid/environment/e-uuid/application/app-uuid/deployment/dep-1)"),
        "{text}"
    );
    assert_eq!(
        count(
            db,
            "SELECT count(*) FROM application_previews WHERE pull_request_issue_comment_id = '99'"
        )
        .await,
        1
    );
}
