# coolify-rs

A Rust implementation of [Coolify](https://github.com/coollabsio/coolify)'s API and data model, plus a migration
engine that tracks upstream Coolify and ports its changes. *Working name; not affiliated with coolLabs.*

**Status: early.** 28 of 321 upstream API operations are implemented and verified against the official
`coollabsio/coolify` image built from upstream `main` @ b5823c7 (`sha-b5823c7`). A web UI covers what is ported (projects, environments, private keys, team). There is no deployment engine or server management yet. See
[docs/PROJECT_STATE.md](docs/PROJECT_STATE.md) and [docs/KNOWN_LIMITATIONS.md](docs/KNOWN_LIMITATIONS.md).

## What works

- Upstream-compatible `/api`: token auth chain (Sanctum tokens, team binding, API switch, IP allowlist, abilities),
  teams, projects and environments, private keys, health/version/disable. Response bodies are byte-identical to
  upstream for all 131 differential cases.
- Web UI (`frontend/`, Svelte): sign in with a Coolify account (2FA supported), switch teams, manage projects,
  environments and private keys, view team members. Styled after upstream's `DESIGN.md`.
- Adoption of an existing Coolify PostgreSQL database (schema of upstream `main` @ b5823c7) in place, with a schema
  guard that goes read-only when the database is older or newer than the build.
- Laravel `APP_KEY` encryption interop in both directions.
- `coolify-port`: tracks upstream, classifies changes with evidence, maps them to Rust components, plans risk,
  regenerates deterministic artifacts in an isolated branch, validates, and writes a ledger.

## Layout

```
crates/coolify-domain   pure types and rules
crates/coolify-compat   Laravel/PHP formats (encrypter, Sanctum, json_encode)
crates/coolify-db       adopted upstream schema, schema guard, repositories
crates/coolify-api      /api contract: middleware, validation, serialization, handlers
crates/coolify-port     upstream migration engine
apps/coolify-rs         server binary
apps/coolify-port       engine CLI
compat/                 baselines, route inventory, mapping registry, reference env, differential harness
migration/              tracking state and run ledger
docs/                   specs, decisions (docs/decisions), project state
```

## Quick start (development)

```bash
docker compose up -d --wait          # dev Postgres on 127.0.0.1:25432
./scripts/test.sh                    # fmt + clippy + all tests (against the upstream main@b5823c7 schema)
(cd frontend && npm ci && npm run build)   # web UI into frontend/dist (served by coolify-rs)
```

UI development with hot reload: run coolify-rs on `127.0.0.1:18080`, then `cd frontend && npm run dev`
(Vite on :5173 proxies `/ui` and `/api`).

Run against an existing Coolify database at the baseline (read the caveats in docs/DATABASE_COMPATIBILITY.md §6 first):

```bash
DATABASE_URL=postgres://coolify:...@host:5432/coolify cargo run -p coolify-rs -- check-schema
DATABASE_URL=postgres://coolify:...@host:5432/coolify COOLIFY_RS_LISTEN=127.0.0.1:8080 cargo run -p coolify-rs
```

Configuration: `DATABASE_URL` or upstream's `DB_HOST/DB_PORT/DB_DATABASE/DB_USERNAME/DB_PASSWORD`;
`APP_KEY` / `APP_PREVIOUS_KEYS` (the upstream instance's key; needed for private keys and 2FA logins);
`COOLIFY_RS_UI_DIR` (default `frontend/dist`); `COOLIFY_RS_SECURE_COOKIES=true` behind HTTPS; `SESSION_LIFETIME`
(minutes, default 10080);
`COOLIFY_RS_LISTEN` (default `0.0.0.0:8080`); `COOLIFY_VERSION` (reported by `/api/v1/version`, default `4.5.1-dev.b5823c777`);
`COOLIFY_RS_TRUSTED_PROXIES` (IPs/CIDRs whose `X-Forwarded-For` is trusted; default none);
`COOLIFY_RS_ALLOW_UNKNOWN_SCHEMA`; `COOLIFY_RS_LOG_FORMAT=pretty|json`; `RUST_LOG`.

## Differential testing against upstream

```bash
git clone --bare --no-single-branch https://github.com/coollabsio/coolify .upstream/coolify.git
./scripts/reference-up.sh            # official sha-b5823c7 image on 127.0.0.1:18000 (throwaway secrets)
./scripts/reference-seed.sh          # fixture users/teams/tokens via upstream's own models
# start coolify-rs on :18080 against the reference database (password in compat/reference/secrets/reference.envfile)
# (COOLIFY_RS_TRUSTED_PROXIES=127.0.0.1 enables the allowlist cases via --db-url)
python3 compat/diff/differential.py --tokens compat/reference/secrets/tokens.json [--db-url postgres://...]
```

## Upstream migration engine

```bash
cargo run -p coolify-port-cli --bin coolify-port -- status
cargo run -p coolify-port-cli --bin coolify-port -- run --generate     # needs COOLIFY_RS_TEST_DATABASE_URL
```

Reports land in `.migration-runs/<run>/report.md`; ledger entries in `migration/ledger/`. See
[docs/MIGRATION_STRATEGY.md](docs/MIGRATION_STRATEGY.md).

## License

Apache-2.0. Includes files copied or generated from Coolify (Apache-2.0); see [NOTICE](NOTICE).
