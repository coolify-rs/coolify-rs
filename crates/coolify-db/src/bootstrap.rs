//! A new installation's database: the baseline schema and the upstream `migrations` rows.
//!
//! `php artisan migrate` builds the same schema by running upstream's migrations; here the dump of
//! the result is loaded. Creation only ever runs on a database with no tables. Anything else is
//! refused without changing it, so an existing Coolify database can never be overwritten.

use sqlx::{Connection, PgConnection};

use crate::{DbError, DbResult};

/// `pg_dump --schema-only` of upstream's schema at the compatibility baseline.
pub const SCHEMA_SQL: &str =
    include_str!("../../../compat/baseline/upstream-main-b5823c7-schema.sql");
/// The `migrations` rows and sequence value at the compatibility baseline.
pub const MIGRATIONS_SQL: &str =
    include_str!("../../../compat/baseline/upstream-main-b5823c7-migrations.sql");

/// Held for the length of the transaction: two starting containers must not both create the schema.
const LOCK_KEY: i64 = 0x636f_6f6c_6966_79;

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
    sqlx::raw_sql(SCHEMA_SQL).execute(&mut *tx).await?;
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

/// Schema changes after the baseline, oldest first. A release that changes the schema adds its
/// entry here and its name to `compat/baseline/supported-migrations.txt`. Upgrades apply them at
/// startup, after `upgrade.sh` has taken its backup.
pub const MIGRATIONS: &[Migration] = &[];

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
