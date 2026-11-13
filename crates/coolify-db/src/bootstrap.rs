//! A new installation's database: the baseline schema and the upstream `migrations` rows.
//!
//! `php artisan migrate` builds the same schema by running upstream's migrations; here the dump of
//! the result is loaded. Creation only ever runs on a database with no tables. Anything else is
//! refused without changing it, so an existing Coolify database can never be overwritten.

use sqlx::{Connection, PgConnection};

use crate::{DbError, DbResult};

/// `pg_dump --schema-only` of upstream's schema at Coolify v4.6.0. The compatibility baseline is
/// Coolify v4.7.5: this dump plus [`MIGRATIONS`], which `coolify-rs boot` and `create-schema`
/// apply after loading it (a v4.7.5 dump makes that a no-op once it is captured; see
/// docs/UPSTREAM_BASELINE.md).
pub const SCHEMA_SQL: &str = include_str!("../../../compat/baseline/upstream-4.6.0-schema.sql");
/// The `migrations` rows and sequence value of that dump.
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
/// coolify-rs builds), at Coolify v4.5.3 (coolify-rs 4.5.3-rs.N) or at Coolify v4.6.0 (coolify-rs
/// 4.6.0-rs.N, and what [`create_schema`] loads) to v4.7.5: the five migrations of v4.5.3, the
/// fifteen of v4.6.0 and the twenty-seven of v4.7.0 to v4.7.5. Their SQL is what Laravel's Postgres
/// grammar runs for the PHP.
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
    // v4.7.5. The five image scanning tables, the instance policy row
    // (`DB::table('image_scan_policies')->insert`, `uuid` = `new_public_id()`: 24 characters of
    // `Str::lower(Str::random())`, `now()` in UTC), then the columns on existing tables. Laravel
    // runs the commands of a blueprint in the order the closure adds them and the fluent
    // `->unique()` of a column last.
    Migration::new(
        "2026_10_29_100000_create_image_scanning_tables",
        r#"CREATE TABLE public.image_scan_policies (
    id bigserial NOT NULL PRIMARY KEY,
    uuid varchar(255) NOT NULL,
    scope_type varchar(255) NULL,
    scope_id bigint NULL,
    gate_mode varchar(255) NULL,
    on_scan_error varchar(255) NULL,
    count_medium boolean NULL,
    created_at timestamp(0) without time zone NULL,
    updated_at timestamp(0) without time zone NULL
);
ALTER TABLE public.image_scan_policies
    ADD CONSTRAINT image_scan_policies_scope_type_scope_id_unique UNIQUE (scope_type, scope_id);
ALTER TABLE public.image_scan_policies ADD CONSTRAINT image_scan_policies_uuid_unique UNIQUE (uuid);
INSERT INTO public.image_scan_policies
    (uuid, scope_type, scope_id, gate_mode, on_scan_error, count_medium, created_at, updated_at)
VALUES (
    (SELECT lower(string_agg(substr(
                'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789',
                (floor(random() * 62))::int + 1, 1), ''))
       FROM generate_series(1, 24)),
    NULL, NULL, 'off', 'deploy', false,
    date_trunc('second', now() AT TIME ZONE 'UTC'), date_trunc('second', now() AT TIME ZONE 'UTC'));
CREATE TABLE public.image_scan_ignores (
    id bigserial NOT NULL PRIMARY KEY,
    uuid varchar(255) NOT NULL,
    team_id bigint NOT NULL,
    resource_type varchar(255) NULL,
    resource_id bigint NULL,
    vulnerability_id varchar(64) NULL,
    package varchar(214) NULL,
    reason varchar(500) NOT NULL,
    expires_at timestamp(0) without time zone NULL,
    created_by bigint NULL,
    created_at timestamp(0) without time zone NULL,
    updated_at timestamp(0) without time zone NULL
);
ALTER TABLE public.image_scan_ignores
    ADD CONSTRAINT image_scan_ignores_team_id_foreign
    FOREIGN KEY (team_id) REFERENCES public.teams (id) ON DELETE CASCADE;
CREATE INDEX image_scan_ignores_resource_type_resource_id_index
    ON public.image_scan_ignores (resource_type, resource_id);
ALTER TABLE public.image_scan_ignores
    ADD CONSTRAINT image_scan_ignores_created_by_foreign
    FOREIGN KEY (created_by) REFERENCES public.users (id) ON DELETE SET NULL;
CREATE INDEX image_scan_ignores_team_id_resource_type_resource_id_index
    ON public.image_scan_ignores (team_id, resource_type, resource_id);
ALTER TABLE public.image_scan_ignores ADD CONSTRAINT image_scan_ignores_uuid_unique UNIQUE (uuid);
CREATE TABLE public.image_sboms (
    id bigserial NOT NULL PRIMARY KEY,
    server_id bigint NOT NULL,
    image_id varchar(80) NOT NULL,
    platform varchar(32) NOT NULL,
    packages jsonb NOT NULL,
    packages_truncated boolean NOT NULL DEFAULT '0',
    last_used_at timestamp(0) without time zone NOT NULL
);
ALTER TABLE public.image_sboms
    ADD CONSTRAINT image_sboms_server_id_foreign
    FOREIGN KEY (server_id) REFERENCES public.servers (id) ON DELETE CASCADE;
ALTER TABLE public.image_sboms
    ADD CONSTRAINT image_sboms_server_id_image_id_unique UNIQUE (server_id, image_id);
CREATE TABLE public.image_scans (
    id bigserial NOT NULL PRIMARY KEY,
    uuid varchar(255) NOT NULL,
    server_id bigint NOT NULL,
    image_sbom_id bigint NULL,
    image_id varchar(80) NOT NULL,
    repo_digest varchar(80) NULL,
    resource_type varchar(255) NOT NULL,
    resource_id bigint NOT NULL,
    image_ref varchar(512) NOT NULL,
    deployment_queue_id bigint NULL,
    status varchar(255) NOT NULL,
    error_code varchar(255) NULL,
    error_context jsonb NULL,
    summary jsonb NULL,
    findings jsonb NULL,
    candidates jsonb NULL,
    base_image_ref varchar(512) NULL,
    builds_image boolean NOT NULL DEFAULT '0',
    is_baseline boolean NOT NULL DEFAULT '1',
    grype_db_built_at timestamp(0) without time zone NULL,
    scanned_at timestamp(0) without time zone NULL,
    created_at timestamp(0) without time zone NULL,
    updated_at timestamp(0) without time zone NULL
);
ALTER TABLE public.image_scans
    ADD CONSTRAINT image_scans_server_id_foreign
    FOREIGN KEY (server_id) REFERENCES public.servers (id) ON DELETE CASCADE;
ALTER TABLE public.image_scans
    ADD CONSTRAINT image_scans_image_sbom_id_foreign
    FOREIGN KEY (image_sbom_id) REFERENCES public.image_sboms (id) ON DELETE SET NULL;
CREATE INDEX image_scans_resource_type_resource_id_index
    ON public.image_scans (resource_type, resource_id);
ALTER TABLE public.image_scans
    ADD CONSTRAINT image_scans_deployment_queue_id_foreign
    FOREIGN KEY (deployment_queue_id) REFERENCES public.application_deployment_queues (id) ON DELETE SET NULL;
ALTER TABLE public.image_scans
    ADD CONSTRAINT image_scans_resource_type_resource_id_image_id_unique
    UNIQUE (resource_type, resource_id, image_id);
CREATE INDEX image_scans_server_id_image_id_index ON public.image_scans (server_id, image_id);
ALTER TABLE public.image_scans ADD CONSTRAINT image_scans_uuid_unique UNIQUE (uuid);
CREATE TABLE public.image_scan_notification_states (
    id bigserial NOT NULL PRIMARY KEY,
    resource_type varchar(255) NOT NULL,
    resource_id bigint NOT NULL,
    team_id bigint NOT NULL,
    notified_ids jsonb NULL,
    notified_candidates jsonb NULL,
    digest_ids jsonb NULL,
    is_baseline boolean NOT NULL DEFAULT '1',
    check_reported boolean NOT NULL DEFAULT '0',
    held_digests smallint NOT NULL DEFAULT '0',
    baseline_deployment_queue_id bigint NULL,
    created_at timestamp(0) without time zone NULL,
    updated_at timestamp(0) without time zone NULL
);
ALTER TABLE public.image_scan_notification_states
    ADD CONSTRAINT image_scan_notification_states_team_id_foreign
    FOREIGN KEY (team_id) REFERENCES public.teams (id) ON DELETE CASCADE;
ALTER TABLE public.image_scan_notification_states
    ADD CONSTRAINT image_scan_notification_states_resource_type_resource_id_unique
    UNIQUE (resource_type, resource_id);
ALTER TABLE public.server_settings
    ADD COLUMN is_image_scan_enabled boolean NOT NULL DEFAULT '0',
    ADD COLUMN image_candidate_scans_today integer NOT NULL DEFAULT '0',
    ADD COLUMN image_candidate_scans_date date NULL;
ALTER TABLE public.teams ADD COLUMN scan_images_on_new_servers boolean NOT NULL DEFAULT '0';
ALTER TABLE public.application_settings
    ADD COLUMN is_image_scan_enabled boolean NOT NULL DEFAULT '1',
    ADD COLUMN image_builds_by_team boolean NULL;
ALTER TABLE public.applications ADD COLUMN docker_compose_image_overrides json NULL;
ALTER TABLE public.application_deployment_queues
    ADD COLUMN scan_gate_result jsonb NULL,
    ADD COLUMN scan_gate_bypassed_by bigint NULL;
ALTER TABLE public.application_deployment_queues
    ADD CONSTRAINT application_deployment_queues_scan_gate_bypassed_by_foreign
    FOREIGN KEY (scan_gate_bypassed_by) REFERENCES public.users (id) ON DELETE SET NULL;
ALTER TABLE public.email_notification_settings
    ADD COLUMN image_vulnerabilities_email_notifications boolean NOT NULL DEFAULT '1';
ALTER TABLE public.discord_notification_settings
    ADD COLUMN image_vulnerabilities_discord_notifications boolean NOT NULL DEFAULT '0';
ALTER TABLE public.telegram_notification_settings
    ADD COLUMN image_vulnerabilities_telegram_notifications boolean NOT NULL DEFAULT '0';
ALTER TABLE public.slack_notification_settings
    ADD COLUMN image_vulnerabilities_slack_notifications boolean NOT NULL DEFAULT '0';
ALTER TABLE public.pushover_notification_settings
    ADD COLUMN image_vulnerabilities_pushover_notifications boolean NOT NULL DEFAULT '0';
ALTER TABLE public.webhook_notification_settings
    ADD COLUMN image_vulnerabilities_webhook_notifications boolean NOT NULL DEFAULT '0';
ALTER TABLE public.telegram_notification_settings
    ADD COLUMN telegram_notifications_image_vulnerabilities_thread_id text NULL"#,
    ),
    // v4.6.0. `$table->timestamp('stripe_checkout_reminder_sent_at')->nullable()` (a Coolify
    // Cloud column; coolify-rs never writes it).
    Migration::new(
        "2026_10_29_221743_add_checkout_reminder_sent_at_to_subscriptions_table",
        "ALTER TABLE public.subscriptions \
         ADD COLUMN stripe_checkout_reminder_sent_at timestamp(0) without time zone NULL",
    ),
    // v4.7.5. `$table->text('git_ssh_known_hosts')->nullable()`. It shares its timestamp with the
    // next one; Laravel sorts pending migrations by file name.
    Migration::new(
        "2026_10_30_120000_add_git_ssh_known_hosts_to_applications_table",
        "ALTER TABLE public.applications ADD COLUMN git_ssh_known_hosts text NULL",
    ),
    // v4.7.5. `ssh_host_key` text, `ssh_host_key_source` string, `ssh_host_key_trusted_at`
    // timestamp, `ssh_host_key_pending` text (all nullable),
    // `$table->boolean('is_ssh_host_key_check_disabled')->default(false)`.
    Migration::new(
        "2026_10_30_120000_add_ssh_host_key_to_server_settings_table",
        "ALTER TABLE public.server_settings \
         ADD COLUMN ssh_host_key text NULL, \
         ADD COLUMN ssh_host_key_source varchar(255) NULL, \
         ADD COLUMN ssh_host_key_trusted_at timestamp(0) without time zone NULL, \
         ADD COLUMN ssh_host_key_pending text NULL, \
         ADD COLUMN is_ssh_host_key_check_disabled boolean NOT NULL DEFAULT '0'",
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
    // v4.7.5. `$table->string('type')->default('s3')`, `$table->longText('bucket')->nullable()->change()`
    // (`compileChange`: the type, `drop not null`, `drop default`), then
    // `$table->foreignId('private_key_id')->nullable()->constrained('private_keys')->nullOnDelete()`.
    Migration::new(
        "2026_10_31_161621_add_type_to_s3_storages_table",
        "ALTER TABLE public.s3_storages ADD COLUMN type varchar(255) NOT NULL DEFAULT 's3'; \
         ALTER TABLE public.s3_storages \
         ALTER COLUMN bucket TYPE text, \
         ALTER COLUMN bucket DROP NOT NULL, \
         ALTER COLUMN bucket DROP DEFAULT; \
         ALTER TABLE public.s3_storages ADD COLUMN private_key_id bigint NULL; \
         ALTER TABLE public.s3_storages \
         ADD CONSTRAINT s3_storages_private_key_id_foreign \
         FOREIGN KEY (private_key_id) REFERENCES public.private_keys (id) ON DELETE SET NULL",
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
    // v4.7.5. `$table->text('sftp_host_key')->nullable()`
    Migration::new(
        "2026_11_01_170003_add_sftp_host_key_to_s3_storages_table",
        "ALTER TABLE public.s3_storages ADD COLUMN sftp_host_key text NULL",
    ),
    // v4.7.5. Two booleans on `application_settings` (queued: default true, building: default
    // false) and `$table->timestamp('deploy_started_at')->nullable()`, each unless the column exists.
    Migration::new(
        "2026_11_03_072701_add_auto_cancel_superseded_deployments",
        "ALTER TABLE public.application_settings \
         ADD COLUMN IF NOT EXISTS is_auto_cancel_queued_deployments_enabled boolean NOT NULL DEFAULT '1', \
         ADD COLUMN IF NOT EXISTS is_auto_cancel_building_deployments_enabled boolean NOT NULL DEFAULT '0'; \
         ALTER TABLE public.application_deployment_queues \
         ADD COLUMN IF NOT EXISTS deploy_started_at timestamp(0) without time zone NULL",
    ),
    // v4.7.5. `$table->boolean('is_mcp')->default(false)` unless the column exists.
    Migration::new(
        "2026_11_03_120133_add_is_mcp_to_application_deployment_queues_table",
        "ALTER TABLE public.application_deployment_queues \
         ADD COLUMN IF NOT EXISTS is_mcp boolean NOT NULL DEFAULT '0'",
    ),
    // v4.7.5. `$table->boolean('skip_tls_verification')->default(false)`
    Migration::new(
        "2026_11_03_171936_add_skip_tls_verification_to_s3_storages_table",
        "ALTER TABLE public.s3_storages ADD COLUMN skip_tls_verification boolean NOT NULL DEFAULT '0'",
    ),
    // v4.7.5. `$table->boolean('is_removed_from_compose')->default(false)` on both tables.
    Migration::new(
        "2026_11_03_192547_add_is_removed_from_compose_to_service_parts",
        "ALTER TABLE public.service_applications \
         ADD COLUMN is_removed_from_compose boolean NOT NULL DEFAULT '0'; \
         ALTER TABLE public.service_databases \
         ADD COLUMN is_removed_from_compose boolean NOT NULL DEFAULT '0'",
    ),
    // v4.7.5. `consecutive_failures` (unsigned integer, default 0), `failing_since` (timestamp) and
    // `disabled_reason` (text), each unless the column exists. Then `setExecutionStatuses()`, its
    // `pgsql` branch as it is: executions may be `skipped`.
    Migration::new(
        "2026_11_03_192735_add_failure_tracking_to_scheduled_tasks_table",
        "ALTER TABLE public.scheduled_tasks \
         ADD COLUMN IF NOT EXISTS consecutive_failures integer NOT NULL DEFAULT '0', \
         ADD COLUMN IF NOT EXISTS failing_since timestamp(0) without time zone NULL, \
         ADD COLUMN IF NOT EXISTS disabled_reason text NULL; \
         ALTER TABLE public.scheduled_task_executions \
         DROP CONSTRAINT IF EXISTS scheduled_task_executions_status_check; \
         ALTER TABLE public.scheduled_task_executions \
         ADD CONSTRAINT scheduled_task_executions_status_check \
         CHECK (status IN ('success', 'failed', 'running', 'skipped'))",
    ),
    // v4.7.5. `$table->text('notification_from_address')->nullable()`,
    // `$table->text('notification_from_name')->nullable()`
    Migration::new(
        "2026_11_03_194650_add_notification_sender_to_instance_settings_table",
        "ALTER TABLE public.instance_settings \
         ADD COLUMN notification_from_address text NULL, \
         ADD COLUMN notification_from_name text NULL",
    ),
    // v4.7.5. `$table->string('deployments')->nullable()->after('pull_requests')` (Postgres has no
    // `after`: the column is last), then `github_deployment_id` (unsigned big integer) and
    // `github_deployment_state` (string), both nullable.
    Migration::new(
        "2026_11_03_195902_add_github_deployments_columns",
        "ALTER TABLE public.github_apps ADD COLUMN deployments varchar(255) NULL; \
         ALTER TABLE public.application_deployment_queues \
         ADD COLUMN github_deployment_id bigint NULL, \
         ADD COLUMN github_deployment_state varchar(255) NULL",
    ),
    // v4.7.5. `$table->boolean('use_shell')->default(true)`
    Migration::new(
        "2026_11_03_202635_add_use_shell_to_scheduled_tasks_table",
        "ALTER TABLE public.scheduled_tasks ADD COLUMN use_shell boolean NOT NULL DEFAULT '1'",
    ),
    // v4.7.5. `smtp_server_name` (string) and `smtp_servers` (text), both nullable, on each of the
    // two tables that does not have `smtp_servers` yet.
    Migration::new(
        "2026_11_03_225251_add_named_smtp_servers_to_email_settings",
        r#"DO $$
DECLARE
    t text;
BEGIN
    FOREACH t IN ARRAY ARRAY['instance_settings', 'email_notification_settings'] LOOP
        IF EXISTS (SELECT 1 FROM information_schema.columns
                    WHERE table_schema = 'public' AND table_name = t AND column_name = 'smtp_servers') THEN
            CONTINUE;
        END IF;
        EXECUTE format(
            'ALTER TABLE public.%I ADD COLUMN smtp_server_name varchar(255) NULL, ADD COLUMN smtp_servers text NULL',
            t);
    END LOOP;
END
$$"#,
    ),
    // v4.7.5. `$table->string('auto_update_target_version')->nullable()`,
    // `$table->timestamp('auto_update_started_at')->nullable()`
    Migration::new(
        "2026_11_04_080000_add_auto_update_attempt_to_instance_settings_table",
        "ALTER TABLE public.instance_settings \
         ADD COLUMN auto_update_target_version varchar(255) NULL, \
         ADD COLUMN auto_update_started_at timestamp(0) without time zone NULL",
    ),
    // v4.7.5. Resend gets its own sender: `resend_from_address` and `resend_from_name` (nullable
    // text) on each of the two tables that does not have them yet, filled with a copy of the SMTP
    // sender (`DB::raw('smtp_from_address')`: the stored, encrypted values as they are). The query
    // builder does not touch `updated_at`.
    Migration::new(
        "2026_11_04_103949_add_resend_sender_to_email_settings",
        r#"DO $$
DECLARE
    t text;
BEGIN
    FOREACH t IN ARRAY ARRAY['instance_settings', 'email_notification_settings'] LOOP
        IF EXISTS (SELECT 1 FROM information_schema.columns
                    WHERE table_schema = 'public' AND table_name = t AND column_name = 'resend_from_address') THEN
            CONTINUE;
        END IF;
        EXECUTE format(
            'ALTER TABLE public.%I ADD COLUMN resend_from_address text NULL, ADD COLUMN resend_from_name text NULL',
            t);
        EXECUTE format(
            'UPDATE public.%I SET resend_from_address = smtp_from_address, resend_from_name = smtp_from_name',
            t);
    END LOOP;
END
$$"#,
    ),
    // v4.7.5. `$table->boolean('include_network_hosts_in_build')->default(true)` unless the column
    // exists (`after()` does nothing on Postgres).
    Migration::new(
        "2026_11_05_163404_add_include_network_hosts_in_build_to_application_settings",
        "ALTER TABLE public.application_settings \
         ADD COLUMN IF NOT EXISTS include_network_hosts_in_build boolean NOT NULL DEFAULT '1'",
    ),
    // v4.7.5. `dropIndex('audit_events_level_index')` and
    // `dropIndex(['team_id', 'resource_type', 'resource_uuid', 'created_at'])`, each when
    // `Schema::hasIndex` finds it. Laravel's name for the second one has 65 characters; Postgres
    // keeps 63.
    Migration::new(
        "2026_11_05_224732_drop_unused_indexes_from_audit_events_table",
        "DROP INDEX IF EXISTS public.audit_events_level_index; \
         DROP INDEX IF EXISTS public.audit_events_team_id_resource_type_resource_uuid_created_at_ind",
    ),
    // v4.7.5. `$blueprint->string('image')->default($image['new'])->change()` on six tables
    // (`compileChange`: the type, `set not null`, `set default`). Existing databases keep their
    // image; only new ones get the new default.
    Migration::new(
        "2026_11_06_083240_update_standalone_database_default_images",
        "ALTER TABLE public.standalone_postgresqls \
         ALTER COLUMN image TYPE varchar(255), \
         ALTER COLUMN image SET NOT NULL, \
         ALTER COLUMN image SET DEFAULT 'postgres:18-alpine'; \
         ALTER TABLE public.standalone_redis \
         ALTER COLUMN image TYPE varchar(255), \
         ALTER COLUMN image SET NOT NULL, \
         ALTER COLUMN image SET DEFAULT 'redis:8'; \
         ALTER TABLE public.standalone_mongodbs \
         ALTER COLUMN image TYPE varchar(255), \
         ALTER COLUMN image SET NOT NULL, \
         ALTER COLUMN image SET DEFAULT 'mongo:9'; \
         ALTER TABLE public.standalone_mysqls \
         ALTER COLUMN image TYPE varchar(255), \
         ALTER COLUMN image SET NOT NULL, \
         ALTER COLUMN image SET DEFAULT 'mysql:9.7'; \
         ALTER TABLE public.standalone_mariadbs \
         ALTER COLUMN image TYPE varchar(255), \
         ALTER COLUMN image SET NOT NULL, \
         ALTER COLUMN image SET DEFAULT 'mariadb:12.3'; \
         ALTER TABLE public.standalone_clickhouses \
         ALTER COLUMN image TYPE varchar(255), \
         ALTER COLUMN image SET NOT NULL, \
         ALTER COLUMN image SET DEFAULT 'clickhouse/clickhouse-server:26.8'",
    ),
    // v4.7.5. `$table->string('image')->default('valkey/valkey:9-alpine')->change()`: new Redis
    // databases are Valkey.
    Migration::new(
        "2026_11_06_113843_set_valkey_as_standalone_redis_default_image",
        "ALTER TABLE public.standalone_redis \
         ALTER COLUMN image TYPE varchar(255), \
         ALTER COLUMN image SET NOT NULL, \
         ALTER COLUMN image SET DEFAULT 'valkey/valkey:9-alpine'",
    ),
    // v4.7.5. One row per server: `$table->foreignId('server_id')->unique()->constrained()->cascadeOnDelete()`.
    Migration::new(
        "2026_11_06_145819_create_server_host_scans_table",
        "CREATE TABLE public.server_host_scans (\
         id bigserial NOT NULL PRIMARY KEY, \
         server_id bigint NOT NULL, \
         error_code varchar(255) NULL, \
         error_context jsonb NULL, \
         eol jsonb NULL, \
         summary jsonb NULL, \
         findings jsonb NULL, \
         findings_truncated boolean NOT NULL DEFAULT '0', \
         grype_db_built_at timestamp(0) without time zone NULL, \
         scanned_at timestamp(0) without time zone NULL, \
         created_at timestamp(0) without time zone NULL, \
         updated_at timestamp(0) without time zone NULL); \
         ALTER TABLE public.server_host_scans \
         ADD CONSTRAINT server_host_scans_server_id_foreign \
         FOREIGN KEY (server_id) REFERENCES public.servers (id) ON DELETE CASCADE; \
         ALTER TABLE public.server_host_scans \
         ADD CONSTRAINT server_host_scans_server_id_unique UNIQUE (server_id)",
    ),
    // v4.7.5. `$table->morphs('starrable')` (two columns and their index),
    // `$table->unsignedInteger('position')->default(0)`, one star per user and resource.
    Migration::new(
        "2026_11_06_165709_create_stars_table",
        "CREATE TABLE public.stars (\
         id bigserial NOT NULL PRIMARY KEY, \
         user_id bigint NOT NULL, \
         starrable_type varchar(255) NOT NULL, \
         starrable_id bigint NOT NULL, \
         \"position\" integer NOT NULL DEFAULT '0', \
         created_at timestamp(0) without time zone NULL, \
         updated_at timestamp(0) without time zone NULL); \
         ALTER TABLE public.stars \
         ADD CONSTRAINT stars_user_id_foreign \
         FOREIGN KEY (user_id) REFERENCES public.users (id) ON DELETE CASCADE; \
         CREATE INDEX stars_starrable_type_starrable_id_index \
         ON public.stars (starrable_type, starrable_id); \
         ALTER TABLE public.stars \
         ADD CONSTRAINT stars_user_id_starrable_type_starrable_id_unique \
         UNIQUE (user_id, starrable_type, starrable_id)",
    ),
    // v4.7.5. `$table->boolean('is_ssh_host_key_check_enforced')->default(false)`
    Migration::new(
        "2026_11_06_194613_add_ssh_host_key_check_enforced_to_server_settings_table",
        "ALTER TABLE public.server_settings \
         ADD COLUMN is_ssh_host_key_check_enforced boolean NOT NULL DEFAULT '0'",
    ),
    // v4.7.5. Columns only nightly builds of the image scanning migration created.
    Migration::new(
        "2026_11_08_120000_drop_image_scanning_columns_from_instance_settings",
        "ALTER TABLE public.instance_settings \
         DROP COLUMN IF EXISTS is_image_scanning_allowed, \
         DROP COLUMN IF EXISTS image_scan_db_url",
    ),
    // v4.7.5. Columns only an earlier version of task auto-disable created.
    Migration::new(
        "2026_11_09_120000_drop_unused_failure_columns_from_scheduled_tasks",
        "ALTER TABLE public.scheduled_tasks \
         DROP COLUMN IF EXISTS failure_count, \
         DROP COLUMN IF EXISTS auto_disabled_at",
    ),
    // v4.7.5. `$table->unsignedInteger('image_scan_memory_mb')->default(1024)`
    Migration::new(
        "2026_11_09_130000_add_image_scan_memory_mb_to_server_settings_table",
        "ALTER TABLE public.server_settings \
         ADD COLUMN image_scan_memory_mb integer NOT NULL DEFAULT '1024'",
    ),
    // v4.7.5. Two unsigned integers (default 600) and `$table->decimal('image_scan_cpus', 5, 2)->default(1)`.
    Migration::new(
        "2026_11_09_140000_add_image_scanner_time_and_cpu_limits_to_server_settings_table",
        "ALTER TABLE public.server_settings \
         ADD COLUMN image_scan_syft_timeout integer NOT NULL DEFAULT '600', \
         ADD COLUMN image_scan_grype_timeout integer NOT NULL DEFAULT '600', \
         ADD COLUMN image_scan_cpus decimal(5, 2) NOT NULL DEFAULT '1'",
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
