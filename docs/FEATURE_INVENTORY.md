# Feature Inventory

Status legend: **verified** (implemented + contract tests + differential match against the upstream reference, now `main` @ b5823c7),
**implemented**, **partial**, **not started**, **out of scope (for now)**.
Upstream references are relative to the upstream repository root.

## 1. REST API coverage (generated from `compat/api/upstream-routes.json`, 2026-10-27)

321 upstream operations (excluding the catch-all). Ported operations are listed in `crates/coolify-api/src/routes/mod.rs`
(`ROUTES`); unported upstream routes answer 501.

| Resource group | Upstream ops | Ported | Status |
|---|---|---|---|
| `services` | 57 | 57 | see KNOWN_LIMITATIONS; `compat/diff/services_diff.py`, `compat/diff/service_backups_diff.py` for storage backups and database imports |
| `applications` | 52 | 13 | Docker image applications: list, show, create (`dockerimage`), start, restart, stop (POST + GET → 405), delete verified (`applications_diff.py`, 57/57 on a real VM including a live deployment). Git applications: create `public`, `private-deploy-key` and `private-github-app` for every git build pack (nixpacks, static, dockerfile, dockercompose, railpack) sharing the create prologue; public and deploy-key verified (`applications_public_diff.py`, 276/276: rows, settings, `NIXPACKS_NODE_VERSION` variables, labels, config hash, repository normalisation, generated names, validation messages); GitHub App create verified up to the GitHub API call (validation, 404s), happy path unit-tested only. Start/restart/stop/delete of git apps follow the deploy job's build pack support (dockercompose and inline Dockerfiles 501). Create fields not applied yet (`docker_compose_domains`, `watch_paths`, `custom_nginx_configuration`, basic auth, custom labels, tags, …), PATCH, envs, storages, previews answer 501 |
| `services` | 57 | 0 | not started |
| `applications` | 52 | 43 | Previews (list, show, create, domains, delete, logs), clone, rollback and rollback images, application logs, move, migrate (404 outside a dev instance), additional destinations verified by `applications_ops_diff.py`; the job does not build previews yet. Docker image applications: list, show, create (`dockerimage`), start, restart, stop (POST + GET → 405), delete verified (`applications_diff.py`, 57/57 on a real VM including a live deployment). Git applications: create `public`, `private-deploy-key` and `private-github-app` for every git build pack (nixpacks, static, dockerfile, dockercompose, railpack) sharing the create prologue; public and deploy-key verified (`applications_public_diff.py`, 276/276: rows, settings, `NIXPACKS_NODE_VERSION` variables, labels, config hash, repository normalisation, generated names, validation messages); GitHub App create verified up to the GitHub API call (validation, 404s), happy path unit-tested only. Start/restart/stop/delete of git apps and inline Dockerfiles are verified by `git_deploy_diff.py` (with custom Docker options, pre- and post-deployment commands, basic auth, file storages and rollback to an older commit; 331/338 on the whole run, the 7 others a ghcr.io timeout and the Railpack build time in the image, 24/24 when those two cases ran again). Every create field, `POST /applications/dockerfile` and `PATCH /applications/{uuid}` verified (`applications_fields_diff.py`, run from the two scripts above: responses, full application and settings rows, decrypted columns, labels with password hashes checked by PHP, config hash, Caddy servers with argon2id). Previews answer 501. Envs (list, create, update, bulk, delete), storages (list, create, update, delete, with the server writes), tags (list, add, remove, `GET /applications?tag=`, `tags` on create) and `PATCH /applications/{uuid}/secret-manager` verified (`applications_subresources_diff.py`, 256/256: responses with key order, rows, each side reading the rows of the other, files on the VM). Volume backup routes (`/storages/{uuid}/backups*`) answer 501 |
| `databases` | 47 | 17 | list, show, create (PostgreSQL, MySQL, MariaDB, MongoDB, Redis, ClickHouse, Dragonfly, KeyDB), start, stop, restart (POST + GET → 405), delete verified (`databases_diff.py --kind <engine>` on a real VM, byte-identical Compose files); SQLite, PATCH, backups, envs, storages, tags, imports, logs, move/clone answer 501 |
| `servers` | 44 | 40 | Everything but import, import check, migrate and transfer complete (transfer services not ported), verified by `compat/diff/servers_diff.py`: settings sub-routes, destinations, registries, proxy, shared variables on throwaway servers (`api`), provider routes against a fake with the requests compared (`cloud`), install, proxy, Sentinel, cleanup, log drains on throwaway VMs (`jobs`). `?force` with resources answers 501 |
| `databases` | 47 | 44 | all but the three volume backup routes; service databases are counted under services. Verified by `compat/diff/database_resources_diff.py` and `database_server_diff.py` |
| `servers` | 44 | 7 | list, show, create, update, delete, validate (`POST` + `GET` → 405) verified (194/194 differential; validation job 8/8 in `validate_diff.py`); `install=true`, `instant_validate`, proxy changes, `?force` with resources and cloud-provider deletes answer 501; resources, domains and the rest not started |
| `projects` | 18 | 10 | verified for project/environment CRUD; canvas links and project-level shared variables not started |
| `teams` / `team` | 12 | 8 | verified for reads and `PATCH /team`; team invitations not started |
| `notifications` | 12 | 0 | not started |
| `cloud-tokens`, `hetzner`, `digitalocean`, `vultr`, `hostinger`, `cloud-init-scripts` | 30 | 0 | not started |
| `github-apps`, `gitlab-apps` | 10 | 10 | all, verified by `compat/diff/git_sources_diff.py` (130/130) with provider answers from `compat/diff/fake_git_provider.py`, not a real provider |
| `s3-storages` | 6 | 6 | list, show, create, update, delete, validate |
| `security` (private keys, integration tokens) | 6 | 5 | private keys verified (CRUD, differential); integration tokens not started |
| `destinations`, `tags`, `resources`, `settings`, `audit-events` | 12 | 0 | not started |
| `deploy`, `deployments` | 6 | 6 | `POST /deploy` by uuid (tags and pull requests 501), `GET /deploy` → 405, deployments list/show/by application, cancel |
| `health`, `version`, `disable` | 5 | 5 | verified |
| `mcp`, `feedback`, `sentinel` | 4 | 0 | not started |
| **Total** | **321** | **33** | |

## 2. Cross-cutting foundations

| Feature | Upstream source | Status | Evidence |
|---|---|---|---|
| Sanctum token auth (`id|secret`, sha256, expiry, tokenable check) | `app/Models/User.php:266`, Sanctum 4 | verified | `tests/auth_chain.rs`, differential |
| Token team binding, membership + role re-check per request | `EnsureTokenBelongsToCurrentTeamMember` | verified | `removed_member_loses_access`, `members_with_elevated_tokens_are_refused` |
| API enabled switch, IP allowlist (IP/CIDR/IPv6, `0.0.0.0`) | `ApiAllowed`, `ApiIpAllowed`, `checkIPAgainstAllowlist` | verified (`main` semantics incl. `CF-Connecting-IP` from Cloudflare peers) | `api_disabled_and_ip_allowlist`, unit tests, 5 differential allowlist cases |
| Ability middleware (`root` superset, any-of, stacked all-of) | `ApiAbility` | verified | `abilities_are_enforced`, route inventory test |
| `can_read_sensitive` flag | `ApiSensitiveData` | implemented (no ported endpoint exposes sensitive fields yet) | unit-level |
| Error vocabulary and status codes | `Handler.php`, controllers | verified for ported routes | differential (102/102) |
| PHP `json_encode` byte compatibility (`\/`, `\uXXXX`) | Laravel `JsonResponse` | verified | `php_json` tests, byte-identical differential |
| `serializeApiResponse` key ordering | `bootstrap/helpers/api.php:45` | verified | unit + differential |
| Input normalization (TrimStrings, ConvertEmptyStringsToNull, query merge) | Laravel middleware | verified for ported fields | `input.rs` tests, differential |
| Laravel validation messages (required/string/min/max/regex/boolean, custom messages) | `ValidationPatterns.php`, `lang/en` | verified for ported rules | differential |
| Laravel encrypter (APP_KEY, previous keys) | `config/app.php` | verified both directions | `laravel_interop.rs`, `verify-encrypter-interop.sh` |
| Upstream schema adoption + schema guard (read-only on unknown migrations) | `database/migrations` | implemented | `read_only_mode_refuses_writes`, `schema.rs` tests |
| Health / readiness / liveness | `/api/health` | verified (`/api/health`); `/readyz`, `/livez` are coolify-rs additions | tests |
| Structured JSON logging, request ids, graceful shutdown, body limit, timeouts | — | implemented | `apps/coolify-rs` |
| Rate limiting (`throttle:api`, 200/min) | `RouteServiceProvider.php:47` | not started (login throttle implemented) | KNOWN_LIMITATIONS |
| SSH private key parsing (OpenSSH Ed25519/ECDSA/RSA, RSA PKCS#1/PKCS#8), public key + fingerprint | phpseclib in `PrivateKey` | verified against upstream output | `sshkey_interop.rs` |
| Browser login (bcrypt, throttle), sessions, CSRF, team switching, the web middleware order | Fortify, `UseSessionTeamApiToken`, `CheckForcePasswordReset`, `DecideWhatToDoWithUser`, `RequireTeamTwoFactor` | verified | `ui_session.rs`, `compat/ui/flow_auth.mjs`, `flow_auth5.mjs` |
| Registration and root setup | `CreateNewUser`, `RootSetupWindow`, `auth/register.blade.php` | verified: rows and audit against `CreateNewUser` run by tinker, errors and screens on both sides; root setup on an empty database against the rendered Blade | `flow_auth2.mjs`, `first_user.mjs` |
| Password reset, forced password change | `Controller::forgot_password`, password broker, `ResetUserPassword`, `ForcePasswordReset` | verified with a local mailpit; tokens and passwords written by coolify-rs are accepted by the reference and the other way around | `flow_auth2.mjs`, `flow_auth5.mjs` |
| Email verification | `VerifyEmail`, `Controller::email_verify` | verified: signatures cross-checked with the reference's key | `flow_auth2.mjs` |
| Two-factor challenge and requirement | Fortify, `TwoFactorRequired` | verified by flows on both sides | `flow_auth.mjs` |
| Passkey sign-in and confirmation | `laravel/passkeys` | verified with Chrome's virtual authenticator on both sides | `flow_auth3.mjs` |
| OAuth/SSO and `/settings/oauth` | `SettingsOauth`, `OauthController`, `OauthLoginService`, Socialite providers, `OidcProvider` | Authentik round trip against a local fake on both sides; other providers' redirects compared; OIDC by unit tests (KNOWN_LIMITATIONS) | `flow_auth4.mjs` |
| Web UI: upstream layout and navigation, dashboard, projects, environments, private keys, team settings, members, appearance | Blade views and Livewire components listed in `compat/mapping/registry.toml` (`ui.frontend`) | implemented | screenshot comparison with the reference 2026-10-27; `ui_pages.rs` (5 tests) |
| Audit events (`audit_events`) | `bootstrap/helpers/audit.php` | not started | KNOWN_LIMITATIONS |

## 3. Product areas (from upstream inventory; all not started unless stated)

| Area | Upstream entry points | Notes |
|---|---|---|
| Web UI beyond the ported areas | Livewire (272 components) | started (ADR 0007, 0008): sign-in screens, dashboard, projects, environments, keys, team, members, profile, 2FA enrollment, passkey management, appearance |
| Teams, invitations, roles | `app/Livewire/Team`, `TeamPolicy`, `Controller::{showInvitation,acceptInvitation,link,acceptLink}` | verified by UI flows on both sides (`compat/ui/flow_teams.mjs`, `flow_teams2.mjs`): new team, danger zone, member actions, invitations, audit log, instance users, avatar |
| Profile, two-factor, passkeys, select-team | `app/Livewire/Profile`, `SelectTeam`, `SettingsDropdown`, Fortify, `laravel/passkeys` | verified by UI flows on both sides (`compat/ui/flow_profile.mjs`, `flow_whatsnew.mjs`); see KNOWN_LIMITATIONS for what is left |
| Servers: validation, SSH, proxy, Sentinel, Cloudflare tunnel, transfers | `ServersController`, `SshMultiplexingHelper`, `app/Actions/Server` | records verified (API + UI: list, new server by IP, General, Danger Zone); SSH execution and validation verified (API + UI dialog, `validate_diff.py`); install, proxy, Sentinel, Docker cleanup, log drains, CA certificate, patch check, Traefik version check, cloud providers ported in the backend (`servers_diff.py`); the UI actions that change a server (proxy start, stop, restart, switch; destination create, delete, scan; unmanaged containers; image delete; Docker cleanup; log drains; Sentinel restart, regenerate, restore; registry login, test, logout; CA save, regenerate; private key switch; TLS delete, acme.json restore and delete; metrics; settings forms; Security Patches, Cloudflare Tunnel manual and token, GitHub Runners) ran through both UIs on throwaway VMs with toasts, `servers` and `server_settings` rows, destinations, containers, networks, files and commands compared (`compat/ui/server_actions.mjs`; coolify-rs through `scripts/ssh-via-orb.sh`, not real ssh); modals, empty states, a non-functional server, the member role and Sentinel logs compared by screenshot (`compat/ui/server_states.mjs`, `diff_pairs.py`); transfers not started |
| Remote execution (`instant_remote_process`, `parseCommandsByLineForSudo`, `SshRetryable`) | `bootstrap/helpers/remoteProcess.php`, `sudo.php`, `app/Traits/SshRetryable.php` | verified: upstream's unit cases + live test host | `coolify-remote` tests |
| Log redaction (`SensitiveDataRedactor::redactText`) and the `validation_logs` purifier | `app/Services/Security`, `config/purify.php` | verified: upstream's unit cases + reference container output | `coolify-compat` tests |
| Private keys | `SecurityController`, `PrivateKey` | verified (API + UI) |
| Git sources (GitHub/GitLab/Gitea/Bitbucket apps, webhooks) | `app/Http/Controllers/Webhook/*` | |
| Applications and build packs (nixpacks, railpack, dockerfile, compose, static, image) | `ApplicationDeploymentJob` (6,479 lines) | Docker image build pack verified (same commands, logs, Compose file, labels, env file, rolling update, health check; UI: Docker Image form, General, Deployment Logs, Danger zone, screenshot-compared 2026-10-28). UI for public git applications: Public Git Repository form (`loadBranch()` with the GitHub branch lookup and rate-limit callout, Smart Scan = `RepositoryDetector` on the destination server, env file import), General page for Dockerfile/Static/Nixpacks/Railpack (build strategy and its side effects, site type, web server, paths, build commands, Nginx configuration, image registry) and the Git Source page; screenshot-compared and rows diffed against the reference UI 2026-10-28 |
| Shared resource sections (environment variables, storages, runtime logs, limits, healthcheck, tags, webhooks, Advanced) | `Project\Shared\*`, `Project\Service\{Storage, FileStorage}`, `Project\Application\Advanced` | Ported as components taking a resource kind and uuid and mounted on application pages. Compared by screenshot, rows and audit events with `compat/ui/flow_shared_{envs,storages,config,logs,advanced}.mjs`; storage file work run on the test VM through the `ssh-via-orb` wrapper. See KNOWN_LIMITATIONS for what is left |
| Deployment queue, logs, cancellation, previews | `application_deployment_queues`, `bootstrap/helpers/applications.php` | |
| Deployment queue, logs, cancellation, previews | `application_deployment_queues`, `bootstrap/helpers/applications.php` | queue rows, preview rows, preview builds, force start (UI route) and `POST /deploy` with a pull request ported; deploy by tag not |
| Standalone databases (9 engines), public proxy | `app/Actions/Database/*` | 8 engines verified (Start* actions with environment variables, StopDatabase, RestartDatabase, DeleteResourceJob, status refresh and the database part of GetContainersStatus; UI: environment resource list, new-resource picker, database page with each engine's General form); SQLite, public TCP proxy, SSL, init scripts upload, the env var page and backups not started |
| Symfony YAML dump (`Yaml::dump($compose, 10)`) | symfony/yaml 7 | verified against the reference image (48 golden cases) | `symfony_yaml.rs` |
| One-click services: API, `serviceParser()`, actions, status polling, and the pages `Project/Service/{Configuration,StackForm,EditCompose,Heading,Status,ResourceCard,Index,Domains,Storage,FileStorage}` with one-click creation (`crates/coolify-api/src/ui_services*`, `frontend/src/pages/Service.svelte`) | `templates/service-templates-latest.json`, `bootstrap/helpers/parsers.php`, `app/Actions/Service` | verified: `compat/diff/services_diff.py`, `crates/coolify-jobs/tests/service_parser.rs` |
| Domains, Traefik/Caddy labels, TLS (ACME) | `bootstrap/helpers/docker.php`, `proxy.php` | |
| Environment variables, shared variables, secret managers | `EnvironmentVariable`, `SharedEnvironmentVariable` | value storage format must be confirmed at runtime |
| Persistent storage, volume backups | `LocalPersistentVolume`, `VolumeBackupJob` | implemented for applications, services and databases; not started by the scheduler |
| Database backups to S3, restore | `DatabaseBackupJob`, `StartDatabaseImport` | implemented for every engine with a backup solution; `database_server_diff.py` |
| Database pages | `Project/Database/*`: General and StatusInfo for all types, SQLite with ConnectApplication, Health, Import, Backups, executions | ported; `compat/ui/flow_database_pages.mjs` compares both UIs |
| Storage backups | `VolumeBackupsController`, `VolumeBackupJob` | manual runs and schedules through the API; `compat/diff/storage_backups_diff.py`, `compat/diff/service_backups_diff.py`; not started by the scheduler |
| Service and application backup pages | `Project/Service/{DatabaseBackups,BackupExecutions,ImportBackup,VolumeBackup/*}`, `Project/Application/Backup/*`, `Project/Shared/Storages/VolumeBackups`, `PreparesBackupDownloads` | ported; `compat/ui/flow_backups.mjs` compares both UIs; S3 downloads have not run, see KNOWN_LIMITATIONS |
| Scheduled tasks, scheduler, Horizon queues | `app/Console/Kernel.php`, 72 jobs | |
| Cloud providers (Hetzner, DigitalOcean, Vultr, Hostinger), cloud-init | controllers + jobs, `Server\New\By*`, `Server\CloudProviderToken\Show` | API and jobs verified against a fake (`servers_diff.py cloud`); UI forms and Cloud Token page compared with upstream's Livewire components under `Http::fake` (`compat/ui/cloud_ui.mjs`: validation messages, provider requests, server rows); Security pages for tokens and scripts not ported |
| Sending notifications (email, Discord, Slack, Telegram, Pushover, webhook), delivery failure tracking, transactional mail | `app/Notifications/*`, `TracksDeliveryFailures` | verified: `compat/diff/notifications_diff.py` (subject, mail HTML, request bodies), `compat/diff/notifications_e2e.py` (delivery to mailpit and a local endpoint), `crates/coolify-jobs/tests/notifications_delivery.rs`; triggers not wired are in KNOWN_LIMITATIONS |
| Terminal page | `Terminal\Index`, `can.access.terminal` | target list, filter and gate compared on both sides (`compat/ui/flow_terminal.mjs`); the shell is not ported (KNOWN_LIMITATIONS) |
| File browser, terminal shell | `docker/coolify-terminal`, `ContainerFilesystemService` | high-risk; security review required |
| Instance settings | `Settings\Index`, `Advanced`, `Updates`, `SettingsEmail`, `SettingsBackup` | rows, toasts and audit events compared on both sides (`compat/ui/flow_settings.mjs`, `flow_mail.mjs`); see KNOWN_LIMITATIONS |
| Layout | `GlobalSearch`, `DeploymentsIndicator`, `Dashboard\ActiveDeployments`, `LayoutPopups`, `Upgrade` | results of the same queries and the screens compared (`compat/ui/flow_layout.mjs`) |
| Onboarding | `Boarding\Index` | remote path through the SSH test host compared on both sides (`compat/ui/flow_onboarding.mjs`) |
| Support form | `Help` | validation on both sides, SMTP path through mailpit on both, Cloud API path against a local fake on coolify-rs (`compat/ui/flow_mail.mjs`) |
| Cloud providers (Hetzner, DigitalOcean, Vultr, Hostinger), cloud-init | controllers + jobs | UI: cloud tokens and cloud-init scripts verified on both UIs (`compat/ui/flow_security.mjs`) up to the calls to the providers |
| UI: notifications (email, Discord, Slack, Telegram, Pushover, webhook) | `Livewire/Notifications/*` | verified on both UIs (`compat/ui/flow_notifications.mjs`): toasts, rows, audit events, member and instance admin states |
| UI: tags, deploy by tag | `Livewire/Tags/Show`, `DeployController::by_tags` | verified on both UIs (`compat/ui/flow_tags.mjs`, `compat/diff/tags_deploy_diff.py`) except a deployment started from a tag; the canvas is not ported |
| UI: shared variables | `Livewire/SharedVariables/*`, `Project/Shared/EnvironmentVariable/{Add, Show}` for shared variables | verified on both UIs (`compat/ui/flow_shared_variables.mjs`); the canvas is not ported |
| UI: S3 storage | `Livewire/Storage/*` | verified on both UIs against a local MinIO (`compat/ui/flow_storages.mjs`) |
| UI: API tokens, integration tokens | `Livewire/Security/{ApiTokens, IntegrationToken*}` | verified on both UIs (`compat/ui/flow_security.mjs`, Vault against a local fake); Cloudflare tokens are refused, see KNOWN_LIMITATIONS |
| Persistent storage, volume backups | `LocalPersistentVolume`, `VolumeBackupJob` | |
| Database backups to S3, restore | `DatabaseBackupJob` | |
| Scheduled tasks, scheduler, Horizon queues | `app/Console/Kernel.php`, 72 jobs | application tasks API and `ScheduledTaskJob` ported; scheduler with a source interface (`coolify_jobs::scheduler`), tasks registered; backups, cleanups and other jobs not |
| Notifications (email, Discord, Slack, Telegram, Pushover, webhook) | `app/Notifications/*` | SSRF protections must be ported with them |
| Terminal and file browser | `docker/coolify-terminal`, `ContainerFilesystemService` | high-risk; security review required |
| Cloud providers (Hetzner, DigitalOcean, Vultr, Hostinger), cloud-init | controllers + jobs | |
| MCP server, Sentinel metrics, traffic analytics | `routes/ai.php`, `SentinelController` | |
| Traffic analytics charts (server page and `/analytics`) | `Livewire\Analytics`, `SentinelTrafficClient`, `TrafficAnalyticsAggregator`, `TrafficResource` | `crates/coolify-api/src/ui_server_analytics.rs`, `frontend/src/components/analytics/`, compared by `compat/ui/analytics_compare.mjs` |
| Install / upgrade scripts, self-update | `scripts/install.sh`, `upgrade.sh`, `UpdateCoolifyJob` | planned, Milestone 7 of IMPLEMENTATION_ROADMAP.md |
| Billing (Stripe, cloud only) | `app/Http/Controllers/Webhook/Stripe.php` | out of scope (self-hosted focus) |

## 4. Migration engine features

| Capability | Status | Evidence |
|---|---|---|
| Read-only upstream mirror, fetch, commit ranges, first-parent units | implemented | `git.rs`, `coolify-port sync/status` |
| Persistent tracking state (baseline / last analysed / last integrated; forward-only) | implemented | `migration/state.json` |
| Classification with path + content + message evidence | implemented (uncalibrated) | `classify.rs` tests, ledger |
| Laravel route extraction | verified (321/321 identical to `route:list`) | `tests/route_parser_reference.rs` |
| Migration summaries (`up()` table/column ops, destructive/raw SQL/encryption flags) | implemented | `migrations.rs` tests |
| Mapping registry and impact query | implemented | `compat/mapping/registry.toml`, `mapping.rs` |
| Plans with risk, actions per layer (A–D), validation commands, escalation | implemented | `plan.rs`, ledger |
| Layer A generation (route inventory, supported migrations) in an isolated branch/worktree | implemented | run `run-5ca69705d-b5823c777` |
| Validation with 5-state outcomes, allowlisted programs, required-env preconditions | implemented | `validate.rs` |
| Bounded repair via external agent + test-tampering guard | implemented, **agent disabled by default** | `run.rs` tests |
| Reports, ledger, outcome metrics, prompt packs | implemented | `migration/ledger/` |
| Resume interrupted runs | implemented (stage file) | `run.rs` |
| Layer B templates (e.g. optional column registration) | not started | roadmap |
| Classification calibration against labelled history | not started | roadmap |

## Server transfer

`ServerTransferController` (`GET /servers/import/check`, `POST /servers/import`, `POST /servers/{uuid}/migrate`,
`POST /servers/{uuid}/transfer/complete`, `GET /servers/{uuid}/transfer`), `ServerTransferExporter`, `ServerTransferImporter`,
`ServerTransferClaimer` (claim, mark transferred, owner marker in `ServerConnectionCheckJob`, delete archive, take back),
`ServerTransferMigrator` with `MigrateServerToInstanceJob`, `TransferFinishChecklist` with `CheckTransferFinishStepsJob`, and the
pages `Server/Transfer` and `Server/TransferFinish`. Differential: `compat/diff/transfer_diff.py`.

## Resource operations and shared sections

| Section | Upstream | Status |
|---|---|---|
| Clone and move a resource, clone an environment | `Project\Shared\ResourceOperations`, `Project\CloneMe` | application, database and service: rows compared with the reference after clones and moves through both UIs (`compat/ui/flow_operations.mjs`) |
| Servers section | `Project\Shared\Destination` | primary, additional, add, promote, remove compared (`compat/ui/flow_destination.mjs`); deploy to one additional server not ported |
| Maintenance mode | `Project\Shared\Maintenance`, `Server::setupMaintenancePages` | Traefik files on the VM compared (`compat/ui/flow_files.mjs`) |
| Files | `Project\Shared\FileBrowser`, `ContainerFilesystemService` | create, edit, chmod, rename, upload, download and delete compared inside a running database container |
| Danger on service pages, application delete with file storages | `Project\Shared\Danger`, `DeleteResourceJob` | rows, directory and volume compared after deletes through both UIs |
| Application domains | `Project\Application\Domains` | rows and labels compared (`compat/ui/flow_domains.mjs`); DNS checks not ported |
| Swarm settings, rollback start, Refresh GPUs, server status badge | `Application\Swarm`, `Rollback`, `Advanced`, `ServerStatusBadge` | compared, except the badge's warning states |

## Deploy stream

- `GET /projects/{uuid}/{environment}` with its resources.
- Deploy job: additional servers (`parent_deployment_uuid`, `only_this_server`, combined notification), build servers,
  registry pushes, Swarm stacks for applications. Status: `ComplexStatusCheck` and Swarm service replicas.
- `/deploy` with services and databases, the public port of a service database, Flush all data, clones with volumes for
  services and across servers (`VolumeCloneJob::cloneRemoteVolume`), `trustedInternalHosts()` for the bundled MinIO.

