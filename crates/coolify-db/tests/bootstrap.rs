//! Schema creation for a new installation. Every database here is created by the test itself on the
//! dev Postgres (`COOLIFY_RS_TEST_DATABASE_URL`) and dropped at the end.
#![cfg(feature = "testing")]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use coolify_db::bootstrap::{self, BootstrapError, Migration, Prepared};
use coolify_db::testing::TestDb;
use rand::Rng;
use sqlx::{Connection, Executor, PgConnection};

fn admin_url() -> String {
    std::env::var("COOLIFY_RS_TEST_DATABASE_URL").expect("COOLIFY_RS_TEST_DATABASE_URL")
}

fn url_of(name: &str) -> String {
    let admin = admin_url();
    let i = admin.rfind('/').expect("database url");
    format!("{}/{name}", &admin[..i])
}

struct Empty(String);

impl Empty {
    async fn new() -> Self {
        let name = format!("t_boot_{:016x}", rand::thread_rng().r#gen::<u64>());
        let mut admin = PgConnection::connect(&admin_url()).await.unwrap();
        admin
            .execute(format!("CREATE DATABASE {name}").as_str())
            .await
            .unwrap();
        Self(name)
    }

    fn url(&self) -> String {
        url_of(&self.0)
    }

    async fn drop_db(self) {
        let mut admin = PgConnection::connect(&admin_url()).await.unwrap();
        admin
            .execute(format!("DROP DATABASE IF EXISTS {} WITH (FORCE)", self.0).as_str())
            .await
            .unwrap();
    }

    async fn count(&self, sql: &str) -> i64 {
        let mut conn = PgConnection::connect(&self.url()).await.unwrap();
        sqlx::query_scalar(sql).fetch_one(&mut conn).await.unwrap()
    }
}

#[test]
fn check_constraints_are_written_as_upstream_does() {
    let sql = bootstrap::schema_sql();
    assert!(!sql.contains("::text = ANY ((ARRAY['"));
    assert!(sql.contains(r#"CHECK ("redirect" IN ('www', 'non-www', 'both'))"#));
}

#[tokio::test]
async fn empty_database_gets_the_baseline() {
    let db = Empty::new().await;
    assert_eq!(
        bootstrap::prepare(&db.url()).await.unwrap(),
        Prepared::Created
    );
    assert_eq!(db.count("SELECT count(*) FROM migrations").await, 478);
    assert_eq!(
        db.count(
            "SELECT count(*) FROM pg_constraint WHERE contype = 'c' AND conrelid <> 0 AND pg_get_constraintdef(oid) LIKE '%ANY ((ARRAY[%::text[]%'"
        )
        .await,
        10,
        "the check constraints deparse as upstream's do"
    );
    assert_eq!(
        db.count("SELECT last_value FROM migrations_id_seq").await,
        478
    );
    assert_eq!(
        db.count("SELECT count(*) FROM migrations WHERE batch <> 1")
            .await,
        0
    );
    // The schema guard accepts what was created.
    let guarded = coolify_db::Db::connect(&coolify_db::DbConfig {
        url: db.url(),
        max_connections: 2,
        acquire_timeout: std::time::Duration::from_secs(5),
        allow_unknown_schema: false,
    })
    .await
    .unwrap();
    assert!(guarded.schema().read_only_reason.is_none());
    assert!(guarded.schema().missing.is_empty());
    guarded.pool().close().await;
    // A second start leaves it alone.
    assert_eq!(
        bootstrap::prepare(&db.url()).await.unwrap(),
        Prepared::Existing
    );
    assert_eq!(db.count("SELECT count(*) FROM migrations").await, 478);
    db.drop_db().await;
}

#[tokio::test]
async fn a_database_with_any_table_is_refused_and_unchanged() {
    let db = Empty::new().await;
    let mut conn = PgConnection::connect(&db.url()).await.unwrap();
    conn.execute("CREATE TABLE precious (id int); INSERT INTO precious VALUES (7)")
        .await
        .unwrap();
    conn.close().await.unwrap();

    match bootstrap::create_schema(&db.url()).await {
        Err(BootstrapError::NotEmpty { relations: 1 }) => {}
        other => panic!("expected a refusal, got {other:?}"),
    }
    assert_eq!(
        bootstrap::prepare(&db.url()).await.unwrap(),
        Prepared::Existing
    );
    assert_eq!(db.count("SELECT count(*) FROM precious").await, 1);
    assert_eq!(
        db.count("SELECT count(*) FROM pg_tables WHERE schemaname = 'public'")
            .await,
        1,
        "nothing was created next to the existing table"
    );
    db.drop_db().await;
}

#[tokio::test]
async fn a_table_in_another_schema_counts() {
    let db = Empty::new().await;
    let mut conn = PgConnection::connect(&db.url()).await.unwrap();
    conn.execute("CREATE SCHEMA other; CREATE TABLE other.t (id int)")
        .await
        .unwrap();
    conn.close().await.unwrap();
    assert!(matches!(
        bootstrap::create_schema(&db.url()).await,
        Err(BootstrapError::NotEmpty { .. })
    ));
    db.drop_db().await;
}

/// The reference database is one of these: a Coolify schema with rows in it. This test builds its
/// own copy of that shape and never connects to the real one.
#[tokio::test]
async fn an_existing_coolify_database_is_refused_and_unchanged() {
    let test = TestDb::new("INSERT INTO teams (id, name, personal_team, created_at, updated_at) VALUES (900, 'kept', false, now(), now());").await;
    let url = url_of(&test.name);
    let before: (i64, i64) = {
        let mut conn = PgConnection::connect(&url).await.unwrap();
        (
            sqlx::query_scalar("SELECT count(*) FROM migrations")
                .fetch_one(&mut conn)
                .await
                .unwrap(),
            sqlx::query_scalar("SELECT count(*) FROM teams")
                .fetch_one(&mut conn)
                .await
                .unwrap(),
        )
    };
    match bootstrap::create_schema(&url).await {
        Err(BootstrapError::NotEmpty { relations }) => assert!(relations > 100),
        other => panic!("expected a refusal, got {other:?}"),
    }
    let mut conn = PgConnection::connect(&url).await.unwrap();
    let after: (i64, i64) = (
        sqlx::query_scalar("SELECT count(*) FROM migrations")
            .fetch_one(&mut conn)
            .await
            .unwrap(),
        sqlx::query_scalar("SELECT count(*) FROM teams")
            .fetch_one(&mut conn)
            .await
            .unwrap(),
    );
    assert_eq!(before, after);
    conn.close().await.unwrap();
    test.drop_db().await;
}

#[tokio::test]
async fn two_starts_at_once_create_it_once() {
    let db = Empty::new().await;
    let url = db.url();
    let (a, b) = tokio::join!(bootstrap::prepare(&url), bootstrap::prepare(&url));
    let mut outcomes = [a.unwrap(), b.unwrap()];
    outcomes.sort_by_key(|o| *o == Prepared::Existing);
    assert_eq!(outcomes, [Prepared::Created, Prepared::Existing]);
    assert_eq!(db.count("SELECT count(*) FROM migrations").await, 478);
    db.drop_db().await;
}

#[tokio::test]
async fn later_schema_changes_run_once_and_a_failure_rolls_back() {
    let db = Empty::new().await;
    bootstrap::prepare(&db.url()).await.unwrap();
    let good = [Migration {
        name: "9999_01_01_000000_add_upgrade_probe",
        sql: "CREATE TABLE public.upgrade_probe (id int)",
    }];
    assert_eq!(
        bootstrap::apply_migrations(&db.url(), &good).await.unwrap(),
        vec![good[0].name.to_owned()]
    );
    assert_eq!(
        db.count("SELECT batch::bigint FROM migrations ORDER BY id DESC LIMIT 1")
            .await,
        2
    );
    assert!(
        bootstrap::apply_migrations(&db.url(), &good)
            .await
            .unwrap()
            .is_empty()
    );

    let bad = [Migration {
        name: "9999_01_01_000001_broken",
        sql: "CREATE TABLE public.half_done (id int); SELECT * FROM does_not_exist",
    }];
    assert!(bootstrap::apply_migrations(&db.url(), &bad).await.is_err());
    assert_eq!(
        db.count("SELECT count(*) FROM pg_tables WHERE tablename = 'half_done'")
            .await,
        0
    );
    assert_eq!(db.count("SELECT count(*) FROM migrations").await, 479);
    db.drop_db().await;
}

/// What `pg_dump --schema-only` prints, read from the catalog: relations, columns in table order
/// (dropped ones left out, as `pg_dump` does), constraints, indexes, sequences, triggers, views,
/// functions. The literal timestamp defaults upstream's migrations bake in at migration time
/// (`last_online_at`, `sentinel_updated_at`) are normalized, as `compat/diff/install_diff.py` does.
const SCHEMA_FINGERPRINT: &str = r"
SELECT 'relation ' || c.relname || ' ' || c.relkind::text
  FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
  WHERE n.nspname = 'public' AND c.relkind IN ('r', 'p', 'v', 'm', 'S', 'f')
UNION ALL
SELECT 'column ' || c.relname || ' #' || row_number() OVER (PARTITION BY a.attrelid ORDER BY a.attnum)
       || ' ' || a.attname || ' ' || format_type(a.atttypid, a.atttypmod)
       || CASE WHEN a.attnotnull THEN ' NOT NULL' ELSE '' END
       || COALESCE(' DEFAULT ' || regexp_replace(pg_get_expr(d.adbin, d.adrelid),
            '''\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}''::timestamp without time zone',
            '<migration time>'), '')
  FROM pg_attribute a
  JOIN pg_class c ON c.oid = a.attrelid
  JOIN pg_namespace n ON n.oid = c.relnamespace
  LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum
  WHERE n.nspname = 'public' AND c.relkind IN ('r', 'p', 'v') AND a.attnum > 0 AND NOT a.attisdropped
UNION ALL
SELECT 'constraint ' || c.conrelid::regclass::text || ' ' || c.conname || ' ' || pg_get_constraintdef(c.oid)
  FROM pg_constraint c JOIN pg_namespace n ON n.oid = c.connamespace
  WHERE n.nspname = 'public'
UNION ALL
SELECT 'index ' || indexdef FROM pg_indexes WHERE schemaname = 'public'
UNION ALL
SELECT 'sequence ' || sequencename || ' ' || data_type::text || ' ' || start_value || ' '
       || increment_by || ' ' || COALESCE(cache_size, 0)
  FROM pg_sequences WHERE schemaname = 'public'
UNION ALL
SELECT 'owned ' || s.relname || ' ' || t.relname || '.' || a.attname
  FROM pg_depend dep
  JOIN pg_class s ON s.oid = dep.objid AND s.relkind = 'S'
  JOIN pg_class t ON t.oid = dep.refobjid
  JOIN pg_attribute a ON a.attrelid = t.oid AND a.attnum = dep.refobjsubid
  JOIN pg_namespace n ON n.oid = s.relnamespace
  WHERE n.nspname = 'public' AND dep.deptype = 'a'
UNION ALL
SELECT 'trigger ' || pg_get_triggerdef(t.oid) FROM pg_trigger t
  JOIN pg_class c ON c.oid = t.tgrelid JOIN pg_namespace n ON n.oid = c.relnamespace
  WHERE n.nspname = 'public' AND NOT t.tgisinternal
UNION ALL
SELECT 'view ' || viewname || ' ' || definition FROM pg_views WHERE schemaname = 'public'
UNION ALL
SELECT 'function ' || p.proname || ' ' || pg_get_functiondef(p.oid) FROM pg_proc p
  JOIN pg_namespace n ON n.oid = p.pronamespace WHERE n.nspname = 'public'
ORDER BY 1";

async fn fingerprint(url: &str) -> Vec<String> {
    let mut conn = PgConnection::connect(url).await.unwrap();
    let rows: Vec<String> = sqlx::query_scalar(SCHEMA_FINGERPRINT)
        .fetch_all(&mut conn)
        .await
        .unwrap();
    conn.close().await.ok();
    rows
}

async fn guard(url: &str) -> coolify_db::Db {
    coolify_db::Db::connect(&coolify_db::DbConfig {
        url: url.to_owned(),
        max_connections: 2,
        acquire_timeout: std::time::Duration::from_secs(5),
        allow_unknown_schema: false,
    })
    .await
    .unwrap()
}

const UPGRADE_SEED: &str = r#"
INSERT INTO teams (id, name, personal_team) VALUES (1, 'acme', false);
INSERT INTO private_keys (id, uuid, name, private_key, team_id) VALUES (1, 'k', 'k', 'x', 1);
INSERT INTO servers (id, uuid, name, ip, team_id, private_key_id, server_metadata) VALUES
  (1, 's1', 'highlight', '10.0.0.1', 1, 1, NULL),
  (2, 's2', 'highlight-moved', '10.0.0.2', 1, 1, '{"transfer": {"status": "transferred"}}'),
  (3, 's3', 'plain', '10.0.0.3', 1, 1, '{"transfer": {"status": "pending"}}');
INSERT INTO server_settings (server_id, is_logdrain_highlight_enabled, logdrain_highlight_project_id) VALUES
  (1, true, 'p1'), (2, true, 'p2'), (3, false, NULL);
INSERT INTO standalone_dockers (id, uuid, name, network, server_id) VALUES (1, 'd1', 'd', 'coolify', 1);
INSERT INTO projects (id, uuid, name, team_id) VALUES (1, 'p-uuid', 'Proj', 1);
INSERT INTO environments (id, uuid, name, project_id) VALUES (1, 'e-uuid', 'production', 1);
INSERT INTO applications (id, uuid, name, environment_id, build_pack, git_branch, git_repository, destination_type,
                          destination_id, custom_healthcheck_found, health_check_enabled, updated_at) VALUES
  (1, 'a1', 'dockerfile-healthcheck', 1, 'dockerfile', 'main', 'r', 'App\Models\StandaloneDocker', 1, true, true, '2026-01-02 03:04:05'),
  (2, 'a2', 'coolify-healthcheck', 1, 'dockerfile', 'main', 'r', 'App\Models\StandaloneDocker', 1, false, true, '2026-01-02 03:04:05'),
  (3, 'a3', 'no-healthcheck', 1, 'dockerfile', 'main', 'r', 'App\Models\StandaloneDocker', 1, true, false, '2026-01-02 03:04:05');
INSERT INTO application_previews (id, uuid, pull_request_id, pull_request_html_url, application_id) VALUES
  (1, 'pv1', 7, 'https://github.test/o/r/pull/7', 1);
INSERT INTO s3_storages (id, uuid, name, region, key, secret, bucket, endpoint, team_id) VALUES
  (1, 's3', 's3', 'us-east-1', 'k', 's', 'b', 'https://s3.test', 1);
"#;

/// A coolify-rs install created at upstream `main` @ b5823c7 upgrades itself on `boot`: the five
/// v4.5.3 migrations run, the schema equals a fresh v4.5.3 install's, and writes are allowed.
#[tokio::test]
async fn a_b5823c7_database_upgrades_to_4_5_3() {
    use coolify_db::testing::{
        BASELINE_MIGRATIONS, BASELINE_SCHEMA, MIGRATIONS_B5823C7, SCHEMA_B5823C7,
    };

    let old = TestDb::with_baseline(SCHEMA_B5823C7, MIGRATIONS_B5823C7, UPGRADE_SEED).await;
    let url = url_of(&old.name);
    assert!(
        old.db.schema().read_only_reason.is_some(),
        "an un-upgraded database lacks the v4.5.3 migrations"
    );
    assert_eq!(old.db.schema().missing.len(), 5);

    // `coolify-rs boot`: prepare, the log drains to stop, then the schema changes.
    assert_eq!(bootstrap::prepare(&url).await.unwrap(), Prepared::Existing);
    assert_eq!(
        bootstrap::highlight_log_drain_servers(&url).await.unwrap(),
        vec![1],
        "the transferred-away server is left alone"
    );
    let ran = bootstrap::apply_migrations(&url, bootstrap::MIGRATIONS)
        .await
        .unwrap();
    assert_eq!(
        ran,
        bootstrap::MIGRATIONS
            .iter()
            .map(|m| m.name.to_owned())
            .collect::<Vec<_>>()
    );
    assert!(
        bootstrap::apply_migrations(&url, bootstrap::MIGRATIONS)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        bootstrap::highlight_log_drain_servers(&url)
            .await
            .unwrap()
            .is_empty()
    );

    let mut conn = PgConnection::connect(&url).await.unwrap();
    let healthchecks: Vec<(i64, bool, String)> = sqlx::query_as(
        "SELECT id, health_check_enabled, updated_at::text FROM applications ORDER BY id",
    )
    .fetch_all(&mut conn)
    .await
    .unwrap();
    assert_eq!(
        healthchecks,
        vec![
            (1, false, "2026-01-02 03:04:05".to_owned()),
            (2, true, "2026-01-02 03:04:05".to_owned()),
            (3, false, "2026-01-02 03:04:05".to_owned()),
        ]
    );
    let keeps: bool =
        sqlx::query_scalar("SELECT keeps_production_values FROM application_previews")
            .fetch_one(&mut conn)
            .await
            .unwrap();
    assert!(
        keeps,
        "previews from before the upgrade keep production values"
    );
    let (path_style, axiom): (bool, Option<String>) = sqlx::query_as(
        "SELECT s.use_path_style_endpoint, ss.logdrain_axiom_domain FROM s3_storages s, server_settings ss WHERE ss.server_id = 1",
    )
    .fetch_one(&mut conn)
    .await
    .unwrap();
    assert!(path_style);
    assert_eq!(axiom.as_deref(), Some("us-east-1.aws.edge.axiom.co"));
    let batches: Vec<(String, i32)> =
        sqlx::query_as("SELECT migration, batch FROM migrations WHERE id > 473 ORDER BY id")
            .fetch_all(&mut conn)
            .await
            .unwrap();
    assert_eq!(batches.len(), 5);
    assert!(batches.iter().all(|(_, b)| *b == 2));
    conn.close().await.ok();

    let upgraded = guard(&url).await;
    assert!(upgraded.schema().read_only_reason.is_none());
    assert!(upgraded.schema().missing.is_empty());
    assert!(upgraded.schema().unknown.is_empty());
    upgraded.pool().close().await;

    let fresh = TestDb::with_baseline(BASELINE_SCHEMA, BASELINE_MIGRATIONS, "").await;
    let expected = fingerprint(&url_of(&fresh.name)).await;
    assert!(expected.iter().any(|r| r.contains("logdrain_axiom_domain")));
    let actual = fingerprint(&url).await;
    let only_upgraded: Vec<&String> = actual.iter().filter(|r| !expected.contains(r)).collect();
    let only_fresh: Vec<&String> = expected.iter().filter(|r| !actual.contains(r)).collect();
    assert!(
        only_upgraded.is_empty() && only_fresh.is_empty(),
        "schema differs from v4.5.3: upgraded has {only_upgraded:#?}, v4.5.3 has {only_fresh:#?}"
    );
    assert_eq!(actual, expected);
    fresh.drop_db().await;
    old.drop_db().await;
}

/// A database Coolify v4.5.3 created is writable as it is, and boot changes nothing in it.
#[tokio::test]
async fn a_4_5_3_database_is_writable() {
    let test = TestDb::new("").await;
    let url = url_of(&test.name);
    assert!(test.db.schema().read_only_reason.is_none());
    assert!(test.db.schema().missing.is_empty());
    let before = fingerprint(&url).await;
    assert!(
        bootstrap::highlight_log_drain_servers(&url)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        bootstrap::apply_migrations(&url, bootstrap::MIGRATIONS)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(fingerprint(&url).await, before);
    test.drop_db().await;
}

/// A newer Coolify (4.6.0) has migrations this build does not know: it stays read-only.
#[tokio::test]
async fn a_newer_coolify_database_is_read_only() {
    let test = TestDb::new(
        "INSERT INTO migrations (migration, batch) VALUES \
         ('2026_11_20_120000_a_coolify_4_6_0_migration', 2);",
    )
    .await;
    let reason = test.db.schema().read_only_reason.clone();
    assert!(
        reason.is_some(),
        "unknown migrations make the database read-only"
    );
    assert_eq!(
        test.db.schema().unknown,
        vec!["2026_11_20_120000_a_coolify_4_6_0_migration".to_owned()]
    );
    test.drop_db().await;
}
