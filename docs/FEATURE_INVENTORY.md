# Feature Inventory

Status legend: **verified** (implemented + contract tests + differential match against the upstream reference, now `main` @ b5823c7),
**implemented**, **partial**, **not started**, **out of scope (for now)**.
Upstream references are relative to the upstream repository root.

## 1. REST API coverage

All 321 upstream operations (excluding the catch-all) are implemented and listed in
`crates/coolify-api/src/routes/mod.rs` (`ROUTES`). `scripts/parity.py` compares them, the web pages and the Livewire
components with upstream; `compat/diff/` holds the differential scripts per area.

## 2. Cross-cutting foundations

| Feature | Upstream source | Status | Evidence |
|---|---|---|---|
| Sanctum token auth (`id|secret`, sha256, expiry, tokenable check) | `app/Models/User.php:266`, Sanctum 4 | verified | `tests/auth_chain.rs`, differential |
| Token team binding, membership + role re-check per request | `EnsureTokenBelongsToCurrentTeamMember` | verified | `removed_member_loses_access`, `members_with_elevated_tokens_are_refused` |
| Scoped access grants of members and tokens (4.6.0): `team_user.access`, token `scopes`, paused tokens, `delete` ability, `tokenWithinCreatorAccess`, scoped API routes, `VisibleThroughAccessGrants`, `access.team-wide` pages | `AccessResolver`, `AccessGrants`, `Grant`, `ApiAbility`, `EnsureTeamWideAccess`, policies | ported; Rust tests only (no reference stack) | `crates/coolify-api/tests/access_grants.rs`, `crates/coolify-db/tests/access.rs`, unit tests in `coolify_domain::grants` |
| API tokens page (scopes, edit, pause, resume, roll, 7-day expiry warning) and member access editor, invitations with scopes | `Security\ApiTokens`, `Forms\ApiTokenForm`, `Team\Member\Access`, `Team\InviteLink` | ported; not compared by screenshot | `ui_api_tokens.rs`, `ui_team_access.rs` unit tests |
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

## 3. Product areas (from upstream inventory; all not started unless stated)

| Area | Upstream entry points | Notes |
|---|---|---|
| Web UI beyond the ported areas | Livewire (272 components) | started (ADR 0007, 0008): sign-in screens, dashboard, projects, environments, keys, team, members, profile, 2FA enrollment, passkey management, appearance |
| Teams, invitations, roles | `app/Livewire/Team`, `TeamPolicy`, `Controller::{showInvitation,acceptInvitation,link,acceptLink}` | verified by UI flows on both sides (`compat/ui/flow_teams.mjs`, `flow_teams2.mjs`): new team, danger zone, member actions, invitations, audit log, instance users, avatar |
| Profile, two-factor, passkeys, select-team | `app/Livewire/Profile`, `SelectTeam`, `SettingsDropdown`, Fortify, `laravel/passkeys` | verified by UI flows on both sides (`compat/ui/flow_profile.mjs`, `flow_whatsnew.mjs`); see KNOWN_LIMITATIONS for what is left |
| Remote execution (`instant_remote_process`, `parseCommandsByLineForSudo`, `SshRetryable`) | `bootstrap/helpers/remoteProcess.php`, `sudo.php`, `app/Traits/SshRetryable.php` | verified: upstream's unit cases + live test host | `coolify-remote` tests |
| Log redaction (`SensitiveDataRedactor::redactText`) and the `validation_logs` purifier | `app/Services/Security`, `config/purify.php` | verified: upstream's unit cases + reference container output | `coolify-compat` tests |
| Private keys | `SecurityController`, `PrivateKey` | verified (API + UI) |
| Git sources (GitHub/GitLab/Gitea/Bitbucket apps, webhooks) | `app/Http/Controllers/Webhook/*` | |
| Applications and build packs (nixpacks, railpack, dockerfile, compose, static, image) | `ApplicationDeploymentJob` (6,479 lines) | Docker image build pack verified (same commands, logs, Compose file, labels, env file, rolling update, health check; UI: Docker Image form, General, Deployment Logs, Danger zone, screenshot-compared 2026-10-28). UI for public git applications: Public Git Repository form (`loadBranch()` with the GitHub branch lookup and rate-limit callout, Smart Scan = `RepositoryDetector` on the destination server, env file import), General page for Dockerfile/Static/Nixpacks/Railpack (build strategy and its side effects, site type, web server, paths, build commands, Nginx configuration, image registry) and the Git Source page; screenshot-compared and rows diffed against the reference UI 2026-10-28 |
| Shared resource sections (environment variables, storages, runtime logs, limits, healthcheck, tags, webhooks, Advanced) | `Project\Shared\*`, `Project\Service\{Storage, FileStorage}`, `Project\Application\Advanced` | Ported as components taking a resource kind and uuid and mounted on application pages. Compared by screenshot, rows and audit events with `compat/ui/flow_shared_{envs,storages,config,logs,advanced}.mjs`; storage file work run on the test VM through the `ssh-via-orb` wrapper. See KNOWN_LIMITATIONS for what is left |
| Deployment queue, logs, cancellation, previews | `application_deployment_queues`, `bootstrap/helpers/applications.php` | |
| Symfony YAML dump (`Yaml::dump($compose, 10)`) | symfony/yaml 7 | verified against the reference image (48 golden cases) | `symfony_yaml.rs` |
| One-click services: API, `serviceParser()`, actions, status polling, and the pages `Project/Service/{Configuration,StackForm,EditCompose,Heading,Status,ResourceCard,Index,Domains,Storage,FileStorage}` with one-click creation (`crates/coolify-api/src/ui_services*`, `frontend/src/pages/Service.svelte`) | `templates/service-templates-latest.json`, `bootstrap/helpers/parsers.php`, `app/Actions/Service` | verified: `compat/diff/services_diff.py`, `crates/coolify-jobs/tests/service_parser.rs` |
| Domains, Traefik/Caddy labels, TLS (ACME) | `bootstrap/helpers/docker.php`, `proxy.php` | |
| Environment variables, shared variables, secret managers | `EnvironmentVariable`, `SharedEnvironmentVariable`, `HasSecretManager`, `SecretManagerLinks` | secret managers verified against a local fake and on the test VM (`compat/diff/integrations_diff.py`, `secret_deploy_diff.py`); application deployments compared, database and service starts not |
| Cloudflare, DNS, GitHub runners | `CloudflareTunnelService`, `CloudflareDnsProvider`, `ManagedDnsRecordCleanup`, `CheckDomainDns`, `GithubRunner*` | verified against local fakes (`integrations_diff.py`, `github_runners_diff.py`), see `KNOWN_LIMITATIONS.md`; the v4.6.0 API for DNS records, integration tokens and resource secret managers (`DnsController`, `IntegrationTokensController`, `SecretManagerController`) has contract tests only |
| Private repositories to a deploy | `Project/New/{Github,Gitlab,Gitea}PrivateRepository` | `compat/diff/private_repo_e2e.py`: a real Gitea container, fakes for GitLab and GitHub |
| Database backups to S3, restore | `DatabaseBackupJob`, `StartDatabaseImport` | implemented for every engine with a backup solution; `database_server_diff.py` |
| Database pages | `Project/Database/*`: General and StatusInfo for all types, SQLite with ConnectApplication, Health, Import, Backups, executions | ported; `compat/ui/flow_database_pages.mjs` compares both UIs |
| Scheduled tasks, scheduler, Horizon queues | `app/Console/Kernel.php`, 72 jobs | |
| Sending notifications (email, Discord, Slack, Telegram, Pushover, webhook), delivery failure tracking, transactional mail | `app/Notifications/*`, `TracksDeliveryFailures` | verified: `compat/diff/notifications_diff.py` (subject, mail HTML, request bodies), `compat/diff/notifications_e2e.py` (delivery to mailpit and a local endpoint), `crates/coolify-jobs/tests/notifications_delivery.rs`; triggers not wired are in KNOWN_LIMITATIONS |
| Terminal page | `Terminal\Index`, `can.access.terminal` | target list, filter and gate compared on both sides (`compat/ui/flow_terminal.mjs`); the shell is ported, see Terminal shell |
| Terminal shell | `docker/coolify-terminal`, `Project\Shared\Terminal`, `Project\Shared\ExecuteContainerCommand`, `TerminalSessionService`, `can.access.terminal` | websocket `/terminal/ws` in this server (`terminal_ws.rs`), upstream's `terminal.js` vendored; real shells in a browser on server, application, database, service and `/terminal` (`compat/ui/terminal_ui.mjs`), screens and socket messages against the reference (`compat/ui/terminal_states.mjs`), refusals in `crates/coolify-api/tests/terminal.rs` |
| Instance settings | `Settings\Index`, `Advanced`, `Updates`, `SettingsEmail`, `SettingsBackup` | rows, toasts and audit events compared on both sides (`compat/ui/flow_settings.mjs`, `flow_mail.mjs`); see KNOWN_LIMITATIONS |
| Layout | `GlobalSearch`, `DeploymentsIndicator`, `Dashboard\ActiveDeployments`, `LayoutPopups`, `Upgrade` | results of the same queries and the screens compared (`compat/ui/flow_layout.mjs`) |
| Onboarding | `Boarding\Index`, `Server\New\By*` modals | remote path through the SSH test host compared on both sides (`compat/ui/flow_onboarding.mjs`), provider cards in `compat/ui/flow_onboarding_cloud.mjs` |
| Support form | `Help` | validation on both sides, SMTP path through mailpit on both, Cloud API path against a local fake on coolify-rs (`compat/ui/flow_mail.mjs`) |
| Cloud providers (Hetzner, DigitalOcean, Vultr, Hostinger), cloud-init | controllers + jobs | UI: cloud tokens and cloud-init scripts verified on both UIs (`compat/ui/flow_security.mjs`) up to the calls to the providers |
| UI: notifications (email, Discord, Slack, Telegram, Pushover, webhook) | `Livewire/Notifications/*` | verified on both UIs (`compat/ui/flow_notifications.mjs`): toasts, rows, audit events, member and instance admin states |
| UI: S3 storage | `Livewire/Storage/*` | verified on both UIs against a local MinIO (`compat/ui/flow_storages.mjs`) |
| Persistent storage, volume backups | `LocalPersistentVolume`, `VolumeBackupJob` | |
| Database backups to S3, restore | `DatabaseBackupJob` | |
| Scheduled tasks, scheduler, Horizon queues | `app/Console/Kernel.php`, 72 jobs | one scheduler (`coolify_jobs::scheduler`, `kernel`, `schedule_sources`): every source of `ScheduledJobManager`, `ServerManagerJob`, the Kernel's jobs and commands; `compat/diff/kernel_schedule_diff.py`, `compat/diff/scheduler_window.py` |
| Application and service screens | `Project\Shared\ScheduledTask\*`, `Project\Application\{Previews, Preview\Form, PreviewDomains}`, `Project\Shared\ResourceDetails`, `Application\General` | scheduled tasks for applications and services; previews with domains, stop and delete; service details; Compose settings, basic auth, deployment commands and Docker options on General (`compat/ui/flow_appui_*.mjs`); DNS checks not |
| Notifications (email, Discord, Slack, Telegram, Pushover, webhook) | `app/Notifications/*` | SSRF protections must be ported with them |
| Cloud providers (Hetzner, DigitalOcean, Vultr, Hostinger), cloud-init | controllers + jobs | |
| MCP server, Sentinel metrics, traffic analytics | `routes/ai.php`, `SentinelController` | |
| Traffic analytics charts (server page and `/analytics`) | `Livewire\Analytics`, `SentinelTrafficClient`, `TrafficAnalyticsAggregator`, `TrafficResource` | `crates/coolify-api/src/ui_server_analytics.rs`, `frontend/src/components/analytics/`, compared by `compat/ui/analytics_compare.mjs` |
| Fleet metrics, resource and dashboard charts, dashboard and resource traffic analytics | `Fleet\Metrics`, `HasMetrics`, `SentinelMetricsClient`, `Dashboard\{ServerMetricsChart,TrafficAnalytics}`, `Project\Shared\{Metrics,ResourceTrafficAnalytics}` | `crates/coolify-api/src/ui_fleet_metrics.rs`, `ui_server_metrics.rs`, `ui_server_analytics/widgets.rs`, compared by `compat/diff/metrics_diff.py` and `compat/ui/metrics_compare.mjs` |
| Install / upgrade scripts, self-update | `scripts/install.sh`, `upgrade.sh`, `ProductionSeeder`, `UpdateCoolify`, `UpdateCoolifyJob`, `CheckForUpdatesJob`, `Upgrade` | ported (`scripts/install`, `coolify-rs boot`, `crates/coolify-jobs/src/first_run.rs`, `updates.rs`), compared on two fresh VMs (`compat/diff/install_diff.py`, `compat/ui/install_firstrun.mjs`) |
| Canvases | `Livewire/{DeployCanvas, Dashboard/Canvas, Project/*Canvas, Server/Canvas, SharedVariables/Canvas, Tags/Canvas}`, `Concerns/RemembersCanvasView`, `Api/*CanvasController`, `Services/*Canvas` | verified on both sides (`compat/diff/canvas_diff.mjs`, `compat/ui/canvas_flow.mjs`, screenshots) |
| Project icons | `ProjectIconController`, `ProjectIconStorageService`, `Project/Edit` | verified in `compat/diff/canvas_diff.mjs`; local disk only |
| Admin (cloud or dev) | `Livewire/Admin/Index` | self-hosted 403 compared; root path in `tests/ui_admin.rs` |
| Billing (Stripe, cloud only) | `Livewire/Subscription/*`, `Actions/Stripe/*`, `Webhook/Stripe`, `StripeProcessJob` | self-hosted redirects compared; Cloud path against a local fake only (`compat/diff/subscription_fake.mjs`) |

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
| Maintenance mode | `Project\Shared\Maintenance`, `Server::setupMaintenancePages` | Traefik files on the VM compared (`compat/ui/flow_files.mjs`) |
| Files | `Project\Shared\FileBrowser`, `ContainerFilesystemService` | create, edit, chmod, rename, upload, download and delete compared inside a running database container |
| Danger on service pages, application delete with file storages | `Project\Shared\Danger`, `DeleteResourceJob` | rows, directory and volume compared after deletes through both UIs |
| Swarm settings, rollback start, Refresh GPUs, server status badge | `Application\Swarm`, `Rollback`, `Advanced`, `ServerStatusBadge` | compared, except the badge's warning states |

## Deploy stream

- `GET /projects/{uuid}/{environment}` with its resources.
- Deploy job: additional servers (`parent_deployment_uuid`, `only_this_server`, combined notification), build servers,
  registry pushes, Swarm stacks for applications. Status: `ComplexStatusCheck` and Swarm service replicas.
- `/deploy` with services and databases, the public port of a service database, Flush all data, clones with volumes for
  services and across servers (`VolumeCloneJob::cloneRemoteVolume`), `trustedInternalHosts()` for the bundled MinIO.

