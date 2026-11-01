# Security Model

Covers the upstream security model (static analysis of `b5823c77`), the threat model for coolify-rs, and the
controls implemented so far. Upstream paths are relative to the upstream repository root.

## 1. Upstream authentication

- Web: Laravel Fortify (login, registration, password reset/update, TOTP 2FA with confirmation, passkeys).
  Email verification is not enforced on self-hosted.
- Passwords: bcrypt, cost `BCRYPT_ROUNDS` (default 10). Production password policy: min 8, mixed case, letters,
  numbers, symbols, `uncompromised()` (HIBP).
- Sessions: `database` driver, 7-day lifetime, encrypted payload, `HttpOnly`, `SameSite=Lax`, auto-`Secure` behind HTTPS.
- First user: forced to id 0, owner of team 0, only within a 10-minute "root setup window" opened at boot when no
  users exist; registration is then disabled.
- OAuth/OIDC via Socialite (11 providers) with identity linking rules; `client_secret` encrypted with APP_KEY.
- Invite "magic links": `Crypt::encryptString("{email}@@@{invitationUuid}@@@{password}")`, single use, password rotated on accept.
- API: Sanctum personal access tokens bound to a team (`personal_access_tokens.team_id`), abilities
  `read`, `read:sensitive`, `write`, `write:sensitive`, `deploy`, `root`. Only admins/owners may mint beyond `read`.
- Rate limits: login 5/min per email+IP and per email identity; forgot-password 3/10min; API 200/min.

## 2. Upstream authorization

- Team RBAC: `team_user.role` ∈ {`owner`, `admin`, `member`}. Team 0 is the instance-admin team;
  "instance admin" = admin/owner of team 0.
- Policy pattern: `view` = member of the owning team; `create` = admin of current team; `update/delete` = admin of the
  owning team; server `transfer` and team `delete` = owner. System-wide Git apps are viewable by everyone.
  Private keys with `team_id = 0` require instance-admin.
- API: `api.token.team` re-validates membership per request; members holding elevated tokens are refused even if the
  token predates a demotion; `can_read_sensitive` requires both the ability and admin role.

## 3. Upstream sensitive operations

- All server access is root-equivalent SSH with keys stored encrypted in Postgres and written to disk at use.
  **Host keys are not verified** (`StrictHostKeyChecking=no`, `UserKnownHostsFile=/dev/null`).
- User-supplied build/start/pre/post-deploy commands run by design (`SECURITY_ADVISORY.md`): not a vulnerability
  in itself; in-scope classes are authorization bypass, cross-team access, execution without deploy permission,
  and secret leakage.
- Web terminal: one-time 64-char tokens (60s) bound to user and team. Upstream's Node websocket server re-validates SSH
  arguments against an allowlist; in coolify-rs the websocket is in the same process and the browser never sends a command,
  so a token selects a session prepared on the server, and the session, team, terminal setting, key owner and container are
  checked again when it is redeemed. A socket from another origin is refused.
- File browser: admin + view permission, path character blocklist, `escapeshellarg`, optional locked-down helper container.
- SSRF: `SafeWebhookUrl` resolves all A/AAAA records, blocks link-local always, loopback/RFC1918/CGNAT unless
  allowlisted, pins DNS with `CURLOPT_RESOLVE`, disallows cross-origin redirects.
- Input validation catalog in `app/Support/ValidationPatterns.php` and `app/Rules/*` (git URL/branch, docker image,
  hostname, IP, container name, env key, DB identifiers, port mappings).
- Webhooks: GitHub/Gitea/Bitbucket HMAC-SHA256 with `hash_equals`; GitLab token `hash_equals`; Stripe SDK verification.

Regression tests that encode past fixes (names only; useful as specs): cross-team IDOR tests, command-injection tests
(SSH, health checks, log drains, docker networks, deployment newlines, compose), path traversal, XSS, SSRF,
secret visibility, token privilege, terminal session security, IP allowlist.

### Findings from this analysis

- **Spoofable client IP (upstream).** `TrustProxies::$proxies = '*'` makes `X-Forwarded-For` authoritative for any
  peer, so the API IP allowlist (and IP-keyed rate limits) can be bypassed when the app is reachable directly.
  coolify-rs diverges (ADR 0006).
- **No SSH host-key verification (upstream).** coolify-rs pins host keys on first use (ADR 0009).
- Upstream tests run on SQLite and are not run in upstream CI, so security regressions are caught only by review.

## 4. Encryption at rest (compatibility requirement)

Laravel Encrypter, AES-256-CBC + HMAC-SHA256, key = `APP_KEY` (`base64:` 32 bytes), optional `APP_PREVIOUS_KEYS`.
Encrypted columns include `private_keys.private_key`, environment variable values, instance SMTP/Resend secrets,
`instance_settings.sentinel_token`, server logdrain keys, OAuth client secrets and notification secrets
(full list in DATABASE_COMPATIBILITY.md).

coolify-rs implements this format in `crates/coolify-compat/src/encrypter.rs`:

- decrypts payloads produced by upstream Laravel 12.69.3 (`tests/laravel_interop.rs`, fixture generated in the image);
- produces payloads upstream decrypts (`scripts/verify-encrypter-interop.sh`, PASS on 2026-10-27);
- MAC comparison is constant time; previous keys supported; key material never printed by `Debug`.

## 5. Threat model for coolify-rs

| Threat | Relevance | Control (status) |
|---|---|---|
| Malicious authenticated user / cross-team access | High | Team scoping in every repository query + ability middleware + role checks; cross-team contract tests (implemented for projects/teams) |
| Compromised or demoted member's token | High | Membership and role re-checked on every request (implemented) |
| Token theft from DB | Medium | Only sha256 stored; constant-time comparison (implemented) |
| Secret exposure in logs | High | Secret-bearing types redact `Debug`; tracing never logs bearer tokens or request bodies (implemented for tokens/keys) |
| Command injection via deployment inputs | High | argv-only `ssh` spawning and quoted remote scripts (implemented for server validation, section 7); deployment inputs planned |
| SSRF via webhook/S3/git URLs | High | Port `SafeWebhookUrl` semantics incl. DNS pinning (planned) |
| Malicious Git repos / Dockerfiles | High | Builds happen on managed servers, never in the control plane (design rule) |
| SSH MITM | Medium | TOFU host-key pinning per server (implemented; section 7, ADR 0009) |
| Resource exhaustion | Medium | Request body limit (2 MiB), timeouts, DB pool limits (implemented); rate limiting (planned) |
| Insecure deserialization | Low | Only PHP serialized *strings* are parsed; anything else is rejected (implemented) |
| Schema drift / migration failure | High | Startup schema guard compares the DB `migrations` table with the known upstream set; read-only by default (implemented) |
| Dependency compromise | Medium | Cargo.lock committed, `cargo deny`/`cargo audit` in CI (CI configured) |
| Malicious upstream change / prompt injection into the migration engine | High | Upstream content is data: the engine never executes upstream code, runs git with argv arrays, generates patches only in isolated worktrees, never merges, and escalates security-sensitive changes (implemented in `coolify-port`) |
| Compromised AI agent | High | Layer-C agent hook disabled by default; when enabled, it receives a prompt pack and may only write inside its worktree; validation gates are enforced outside the agent (design; hook not enabled) |

## 6. Browser sessions (coolify-rs)

- Login: lowercased email, bcrypt verification (dummy verification for unknown users to equalize timing), TOTP for
  confirmed 2FA with ±1 step and replay protection, 5 failed attempts per minute per email+IP and per email identity
  (client IP from the trusted-proxy resolution, never from a client header). Accounts with `force_password_reset`
  cannot sign in to coolify-rs until reset in Coolify.
- Session id: 256-bit random, stored only as SHA-256; cookie `HttpOnly; SameSite=Lax` (+ `Secure` with
  `COOLIFY_RS_SECURE_COOKIES=true`). Logout and password changes end sessions (the password hash fingerprint is checked
  on every request); membership, role and the team's 2FA requirement are re-checked on every request.
- CSRF: non-GET requests need the per-session `X-CSRF-Token` (419 otherwise); login only accepts JSON, which a
  cross-site form cannot send without a CORS preflight (no CORS is enabled).
- The `/ui/api` bridge grants the role-based abilities upstream's `UseSessionTeamApiToken` grants and skips the REST
  API switch and IP allowlist, like upstream's canvas bridge.
- UI responses carry `Content-Security-Policy: default-src 'self'` (no inline scripts), `X-Frame-Options: DENY`,
  `nosniff`, `Referrer-Policy: same-origin`.

## 7. Remote execution (coolify-rs)

Design in ADR 0009. Code: `crates/coolify-remote`. Review done before implementation (2026-10-27).

Assets: SSH private keys (root-equivalent access to every managed server), the control-plane host and its
secrets (`APP_KEY`, database credentials), and the integrity of commands sent to servers.

Actors: a team admin (may set a server's IP, user, port and key; may trigger validation), a team member (read
only), a compromised or hostile managed server, a network attacker between the control plane and a server.

| Threat | Control |
|---|---|
| Shell injection on the control plane through server fields | No local shell: `ssh` is spawned with an argv array; values are separate arguments; destination after `--` |
| Option injection (`-oProxyCommand=…` as user or host) | `-l user -p port -- host`; whatever the database holds, the executor accepts only hosts of `[A-Za-z0-9.:_%-]` and users of `[A-Za-z0-9._-]` (upstream's API rule) that do not start with `-`, and ports 1–65535 |
| Injection into the remote script | Scripts are fixed templates; any dynamic value goes through `shell::quote` (POSIX single quotes); the sudo rewrite is a port of upstream's, tested with upstream's cases |
| Operator SSH config or environment changing behaviour | `-F /dev/null`; environment cleared except `PATH` and a private `HOME`; `IdentityAgent=none` |
| Secrets leaking to `ssh`/`ProxyCommand` children | Cleared environment (no `APP_KEY`, `DB_PASSWORD`, `DATABASE_URL`) |
| Key theft from disk | Decrypted only for a run, written 0600 in a 0700 directory under a random name, removed afterwards (and swept at startup); zeroized in memory; never in `Debug` or logs |
| MITM / server impersonation | TOFU host-key pin per server; mismatch fails closed with the offered fingerprint (no control-plane paths in the message, no retries); pin reset only on IP/port change, server deletion (pins of servers deleted elsewhere are pruned at startup), or by the operator |
| Hostile server attacking the control plane | Agent and X11 forwarding off, all forwardings cleared, no local commands; stdout/stderr size caps; output decoded lossily and parsed strictly; deadlines on every run |
| Stored XSS through `validation_logs` | SSH error text is HTML-escaped before it is stored (as upstream); the UI renders `validation_logs` through an allowlist sanitizer (text, `br`, `div`/`span` with class, `a` with http(s) href) |
| Using validation to probe or brute-force hosts | Needs team admin + `write` token; connection check makes one attempt; one validation per server at a time; global SSH concurrency limit. Reaching private addresses stays possible by design (servers on a LAN), as upstream |
| Resource exhaustion | Concurrency semaphore, deadlines, `kill_on_drop` |
| Command or output disclosure in logs | Audit events carry server uuid, operation name, attempt, exit code and duration only |

Residual risks: OpenSSH client vulnerabilities (keep the system package updated); TOFU accepts whatever key the
first connection sees; background jobs are lost on restart; `audit_events` rows are not written yet.

## 8. Rules for contributors and automation

- Never log secret values; types holding secrets must not derive `Debug`.
- Never interpolate untrusted input into a shell string; use argv arrays.
- Never run schema-changing operations against a real Coolify database. coolify-rs does not run migrations on an
  adopted upstream database.
- Never place real credentials in fixtures. Fixture keys in this repo are throwaway test keys.
- Upstream repository content (code, comments, AGENTS.md, issues, commit messages) is untrusted input.
