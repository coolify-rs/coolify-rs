# Known Limitations and Divergences

Everything here is deliberate or known; none of it is hidden behind a "compatible" claim.

## Scope

- Upstream API operations that are not ported answer `501 {"message":"This endpoint is not implemented in coolify-rs
  yet."}`; `scripts/parity.py --missing` lists them.
- Not every upstream API operation is implemented (`scripts/parity.py`). Others answer `501 {"message":"This endpoint is not
  implemented in coolify-rs yet."}`.
- The web UI covers the dashboard, projects, environments, private keys, API, cloud and integration tokens, cloud-init scripts,
  notifications, tags, shared variables, S3 storage, teams, members, invitations, profile, 2FA enrollment, passkey
  management and appearance, every sign-in screen (registration and the root setup window, password reset, forced
  password change, email verification, the two-factor challenge and requirement, passkey sign-in, OAuth/SSO and
  `/settings/oauth`), instance settings, onboarding, servers, destinations, registries and sources.
  Not available yet: the sections of applications, databases and services that have no page (they open a "not
  available in coolify-rs yet" page).
- Within the ported pages: no canvas views, global search, project icons or custom color themes;
  applications and services in an environment are counted, not listed; the dashboard's server status does not include
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
  only, the CA certificate's subject key identifier is SHA-256 based, `PushServerUpdateJob` and `GetContainersStatus` from a Sentinel push are not run (the
  heartbeat is), `RegenerateSslCertJob` after a CA change is not run, marks upstream keeps in Redis are per process. The
  scheduler (`ServerManagerJob`) runs only with `COOLIFY_RS_SERVER_SCHEDULER=1`, because an instance sharing its database
  with upstream must not check servers twice, and evaluates cron expressions in UTC. Compose merge tags (`!reset`,
  `!override`) in a proxy configuration are ignored when reading it, but traffic analytics does not rewrite a stored
  configuration.
- Server and destination pages: ported are destinations (index, new, show, resources, danger), Advanced, Private Key, CA
  Certificate, Swarm, server Destinations, Resources (no canvas toggle), Proxy (configuration, dynamic configurations, TLS
  list, logs, start/stop/restart), Sentinel (configuration, logs), Docker Cleanup with executions, Images, Log Drains,
  Registries (server and `/registries`), Terminal Access, Security Patches, Cloudflare Tunnel, GitHub Runners and the settings of Traffic Analytics. Not ported yet (the page
  says "not available"): Transfer. Cloudflare Tunnel: the setup and tear down through the Cloudflare API
  (`CloudflareTunnelService`) and the integration token form answer with an error toast or link to the unported token page; manual mode and the
  tunnel token path are ported. GitHub Runners: the page, settings, Sysbox install and cancel are ported; a GitHub App that is ready for runners
  answers with an error toast, because `GithubRunnerApi`, provisioning, reconciling and the `workflow_job` webhook are not ported. Security
  Patches: the `ServerPatchCheck` notifications are not sent. Saving a CA certificate does not run `RegenerateSslCertJob`. Log drains: the secret fields stay empty for a
  member, where upstream's Livewire snapshot carries them. Proxy status toasts come from real-time events upstream
  (`ProxyStatusChangedUI`); the UI here polls, and neither side shows them when the real-time service is down.
  Traffic analytics: the charts of the server page and of `/analytics` are ported (Traefik verified, the Caddy proxy not run);
  the dashboard widget (`Dashboard\TrafficAnalytics`) and the application and service analytics pages (`ResourceTrafficAnalytics`), where the "Top applications" rows link, are not. After a filter change, rows of a list with equal values can
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
  `VolumeBackupJob` runs for a manual run or "Back up now". The scheduler does not start it yet (needs
  `Jobs::spawn_volume_backup(backup_id)` per due `scheduled_volume_backups` row, plus `BackupMissing` and the recovery
  sweep). Recovery runs once in the job, or on "Retry recovery" (`Jobs::retry_volume_recovery`), not on a timer. A run
  that finds another run of the same schedule in progress is dropped using the executions table, not a cache lock.
  The `VolumeBackup/*` notifications are sent, but their delivery was not compared: the test team has no channel enabled.
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
  Secret manager `{{vault.X}}` references stop a start with a message. Upstream stores `config_hash` only when the database
  page is open after a start; coolify-rs stores it after every start. The ClickHouse "current data is not in the data
  volume" check is not ported. `Str::slug` in container labels does not transliterate non-ASCII names.
- Notifications (`crates/coolify-jobs/src/notifications`): delivery runs on tokio tasks, so queued retries are lost when
  the process restarts. Not sent yet, because their job is not ported: `RestartLimitReached` for applications, previews
  and databases, `ContainerRestarted` and the disk usage check of `PushServerUpdateJob`, `BackupMissing`,
  tasks of applications and the missed runs of `ScheduledJobDeliveryService`,
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
  upstream keeps the application it had already saved. PATCH does not queue `ReleaseRemovedDnsHostnamesJob`: managed DNS
  is not ported. The GitHub App repository check does not apply upstream's `SafeExternalUrl` private-network policy to the GitHub API URL, and the
  GitHub App path is not yet verified against a real GitHub App (only up to the lookup of the App).
- Application envs, storages, tags and the secret manager link are ported; the secret manager stores only the source
  (nothing fetches secrets from Doppler, Infisical or Vault at deployment). File storages are written and deleted on the
  server by the API, and the deploy job mounts them and writes missing content files (`EnsureContentFilesOnServer`), also
  when a directory is in the way (`file-storage*` and `compose-files` in `compat/diff/git_deploy_diff.py`, with the delete).
  The job reads the rows `ORDER BY id`; upstream has no `ORDER BY`, so its mount order in the Compose file follows the
  heap of the table and can differ from ours and between its own runs (the diff sorts these lines).
  `DELETE /applications/{uuid}/envs/bulk` answers 405 instead of upstream's 404 (the literal path wins over `{env_uuid}`).
  Responses of a created or updated storage include the owning application with its server, as upstream does, also
  `proxy.last_saved_proxy_configuration` for tokens without `read:sensitive`: an upstream leak kept for parity.
- Application operations are ported in the API (`compat/diff/applications_ops_diff.py`): previews, clone, rollback,
  logs, move, destinations. What they do not do yet:
  - Preview deployments build and run for every build pack (`preview-*` in `compat/diff/git_deploy_diff.py`). Not ported
    around them: `ApplicationPullRequestUpdateJob` (no GitHub App to comment on), the preview restart limit
    (the status poller counts restarts but does not stop the preview), deployment notifications. `POST /deploy` by tag answers 501. An application with
    an additional destination answers 501 to start, restart and rollback (`unported_parts`).
  - `POST /migrate` answers 404 like upstream outside a Coolify development instance (`isDev()`), so
    `MigrateResourceToDestination` is not ported.
  - `clone_volumes` copies data on one server only. A copy to another server (upstream goes through scp on the Coolify
    host) is logged and the new volume stays empty. A source the job cannot deploy answers 501 before anything is created.
  - The preview routes read the service names and named volumes of a Compose preview from `docker_compose_raw`, where
    upstream's `composeServiceNamesForPreview()` runs `parse(pull_request_id)`: creating a preview does not yet add its bind
    mounts or store the rendered file; the deployment does. A clone of a Compose application with a named volume and a
    preview answers 500 like upstream, but writes nothing where upstream leaves a half-made clone.
  - Managed DNS records (`ManagedDnsRecordCleanup`) are not released when a preview's domains change or it is deleted, and
    log redaction does not resolve remote secrets.
- Scheduled tasks and the scheduler are ported for applications (`compat/diff/applications_scheduled_tasks_diff.py`).
  - The scheduler starts with the process. `COOLIFY_RS_SCHEDULER=off` (also `0`, `false`, `no`) turns it off, and
    `scripts/dev-server.sh` sets it to off unless the caller says otherwise, because the reference runs its own scheduler
    on the same database. Both use `scheduled_job_states` and `scheduled_job_deliveries`, so running both still runs an
    occurrence once; a manual execute works with the scheduler off.
  - Only the `tasks` source is registered. Database backups, volume backups and Docker cleanups register their sources
    with `Scheduler::with_default_sources` when they are ported. Notifications are not ported: `TaskSuccess` and
    `TaskFailed` end in `notify_task_success()` and `notify_task_failed()`, the missed-run throttle too. The
    `ScheduledTaskDone` broadcast and the manager's cache heartbeat for the UI are not ported.
  - Jobs run on tokio tasks, not on a queue. A job that finds the overlap lock of its task held does nothing, as upstream's
    middleware says, but with one Horizon worker upstream's second job only starts after the first ended, so it runs then
    (a manual execute during a run: one execution here, two on the reference). The lock lives in the process, not in Redis.
    A process restart fails the running executions at startup ("The worker stopped..."), without a trace in `error_details`.
  - Service tasks: the job and the scheduler handle them, untested, since services are not ported. The routes are not
    registered (`routes/scheduled_tasks.rs` has the generic handlers), and a service that is starting counts as running
    if its containers run, because `isStarting()` needs the service start activities.
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
- Deployments: additional servers, build servers and registry pushes are not ported or not compared with the reference (`git_deploy_diff.py` runs one server without a registry). `current_process_id` stays empty, and commands that upstream feeds on stdin carry their input inside the script. Jobs run in the coolify-rs process: a restart drops a running deployment.
- No proxy management, backups, scheduler, notifications or webhooks yet. SSH runs without
  connection multiplexing (one connection per command).
- coolify-rs cannot bootstrap a new installation: it requires a database created by upstream Coolify at the baseline
  (`main` @ b5823c7, 473 migrations); it runs no upstream DDL. A 4.5.0 database is served read-only until upstream
  is upgraded.
  New installations are planned as Milestone 7 of IMPLEMENTATION_ROADMAP.md.

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
- A password confirmation is remembered per session and skips the password step only on the pages listed above. Older
  pages (server, application and database deletion) still ask for the password within upstream's three hours.
- What's New reads `{COOLIFY_RS_DATA_DIR}/changelogs/*.json`. `PullChangelog` is not ported, because it fetches them
  from upstream's CDN, so a new install shows no updates. Unread counts are not cached.
- A new user is sent to `/onboarding` as upstream does, but the wizard is not ported: the page offers "Skip setup".
- `Password::defaults()` in coolify-rs has no `uncompromised()` rule: it would send a hash prefix of the password to
  Have I Been Pwned. Upstream in production refuses a password found there; coolify-rs accepts it.
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
| Pages `Project/Service/{Analytics,ImportBackup,DatabaseBackups,BackupExecutions,VolumeBackup/*}` | the page says it is not available | later task |
| Page `Project/Service/Analytics` | the page says it is not available | later task |
| `Project/Shared/*` sections on service pages (variables, tags, danger, logs, volumes and others) | placeholder named after the section | connected when the shared components merge |
| DNS checks and Cloudflare automation on the service Domains page (`CheckDomainDnsJob`, `checkDomainDns`, `cloudflare-autoconfigure`) | rows stay "Not checked", `domain_dns_statuses` is not written, the buttons say it is not available | they contact external services |
| `addSuggestedDomain` and `ManagedDnsRecordCleanup` on the Domains page | not ported | they need the DNS checks |
| Configuration warning on service pages, with the diff of changes | the warning without the diff | no configuration diff yet |
| Livewire pages `Project/Service/*`, `one-click-service-*` in `Project/Resource/Create` | not available | later task |
| `POST /services/{uuid}/clone` with `clone_volumes: true` | `501` | `VolumeCloneJob` is not ported |
| `PATCH /services/{uuid}/databases/{database_uuid}` that changes `is_public` | `501` | `StartDatabaseProxy` / `StopDatabaseProxy` are not ported |
| `POST /services/{uuid}/migrate` in a dev instance | `404`, as upstream outside dev | `MigrateResourceToDestination` only runs in dev upstream |
| Scheduled tasks on their cron schedule | only `POST .../execute` runs a task | the scheduler is not ported |
| `PullTemplatesFromCDN`, `get_service_templates(force: true)` | the shipped file, or `COOLIFY_RS_TEMPLATES_DIR` | no scheduler |
| `CleanupDocker` after a stop, maintenance pages, secret managers (`{{vault.*}}` stops a start), managed DNS cleanup on a changed domain, `parseDockerComposeFile` for parsing versions below 3 | not run | their subsystems are not ported |
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

## Instance settings, layout, onboarding and terminal

| Area | Upstream | coolify-rs | Why |
|---|---|---|---|
| `Upgrade::upgrade` | runs `UpdateCoolify` | the badge, the full button and the confirmation show; "Upgrade now" answers that upgrading from the UI is not available. No progress view | upstream's action replaces the container with its own image, which is not what a coolify-rs instance runs |
| `is_auto_update_enabled` | the scheduler installs updates | stored only | same; no scheduler |
| `Settings\Index::buildHelperImage`, the Development helper section | `isDev()` only | not ported | no dev mode |
| Check now, Detect, Help without mail settings | `cdn.coollabs.io`, `ifconfig.io`, `app.coolify.io` | the same hosts; `COOLIFY_RS_VERSIONS_URL`, `COOLIFY_RS_IFCONFIG_URL` and `COOLIFY_RS_FEEDBACK_URL` replace them in tests. `CheckTraefikVersionJob` is not dispatched by Check now. Only verified against local fakes | no test may reach them |
| File download storage on S3 (`FileDownloadStorageService`) | checks the bucket and adds a lifecycle rule | a storage other than the stored one is refused with upstream's "not available" message | not ported |
| `settingsSearchItems` in the palette's "This page" scope | sidebar pages and sections | sections only | |
| Instance backup page beyond "Localhost is not ready" | | built from the Blade views and not compared: localhost is not reachable in the test setup, so the reference never shows these states. Selecting storages uses checkboxes instead of the multiple listbox. Executions are listed without Delete, Download, Clean failed and Clean deleted | |
| A failing `setupDynamicProxyConfiguration` with a missing private key | `ModelNotFoundException` becomes a 404 dialog over the page | a toast with the same message | only seen with this test setup's localhost |
| Global search cache | 300 seconds, cleared by `ClearsGlobalSearchCache` | no cache: read on each opening | same visible results |
| "new storage" and "new private key" in the palette | open the create modal in place | go to the page and open its modal | the modals live on those pages |
| "new github" and "new canvas" in the palette | modal, deploy canvas | go to `/sources` and `/deploy-canvas`, which are not ported | |
| Real-time warning popup | shown when Echo cannot connect | never: coolify-rs has no Echo connection to fail | |
| Onboarding cloud provider cards | modals of `Server\New\By*` | links to `/servers/new` | those components are not ported |
| Onboarding validation | `validate-and-install`, which installs prerequisites and Docker when missing | checks through the existing validation dialog, then the proxy start. A server without Docker stops with upstream's message | the dialog has no install progress |
| `api.auth.unauthenticated` deduplication | per source, reason, server and email through the cache | per source, in memory | only API requests reach it, with one reason |
| Audit events of the wizard | `Auditable` on every model | written for the key, server, shared variables, project, environment and team; only one `ui.server.updated` (`proxy`) of upstream's three, and the `changes` hold the name only | |
| Terminal websocket | a separate Node server on port 6002, `TERMINAL_PROTOCOL`, `TERMINAL_HOST`, `TERMINAL_PORT` | `/terminal/ws` on the port of the UI; those settings do not apply. A page of another origin is refused (close 4401) where upstream relies on `SameSite`. Multiplexed ssh does not apply: upstream drops it for terminals too | one server; the extra check does not change what a browser on this instance sees |
| Terminal for a non-root ssh user | `sudo docker exec`, `sudo docker attach` | the same commands, checked as strings against `containerCommand()` on the reference and in tests, not run on a server | the test VM only has root |
| Shared resource sections | `Project\Shared\*`, `Project\Service\{Storage, FileStorage}` and `Project\Application\{Advanced, Rollback}` for every resource | environment variables, persistent storage, runtime logs, resource limits, healthcheck, tags, webhooks and Advanced are ported and mounted on application pages as `frontend/src/components/shared/*` with `/ui/resources/{kind}/{uuid}/...` | Not ported: Resource operations (clone, move, migrate), Servers (`Destination`), Previews UI, Swarm, Maintenance, Files (`FileBrowser`), resource Metrics charts (only the states before charts), starting a rollback, Refresh GPUs, linking a secret manager source (it contacts a third party; only the empty state), volume backup modals, external Compose volumes, stale Compose volume actions |

## Server transfer

- A transfer between two real instances with a second Docker host has not been run. Export, job and source side ran against a recorded
  fake target; import and claim ran on the reference and on coolify-rs against the SSH testing host.
- The import does not take over DNS records at Cloudflare (`adoptRecord`, `reclaimRecord`): an owned record of a bundle is stored as not
  owned, with the warning upstream gives when the provider refuses. The finish list steps `dns_record`, `github_app_webhook` (GitHub API),
  `s3_storage` (`testConnection`) and webhook receipts (`recordWebhook`) make no provider call and stay on "Needs action" until marked done.
- Project icons do not travel: `ProjectIconStorageService` is not ported, so a project with an icon gets upstream's "cannot be read" warning
  on export, and an icon in a bundle is not stored on import.
- `clearLocalSshArtifacts` and `remove_iip` have no equivalent: this build keeps no SSH mux or key cache to clear, and messages are not
  stripped of instance addresses.
- A database error during import answers with a shorter message than Laravel's, without SQL and bindings.
- `TransferFinishChanged` is not broadcast: the list refreshes through its 15 second poll.
- `MigrateServerToInstanceJob::failed` (`recordStoppedMigration`) has no equivalent, because this job has no worker timeout. A restart during a
  transfer leaves the state "running" until `STALE_MIGRATION_SECONDS` passes.

## Resource operations and application sections

- Migrate to another server (`MigrateResourceToDestination`) is not ported. Upstream renders the section only in a development
  instance (`isDev()`) and its API answers 404 elsewhere, so the section is not shown here either.
- Clone and clone-me copy no volume data: neither Blade view has a control for it. `POST /services/{uuid}/clone` with
  `clone_volumes` still answers 501, and a database clone across servers skips the data.
- Clones of an application made through the UI copy its previews without domains until the preview helpers of the application
  API are on this branch. Clone-me is not one transaction here: a failure partway leaves what was already cloned.
- DNS checks (`CheckDomainDns`, `CheckDomainDnsJob`) and the Cloudflare automation on the application Domains page are not
  ported, as they contact external services. Rows stay "Not checked" and the toast after adding is "Domain added.". The www
  redirect is set from the domain dialog; suggested www rows are not offered.
- Deploying to one additional server (`only_this_server`) answers 501: the deploy job has no multi-server path. Adding, removing,
  promoting and stopping work. A Swarm application shows "Server unavailable" in its heading, because `ApplicationRow` only loads
  standalone destinations; deploying to Swarm is not ported.
- File browser: downloads through an S3 file storage (`FileDownloadStorageService`) are not ported, so files always stream from
  the server. The helper container path for images without a shell is ported but was not run on the VM.
- Maintenance mode: the Caddy configuration is ported but ran only against Traefik on the VM.
- Flush all data on the Danger page of Redis, KeyDB and Dragonfly is not ported.
