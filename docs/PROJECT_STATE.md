# Project State

Last updated: 2026-10-27 (end of session 2). Verify these claims against the repository before relying on them.

## Stage

Milestone 1 complete. Milestone 2 item 1 complete: the compatibility baseline is now upstream `main`.
Next: the rest of milestone 2 (IMPLEMENTATION_ROADMAP.md).

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

## Implemented, not yet verified end-to-end

- `can_read_sensitive` computation (no ported endpoint exposes sensitive fields yet).
- Container image (`docker build` succeeds; `check-schema` works against the reference DB).
- CI workflow (`.github/workflows/ci.yml`) written but **never executed** (no remote repository).
- Migration engine repair loop (agent disabled; tamper guard unit-tested only).

## Evidence (reproduce)

```bash
docker compose up -d --wait && ./scripts/test.sh          # 86 tests, fmt + clippy clean
./scripts/reference-up.sh && ./scripts/reference-seed.sh   # upstream sha-b5823c7 reference
# coolify-rs on :18080 against the reference DB with COOLIFY_RS_TRUSTED_PROXIES=127.0.0.1, then:
python3 compat/diff/differential.py --tokens compat/reference/secrets/tokens.json --db-url postgres://coolify:...@127.0.0.1:15432/coolify
./scripts/verify-encrypter-interop.sh
cargo test -p coolify-port --test route_parser_reference   # parser == artisan route:list (321 routes)
```

Results on 2026-10-27: 86/86 tests passed; 102/102 differential cases matched on `main`; interop PASS.

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

1. Layer B template for "new column on a mapped table".
2. Label historical commits for classifier calibration.
3. Private keys API (milestone 3), then servers read endpoints.

## Local environment notes

- Upstream mirror: `.upstream/coolify.git` (bare, depth 400; `git fetch --deepen=N` for more history).
- Reference stack (now `sha-b5823c7`): `docker compose -f compat/reference/compose.yaml down -v` to remove it.
- Dev Postgres: `docker compose down` (data on tmpfs).
