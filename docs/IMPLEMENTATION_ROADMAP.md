# Implementation Roadmap

Ordered by dependency and risk, revised after slice 1 and the first migration runs. Each item is done only per the
definition in PROJECT_VISION.md.

## Milestone 1: foundation and first slice (done, 2026-10-27)

Workspace, config, logging, readiness, schema adoption + guard, Laravel encrypter, Sanctum auth chain, teams,
projects/environments, PHP-compatible JSON, differential harness, migration engine v1 with three ledger runs.

## Milestone 2: absorb upstream `main` and strengthen the engine (next)

1. ~~Resolve run `run-5ca69705d-b5823c777`~~ **done (session 2)**: baseline moved to `main` @ b5823c7 using the
   upstream per-commit image; 4 migrations accepted; `CF-Connecting-IP` ported with upstream's own range list;
   102/102 differential cases on `main`.
2. **Layer B template**: "new nullable column on a mapped table": generate the `records.rs` known-column entry and
   a fixture update; validate with the differential harness.
3. **Calibration**: label ~200 historical commits; report precision/recall per category; tune `PATH_RULES`.
4. **Upstream test extraction**: map Pest test files to components (`upstream.tests`) and surface changed upstream
   tests in plans as specification diffs.
5. Containerize the differential run so CI can execute it (reference image + coolify-rs + seeded fixtures).

## Web UI track (started session 3, ADR 0007)

Done: sessions, login with 2FA, projects, environments, private keys, team view. Next with each backend slice: its
pages. Separately: 2FA enrollment, recovery codes, password reset, API tokens, invitations, instance settings.

## Milestone 3: servers and private keys (private keys session 3; server records session 5)

Done: list, show, create, update, delete without connecting to the server, and the server UI pages for them.
Remaining: `/servers/{uuid}/resources` and `/domains` (need the resource models).


Private keys API (needs encrypter, `read:sensitive` exposure, fingerprinting via an SSH key library), then servers
read endpoints (`GET /servers`, `/servers/{uuid}`, `/resources`, `/domains`) and `ServerSetting` serialization rules,
then server create/update/delete without validation side effects.

## Milestone 4: remote execution core (high risk, security review required)

Done (session 6): threat model and ADR 0009, `coolify-remote` (system `ssh`, argv only, TOFU pins, ephemeral keys,
deadlines, output caps, upstream retries and sudo rewrite), the SSH test host, server validation (API, job, UI dialog)
verified against the reference. Remaining: ControlMaster multiplexing and `audit_events` rows (with the deployment
engine), installing prerequisites/Docker (`install=true`).

Original plan:

Typed command builder (argv-level escaping, no string concatenation), SSH via the system `ssh` binary with explicit
options, ControlMaster multiplexing, timeouts/retries (exit 255 only), TOFU host-key pinning, audit log. Server
validation (`POST /servers/{uuid}/validate`) as the first consumer. Requires the threat-model review in
SECURITY_MODEL.md and an isolated test host container (upstream's `docker/testing-host` is a good model).

## Milestone 4b: standalone databases (session 7)

Done: PostgreSQL end to end (API, Compose generation byte-identical to upstream, start/stop/restart/delete jobs with
activity logs, status refresh and minute status checks, UI: environment resource list, new-resource picker, database
page). Then the other engines (Redis, MySQL, MariaDB, MongoDB, KeyDB, Dragonfly, ClickHouse) with database environment
variables at start and per-engine General forms, each verified on the VM (session 8). Next: the public TCP proxy, the
Environment Variables page, `PATCH /databases/{uuid}`, backups.

## Milestone 5: deployment engine

Done (session 8): queue admission and the job for the `dockerimage` build pack, status checks, stop and delete,
verified against upstream on the VM (`compat/diff/applications_diff.py`). Next: git build packs, then private sources.


Explicit state machine over `application_deployment_queues` (status strings and log JSON unchanged), queue admission
(`concurrent_builds`, per-application exclusivity), helper-container build for `dockerimage` then `dockerfile`, health
checks, rolling update, cancellation, recovery after restart. Each build pack is tested independently.

## Milestone 6+

Proxy/labels/TLS, environment variables (confirm storage format at runtime first), standalone databases, backups,
scheduler/jobs, notifications (with SSRF protections), services/templates, webhooks, terminal/file browser,
install/upgrade. Frontend project per ADR 0003 starts once the API covers the main workflows.

## Engine capabilities by milestone

| Milestone | Engine capability |
|---|---|
| 2 | layer B template, calibration, upstream-test surfacing, CI differential |
| 3 | PHP AST in an isolated container for controller/validation-rule extraction |
| 4 | agent-assisted (layer C) ports enabled in a sandbox for low-risk components, with metrics |
| 5 | per-change effort measurement (review time, repair iterations) to quantify human-effort reduction |
