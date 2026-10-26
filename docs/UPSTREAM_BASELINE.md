# Upstream Baseline

## Repository

| Item | Value | How verified |
|---|---|---|
| Upstream | https://github.com/coollabsio/coolify | `gh api repos/coollabsio/coolify` (2026-10-27) |
| Default branch | `main` (also `next`) | `git ls-remote --heads` |
| License | Apache-2.0 | `LICENSE`, `composer.json` |
| Analysed snapshot | `main` @ `b5823c777b8d60df856fcf779b70e4b1d237a845` (2026-10-26 19:30 +0100), version string `4.5.1` | `config/constants.php:8` |
| **Compatibility baseline** | tag `v4.5.0` @ `5ca69705d681da46028ea15a8a39d90a4ebcce89` (2026-10-23) | `git rev-parse v4.5.0` |
| Runtime reference | image `coollabsio/coolify:4.5.0` (laravel/framework 12.69.3) | `compat/reference` |
| Commits `v4.5.0..main` | 148 first-parent (156 total), 4 new migrations | `git log`, migration run `run-5ca69705d-b5823c777` |

Why 4.5.0 and not `main`: it is the newest release with an official image, so the static baseline (source)
and runtime baseline (container) are the same version. Everything after it is processed by the migration engine.

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

`scripts/reference-up.sh` starts `compat/reference/compose.yaml`: the official 4.5.0 image + `postgres:15-alpine` +
`redis:7-alpine`, bound to 127.0.0.1 (app on :18000, Postgres on :15432), with freshly generated throwaway secrets in
`compat/reference/secrets/` (git-ignored). Observed on 2026-10-27:

- container healthy; `/api/health` → `200 OK` (`text/html`); `/api/v1/version` without token → 401 JSON;
- 469 migrations applied, newest `2026_10_23_090005_add_server_validation_limits_to_instance_settings_table`;
- seeded: team 0 "Root Team", instance settings 0 (`is_api_enabled=false`), server 0 `localhost` → `host.docker.internal`;
- no users until the first registration (root setup window) or `scripts/reference-seed.sh`.

`scripts/reference-seed.sh` creates fixture users, teams and tokens through upstream's own models (tinker), enables
the API, and writes the tokens to `compat/reference/secrets/tokens.json`.

## Captured artifacts

| File | Content |
|---|---|
| `compat/baseline/upstream-4.5.0-schema.sql` | `pg_dump --schema-only` of the reference DB (83 tables, 56 FKs) |
| `compat/baseline/upstream-4.5.0-migrations.sql` | the 469 `migrations` rows |
| `compat/baseline/supported-migrations.txt` | migration names from source at `v4.5.0` (identical set to the DB rows: verified by `diff`) |
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
cargo run -p coolify-port-cli --bin coolify-port -- inventory --rev v4.5.0   # prints 321 routes, 469 migrations
```
