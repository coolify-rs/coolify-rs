# Upstream Baseline

## Repository

| Item | Value | How verified |
|---|---|---|
| Upstream | https://github.com/coollabsio/coolify | `gh api repos/coollabsio/coolify` (2026-10-27) |
| Default branch | `main` (also `next`) | `git ls-remote --heads` |
| License | Apache-2.0 | `LICENSE`, `composer.json` |
| Analysed snapshot | tag `v4.5.3` @ `70c1bcb1381de7d42fd290e6cd50bf9a5bcf714d`, version string `4.5.3` (`.upstream/snapshot-4.5.3`); `main` @ `b5823c777b8d60df856fcf779b70e4b1d237a845` (`.upstream/snapshot`) for older notes | `config/constants.php:8` |
| **Compatibility baseline** | tag `v4.7.5` @ `378ce3735c4522ebb6b68e93b909b0511f278763` (coolify-rs reports `4.7.5` by default; releases set `COOLIFY_VERSION=4.7.5-rs.N`) | port of `v4.6.0..v4.7.5`, 2026-11-12 |
| Schema baseline | Coolify v4.7.5 (520 migrations): the v4.6.0 dump plus the twenty-seven migrations of v4.7.0 to v4.7.5 as `bootstrap::MIGRATIONS` runs them (compared on 2026-11-13 with a real `coollabsio/coolify:4.7.5` database, `php artisan migrate` on Postgres 15: identical, the same 520 migration names, apart from install-time timestamp defaults) | `compat/baseline/upstream-4.7.5-*.sql` |
| v4.6.0 baseline | tag `v4.6.0` @ `0becfba9113063336b09860579627fd218cb3fc4` (493 migrations: the v4.5.3 capture plus the fifteen v4.6.0 migrations; compared with a real `coollabsio/coolify:4.6.0` database: identical except install-time timestamp defaults); `coolify-rs boot` upgrades it | `compat/baseline/upstream-4.6.0-*.sql` |
| Runtime reference | image `coollabsio/coolify:sha-b5823c7` (upstream's per-commit `main` build; image label `org.opencontainers.image.revision` = `b5823c77…`); not moved to 4.5.3, so the v4.5.3 port was verified by reading the PHP and by Rust tests | `compat/reference` |
| Previous baseline | `main` @ `b5823c777b8d60df856fcf779b70e4b1d237a845` (image `sha-b5823c7`): the schema earlier coolify-rs builds created; `coolify-rs boot` upgrades it | upgrade test |
| Older baseline | tag `v4.5.0` @ `5ca69705d681da46028ea15a8a39d90a4ebcce89` (image `coollabsio/coolify:4.5.0`, laravel/framework 12.69.3) | kept for the older-database test |
| Commits `v4.5.0..main` | 148 first-parent (156 total), 4 new migrations | migration run `run-5ca69705d-b5823c777` |
| Commits `b5823c7..v4.5.3` | 71 first-parent, 5 new migrations | migration run `run-b5823c777-70c1bcb13` |
| Commits `v4.5.3..v4.6.0` | 74, 15 new migrations | migration run `run-70c1bcb13-0becfba91` |
| Commits `v4.6.0..v4.7.5` | 242, 27 new migrations | ported by hand, one area at a time |

History: session 1 used `v4.5.0` (newest release with an official image). Session 2 adopted `main` after finding that
upstream publishes immutable per-commit images (`sha-<7>`), so source and runtime can again be pinned to the same
commit. The adoption followed the migration engine's escalated run; see MIGRATION_STRATEGY.md §9. Session 3 moved
to the release tag `v4.5.3` so that a Coolify v4.5.3 installation can be migrated to the first coolify-rs release:
the 71 commits `b5823c7..v4.5.3` were ported by hand and recorded with `coolify-port mark-integrated`.

## Local layout (not committed)

- `.upstream/coolify.git`: bare mirror (`git clone --bare --depth 400 --no-single-branch`; deepen with `git fetch --deepen`).
- `.upstream/snapshot`: extracted tarball of `b5823c77` used for static reading.
- `.upstream/snapshot-4.5.3`: extracted tarball of `v4.5.3` (`70c1bcb1`).

## Upstream stack (from source)

PHP ^8.4, Laravel ^12.70.1, Livewire ^3.8, Fortify, Sanctum ^4.3.3, Horizon, Reverb, Socialite, PostgreSQL 15,
Redis 7, Node terminal server, Tailwind 4, Vite, Svelte 5 (canvases). See ARCHITECTURE_ASSESSMENT.md.

## Build and test commands (upstream)

- Dev: `docker compose -f docker-compose.dev.yml up` (builds `docker/development/Dockerfile`).
- Tests: Pest (`./vendor/bin/pest`), default test DB is SQLite in memory (`phpunit.xml`, `DB_CONNECTION=testing`).
- **Not run here.** The local PHP is 8.2 (upstream requires 8.4) and no upstream CI job runs the test suite, so there
  is no known-green upstream test baseline. Upstream tests are used as behavioural specifications, not as an oracle.

## Runtime reference environment (verified)

`scripts/reference-up.sh` starts `compat/reference/compose.yaml`: the official image (`sha-b5823c7` by default,
override with `COOLIFY_REFERENCE_VERSION`) + `postgres:15-alpine` +
`redis:7-alpine`, bound to 127.0.0.1 (app on :18000, Postgres on :15432), with freshly generated throwaway secrets in
`compat/reference/secrets/` (git-ignored). Observed on 2026-10-27 with 4.5.0 (the stack was later upgraded in
place to `sha-b5823c7`, which applied the 4 new migrations; a fresh `sha-b5823c7` install produced the same schema
apart from upstream's literal timestamp defaults):

- container healthy; `/api/health` → `200 OK` (`text/html`); `/api/v1/version` without token → 401 JSON;
- 469 migrations applied, newest `2026_10_23_090005_add_server_validation_limits_to_instance_settings_table`;
- seeded: team 0 "Root Team", instance settings 0 (`is_api_enabled=false`), server 0 `localhost` → `host.docker.internal`;
- no users until the first registration (root setup window) or `scripts/reference-seed.sh`.

`scripts/reference-seed.sh` creates fixture users, teams and tokens through upstream's own models (tinker), enables
the API, and writes the tokens to `compat/reference/secrets/tokens.json`.

## Captured artifacts

| File | Content |
|---|---|
| `compat/baseline/upstream-4.7.5-schema.sql` | `pg_dump --schema-only` of a fresh `coollabsio/coolify:4.7.5` database (reference for comparison) |
| `compat/baseline/upstream-4.7.5-migrations.sql` | its 520 `migrations` rows |
| `compat/baseline/upstream-4.6.0-schema.sql` | `pg_dump --schema-only` of the v4.5.3 capture after the v4.6.0 migrations (the dump a new installation loads before the v4.7.5 migrations; see below) |
| `compat/baseline/upstream-4.6.0-migrations.sql` | its 493 `migrations` rows, in name order, batch 1 |
| `compat/baseline/upstream-4.5.3-schema.sql` | `pg_dump --schema-only` of a fresh `coollabsio/coolify:4.5.3` install (previous baseline) |
| `compat/baseline/upstream-4.5.3-migrations.sql` | its 478 `migrations` rows |
| `compat/baseline/upstream-main-b5823c7-schema.sql` | `pg_dump --schema-only` of a fresh `sha-b5823c7` install (previous baseline) |
| `compat/baseline/upstream-main-b5823c7-migrations.sql` | its 473 `migrations` rows |
| `compat/baseline/upstream-4.5.0-schema.sql` | `pg_dump --schema-only` of the 4.5.0 reference DB (83 tables, 56 FKs) |
| `compat/baseline/upstream-4.5.0-migrations.sql` | the 469 `migrations` rows of 4.5.0 |
| `compat/baseline/supported-migrations.txt` | migration names from source at `v4.7.5` (520: the 493 rows of the v4.6.0 dump and the 27 of `bootstrap::MIGRATIONS`) |
| `compat/baseline/historical-migrations.txt` | the 64 migration names upstream history had on the v4 line and the v4.6.0 tree no longer has (accepted by the schema guard) |
| `compat/api/reference-route-list-4.5.0.json` | `php artisan route:list --json --path=api` from the image |
| `compat/api/upstream-routes.json` | static route inventory from `routes/api.php` (321 routes; equal to `route:list` for all 321) |
| `crates/coolify-compat/tests/fixtures/laravel_encrypter.json` | payloads produced by the image's Laravel Encrypter |

## Required environment variables (upstream production)

From `scripts/install.sh` / `docker-compose.prod.yml`: `APP_ID`, `APP_KEY` (`base64:` 32 bytes), `DB_USERNAME`,
`DB_PASSWORD`, `DB_DATABASE`, `REDIS_PASSWORD`, `PUSHER_APP_ID`, `PUSHER_APP_KEY`, `PUSHER_APP_SECRET`,
`PUSHER_BACKEND_PORT`, optional `ROOT_USERNAME`, `ROOT_USER_EMAIL`, `ROOT_USER_PASSWORD`, `REGISTRY_URL`, `AUTOUPDATE`,
`API_RATE_LIMIT`, `SSH_COMMAND_TIMEOUT`, `SELF_HOSTED`.

## Reproducing the baseline

```bash
git clone --bare --no-single-branch https://github.com/coollabsio/coolify .upstream/coolify.git
./scripts/reference-up.sh
./scripts/reference-seed.sh
cargo run -p coolify-port-cli --bin coolify-port -- inventory --rev b5823c777   # prints 321 routes, 473 migrations
```

The v4.5.3 schema baseline was captured without the reference stack, on a scratch network and Postgres that were
removed afterwards (any throwaway password and `base64:` key will do):

```bash
docker network create crs-capture-453-net
docker run -d --name crs-capture-453-pg --network crs-capture-453-net -e POSTGRES_USER=coolify \
  -e POSTGRES_PASSWORD=<throwaway> -e POSTGRES_DB=coolify postgres:15-alpine          # 15.19
docker run --rm --network crs-capture-453-net --entrypoint "" -w /var/www/html \
  -e APP_ENV=production -e APP_KEY=base64:<throwaway> -e DB_CONNECTION=pgsql -e DB_HOST=crs-capture-453-pg \
  -e DB_DATABASE=coolify -e DB_USERNAME=coolify -e DB_PASSWORD=<throwaway> \
  -e CACHE_STORE=array -e QUEUE_CONNECTION=sync -e SESSION_DRIVER=array \
  coollabsio/coolify:4.5.3 php artisan migrate --force
docker exec crs-capture-453-pg pg_dump -U coolify -d coolify -s --no-owner --no-privileges \
  | grep -v '^\\' > compat/baseline/upstream-4.5.3-schema.sql
docker exec crs-capture-453-pg pg_dump -U coolify -d coolify --data-only --inserts --no-owner --no-privileges \
  -t migrations | grep -v '^\\' > compat/baseline/upstream-4.5.3-migrations.sql
docker rm -f crs-capture-453-pg && docker network rm crs-capture-453-net
cargo run -p coolify-port-cli --bin coolify-port -- inventory --rev 70c1bcb13 --write   # 321 routes, 478 migrations
```

The v4.6.0 schema baseline was built without any upstream image: a scratch database on the test Postgres (15.19)
got the v4.5.3 baseline (`bootstrap::prepare`), then `bootstrap::apply_migrations(MIGRATIONS)` (the fifteen v4.6.0
ones ran), then its `migrations` rows were renumbered in name order in batch 1 (`TRUNCATE` and re-insert ordered by
`migration COLLATE "C"`, `setval`), and both files were dumped with the two `pg_dump` commands above. `diff` with the
v4.5.3 dump shows only the new columns, the dropped `standalone_sqlites.image` default and the nine indexes. The
supported list came from `coolify-port inventory --rev v4.6.0 --write` (493 migrations; its `upstream-routes.json`
was regenerated at the merge). A dump of a real `coollabsio/coolify:4.6.0` database matched it except for
install-time timestamp defaults.

A new installation and every test database are the v4.6.0 dump plus `bootstrap::MIGRATIONS`. The v4.7.5 files in
`compat/baseline/` are a real `coollabsio/coolify:4.7.5` database (`php artisan migrate --force` on a scratch Postgres
15, the v4.5.3 procedure above), kept to compare against: a fresh `coolify-rs create-schema` gives the same schema
apart from upstream's install-time timestamp defaults. `historical-migrations.txt` is generated at `v4.7.5`.
