# Project State

Last updated: 2026-10-27 (session 3). Verify these claims against the repository before relying on them.

## Stage

Milestone 1 complete. Milestone 2 item 1 complete (baseline = upstream `main`). Session 3 added the private keys API
(milestone 3, first half), browser sessions and the web UI (ADR 0007).
Next: servers read endpoints + UI, then the rest of milestone 2 (IMPLEMENTATION_ROADMAP.md).

## Versions

| | |
|---|---|
| Compatibility baseline | upstream `main` @ `b5823c777b8d60df856fcf779b70e4b1d237a845` (`4.5.1-dev.b5823c777`), image `coollabsio/coolify:sha-b5823c7` |
| Previous baseline | `v4.5.0` = `5ca69705d681da46028ea15a8a39d90a4ebcce89` (older-database test only) |
| Upstream analysed up to | `b5823c77…` (`migration/state.json` → `last_analyzed`) |
| Upstream integrated up to | `b5823c77…` (`last_integrated`, via run `run-5ca69705d-b5823c777`) |
| Toolchain | Rust 1.99.1 (pinned) |

## Implemented and verified

Verified = contract tests **and** 0 mismatches in the differential run against the `sha-b5823c7` reference.

- API auth chain: Sanctum tokens, token-team membership/role re-check, API switch, IP allowlist with `main`'s
  `client_ip()` (Cloudflare peers' `CF-Connecting-IP`), abilities.
- `GET /api/health`, `/api/v1/health`, `/api/v1/version`, `GET|POST /api/v1/disable`.
- Teams (incl. `is_two_factor_required`), `PATCH /team`.
- Projects/environments CRUD (project details incl. `canvas_position`); environment details for empty environments.
- Laravel encrypter interop (both directions), PHP `json_encode` byte compatibility, `serializeApiResponse` ordering.
- Schema guard: a 4.5.0 database is read-only; the `main` database is writable.
- Private keys CRUD (OpenSSH Ed25519/ECDSA/RSA and RSA PEM; public key and fingerprint identical to upstream's
  phpseclib output; keys encrypted with `APP_KEY` and readable by upstream).
- Servers: `GET /servers`, `GET /servers/{uuid}` (accessors, casts, hidden and sensitive settings, decrypted
  Sentinel token), `POST /servers` (settings row with double-encrypted Sentinel token, default Docker destination,
  server shared variables), `PATCH /servers/{uuid}` (Laravel `url`, PHP timezone list, poliander/cron ported),
  `DELETE /servers/{uuid}` (soft delete plus the force-delete cleanup). Rows written by either side are equal
  after normalization and upstream decrypts coolify-rs's secrets.
- Remote execution (`coolify-remote`, ADR 0009) and server validation: `POST /servers/{uuid}/validate` and the UI
  dialog run upstream's `ValidateServer` steps over the system `ssh`; for each scenario of the SSH test host (root,
  real sudo, missing jq, missing Docker, old Docker, unreadable version, unsupported OS, refused login) the rows
  written match the reference's (`compat/diff/validate_diff.py`, 8/8). `validation_logs` go through ports of
  upstream's redactor and purifier.
- Databases (PostgreSQL, MySQL, MariaDB, MongoDB, Redis, KeyDB, Dragonfly, ClickHouse): `GET /databases`,
  `GET /databases/{uuid}`, `POST /databases/{engine}`, start, stop, restart, `DELETE /databases/{uuid}`, database
  environment variables at start (Redis keeps its credentials there), and the UI flow (environment resources, new
  resource, database page with each engine's General form). On a real VM the Compose file coolify-rs writes is
  byte-identical to upstream's for every engine; rows, variables, responses, errors and `config_hash` match
  (`compat/diff/databases_diff.py --kind <engine>`); deletion removes the same container, volume, directory and rows.
- End to end on a real machine (OrbStack Ubuntu 26.04 VM, Docker 29.1.3, Compose 2.40.3 from Ubuntu packages):
  coolify-rs added and validated the VM through its API and through the UI dialog; upstream validating the same row
  wrote identical columns for root, a sudo user, missing jq, a stopped Docker daemon and a refused login (5/5). A
  regenerated VM host key was refused by coolify-rs (upstream connected anyway); re-entering the address accepted the
  new key.

## Implemented, verified by tests and a manual browser walkthrough

- Browser login (bcrypt, TOTP 2FA with replay protection, throttling), sessions, CSRF, team switching, `/ui/api` bridge.
- Web UI ported from upstream's Blade views (ADR 0008), compared by headless screenshots against the reference
  instance at the same URLs: login and 2FA challenge, shell (topbar, breadcrumb with team/project/environment
  switchers, drill-down sidebar, user menu, collapsed sidebar, phone sheet), dashboard, servers (list, new server
  by IP, General with the server menu, switcher and status chip, Danger Zone with password confirmation), projects, project,
  project settings, environment, environment settings, private keys (list, generate ED25519/RSA, add, edit, delete,
  delete unused), team settings, members (roles, removal), appearance (theme, page width, navigation style),
  the server validation dialog (running, complete, failed) and the previous validation output.
  The `/ui/*` read models and writes behind them are covered by `ui_pages.rs`.

## Implemented, not yet verified end-to-end

- `can_read_sensitive` computation (no ported endpoint exposes sensitive fields yet).
- Container image (`docker build` succeeds incl. the UI; serves UI and API against the reference DB).
- CI workflow (`.github/workflows/ci.yml`) written but **never executed** (no remote repository).
- Migration engine repair loop (agent disabled; tamper guard unit-tested only).

## Evidence (reproduce)

```bash
docker compose up -d --wait && ./scripts/testing-host-up.sh
COOLIFY_RS_TEST_SSH=127.0.0.1:2223 COOLIFY_RS_TEST_SSH_KEY=compat/testing-host/secrets/id_ed25519 ./scripts/test.sh  # 181 tests
(cd frontend && npm ci && npm run check && npm run build)  # UI type-check + build
./scripts/reference-up.sh && ./scripts/reference-seed.sh   # upstream sha-b5823c7 reference
# coolify-rs on :18080 against the reference DB with APP_KEY (reference) and COOLIFY_RS_TRUSTED_PROXIES=127.0.0.1, then:
python3 compat/diff/differential.py --tokens compat/reference/secrets/tokens.json --db-url postgres://coolify:...@127.0.0.1:15432/coolify
python3 compat/diff/validate_diff.py                       # coolify-rs started with COOLIFY_RS_DATA_DIR
python3 compat/diff/databases_diff.py --server <uuid> --vm <orbstack machine>
./scripts/verify-encrypter-interop.sh
cargo test -p coolify-port --test route_parser_reference   # parser == artisan route:list (321 routes)
```

Results on 2026-10-28: 177/177 tests passed; 194/194 differential cases, 8/8 validation scenarios and every
database check (real VM: PostgreSQL 41/41, Redis 42/42, KeyDB 40/40, Dragonfly 39/39, MongoDB 40/40, MySQL 40/40,
MariaDB 40/40, ClickHouse 39/39) matched on `main`; interop PASS; UI type-check clean; container image builds (with OpenSSH).

## Migration ledger

| Run | Risk | Verdict | Status |
|---|---|---|---|
| `run-5ca69705d-b5823c777` (v4.5.0..main, 148 commits) | High | failed, then resolved by manual port | **integrated** |
| `run-b17c83d68-a1c24e084` | Low | validated | nothing to port |
| `run-f75136586-4559d21b4` | Medium | validated | nothing to port (unported area) |

## Architectural decisions

ADRs 0001–0009 in `docs/decisions/`.

## Active branches

- `main`: all work.
- `port/run-*`: engine-generated branches (local only, not for merge as-is).

## Known failures

None.

## Blockers / decisions needed

1. Enabling an AI repair agent (`port.toml [repair].agent_command`) is a security decision.
2. Publishing (repository name, remote, CI) is not decided; the name "coolify-rs" is provisional.

## Next concrete actions

1. `audit_events` rows (cross-cutting; upstream writes them for server validation and most writes).
2. The public TCP proxy, the database Environment Variables page and `PATCH /databases/{uuid}`;
   then the deployment engine (milestone 5), with SSH multiplexing.
3. Persist sessions in a `coolify_rs` schema; 2FA enrollment and recovery codes; API token management UI.
4. Layer B template for "new column on a mapped table"; classifier calibration.

## Local environment notes

- Upstream mirror: `.upstream/coolify.git` (bare, depth 400; `git fetch --deepen=N` for more history).
- Reference stack (now `sha-b5823c7`): `docker compose -f compat/reference/compose.yaml down -v` to remove it.
- Dev Postgres: `docker compose down` (data on tmpfs).
- Test logins for the reference instance: `compat/reference/secrets/ui-login.txt` (git-ignored, throwaway).
