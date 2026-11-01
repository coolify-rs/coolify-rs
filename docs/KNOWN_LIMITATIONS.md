# Known Limitations and Divergences

Everything here is deliberate or known; none of it is hidden behind a "compatible" claim.

## Scope

- Upstream API operations that are not ported answer `501 {"message":"This endpoint is not implemented in coolify-rs
  yet."}`; `scripts/parity.py --missing` lists them.
- A path upstream does not have goes home, as `Route::any('/{any}')` does. A section of an application, database or service
  that has no page yet opens a "not available in coolify-rs yet" page; `scripts/parity.py --missing` lists what is left.
- Within the ported pages: no custom color themes; the dashboard's server status does not include
  the Traefik-outdated and Sentinel checks; table/grid choices are remembered per browser, not per user.
- Browser sessions live in memory: a restart signs everyone out. Session limits: 10,000 sessions, 7-day idle
  (`SESSION_LIFETIME`), 30-day absolute.
- Users must sign in with an existing Coolify account (created in upstream Coolify).
- Servers (API): sub-routes, destinations, proxy, Sentinel, Docker cleanup, log drains, registries, shared variables,
  install, cloud providers and cloud-init scripts are ported (backend; of the UI, the provider forms and the server's Cloud
  Token page are, the Security pages for tokens and scripts are not, and UI validation still ends with the "install them
  manually" messages). Not ported: `POST /servers/import`, `GET /servers/import/check`,
  `POST /servers/{uuid}/migrate`, `POST /servers/{uuid}/transfer/complete` (the 7,000 line transfer services; also deleting
  a transferred server), `DELETE ?force=true` on a server with resources, `/servers/{uuid}/cloudflare-tunnel` account
  features outside the API, `delete_from_provider` for Hostinger
  (upstream only disables auto renewal there). Throttling of validations exists only on Cloud upstream.
- Server jobs differ from upstream as follows: `remote_process` activities have no `callEventOnFinish` queue, the proxy
  file upload goes through the shell instead of scp, maintenance pages are written for "no resource in maintenance"
  only, the CA certificate's subject key identifier is SHA-256 based, marks upstream keeps in Redis are per process.
  `PushServerUpdateJob` goes through the code of `GetContainersStatus`, so `additional_servers_status` is not updated from a
  push, and containers on a Swarm are not told apart. `spawn_status_poller` still runs `GetContainersStatus` every minute on
  every server, also with the scheduler off and also when Sentinel is live, where upstream's `ServerCheckJob` skips it.
  Compose merge tags (`!reset`,
  `!override`) in a proxy configuration are ignored when reading it, but traffic analytics does not rewrite a stored
  configuration.
- Server and destination pages: ported are destinations (index, new, show, resources, danger), Advanced, Private Key, CA
  Certificate, Swarm, server Destinations, Resources, Proxy (configuration, dynamic configurations, TLS
  list, logs, start/stop/restart), Sentinel (configuration, logs), Docker Cleanup with executions, Images, Log Drains,
  Registries (server and `/registries`), Terminal Access, Security Patches, Cloudflare Tunnel, GitHub Runners and the settings of Traffic Analytics. Not ported yet (the page
  says "not available"): nothing. Log drains: the secret fields stay empty for a
  member, where upstream's Livewire snapshot carries them. Proxy status toasts come from real-time events upstream
  (`ProxyStatusChangedUI`); the UI here polls, and neither side shows them when the real-time service is down.
  Traffic analytics: the charts of the server page, of `/analytics`, the dashboard widget and the application and service analytics pages are ported (Traefik verified, the Caddy proxy not run). The dashboard widget stays empty in Chrome on both sides: upstream's lazy placeholder has no box, so it never enters the viewport; its data is compared directly. After a filter change, rows of a list with equal values can
  stand in another order than on the reference, which keeps the previous DOM order for ties; a fresh load has the same order.
- Validation jobs run inside the coolify-rs process: a restart drops a running validation (upstream's queue keeps it).
- Databases (all 9 engines; API, proxy, SSL, backups, imports and the `Project/Database/*` pages): sections that only
  render `Project/Shared/*` are placeholders until the shared branch is merged. Starting a ClickHouse database with an
  unnamed data volume is not blocked (the page warns as upstream does).
- Service database backups and imports run the standalone code through a `ServiceDatabase` row (`dbs::service_database_row`).
  A database whose image or custom type names no known engine has no such row, so its pages answer 404 and its schedule
  ends without a run. Reading MongoDB credentials from the container and the MySQL and MariaDB branches are ported but
  only PostgreSQL ran on the VM.
- Storage backups (`PUT`, `DELETE`, `POST .../run` on `/{applications,databases,services}/{uuid}/storages/{storage_uuid}/backups`):
  `VolumeBackupJob` runs for a manual run or "Back up now". The scheduler starts it too (`schedule_sources.rs`), and every 5 minutes one
  attempt of `Jobs::retry_volume_recovery` for an execution that waits for recovery. A run
  that finds another run of the same schedule in progress is dropped using the executions table, not a cache lock.
  The `VolumeBackup/*` notifications are sent; `compat/diff/scheduler_window.py` sees them on a local channel.
- `/download/backup/{id}` and `/download/volume-backup/{id}` read over `cat` on the ssh connection instead of SFTP. Through
  the `scripts/ssh-via-orb.sh` wrapper `orb` truncates large output (about 4 to 5 MB) with exit code 0, so large downloads
  were not verified; small ones were byte-identical. The S3 download storage of `PreparesBackupDownloads`
  (`databases/backup_download.rs`) is ported but has never run: selecting it needs `instance_settings`, which test runs
  leave alone, and the local S3 fake is a private endpoint both sides refuse. Its pending list and the results are kept
  in memory, and a restart forgets a pending download and its delayed object delete.
- `S3 endpoint is not allowed` messages link to the relative `/settings/advanced`; upstream writes the absolute URL.
- S3 uploads and restores could not run against the local MinIO in the page comparison: both sides refuse a private
  endpoint unless `webhook_allowed_internal_hosts` names it, and that setting stayed unchanged. The refusal itself matches.
  `PATCH /databases/{uuid}/envs/bulk` applies `key`, `value`, `is_literal`, `is_multiline`, `is_shown_once` and `comment`;
  upstream's mass assignment also takes other fillable columns of an item, including `resourceable_*`.
  Upstream stores `config_hash` only when the database
  page is open after a start; coolify-rs stores it after every start. The ClickHouse "current data is not in the data
  volume" check is not ported. `Str::slug` in container labels does not transliterate non-ASCII names.
- Notifications (`crates/coolify-jobs/src/notifications`): delivery runs on tokio tasks, so queued retries are lost when
  the process restarts. Not sent, because their job is not ported: `ForceDisabled` and `ForceEnabled` (`ServerLimitCheckJob`,
  Stripe and Coolify Cloud), and the internal notifications of Stripe jobs and of `EmailChannel` (a recipient outside the
  team, Resend errors). `SslExpirationNotification` is wired, but like upstream no caller passes a team. The Livewire test
  button of `Patches` exists only in `isDev()`. Not sent yet: the combined result of a deployment to several servers. `StatusChanged` is ported but upstream never sends it. Its mail view reads a variable
  `toMail` does not pass: tinker renders an empty link, a queue worker would throw. Request headers: reqwest adds
  `Accept: */*`, which Guzzle does not send. SMTP mail is the same in from, to, subject and body but has other MIME framing
  (Symfony and lettre differ in `Message-ID`, encoding choice and header folding). `SafeWebhookUrl` at send time uses the
  system resolver, not `custom_dns_servers`. `mb_strwidth` for the 1000 column limit of a commit message uses a table of
  wide ranges, not mbstring's. The internal notification of `EmailChannel` does not include the settings JSON. Telegram
  and Pushover could only be compared on the reference through `Http::fake` (their hosts are fixed there).
- Deliberate divergence: upstream's `POST` and `PATCH /databases/{uuid}/storages` answers carry the relation the model
  loaded on the way (`resource` or `service`): the whole database with `destination.server`, including the saved proxy
  configuration, also for tokens without `read:sensitive`. coolify-rs leaves this relation out.
- Clone with `clone_volumes`: a database copy across servers was only checked for the new volume existing, not its
  contents. `migrate` answers 404 as upstream does in production.
- S3: `testConnection` signs its own `ListObjectsV2`; when the endpoint answers with an error, the message is shaped like
  Flysystem's but not identical to the AWS SDK's text. The S3 endpoint's DNS is not pinned for the request as
  `httpClientOptions` does. The "S3 connection error" email is not sent.
- Docker cleanup after a stop runs the prune commands of `CleanupDocker`; `cleanupApplicationImages` and the buildx prune
  are not run there.
- Database imports run in the coolify-rs process: a restart drops a running import, and the stop-after-transport-failure
  path of `DatabaseImportCleanup` is not ported. `fileContainsPostgresqlProgramExecution` scans plain uploads locally;
  gzip uploads rely on the scan in the container. The upload's `mime_type` is derived from the file's signature.
- Backup overlap protection and the reservation live in the process.
- `RegenerateSslCertJob` regenerates the certificates of databases, through the code that made them, so the names come from
  the database rather than from the stored `common_name`. A row without a database resource is left alone.
- Applications in the UI: Docker image applications and git applications from a public repository (new-resource
  "Docker Image" and "Public Git Repository" forms with Smart Scan and env file import, General page for the Docker
  image, Dockerfile, Static, Nixpacks and Railpack build packs, Git Source page, Deployment Logs list and log viewer,
  Danger zone, heading actions). Not ported in the UI: the other new-resource types (private repositories through a
  GitHub App or a deploy key), the Docker Compose sections of the General page (a Compose application shows the "not
  available" page; it can be created but not deployed), HTTP Basic Authentication, custom Docker run options,
  pre/post-deployment commands, building on a build server (selecting it says so), a Dockerfile stored on the
  application (no repository), "Force start" of a queued
  deployment and the other application pages (Domains, Environment Variables, Storage, Advanced, Healthcheck,
  application (no repository), switching the Git source to a GitLab or Gitea App and the other application pages (Domains, Environment Variables, Storage, Advanced, Healthcheck,
  Runtime Logs, …). The General page's instant saves (site type, labels) store only the setting they
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
  written as upstream writes it, and `build_pack = dockercompose` without `instant_deploy` runs `LoadComposeFile` after the
  response. Every create field and `PATCH /applications/{uuid}` store what upstream stores. Inline Dockerfiles deploy, also with
  `instant_deploy` (`inline` and `inline-instant` in `compat/diff/git_deploy_diff.py`). A request that upstream fails halfway (a value PostgreSQL rejects, such as
  `gpu_driver: null` or `connect_to_docker_network: "maybe"`) answers the same 500 but leaves no row behind, where
  upstream keeps the application it had already saved. PATCH does not queue `ReleaseRemovedDnsHostnamesJob`
  (`queueReleaseOfRemovedHostnames`); the Domains pages and resource deletes do release records. The GitHub App repository check does not apply upstream's `SafeExternalUrl` private-network policy to the GitHub API URL, and the
  GitHub App path is not yet verified against a real GitHub App (only up to the lookup of the App).
- Application envs, storages, tags and the secret manager link are ported. File storages are written and deleted on the
  server by the API, and the deploy job mounts them and writes missing content files (`EnsureContentFilesOnServer`), also
  when a directory is in the way (`file-storage*` and `compose-files` in `compat/diff/git_deploy_diff.py`, with the delete).
  The job reads the rows `ORDER BY id`; upstream has no `ORDER BY`, so its mount order in the Compose file follows the
  heap of the table and can differ from ours and between its own runs (the diff sorts these lines).
  `DELETE /applications/{uuid}/envs/bulk` answers 405 instead of upstream's 404 (the literal path wins over `{env_uuid}`).
  Responses of a created or updated storage include the owning application with its server, as upstream does, also
  `proxy.last_saved_proxy_configuration` for tokens without `read:sensitive`: an upstream leak kept for parity.
- Application operations are ported in the API (`compat/diff/applications_ops_diff.py`): previews, clone, rollback,
  logs, move, destinations. What they do not do yet:
  - Preview deployments build and run for every build pack (`preview-*` in `compat/diff/git_deploy_diff.py`). `ApplicationPullRequestUpdateJob` ran against a local fake of the GitHub API only.
  - `POST /migrate` answers 404 like upstream outside a Coolify development instance (`isDev()`), so
    `MigrateResourceToDestination` is not ported.
  - `clone_volumes` across servers goes through this host, as upstream's scp does. It ran for services; the application
    path uses the same code but was not run. A source the job cannot deploy answers 501 before anything is created.
  - The preview routes read the service names and named volumes of a Compose preview from `docker_compose_raw`, where
    upstream's `composeServiceNamesForPreview()` runs `parse(pull_request_id)`: creating a preview does not yet add its bind
    mounts or store the rendered file; the deployment does. A clone of a Compose application with a named volume and a
    preview answers 500 like upstream, but writes nothing where upstream leaves a half-made clone.
  - Managed DNS records are not released when a preview's domains change through the API.
- Scheduled tasks and the scheduler are ported for applications (`compat/diff/applications_scheduled_tasks_diff.py`).
  - The scheduler starts with the process. `COOLIFY_RS_SCHEDULER=off` (also `0`, `false`, `no`) turns it off, and
    `scripts/dev-server.sh` sets it to off unless the caller says otherwise, because the reference runs its own scheduler
    on the same database. Both use `scheduled_job_states` and `scheduled_job_deliveries`, so running both still runs an
    occurrence once; a manual execute works with the scheduler off.
  - One loop runs everything: `Scheduler::tick` is `ScheduledJobManager` (`tasks`, `backups`, `volume-backups`,
    `docker-cleanups`) and `coolify_jobs::kernel` the rest of `Kernel::schedule()`, including `ServerManagerJob`
    (`compat/diff/kernel_schedule_diff.py` compares it with `schedule:list`). `is_scheduler_healthy` is the heartbeat of that
    loop, so it is false on an instance with the scheduler off.
  - `COOLIFY_RS_SCHEDULER_ONLY` (a comma separated list of schedule keys) limits a test run to named rows, housekeeping
    included. It does not stop `spawn_status_poller`. Production leaves it unset.
  - Not in the table, with the reasons in `kernel.rs`: `cleanup:ssh-mux`, `versions:refresh-local`, `cleanup:redis`,
    `horizon:snapshot`, `uploads:clear`, and the `isDev()` branch. Waiting for their areas: `dns:release-orphaned-records`,
    `ReconcileGithubRunnersJob` and `UpdateCoolifyJob`.
  - `dump_all` backups, S3 and every provider check were not run by the scheduler on the VM: `cleanup_stucked_resources`,
    `RegenerateSslCertJob`, `CleanupOrphanedPreviewContainersJob` and `ServerCloudProviderStatusCheckJob` ran only in
    tests or not at all, because they act on what other work on the VM owns or call a real provider.
  - The `ScheduledTaskDone` broadcast is not ported.
  - Jobs run on tokio tasks, not on a queue. A job that finds the overlap lock of its task held does nothing, as upstream's
    middleware says, but with one Horizon worker upstream's second job only starts after the first ended, so it runs then
    (a manual execute during a run: one execution here, two on the reference). The lock lives in the process, not in Redis.
    A process restart fails the running executions at startup ("The worker stopped..."), without a trace in `error_details`.
  - One job handles the tasks of applications and services (`scheduled_task.rs`). A service that is starting counts as
    running if its containers run, because `isStarting()` needs the service start activities.
  - `subscription_unpaid` is not ported: it applies to Coolify Cloud only. Redaction does not resolve remote secrets. A
    timezone uses chrono-tz data, not PHP's, so a future rule change could differ until both update. A cron step below zero
    (`*/-5`) is invalid here and an endless loop upstream; `validate_cron_expression()` refuses it anyway.
- Docker Compose applications deploy (build, pull, `docker compose up`, build fingerprint reuse, raw mode, custom start and
  build commands, preserved repositories, build secrets, previews). The `content` column of a file storage that came from
  the repository (`is_based_on_git`) can be filled on the reference and empty here after a preserved-repository deployment;
  the cause on the reference was not established and `git_deploy_diff.py` does not compare it. The startup cleanup scopes interrupted deployments by host name, so
  two coolify-rs instances on one host would mark each other's running deployments as failed.
- At startup upstream (`app:init`) marks every in-progress and queued deployment as failed; coolify-rs marks only the
  in-progress deployments its own host was running (`horizon_job_worker`), so an upstream instance sharing the database
  keeps its deployments. Queued deployments are left queued.
- `sentinel_custom_url` for a new server falls back to the request's scheme and host; `X-Forwarded-*` count only
  from trusted proxies (upstream trusts every proxy).
- Deployments: only the Docker image build pack. Git build packs (Dockerfile, static, Railpack, Nixpacks, Compose), previews, additional servers, build servers, registry pushes, basic auth, pre/post-deployment commands and custom Docker options are not ported (501). `current_process_id` stays empty, and commands that upstream feeds on stdin carry their input inside the script. Jobs run in the coolify-rs process: a restart drops a running deployment.
- No backups, notifications or webhooks yet. SSH runs without
- No proxy management, scheduler, notifications or webhooks yet. SSH runs without
- The new-server page offers only "IP address or domain" (no cloud provisioning).
- Deployments: `current_process_id` stays empty, and commands that upstream feeds on stdin carry their input inside the script. Jobs run in the coolify-rs process: a restart drops a running deployment.
- No proxy management, backups, scheduler, notifications or webhooks yet. SSH runs without
  connection multiplexing (one connection per command).
- New installations (`scripts/install`, `coolify-rs boot`) use the baseline schema; a 4.5.0 database is served read-only
  until upstream is upgraded. No image or publishing address exists yet, so `COOLIFY_RS_CDN` has no default and the
  image name `coolify-rs/coolify-rs` is a placeholder. A clean install differs from upstream's in what
  `compat/diff/install_diff.py` lists as explained: no key files or mux sockets on disk (ADR 0009), ports 6001 and 6002
  are not published, no `applications`, `databases`, `services` or `backups` mounts.
- `app:init` at startup: `cleanup:redis`, `cleanup:names`, `cleanup:stucked-resources`, `replaceSlashInEnvironmentName`,
  `restoreCoolifyDbBackup`, `updateUserEmails` and `cleanupUnusedNetworkFromCoolifyProxy` are not run. The alive signal to
  upstream's telemetry address is not ported on purpose.
- `RootUserSeeder`: the password is not checked against the breach list, because the server sends nothing to a third party
  while it starts.
- `upgrade-postgres.sh` is upstream's script unchanged and was not run.
- Browser sessions are kept in memory, not in the `sessions` table, so a restart or an upgrade signs users out. Upstream
  keeps them signed in. The upgrade's progress view still finishes, and its reload shows the login page.

## Keys, notifications, tags, shared variables and S3 pages

- Cloud token "Validate" and "Validate and add" call the real provider APIs, as upstream. They were only exercised up to the
  form validation, since nothing may go to an external service during tests.
- Send-test on Pushover and Telegram calls the real services, as upstream, so the comparison flows leave them out. The Telegram
  path shares `notify_test` with `compat/diff/notifications_e2e.py`.
- A failing SMTP send in the test email: only "connection refused" uses Symfony's message. Other transport errors (DNS, TLS,
  authentication) show lettre's text.
- S3 connection failures do not send `emails.s3-connection-error`, and `unusable_email_sent` is only reset, not set.
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

- Avatars are stored on the local disk (`{COOLIFY_RS_DATA_DIR}/images`). Upstream's `avatar_storage_type = s3` is not
  used for writing, and a row with an S3 avatar answers 404 in coolify-rs. A file stored by upstream's container is not
  visible to coolify-rs unless the disks are shared.
- Deleting a user through `/team/admin` is refused with a message when a team only that user belongs to still owns
  servers or projects. Upstream force-deletes them; their deletion hooks are not all ported.
- Deleting a team or account removes keys, tags, shared variables, S3 storages and sources with their rows, but writes
  no audit events for those rows and does not remove a key's file (upstream's model events do).
- A password confirmation is remembered per session and skips the password step only on the pages listed above. Older
  pages (server, application and database deletion) still ask for the password within upstream's three hours.
- What's New reads `{COOLIFY_RS_DATA_DIR}/changelogs/*.json`, which `PullChangelog` writes. Unread counts are not cached.
- A new user is sent to `/onboarding` as upstream does, but the wizard is not ported: the page offers "Skip setup".
- Sign-in state before login (the two-factor challenge's `login.id`, OAuth state, passkey challenge, flash messages)
  lives in memory under its own cookie, like the sessions: a restart ends a challenge in progress. Rate limits are also
  per process.
- "Remember me" is not offered by upstream's forms and is not implemented, so `remember_token` is only rotated.
- OIDC reads the discovery document and the JWKS on every sign-in; upstream caches them for an hour and six hours.
- OIDC requires HTTPS issuers, as upstream does, so the local fake provider could not be used for it: the id_token
  checks have unit tests and the provider has not done a full round trip. GitHub, Google, GitLab, Azure, Discord,
  Bitbucket, Infomaniak, Clerk and Zitadel were compared up to the redirect to the provider, without contacting them;
  only Authentik did the full round trip (against a local fake). Their user mappings come from the provider classes and
  have unit tests only.
- A resend of a reset link within a minute answers upstream's plain text page as an error in the form, and the 400 of
  `forgot_password` when mail is off cannot be reached from the page in either. A failed `/auth/oidc/redirect`
  (discovery failed) answers a bare 500 rather than the styled error page.
- `auth.oauth.callback_failed` names exception classes from a short list; other failures name the closest one.
- Email verification is only enforced on Coolify Cloud upstream, so it is not enforced here; `/verify` and the signed
  link work for any signed-in user.
- The subscription checks of `DecideWhatToDoWithUser` only apply to Coolify Cloud and are not ported.

## API divergences (implemented routes)

| Area | Upstream | coolify-rs | Reason |
|---|---|---|---|
| Client IP for the API allowlist | `TrustProxies('*')`: `X-Forwarded-For` trusted from anyone, so the leftmost (client-controlled) entry is used | peer address unless `COOLIFY_RS_TRUSTED_PROXIES` is set; then rightmost untrusted hop | security fix: the allowlist is spoofable upstream when the app is reachable without a proxy |
| `CF-Connecting-IP` on Coolify Cloud | honoured for any peer when `isCloud()` | not applicable | coolify-rs has no cloud mode; the self-hosted rule (Cloudflare peers only) is implemented |
| Rate limiting | `throttle:api` 200/min, `X-RateLimit-*` headers | none | not implemented yet |
| Audit events | `audit_events` rows for environment changes, auth failures, server validation | written, except server validations and remote commands, which go to the `audit` tracing target | not implemented yet |
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
| `Project/Shared/*` sections on service pages (variables, tags, danger, logs, volumes and others) | placeholder named after the section | connected when the shared components merge |
| `addSuggestedDomain` on the Domains page | not ported | suggestions are not offered |
| Configuration warning on service pages, with the diff of changes | the warning without the diff | no configuration diff yet |
| `PATCH /services/{uuid}/databases/{database_uuid}` that changes `is_public` | `501` | `StartDatabaseProxy` / `StopDatabaseProxy` are not ported |
| `POST /services/{uuid}/migrate` in a dev instance | `404`, as upstream outside dev | `MigrateResourceToDestination` only runs in dev upstream |
| `get_service_templates(force: true)` | the shipped file, or `COOLIFY_RS_TEMPLATES_DIR`, which `PullTemplatesFromCDN` replaces | forcing a refresh from a page is not ported |
| `CleanupDocker` after a stop, maintenance pages, managed DNS cleanup on a changed domain, `parseDockerComposeFile` for parsing versions below 3 | not run | their subsystems are not ported |
| Order of `environment_variables`, `applications`, `databases` | ordered by id | upstream has no `ORDER BY`: Postgres returns heap order, which changes after updates |

## Secret managers, Cloudflare, DNS and GitHub runners

Verified against local fakes only (`compat/diff/fake_providers.py`, `fake_git_provider.py`), never a real provider. Provider
base URLs change only through the test-only setting `COOLIFY_RS_TEST_ONLY_FAKE_PROVIDERS`, which accepts a loopback `http`
origin and nothing else. `scripts/dev-server.sh` points it at a closed local port by default.

- `reclaimRecord` and `adoptRecord` are ported, but the server transfer import does not call them yet.
- `adoptRecord` and `reclaimRecord` have no differential: they run only during a transfer.
- A conflict from "Add with" in the DNS entries modal opens the Configure DNS modal, so the record can be replaced. Upstream
  stores the conflict and shows it only if that modal is open.
- The hostname lock is per process: upstream's lock is in Redis, shared by its workers.
- The ownership comment reads `APP_ID` from the environment, as upstream's `config('app.id')`. Without it the comment says
  `default`, so a record created under another id counts as changed outside Coolify.
- Log redaction of database and service starts does not add fetched secret values (`DatabaseStartCommandExecutor`,
  `redact`). Deployment logs do. Values in a failing start's output can show.
- Rows written by earlier coolify-rs builds hold source secrets in serialized form. coolify-rs reads them, but upstream's
  models read `s:3:"abc";` until the secret is saved again.

What the fakes cannot prove: GitHub's manifest conversion, JWT clock checks, real installation permissions, real runner
registration (the JIT config was fake and the runner image a stub that sleeps), Cloudflare's real answers and propagation,
Doppler, Infisical, Vault and Have I Been Pwned themselves, and GitLab's real OAuth. Gitea was a real container. A real GitHub
App test stays with the user.

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

## Instance settings, layout, onboarding and terminal

| Area | Upstream | coolify-rs | Why |
|---|---|---|---|
| `Settings\Index::buildHelperImage`, the Development helper section | `isDev()` only | not ported | no dev mode |
| Check now, Detect, Help without mail settings | `cdn.coollabs.io`, `ifconfig.io`, `app.coolify.io` | the same hosts; `COOLIFY_RS_VERSIONS_URL`, `COOLIFY_RS_IFCONFIG_URL` and `COOLIFY_RS_FEEDBACK_URL` replace them in tests. `CheckTraefikVersionJob` is not dispatched by Check now. Only verified against local fakes | no test may reach them |
| File download storage on S3 (`FileDownloadStorageService`) | checks the bucket and adds a lifecycle rule | a storage other than the stored one is refused with upstream's "not available" message | not ported |
| `settingsSearchItems` in the palette's "This page" scope | sidebar pages and sections | sections only | |
| Instance backup page beyond "Localhost is not ready" | | built from the Blade views and not compared: localhost is not reachable in the test setup, so the reference never shows these states. Selecting storages uses checkboxes instead of the multiple listbox. Executions are listed without Delete, Download, Clean failed and Clean deleted | |
| A failing `setupDynamicProxyConfiguration` with a missing private key | `ModelNotFoundException` becomes a 404 dialog over the page | a toast with the same message | only seen with this test setup's localhost |
| Global search cache | 300 seconds, cleared by `ClearsGlobalSearchCache` | no cache: read on each opening | same visible results |
| "new storage" and "new private key" in the palette | open the create modal in place | go to the page and open its modal | the modals live on those pages |
| "new github" in the palette | modal | goes to `/sources` | |
| Real-time warning popup | shown when Echo cannot connect | never: coolify-rs has no Echo connection to fail | |
| Onboarding cloud provider cards | modals of `Server\New\By*` | links to `/servers/new` | those components are not ported |
| Onboarding validation | `validate-and-install`, which installs prerequisites and Docker when missing | checks through the existing validation dialog, then the proxy start. A server without Docker stops with upstream's message | the dialog has no install progress |
| `api.auth.unauthenticated` deduplication | per source, reason, server and email through the cache | per source, in memory | only API requests reach it, with one reason |
| Audit events of the wizard | `Auditable` on every model | written for the key, server, shared variables, project, environment and team; only one `ui.server.updated` (`proxy`) of upstream's three, and the `changes` hold the name only | |
| Terminal websocket | a separate Node server on port 6002, `TERMINAL_PROTOCOL`, `TERMINAL_HOST`, `TERMINAL_PORT` | `/terminal/ws` on the port of the UI; those settings do not apply. A page of another origin is refused (close 4401) where upstream relies on `SameSite`. Multiplexed ssh does not apply: upstream drops it for terminals too | one server; the extra check does not change what a browser on this instance sees |
| Terminal for a non-root ssh user | `sudo docker exec`, `sudo docker attach` | the same commands, checked as strings against `containerCommand()` on the reference and in tests, not run on a server | the test VM only has root |
| Shared resource sections | `Project\Shared\*`, `Project\Service\{Storage, FileStorage}` and `Project\Application\{Advanced, Rollback}` for every resource | environment variables, persistent storage, runtime logs, resource limits, healthcheck, tags, webhooks and Advanced are ported and mounted on application pages as `frontend/src/components/shared/*` with `/ui/resources/{kind}/{uuid}/...` | Not ported: Resource operations (clone, move, migrate), Servers (`Destination`), Swarm, Maintenance, Files (`FileBrowser`), resource Metrics charts (only the states before charts), starting a rollback, Refresh GPUs, linking a secret manager source (it contacts a third party; only the empty state), volume backup modals, external Compose volumes, stale Compose volume actions |

## Server transfer

- A transfer between two real instances with a second Docker host has not been run. Export, job and source side ran against a recorded
  fake target; import and claim ran on the reference and on coolify-rs against the SSH testing host.
- The import does not take over DNS records at Cloudflare (`adoptRecord`, `reclaimRecord`): an owned record of a bundle is stored as not
  owned, with the warning upstream gives when the provider refuses. The finish list steps `dns_record`, `github_app_webhook` (GitHub API),
  `s3_storage` (`testConnection`) and webhook receipts (`recordWebhook`) make no provider call and stay on "Needs action" until marked done.
- Project icons do not travel: a project with an icon gets upstream's "cannot be read" warning on export, and an icon in a bundle is not
  stored on import.
- `clearLocalSshArtifacts` and `remove_iip` have no equivalent: this build keeps no SSH mux or key cache to clear, and messages are not
  stripped of instance addresses.
- A database error during import answers with a shorter message than Laravel's, without SQL and bindings.
- `TransferFinishChanged` is not broadcast: the list refreshes through its 15 second poll.
- `MigrateServerToInstanceJob::failed` (`recordStoppedMigration`) has no equivalent, because this job has no worker timeout. A restart during a
  transfer leaves the state "running" until `STALE_MIGRATION_SECONDS` passes.

## Previews, scheduled tasks and the General page

- Preview domains make no DNS check, as on the application Domains page: a new domain stays "Not checked" and the toast has no
  "DNS check started", and "Check DNS" and "Check all DNS" only say that checks are not available.
- "Load pull requests" (`githubApi`) has never run against GitHub or a fake: the comparisons do not press it, because it contacts GitHub.
- A Preview ID that is not a number answers with a message under the field; upstream's `?int` property fails with an error page.
- A Compose preview that generates domains without a service does not use `generateWithoutApplicationDomain`.
- "Execute now" and running task executions were not compared: the fixtures were not running applications.
- Saving Compose domains on General does not run `validateDNSEntry` or `checkDomainUsage`; the domains are edited on the Domains page.

## Resource operations and application sections

- Migrate to another server (`MigrateResourceToDestination`) is not ported. Upstream renders the section only in a development
  instance (`isDev()`) and its API answers 404 elsewhere, so the section is not shown here either.
- Clone and clone-me copy no volume data: neither Blade view has a control for it.
- Clones of an application made through the UI copy its previews without domains until the preview helpers of the application
  API are on this branch. Clone-me is not one transaction here: a failure partway leaves what was already cloned.
- The toast after adding a domain is "Domain added.": the DNS check and record creation that upstream starts after adding
  (`configureDnsAfterDomainAdd`, `ConfigureDnsRecordJob`, the DNS gate of the add and edit dialogs) are not ported. Run them
  from the DNS menu. The www redirect is set from the domain dialog; suggested www rows are not offered.
- Adding, removing, promoting and stopping an additional server work, and so does deploying to one (`only_this_server`).
- File browser: downloads through an S3 file storage (`FileDownloadStorageService`) are not ported, so files always stream from
  the server. The helper container path for images without a shell is ported but was not run on the VM.
- Maintenance mode: the Caddy configuration is ported but ran only against Traefik on the VM.

## Additional servers, build servers, Swarm and S3 (deploy stream)

Compared with `compat/diff/multi_server_diff.py`, `compat/diff/environment_details_diff.py` and
`compat/diff/s3_backup_diff.py`. The S3 script needs `scripts/dev-server-container.sh`: the trusted host `coolify-minio`
only resolves on the reference's Docker network.

- `git_deploy_diff.py`, `force_start_diff.py` and `git_source_deploy_diff.py` clone from local git servers on the test VM
  (`local_git_http.py`, `fake_git_provider.py`), not from GitHub, so a public repository is a row in `github_apps` with the
  mirror as `html_url`. Bitbucket exists upstream only as the `git_type` of a preview (`deploykey-pr-bitbucket`,
  `other-pr-bitbucket`); there is no Bitbucket source.
- `ServerFilesFromServerJob` runs beside the deployment that parsed, as upstream's queue does, so when `content` of a file
  storage based on the repository fills depends on timing on both sides.
- Databases on a Swarm destination can be created and cloned through the API and the UI routes. A start of one creates no
  container, on the reference and here alike. The destination picker of the create forms was not compared with a Swarm server.
- Deleting a server with `force=true` removes the rows of its resources and leaves their containers on the host, as upstream
  does: the jobs start after the soft delete, when `destination.server` is empty. Compared on the API; the UI uses the same job.
  Deregistering GitHub runners on that path was not run.
- A row whose destination is gone answers 500 on start, stop and restart like upstream. The response of the application UI
  actions and of the Destination section in that state was read from the PHP and not compared.
- Service clone with volumes: on the reference, the same-server chain can die in its `StopService` step with `Nesting level
  too deep` (`Closure (services.php:783)`), leaving nothing copied and the source stopped. coolify-rs completes it, so
  `multi_server_diff.py` reports those two reference checks as failed. That is an upstream bug, deliberately not copied.
- `PreparesBackupDownloads` and S3 file downloads have still never run: they need `file_download_s3_storage_id` in
  `instance_settings`.
- The combined multi-server notification remembers "queued" and "notified" in memory, not in a cache, so a restart between
  the deployments of one group sends none. Its delivery was not compared.
- Custom-type service databases still answer 404 on their pages. `ip_previous` is empty after a create on both sides
  (`server-delete` in `multi_server_diff.py`).
- `MigrateResourceToDestination` is development-only upstream (`isDev()`), so it answers 404 on both sides and was not run.

## Canvas, Admin and Subscription

- Canvases are upstream's Svelte code, copied unmodified to `frontend/src/upstream/js`. Live status needs `window.Echo`, which coolify-rs does
  not have, so a canvas shows a status change after its Refresh button or, on the dashboard canvas, its five second poll. `logo.src` has no
  `?v=` of the file's modification time.
- The remembered view is a cookie the browser reads, not one the server reads before rendering, so a list page shows for an instant before it
  opens the canvas. A `/project/{uuid}/icon` that does not exist answers a JSON 404 where upstream answers its HTML error page.
- Project icons: uploads go to the local disk. `avatar_storage_type = s3` is not used for writing, and a row with an S3 icon answers 404.
  An icon stored by upstream's container is not on this disk either. Deleting a project does not remove its icon file.
- `Admin\Index`, `/subscription` and `/subscription/new` behave as upstream for a self-hosted instance (403 and redirects, compared with the
  reference). The Coolify Cloud path (`SELF_HOSTED=false`) was only run against a local fake of Stripe (`compat/diff/stripe_fake.py`) and read
  against the PHP, because the reference runs self-hosted and has no root password in its fixtures. The Admin page's root path is tested in
  `crates/coolify-api/tests/ui_admin.rs`, which needs `APP_ENV=local`.
- Not ported on Cloud: `ServerLimitCheckJob`, `VerifyStripeSubscriptionStatusJob`, `SubscriptionInvoiceFailedJob`, `SyncStripeSubscriptions`,
  `send_internal_notification` (a log line here), and account deletion cancelling the user's other Stripe subscriptions.
  `isSubscriptionOnGracePeriod` and the subscription gates of `DecideWhatToDoWithUser` are not applied. An early fraud warning looks for
  subscriptions by customer, not by email across customers.
- `/__error/{code}` and `/__exception` work with `APP_ENV=local` or `testing` and go home otherwise, as in production upstream. `/__exception` shows
  the 500 page, not Laravel's debug renderer. `/__livewire-request-failure` previews Livewire's failure dialog, which has no counterpart here.
  A `/files/download/{token}` with an unknown token answers JSON where upstream answers its HTML 404 page.
- `/realtime` answers "Look at your other tab." for the root team without sending `TestEvent`.
