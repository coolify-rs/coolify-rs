# Upstream Baseline

## Repository

| Item | Value | How verified |
|---|---|---|
| Upstream | https://github.com/coollabsio/coolify | `gh api repos/coollabsio/coolify` (2026-10-27) |
| Default branch | `main` (also `next`) | `git ls-remote --heads` |
| License | Apache-2.0 | `LICENSE`, `composer.json` |
| Analysed snapshot | `main` @ `b5823c777b8d60df856fcf779b70e4b1d237a845` (2026-10-26 19:30 +0100), version string `4.5.1` | `config/constants.php:8` |
| **Compatibility baseline** | `main` @ `b5823c777b8d60df856fcf779b70e4b1d237a845` (reports `4.5.1-dev.b5823c777`) | adopted 2026-10-27 |
| Runtime reference | image `coollabsio/coolify:sha-b5823c7` (upstream's per-commit `main` build; image label `org.opencontainers.image.revision` = `b5823c77…`) | `compat/reference` |
| Previous baseline | tag `v4.5.0` @ `5ca69705d681da46028ea15a8a39d90a4ebcce89` (image `coollabsio/coolify:4.5.0`, laravel/framework 12.69.3) | kept for the older-database test |
| Commits `v4.5.0..main` | 148 first-parent (156 total), 4 new migrations | migration run `run-5ca69705d-b5823c777` |

History: session 1 used `v4.5.0` (newest release with an official image). Session 2 adopted `main` after finding that
upstream publishes immutable per-commit images (`sha-<7>`), so source and runtime can again be pinned to the same
commit. The adoption followed the migration engine's escalated run; see MIGRATION_STRATEGY.md §9.

## Local layout (not committed)

- `.upstream/coolify.git`: bare mirror (`git clone --bare --depth 400 --no-single-branch`; deepen with `git fetch --deepen`).
- `.upstream/snapshot`: extracted tarball of `b5823c77` used for static reading.

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
| `compat/baseline/upstream-main-b5823c7-schema.sql` | `pg_dump --schema-only` of a fresh `sha-b5823c7` install (current baseline) |
| `compat/baseline/upstream-main-b5823c7-migrations.sql` | its 473 `migrations` rows |
| `compat/baseline/upstream-4.5.0-schema.sql` | `pg_dump --schema-only` of the 4.5.0 reference DB (83 tables, 56 FKs) |
| `compat/baseline/upstream-4.5.0-migrations.sql` | the 469 `migrations` rows of 4.5.0 |
| `compat/baseline/supported-migrations.txt` | migration names from source at `main` (473; set equal to the DB rows) |
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
