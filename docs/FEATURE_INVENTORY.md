# Feature Inventory

Status legend: **verified** (implemented + contract tests + differential match against the upstream reference, now `main` @ b5823c7),
**implemented**, **partial**, **not started**, **out of scope (for now)**.
Upstream references are relative to the upstream repository root.

## 1. REST API coverage (generated from `compat/api/upstream-routes.json`, 2026-10-27)

321 upstream operations (excluding the catch-all). Ported operations are listed in `crates/coolify-api/src/routes/mod.rs`
(`ROUTES`); unported upstream routes answer 501.

| Resource group | Upstream ops | Ported | Status |
|---|---|---|---|
| `services` | 57 | 0 | not started |
| `applications` | 52 | 0 | not started |
| `databases` | 47 | 0 | not started |
| `servers` | 44 | 5 | list, show, create, update, delete verified (183/183 differential); no SSH: `instant_validate`, proxy changes, `?force` with resources and cloud-provider deletes answer 501; resources, domains and the rest not started |
| `projects` | 18 | 10 | verified for project/environment CRUD; canvas links and project-level shared variables not started |
| `teams` / `team` | 12 | 8 | verified for reads and `PATCH /team`; team invitations not started |
| `notifications` | 12 | 0 | not started |
| `cloud-tokens`, `hetzner`, `digitalocean`, `vultr`, `hostinger`, `cloud-init-scripts` | 30 | 0 | not started |
| `github-apps`, `gitlab-apps` | 10 | 0 | not started |
| `s3-storages` | 6 | 0 | not started |
| `security` (private keys, integration tokens) | 6 | 5 | private keys verified (CRUD, differential); integration tokens not started |
| `destinations`, `tags`, `resources`, `settings`, `audit-events` | 12 | 0 | not started |
| `deploy`, `deployments` | 6 | 0 | not started |
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
| Browser login (bcrypt, TOTP 2FA, throttle), sessions, CSRF, team switching | Fortify, `UseSessionTeamApiToken`, `RequireTeamTwoFactor` | implemented | `ui_session.rs` (8 tests), manual browser test |
| Web UI: upstream layout and navigation, dashboard, projects, environments, private keys, team settings, members, appearance | Blade views and Livewire components listed in `compat/mapping/registry.toml` (`ui.frontend`) | implemented | screenshot comparison with the reference 2026-10-27; `ui_pages.rs` (5 tests) |
| Audit events (`audit_events`) | `bootstrap/helpers/audit.php` | not started | KNOWN_LIMITATIONS |

## 3. Product areas (from upstream inventory; all not started unless stated)

| Area | Upstream entry points | Notes |
|---|---|---|
| Web UI beyond the ported areas; registration/root setup window, password reset, 2FA enrollment, passkeys, OAuth | Livewire (272 components), Fortify, Socialite | started (ADR 0007, 0008): login, dashboard, projects, environments, keys, team, members, appearance |
| Teams, invitations, roles | `app/Livewire/Team`, `TeamPolicy` | API reads verified; UI settings and member roles verified; invitations not started |
| Servers: validation, SSH, proxy, Sentinel, Cloudflare tunnel, transfers | `ServersController`, `SshMultiplexingHelper`, `app/Actions/Server` | records verified (API + UI: list, new server by IP, General, Danger Zone); everything that connects to a server not started (milestone 4) |
| Private keys | `SecurityController`, `PrivateKey` | verified (API + UI) |
| Git sources (GitHub/GitLab/Gitea/Bitbucket apps, webhooks) | `app/Http/Controllers/Webhook/*` | |
| Applications and build packs (nixpacks, railpack, dockerfile, compose, static, image) | `ApplicationDeploymentJob` (6,479 lines) | critical, large |
| Deployment queue, logs, cancellation, previews | `application_deployment_queues`, `bootstrap/helpers/applications.php` | |
| Standalone databases (9 engines), public proxy | `app/Actions/Database/*` | |
| One-click services (386 compose templates) | `templates/compose`, `bootstrap/helpers/parsers.php` | |
| Domains, Traefik/Caddy labels, TLS (ACME) | `bootstrap/helpers/docker.php`, `proxy.php` | |
| Environment variables, shared variables, secret managers | `EnvironmentVariable`, `SharedEnvironmentVariable` | value storage format must be confirmed at runtime |
| Persistent storage, volume backups | `LocalPersistentVolume`, `VolumeBackupJob` | |
| Database backups to S3, restore | `DatabaseBackupJob` | |
| Scheduled tasks, scheduler, Horizon queues | `app/Console/Kernel.php`, 72 jobs | |
| Notifications (email, Discord, Slack, Telegram, Pushover, webhook) | `app/Notifications/*` | SSRF protections must be ported with them |
| Terminal and file browser | `docker/coolify-terminal`, `ContainerFilesystemService` | high-risk; security review required |
| Cloud providers (Hetzner, DigitalOcean, Vultr, Hostinger), cloud-init | controllers + jobs | |
| MCP server, Sentinel metrics, traffic analytics | `routes/ai.php`, `SentinelController` | |
| Install / upgrade scripts, self-update | `scripts/install.sh`, `upgrade.sh`, `UpdateCoolifyJob` | |
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
