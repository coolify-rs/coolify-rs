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
| Toolchain | Rust 1.95.0 (pinned) |

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

## Implemented, verified by tests and a manual browser walkthrough

- Browser login (bcrypt, TOTP 2FA with replay protection, throttling), sessions, CSRF, team switching, `/ui/api` bridge.
- Web UI ported from upstream's Blade views (ADR 0008), compared by headless screenshots against the reference
  instance at the same URLs: login and 2FA challenge, shell (topbar, breadcrumb with team/project/environment
  switchers, drill-down sidebar, user menu, collapsed sidebar, phone sheet), dashboard, servers (list, new server
  by IP, General with the server menu, switcher and status chip, Danger Zone with password confirmation), projects, project,
  project settings, environment, environment settings, private keys (list, generate ED25519/RSA, add, edit, delete,
  delete unused), team settings, members (roles, removal), appearance (theme, page width, navigation style).
  The `/ui/*` read models and writes behind them are covered by `ui_pages.rs`.

## Implemented, not yet verified end-to-end

- `can_read_sensitive` computation (no ported endpoint exposes sensitive fields yet).
- Container image (`docker build` succeeds incl. the UI; serves UI and API against the reference DB).
- CI workflow (`.github/workflows/ci.yml`) written but **never executed** (no remote repository).
- Migration engine repair loop (agent disabled; tamper guard unit-tested only).

## Evidence (reproduce)

```bash
docker compose up -d --wait && ./scripts/test.sh          # 109 tests, fmt + clippy clean
(cd frontend && npm ci && npm run check && npm run build)  # UI type-check + build
./scripts/reference-up.sh && ./scripts/reference-seed.sh   # upstream sha-b5823c7 reference
# coolify-rs on :18080 against the reference DB with APP_KEY (reference) and COOLIFY_RS_TRUSTED_PROXIES=127.0.0.1, then:
python3 compat/diff/differential.py --tokens compat/reference/secrets/tokens.json --db-url postgres://coolify:...@127.0.0.1:15432/coolify
./scripts/verify-encrypter-interop.sh
cargo test -p coolify-port --test route_parser_reference   # parser == artisan route:list (321 routes)
```

Results on 2026-10-27: 124/124 tests passed; 183/183 differential cases matched on `main`; interop PASS; UI
type-check clean; container image builds and serves UI + API.

## Migration ledger

| Run | Risk | Verdict | Status |
|---|---|---|---|
| `run-5ca69705d-b5823c777` (v4.5.0..main, 148 commits) | High | failed, then resolved by manual port | **integrated** |
| `run-b17c83d68-a1c24e084` | Low | validated | nothing to port |
| `run-f75136586-4559d21b4` | Medium | validated | nothing to port (unported area) |

## Architectural decisions

ADRs 0001–0006 in `docs/decisions/`.

## Active branches

- `main`: all work.
- `port/run-*`: engine-generated branches (local only, not for merge as-is).

## Known failures

None.

## Blockers / decisions needed

1. Enabling an AI repair agent (`port.toml [repair].agent_command`) is a security decision.
2. Publishing (repository name, remote, CI) is not decided; the name "coolify-rs" is provisional.

## Next concrete actions

1. Remote execution core (milestone 4, security review first), then server validation; `/resources` and
   `/domains` with the resource models.
2. Persist sessions in a `coolify_rs` schema; 2FA enrollment and recovery codes; API token management UI.
3. Layer B template for "new column on a mapped table"; classifier calibration.

## Local environment notes

- Upstream mirror: `.upstream/coolify.git` (bare, depth 400; `git fetch --deepen=N` for more history).
- Reference stack (now `sha-b5823c7`): `docker compose -f compat/reference/compose.yaml down -v` to remove it.
- Dev Postgres: `docker compose down` (data on tmpfs).
- Test logins for the reference instance: `compat/reference/secrets/ui-login.txt` (git-ignored, throwaway).
