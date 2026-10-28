# Known Limitations and Divergences

Everything here is deliberate or known; none of it is hidden behind a "compatible" claim.

## Scope

- Only 33 of 321 upstream API operations are implemented. Others answer `501 {"message":"This endpoint is not
  implemented in coolify-rs yet."}`.
- The web UI covers the dashboard, projects, environments, private keys, team settings, members and appearance.
  Not available yet: registration and the root setup window, password reset and forced password change, 2FA
  enrollment and recovery codes, passkeys, OAuth/SSO, team creation and invitations, instance settings, API token
  management, and every page for servers, resources, sources, storages, notifications and tags (they open a "not
  available in coolify-rs yet" page).
- Within the ported pages: no canvas views, global search, project icons, team avatars or custom color themes;
  applications and services in an environment are counted, not listed; the dashboard's server status does not include
  the Traefik-outdated and Sentinel checks; table/grid choices are remembered per browser, not per user.
- Browser sessions live in memory: a restart signs everyone out. Session limits: 10,000 sessions, 7-day idle
  (`SESSION_LIFETIME`), 30-day absolute.
- Users must sign in with an existing Coolify account (created in upstream Coolify).
- Servers: validation works (API and UI); nothing else connects to a server yet. `instant_validate`, validating with
  `install=true` (installing prerequisites, Docker and the proxy), validating a server linked to Vultr, DigitalOcean or
  Hostinger, changing `proxy_type`, `DELETE ?force=true` on a server with resources, `delete_from_provider`, deleting
  a transferred server and `GET /servers/{uuid}?resources=1` answer 501; `/servers/{uuid}/resources`, `/domains` and
  the other server sub-routes are not ported.
- Validation from the UI does not install anything: the "this will install or update Docker" question upstream asks
  before revalidating is not shown, missing prerequisites or Docker end the validation with upstream's
  "install them manually" messages, and after success coolify-rs neither fetches the server details nor starts the
  proxy or Sentinel (upstream does all three).
- Validation jobs run inside the coolify-rs process: a restart drops a running validation (upstream's queue keeps it).
- Databases: every engine but SQLite. Environment variables are read at start (shared `{{team.X}}`-style references
  resolve; secret manager `{{vault.X}}` references stop the start with a message), but the Environment Variables page is
  not ported. Upstream stores `config_hash` only when the database page is open after a start; coolify-rs stores it after
  every start. The ClickHouse "current data is not in the data volume" check is not ported. Making a database public
  (TCP proxy), SSL, init script upload, file storages, backups, imports, container logs, terminal, metrics, tags, custom Docker run options at start, and
  `PATCH /databases/{uuid}` are not ported (501, or disabled in the UI). After a start coolify-rs does not run Docker
  cleanup, and `Str::slug` in container labels does not transliterate non-ASCII names.
  Reachability changes do not send the Reachable/Unreachable notifications.
- Applications in the UI: Docker image applications and git applications from a public repository (new-resource
  "Docker Image" and "Public Git Repository" forms with Smart Scan and env file import, General page for the Docker
  image, Dockerfile, Static, Nixpacks and Railpack build packs, Git Source page, Deployment Logs list and log viewer,
  Danger zone, heading actions). Not ported in the UI: the other new-resource types (private repositories through a
  GitHub App or a deploy key), the Docker Compose sections of the General page (a Compose application shows the "not
  available" page; it can be created but not deployed), HTTP Basic Authentication, custom Docker run options,
  pre/post-deployment commands, building on a build server (selecting it says so), a Dockerfile stored on the
  application (no repository), switching the Git source to a GitLab or Gitea App, "Force start" of a queued
  deployment and the other application pages (Domains, Environment Variables, Storage, Advanced, Healthcheck,
  Runtime Logs, Terminal, …). The General page's instant saves (site type, labels) store only the setting they
  change, while upstream's `syncData()` also persists unsaved form edits when it regenerates the Nginx
  configuration. Log export does not apply upstream's secret redaction beyond what the deployment already wrote
  redacted.
- Creating git applications (`POST /applications/public`, `/private-deploy-key`, `/private-github-app`): the row is
  written as upstream writes it, but for `build_pack = dockercompose` without `instant_deploy` upstream's queued
  `LoadComposeFile` job (clone the repository, read and parse the compose file, store `docker_compose_raw`,
  `docker_compose`, variables and volumes) is not ported, so the compose columns stay empty until a deployment.
  `docker_compose_domains`, `watch_paths`, `custom_nginx_configuration`, `use_build_server`,
  `connect_to_docker_network`, HTTP basic auth, custom labels, Docker run options, pre/post-deployment commands,
  manual webhook secrets, GPU and container-name settings and tags answer 501 when sent with a value. The GitHub App
  repository check does not apply upstream's `SafeExternalUrl` private-network policy to the GitHub API URL, and the
  GitHub App path is not yet verified against a real GitHub App (only up to the lookup of the App).
- At startup upstream (`app:init`) marks every in-progress and queued deployment as failed; coolify-rs marks only the
  in-progress deployments its own host was running (`horizon_job_worker`), so an upstream instance sharing the database
  keeps its deployments. Queued deployments are left queued.
- Server status in the UI and in `/ui/*` omits the Sentinel and Traefik-update checks.
- `sentinel_custom_url` for a new server falls back to the request's scheme and host; `X-Forwarded-*` count only
  from trusted proxies (upstream trusts every proxy).
- The new-server page offers only "IP address or domain" (no cloud provisioning).
- Deployments: only the Docker image build pack. Git build packs (Dockerfile, static, Railpack, Nixpacks, Compose), previews, additional servers, build servers, registry pushes, basic auth, pre/post-deployment commands and custom Docker options are not ported (501). `current_process_id` stays empty, and commands that upstream feeds on stdin carry their input inside the script. Jobs run in the coolify-rs process: a restart drops a running deployment.
- No proxy management, backups, scheduler, notifications or webhooks yet. SSH runs without
  connection multiplexing (one connection per command).
- coolify-rs cannot bootstrap a new installation: it requires a database created by upstream Coolify at the baseline
  (`main` @ b5823c7, 473 migrations); it runs no upstream DDL. A 4.5.0 database is served read-only until upstream
  is upgraded.

## API divergences (implemented routes)

| Area | Upstream | coolify-rs | Reason |
|---|---|---|---|
| Client IP for the API allowlist | `TrustProxies('*')`: `X-Forwarded-For` trusted from anyone, so the leftmost (client-controlled) entry is used | peer address unless `COOLIFY_RS_TRUSTED_PROXIES` is set; then rightmost untrusted hop | security fix: the allowlist is spoofable upstream when the app is reachable without a proxy |
| `CF-Connecting-IP` on Coolify Cloud | honoured for any peer when `isCloud()` | not applicable | coolify-rs has no cloud mode; the self-hosted rule (Cloudflare peers only) is implemented |
| Rate limiting | `throttle:api` 200/min, `X-RateLimit-*` headers | none | not implemented yet |
| Audit events | `audit_events` rows for environment changes, auth failures, server validation | not written (remote commands and validations are logged to the `audit` tracing target) | not implemented yet |
| SSH host keys | not verified (`StrictHostKeyChecking=no`, `UserKnownHostsFile=/dev/null`) | pinned per server on first use; a changed key fails with `Host key verification failed: the server's SSH host key changed ...` (new fingerprint and how to accept it); the pin is dropped when the IP or port changes or the server is deleted | security fix (ADR 0009); `COOLIFY_RS_SSH_HOST_KEYS=ignore` restores upstream's behaviour |
| SSH key files | written once to `storage/app/ssh/keys/ssh_key@<uuid>` and kept | written 0600 for the length of a command, then removed; errors still name the key `ssh_key@<uuid>` | key material stays off disk |
| SSH options | upstream's options with the operator's environment | adds `-F /dev/null`, `BatchMode`, `IdentitiesOnly`, no agent, no forwarding; environment cleared | a hostile server or operator config cannot change behaviour |
| Remote output size | unbounded | stdout capped at 16 MiB, stderr at 1 MiB | memory safety against a hostile server |
| Validation throttle on Coolify Cloud | per-IP and per-team limits | none | coolify-rs is self-hosted only (upstream applies none there either) |
| Environment details with resources | applications/databases/services serialized with sensitive-field rules | `501` | refusing is safer than partial or unfiltered data |
| `abort()` responses for clients that do not ask for JSON | Laravel's HTML error page | the JSON body upstream sends to JSON clients (pretty-printed, as upstream) | API clients |
| Database start/stop/restart/delete jobs | Redis queue (Horizon), activity rows | tokio tasks in the coolify-rs process; same activity rows | a restart of coolify-rs drops a running job |
| Validation of non-string values for string fields | Laravel continues evaluating min/max/regex | only "must be a string" is reported | rare; documented |
| Concurrent duplicate environment names | unique index violation → 500 | 409 | improvement |
| Multi-row writes (project create/delete) | model hooks, no transaction | single transaction | improvement; same end state |
| Unknown columns from newer upstream releases | serialized automatically (`$hidden` is a deny-list) | dropped from output and logged until reviewed in `records.rs` | prevents leaking secret columns added upstream |
| Writes when the DB schema is newer than this build | n/a | read-only mode (`503`) unless `COOLIFY_RS_ALLOW_UNKNOWN_SCHEMA=true` | schema guard |
| Invalid `private_key` on `PATCH /security/keys/{uuid}` without `Accept: application/json` | model `ValidationException` → **302 redirect** to the app URL (upstream defect) | 422 JSON, the body upstream returns to JSON clients | API clients should not receive HTML redirects |

## Upstream defects observed (not reproduced as security issues, recorded for review)

- `Project::deleting` / `Environment::deleting` iterate a relation object instead of its results, so shared environment
  variables are likely **not** deleted (orphans). coolify-rs matches upstream's end state (does not delete them) pending
  runtime confirmation; candidate remediation once confirmed.
- SSH host keys are not verified (`StrictHostKeyChecking=no`). coolify-rs pins them (see API divergences).

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
