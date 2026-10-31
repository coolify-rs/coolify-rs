//! Delivery against a local HTTP endpoint on a test database: channel and event switches, failure
//! tracking, throttles and the data lookups of the triggers.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use coolify_compat::Encrypter;
use coolify_db::Db;
use coolify_db::notification_settings as ns;
use coolify_db::servers::ColumnValue;
use coolify_db::testing::TestDb;
use coolify_jobs::notifications::{self, Notification, ServerRef, lookup};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const KEY: &str = "base64:AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=";

const SEED: &str = r#"
INSERT INTO instance_settings (id, public_ipv4) VALUES (0, '203.0.113.5');
INSERT INTO teams (id, name, personal_team) VALUES (0, 'root', false), (1, 'acme', false);
INSERT INTO users (id, name, email) VALUES (1, 'a', 'owner@acme.test'), (2, 'b', 'member@acme.test');
INSERT INTO team_user (team_id, user_id, role) VALUES (1, 1, 'owner'), (1, 2, 'member');
INSERT INTO private_keys (id, uuid, name, private_key, team_id) VALUES (1, 'k', 'k', 'x', 1);
INSERT INTO servers (id, uuid, name, ip, team_id, private_key_id, unreachable_count)
  VALUES (1, 'srv-uuid', 'web-1', '10.0.0.1', 1, 1, 2);
INSERT INTO server_settings (server_id, is_reachable) VALUES (1, false);
INSERT INTO projects (id, uuid, name, team_id) VALUES (1, 'p-uuid', 'Proj', 1);
INSERT INTO environments (id, uuid, name, project_id) VALUES (1, 'e-uuid', 'production', 1);
INSERT INTO applications (id, uuid, name, environment_id, git_branch, git_repository, build_pack, fqdn)
  VALUES (1, 'app-uuid', 'my-app', 1, 'main', 'https://github.com/o/r.git', 'nixpacks', 'https://a.test,https://b.test');
INSERT INTO application_deployment_queues (id, deployment_uuid, application_id, commit_message)
  VALUES (1, 'dep-uuid', '1', 'fix: thing');
"#;

/// Answers with the queued statuses (then 200) and records what it got.
#[derive(Clone, Default)]
struct Endpoint {
    statuses: Arc<Mutex<VecDeque<u16>>>,
    bodies: Arc<Mutex<Vec<(String, Value)>>>,
}

impl Endpoint {
    fn bodies(&self) -> Vec<(String, Value)> {
        self.bodies.lock().unwrap().clone()
    }

    fn clear(&self) {
        self.bodies.lock().unwrap().clear();
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
                    let body: Value = serde_json::from_slice(&buf[head_end..head_end + length])
                        .unwrap_or(Value::Null);
                    me.bodies.lock().unwrap().push((path.clone(), body));
                    let status = if path.contains("/fail/404") {
                        404
                    } else {
                        me.statuses.lock().unwrap().pop_front().unwrap_or(200)
                    };
                    let reply = format!(
                        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}"
                    );
                    let _ = stream.write_all(reply.as_bytes()).await;
                });
            }
        });
        port
    }
}

async fn setup() -> (TestDb, Encrypter, Endpoint, u16) {
    let t = TestDb::new(SEED).await;
    let enc = Encrypter::new(KEY).unwrap();
    let endpoint = Endpoint::default();
    let port = endpoint.start().await;
    // `SafeWebhookUrl`: the local endpoint is allow-listed.
    sqlx::query(
        "UPDATE instance_settings SET webhook_allow_localhost = true, webhook_allowed_internal_hosts = '[\"127.0.0.1\"]'",
    )
    .execute(t.db.pool())
    .await
    .unwrap();
    for ch in ns::ALL {
        ns::first_or_create(&t.db, ch, 1).await.unwrap();
    }
    (t, enc, endpoint, port)
}

async fn enable_discord(db: &Db, enc: &Encrypter, port: u16) {
    let url = enc.encrypt_string(&format!("http://127.0.0.1:{port}/discord/secret"));
    sqlx::query("UPDATE discord_notification_settings SET discord_enabled = true, discord_webhook_url = $1 WHERE team_id = 1")
        .bind(url)
        .execute(db.pool())
        .await
        .unwrap();
}

async fn enable_webhook(db: &Db, enc: &Encrypter, port: u16) {
    let url = enc.encrypt_string(&format!("http://127.0.0.1:{port}/webhook/secret"));
    sqlx::query("UPDATE webhook_notification_settings SET webhook_enabled = true, webhook_url = $1 WHERE team_id = 1")
        .bind(url)
        .execute(db.pool())
        .await
        .unwrap();
}

fn server() -> ServerRef {
    ServerRef {
        id: 1,
        name: "web-1".into(),
        uuid: "srv-uuid".into(),
    }
}

fn cleanup_success() -> Notification {
    Notification::DockerCleanupSuccess {
        server: server(),
        message: "done".into(),
    }
}

#[tokio::test]
async fn delivery_tracking_and_throttles() {
    let (t, enc, endpoint, port) = setup().await;
    let db = &t.db;
    enable_discord(db, &enc, port).await;

    // The event is off by default for docker_cleanup_success: nothing goes out.
    sqlx::query("UPDATE discord_notification_settings SET docker_cleanup_success_discord_notifications = false WHERE team_id = 1")
        .execute(db.pool())
        .await
        .unwrap();
    notifications::deliver(db, Some(&enc), 1, cleanup_success()).await;
    assert!(endpoint.bodies().is_empty());

    // On: one request, `content` only with ping enabled and a critical message.
    sqlx::query("UPDATE discord_notification_settings SET docker_cleanup_success_discord_notifications = true, discord_ping_enabled = false WHERE team_id = 1")
        .execute(db.pool())
        .await
        .unwrap();
    notifications::deliver(db, Some(&enc), 1, cleanup_success()).await;
    let bodies = endpoint.bodies();
    assert_eq!(bodies.len(), 1);
    assert_eq!(bodies[0].0, "/discord/secret");
    assert_eq!(
        bodies[0].1["embeds"][0]["title"],
        json!(":white_check_mark: Coolify: Docker cleanup job succeeded on web-1")
    );
    endpoint.clear();

    // `general` is always sent. A success resets the count, a 404 raises it, the third disables.
    sqlx::query("UPDATE discord_notification_settings SET failure_count = 2 WHERE team_id = 1")
        .execute(db.pool())
        .await
        .unwrap();
    let general = Notification::General {
        message: "m".into(),
        success: true,
    };
    notifications::deliver(db, Some(&enc), 1, general.clone()).await;
    let count = |db: Db| async move {
        sqlx::query_as::<_, (i32, bool)>(
            "SELECT failure_count, discord_enabled FROM discord_notification_settings WHERE team_id = 1",
        )
        .fetch_one(db.pool())
        .await
        .unwrap()
    };
    assert_eq!(count(db.clone()).await, (0, true));

    // Discord answers 404 from here on; the webhook keeps working.
    enable_webhook(db, &enc, port).await;
    sqlx::query(
        "UPDATE discord_notification_settings SET discord_webhook_url = $1 WHERE team_id = 1",
    )
    .bind(enc.encrypt_string(&format!("http://127.0.0.1:{port}/fail/404")))
    .execute(db.pool())
    .await
    .unwrap();
    notifications::deliver(db, Some(&enc), 1, general.clone()).await;
    notifications::deliver(db, Some(&enc), 1, general.clone()).await;
    assert_eq!(count(db.clone()).await, (2, true));
    endpoint.clear();
    notifications::deliver(db, Some(&enc), 1, general.clone()).await;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    // `notifyOtherChannels`: the webhook hears about it.
    let told = endpoint.bodies();
    assert!(told.iter().any(|(p, b)| p == "/webhook/secret"
        && b["success"] == json!(false)
        && b["message"].as_str().unwrap().starts_with(
            "Discord notifications were disabled after 3 failed deliveries in a row."
        )));
    sqlx::query(
        "UPDATE webhook_notification_settings SET webhook_enabled = false WHERE team_id = 1",
    )
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "UPDATE discord_notification_settings SET discord_webhook_url = $1 WHERE team_id = 1",
    )
    .bind(enc.encrypt_string(&format!("http://127.0.0.1:{port}/discord/secret")))
    .execute(db.pool())
    .await
    .unwrap();
    assert_eq!(count(db.clone()).await, (0, false));
    let audit: Vec<(String, String, Value)> =
        sqlx::query_as("SELECT event, level, metadata FROM audit_events WHERE team_id = 1")
            .fetch_all(db.pool())
            .await
            .unwrap();
    assert_eq!(audit.len(), 1);
    assert_eq!(audit[0].0, "notifications.discord.auto_disabled");
    assert_eq!(audit[0].1, "warning");
    assert_eq!(
        audit[0].2["reason"],
        json!("Discord returned HTTP 404 (Not Found).")
    );

    // A settings change sets the count back to zero.
    sqlx::query("UPDATE discord_notification_settings SET failure_count = 2 WHERE team_id = 1")
        .execute(db.pool())
        .await
        .unwrap();
    ns::update(
        db,
        &ns::DISCORD,
        1,
        &[("discord_enabled", ColumnValue::Bool(true))],
    )
    .await
    .unwrap();
    assert_eq!(count(db.clone()).await, (0, true));
    endpoint.clear();
    endpoint.statuses.lock().unwrap().clear();

    // `HighDiskUsage` is throttled until released.
    sqlx::query("UPDATE discord_notification_settings SET server_disk_usage_discord_notifications = true WHERE team_id = 1")
        .execute(db.pool())
        .await
        .unwrap();
    let disk = Notification::HighDiskUsage {
        server: server(),
        disk_usage: 90,
        threshold: 80,
        interval_hours: 24,
    };
    notifications::deliver(db, Some(&enc), 1, disk.clone()).await;
    notifications::deliver(db, Some(&enc), 1, disk.clone()).await;
    assert_eq!(endpoint.bodies().len(), 1);
    notifications::release_high_disk_usage(db, 1).await;
    notifications::deliver(db, Some(&enc), 1, disk).await;
    assert_eq!(endpoint.bodies().len(), 2);
    endpoint.clear();

    // `isReachableChanged`: unreachable once, then reachable once.
    sqlx::query("UPDATE discord_notification_settings SET server_unreachable_discord_notifications = true, server_reachable_discord_notifications = true WHERE team_id = 1")
        .execute(db.pool())
        .await
        .unwrap();
    lookup::server_reachability_changed(db, Some(&enc), 1, 2).await;
    lookup::server_reachability_changed(db, Some(&enc), 1, 2).await;
    assert!(lookup::unreachable_notification_sent(db, 1).await);
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let bodies = endpoint.bodies();
    assert_eq!(bodies.len(), 1);
    assert_eq!(
        bodies[0].1["embeds"][0]["title"],
        json!(":cross_mark: Server unreachable")
    );
    endpoint.clear();
    sqlx::query("UPDATE server_settings SET is_reachable = true WHERE server_id = 1")
        .execute(db.pool())
        .await
        .unwrap();
    lookup::server_reachability_changed(db, Some(&enc), 1, 2).await;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let bodies = endpoint.bodies();
    assert_eq!(bodies.len(), 1);
    assert_eq!(
        bodies[0].1["embeds"][0]["title"],
        json!(":white_check_mark: Server 'web-1' revived")
    );
    assert!(!lookup::unreachable_notification_sent(db, 1).await);
    endpoint.clear();

    // `sendDeploymentNotification`: the rows give the links and the commit details.
    enable_webhook(db, &enc, port).await;
    sqlx::query("UPDATE webhook_notification_settings SET deployment_success_webhook_notifications = true WHERE team_id = 1")
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE discord_notification_settings SET deployment_success_discord_notifications = false WHERE team_id = 1")
        .execute(db.pool())
        .await
        .unwrap();
    lookup::notify_deployment(db, Some(&enc), 1, 1, "abcdef1234", true).await;
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let bodies = endpoint.bodies();
    assert_eq!(bodies.len(), 1);
    assert_eq!(
        bodies[0].1,
        json!({
            "success": true,
            "message": "New version successfully deployed",
            "event": "deployment_success",
            "application_name": "my-app",
            "application_uuid": "app-uuid",
            "deployment_uuid": "dep-uuid",
            "deployment_url": "http://203.0.113.5:8000/project/p-uuid/environment/e-uuid/application/app-uuid/deployment/dep-uuid",
            "project": "Proj",
            "environment": "production",
            "fqdn": "https://a.test",
        })
    );

    // The other lookups read the same environment.
    let backup = lookup::database_backup(db, "pg", "db-uuid", Some(1), Some("app"), "daily").await;
    assert_eq!(backup.project_uuid.as_deref(), Some("p-uuid"));
    assert_eq!(backup.environment_uuid.as_deref(), Some("e-uuid"));
    let task = lookup::service_task(db, "t", "t-uuid", "out", "svc", Some(1)).await;
    assert_eq!(
        task.resource.and_then(|r| r.project_uuid).as_deref(),
        Some("p-uuid")
    );
    let (team, restart) =
        lookup::service_part_restart_limit(db, 1, "part", "part-uuid", "svc-uuid", None, 5, 5)
            .await
            .unwrap();
    assert_eq!(team, 1);
    assert_eq!(restart.event(), "restart_limit_reached");

    // A blocked webhook URL is not retried and does not count.
    let blocked = enc.encrypt_string("http://169.254.169.254/latest");
    sqlx::query("UPDATE webhook_notification_settings SET webhook_url = $1, failure_count = 0 WHERE team_id = 1")
        .bind(blocked)
        .execute(db.pool())
        .await
        .unwrap();
    endpoint.clear();
    notifications::deliver(db, Some(&enc), 1, general).await;
    let fails: i32 = sqlx::query_scalar(
        "SELECT failure_count FROM webhook_notification_settings WHERE team_id = 1",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(fails, 0);
    assert!(endpoint.bodies().iter().all(|(p, _)| !p.contains("latest")));

    // The test button reports a rejected request.
    let bad = enc.encrypt_string(&format!("http://127.0.0.1:{port}/discord/secret"));
    sqlx::query(
        "UPDATE discord_notification_settings SET discord_webhook_url = $1 WHERE team_id = 1",
    )
    .bind(bad)
    .execute(db.pool())
    .await
    .unwrap();
    endpoint.statuses.lock().unwrap().extend([404u16]);
    let r = notifications::notify_test(db, Some(&enc), 1, "discord", None).await;
    assert_eq!(r.unwrap_err(), "Discord returned HTTP 404 (Not Found).");
    assert!(
        notifications::notify_test(db, Some(&enc), 1, "discord", None)
            .await
            .is_ok()
    );

    t.drop_db().await;
}

/// Retries with the real 10 second backoff: `cargo test -- --ignored server_errors_are_retried`.
#[tokio::test]
#[ignore = "takes 40 seconds"]
async fn server_errors_are_retried() {
    let (t, enc, endpoint, port) = setup().await;
    let db = &t.db;
    enable_discord(db, &enc, port).await;
    endpoint.statuses.lock().unwrap().extend([500u16; 9]);
    let general = Notification::General {
        message: "m".into(),
        success: true,
    };
    notifications::deliver(db, Some(&enc), 1, general).await;
    // `$tries = 5`, and a server error never counts as a delivery failure.
    assert_eq!(endpoint.bodies().len(), 5);
    let fails: i32 = sqlx::query_scalar(
        "SELECT failure_count FROM discord_notification_settings WHERE team_id = 1",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(fails, 0);
    t.drop_db().await;
}
