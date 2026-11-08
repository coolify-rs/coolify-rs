//! A new installation's database: the baseline schema and the upstream `migrations` rows.
//!
//! `php artisan migrate` builds the same schema by running upstream's migrations; here the dump of
//! the result is loaded. Creation only ever runs on a database with no tables. Anything else is
//! refused without changing it, so an existing Coolify database can never be overwritten.

use sqlx::{Connection, PgConnection};

use crate::{DbError, DbResult};

/// `pg_dump --schema-only` of upstream's schema at the compatibility baseline (Coolify v4.6.0).
pub const SCHEMA_SQL: &str = include_str!("../../../compat/baseline/upstream-4.6.0-schema.sql");
/// The `migrations` rows and sequence value at the compatibility baseline.
pub const MIGRATIONS_SQL: &str =
    include_str!("../../../compat/baseline/upstream-4.6.0-migrations.sql");

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
    /// Laravel's `$withinTransaction`. When false the statements of `sql` (separated by `;`) run
    /// one by one outside a transaction, as `CREATE INDEX CONCURRENTLY` requires, and the
    /// `migrations` row is written after them.
    pub transaction: bool,
}

impl Migration {
    /// A migration that runs in a transaction with its `migrations` row (Laravel's default).
    pub const fn new(name: &'static str, sql: &'static str) -> Self {
        Self {
            name,
            sql,
            transaction: true,
        }
    }

    /// A migration with `public $withinTransaction = false`.
    pub const fn without_transaction(name: &'static str, sql: &'static str) -> Self {
        Self {
            name,
            sql,
            transaction: false,
        }
    }
}

/// Upstream's migrations since an earlier baseline, and schema changes of later coolify-rs
/// releases, in name order, the order in which `php artisan migrate` runs pending migrations. A
/// release that changes the schema adds its entry here and its name to
/// `compat/baseline/supported-migrations.txt`. Upgrades apply them at startup, after `upgrade.sh`
/// has taken its backup. Entries the `migrations` table already lists (a database created at the
/// current baseline) are skipped.
///
/// They bring a database created at upstream `main` @ b5823c7 (the baseline of the first
/// coolify-rs builds) or at Coolify v4.5.3 (coolify-rs 4.5.3-rs.N) to v4.6.0: the five migrations
/// of v4.5.3 and the fifteen of v4.6.0. Their SQL is what Laravel's Postgres grammar runs for the
/// PHP.
pub const MIGRATIONS: &[Migration] = &[
    // v4.6.0. `$table->json('access')->nullable()` and `$table->json('scopes')->nullable()`, each
    // unless the column exists. Then delete becomes its own ability: every token whose `abilities`
    // (a JSON array) has `write` and not `delete` gets `delete` appended (`json_decode`,
    // `$abilities[] = 'delete'`, `json_encode`: compact, `/` escaped). The query builder does not
    // touch `updated_at`.
    Migration::new(
        "2026_10_25_100000_add_access_grants_to_team_members_and_tokens",
        r#"ALTER TABLE public.team_user ADD COLUMN IF NOT EXISTS access json NULL;
ALTER TABLE public.personal_access_tokens ADD COLUMN IF NOT EXISTS scopes json NULL;
DO $$
DECLARE
    r record;
    a jsonb;
BEGIN
    FOR r IN SELECT id, abilities FROM public.personal_access_tokens ORDER BY id LOOP
        BEGIN
            a := COALESCE(r.abilities, '[]')::jsonb;
        EXCEPTION WHEN others THEN
            CONTINUE;
        END;
        IF jsonb_typeof(a) = 'array' AND a ? 'write' AND NOT a ? 'delete' THEN
            UPDATE public.personal_access_tokens
               SET abilities = (
                   SELECT '[' || string_agg(replace(e::text, '/', '\/'), ',' ORDER BY i) || ',"delete"]'
                     FROM jsonb_array_elements(a) WITH ORDINALITY AS t(e, i))
             WHERE id = r.id;
        END IF;
    END LOOP;
END
$$"#,
    ),
    // v4.6.0. `$table->string('git_https_username')->nullable()`,
    // `$table->text('git_https_password')->nullable()`
    Migration::new(
        "2026_10_26_081954_add_git_https_credentials_to_applications_table",
        "ALTER TABLE public.applications \
         ADD COLUMN git_https_username varchar(255) NULL, \
         ADD COLUMN git_https_password text NULL",
    ),
    // v4.6.0. `$table->timestamp('paused_at')->nullable()` unless the column exists.
    Migration::new(
        "2026_10_27_142736_add_paused_at_to_personal_access_tokens_table",
        "ALTER TABLE public.personal_access_tokens \
         ADD COLUMN IF NOT EXISTS paused_at timestamp(0) without time zone NULL",
    ),
    // v4.5.3. `$table->boolean('use_path_style_endpoint')->default(true)`
    Migration::new(
        "2026_10_27_142752_add_use_path_style_endpoint_to_s3_storages_table",
        "ALTER TABLE public.s3_storages \
         ADD COLUMN use_path_style_endpoint boolean NOT NULL DEFAULT '1'",
    ),
    // v4.5.3. `$table->string('logdrain_axiom_domain')->nullable()->default('us-east-1.aws.edge.axiom.co')`
    Migration::new(
        "2026_10_27_155023_add_logdrain_axiom_domain_to_server_settings_table",
        "ALTER TABLE public.server_settings \
         ADD COLUMN logdrain_axiom_domain varchar(255) NULL DEFAULT 'us-east-1.aws.edge.axiom.co'",
    ),
    // v4.5.3. Drops both columns when they exist. Its `StopLogDrain::dispatch` for the servers
    // that had the drain enabled uses [`highlight_log_drain_servers`], read before this runs.
    Migration::new(
        "2026_10_27_160258_remove_highlight_log_drain_from_server_settings_table",
        "ALTER TABLE public.server_settings \
         DROP COLUMN IF EXISTS is_logdrain_highlight_enabled, \
         DROP COLUMN IF EXISTS logdrain_highlight_project_id",
    ),
    // v4.5.3. The column, then `DB::table('application_previews')->update(['keeps_production_values' => true])`:
    // previews from before the upgrade keep the production values they used.
    Migration::new(
        "2026_10_28_103619_add_keeps_production_values_to_application_previews_table",
        "ALTER TABLE public.application_previews \
         ADD COLUMN keeps_production_values boolean NOT NULL DEFAULT '0'; \
         UPDATE public.application_previews SET keeps_production_values = true",
    ),
    // v4.5.3. A data change only; the query builder does not touch `updated_at`.
    Migration::new(
        "2026_10_28_124421_keep_dockerfile_healthcheck_for_existing_applications",
        "UPDATE public.applications SET health_check_enabled = false \
         WHERE custom_healthcheck_found = true AND health_check_enabled = true",
    ),
    // v4.6.0. `$table->string('worker_host')->nullable()` on each of the three tables.
    Migration::new(
        "2026_10_28_204953_add_worker_host_to_scheduled_job_executions",
        "ALTER TABLE public.scheduled_task_executions ADD COLUMN worker_host varchar(255) NULL; \
         ALTER TABLE public.scheduled_database_backup_executions ADD COLUMN worker_host varchar(255) NULL; \
         ALTER TABLE public.scheduled_volume_backup_executions ADD COLUMN worker_host varchar(255) NULL",
    ),
    // v4.6.0, the seven index migrations: their `pgsql` branch as it is, outside a transaction.
    Migration::without_transaction(
        "2026_10_28_211105_add_indexes_for_team_deployments_and_docker_cleanup_history",
        "CREATE INDEX CONCURRENTLY IF NOT EXISTS idx_applications_environment_id_all \
         ON public.applications (environment_id); \
         CREATE INDEX CONCURRENTLY IF NOT EXISTS idx_docker_cleanup_executions_server_created_at \
         ON public.docker_cleanup_executions (server_id, created_at DESC)",
    ),
    Migration::without_transaction(
        "2026_10_28_212642_add_indexes_for_server_settings_and_team_membership_lookups",
        "CREATE INDEX CONCURRENTLY IF NOT EXISTS idx_server_settings_server_id \
         ON public.server_settings (server_id); \
         CREATE INDEX CONCURRENTLY IF NOT EXISTS idx_team_user_user_id ON public.team_user (user_id)",
    ),
    Migration::without_transaction(
        "2026_10_28_214500_add_index_for_application_deployment_history",
        "CREATE INDEX CONCURRENTLY IF NOT EXISTS idx_deployment_queues_app_created_id \
         ON public.application_deployment_queues (application_id, created_at DESC, id DESC)",
    ),
    Migration::without_transaction(
        "2026_10_28_214953_add_index_for_interrupted_docker_cleanup_executions",
        "CREATE INDEX CONCURRENTLY IF NOT EXISTS idx_docker_cleanup_executions_unfinished_status_created \
         ON public.docker_cleanup_executions (status, created_at) WHERE finished_at IS NULL",
    ),
    Migration::without_transaction(
        "2026_10_28_224219_add_index_for_active_github_runner_executions",
        "CREATE INDEX CONCURRENTLY IF NOT EXISTS idx_github_runner_executions_status_server_id \
         ON public.github_runner_executions (status, server_id)",
    ),
    Migration::without_transaction(
        "2026_10_28_224423_add_index_for_dashboard_recent_deployments",
        "CREATE INDEX CONCURRENTLY IF NOT EXISTS idx_deployment_queues_app_id \
         ON public.application_deployment_queues (application_id, id)",
    ),
    Migration::without_transaction(
        "2026_10_28_232000_add_index_for_active_servers_by_private_key",
        "CREATE INDEX CONCURRENTLY IF NOT EXISTS idx_servers_private_key_id_active \
         ON public.servers (private_key_id) WHERE deleted_at IS NULL",
    ),
    // v4.6.0. `$table->timestamp('stripe_checkout_reminder_sent_at')->nullable()` (a Coolify
    // Cloud column; coolify-rs never writes it).
    Migration::new(
        "2026_10_29_221743_add_checkout_reminder_sent_at_to_subscriptions_table",
        "ALTER TABLE public.subscriptions \
         ADD COLUMN stripe_checkout_reminder_sent_at timestamp(0) without time zone NULL",
    ),
    // v4.6.0. `$table->json('access')->nullable()` unless the column exists.
    Migration::new(
        "2026_10_30_144551_add_access_to_team_invitations_table",
        "ALTER TABLE public.team_invitations ADD COLUMN IF NOT EXISTS access json NULL",
    ),
    // v4.6.0. `$table->string('image')->change()`: Laravel's `compileChange` restates the type,
    // `set not null` (not nullable) and `drop default` (no default).
    Migration::new(
        "2026_10_30_171359_drop_image_default_from_standalone_sqlites_table",
        "ALTER TABLE public.standalone_sqlites \
         ALTER COLUMN image TYPE varchar(255), \
         ALTER COLUMN image SET NOT NULL, \
         ALTER COLUMN image DROP DEFAULT",
    ),
    // v4.6.0. `$table->string('update_channel')->default('stable')`, then `update_custom_tag` and
    // `previous_update_channel` as nullable strings.
    Migration::new(
        "2026_11_01_142702_add_update_channel_to_instance_settings_table",
        "ALTER TABLE public.instance_settings \
         ADD COLUMN update_channel varchar(255) NOT NULL DEFAULT 'stable', \
         ADD COLUMN update_custom_tag varchar(255) NULL, \
         ADD COLUMN previous_update_channel varchar(255) NULL",
    ),
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
        if migration.transaction {
            let mut tx = conn.begin().await?;
            if is_listed(&mut tx, migration.name).await? {
                tx.rollback().await?;
                continue;
            }
            sqlx::raw_sql(migration.sql).execute(&mut *tx).await?;
            log_migration(&mut tx, migration.name, batch).await?;
            tx.commit().await?;
        } else {
            if is_listed(&mut conn, migration.name).await? {
                continue;
            }
            // One statement per round trip: several in one query string are an implicit
            // transaction, which `CREATE INDEX CONCURRENTLY` refuses.
            for statement in migration
                .sql
                .split(';')
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                sqlx::raw_sql(statement).execute(&mut conn).await?;
            }
            log_migration(&mut conn, migration.name, batch).await?;
        }
        ran.push(migration.name.to_owned());
    }
    conn.close().await.ok();
    Ok(ran)
}

async fn is_listed(conn: &mut PgConnection, name: &str) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM migrations WHERE migration = $1)")
        .bind(name)
        .fetch_one(conn)
        .await
}

async fn log_migration(conn: &mut PgConnection, name: &str, batch: i32) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO migrations (migration, batch) VALUES ($1, $2)")
        .bind(name)
        .bind(batch)
        .execute(conn)
        .await
        .map(|_| ())
}
