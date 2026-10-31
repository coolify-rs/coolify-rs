# Known Limitations and Divergences

Everything here is deliberate or known; none of it is hidden behind a "compatible" claim.

## Scope

- Upstream API operations that are not ported answer `501 {"message":"This endpoint is not implemented in coolify-rs
  yet."}`; `scripts/parity.py --missing` lists them.
- Not every upstream API operation is implemented (`scripts/parity.py`). Others answer `501 {"message":"This endpoint is not
  implemented in coolify-rs yet."}`.
- The web UI covers the dashboard, projects, environments, private keys, API, cloud and integration tokens, cloud-init scripts,
  notifications, tags, shared variables, S3 storage, teams, members, invitations, profile, 2FA enrollment, passkey
  management and appearance.
  Not available yet: registration and the root setup window, password reset and forced password change, OAuth/SSO,
  instance settings, and every page for servers, resources, sources, storages, notifications and tags (they open a "not
  available in coolify-rs yet" page).
- Within the ported pages: no canvas views, global search, project icons or custom color themes;
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
  features outside the API, `/resources`, `/servers/{uuid}/resources` and `/domains` for a team or server with services
  (501 until services are ported), registry users of build servers and services, `delete_from_provider` for Hostinger
  (upstream only disables auto renewal there). Throttling of validations exists only on Cloud upstream.
- Server jobs differ from upstream as follows: `remote_process` activities have no `callEventOnFinish` queue, the proxy
  file upload goes through the shell instead of scp, maintenance pages are written for "no resource in maintenance"
  only, the CA certificate's subject key identifier is SHA-256 based, `PushServerUpdateJob` and `GetContainersStatus` from a Sentinel push are not run (the
  heartbeat is), `RegenerateSslCertJob` after a CA change is not run, marks upstream keeps in Redis are per process. The
  scheduler (`ServerManagerJob`) runs only with `COOLIFY_RS_SERVER_SCHEDULER=1`, because an instance sharing its database
  with upstream must not check servers twice, and evaluates cron expressions in UTC. Compose merge tags (`!reset`,
  `!override`) in a proxy configuration are ignored when reading it, but traffic analytics does not rewrite a stored
  configuration.
- Validation jobs run inside the coolify-rs process: a restart drops a running validation (upstream's queue keeps it).
- Databases (all 9 engines; API, proxy, SSL, backups, imports; the pages for them are a later task): the API routes of
  `/databases/{uuid}/storages/{storage_uuid}/backups` (volume backups, `VolumeBackupJob`) are not ported (501).
  `PATCH /databases/{uuid}/envs/bulk` applies `key`, `value`, `is_literal`, `is_multiline`, `is_shown_once` and `comment`;
  upstream's mass assignment also takes other fillable columns of an item, including `resourceable_*`.
  Secret manager `{{vault.X}}` references stop a start with a message. Upstream stores `config_hash` only when the database
  page is open after a start; coolify-rs stores it after every start. The ClickHouse "current data is not in the data
  volume" check is not ported. `Str::slug` in container labels does not transliterate non-ASCII names.
- Notifications (`crates/coolify-jobs/src/notifications`): delivery runs on tokio tasks, so queued retries are lost when
  the process restarts. Not sent yet, because their job is not ported: `RestartLimitReached` for applications, previews
  and databases, `ContainerRestarted` and the disk usage check of `PushServerUpdateJob`, `BackupMissing` and the
  `VolumeBackup/*` notifications, tasks of applications and the missed runs of `ScheduledJobDeliveryService`,
  `ApiTokenExpiringNotification`, `SslExpirationNotification`, `ForceDisabled` and `ForceEnabled` (`ServerLimitCheckJob`,
  cloud), the `GeneralNotification` of `DeleteResourceJob` and server transfers, the combined result of a deployment to
  several servers and previews. `StatusChanged` is ported but upstream never sends it. Its mail view reads a variable
  `toMail` does not pass: tinker renders an empty link, a queue worker would throw. Request headers: reqwest adds
  `Accept: */*`, which Guzzle does not send. SMTP mail is the same in from, to, subject and body but has other MIME framing
  (Symfony and lettre differ in `Message-ID`, encoding choice and header folding). `SafeWebhookUrl` at send time uses the
  system resolver, not `custom_dns_servers`. `mb_strwidth` for the 1000 column limit of a commit message uses a table of
  wide ranges, not mbstring's. The internal notification of `EmailChannel` does not include the settings JSON. Telegram
  and Pushover could only be compared on the reference through `Http::fake` (their hosts are fixed there).
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

## Keys, notifications, tags, shared variables and S3 pages

- Integration tokens for Cloudflare: `IntegrationTokenValidator` for Cloudflare and `CloudflareDnsProvider::syncZones` are not
  ported. Creating such a token shows the provider's validation error, and "Refresh zones" shows an error. Stored Cloudflare
  tokens can be listed, renamed and deleted.
- Cloud token "Validate" and "Validate and add" call the real provider APIs, as upstream. They were only exercised up to the
  form validation, since nothing may go to an external service during tests.
- Send-test on Pushover and Telegram calls the real services, as upstream, so the comparison flows leave them out. The Telegram
  path shares `notify_test` with `compat/diff/notifications_e2e.py`.
- A failing SMTP send in the test email: only "connection refused" uses Symfony's message. Other transport errors (DNS, TLS,
  authentication) show lettre's text.
- S3 connection failures do not send `emails.s3-connection-error`, and `unusable_email_sent` is only reset, not set.
- `Canvas` links (tags, shared variables) lead to the not-available page, as the canvas pages are not ported. Their toggles
  are shown, and `prefersCanvasView` is not honoured.
- Upstream glitches reproduced on purpose: the Issued tokens list hides tokens created since the page load (Alpine list), and
  the new-tag modal shows no validation error (`wire:ignore`). Not reproduced: the variable count of the shared variables table
  lags one request behind after an add, and rows of the S3 resources table without `wire:key` show stale data after a move.
- Pages behind `viewAny` (cloud tokens, cloud-init scripts, integration tokens) stay empty for a member instead of upstream's
  403 page.
- `SharedVariables` overview: `{{scope.KEY}}` matching decrypts every resource variable of the team on each visit, as upstream.

## Team and profile pages

Ported and compared with the reference (`compat/ui/flow_teams.mjs`, `flow_teams2.mjs`, `flow_profile.mjs`,
`flow_whatsnew.mjs`): new team, danger zone, member actions, invitations by link and email and their acceptance
(`/invitations/{uuid}`, `/auth/link`), team and profile avatars, audit log, instance users, `/profile` with email
change, password, two-factor enrollment and recovery codes, passkey management, account deletion, `/select-team`,
`/user/confirm-password` and the user menu's What's New.

- A valid new password is not checked against Have I Been Pwned: upstream's `uncompromised()` rule asks an external
  service. The other production rules apply. A successful password change could only be tried on coolify-rs for the
  same reason, so it is compared with the PHP and not with reference rows.
- Avatars are stored on the local disk (`{COOLIFY_RS_DATA_DIR}/images`). Upstream's `avatar_storage_type = s3` is not
  used for writing, and a row with an S3 avatar answers 404 in coolify-rs. A file stored by upstream's container is not
  visible to coolify-rs unless the disks are shared.
- Deleting a user through `/team/admin` is refused with a message when a team only that user belongs to still owns
  servers or projects. Upstream force-deletes them; their deletion hooks are not all ported.
- Deleting a team or account removes keys, tags, shared variables, S3 storages and sources with their rows, but writes
  no audit events for those rows and does not remove a key's file (upstream's model events do).
- The confirm-password page has no "Confirm with a passkey" button, and passkey sign-in and the use of a passkey at
  login are not part of this page set. Passkey `last_used_at` therefore stays empty unless upstream signs in with it.
- A password confirmation is remembered per session and skips the password step only on the pages listed above. Older
  pages (server, application and database deletion) still ask for the password within upstream's three hours.
- What's New reads `{COOLIFY_RS_DATA_DIR}/changelogs/*.json`. `PullChangelog` is not ported, because it fetches them
  from upstream's CDN, so a new install shows no updates. Unread counts are not cached.
- The user menu's Feedback entry is not ported: `Livewire\Help` sends to upstream's support address or Cloud API.
- Forced password change after a magic link is the job of the `/force-password-reset` page. Until that page exists a
  new invitee lands on the dashboard after accepting.

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
| Scheduled tasks on their cron schedule | only `POST .../execute` runs a task | the scheduler is not ported |
| `PullTemplatesFromCDN`, `get_service_templates(force: true)` | the shipped file, or `COOLIFY_RS_TEMPLATES_DIR` | no scheduler |
| `CleanupDocker` after a stop, maintenance pages, secret managers (`{{vault.*}}` stops a start), managed DNS cleanup on a changed domain, `parseDockerComposeFile` for parsing versions below 3 | not run | their subsystems are not ported |
| Order of `environment_variables`, `applications`, `databases` | ordered by id | upstream has no `ORDER BY`: Postgres returns heap order, which changes after updates |
