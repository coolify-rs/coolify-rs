# Architecture Assessment of Upstream Coolify

Source: `coollabsio/coolify` `main` @ `b5823c777b8d60df856fcf779b70e4b1d237a845` (4.5.1), static analysis unless marked
**[runtime]** (checked against `coollabsio/coolify:4.5.0` in `compat/reference`). Paths are relative to the upstream root.

## 1. Size and shape

| Area | Files | PHP lines |
|---|---|---|
| `app/` total | 1057 | 209,004 |
| `app/Models` | 73 | 21,992 |
| `app/Http/Controllers` (of which `Api/`) | 67 (50) | 38,278 (34,413) |
| `app/Livewire` | 272 | 57,024 |
| `app/Jobs` | 68 | 18,179 |
| `app/Actions` | 99 | 14,344 |
| `app/Console/Commands` | 42 | 8,717 |
| `database/migrations` | 473 (469 applied on 4.5.0 [runtime]) | 17,398 |
| `resources/views` (Blade) | 512 | 49,763 |
| `tests/` | 1339 | 236,375 |

`bootstrap/helpers/*.php` holds a large amount of procedural domain logic (shared.php alone is >4,700 lines;
`ApplicationDeploymentJob.php` is 6,479 lines).

## 2. Runtime stack

- PHP 8.4, Laravel 12 (12.69.3 in the 4.5.0 image [runtime]), Fortify, Sanctum 4, Livewire 3, Horizon, Reverb,
  Socialite, spatie/activitylog, phpseclib (key parsing only), lorisleiva/laravel-actions.
- PostgreSQL 15 (`postgres:15-alpine`), Redis 7 (cache, queues, locks, sessions-by-default is `database`).
- One `coolify` container supervised by s6-overlay running: nginx+php-fpm, `start:migration` → `start:seeder` →
  `app:init` one-shots, `horizon`, `schedule:work`, `reverb` (6001) and a Node terminal websocket server (6002).
- Frontend: Blade + Livewire + Alpine + Tailwind 4; Svelte 5 only for the deploy/environment "canvas" pages.

## 3. Remote execution model

- **Everything** on a managed server, including the Coolify host (server id 0, `ip = host.docker.internal`),
  goes over the system `ssh` binary via Laravel `Process`. The Docker socket is **not** mounted into the coolify container.
- A command batch is an array of shell strings joined by `\n` and sent as a quoted heredoc on stdin
  (`app/Helpers/SshMultiplexingHelper.php:243-254`) to `sh -c "exec bash -se || exec sh -se"`.
- SSH options include `StrictHostKeyChecking=no` and `UserKnownHostsFile=/dev/null`: **host keys are not verified**.
- ControlMaster multiplexing per server (`storage/app/ssh/mux/mux_<uuid>`), cache-locked creation, health-checked reuse.
- Keys are written from the DB (encrypted column) to `storage/app/ssh/keys/ssh_key@<uuid>` with 0600 before use.
- Retries: only SSH exit 255 is retried (3 tries, exponential 2s..30s). `SSH_COMMAND_TIMEOUT` default 3600s.
- Escaping is by convention (`escapeshellarg`, `escapeShellValue`) at each call site; there is no typed command builder.
- Non-root servers: each line is rewritten to add `sudo` (`bootstrap/helpers/sudo.php`).
- `coolify-helper` image (docker CLI, compose, buildx, pack, nixpacks, railpack, git, mc) is the build container,
  the S3 upload container and the upgrade runner.

## 4. Deployment engine (`app/Jobs/ApplicationDeploymentJob.php`)

- One queued job per deployment (`tries=1`, timeout = server `dynamic_timeout`, default 3600s), queue `high` (self-hosted).
- Build packs: `dockerimage`, `dockerfile` (repo or inline), `dockercompose`, `nixpacks`, `railpack`, `static`,
  plus restart-only and pull-request preview flows.
- Phases (nixpacks): start helper container on the destination network → `git ls-remote` → image naming
  (`<uuid>:<sha>`) → skip if image exists → clone → plan → generate compose with labels → build → save env →
  optional registry push → rolling update (compose up new, docker health check loop, proxy drain wait, stop old) →
  post-deployment (status, PR status, post-deploy command).
- Status enum `application_deployment_queues.status`: `queued`, `in_progress`, `finished`, `failed`, `cancelled-by-user`;
  terminal states are never overwritten. Every transition triggers `queue_next_deployment`.
- Logs: JSON array in a `text` column, entries `{command, output, type, timestamp, hidden, batch, order}`,
  appended under `SELECT ... FOR UPDATE` with secret redaction. UI polls every 2s.
- Cancellation: status set to `cancelled-by-user`, helper container `docker rm -f`; the job notices by polling.
- Concurrency: one in-progress deployment per (application, pull_request_id); `concurrent_builds` per server (default 2);
  `deployment_queue_limit` default 25.

Assessment: the engine is a long imperative script with state in the DB row. A Rust port should model it as an explicit
state machine with typed steps and a command builder, keeping the DB row format (status strings and log JSON) for
compatibility.

## 5. Background work

- Horizon on Redis; self-hosted uses queues `high,default` only (all logical queues map to `high`).
- 72 job classes: deployment, server health (ServerManagerJob fan-out every minute), proxy/DNS/TLS, backups and
  scheduled tasks (ScheduledJobManager: backups, tasks, volume-backups, docker-cleanups), transfers, GitHub runners,
  instance maintenance, notifications, Stripe (cloud).
- Scheduler (`app/Console/Kernel.php`): hourly token pruning and cleanups, every-minute `ServerManagerJob` and
  `scheduled:dispatch`, update checks on `update_check_frequency`, auto-update on `auto_update_frequency`, etc.
- Async SSH work (`remote_process`) writes streaming output into `activity_log.description` as JSON.

## 6. Frontend coupling

- 272 Livewire components, 511 Blade views (289 with `wire:` directives), ~193 GET routes mapping straight to
  full-page Livewire components. State lives server-side and mutations go through `/livewire/update` RPC with
  `#[Locked]` properties and `$this->authorize()` inside components.
- Only the Svelte canvas pages consume JSON, through `/canvas/api/*`, which maps onto the **same**
  `App\Http\Controllers\Api\*` controllers as the REST API using a session-bridged token.
- **Conclusion:** the existing frontend cannot be retained on a non-PHP backend. Options are recorded in ADR 0003.

## 7. Proxy, TLS, real-time, services, backups

- Proxy is Traefik v3.7 (default) or caddy-docker-proxy, configured with a generated compose file on each server
  and Docker labels on each container; Traefik ACME HTTP challenge for TLS.
- Reverb private channels `team.{id}` and `user.{id}` for status events; deployment logs are polled.
- 9 standalone database types started from generated compose files; 386 one-click service templates
  (`templates/compose`) with magic `SERVICE_*` variables.
- Database backups dump via `docker exec`, upload with MinIO `mc` inside the helper container on the managed server.

## 8. Install / upgrade

- `scripts/install.sh` creates `/data/coolify/{source,ssh/keys,ssh/mux,applications,databases,services,backups,proxy,...}`,
  generates `APP_KEY`, DB/Redis passwords and Pusher credentials once, creates the localhost SSH key pair, then runs
  `upgrade.sh`, which recreates containers via `docker compose` run inside the helper image.
- The first registered user becomes user id 0, owner of team 0, within a 10-minute root setup window.

## 9. Tests and CI

- Pest: Unit 348 files / 2,902 cases, Feature 916 files / 7,975 cases, v4 45 files (mostly browser).
- Default test DB is SQLite in memory (`DB_CONNECTION=testing`), which means upstream tests do not fully exercise
  Postgres semantics.
- **No upstream CI workflow runs the test suite** (18 workflows: image builds, housekeeping, changelog).
  Upstream tests therefore cannot be assumed green at any given commit; this matters for the migration engine,
  which treats them as specifications, not as a passing oracle.

## 10. Implications for coolify-rs

1. The REST API and the Postgres schema are the stable contracts; both are adopted as-is (ADR 0002, 0004).
2. Laravel-specific storage formats must be implemented: encrypter (done, verified), Sanctum tokens (done),
   bcrypt `$2y$` hashes, session payloads (only if sessions are shared; not planned).
3. Remote execution must be redesigned with a typed command builder and argument-level escaping, while staying
   SSH-based so the operational model (no Docker socket in the control plane) is preserved.
4. Host-key verification is a documented upstream weakness; coolify-rs should support TOFU pinning (see KNOWN_LIMITATIONS.md).
5. The UI is a separate project; until it exists, coolify-rs runs side-by-side as an API server only.
