# Known Limitations and Divergences

Everything here is deliberate or known; none of it is hidden behind a "compatible" claim.

## Scope

- Only 23 of 321 upstream API operations are implemented. Others answer `501 {"message":"This endpoint is not
  implemented in coolify-rs yet."}`.
- No web UI, sessions, login, 2FA, OAuth, registration or root setup window (ADR 0003).
- No deployment engine, SSH execution, proxy management, backups, scheduler, notifications or webhooks yet.
- coolify-rs cannot bootstrap a new installation: it requires a database created by upstream Coolify 4.5.0
  (it runs no upstream DDL).

## API divergences (implemented routes)

| Area | Upstream | coolify-rs | Reason |
|---|---|---|---|
| Client IP for the API allowlist | `TrustProxies('*')`: `X-Forwarded-For` trusted from anyone, so the leftmost (client-controlled) entry is used | peer address unless `COOLIFY_RS_TRUSTED_PROXIES` is set; then rightmost untrusted hop | security fix: the allowlist is spoofable upstream when the app is reachable without a proxy |
| `CF-Connecting-IP` for the allowlist | `main` (post-4.5.0, commit `80ce0efe7`) honours it from Cloudflare peers | not honoured (4.5.0 behaviour) | escalated by migration run `run-5ca69705d-b5823c777`; needs a maintained Cloudflare range list |
| Rate limiting | `throttle:api` 200/min, `X-RateLimit-*` headers | none | not implemented yet |
| Audit events | `audit_events` rows for environment changes, auth failures | not written | not implemented yet |
| Environment details with resources | applications/databases/services serialized with sensitive-field rules | `501` | refusing is safer than partial or unfiltered data |
| Validation of non-string values for string fields | Laravel continues evaluating min/max/regex | only "must be a string" is reported | rare; documented |
| Concurrent duplicate environment names | unique index violation → 500 | 409 | improvement |
| Multi-row writes (project create/delete) | model hooks, no transaction | single transaction | improvement; same end state |
| Unknown columns from newer upstream releases | serialized automatically (`$hidden` is a deny-list) | dropped from output and logged until reviewed in `records.rs` | prevents leaking secret columns added upstream |
| Writes when the DB schema is newer than this build | n/a | read-only mode (`503`) unless `COOLIFY_RS_ALLOW_UNKNOWN_SCHEMA=true` | schema guard |
| Ordering of list results | unspecified (insertion order in practice) | `ORDER BY id` / pivot id | deterministic |

## Upstream defects observed (not reproduced as security issues, recorded for review)

- `Project::deleting` / `Environment::deleting` iterate a relation object instead of its results, so shared environment
  variables are likely **not** deleted (orphans). coolify-rs matches upstream's end state (does not delete them) pending
  runtime confirmation; candidate remediation once confirmed.
- SSH host keys are not verified (`StrictHostKeyChecking=no`). coolify-rs will add TOFU pinning when SSH lands.

## Verification gaps

- Upstream's own Pest suite was not run (PHP 8.4 unavailable locally; upstream CI does not run it either).
- Differential tests cover the implemented routes with fixtures for one instance; no fuzzing yet.
- `environment_variables.value` storage format (`encryptString` vs serialized `encrypt`) is inferred from source, not
  confirmed at runtime; the decrypter handles both.
- Migration engine classification is uncalibrated; risk rules are hand-written heuristics.
- The container image builds (see PROJECT_STATE.md) but no end-to-end deployment of coolify-rs beside a live upstream
  instance has been exercised beyond the local reference environment.
- Performance has not been measured for either implementation.
- Reproducible builds are not claimed: `Cargo.lock` and the toolchain are pinned, but bit-for-bit reproducibility was not
  verified.
