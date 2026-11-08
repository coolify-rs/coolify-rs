# Database Compatibility

coolify-rs **adopts the upstream PostgreSQL schema unchanged** (ADR 0004). It never alters upstream tables on its
own: a new installation gets the dump of upstream's schema, and `coolify-rs boot` runs the DDL of upstream's own
migrations (translated from the PHP) to bring a database created by an earlier coolify-rs build, or by an older
Coolify, to the current baseline. The baseline schemas:

- `compat/baseline/upstream-4.6.0-schema.sql` + `-migrations.sql`: Coolify v4.6.0 (493 migrations) **current
  baseline**. Not captured from the official image (the reference stack was not used): it is the v4.5.3 dump below,
  loaded on Postgres 15.19, with the fifteen v4.6.0 migrations of `bootstrap::MIGRATIONS` applied, then `pg_dump -s`;
  the `migrations` rows are renumbered in name order in batch 1, as a fresh `php artisan migrate` writes them.
  `diff` against the v4.5.3 dump shows only the columns and indexes of the table below. Capturing it from
  `coollabsio/coolify:4.6.0` (the v4.5.3 procedure in UPSTREAM_BASELINE.md) and comparing is a pending check.
- `compat/baseline/upstream-4.5.3-schema.sql` + `-migrations.sql`: fresh install of `coollabsio/coolify:4.5.3`
  (478 migrations; `php artisan migrate --force` on an empty Postgres 15.19, captured 2026-11-06) [runtime]: the
  schema coolify-rs 4.5.3-rs.N created; used to test the upgrade to v4.6.0
- `compat/baseline/upstream-main-b5823c7-schema.sql` + `-migrations.sql`: fresh install of
  `coollabsio/coolify:sha-b5823c7` (473 migrations) [runtime]: the schema the first coolify-rs builds created; used
  to test the upgrade to v4.6.0
- `compat/baseline/upstream-4.5.0-schema.sql` + `-migrations.sql`: 4.5.0 install (83 tables, 56 FKs, 469 migrations)
  [runtime]: older baseline, used to test that older databases are refused for writes

Upgrading the 4.5.0 reference in place to `sha-b5823c7` produced the same schema as the fresh install except for
the literal timestamp defaults upstream bakes in at migration time (`servers.sentinel_updated_at`,
`*.last_online_at`). The same holds for a `sha-b5823c7` database after the v4.5.3 migrations of
`bootstrap::MIGRATIONS` (`pg_dump -s` compared). The tests `a_b5823c7_database_upgrades_to_4_6_0` and
`a_4_5_3_database_upgrades_to_4_6_0` compare the catalog after `boot` with the v4.6.0 baseline.

### v4.6.0 migrations (v4.5.3 → v4.6.0)

| Migration | Change |
|---|---|
| `2026_10_25_100000_add_access_grants_to_team_members_and_tokens` | `team_user.access json`, `personal_access_tokens.scopes json`; tokens with `write` and not `delete` get `delete` appended to `abilities` |
| `2026_10_26_081954_add_git_https_credentials_to_applications_table` | `applications.git_https_username varchar(255)`, `git_https_password text` |
| `2026_10_27_142736_add_paused_at_to_personal_access_tokens_table` | `personal_access_tokens.paused_at timestamp(0)` |
| `2026_10_28_204953_add_worker_host_to_scheduled_job_executions` | `worker_host varchar(255)` on the three scheduled execution tables |
| `2026_10_28_211105` … `2026_10_28_232000` (seven) | indexes `idx_applications_environment_id_all`, `idx_docker_cleanup_executions_server_created_at`, `idx_server_settings_server_id`, `idx_team_user_user_id`, `idx_deployment_queues_app_created_id`, `idx_docker_cleanup_executions_unfinished_status_created`, `idx_github_runner_executions_status_server_id`, `idx_deployment_queues_app_id`, `idx_servers_private_key_id_active`; `CREATE INDEX CONCURRENTLY` outside a transaction, as upstream (`$withinTransaction = false`) |
| `2026_10_29_221743_add_checkout_reminder_sent_at_to_subscriptions_table` | `subscriptions.stripe_checkout_reminder_sent_at timestamp(0)` (Coolify Cloud only) |
| `2026_10_30_144551_add_access_to_team_invitations_table` | `team_invitations.access json` |
| `2026_10_30_171359_drop_image_default_from_standalone_sqlites_table` | `standalone_sqlites.image` loses its default |
| `2026_11_01_142702_add_update_channel_to_instance_settings_table` | `instance_settings.update_channel varchar(255) NOT NULL DEFAULT 'stable'`, `update_custom_tag`, `previous_update_channel` |

### v4.5.3 migrations (b5823c7 → v4.5.3)

| Migration | Change |
|---|---|
| `2026_10_27_142752_add_use_path_style_endpoint_to_s3_storages_table` | `s3_storages.use_path_style_endpoint boolean NOT NULL DEFAULT true` |
| `2026_10_27_155023_add_logdrain_axiom_domain_to_server_settings_table` | `server_settings.logdrain_axiom_domain varchar(255) DEFAULT 'us-east-1.aws.edge.axiom.co'` |
| `2026_10_27_160258_remove_highlight_log_drain_from_server_settings_table` | drops `is_logdrain_highlight_enabled`, `logdrain_highlight_project_id`; stops the log drain of servers that had it on (not transferred away) |
| `2026_10_28_103619_add_keeps_production_values_to_application_previews_table` | `application_previews.keeps_production_values boolean NOT NULL DEFAULT false`, set to true on existing previews |
| `2026_10_28_124421_keep_dockerfile_healthcheck_for_existing_applications` | data only: `health_check_enabled = false` where `custom_healthcheck_found` |

`coolify-rs boot` applies them like `php artisan migrate`: in name order, each in a transaction with its
`migrations` row (the index migrations statement by statement, then the row), under one new batch, skipping those
already listed. A database created by Coolify v4.6.0 has them all and is left unchanged.

Test databases are created from that dump, so Rust tests run against the real upstream schema.

## 1. Decisions

| Question | Decision |
|---|---|
| Read an existing Coolify DB? | Yes, the primary mode. |
| Adopt an existing installation? | Yes, side-by-side with upstream initially (API only); see §6 for the consistency model. |
| Import an export? | Not needed: the schema is shared. A `pg_dump`/restore of the upstream DB is the supported import path. |
| Upgrade through migrations? | Only upstream's own migrations, translated from the PHP (`bootstrap::MIGRATIONS`), on `coolify-rs boot`, to bring a coolify-rs database to the current baseline. coolify-rs-only state, if ever needed, goes in a separate `coolify_rs` schema. |
| Preserve identifiers | Yes: same `id`, `uuid`, team 0 / server 0 / instance settings 0 conventions. |
| Preserve secrets | Yes: Laravel encrypter implemented and verified both directions with APP_KEY (+ APP_PREVIOUS_KEYS). |
| Rollback | `upgrade.sh` takes a database backup before `boot` runs migrations; data written via the API is upstream-compatible by construction. |

### Schema version guard

At startup coolify-rs reads the `migrations` table and compares it with the set of upstream migrations it was built and
tested against (`compat/baseline/supported-migrations.txt`, generated by `coolify-port inventory`).

- DB has migrations unknown to this build → **refuse writes** (read-only mode) unless `COOLIFY_RS_ALLOW_UNKNOWN_SCHEMA=true`.
  This is what happens when upstream ships a new release before coolify-rs has ported it.
- DB is missing migrations the build knows about (older upstream) → **refuse writes**. `coolify-rs boot` first applies
  the missing ones it has in `bootstrap::MIGRATIONS`, so a Coolify v4.5.3 database becomes writable on the first start.
- DB lists migrations upstream **renamed or deleted** after they ran → accepted: they are known, and not required.
  Long-lived installations carry such names; a Coolify 4.6.0 that started on an early v4 has, beyond the v4.6.0 tree,
  `2018_08_08_100000_create_telescope_entries_table`, `2025_10_10_120000_create_cloud_init_scripts_table`,
  `2025_10_10_120000_create_webhook_notification_settings_table` and
  `2025_10_10_120001_populate_webhook_notification_settings_for_existing_teams`. The full list,
  `compat/baseline/historical-migrations.txt` (64 names), is generated from upstream history: every migration file name
  that ever existed on the v4 line up to v4.6.0 but is not in the v4.6.0 tree (the command is in its header).

`coolify-rs check-schema` prints the verdict as JSON, as it will be after `boot`: `applied_migrations` (rows now),
`unknown_migrations`, `missing_migrations` (still missing after `boot`), `applied_on_boot` (what `boot` will apply),
`historical_migrations` (how many renamed or deleted names the table lists; nothing to do about them) and
`read_only_reason` (`null` when coolify-rs can write).

## 2. Type conventions (Laravel 12 Postgres grammar, confirmed by the dump)

- PK `bigint` from a sequence; business tables also have `uuid varchar(255)` (unique where noted).
- `string` → `varchar(255)`, `json` → `json` (not jsonb), `enum` → `varchar` + CHECK.
- Almost all timestamps are `timestamp(0) without time zone`, written in UTC (connection timezone forced to UTC).
  Only 6 `timestamptz` columns exist (notification_throttles, scheduled job state/delivery tables).
- Most `foreignId()` columns have **no FK constraint**: integrity is enforced in application code.
  coolify-rs must enforce the same relationships in its use cases (e.g. scoping by `team_id`).
- Odd defaults: `servers.sentinel_updated_at` and `applications.last_online_at` are NOT NULL with a literal timestamp
  default captured at migration time.

## 3. Identifiers

- `uuid` values are generated by `new_public_id()` = 24 lowercase alphanumeric characters
  (`bootstrap/helpers/shared.php:130-135`). Older rows may hold cuid2 values from migration backfills.
  coolify-rs generates the 24-char form and treats stored uuids as opaque strings.
- Team, User, ServerSetting, ProjectSetting, InstanceSettings, ApplicationDeploymentQueue, TeamInvitation and
  PersonalAccessToken have no uuid.
- Reserved id 0 rows: team 0 "Root Team", user 0 (first/root user), instance_settings 0, server 0 (localhost,
  `host.docker.internal`), private key 0, standalone_docker 0, github_apps 0 / gitlab_apps 0 (public).
  They are inserted explicitly and do not advance sequences.

## 4. Tables used by the first vertical slice

| Table | Notes |
|---|---|
| `users` | `password` nullable bcrypt `$2y$`; `email` unique, lowercased; hidden fields listed in API_COMPATIBILITY.md |
| `teams` | no uuid; `custom_server_limit` hidden in API; `is_two_factor_required` boolean (migration `2026_10_25_115811`, absent in 4.5.0) |
| `team_user` | `(team_id, user_id)` unique; `role` ∈ owner/admin/member; no FKs |
| `personal_access_tokens` | `token varchar(64)` unique (sha256 hex); **`team_id varchar(255) NOT NULL`**; `abilities` text JSON; `tokenable_type = 'App\Models\User'` |
| `instance_settings` | id 0; `is_api_enabled` default false; `allowed_ips` text |
| `projects` | `uuid` unique, `team_id` (no FK); `canvas_position` json cast to array (migration `2026_10_23_182201`, absent in 4.5.0) |
| `project_settings` | one row per project, created on project creation |
| `environments` | `(name, project_id)` unique; uuid NOT NULL unique; `canvas_positions` json |

Side effects reproduced on create (from model `created` hooks):

- `Project::created` inserts a `project_settings` row and an environment named `production`.
- `Team::created` inserts the six notification-settings rows (not exercised: teams are not created via API).

## 5. Release-to-release drift

The upstream schema changes frequently: 159 migrations are newer than the Feb 2026 SQLite dump, and `v4.5.0..main`
(156 commits, 3 days) adds 4 migrations: `canvas_position` on projects and servers, `rolling_update_drain_seconds` on
application_settings, `is_two_factor_required` on teams. coolify-rs handles drift in three ways:

1. Queries name their columns explicitly; optional columns introduced after the baseline are discovered at startup
   from `information_schema.columns` and included only if present (`coolify-db::schema::ColumnSet`).
2. The schema guard above refuses writes against unknown schemas.
3. The migration engine classifies every new upstream migration, maps the tables it touches to Rust repositories via
   `compat/mapping/registry.toml`, and escalates (`high` risk) any that touch mapped tables.

## 6. Running side-by-side with upstream

Sharing a live database with a running upstream instance is possible for the implemented API surface because coolify-rs
writes rows exactly as upstream would. Caveats (also in KNOWN_LIMITATIONS.md):

- Upstream caches (`once()` instance settings, team cache in Redis for 1h) may serve stale data after a coolify-rs write.
- Upstream model events (activity log, audit events, broadcast events) are not emitted by coolify-rs writes.
- Not recommended for production until differential tests cover each write endpoint.

## 7. Encrypted columns (must decrypt with APP_KEY / APP_PREVIOUS_KEYS)

From casts and accessors (static): `applications.{http_basic_auth_password, manual_webhook_secret_*}`,
`application_deployment_queues.{configuration_snapshot, configuration_diff}` (encrypted JSON, legacy plaintext tolerated),
`environment_variables.value` and `shared_environment_variables.value` (a `value()` accessor uses `encrypt()`, so values are
likely **PHP-serialized** strings: `Encrypter::decrypt_maybe_serialized` handles both; to be confirmed at runtime),
`private_keys.private_key`, `server_settings.{sentinel_token, geoip_maxmind_license_key, logdrain_custom_config}`,
`instance_settings.{smtp_* identity fields, smtp_password, resend_api_key, domain_connect_private_key}`,
`team_invitations.link`, notification settings secrets (Discord/Slack/webhook URLs, Pushover keys, Telegram token/chat/thread
ids, email SMTP fields), Git source secrets (GitHub/GitLab/Gitea apps), `oauth_settings.client_secret`,
`s3_storages.{key,secret}`, `cloud_provider_tokens.token`, `integration_tokens.token`, `cloud_init_scripts.script`,
`ssl_certificates.{ssl_certificate, ssl_private_key}`, `local_file_volumes.content`, `audit_events.changes`,
standalone database passwords. Fortify 2FA columns use Fortify's own encryption (encrypter-compatible).

## 8. Polymorphism

There is **no morph map**: `*_type` columns store fully-qualified PHP class names (`App\Models\Application`,
`App\Models\StandaloneDocker`, `App\Models\User`, ...). coolify-rs keeps these strings as a closed Rust enum with
explicit string mapping (`coolify-domain::MorphType`), and treats unknown values as an error, not a default.
