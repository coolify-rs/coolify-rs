//! The scheduler against a real database: dedupe, skips, recovery and the scheduled tasks source.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, TimeZone, Utc};
use coolify_db::Db;
use coolify_db::testing::TestDb;
use coolify_jobs::Jobs;
use coolify_jobs::scheduled_task::ScheduledTaskSource;
use coolify_jobs::scheduler::{BoxFuture, Due, Occurrence, Overlap, ScheduleSource, Scheduler};
use serde_json::json;

const SEED: &str = r#"
INSERT INTO instance_settings (id) VALUES (0);
INSERT INTO teams (id, name) VALUES (1, 'team');
INSERT INTO private_keys (id, uuid, team_id, name, private_key) VALUES (1, 'key', 1, 'k', 'x');
INSERT INTO servers (id, uuid, name, ip, team_id, private_key_id) VALUES
  (1, 'good', 'good', '10.0.0.1', 1, 1),
  (2, 'broken', 'broken', '10.0.0.2', 1, 1),
  (3, 'karachi', 'karachi', '10.0.0.3', 1, 1);
INSERT INTO server_settings (server_id, is_reachable, is_usable, force_disabled, server_timezone) VALUES
  (1, true, true, false, 'UTC'),
  (2, false, true, false, 'UTC'),
  (3, true, true, false, 'Asia/Karachi');
INSERT INTO standalone_dockers (id, uuid, name, network, server_id) VALUES
  (1, 'd1', 'd', 'coolify', 1), (2, 'd2', 'd', 'coolify', 2), (3, 'd3', 'd', 'coolify', 3);
INSERT INTO projects (id, uuid, name, team_id) VALUES (1, 'p', 'p', 1);
INSERT INTO environments (id, uuid, name, project_id) VALUES (1, 'e', 'production', 1);
INSERT INTO applications (id, uuid, name, environment_id, build_pack, git_branch, git_repository, status, destination_type, destination_id) VALUES
  (1, 'running', 'a', 1, 'dockerimage', 'main', 'r', 'running:healthy', 'App\Models\StandaloneDocker', 1),
  (2, 'stopped', 'b', 1, 'dockerimage', 'main', 'r', 'exited', 'App\Models\StandaloneDocker', 1),
  (3, 'on-broken', 'c', 1, 'dockerimage', 'main', 'r', 'running:healthy', 'App\Models\StandaloneDocker', 2),
  (4, 'on-karachi', 'd', 1, 'dockerimage', 'main', 'r', 'running:healthy', 'App\Models\StandaloneDocker', 3),
  (5, 'gone-server', 'e', 1, 'dockerimage', 'main', 'r', 'running:healthy', 'App\Models\StandaloneDocker', 99);
"#;

fn at(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(y, mo, d, h, mi, s).unwrap()
}

fn jobs_for(db: &Db, dir: &tempfile::TempDir) -> Jobs {
    let remote =
        coolify_remote::Remote::new(coolify_remote::RemoteConfig::new(dir.path())).unwrap();
    Jobs::new(db.clone(), remote, None)
}

async fn task(db: &Db, id: i64, application_id: Option<i64>, frequency: &str, enabled: bool) {
    sqlx::query(
        "INSERT INTO scheduled_tasks (id, uuid, name, command, frequency, enabled, team_id, application_id, created_at, updated_at) \
         VALUES ($1, $2, 't', 'echo x', $3, $4, 1, $5, now(), now())",
    )
    .bind(id)
    .bind(format!("task-{id}"))
    .bind(frequency)
    .bind(enabled)
    .bind(application_id)
    .execute(db.pool())
    .await
    .unwrap();
}

async fn scalar(db: &Db, query: &str) -> i64 {
    sqlx::query_scalar(query)
        .fetch_one(db.pool())
        .await
        .unwrap()
}

/// Polls until `check` holds, up to five seconds.
async fn eventually(db: &Db, query: &str, expected: i64) -> i64 {
    let mut got = -1;
    for _ in 0..100 {
        got = scalar(db, query).await;
        if got == expected {
            return got;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    got
}

/// A source with fixed schedules that records what ran.
#[derive(Clone)]
struct Recording {
    due: Vec<Due>,
    runs: Arc<Mutex<Vec<String>>>,
    fails: bool,
    late: bool,
    gone: bool,
    hold: Duration,
    missed: Arc<Mutex<Vec<String>>>,
}

impl Recording {
    fn new(frequency: &str, timezone: &str, skip: Option<&str>) -> Self {
        Self {
            due: vec![Due {
                schedule_key: "fake:1".to_owned(),
                frequency: frequency.to_owned(),
                timezone: timezone.to_owned(),
                job_type: "fake-job",
                resource_id: 1,
                payload: json!({"keep": true}),
                skip: skip.map(str::to_owned),
            }],
            runs: Arc::default(),
            fails: false,
            late: false,
            gone: false,
            hold: Duration::ZERO,
            missed: Arc::default(),
        }
    }

    fn runs(&self) -> Vec<String> {
        self.runs.lock().unwrap().clone()
    }
}

impl ScheduleSource for Recording {
    fn name(&self) -> &'static str {
        "fake"
    }
    fn job_types(&self) -> &'static [&'static str] {
        &["fake-job"]
    }
    fn due<'a>(&'a self, _: &'a Jobs, _: DateTime<Utc>) -> BoxFuture<'a, Result<Vec<Due>, String>> {
        Box::pin(async { Ok(self.due.clone()) })
    }
    fn overlap<'a>(&'a self, _: &'a Jobs, _: i64) -> BoxFuture<'a, Option<Overlap>> {
        Box::pin(async {
            (!self.gone).then(|| Overlap {
                key: "fake-1".to_owned(),
                ttl: Duration::from_secs(300),
            })
        })
    }
    fn run<'a>(&'a self, _: &'a Jobs, o: &'a Occurrence) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            tokio::time::sleep(self.hold).await;
            self.runs.lock().unwrap().push(format!(
                "{} {}",
                o.scheduled_for.format("%H:%M"),
                o.payload
            ));
            if self.fails {
                Err("no".to_owned())
            } else {
                Ok(())
            }
        })
    }
    fn runs_late(&self) -> bool {
        self.late
    }
    fn report_missed<'a>(
        &'a self,
        _: &'a Jobs,
        o: &'a Occurrence,
        m: &'a str,
    ) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            self.missed
                .lock()
                .unwrap()
                .push(format!("{}: {m}", o.schedule_key));
        })
    }
}

fn scheduler(jobs: &Jobs, source: &Recording) -> Scheduler {
    let mut s = Scheduler::new(jobs.clone());
    s.register(source.clone());
    s
}

const STATES: &str = "SELECT count(*) FROM scheduled_job_states WHERE schedule_key = 'fake:1'";
const DELIVERIES: &str =
    "SELECT count(*) FROM scheduled_job_deliveries WHERE schedule_key = 'fake:1'";

#[tokio::test]
async fn an_occurrence_runs_once() {
    let tdb = TestDb::new(SEED).await;
    let dir = tempfile::tempdir().unwrap();
    let jobs = jobs_for(&tdb.db, &dir);
    let source = Recording::new("*/5 * * * *", "UTC", None);
    let s = scheduler(&jobs, &source);

    // Not due: 12:03 is inside the catch-up window of 12:00, so a first tick runs that occurrence.
    let first = s.tick(at(2026, 10, 1, 12, 3, 0)).await;
    assert_eq!(first.dispatched, 1);
    assert_eq!(
        eventually(&tdb.db, DELIVERIES, 0).await,
        0,
        "completed rows are deleted"
    );
    assert_eq!(source.runs(), vec![r#"12:00 {"keep":true}"#]);

    // The same occurrence again, and a late tick of it.
    assert_eq!(s.tick(at(2026, 10, 1, 12, 4, 59)).await.dispatched, 0);
    // The next occurrence.
    assert_eq!(s.tick(at(2026, 10, 1, 12, 5, 1)).await.dispatched, 1);
    assert_eq!(eventually(&tdb.db, DELIVERIES, 0).await, 0);
    assert_eq!(source.runs().len(), 2);
    assert_eq!(scalar(&tdb.db, STATES).await, 1);
    tdb.drop_db().await;
}

#[tokio::test]
async fn two_schedulers_share_the_dedupe() {
    let tdb = TestDb::new(SEED).await;
    let dir = tempfile::tempdir().unwrap();
    let (j1, j2) = (jobs_for(&tdb.db, &dir), jobs_for(&tdb.db, &dir));
    let source = Recording::new("* * * * *", "UTC", None);
    let (s1, s2) = (scheduler(&j1, &source), scheduler(&j2, &source));
    let now = at(2026, 10, 1, 12, 0, 1);
    let (a, b) = tokio::join!(s1.tick(now), s2.tick(now));
    assert_eq!(a.dispatched + b.dispatched, 1);
    assert_eq!(eventually(&tdb.db, DELIVERIES, 0).await, 0);
    assert_eq!(source.runs().len(), 1);
    tdb.drop_db().await;
}

#[tokio::test]
async fn outside_the_catch_up_window_nothing_is_recorded() {
    let tdb = TestDb::new(SEED).await;
    let dir = tempfile::tempdir().unwrap();
    let jobs = jobs_for(&tdb.db, &dir);
    let source = Recording::new("0 12 * * *", "UTC", None);
    let s = scheduler(&jobs, &source);
    assert_eq!(s.tick(at(2026, 10, 1, 12, 10, 1)).await.dispatched, 0);
    assert_eq!(scalar(&tdb.db, STATES).await, 0);
    // In the server's time zone: 12:00 in Karachi is 07:00 UTC.
    let source = Recording::new("0 12 * * *", "Asia/Karachi", None);
    let s = scheduler(&jobs, &source);
    assert_eq!(s.tick(at(2026, 10, 1, 7, 0, 5)).await.dispatched, 1);
    assert_eq!(eventually(&tdb.db, DELIVERIES, 0).await, 0);
    tdb.drop_db().await;
}

#[tokio::test]
async fn a_skip_moves_the_state_without_a_delivery() {
    let tdb = TestDb::new(SEED).await;
    let dir = tempfile::tempdir().unwrap();
    let jobs = jobs_for(&tdb.db, &dir);
    let source = Recording::new("* * * * *", "UTC", Some("server_not_functional"));
    let s = scheduler(&jobs, &source);
    let now = at(2026, 10, 1, 12, 0, 1);
    let summary = s.tick(now).await;
    assert_eq!((summary.dispatched, summary.skipped), (0, 1));
    assert_eq!(s.tick(now).await.skipped, 0, "one skip per occurrence");
    assert_eq!(scalar(&tdb.db, STATES).await, 1);
    assert_eq!(scalar(&tdb.db, DELIVERIES).await, 0);
    assert!(source.runs().is_empty());
    // The schedule's next run after it recovers does not repeat the skipped occurrence.
    let healthy = Recording::new("* * * * *", "UTC", None);
    let s = scheduler(&jobs, &healthy);
    assert_eq!(s.tick(now).await.dispatched, 0);
    tdb.drop_db().await;
}

#[tokio::test]
async fn a_failed_job_keeps_its_row_and_a_gone_resource_is_skipped() {
    let tdb = TestDb::new(SEED).await;
    let dir = tempfile::tempdir().unwrap();
    let jobs = jobs_for(&tdb.db, &dir);
    let mut source = Recording::new("* * * * *", "UTC", None);
    source.fails = true;
    let s = scheduler(&jobs, &source);
    s.tick(at(2026, 10, 1, 12, 0, 1)).await;
    let failed = "SELECT count(*) FROM scheduled_job_deliveries WHERE schedule_key = 'fake:1' AND status = 'failed'";
    assert_eq!(eventually(&tdb.db, failed, 1).await, 1);

    let mut gone = Recording::new("* * * * *", "UTC", None);
    gone.gone = true;
    let s = scheduler(&jobs, &gone);
    assert_eq!(s.tick(at(2026, 10, 1, 12, 1, 1)).await.dispatched, 0);
    let skipped = "SELECT count(*) FROM scheduled_job_deliveries WHERE schedule_key = 'fake:1' AND status = 'skipped'";
    assert_eq!(scalar(&tdb.db, skipped).await, 1);
    tdb.drop_db().await;
}

#[tokio::test]
async fn a_job_that_finds_the_lock_held_is_skipped() {
    let tdb = TestDb::new(SEED).await;
    let dir = tempfile::tempdir().unwrap();
    let jobs = jobs_for(&tdb.db, &dir);
    let mut source = Recording::new("* * * * *", "UTC", None);
    source.hold = Duration::from_millis(600);
    let s = scheduler(&jobs, &source);
    assert_eq!(s.tick(at(2026, 10, 1, 12, 0, 1)).await.dispatched, 1);
    let claimed = "SELECT count(*) FROM scheduled_job_deliveries WHERE status = 'claimed'";
    assert_eq!(eventually(&tdb.db, claimed, 1).await, 1);
    assert_eq!(s.tick(at(2026, 10, 1, 12, 1, 1)).await.dispatched, 1);
    let skipped = "SELECT count(*) FROM scheduled_job_deliveries WHERE status = 'skipped'";
    assert_eq!(eventually(&tdb.db, skipped, 1).await, 1);
    assert_eq!(
        eventually(&tdb.db, DELIVERIES, 1).await,
        1,
        "only the skipped row stays"
    );
    assert_eq!(source.runs().len(), 1);
    tdb.drop_db().await;
}

/// An occurrence row in a given state, as a crashed scheduler leaves it.
async fn stuck(db: &Db, uuid: &str, status: &str, minutes_ago: i64) {
    sqlx::query(
        "INSERT INTO scheduled_job_deliveries (uuid, schedule_key, scheduled_for, job_type, resource_id, status, enqueued_at, started_at, created_at, updated_at) \
         VALUES ($1, 'fake:1', $3 - make_interval(mins => $4), 'fake-job', 1, $2, \
                 $3 - make_interval(mins => $4), $3 - make_interval(mins => $4), ($3 - make_interval(mins => $4))::timestamp, now())",
    )
    .bind(uuid)
    .bind(status)
    .bind(at(2026, 10, 1, 12, 0, 1))
    .bind(minutes_ago as i32)
    .execute(db.pool())
    .await
    .unwrap();
}

async fn status_of(db: &Db, uuid: &str) -> String {
    sqlx::query_scalar("SELECT status FROM scheduled_job_deliveries WHERE uuid = $1")
        .bind(uuid)
        .fetch_one(db.pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn recovery_of_lost_occurrences() {
    let tdb = TestDb::new(SEED).await;
    let dir = tempfile::tempdir().unwrap();
    let jobs = jobs_for(&tdb.db, &dir);
    stuck(&tdb.db, "old-enqueued", "enqueued", 61).await;
    stuck(&tdb.db, "fresh-enqueued", "enqueued", 59).await;
    stuck(&tdb.db, "old-claim", "claimed", 700).await;
    stuck(&tdb.db, "fresh-claim", "claimed", 600).await;
    let mut source = Recording::new("0 0 1 1 *", "UTC", None);
    source.due.clear();
    let s = scheduler(&jobs, &source);
    s.tick(at(2026, 10, 1, 12, 0, 1)).await;
    assert_eq!(status_of(&tdb.db, "old-enqueued").await, "failed");
    assert_eq!(status_of(&tdb.db, "fresh-enqueued").await, "enqueued");
    assert_eq!(status_of(&tdb.db, "old-claim").await, "failed");
    assert_eq!(status_of(&tdb.db, "fresh-claim").await, "claimed");
    assert_eq!(
        source.missed.lock().unwrap().clone(),
        vec!["fake:1: Skipped: the queued job did not start within 60 minutes."]
    );
    tdb.drop_db().await;
}

#[tokio::test]
async fn a_pending_occurrence_is_published_again_and_a_late_backup_runs() {
    let tdb = TestDb::new(SEED).await;
    let dir = tempfile::tempdir().unwrap();
    let jobs = jobs_for(&tdb.db, &dir);
    stuck(&tdb.db, "pending", "pending", 1).await;
    let mut source = Recording::new("0 0 1 1 *", "UTC", None);
    source.due.clear();
    source.late = true;
    let s = scheduler(&jobs, &source);
    s.tick(at(2026, 10, 1, 12, 0, 1)).await;
    assert_eq!(eventually(&tdb.db, DELIVERIES, 0).await, 0);
    assert_eq!(source.runs().len(), 1);

    stuck(&tdb.db, "lost", "enqueued", 61).await;
    s.tick(at(2026, 10, 1, 12, 0, 1)).await;
    assert_eq!(eventually(&tdb.db, DELIVERIES, 0).await, 0);
    assert_eq!(source.runs().len(), 2, "a backup runs once when late");
    assert!(source.missed.lock().unwrap().is_empty());
    tdb.drop_db().await;
}

#[tokio::test]
async fn the_task_source_deletes_orphans_and_skips() {
    let tdb = TestDb::new(SEED).await;
    let dir = tempfile::tempdir().unwrap();
    let jobs = jobs_for(&tdb.db, &dir);
    task(&tdb.db, 1, Some(2), "* * * * *", true).await; // application not running
    task(&tdb.db, 2, Some(3), "* * * * *", true).await; // server not functional
    task(&tdb.db, 3, Some(5), "0 0 1 1 *", true).await; // the server is gone: deleted when not due too
    task(&tdb.db, 4, Some(99), "0 0 1 1 *", true).await; // the application is gone: deleted
    task(&tdb.db, 5, None, "0 0 1 1 *", true).await; // no resource: deleted
    task(&tdb.db, 6, Some(99), "0 0 1 1 *", false).await; // disabled: not looked at
    task(&tdb.db, 7, Some(1), "* * * * *", false).await; // disabled: does not run
    task(&tdb.db, 8, Some(1), "0 0 1 1 *", true).await; // not due

    let mut s = Scheduler::new(jobs.clone());
    s.register(ScheduledTaskSource);
    let summary = s.tick(at(2026, 10, 1, 12, 0, 1)).await;
    assert_eq!((summary.dispatched, summary.skipped), (0, 2));
    let left: Vec<i64> = sqlx::query_scalar("SELECT id FROM scheduled_tasks ORDER BY id")
        .fetch_all(tdb.db.pool())
        .await
        .unwrap();
    assert_eq!(left, vec![1, 2, 6, 7, 8]);
    let keys: Vec<String> =
        sqlx::query_scalar("SELECT schedule_key FROM scheduled_job_states ORDER BY schedule_key")
            .fetch_all(tdb.db.pool())
            .await
            .unwrap();
    assert_eq!(keys, vec!["scheduled-task:1", "scheduled-task:2"]);
    assert_eq!(
        scalar(&tdb.db, "SELECT count(*) FROM scheduled_job_deliveries").await,
        0
    );
    assert_eq!(
        scalar(&tdb.db, "SELECT count(*) FROM scheduled_task_executions").await,
        0
    );
    tdb.drop_db().await;
}

#[tokio::test]
async fn the_task_source_uses_the_server_time_zone_and_records_a_failure() {
    let tdb = TestDb::new(SEED).await;
    let dir = tempfile::tempdir().unwrap();
    let jobs = jobs_for(&tdb.db, &dir);
    // 17:00 in Karachi is 12:00 UTC; 12:00 UTC on the UTC server is not 17:00.
    task(&tdb.db, 1, Some(4), "0 17 * * *", true).await;
    task(&tdb.db, 2, Some(1), "0 17 * * *", true).await;
    // A name of VALID_CRON_STRINGS runs like its expression.
    task(&tdb.db, 3, Some(1), "every_minute", true).await;
    let mut s = Scheduler::new(jobs.clone());
    s.register(ScheduledTaskSource);
    let summary = s.tick(at(2026, 10, 1, 12, 0, 1)).await;
    assert_eq!(summary.dispatched, 2);
    let finished = "SELECT count(*) FROM scheduled_task_executions WHERE finished_at IS NOT NULL";
    assert_eq!(eventually(&tdb.db, finished, 2).await, 2);
    let ids: Vec<i64> =
        sqlx::query_scalar("SELECT scheduled_task_id FROM scheduled_task_executions ORDER BY 1")
            .fetch_all(tdb.db.pool())
            .await
            .unwrap();
    assert_eq!(ids, vec![1, 3]);
    // No application key in this test, so the server's key cannot be decrypted.
    let row: (String, String, i32) = sqlx::query_as(
        "SELECT status, message, retry_count FROM scheduled_task_executions WHERE scheduled_task_id = 3",
    )
    .fetch_one(tdb.db.pool())
    .await
    .unwrap();
    assert_eq!(row.0, "failed");
    assert_eq!(row.1, "No application encryption key has been specified.");
    assert_eq!(row.2, 0);
    let failed = "SELECT count(*) FROM scheduled_job_deliveries WHERE status = 'failed'";
    assert_eq!(eventually(&tdb.db, failed, 2).await, 2);
    tdb.drop_db().await;
}

#[tokio::test]
async fn startup_fails_runs_a_stopped_process_left_behind() {
    let tdb = TestDb::new(SEED).await;
    let dir = tempfile::tempdir().unwrap();
    let jobs = jobs_for(&tdb.db, &dir);
    task(&tdb.db, 1, Some(1), "0 0 1 1 *", false).await;
    sqlx::query(
        "INSERT INTO scheduled_task_executions (uuid, scheduled_task_id, started_at, created_at, updated_at) VALUES ('r', 1, now(), now(), now())",
    )
    .execute(tdb.db.pool())
    .await
    .unwrap();
    jobs.fail_interrupted_scheduled_tasks().await;
    let (status, message): (String, String) =
        sqlx::query_as("SELECT status, message FROM scheduled_task_executions WHERE uuid = 'r'")
            .fetch_one(tdb.db.pool())
            .await
            .unwrap();
    assert_eq!(status, "failed");
    assert!(message.starts_with("Job permanently failed after 2 attempts: The worker stopped"));
    tdb.drop_db().await;
}
