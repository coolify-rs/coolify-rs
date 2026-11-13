//! Per-test databases loaded with an upstream baseline schema (default: Coolify
//! v4.7.5, built as the v4.6.0 dump plus `bootstrap::MIGRATIONS`, which is what
//! `coolify-rs boot` does on an empty database; v4.6.0, v4.5.3 and upstream `main` @
//! b5823c7 are kept to test the upgrade of databases created by earlier coolify-rs
//! builds, 4.5.0 to test older databases).
//!
//! Requires `COOLIFY_RS_TEST_DATABASE_URL` (an admin connection, e.g. the
//! `compose.yaml` Postgres). Missing infrastructure is a hard failure, never
//! a silent pass.

// Test-only helper: failing loudly is the intended behaviour.
#![allow(clippy::expect_used, clippy::panic)]

use std::time::Duration;

use rand::Rng;
use sqlx::{Connection, Executor, PgConnection};

use crate::{Db, DbConfig};

/// The dump [`TestDb::new`] loads before it applies `bootstrap::MIGRATIONS`: Coolify v4.6.0 until
/// a v4.7.5 dump is captured (then this is that dump, and the migrations are all listed).
pub const BASELINE_SCHEMA: &str =
    include_str!("../../../compat/baseline/upstream-4.6.0-schema.sql");
pub const BASELINE_MIGRATIONS: &str =
    include_str!("../../../compat/baseline/upstream-4.6.0-migrations.sql");
/// A database Coolify v4.6.0 (coolify-rs 4.6.0-rs.N) created, for the upgrade tests.
pub const SCHEMA_4_6_0: &str = include_str!("../../../compat/baseline/upstream-4.6.0-schema.sql");
pub const MIGRATIONS_4_6_0: &str =
    include_str!("../../../compat/baseline/upstream-4.6.0-migrations.sql");
pub const SCHEMA_4_5_3: &str = include_str!("../../../compat/baseline/upstream-4.5.3-schema.sql");
pub const MIGRATIONS_4_5_3: &str =
    include_str!("../../../compat/baseline/upstream-4.5.3-migrations.sql");
pub const SCHEMA_B5823C7: &str =
    include_str!("../../../compat/baseline/upstream-main-b5823c7-schema.sql");
pub const MIGRATIONS_B5823C7: &str =
    include_str!("../../../compat/baseline/upstream-main-b5823c7-migrations.sql");
pub const SCHEMA_4_5_0: &str = include_str!("../../../compat/baseline/upstream-4.5.0-schema.sql");
pub const MIGRATIONS_4_5_0: &str =
    include_str!("../../../compat/baseline/upstream-4.5.0-migrations.sql");

#[derive(Debug)]
pub struct TestDb {
    pub db: Db,
    pub name: String,
    admin_url: String,
}

fn admin_url() -> String {
    std::env::var("COOLIFY_RS_TEST_DATABASE_URL").unwrap_or_else(|_| {
        panic!(
            "COOLIFY_RS_TEST_DATABASE_URL is not set. Start the dev database with \
             `docker compose up -d --wait` and run tests via scripts/test.sh"
        )
    })
}

fn with_database(url: &str, name: &str) -> String {
    match url.rfind('/') {
        Some(i) if !url[..i].ends_with('/') => format!("{}/{name}", &url[..i]),
        _ => format!("{url}/{name}"),
    }
}

impl TestDb {
    /// Creates a fresh database with the current upstream schema (Coolify v4.7.5: the baseline
    /// dump, its migrations table, then `bootstrap::MIGRATIONS`), and `seed_sql` applied.
    pub async fn new(seed_sql: &str) -> Self {
        Self::create(BASELINE_SCHEMA, BASELINE_MIGRATIONS, true, seed_sql).await
    }

    /// Like [`new`](Self::new) with an explicit baseline (schema dump + migrations rows), left as
    /// that upstream release created it: `bootstrap::MIGRATIONS` are not applied.
    pub async fn with_baseline(schema: &str, migrations: &str, seed_sql: &str) -> Self {
        Self::create(schema, migrations, false, seed_sql).await
    }

    async fn create(schema: &str, migrations: &str, upgrade: bool, seed_sql: &str) -> Self {
        let admin_url = admin_url();
        let name = format!("t_{:016x}", rand::thread_rng().r#gen::<u64>());
        let mut admin = PgConnection::connect(&admin_url)
            .await
            .expect("connect admin database");
        admin
            .execute(format!("CREATE DATABASE {name}").as_str())
            .await
            .expect("create test database");
        drop(admin);

        let url = with_database(&admin_url, &name);
        // The dump changes search_path for its session; use a throwaway connection.
        let mut conn = PgConnection::connect(&url)
            .await
            .expect("connect test database");
        sqlx::raw_sql(schema)
            .execute(&mut conn)
            .await
            .expect("load baseline schema");
        sqlx::raw_sql(migrations)
            .execute(&mut conn)
            .await
            .expect("load migrations table");
        conn.close().await.ok();
        if upgrade {
            crate::bootstrap::apply_migrations(&url, crate::bootstrap::MIGRATIONS)
                .await
                .expect("apply bootstrap migrations");
        }
        let mut conn = PgConnection::connect(&url)
            .await
            .expect("reconnect test database");
        sqlx::raw_sql(seed_sql)
            .execute(&mut conn)
            .await
            .expect("apply seed");
        conn.close().await.ok();

        let db = Db::connect(&DbConfig {
            url,
            max_connections: 5,
            acquire_timeout: Duration::from_secs(10),
            allow_unknown_schema: false,
        })
        .await
        .expect("connect Db");
        Self {
            db,
            name,
            admin_url,
        }
    }

    /// Drops the database. Call at the end of a test (failed tests leave it
    /// behind for inspection; the dev Postgres uses tmpfs).
    pub async fn drop_db(self) {
        let TestDb {
            db,
            name,
            admin_url,
        } = self;
        db.pool().close().await;
        if let Ok(mut admin) = PgConnection::connect(&admin_url).await {
            let _ = admin
                .execute(format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)").as_str())
                .await;
        }
    }
}
