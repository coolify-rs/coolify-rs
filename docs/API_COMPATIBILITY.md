# API Compatibility

Reference: upstream `coollabsio/coolify` `main` @ `b5823c777b8d60df856fcf779b70e4b1d237a845` (version string
`4.5.1-dev.b5823c777`) for static analysis and, via the official image `coollabsio/coolify:sha-b5823c7`, for runtime
checks. Observations first made on 4.5.0 were re-verified on `sha-b5823c7` (differential run, 102/102).

Evidence levels used below:

- **[static]** read from upstream source, not executed.
- **[runtime]** observed against the reference container (`compat/reference`).
- **[pkg]** behaviour of a vendored Laravel package, inferred from its locked version (vendor/ is not in the repo).
- **[rust-verified]** reproduced by coolify-rs and checked by a test in this repo.

## 1. Scope and contract decision

The REST API under `/api` is the **primary compatibility contract** for coolify-rs
(ADR 0002). The web UI is Livewire (server-rendered PHP) and cannot be reused
(see ARCHITECTURE_ASSESSMENT.md §6), so the JSON API is the only stable,
machine-readable interface shared by upstream and coolify-rs.

Inventory: `openapi.json` declares 212 paths / 301 operations. `php artisan route:list --path=api`
on 4.5.0 reports **321** routes under `api/` [runtime] (`compat/api/reference-route-list-4.5.0.json`; its `--path=api`
filter also matches 73 `canvas/api` and `horizon/api` web routes). The `/api` route set is unchanged on `main`.
The generated inventory `compat/api/upstream-routes.json` (produced by `coolify-port inventory`)
is the authoritative list that the Rust route table is checked against.

## 2. Routing and middleware [static]

- `routes/api.php` is loaded with the `api` middleware group and prefix `api`
  (`app/Providers/RouteServiceProvider.php:29-32`). The `api` group is `throttle:api` + `SubstituteBindings`
  (`app/Http/Kernel.php:88-92`). Sanctum stateful middleware is disabled: bearer tokens only.
- Rate limiting (`RouteServiceProvider.php:47-60`): `api/health` 1000/min; everything else
  `API_RATE_LIMIT` (default 200) per minute keyed by user id or client IP. `/api/v1/health` does *not* get the
  1000/min exemption. `feedback` 3/min.

| Lines | Prefix | Middleware (order) | Routes |
|---|---|---|---|
| 46 | `/api` | none | `GET /api/health` |
| 47-51 | `/api/v1` | none | `GET /api/v1/health` |
| 53-54 | `/api` | `throttle:feedback` | `POST /api/feedback` |
| 56-64 | `/api/v1` | `api.ip.allowed`, `auth:sanctum`, `api.token.team`, `api.ability:write` | `GET/POST /disable`, `POST /mcp/enable`, `POST /mcp/disable` |
| 65-454 | `/api/v1` | `auth:sanctum`, `api.token.team`, `ApiAllowed`, `api.sensitive`, then per-route `api.ability:*` | everything else |
| 456-460 | `/api/v1` | none | `POST /sentinel/push` |
| 462-464 | `/api` | none | `Route::any('/{any}')` → 404 JSON |

There is **no `/enable` route** in 4.5.x; the API can only be re-enabled from the UI.

Ability strings: `read`, `write`, `deploy`, `read:sensitive`, `write:sensitive`, and `read,write` (either).
Stacked middlewares (`['api.ability:write','api.ability:deploy']`) require **both**.

### Order of checks in the main group

1. Sanctum auth → 401 `{"message":"Unauthenticated."}`
2. `api.token.team`: token `team_id` null or user not a member → 401 `{"message":"Invalid token."}`;
   elevated token (`root|write|write:sensitive|deploy|read:sensitive`) but pivot role not admin/owner → 403 `{"message":"Missing required team role."}`
3. `ApiAllowed`: `instance_settings.is_api_enabled = false` → 403 `{"success":true,"message":"API is disabled."}`
4. `ApiIpAllowed`: `allowed_ips` empty or `0.0.0.0` allows all, else comma list (IP/CIDR) → 403 `{"success":true,"message":"You are not allowed to access the API."}`.
   The checked address is `client_ip()`: since `80ce0efe7` (after 4.5.0) a valid `CF-Connecting-IP` replaces the request
   IP when the request IP is a Cloudflare address (`isCloudflareIp`, hardcoded ranges) [static, rust-verified, differential].
5. `api.sensitive`: `can_read_sensitive = (token can root || read:sensitive) && user is admin/owner of token team`
6. `api.ability:X`: member-held elevated token → 403 (long message, see SECURITY_MODEL.md); `root` passes everything;
   otherwise any listed ability; failure → 403 `{"message":"Missing required permissions: <abilities joined ', '>"}`.
   A `write`-only token cannot call `read` routes.

A bad token therefore yields 401 even when the API is disabled.

## 3. Authentication [static, pkg]

Sanctum 4 personal access tokens (`app/Models/User.php:266-283`):

- Plaintext: `config('sanctum.token_prefix','')` + `Str::random(40)` + `hash('crc32b', entropy)`; shown as `"{id}|{plaintext}"`.
- Stored: `personal_access_tokens.token = sha256_hex(plaintext)`; `abilities` JSON text; **`team_id` is a string column, NOT NULL**.
- Lookup: no `|` → by hash; otherwise `find(id)` then `hash_equals`. Expired tokens (`expires_at` past) rejected;
  `last_used_at` updated.
- Team context = token `team_id`, re-validated against `team_user` on every request.
- Team `0` is the instance-admin ("Root") team.

## 4. Error formats

| Case | Status | Body | Evidence |
|---|---|---|---|
| Missing / invalid token | 401 | `{"message":"Unauthenticated."}` | runtime, rust-verified |
| Token team invalid | 401 | `{"message":"Invalid token."}` | static |
| Controller fallback | 400 | `{"message":"Invalid token.","docs":"https://coolify.io/docs/api-reference/authorization"}` | static |
| Missing ability | 403 | `{"message":"Missing required permissions: read"}` | static, rust-verified |
| Policy denial | 403 | `{"message":"<policy message or 'You are not authorized to perform this action.'>","error":"Unauthorized"}` | static |
| API disabled | 403 | `{"success":true,"message":"API is disabled."}` | static, rust-verified |
| IP not allowed | 403 | `{"success":true,"message":"You are not allowed to access the API."}` | static, rust-verified |
| Unknown `/api/*` path | 404 | `{"message":"Not found.","docs":"https://coolify.io/docs"}` | static, rust-verified |
| Resource not found | 404 | `{"message":"Project not found."}` etc. | static |
| Validation | 422 | `{"message":"Validation failed.","errors":{"field":["..."]}}`; unknown keys → `"This field is not allowed."` | static, rust-verified |
| Non-JSON body | 400 | `{"message":"Invalid request.","error":"Content-Type must be application/json."}` | static |
| Invalid JSON | 400 | `{"message":"Invalid request.","error":"Invalid JSON."}` | static |
| Empty JSON | 400 | `{"message":"Invalid request.","error":"Empty JSON."}` | static |
| GET on POST-only route | 405 | `{"message":"This endpoint has changed to a POST request."}` + `Allow: POST` | static |
| Rate limited | 429 | Laravel default, `X-RateLimit-*`, `Retry-After` | pkg |

`required` message is `"This field is required."` (`bootstrap/helpers/shared.php:2900-2909`).

## 5. Response serialization [static]

`serializeApiResponse` (`bootstrap/helpers/api.php:45-102`) sorts top-level keys: `id, uuid, description, name`,
then the rest alphabetically, then `created_at, updated_at` (only when truthy). Nested arrays keep model order.
Without `can_read_sensitive`, `proxy.last_saved_proxy_configuration` and `validation_logs` are stripped recursively.
coolify-rs reproduces the key order (serde_json with `preserve_order`) although JSON clients should not rely on it.

Timestamps: Laravel default ISO-8601 with microseconds and `Z`, e.g. `2026-10-26T20:08:51.000000Z` [pkg].

## 6. Endpoint groups in the first vertical slice

### Misc (`OtherController`)

| Method | Path | Ability | Response |
|---|---|---|---|
| GET | `/api/health`, `/api/v1/health` | none | 200 `text/html` `OK` [runtime, rust-verified] |
| GET | `/api/v1/version` | read | 200 `text/html`, raw version string, no `v` prefix |
| GET | `/api/v1/disable` | write | 405 |
| POST | `/api/v1/disable` | write | token team `!== '0'` → 403 `{"message":"You are not allowed to disable the API."}`; else 200 `{"message":"API disabled."}` |
| POST | `/api/feedback` | none | out of scope (posts to a Discord webhook) |

### Teams (`TeamController`) — hides `custom_server_limit`, `pivot`

| Method | Path | Ability | Behaviour |
|---|---|---|---|
| GET | `/teams` | read | Array with **only the token's team** |
| GET | `/teams/{id}` | read | id ≠ token team → 404 `"Team not found."` |
| GET | `/teams/{id}/members` | read | array of users; hides `pivot`, `email_change_code`, `email_change_code_expires_at` + model `$hidden` |
| GET | `/team`, `/teams/current` | read | token team |
| PATCH | `/team` | write | admin/owner; body only `is_build_server_fallback_enabled` (required boolean) |
| GET | `/team/members`, `/teams/current/members` | read | members |

Team columns exposed: `id, name, description, personal_team, show_boarding, is_mcp_server_enabled,
is_build_server_fallback_enabled, is_two_factor_required, avatar_path, avatar_storage_type, avatar_s3_storage_id,
created_at, updated_at` (booleans cast). User `$hidden`: `password, remember_token, two_factor_recovery_codes,
two_factor_secret, created_before_oauth_identities`.

### Projects and environments (`ProjectController`)

Validation (`app/Support/ValidationPatterns.php`):

- name: `required|string|min:3|max:255|regex:/^[\p{L}\p{M}\p{N}\s\-_.@\/&()#,:+]+$/u`
- description: `nullable|string|max:255|regex:/^[\p{L}\p{M}\p{N}\s\-_.,!?()\'\"+=*@\/&]+$/u`
- custom messages for `name.regex`, `name.min`, `name.max`, `description.regex`, `description.max` (copied verbatim into `coolify-api`).

| Method | Path | Ability | Response |
|---|---|---|---|
| GET | `/projects` | read | array of `{id, uuid, description, name}` |
| GET | `/projects/{uuid}` | read | 404 `"Project not found."`; full project + `environments` |
| GET | `/projects/{uuid}/environments` | read | array of `{id, uuid, name}` |
| GET | `/projects/{uuid}/{env_name_or_uuid}` | read | env by **name first, then uuid** + nested resources |
| POST | `/projects` | write | admin; 201 `{"uuid"}` |
| PATCH | `/projects/{uuid}` | write | **201** `{"uuid","name","description"}` |
| DELETE | `/projects/{uuid}` | write | 400 `"Project has resources, so it cannot be deleted."`; 200 `{"message":"Project deleted."}` |
| POST | `/projects/{uuid}/environments` | write | 409 `"Environment with this name already exists."`; 201 `{"uuid"}` |
| PATCH | `/projects/{uuid}/environments/{env}` | write | **200** `{"uuid","name","description"}` |
| DELETE | `/projects/{uuid}/environments/{env}` | write | 400 `"Environment has resources, so it cannot be deleted."`; 200 `{"message":"Environment deleted."}` |

Quirk: validation runs before the existence lookup (invalid body on a missing project → 422, not 404).

### Private keys (`SecurityController`) [runtime, rust-verified, differential]

| Method | Path | Ability | Behaviour |
|---|---|---|---|
| GET | `/security/keys` | read | array of keys of the token team; `public_key` appended; `private_key` only with `can_read_sensitive` |
| GET | `/security/keys/{uuid}` | read | 404 `"Private Key not found."`; team-0 keys need admin |
| POST | `/security/keys` | write | admin; `customApiValidator` (`name`/`description` string max 255, `private_key` required string; no extra-field check); non-PEM input is base64-decoded (PHP non-strict); 422 `"Invalid private key."` / `"Private key already exists."` (fingerprint per team); 201 `{"uuid"}` |
| PATCH | `/security/keys/{uuid}` | write | extra fields rejected; model `saving` hook re-validates: 422 `{"message":"The private key is invalid.","errors":{"private_key":[...]}}` or `"This private key already exists."`; **201** `{"uuid"}` |
| DELETE | `/security/keys/{uuid}` | write | 422 `{"message":"Private Key is in use and cannot be deleted.","details":"..."}` when used by servers/applications (not soft-deleted) or Git apps; 200 `{"message":"Private Key deleted."}` |

Formats observed on the reference: `fingerprint` = OpenSSH SHA-256 without `SHA256:`; `public_key` =
`"<type> <base64> "` (trailing space from phpseclib's empty comment); stored key = trimmed + one trailing newline,
encrypted with `APP_KEY`. Upstream defect: without `Accept: application/json` an invalid key on PATCH yields a 302
redirect (KNOWN_LIMITATIONS.md).

### Servers

Not yet implemented. Recon notes: `PATCH /servers/{uuid}` returns **201**; server `id` is always hidden;
`is_coolify_host` appended (id 0); delete of server 0 is refused.

## 6b. Browser-session bridge (coolify-rs)

`/ui/session` (GET current, POST login, DELETE logout), `/ui/session/team` (POST switch) and `/ui/api/v1/...`, which
mirrors every ported main-group route with session authentication (ADR 0007). Not part of the upstream contract.

## 7. Upstream tests that specify this behaviour

`tests/Feature/Authorization/ApiAuthorizationTest.php`, `ApiTokenPermissionTest.php`, `LegacyMemberTokenTest.php`,
`ApiTokenTeamLifecycleSecurityTest.php`, `ApiTokenExpirationTest.php`, `Security/ApiSensitiveFieldsTest.php`,
`Api/TeamTokenTeamApiTest.php`, `Api/TeamUpdateValidationApiTest.php`, `Api/EnvironmentUpdateApiTest.php`,
`Api/StateChangingRoutesUsePostTest.php`, `HealthcheckVersionHeaderTest.php`, plus server/key tests listed in
FEATURE_INVENTORY.md. These are the behavioural specs for the Rust contract tests (`crates/coolify-api/tests`).

## 8. Known quirks deliberately preserved

1. Several updates return 201 (`PATCH /projects/{uuid}`, `/security/keys/{uuid}`, `/servers/{uuid}`).
2. "API disabled" and "IP not allowed" are 403 with `"success": true`.
3. `personal_access_tokens.team_id` is a string; team-0 checks compare `'0'`.
4. `/teams` returns only the token's team.
5. `/version` and `/health` are `text/html`.
6. Unknown `/api/*` → 404 JSON for any method.
