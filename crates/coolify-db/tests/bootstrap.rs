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
    assert_eq!(db.count("SELECT count(*) FROM migrations").await, 493);
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
        493
    );
    assert_eq!(
        db.count("SELECT count(*) FROM migrations WHERE batch <> 1")
            .await,
        0
    );
    // The dump is Coolify v4.6.0: `boot` goes on to the v4.7.5 migrations, which are all it lacks.
    let created = guard(&db.url()).await;
    assert_eq!(created.schema().missing.len(), 27);
    let names: Vec<&str> = bootstrap::MIGRATIONS.iter().map(|m| m.name).collect();
    let (after, on_boot) = created.schema().after_migrations(&names, false);
    assert_eq!(on_boot.len(), 27);
    assert!(after.read_only_reason.is_none(), "{after:?}");
    created.pool().close().await;
    let ran = bootstrap::apply_migrations(&db.url(), bootstrap::MIGRATIONS)
        .await
        .unwrap();
    assert_eq!(ran, on_boot);
    assert_eq!(db.count("SELECT count(*) FROM migrations").await, 520);
    assert_eq!(
        db.count("SELECT count(*) FROM migrations WHERE batch = 2")
            .await,
        27
    );
    assert_eq!(
        db.count(
            "SELECT count(*) FROM pg_constraint WHERE contype = 'c' AND conrelid <> 0 AND pg_get_constraintdef(oid) LIKE '%ANY ((ARRAY[%::text[]%'"
        )
        .await,
        10,
        "the status check of scheduled_task_executions is replaced, not added"
    );
    // The schema guard accepts what was created.
    let guarded = guard(&db.url()).await;
    assert!(guarded.schema().read_only_reason.is_none());
    assert!(guarded.schema().missing.is_empty());
    assert!(guarded.schema().has_table("stars"));
    guarded.pool().close().await;
    // A second start leaves it alone.
    assert_eq!(
        bootstrap::prepare(&db.url()).await.unwrap(),
        Prepared::Existing
    );
    assert!(
        bootstrap::apply_migrations(&db.url(), bootstrap::MIGRATIONS)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(db.count("SELECT count(*) FROM migrations").await, 520);
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
    assert_eq!(db.count("SELECT count(*) FROM migrations").await, 493);
    db.drop_db().await;
}

#[tokio::test]
async fn later_schema_changes_run_once_and_a_failure_rolls_back() {
    let db = Empty::new().await;
    bootstrap::prepare(&db.url()).await.unwrap();
    let good = [Migration::new(
        "9999_01_01_000000_add_upgrade_probe",
        "CREATE TABLE public.upgrade_probe (id int)",
    )];
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

    let bad = [Migration::new(
        "9999_01_01_000001_broken",
        "CREATE TABLE public.half_done (id int); SELECT * FROM does_not_exist",
    )];
    assert!(bootstrap::apply_migrations(&db.url(), &bad).await.is_err());
    assert_eq!(
        db.count("SELECT count(*) FROM pg_tables WHERE tablename = 'half_done'")
            .await,
        0
    );
    assert_eq!(db.count("SELECT count(*) FROM migrations").await, 494);

    // `$withinTransaction = false`: each statement on its own, so `CONCURRENTLY` is accepted.
    let concurrent = [Migration::without_transaction(
        "9999_01_01_000002_add_probe_indexes",
        "CREATE INDEX CONCURRENTLY IF NOT EXISTS idx_probe_a ON public.upgrade_probe (id); \
         CREATE INDEX CONCURRENTLY IF NOT EXISTS idx_probe_b ON public.upgrade_probe (id DESC)",
    )];
    assert_eq!(
        bootstrap::apply_migrations(&db.url(), &concurrent)
            .await
            .unwrap(),
        vec![concurrent[0].name.to_owned()]
    );
    assert_eq!(
        db.count("SELECT count(*) FROM pg_indexes WHERE tablename = 'upgrade_probe'")
            .await,
        2
    );
    assert!(
        bootstrap::apply_migrations(&db.url(), &concurrent)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(db.count("SELECT count(*) FROM migrations").await, 495);
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
/// v4.5.3 migrations, the fifteen of v4.6.0 and the twenty-seven of v4.7.5 run, the schema equals
/// a fresh install's, and writes are allowed.
#[tokio::test]
async fn a_b5823c7_database_upgrades_to_4_7_5() {
    use coolify_db::testing::{MIGRATIONS_B5823C7, SCHEMA_B5823C7};

    let old = TestDb::with_baseline(SCHEMA_B5823C7, MIGRATIONS_B5823C7, UPGRADE_SEED).await;
    let url = url_of(&old.name);
    assert!(
        old.db.schema().read_only_reason.is_some(),
        "an un-upgraded database lacks the v4.5.3 migrations"
    );
    assert_eq!(old.db.schema().missing.len(), 47);

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
    assert_eq!(batches.len(), 47);
    assert!(batches.iter().all(|(_, b)| *b == 2));
    // v4.7.5: the storage from before the upgrade is an S3 one.
    let (kind, bucket): (String, Option<String>) =
        sqlx::query_as("SELECT type, bucket FROM s3_storages WHERE id = 1")
            .fetch_one(&mut conn)
            .await
            .unwrap();
    assert_eq!((kind.as_str(), bucket.as_deref()), ("s3", Some("b")));
    conn.close().await.ok();

    let upgraded = guard(&url).await;
    assert!(upgraded.schema().read_only_reason.is_none());
    assert!(upgraded.schema().missing.is_empty());
    assert!(upgraded.schema().unknown.is_empty());
    upgraded.pool().close().await;

    assert_same_schema_as_baseline(&url).await;
    old.drop_db().await;
}

/// The schema at `url` equals a fresh install's (the baseline: Coolify v4.7.5).
async fn assert_same_schema_as_baseline(url: &str) {
    let fresh = TestDb::new("").await;
    let expected = fingerprint(&url_of(&fresh.name)).await;
    assert!(expected.iter().any(|r| r.contains("logdrain_axiom_domain")));
    assert!(expected.iter().any(|r| r.contains("update_channel")));
    assert!(expected.iter().any(|r| r.contains("image_scan_cpus")));
    assert!(expected.iter().any(|r| r == "relation stars r"));
    let actual = fingerprint(url).await;
    let only_upgraded: Vec<&String> = actual.iter().filter(|r| !expected.contains(r)).collect();
    let only_fresh: Vec<&String> = expected.iter().filter(|r| !actual.contains(r)).collect();
    assert!(
        only_upgraded.is_empty() && only_fresh.is_empty(),
        "schema differs from v4.7.5: upgraded has {only_upgraded:#?}, v4.7.5 has {only_fresh:#?}"
    );
    assert_eq!(actual, expected);
    fresh.drop_db().await;
}

/// API tokens and an invitation as Coolify v4.5.3 writes them (`json_encode` of the abilities).
const TOKENS_SEED: &str = r#"
INSERT INTO users (id, name, email, password) VALUES (1, 'u', 'u@example.test', 'x');
INSERT INTO teams (id, name, personal_team) VALUES (1, 'acme', false);
INSERT INTO team_user (team_id, user_id, role) VALUES (1, 1, 'owner');
INSERT INTO team_invitations (id, uuid, team_id, email, link) VALUES (1, 'inv', 1, 'v@example.test', 'https://x.test/i');
INSERT INTO personal_access_tokens (id, tokenable_type, tokenable_id, name, token, team_id, abilities, updated_at) VALUES
  (1, 'App\Models\User', 1, 'root', 't1', '1', '["root"]', '2026-01-02 03:04:05'),
  (2, 'App\Models\User', 1, 'write', 't2', '1', '["write"]', '2026-01-02 03:04:05'),
  (3, 'App\Models\User', 1, 'read-write', 't3', '1', '["read","write","deploy"]', '2026-01-02 03:04:05'),
  (4, 'App\Models\User', 1, 'has-delete', 't4', '1', '["write","delete"]', '2026-01-02 03:04:05'),
  (5, 'App\Models\User', 1, 'read', 't5', '1', '["read","read:sensitive"]', '2026-01-02 03:04:05'),
  (6, 'App\Models\User', 1, 'null', 't6', '1', NULL, '2026-01-02 03:04:05'),
  (7, 'App\Models\User', 1, 'broken', 't7', '1', 'not json', '2026-01-02 03:04:05'),
  (8, 'App\Models\User', 1, 'spaced', 't8', '1', '[ "write", "a/b" ]', '2026-01-02 03:04:05');
"#;

/// coolify-rs 4.5.3-rs.N (a database Coolify v4.5.3 created) upgrades itself on `boot`: the fifteen
/// v4.6.0 migrations and the twenty-seven of v4.7.5 run in name order, write tokens get the new
/// delete ability, the schema equals a fresh install's, and writes are allowed.
#[tokio::test]
async fn a_4_5_3_database_upgrades_to_4_7_5() {
    use coolify_db::testing::{MIGRATIONS_4_5_3, SCHEMA_4_5_3};

    let old = TestDb::with_baseline(SCHEMA_4_5_3, MIGRATIONS_4_5_3, TOKENS_SEED).await;
    let url = url_of(&old.name);
    assert!(old.db.schema().read_only_reason.is_some());
    assert_eq!(old.db.schema().missing.len(), 42);
    assert!(
        !old.db
            .schema()
            .has_column("personal_access_tokens", "scopes")
    );
    // What `check-schema` reports for it: nothing left missing once `boot` has run.
    let names: Vec<&str> = bootstrap::MIGRATIONS.iter().map(|m| m.name).collect();
    let (after, on_boot) = old.db.schema().after_migrations(&names, false);
    assert_eq!(on_boot.len(), 42);
    assert!(after.read_only_reason.is_none(), "{after:?}");

    assert_eq!(bootstrap::prepare(&url).await.unwrap(), Prepared::Existing);
    let ran = bootstrap::apply_migrations(&url, bootstrap::MIGRATIONS)
        .await
        .unwrap();
    // Name order, as `php artisan migrate` runs pending migrations: v4.7.5's image scanning
    // tables come before v4.6.0's last four.
    assert_eq!(ran, on_boot);
    assert!(ran.windows(2).all(|w| w[0] < w[1]));
    assert!(ran.iter().all(|m| m.as_str() < "2026_11_10"));
    assert!(
        bootstrap::apply_migrations(&url, bootstrap::MIGRATIONS)
            .await
            .unwrap()
            .is_empty()
    );

    let mut conn = PgConnection::connect(&url).await.unwrap();
    let tokens: Vec<(i64, Option<String>, String)> = sqlx::query_as(
        "SELECT id, abilities, updated_at::text FROM personal_access_tokens ORDER BY id",
    )
    .fetch_all(&mut conn)
    .await
    .unwrap();
    let abilities: Vec<(i64, Option<&str>)> = tokens
        .iter()
        .map(|(id, a, _)| (*id, a.as_deref()))
        .collect();
    assert_eq!(
        abilities,
        vec![
            (1, Some(r#"["root"]"#)),
            (2, Some(r#"["write","delete"]"#)),
            (3, Some(r#"["read","write","deploy","delete"]"#)),
            (4, Some(r#"["write","delete"]"#)),
            (5, Some(r#"["read","read:sensitive"]"#)),
            (6, None),
            (7, Some("not json")),
            // `json_encode` writes it compact, with `/` escaped.
            (8, Some(r#"["write","a\/b","delete"]"#)),
        ]
    );
    assert!(tokens.iter().all(|(_, _, u)| u == "2026-01-02 03:04:05"));
    // Existing invitations and memberships get no grants (full access, as before).
    let access: (Option<String>, Option<String>) = sqlx::query_as(
        "SELECT (SELECT access::text FROM team_invitations WHERE id = 1), \
                (SELECT access::text FROM team_user WHERE user_id = 1)",
    )
    .fetch_one(&mut conn)
    .await
    .unwrap();
    assert_eq!(access, (None, None));
    let batches: Vec<i32> = sqlx::query_scalar("SELECT batch FROM migrations WHERE id > 478")
        .fetch_all(&mut conn)
        .await
        .unwrap();
    assert_eq!(batches, vec![2; 42]);
    conn.close().await.ok();

    let upgraded = guard(&url).await;
    assert!(upgraded.schema().read_only_reason.is_none());
    assert!(upgraded.schema().missing.is_empty());
    assert!(upgraded.schema().unknown.is_empty());
    assert!(
        upgraded
            .schema()
            .has_column("personal_access_tokens", "scopes")
    );
    assert!(upgraded.schema().has_column("team_user", "access"));
    upgraded.pool().close().await;

    assert_same_schema_as_baseline(&url).await;
    old.drop_db().await;
}

/// A Coolify v4.6.0 installation with rows the v4.7.5 migrations touch or must leave alone.
const SEED_4_6_0: &str = r#"
INSERT INTO instance_settings (id, smtp_from_address, smtp_from_name) VALUES (0, 'enc-address', 'enc-name');
INSERT INTO teams (id, name, personal_team) VALUES (1, 'acme', false), (2, 'other', false);
INSERT INTO email_notification_settings (team_id, smtp_from_address, smtp_from_name) VALUES
  (1, 'team-address', 'team-name'), (2, NULL, NULL);
INSERT INTO private_keys (id, uuid, name, private_key, team_id) VALUES (1, 'k', 'k', 'x', 1), (2, 'k2', 'k2', 'x', 1);
INSERT INTO servers (id, uuid, name, ip, team_id, private_key_id) VALUES (1, 's1', 'one', '10.0.0.1', 1, 1);
INSERT INTO server_settings (server_id) VALUES (1);
INSERT INTO standalone_dockers (id, uuid, name, network, server_id) VALUES (1, 'd1', 'd', 'coolify', 1);
INSERT INTO standalone_redis (id, uuid, name, destination_type, destination_id) VALUES
  (1, 'r1', 'default-image', 'App\Models\StandaloneDocker', 1);
INSERT INTO standalone_redis (id, uuid, name, image, destination_type, destination_id) VALUES
  (2, 'r2', 'own-image', 'redis:6', 'App\Models\StandaloneDocker', 1);
INSERT INTO standalone_postgresqls (id, uuid, name, postgres_password, destination_type, destination_id) VALUES
  (1, 'pg1', 'default-image', 'x', 'App\Models\StandaloneDocker', 1);
INSERT INTO s3_storages (id, uuid, name, region, key, secret, bucket, endpoint, team_id) VALUES
  (1, 's3', 's3', 'us-east-1', 'k', 's', 'b', 'https://s3.test', 1);
"#;

/// The instance policy row of `create_image_scanning_tables`.
type PolicyRow = (
    String,
    Option<String>,
    Option<i64>,
    Option<String>,
    Option<String>,
    Option<bool>,
    bool,
);

/// coolify-rs 4.6.0-rs.N (a database Coolify v4.6.0 created) upgrades itself on `boot`: the
/// twenty-seven migrations of v4.7.0 to v4.7.5 run with their data changes, and writes are allowed.
#[tokio::test]
async fn a_4_6_0_database_upgrades_to_4_7_5() {
    use coolify_db::testing::{MIGRATIONS_4_6_0, SCHEMA_4_6_0};

    let old = TestDb::with_baseline(SCHEMA_4_6_0, MIGRATIONS_4_6_0, SEED_4_6_0).await;
    let url = url_of(&old.name);
    assert!(
        old.db.schema().read_only_reason.is_some(),
        "an un-upgraded database lacks the v4.7.5 migrations"
    );
    assert_eq!(old.db.schema().missing.len(), 27);
    assert!(!old.db.schema().has_column("s3_storages", "type"));
    assert!(!old.db.schema().has_table("stars"));
    let names: Vec<&str> = bootstrap::MIGRATIONS.iter().map(|m| m.name).collect();
    let (after, on_boot) = old.db.schema().after_migrations(&names, false);
    assert_eq!(on_boot.len(), 27);
    assert!(after.read_only_reason.is_none(), "{after:?}");

    assert_eq!(bootstrap::prepare(&url).await.unwrap(), Prepared::Existing);
    assert!(
        bootstrap::highlight_log_drain_servers(&url)
            .await
            .unwrap()
            .is_empty()
    );
    let ran = bootstrap::apply_migrations(&url, bootstrap::MIGRATIONS)
        .await
        .unwrap();
    assert_eq!(ran, on_boot);
    assert_eq!(
        ran.first().map(String::as_str),
        Some("2026_10_29_100000_create_image_scanning_tables")
    );
    // The two that share a timestamp run in file name order.
    assert_eq!(
        ran[1..3],
        [
            "2026_10_30_120000_add_git_ssh_known_hosts_to_applications_table".to_owned(),
            "2026_10_30_120000_add_ssh_host_key_to_server_settings_table".to_owned(),
        ]
    );
    assert_eq!(
        ran.last().map(String::as_str),
        Some("2026_11_09_140000_add_image_scanner_time_and_cpu_limits_to_server_settings_table")
    );
    assert!(
        bootstrap::apply_migrations(&url, bootstrap::MIGRATIONS)
            .await
            .unwrap()
            .is_empty()
    );

    let mut conn = PgConnection::connect(&url).await.unwrap();
    // `create_image_scanning_tables` inserts the instance policy.
    let policies: Vec<PolicyRow> = sqlx::query_as(
        "SELECT uuid, scope_type, scope_id, gate_mode, on_scan_error, count_medium, \
                (created_at IS NOT NULL AND created_at = updated_at) FROM image_scan_policies",
    )
    .fetch_all(&mut conn)
    .await
    .unwrap();
    assert_eq!(policies.len(), 1);
    let (uuid, scope_type, scope_id, gate_mode, on_scan_error, count_medium, stamped) =
        &policies[0];
    assert_eq!(uuid.len(), 24);
    assert!(
        uuid.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()),
        "{uuid}"
    );
    assert!(scope_type.is_none() && scope_id.is_none());
    assert_eq!(gate_mode.as_deref(), Some("off"));
    assert_eq!(on_scan_error.as_deref(), Some("deploy"));
    assert_eq!(*count_medium, Some(false));
    assert!(*stamped);

    // `add_resend_sender_to_email_settings` copies the SMTP sender as it is stored.
    let instance: (Option<String>, Option<String>) = sqlx::query_as(
        "SELECT resend_from_address, resend_from_name FROM instance_settings WHERE id = 0",
    )
    .fetch_one(&mut conn)
    .await
    .unwrap();
    assert_eq!(
        instance,
        (Some("enc-address".to_owned()), Some("enc-name".to_owned()))
    );
    let teams: Vec<(i64, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT team_id, resend_from_address, resend_from_name \
         FROM email_notification_settings ORDER BY team_id",
    )
    .fetch_all(&mut conn)
    .await
    .unwrap();
    assert_eq!(
        teams,
        vec![
            (
                1,
                Some("team-address".to_owned()),
                Some("team-name".to_owned())
            ),
            (2, None, None),
        ]
    );
    // Named SMTP servers start empty; the alert switch is on for email only.
    let smtp: (Option<String>, Option<String>, bool) = sqlx::query_as(
        "SELECT smtp_server_name, smtp_servers, image_vulnerabilities_email_notifications \
         FROM email_notification_settings WHERE team_id = 1",
    )
    .fetch_one(&mut conn)
    .await
    .unwrap();
    assert_eq!(smtp, (None, None, true));

    // Existing databases keep their image; only the column defaults change.
    let images: Vec<(i64, String)> =
        sqlx::query_as("SELECT id, image FROM standalone_redis ORDER BY id")
            .fetch_all(&mut conn)
            .await
            .unwrap();
    assert_eq!(
        images,
        vec![(1, "redis:7.2".to_owned()), (2, "redis:6".to_owned())]
    );
    let postgres: String =
        sqlx::query_scalar("SELECT image FROM standalone_postgresqls WHERE id = 1")
            .fetch_one(&mut conn)
            .await
            .unwrap();
    assert_eq!(postgres, "postgres:16-alpine");
    let defaults: Vec<(String, String)> = sqlx::query_as(
        "SELECT table_name::text, column_default::text FROM information_schema.columns \
         WHERE table_schema = 'public' AND column_name = 'image' AND table_name LIKE 'standalone%' \
           AND column_default IS NOT NULL ORDER BY table_name",
    )
    .fetch_all(&mut conn)
    .await
    .unwrap();
    let defaults: Vec<(&str, &str)> = defaults
        .iter()
        .map(|(t, d)| (t.as_str(), d.as_str()))
        .collect();
    assert_eq!(
        defaults,
        vec![
            (
                "standalone_clickhouses",
                "'clickhouse/clickhouse-server:26.8'::character varying"
            ),
            // Not changed by v4.7: the defaults these tables have had since v4.5.
            (
                "standalone_dragonflies",
                "'docker.dragonflydb.io/dragonflydb/dragonfly'::character varying"
            ),
            (
                "standalone_keydbs",
                "'eqalpha/keydb:latest'::character varying"
            ),
            ("standalone_mariadbs", "'mariadb:12.3'::character varying"),
            ("standalone_mongodbs", "'mongo:9'::character varying"),
            ("standalone_mysqls", "'mysql:9.7'::character varying"),
            (
                "standalone_postgresqls",
                "'postgres:18-alpine'::character varying"
            ),
            (
                "standalone_redis",
                "'valkey/valkey:9-alpine'::character varying"
            ),
        ]
    );

    // The storage is an S3 one. A storage without a bucket (SFTP) can be stored, and loses its
    // key when the key is deleted (`nullOnDelete`).
    let storage: (String, Option<String>, Option<i64>, Option<String>, bool) = sqlx::query_as(
        "SELECT type, bucket, private_key_id, sftp_host_key, skip_tls_verification FROM s3_storages WHERE id = 1",
    )
    .fetch_one(&mut conn)
    .await
    .unwrap();
    assert_eq!(
        storage,
        ("s3".to_owned(), Some("b".to_owned()), None, None, false)
    );
    conn.execute(
        "INSERT INTO s3_storages (id, uuid, name, region, key, secret, bucket, endpoint, team_id, type, private_key_id) \
         VALUES (2, 'sftp', 'sftp', 'us-east-1', 'k', 's', NULL, 'sftp://host', 1, 'sftp', 2); \
         DELETE FROM private_keys WHERE id = 2",
    )
    .await
    .unwrap();
    let sftp: (String, Option<String>, Option<i64>) =
        sqlx::query_as("SELECT type, bucket, private_key_id FROM s3_storages WHERE id = 2")
            .fetch_one(&mut conn)
            .await
            .unwrap();
    assert_eq!(sftp, ("sftp".to_owned(), None, None));

    // Executions may be skipped; the two unused audit indexes are gone.
    let check: String = sqlx::query_scalar(
        "SELECT pg_get_constraintdef(oid) FROM pg_constraint WHERE conname = 'scheduled_task_executions_status_check'",
    )
    .fetch_one(&mut conn)
    .await
    .unwrap();
    assert!(check.contains("'skipped'"), "{check}");
    let audit_indexes: Vec<String> = sqlx::query_scalar(
        "SELECT indexname::text FROM pg_indexes WHERE schemaname = 'public' AND tablename = 'audit_events' \
         ORDER BY indexname COLLATE \"C\"",
    )
    .fetch_all(&mut conn)
    .await
    .unwrap();
    assert_eq!(
        audit_indexes,
        [
            "audit_events_created_at_index",
            "audit_events_pkey",
            "audit_events_team_id_action_created_at_id_index",
            "audit_events_team_id_actor_id_created_at_index",
            "audit_events_team_id_created_at_id_index",
            "audit_events_team_id_source_created_at_id_index",
        ]
    );

    // Column defaults of the settings the other areas read.
    let server: (bool, bool, bool, i32, i32, i32, String, i32) = sqlx::query_as(
        "SELECT is_ssh_host_key_check_disabled, is_ssh_host_key_check_enforced, is_image_scan_enabled, \
                image_scan_memory_mb, image_scan_syft_timeout, image_scan_grype_timeout, image_scan_cpus::text, \
                image_candidate_scans_today \
         FROM server_settings WHERE server_id = 1",
    )
    .fetch_one(&mut conn)
    .await
    .unwrap();
    assert_eq!(
        server,
        (false, false, false, 1024, 600, 600, "1.00".to_owned(), 0)
    );
    let batches: Vec<i32> = sqlx::query_scalar("SELECT batch FROM migrations WHERE id > 493")
        .fetch_all(&mut conn)
        .await
        .unwrap();
    assert_eq!(batches, vec![2; 27]);
    conn.close().await.ok();

    let upgraded = guard(&url).await;
    assert!(upgraded.schema().read_only_reason.is_none());
    assert!(upgraded.schema().missing.is_empty());
    assert!(upgraded.schema().unknown.is_empty());
    assert!(upgraded.schema().has_column("s3_storages", "type"));
    assert!(
        upgraded
            .schema()
            .has_column("application_settings", "include_network_hosts_in_build")
    );
    for table in [
        "image_scan_policies",
        "image_scan_ignores",
        "image_sboms",
        "image_scans",
        "image_scan_notification_states",
        "server_host_scans",
        "stars",
    ] {
        assert!(upgraded.schema().has_table(table), "{table}");
    }
    upgraded.pool().close().await;

    assert_same_schema_as_baseline(&url).await;
    old.drop_db().await;
}

async fn columns_of(url: &str, table: &str) -> Vec<String> {
    let mut conn = PgConnection::connect(url).await.unwrap();
    let names: Vec<String> = sqlx::query_scalar(
        "SELECT column_name::text FROM information_schema.columns \
         WHERE table_schema = 'public' AND table_name = $1",
    )
    .bind(table)
    .fetch_all(&mut conn)
    .await
    .unwrap();
    conn.close().await.ok();
    names
}

/// The columns nightly builds of v4.7.0 created and v4.7.5 removed again are dropped, and the
/// columns such a build already has are kept as they are.
#[tokio::test]
async fn a_nightly_database_loses_the_removed_columns() {
    use coolify_db::testing::{MIGRATIONS_4_6_0, SCHEMA_4_6_0};

    let old = TestDb::with_baseline(
        SCHEMA_4_6_0,
        MIGRATIONS_4_6_0,
        "ALTER TABLE instance_settings ADD COLUMN is_image_scanning_allowed boolean NOT NULL DEFAULT false, \
             ADD COLUMN image_scan_db_url text NULL, ADD COLUMN smtp_servers text NULL; \
         ALTER TABLE scheduled_tasks ADD COLUMN failure_count integer NOT NULL DEFAULT 0, \
             ADD COLUMN auto_disabled_at timestamp(0) without time zone NULL, \
             ADD COLUMN failing_since timestamp(0) without time zone NULL; \
         ALTER TABLE application_deployment_queues ADD COLUMN is_mcp boolean NOT NULL DEFAULT true;",
    )
    .await;
    let url = url_of(&old.name);
    assert_eq!(
        bootstrap::apply_migrations(&url, bootstrap::MIGRATIONS)
            .await
            .unwrap()
            .len(),
        27
    );
    let has = |columns: &[String], name: &str| columns.iter().any(|c| c == name);
    let instance = columns_of(&url, "instance_settings").await;
    assert!(!has(&instance, "is_image_scanning_allowed"));
    assert!(!has(&instance, "image_scan_db_url"));
    // `smtp_servers` was there: upstream skips the table, so `smtp_server_name` is not added.
    assert!(has(&instance, "smtp_servers"));
    assert!(!has(&instance, "smtp_server_name"));
    assert!(has(&instance, "resend_from_address"));
    let email = columns_of(&url, "email_notification_settings").await;
    assert!(has(&email, "smtp_servers") && has(&email, "smtp_server_name"));
    let tasks = columns_of(&url, "scheduled_tasks").await;
    assert!(!has(&tasks, "failure_count"));
    assert!(!has(&tasks, "auto_disabled_at"));
    for kept in [
        "consecutive_failures",
        "failing_since",
        "disabled_reason",
        "use_shell",
    ] {
        assert!(has(&tasks, kept), "{kept}");
    }
    let mut conn = PgConnection::connect(&url).await.unwrap();
    let is_mcp_default: String = sqlx::query_scalar(
        "SELECT column_default::text FROM information_schema.columns \
         WHERE table_schema = 'public' AND table_name = 'application_deployment_queues' AND column_name = 'is_mcp'",
    )
    .fetch_one(&mut conn)
    .await
    .unwrap();
    conn.close().await.ok();
    assert_eq!(
        is_mcp_default, "true",
        "the existing is_mcp column is left as it is"
    );
    old.drop_db().await;
}

/// A database Coolify v4.7.5 created is writable as it is, and boot changes nothing in it.
#[tokio::test]
async fn a_4_7_5_database_is_writable() {
    let test = TestDb::new("").await;
    let url = url_of(&test.name);
    assert!(test.db.schema().read_only_reason.is_none());
    assert!(test.db.schema().missing.is_empty());
    assert_eq!(test.db.schema().applied, 520);
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

/// A newer Coolify (4.8.0) has migrations this build does not know: it stays read-only.
#[tokio::test]
async fn a_newer_coolify_database_is_read_only() {
    let test = TestDb::new(
        "INSERT INTO migrations (migration, batch) VALUES \
         ('2026_11_20_120000_a_coolify_4_8_0_migration', 2);",
    )
    .await;
    let reason = test.db.schema().read_only_reason.clone();
    assert!(
        reason.is_some(),
        "unknown migrations make the database read-only"
    );
    assert_eq!(
        test.db.schema().unknown,
        vec!["2026_11_20_120000_a_coolify_4_8_0_migration".to_owned()]
    );
    test.drop_db().await;
}

/// The four rows a real, long-lived Coolify installation has beyond the v4.7.5 tree:
/// migrations upstream renamed or deleted after they ran.
const HISTORICAL_ROWS: &str = "INSERT INTO migrations (migration, batch) VALUES \
     ('2018_08_08_100000_create_telescope_entries_table', 1), \
     ('2025_10_10_120000_create_cloud_init_scripts_table', 30), \
     ('2025_10_10_120000_create_webhook_notification_settings_table', 30), \
     ('2025_10_10_120001_populate_webhook_notification_settings_for_existing_teams', 30);";

/// Those names are known: the database is writable, and boot changes nothing.
#[tokio::test]
async fn a_long_lived_coolify_database_is_writable() {
    let test = TestDb::new(HISTORICAL_ROWS).await;
    let url = url_of(&test.name);
    let schema = test.db.schema();
    assert!(schema.read_only_reason.is_none(), "{schema:?}");
    assert!(schema.unknown.is_empty());
    assert!(schema.missing.is_empty());
    assert_eq!(schema.historical.len(), 4);
    assert_eq!(schema.applied, 524);
    test.db.ensure_writable().unwrap();
    assert!(
        bootstrap::apply_migrations(&url, bootstrap::MIGRATIONS)
            .await
            .unwrap()
            .is_empty()
    );
    test.drop_db().await;

    // A truly unknown name next to them still makes it read-only, and is the one reported.
    let test = TestDb::new(&format!(
        "{HISTORICAL_ROWS} INSERT INTO migrations (migration, batch) VALUES \
         ('2026_11_20_120000_a_coolify_4_8_0_migration', 31);"
    ))
    .await;
    assert!(test.db.schema().read_only_reason.is_some());
    assert!(test.db.ensure_writable().is_err());
    assert_eq!(
        test.db.schema().unknown,
        vec!["2026_11_20_120000_a_coolify_4_8_0_migration".to_owned()]
    );
    assert_eq!(test.db.schema().historical.len(), 4);
    test.drop_db().await;
}
