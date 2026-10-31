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
    assert_eq!(db.count("SELECT count(*) FROM migrations").await, 473);
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
        473
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
    assert_eq!(db.count("SELECT count(*) FROM migrations").await, 473);
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
    assert_eq!(db.count("SELECT count(*) FROM migrations").await, 473);
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
    assert_eq!(db.count("SELECT count(*) FROM migrations").await, 474);
    db.drop_db().await;
}
