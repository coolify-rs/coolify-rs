//! A new installation's database: the baseline schema and the upstream `migrations` rows.
//!
//! `php artisan migrate` builds the same schema by running upstream's migrations; here the dump of
//! the result is loaded. Creation only ever runs on a database with no tables. Anything else is
//! refused without changing it, so an existing Coolify database can never be overwritten.

use sqlx::{Connection, PgConnection};

use crate::{DbError, DbResult};

/// `pg_dump --schema-only` of upstream's schema at the compatibility baseline (Coolify v4.5.3).
pub const SCHEMA_SQL: &str = include_str!("../../../compat/baseline/upstream-4.5.3-schema.sql");
/// The `migrations` rows and sequence value at the compatibility baseline.
pub const MIGRATIONS_SQL: &str =
    include_str!("../../../compat/baseline/upstream-4.5.3-migrations.sql");

/// [`SCHEMA_SQL`] with its `CHECK ... IN` constraints as upstream's migrations write them.
///
/// The dump renders them as `ANY ((ARRAY['a'::character varying, ...])::text[])`. Postgres parses
/// that text into a different expression than `col IN ('a', ...)` on a `varchar` column, so loading
/// it as it is makes `pg_dump` of the result differ from upstream's. The rules are the same.
pub fn schema_sql() -> String {
    let check = regex::Regex::new(
        r"CHECK \(\(\((\w+)\)::text = ANY \(\(ARRAY\[((?:'[^']*'::character varying(?:, )?)+)\]\)::text\[\]\)\)\)",
    )
    .ok();
    let item = regex::Regex::new(r"'([^']*)'::character varying").ok();
    let (Some(check), Some(item)) = (check, item) else {
        return SCHEMA_SQL.to_owned();
    };
    check
        .replace_all(SCHEMA_SQL, |c: &regex::Captures<'_>| {
            let values: Vec<String> = item
                .captures_iter(&c[2])
                .map(|v| format!("'{}'", &v[1]))
                .collect();
            format!("CHECK (\"{}\" IN ({}))", &c[1], values.join(", "))
        })
        .into_owned()
}

/// Held for the length of the transaction: two starting containers must not both create the schema.
const LOCK_KEY: i64 = 0x0063_6f6f_6c69_6679;

/// Why a database has to be left alone, or that something failed while looking at it.
#[derive(Debug, thiserror::Error)]
pub enum BootstrapError {
    #[error(
        "refusing to create the schema: the database already holds {relations} tables or other objects. \
         Schema creation only runs on an empty database"
    )]
    NotEmpty { relations: i64 },
    #[error(transparent)]
    Database(#[from] sqlx::Error),
}

const USER_RELATIONS: &str = "SELECT count(*) FROM pg_catalog.pg_class c \
     JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
     WHERE n.nspname NOT IN ('pg_catalog', 'information_schema') \
       AND n.nspname NOT LIKE 'pg\\_toast%' AND n.nspname NOT LIKE 'pg\\_temp%' \
       AND c.relkind IN ('r', 'p', 'v', 'm', 'f', 'S')";

/// Tables, views and sequences outside the system schemas, in every schema.
pub async fn user_relations(conn: &mut PgConnection) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(USER_RELATIONS).fetch_one(conn).await
}

/// Creates the baseline schema and fills `migrations`. Refuses a database that has any table.
/// The check and the creation are one transaction: a failure leaves the database empty.
pub async fn create_schema(url: &str) -> Result<(), BootstrapError> {
    // The dump changes search_path for its session, so this connection is not reused.
    let mut conn = PgConnection::connect(url).await?;
    let mut tx = conn.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(LOCK_KEY)
        .execute(&mut *tx)
        .await?;
    let relations = user_relations(&mut tx).await?;
    if relations > 0 {
        tx.rollback().await?;
        return Err(BootstrapError::NotEmpty { relations });
    }
    sqlx::raw_sql(&schema_sql()).execute(&mut *tx).await?;
    sqlx::raw_sql(MIGRATIONS_SQL).execute(&mut *tx).await?;
    tx.commit().await?;
    conn.close().await.ok();
    Ok(())
}

/// What [`prepare`] found.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Prepared {
    /// The database was empty and now holds the baseline.
    Created,
    /// The database already had tables. They were not touched.
    Existing,
}

/// The startup step of an installed server: an empty database gets the baseline, any other is left
/// as it is and goes on to the schema guard of [`crate::Db::connect`].
pub async fn prepare(url: &str) -> DbResult<Prepared> {
    let mut conn = PgConnection::connect(url).await?;
    let relations = user_relations(&mut conn).await?;
    conn.close().await.ok();
    if relations > 0 {
        return Ok(Prepared::Existing);
    }
    match create_schema(url).await {
        Ok(()) => Ok(Prepared::Created),
        // Another container created it between the check and the lock.
        Err(BootstrapError::NotEmpty { .. }) => Ok(Prepared::Existing),
        Err(BootstrapError::Database(e)) => Err(DbError::from(e)),
    }
}

/// A schema change of a later coolify-rs release: a name for the `migrations` table and its SQL.
#[derive(Debug, Clone, Copy)]
pub struct Migration {
    pub name: &'static str,
    pub sql: &'static str,
}

/// Upstream's migrations since an earlier baseline, and schema changes of later coolify-rs
/// releases, oldest first. A release that changes the schema adds its entry here and its name to
/// `compat/baseline/supported-migrations.txt`. Upgrades apply them at startup, after `upgrade.sh`
/// has taken its backup, as `php artisan migrate` applies upstream's. Entries the `migrations`
/// table already lists (a database created at the current baseline) are skipped.
///
/// The first five bring a database created at upstream `main` @ b5823c7 (the baseline of earlier
/// coolify-rs builds) to v4.5.3. Their SQL is what Laravel's Postgres grammar runs for the PHP.
pub const MIGRATIONS: &[Migration] = &[
    // `$table->boolean('use_path_style_endpoint')->default(true)`
    Migration {
        name: "2026_10_27_142752_add_use_path_style_endpoint_to_s3_storages_table",
        sql: "ALTER TABLE public.s3_storages \
              ADD COLUMN use_path_style_endpoint boolean NOT NULL DEFAULT '1'",
    },
    // `$table->string('logdrain_axiom_domain')->nullable()->default('us-east-1.aws.edge.axiom.co')`
    Migration {
        name: "2026_10_27_155023_add_logdrain_axiom_domain_to_server_settings_table",
        sql: "ALTER TABLE public.server_settings \
              ADD COLUMN logdrain_axiom_domain varchar(255) NULL DEFAULT 'us-east-1.aws.edge.axiom.co'",
    },
    // Drops both columns when they exist. Its `StopLogDrain::dispatch` for the servers that had
    // the drain enabled uses [`highlight_log_drain_servers`], read before this runs.
    Migration {
        name: "2026_10_27_160258_remove_highlight_log_drain_from_server_settings_table",
        sql: "ALTER TABLE public.server_settings \
              DROP COLUMN IF EXISTS is_logdrain_highlight_enabled, \
              DROP COLUMN IF EXISTS logdrain_highlight_project_id",
    },
    // The column, then `DB::table('application_previews')->update(['keeps_production_values' => true])`:
    // previews from before the upgrade keep the production values they used.
    Migration {
        name: "2026_10_28_103619_add_keeps_production_values_to_application_previews_table",
        sql: "ALTER TABLE public.application_previews \
              ADD COLUMN keeps_production_values boolean NOT NULL DEFAULT '0'; \
              UPDATE public.application_previews SET keeps_production_values = true",
    },
    // A data change only; the query builder does not touch `updated_at`.
    Migration {
        name: "2026_10_28_124421_keep_dockerfile_healthcheck_for_existing_applications",
        sql: "UPDATE public.applications SET health_check_enabled = false \
              WHERE custom_healthcheck_found = true AND health_check_enabled = true",
    },
];

/// The servers `2026_10_27_160258_remove_highlight_log_drain_from_server_settings_table` stops the
/// log drain of (`StopLogDrain::dispatch`), read before [`MIGRATIONS`] drops the column: servers
/// with `is_logdrain_highlight_enabled` that are not transferred away (`isTransferredAway`).
/// Empty when the column does not exist (the migration ran, or the database is newer).
pub async fn highlight_log_drain_servers(url: &str) -> DbResult<Vec<i64>> {
    let mut conn = PgConnection::connect(url).await?;
    let has_column: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM information_schema.columns \
         WHERE table_schema = 'public' AND table_name = 'server_settings' \
           AND column_name = 'is_logdrain_highlight_enabled')",
    )
    .fetch_one(&mut conn)
    .await?;
    let ids = if has_column {
        sqlx::query_scalar(
            "SELECT s.id FROM public.servers s \
             JOIN public.server_settings ss ON ss.server_id = s.id \
             WHERE ss.is_logdrain_highlight_enabled = true \
               AND (s.server_metadata::jsonb #>> '{transfer,status}') IS DISTINCT FROM 'transferred' \
             ORDER BY s.id",
        )
        .fetch_all(&mut conn)
        .await?
    } else {
        Vec::new()
    };
    conn.close().await.ok();
    Ok(ids)
}

/// Runs the entries of `migrations` the `migrations` table does not list, each in a transaction
/// with its row, under one new batch number as `php artisan migrate` does. Returns the names run.
pub async fn apply_migrations(url: &str, migrations: &[Migration]) -> DbResult<Vec<String>> {
    let mut conn = PgConnection::connect(url).await?;
    sqlx::query("SELECT pg_advisory_lock($1)")
        .bind(LOCK_KEY)
        .execute(&mut conn)
        .await?;
    let batch: i32 = sqlx::query_scalar("SELECT COALESCE(MAX(batch), 0) + 1 FROM migrations")
        .fetch_one(&mut conn)
        .await?;
    let mut ran = Vec::new();
    for migration in migrations {
        let mut tx = conn.begin().await?;
        let known: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM migrations WHERE migration = $1)")
                .bind(migration.name)
                .fetch_one(&mut *tx)
                .await?;
        if known {
            tx.rollback().await?;
            continue;
        }
        sqlx::raw_sql(migration.sql).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO migrations (migration, batch) VALUES ($1, $2)")
            .bind(migration.name)
            .bind(batch)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        ran.push(migration.name.to_owned());
    }
    conn.close().await.ok();
    Ok(ran)
}
