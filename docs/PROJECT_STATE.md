# Project State

Last updated: 2026-10-27 (end of session 1). Verify these claims against the repository before relying on them.

## Stage

Milestone 1 complete: foundation, first verified vertical slice, migration engine v1 with recorded runs.
Next: milestone 2 (IMPLEMENTATION_ROADMAP.md).

## Versions

| | |
|---|---|
| Compatibility baseline | upstream `v4.5.0` = `5ca69705d681da46028ea15a8a39d90a4ebcce89` (image `coollabsio/coolify:4.5.0`) |
| Upstream analysed up to | `main` @ `b5823c777b8d60df856fcf779b70e4b1d237a845` (`migration/state.json` → `last_analyzed`) |
| Upstream integrated up to | none beyond the baseline (`last_integrated = null`) |
| Rust commit | see `git log` on `main` (session 1 ends with the commit that adds this file) |
| Toolchain | Rust 1.95.0 (pinned) |

## Implemented and verified

Verified = contract tests in CI **and** 0 mismatches in the differential run against the 4.5.0 reference.

- API auth chain: Sanctum tokens, token-team membership/role re-check, API switch, IP allowlist, abilities.
- `GET /api/health`, `/api/v1/health`, `/api/v1/version`, `GET|POST /api/v1/disable`.
- Teams: `/teams`, `/teams/{id}`, `/teams/{id}/members`, `/team`, `/team/members`, `/teams/current[/members]`, `PATCH /team`.
- Projects/environments: list, show, create, update, delete; environments list/create/update/delete; environment
  details (empty environments; non-empty → 501).
- Laravel encrypter interop (both directions), PHP `json_encode` byte compatibility, `serializeApiResponse` ordering.
- Database writes equivalent to upstream (checked row-by-row for project creation).

## Implemented, not yet verified end-to-end

- Schema guard / read-only mode (unit + contract tests; not exercised against a real newer upstream DB).
- `can_read_sensitive` computation (no ported endpoint exposes sensitive fields yet).
- Container image (`docker build` succeeds, 235 MB; binary runs `check-schema` against the reference DB).
- CI workflow (`.github/workflows/ci.yml`) written but **never executed** (no remote repository).
- Migration engine repair loop (agent disabled; tamper guard unit-tested only).

## Evidence (reproduce)

```bash
docker compose up -d --wait && ./scripts/test.sh          # 83 tests, fmt + clippy clean
./scripts/reference-up.sh && ./scripts/reference-seed.sh   # upstream 4.5.0 reference
# coolify-rs against the reference DB on :18080, then:
python3 compat/diff/differential.py --tokens compat/reference/secrets/tokens.json   # 97/97
./scripts/verify-encrypter-interop.sh                      # PASS
cargo test -p coolify-port --test route_parser_reference   # parser == artisan route:list (321 routes)
```

Results on 2026-10-27: 83/83 tests passed; 97/97 differential cases matched; interop PASS; route parser identical.

## Migration ledger

| Run | Risk | Verdict | Status |
|---|---|---|---|
| `run-5ca69705d-b5823c777` (v4.5.0..main, 148 commits) | High | failed (expected) | **escalated, awaiting decisions** (see blockers) |
| `run-b17c83d68-a1c24e084` | Low | validated | nothing to port |
| `run-f75136586-4559d21b4` | Medium | validated | nothing to port (unported area) |

## Architectural decisions

ADRs 0001–0006 in `docs/decisions/`: workspace/deps, REST API as primary contract, frontend strategy, adopt upstream
schema in place, engine trust boundaries, client IP/trusted proxies divergence.

## Active branches

- `main`: all work.
- `port/run-*`: engine-generated branches in `.migration-runs/*/worktree` (local only, not for merge as-is).

## Known failures

None in the test suite. The escalated migration run fails validation by design until the baseline is updated.

## Blockers / decisions needed

1. Accept upstream `main`'s schema (4 migrations) as the new baseline. Needs a runtime schema for `main`/4.5.1
   (build the image from source or wait for the 4.5.1 release).
2. Port `CF-Connecting-IP` handling for the API allowlist (`80ce0efe7`)? Needs a maintained Cloudflare IP list.
3. Enabling an AI repair agent (`port.toml [repair].agent_command`) is a security decision.
4. Publishing (repository name, remote, CI) is not decided; the name "coolify-rs" is provisional.

## Next concrete actions

1. Milestone 2 item 1 (resolve the escalated run), then the layer B "new column" template.
2. Label historical commits for classifier calibration.
3. Private keys API (milestone 3), then servers read endpoints.

## Local environment notes

- Upstream mirror: `.upstream/coolify.git` (bare, depth 400; `git fetch --deepen=N` for more history).
- Reference stack: `docker compose -f compat/reference/compose.yaml down -v` to remove it.
- Dev Postgres: `docker compose down` (data on tmpfs).
