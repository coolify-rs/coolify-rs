# Known Limitations and Divergences

Everything here is deliberate or known; none of it is hidden behind a "compatible" claim.

## Scope

- Upstream API operations that are not ported answer `501 {"message":"This endpoint is not implemented in coolify-rs
  yet."}`; `scripts/parity.py --missing` lists them.
- Not every upstream API operation is implemented (`scripts/parity.py`). Others answer `501 {"message":"This endpoint is not
  implemented in coolify-rs yet."}`.
- The web UI covers the dashboard, projects, environments, private keys, team settings, members and appearance.
  Not available yet: registration and the root setup window, password reset and forced password change, 2FA
  enrollment and recovery codes, passkeys, OAuth/SSO, team creation and invitations, instance settings, API token
  management, and every page for servers, resources, storages, notifications and tags (they open a "not
  available in coolify-rs yet" page).
- Within the ported pages: no canvas views, global search, project icons, team avatars or custom color themes;
  applications and services in an environment are counted, not listed; the dashboard's server status does not include
  the Traefik-outdated and Sentinel checks; table/grid choices are remembered per browser, not per user.
- Browser sessions live in memory: a restart signs everyone out. Session limits: 10,000 sessions, 7-day idle
  (`SESSION_LIFETIME`), 30-day absolute.
- Users must sign in with an existing Coolify account (created in upstream Coolify).
- Servers (API): sub-routes, destinations, proxy, Sentinel, Docker cleanup, log drains, registries, shared variables,
  install, cloud providers and cloud-init scripts are ported (backend; their UI pages are not, and UI validation still ends
  with the "install them manually" messages). Not ported: `POST /servers/import`, `GET /servers/import/check`,
  `POST /servers/{uuid}/migrate`, `POST /servers/{uuid}/transfer/complete` (the 7,000 line transfer services; also deleting
  a transferred server), `DELETE ?force=true` on a server with resources, `/servers/{uuid}/cloudflare-tunnel` account
  features outside the API, `delete_from_provider` for Hostinger
  (upstream only disables auto renewal there). Throttling of validations exists only on Cloud upstream.
- Server jobs differ from upstream as follows: `remote_process` activities have no `callEventOnFinish` queue, the proxy
  file upload goes through the shell instead of scp, maintenance pages are written for "no resource in maintenance"
  only, the CA certificate's subject key identifier is SHA-256 based, notifications (high disk usage, Traefik outdated,
  patches, cleanup) are not sent, `PushServerUpdateJob` and `GetContainersStatus` from a Sentinel push are not run (the
  heartbeat is), `RegenerateSslCertJob` after a CA change is not run, marks upstream keeps in Redis are per process. The
  scheduler (`ServerManagerJob`) runs only with `COOLIFY_RS_SERVER_SCHEDULER=1`, because an instance sharing its database
  with upstream must not check servers twice, and evaluates cron expressions in UTC. Compose merge tags (`!reset`,
  `!override`) in a proxy configuration are ignored when reading it, but traffic analytics does not rewrite a stored
  configuration.
- Server and destination pages: ported are destinations (index, new, show, resources, danger), Advanced, Private Key, CA
  Certificate, Swarm, server Destinations, Resources (no canvas toggle), Proxy (configuration, dynamic configurations, TLS
  list, logs, start/stop/restart), Sentinel (configuration, logs), Docker Cleanup with executions, Images, Log Drains,
  Registries (server and `/registries`), Terminal Access and the settings of Traffic Analytics. Not ported yet (the page
  says "not available"): Metrics/Charts, Cloudflare Tunnel, GitHub Runners, Security Patches, Transfer, the
  `By{DigitalOcean,Hetzner,Hostinger,Vultr}` forms, Cloud Token, the Terminal page. Within the ported pages: deleting TLS
  certificates and acme.json backups, enabling traffic analytics (`ConfigureTrafficAnalytics`) and the analytics charts, the
  "Proxy Startup Logs" dialog, the Docker cleanup "may be stalled" warning, and the toolbar of the shared log viewer (search,
  filter, copy, download) are missing. Saving a CA certificate checks it against P-521 keys only and does not run
  `RegenerateSslCertJob`.
- Validation jobs run inside the coolify-rs process: a restart drops a running validation (upstream's queue keeps it).
- Databases (all 9 engines; API, proxy, SSL, backups, imports; the pages for them are a later task): the API routes of
  `/databases/{uuid}/storages/{storage_uuid}/backups` (volume backups, `VolumeBackupJob`) are not ported (501).
  `PATCH /databases/{uuid}/envs/bulk` applies `key`, `value`, `is_literal`, `is_multiline`, `is_shown_once` and `comment`;
  upstream's mass assignment also takes other fillable columns of an item, including `resourceable_*`.
  Secret manager `{{vault.X}}` references stop a start with a message. Upstream stores `config_hash` only when the database
  page is open after a start; coolify-rs stores it after every start. The ClickHouse "current data is not in the data
  volume" check is not ported. `Str::slug` in container labels does not transliterate non-ASCII names.
  Reachability, backup and TCP proxy failure notifications are not sent.
- Deliberate divergence: upstream's `POST` and `PATCH /databases/{uuid}/storages` answers carry the relation the model
  loaded on the way (`resource` or `service`): the whole database with `destination.server`, including the saved proxy
  configuration, also for tokens without `read:sensitive`. coolify-rs leaves this relation out.
- Clone with `clone_volumes`: same-server copies only, onto a plain `local` volume; `cloneRemoteVolume` and the source
  volume's driver options are not ported. `migrate` answers 404 as upstream does in production.
- S3: `testConnection` signs its own `ListObjectsV2`; when the endpoint answers with an error, the message is shaped like
  Flysystem's but not identical to the AWS SDK's text. The S3 endpoint's DNS is not pinned for the request as
  `httpClientOptions` does. The "S3 connection error" email is not sent.
- Docker cleanup after a stop runs the prune commands of `CleanupDocker`; `cleanupApplicationImages` and the buildx prune
  are not run there.
- Database imports run in the coolify-rs process: a restart drops a running import, and the stop-after-transport-failure
  path of `DatabaseImportCleanup` is not ported. `fileContainsPostgresqlProgramExecution` scans plain uploads locally;
  gzip uploads rely on the scan in the container. The upload's `mime_type` is derived from the file's signature.
- Scheduled backups run through `Jobs::dispatch_scheduled_backup(id)`; cron timing belongs to the scheduler. Backup
  overlap protection and the reservation live in the process.
- SSL certificates are not regenerated when they near expiry (`RegenerateSslCertJob`).
- Applications in the UI: Docker image applications and git applications from a public repository (new-resource
  "Docker Image" and "Public Git Repository" forms with Smart Scan and env file import, General page for the Docker
  image, Dockerfile, Static, Nixpacks and Railpack build packs, Git Source page, Deployment Logs list and log viewer,
  Danger zone, heading actions). Not ported in the UI: the other new-resource types (private repositories through a
  GitHub App or a deploy key), the Docker Compose sections of the General page (a Compose application shows the "not
  available" page; it can be created but not deployed), HTTP Basic Authentication, custom Docker run options,
  pre/post-deployment commands, building on a build server (selecting it says so), a Dockerfile stored on the
  application (no repository), "Force start" of a queued
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
  path is not yet verified against a real GitHub App.
- Docker Compose applications deploy (build, pull, `docker compose up`, build fingerprint reuse, raw mode). Not ported:
  custom Compose start/build commands, "preserve repository", preview deployments, configuration files with content
  (file storages Coolify writes), build secrets. The startup cleanup scopes interrupted deployments by host name, so
  two coolify-rs instances on one host would mark each other's running deployments as failed.
- At startup upstream (`app:init`) marks every in-progress and queued deployment as failed; coolify-rs marks only the
  in-progress deployments its own host was running (`horizon_job_worker`), so an upstream instance sharing the database
  keeps its deployments. Queued deployments are left queued.
- `sentinel_custom_url` for a new server falls back to the request's scheme and host; `X-Forwarded-*` count only
  from trusted proxies (upstream trusts every proxy).
- The new-server page offers only "IP address or domain" (cloud provisioning exists in the API only).
- Deployments: only the Docker image build pack. Git build packs (Dockerfile, static, Railpack, Nixpacks, Compose), previews, additional servers, build servers, registry pushes, basic auth, pre/post-deployment commands and custom Docker options are not ported (501). `current_process_id` stays empty, and commands that upstream feeds on stdin carry their input inside the script. Jobs run in the coolify-rs process: a restart drops a running deployment.
- No backups, notifications or webhooks yet. SSH runs without
- No proxy management, scheduler, notifications or webhooks yet. SSH runs without
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

## One-click services

The API, the parser, the actions and status polling are ported (`compat/diff/services_diff.py`). Not ported:

| Upstream | coolify-rs | Why |
| --- | --- | --- |
| Livewire pages `Project/Service/*`, `one-click-service-*` in `Project/Resource/Create` | not available | later task |
| `PUT`/`DELETE /services/{uuid}/storages/{storage_uuid}/backups`, `.../backups/run` | `501` | S3 volume backups are not ported |
| `POST /services/{uuid}/databases/{database_uuid}/imports`, `.../imports/uploads`, `GET .../imports/{activity_id}` | `501` | database imports are not ported |
| `POST /services/{uuid}/clone` with `clone_volumes: true` | `501` | `VolumeCloneJob` is not ported |
| `PATCH /services/{uuid}/databases/{database_uuid}` that changes `is_public` | `501` | `StartDatabaseProxy` / `StopDatabaseProxy` are not ported |
| `POST /services/{uuid}/migrate` in a dev instance | `404`, as upstream outside dev | `MigrateResourceToDestination` only runs in dev upstream |
| Scheduled tasks on their cron schedule | only `POST .../execute` runs a task | the scheduler is not ported; no notifications |
| `PullTemplatesFromCDN`, `get_service_templates(force: true)` | the shipped file, or `COOLIFY_RS_TEMPLATES_DIR` | no scheduler |
| `CleanupDocker` after a stop, maintenance pages, secret managers (`{{vault.*}}` stops a start), managed DNS cleanup on a changed domain, restart limit notifications, `parseDockerComposeFile` for parsing versions below 3 | not run | their subsystems are not ported |
| Order of `environment_variables`, `applications`, `databases` | ordered by id | upstream has no `ORDER BY`: Postgres returns heap order, which changes after updates |

## Git sources and webhooks

Verified against `compat/diff/fake_git_provider.py` and the reference, not against a real GitHub App, GitLab or Gitea:
the API (`git_sources_diff.py`), signed webhooks (`webhooks_diff.py`), deployments (`git_source_deploy_diff.py`), the
manifest and OAuth browser flows (`compat/ui/source_flows.mjs`) and the private repository forms
(`compat/ui/private_forms.mjs`). Deploy keys ran against a real SSH mirror on the test VM.

- Preview deployments: webhooks create the `application_previews` row and queue the deployment like upstream, but the
  deployment job has no preview pipeline yet: it fails the queued preview with "Preview deployments are not supported
  by coolify-rs yet." and comments on the pull request. Closing a pull request cancels, removes containers and deletes
  the row. Compose previews only copy `docker_compose_domains` (`generate_preview_fqdn_compose()` needs the preview
  parser).
- `workflow_job` deliveries (GitHub Actions runners) answer without doing anything: runners are not ported.
- `TransferFinishChecklist::recordWebhook()` is not ported.
- GitHub App setup states, OAuth states and the manual webhook failure counters live in the process: a restart drops
  them (start the flow again).
- Failure pages of the webhooks are JSON, upstream renders its HTML 500 page (same status).
- `SimpleDockerfile` does not run `parseHealthcheckFromDockerfile(isInit: true)`.
- A failed deployment does not log "Deployment failed. Removing the new version of your application." or remove the
  new container (all build packs).
- New Docker Compose services are created through the `POST /services` code; the service page they open is another
  area.
- The CSP allows forms to post to any http(s) host, because the manifest flow posts to the GitHub (Enterprise) site.
