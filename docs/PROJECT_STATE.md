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

## Implemented, verified by tests and a manual browser walkthrough

- Browser login (bcrypt, TOTP 2FA with replay protection, throttling), sessions, CSRF, team switching, `/ui/api` bridge.
- Web UI: sign-in, projects (list/create/edit/delete), environments (create/edit/delete/details), private keys
  (list/add/edit/delete/copy public key), team (members, build-server fallback toggle), role-aware controls,
  light/dark theme, phone layout. Objects created in the UI were read back through upstream PHP's API.

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

Results on 2026-10-27: 109/109 tests passed; 131/131 differential cases matched on `main`; interop PASS; UI
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

1. Servers read endpoints (`GET /servers`, `/servers/{uuid}`, `/resources`, `/domains`) and a servers UI.
2. Persist sessions in a `coolify_rs` schema; 2FA enrollment and recovery codes; API token management UI.
3. Layer B template for "new column on a mapped table"; classifier calibration.

## Local environment notes

- Upstream mirror: `.upstream/coolify.git` (bare, depth 400; `git fetch --deepen=N` for more history).
- Reference stack (now `sha-b5823c7`): `docker compose -f compat/reference/compose.yaml down -v` to remove it.
- Dev Postgres: `docker compose down` (data on tmpfs).
- Test logins for the reference instance: `compat/reference/secrets/ui-login.txt` (git-ignored, throwaway).
