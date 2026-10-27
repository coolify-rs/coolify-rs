# Applications API + deployment queue: upstream spec (Docker-image path)

Source: `.upstream/snapshot` (Coolify main @ b5823c7, Laravel 12.70.1). All paths below are relative to it.
Schema facts come from `compat/baseline/upstream-main-b5823c7-schema.sql` (the `database/schema/testing-schema.sql`
in upstream is stale: it lacks `docker_registry_image_tag`, `configuration_*`, `parent_deployment_uuid`).

Strings are verbatim. "→" means "in this order". `R` = needed for the Docker-image MVP.

---

## 0. Cross-cutting behaviour that affects every byte

### 0.1 Middleware stack for every route below

Global (`app/Http/Kernel.php:47-56`), in order: `AddServerTimingHeaders`, `TrustProxies`, `HandleCors`,
`PreventRequestsDuringMaintenance`, `ValidatePostSize`, `TrimStrings`, `ConvertEmptyStringsToNull`, `InvokeDeferredCallbacks`.

- **`TrimStrings` + `ConvertEmptyStringsToNull` also rewrite JSON bodies and query strings**: every string value is
  trimmed (except keys `current_password`, `password`, `password_confirmation`, `app/Http/Middleware/TrimStrings.php`),
  then `""` becomes `null`. So `"domains": ""` arrives as `"domains": null` (key still present → `has('domains')` true).

Route group `api` (`Kernel.php:73-76`, `RouteServiceProvider.php:30-32`): `ThrottleRequests:api` (limit
`config('api.rate_limit')` = env `API_RATE_LIMIT`, default `200`/min, keyed by user id or client IP,
`RouteServiceProvider.php:47-53`; responses carry `X-RateLimit-Limit` / `X-RateLimit-Remaining`), `SubstituteBindings`.
Prefix `/api`.

Route group in `routes/api.php:65-68`: prefix `v1`, middleware → `auth:sanctum` → `api.token.team` →
`ApiAllowed` → `api.sensitive`, then per-route `api.ability:<x>` (each `api.ability:` entry is a separate middleware
instance, run in listed order).

| Middleware | File | Failure response |
|---|---|---|
| `auth:sanctum` | Handler `unauthenticated()` `app/Exceptions/Handler.php:47-61` | `401 {"message":"Unauthenticated."}` (audit `api.auth.unauthenticated`) |
| `api.token.team` | `app/Http/Middleware/EnsureTokenBelongsToCurrentTeamMember.php` | no user/token/`team_id` null or user not in team → `401 {"message":"Invalid token."}`; token has any of `root, write, write:sensitive, deploy, read:sensitive` and pivot role not `admin`/`owner` → `403 {"message":"Missing required team role."}` |
| `ApiAllowed` | `app/Http/Middleware/ApiAllowed.php` | self-hosted and `instance_settings.is_api_enabled === false` → `403 {"success":true,"message":"API is disabled."}`; then `ApiIpAllowed`: `allowed_ips` not empty/`0.0.0.0` and client IP not in list → `403 {"success":true,"message":"You are not allowed to access the API."}` |
| `api.sensitive` | `app/Http/Middleware/ApiSensitiveData.php` | never fails; sets request attribute `can_read_sensitive = (token can root OR read:sensitive) AND user isAdminOfTeam(token.team_id)` |
| `api.ability:X` | `app/Http/Middleware/ApiAbility.php` | non-admin member holding any elevated ability → `403 {"message":"This API token has permissions (<comma+space list>) that exceed your current role as a team member. Members are restricted to read-only API access. Please revoke this token and create a new one with only read permissions."}`; token `root` → pass; else Sanctum any-ability check; missing → `403 {"message":"Missing required permissions: X"}` (audit `api.auth.ability_denied`) |

### 0.2 JSON encoding
`response()->json()` uses `json_encode` with flags `0`: **`/` is escaped as `\/`** and non-ASCII as `\uXXXX`.
Dates serialize as `YYYY-MM-DDTHH:MM:SS.000000Z` (UTC, `config/app.php:127`; columns are `timestamp(0)` so micros are `000000`).

### 0.3 `getTeamIdFromToken()` / `invalidTokenResponse()` (`bootstrap/helpers/api.php:28-43`)
Returns token `team_id` if the user is in that team, else null. Null → `400 {"message":"Invalid token.","docs":"https://coolify.io/docs/api-reference/authorization"}`.
(Practically unreachable after `api.token.team`, but every controller method starts with it.)

### 0.4 Policy failures (`Handler.php:66-103`)
`$this->authorize()` denial without explicit status, on `api/*` → audit `api.auth.policy_denied` →
`403 {"message":<policy message, strip_tags, or "You are not authorized to perform this action." when empty or "This action is unauthorized.">,"error":"Unauthorized"}`.
`ApplicationPolicy` (`app/Policies/ApplicationPolicy.php`): `create` = `user->isAdmin()` (role in current team; for API
the token team, `User.php:387-411,461-475`); `view` = user in app's team (bool → default message); `update` returns
`Response::deny('You need at least admin or owner permissions to update this application.')` (or
`'Application team not found.'`); `delete`, `deploy`, `manageEnvironment` = isAdminOfTeam (bool).

### 0.5 `abort_unless(..., 403, 'Missing required permissions: deploy')`
Rendered by Laravel's default handler: JSON `{"message":"Missing required permissions: deploy"}` **only when the
request `expectsJson()`** (Accept contains json); otherwise Laravel's HTML 403 page. Same for any 500.

### 0.6 `serializeApiResponse($data)` (`bootstrap/helpers/api.php:45-104`)
For a non-Eloquent-Collection value: `collect($data)` (Model → `toArray()`) → `sortKeys()` (PHP `ksort` SORT_REGULAR:
byte-wise for non-numeric keys; only **top level**, nested arrays keep their order) → if `created_at` truthy move it
to the end → same for `updated_at` → if `name` truthy prepend `name` → if `description` truthy prepend `description`
→ if `uuid` truthy prepend `uuid` → if `id` not null prepend `id`. Result order: `id, uuid, description, name, <sorted rest>, created_at, updated_at`.
Then `removeServerProxySecretsFromApiPayload()` (`api.php:130-165`): unless `can_read_sensitive`, recursively for
every array/collection having an array `proxy`: unset `proxy.last_saved_proxy_configuration` and unset sibling `validation_logs`.
(Eloquent Collection input maps each item the same way; `apiEnvironmentVariableDisplayValue` only matters for env vars.)

### 0.7 Validation error envelope
`{"message":"Validation failed.","errors":<obj>}` status 422. `<obj>` is either a Laravel MessageBag (`{"field":["msg",...]}`)
or a hand-written map whose values may be **plain strings** (not arrays) — noted per case below.
`customApiValidator()` (`bootstrap/helpers/shared.php:2900-2909`) = `Validator::make` with message override
`'required' => 'This field is required.'`; plain `Validator::make` uses Laravel 12 default English messages
(e.g. `The :attribute field must be a string.`, `...must be true or false.`, `...format is invalid.`,
`...must not be greater than :max characters.`, `The selected :attribute is invalid.`; `:attribute` = key with `_`→space).
Errors appear in rule-array order; `array_merge(shared, specific)` keeps the **shared array's key position** for overridden keys.

---

## 1. Routes (`routes/api.php`)

All under `/api/v1`, group middleware from 0.1.

### 1.1 Deploy / deployments (`routes/api.php:151-156`)
| R | Method | Path | Controller | Abilities |
|---|---|---|---|---|
| R | GET | `/deploy` | `OtherController@post_required` | deploy |
| R | POST | `/deploy` | `DeployController@deploy` | deploy |
| R | GET | `/deployments` | `DeployController@deployments` | read |
| R | GET | `/deployments/{uuid}` | `DeployController@deployment_by_uuid` | read |
| R | POST | `/deployments/{uuid}/cancel` | `DeployController@cancel_deployment` | deploy |
| R | GET | `/deployments/applications/{uuid}` | `DeployController@get_application_deployments` | read |

`post_required` (`app/Http/Controllers/Api/OtherController.php:14-19`): `405 {"message":"This endpoint has changed to a POST request."}` + header `Allow: POST`.
**So `GET /deploy?uuid=` never deploys** — it returns that 405 (after auth + `deploy` ability).

### 1.2 Applications (`routes/api.php:248-304`, `441-443`)
| R | Method | Path | Controller method | Abilities |
|---|---|---|---|---|
| R | GET | `/applications` | `applications` | read |
| | POST | `/applications/public` | `create_public_application` | write |
| | POST | `/applications/private-github-app` | `create_private_gh_app_application` | write |
| | POST | `/applications/private-deploy-key` | `create_private_deploy_key_application` | write |
| | POST | `/applications/dockerfile` | `create_dockerfile_application` | write |
| R | POST | `/applications/dockerimage` | `create_dockerimage_application` | write |
| R | GET | `/applications/{uuid}` | `application_by_uuid` | read |
| R | PATCH | `/applications/{uuid}` | `update_by_uuid` | write |
| R | DELETE | `/applications/{uuid}` | `delete_by_uuid` | write, deploy |
| | PATCH | `/applications/{uuid}/secret-manager` | `ApplicationSecretManagerController@update` | write |
| | GET/POST | `/applications/{uuid}/envs` | `envs` / `create_env` | read / write |
| | PATCH | `/applications/{uuid}/envs/bulk` | `create_bulk_envs` | write |
| | PATCH | `/applications/{uuid}/envs` | `update_env_by_uuid` | write |
| | DELETE | `/applications/{uuid}/envs/{env_uuid}` | `delete_env_by_uuid` | write |
| | GET | `/applications/{uuid}/logs` | `logs_by_uuid` | read:sensitive |
| | GET | `/applications/{uuid}/previews/{pull_request_id}/logs` | `logs_by_uuid` | read:sensitive |
| | GET/POST/PATCH | `/applications/{uuid}/storages` | `storages` / `create_storage` / `update_storage` | read / write / write |
| | DELETE | `/applications/{uuid}/storages/{storage_uuid}` | `delete_storage` | write |
| | PUT | `/applications/{uuid}/storages/{storage_uuid}/backups` | `VolumeBackupsController@upsert` (defaults `resource_type=application`) | write |
| | POST | `/applications/{uuid}/storages/{storage_uuid}/backups/run` | `VolumeBackupsController@run` | write, deploy |
| | DELETE | `/applications/{uuid}/storages/{storage_uuid}/backups` | `VolumeBackupsController@destroy` | write |
| | GET/POST | `/applications/{uuid}/tags` | `tags` / `create_tag` | read / write |
| | DELETE | `/applications/{uuid}/tags/{tag_uuid}` | `delete_tag` | write |
| | POST | `/applications/{uuid}/move` / `migrate` / `clone` | `move_by_uuid` / `migrate_by_uuid` / `clone_by_uuid` | write |
| | GET | `/applications/{uuid}/rollback-images` | `rollback_images` | read |
| | POST | `/applications/{uuid}/rollback` | `rollback_by_uuid` | deploy |
| | GET/POST | `/applications/{uuid}/destinations` | `destinations` / `add_destination` | read / write |
| | DELETE | `/applications/{uuid}/destinations/{destination_uuid}` | `remove_destination` | write |
| R | GET | `/applications/{uuid}/start` `/restart` `/stop` | `OtherController@post_required` (405) | deploy |
| R | POST | `/applications/{uuid}/start` | `action_deploy` | deploy |
| R | POST | `/applications/{uuid}/restart` | `action_restart` | deploy |
| R | POST | `/applications/{uuid}/stop` | `action_stop` | deploy |
| | GET/POST | `/applications/{uuid}/previews` | `ApplicationPreviewsController@index` / `store` | read / deploy |
| | GET | `/applications/{uuid}/previews/{pull_request_id}` | `ApplicationPreviewsController@show` | read |
| | PATCH | `/applications/{uuid}/previews/{pull_request_id}` | `update_preview_by_pull_request_id` | write |
| | DELETE | `/applications/{uuid}/previews/{pull_request_id}` | `delete_preview_by_pull_request_id` | write, deploy |
| | GET/POST | `/applications/{uuid}/scheduled-tasks` | `ScheduledTasksController@...` | read / write |
| | PATCH | `/applications/{uuid}/scheduled-tasks/{task_uuid}` | `ScheduledTasksController@...` | write |

(Also `/services/{uuid}/applications*` exist — service apps, out of scope.)

---

## 2. `POST /applications/dockerimage`

`create_dockerimage_application` (`ApplicationsController.php:1347-1350`) = `create_application($request, 'dockerimage')`
(`:1352-2535`). Shared prologue `:1352-1542`, docker-image branch `:2396-2532`.

### 2.1 Order of operations (every early return)
1. `getTeamIdFromToken()` null → 400 (0.3).
2. `$this->authorize('create', Application::class)` → 403 per 0.4 (`"You are not authorized to perform this action."`, `"error":"Unauthorized"`).
3. `$forceDomainOverride = $request->boolean('force_domain_override')`.
4. If `$request->instant_deploy` truthy and token lacks `deploy` and `root` → `abort 403 'Missing required permissions: deploy'` (0.5).
5. `validateIncomingRequest()` (`api.php:434-457`), each `400`:
   - not JSON content type → `{"message":"Invalid request.","error":"Content-Type must be application/json."}`
   - `!json_decode(body)` (invalid, or falsy: `[]`, `0`, `""`, `false`, `null`) → `{"message":"Invalid request.","error":"Invalid JSON."}`
   - `empty($request->json()->all())` (e.g. `{}`) → `{"message":"Invalid request.","error":"Empty JSON."}`
6. Base validation (`customApiValidator`, `:1374-1390`):
   ```
   'name' => 'string|max:255',
   'description' => 'string|nullable',
   'project_uuid' => 'string|required',
   'environment_name' => 'string|nullable',
   'environment_uuid' => 'string|nullable',
   'server_uuid' => 'string|required',
   'destination_uuid' => 'string',
   'is_http_basic_auth_enabled' => 'boolean',
   'http_basic_auth_username' => 'string|nullable',
   'http_basic_auth_password' => 'string|nullable|max:72',
   'autogenerate_domain' => 'boolean',
   'tags' => 'array|nullable',
   'tags.*' => 'string|min:2',
   ```
   plus extra-field check: `array_diff(keys(request.all()), $allowedFields)`; each extra key gets message
   `"Use the domains field to set application URLs."` if key is `fqdn`, else `"This field is not allowed."`
   (`:263-266`), appended to the same MessageBag after validator errors. Any failure → 422 (0.7, values are arrays).
   `$allowedFields` (`:1372`), verbatim order:
   `project_uuid, environment_name, environment_uuid, server_uuid, destination_uuid, type, name, description, is_static, is_spa, is_auto_deploy_enabled, is_force_https_enabled, is_preview_deployments_enabled, domains, noindex_domains, git_repository, git_branch, git_commit_sha, private_key_uuid, docker_registry_image_name, docker_registry_image_tag, build_pack, install_command, build_command, start_command, ports_exposes, ports_mappings, custom_network_aliases, base_directory, publish_directory, health_check_enabled, health_check_type, health_check_command, health_check_path, health_check_port, health_check_host, health_check_method, health_check_return_code, health_check_scheme, health_check_response_text, health_check_interval, health_check_timeout, health_check_retries, health_check_start_period, limits_memory, limits_memory_swap, limits_memory_swappiness, limits_memory_reservation, limits_cpus, limits_cpuset, limits_cpu_shares, custom_labels, custom_docker_run_options, post_deployment_command, post_deployment_command_container, pre_deployment_command, pre_deployment_command_container, manual_webhook_secret_github, manual_webhook_secret_gitlab, manual_webhook_secret_bitbucket, manual_webhook_secret_gitea, redirect, github_app_uuid, instant_deploy, dockerfile, dockerfile_location, dockerfile_target_build, docker_compose_location, docker_compose_raw, docker_compose_custom_start_command, docker_compose_custom_build_command, docker_compose_domains, watch_paths, use_build_server, use_build_secrets, static_image, custom_nginx_configuration, is_http_basic_auth_enabled, http_basic_auth_username, http_basic_auth_password, http_basic_auth_hash_algorithm, http_basic_auth_bcrypt_cost, http_basic_auth_argon2id_memory_cost, http_basic_auth_argon2id_time_cost, connect_to_docker_network, force_domain_override, autogenerate_domain, is_container_label_escape_enabled, tags, is_preserve_repository_enabled, preview_url_template, max_restart_count`
   + `APPLICATION_SETTING_FIELDS` (`:48-77`): `is_git_submodules_enabled, is_git_lfs_enabled, is_git_shallow_clone_enabled, disable_build_cache, inject_build_args_to_dockerfile, include_source_commit_in_build, is_env_sorting_enabled, redact_all_env_values_in_logs, include_coolify_default_variables, is_pr_deployments_public_enabled, stop_grace_period, rolling_update_drain_seconds, docker_images_to_keep, is_gzip_enabled, is_stripprefix_enabled, is_permanent_redirect_enabled, traefik_certificate_resolver, is_container_label_readonly_enabled, is_raw_compose_deployment_enabled, is_log_drain_enabled, is_gpu_enabled, gpu_driver, gpu_count, gpu_device_ids, gpu_options, is_consistent_container_name_enabled, custom_internal_name, custom_container_name_prefix`.
   Note: `fqdn`, `uuid`, `status` etc. are **not** allowed.
7. `docker_compose_raw` filled (it is an allowed field even here): non-string → error `'The docker_compose_raw field must be a string.'`;
   else `validateDockerComposeForInjection()` (`bootstrap/helpers/parsers.php:44`) exception message.
   → `422 {"message":"Validation failed.","errors":{"docker_compose_raw":"<msg>"}}` (string value).
8. `validateTagsParameter()` (`Concerns/HandlesTagsApi.php:146-164`): if `tags` key present, normalize each
   `strtolower(trim(strip_tags((string)$t)))`, unique; any `mb_strlen < 2` →
   `422 {"message":"Validation failed.","errors":{"tags":["Each tag name must be at least 2 characters after sanitization."]}}`; else merge normalized back.
9. `$tagNames = input('tags') ?? []`.
10. `blank(environment_uuid) && blank(environment_name)` → `422 {"message":"You need to provide at least one of environment_name or environment_uuid."}` (no `errors`).
11. `$fqdn = $request->domains`; if `has('domains')` and string → `$fqdn = ValidationPatterns::normalizeApplicationDomains(domains)` (2.4) and written back to request.
12. `$autogenerateDomain = boolean('autogenerate_domain', true) && build_pack !== 'dockercompose'` (request `build_pack`; for dockerimage normally absent → true).
13. Capture raw (un-cast) request values: `instant_deploy, github_app_uuid, use_build_server, use_build_secrets, is_static, is_spa, is_auto_deploy_enabled, is_force_https_enabled, is_preview_deployments_enabled, connect_to_docker_network, custom_nginx_configuration`;
    `$isContainerLabelEscapeEnabled = boolean('is_container_label_escape_enabled', true)`; `$isPreserveRepositoryEnabled = boolean(..., false)` (unused in dockerimage branch);
    `$applicationSettings = applicationSettingsFromRequest()` (`:179-203`): for each `APPLICATION_SETTING_FIELDS` key **present** in request: boolean fields via `$request->boolean()`, others raw; `custom_container_name_prefix` (if scalar or null) → `Str::slug()` or null if empty; if `custom_internal_name` filled and `is_consistent_container_name_enabled` not sent → set it `true`.
14. `$requestedBuildPack = 'dockerimage'`; `is_raw_compose_deployment_enabled` truthy → `422 {"message":"Validation failed.","errors":{"is_raw_compose_deployment_enabled":"Raw compose deployment can only be enabled for Docker Compose applications."}}`.
15. `custom_nginx_configuration` not null: not base64 (`base64_encode(base64_decode($v,true)) === $v`) or decoded not UTF-8 → `422 {"message":"Validation failed.","errors":{"custom_nginx_configuration":"The custom_nginx_configuration should be base64 encoded."}}`; else request value replaced by the decoded text.
16. `Project::whereTeamId(team)->whereUuid(project_uuid)` missing → `404 {"message":"Project not found."}`.
17. Environment: first `where('name', environment_name)`, then `where('uuid', environment_uuid)` → missing `404 {"message":"Environment not found."}`.
18. `Server::whereTeamId(team)->whereUuid(server_uuid)` missing → `404 {"message":"Server not found."}`.
19. `!server->canHostResources()` (`Server.php:682-685`, server role can deploy) → `422 {"message":"Validation failed.","errors":{"server_uuid":["The specified server is configured as a build server and cannot host resources."]}}`.
20. Destinations = standalone_dockers of server concat swarm_dockers (`Server.php:2022-2028`):
    0 → `400 {"message":"Server has no destinations."}`; >1 and no `destination_uuid` key → `400 {"message":"Server has multiple destinations and you do not set destination_uuid."}`;
    default first; if >1 and key given → match by uuid else `422 {"message":"Validation failed.","errors":{"destination_uuid":"Provided destination_uuid does not belong to the specified server."}}`.
    (With exactly one destination, a given `destination_uuid` is ignored.)
21. `containerNamePrefixValidationResponse` (`:205-225`): non-string non-null prefix → `422 ... {"custom_container_name_prefix":["The custom container name prefix field must be a string."]}`; prefix in use by another app on the server (equals an app's uuid / custom_container_name_prefix / custom_internal_name, `ApplicationSetting.php` `isContainerNamePrefixInUse`) → `422 ... {"custom_container_name_prefix":["This container name prefix is already in use by another application."]}`.
22. `customInternalNameValidationResponse` (`:227-237`): `custom_internal_name` filled and `is_consistent_container_name_enabled` explicitly false → `422 ... {"custom_internal_name":["Set is_consistent_container_name_enabled to true to use custom_internal_name. Coolify ignores the custom internal name while consistent container naming is turned off."]}`.
23. `manualWebhookSecretForbiddenResponse` (`:168-177`): any `manual_webhook_secret_{github,gitlab,bitbucket,gitea}` filled (on create there is no stored value, so any filled value counts; on update only a value differing from the stored one) without deploy/root → `403 {"message":"Missing required permissions: deploy. A manual webhook secret lets anyone who knows it start a deployment, so it needs a token with the deploy permission."}` (`Concerns/RequiresDeployAbility.php`).
24. Docker-image validation (`:2397-2410`, `customApiValidator`): rules = `array_merge(sharedDataApplications(), [...])` where
    ```
    'docker_registry_image_name' => ['required', 'string', 'max:255', new DockerImageFormat],
    'docker_registry_image_tag'  => ValidationPatterns::dockerImageTagRules(),   // ['nullable','string','max:128','regex:'.DOCKER_IMAGE_TAG_PATTERN]
    'ports_exposes'              => 'string|regex:/^(\d+)(,\d+)*$/|nullable',
    ```
    `sharedDataApplications()` = `bootstrap/helpers/api.php:187-300` (full array, verbatim there; key rules relevant to docker image:
    `domains` = `applicationDomainRules()` (2.4), `noindex_domains` `array|nullable`, `noindex_domains.*` `string`, `redirect` `Rule::enum(RedirectTypes)`
    (`www|non-www|both`), `git_commit_sha` `['string','regex:/^[a-zA-Z0-9][a-zA-Z0-9._\-\/]*$/']`, `ports_mappings` `string|regex:/^(\d+:\d+)(,\d+:\d+)*$/|nullable`,
    `health_check_*` (`health_check_port` `integer|nullable|min:1|max:65535`; `health_check_path` `['string','regex:#^[a-zA-Z0-9/\-_.~%,;]+$#']`; `health_check_method` `in:GET,HEAD,POST,OPTIONS`;
    `health_check_scheme` `in:http,https`; `health_check_type` `in:http,cmd`; `health_check_command` nullable max:1000 regex `/^[a-zA-Z0-9 \-_.\/:=@,+]+$/`; `health_check_host` closure message
    `The health check host must be a hostname or an IP address.`; return_code/interval/timeout/retries/start_period `numeric`), `limits_*` (`string`/`numeric`, `limits_cpuset` `string|nullable`),
    `custom_labels` `string|nullable`, `custom_docker_run_options` shellSafe(2000), `pre/post_deployment_command` `string|nullable`, `*_container` = `containerNameRules()` (`['string','max:255','regex:/\A[a-zA-Z0-9][a-zA-Z0-9._-]*\z/']`),
    `stop_grace_period` `nullable|integer|min:1|max:3600`, `rolling_update_drain_seconds` `integer|min:0|max:300`, `docker_images_to_keep` `integer|min:0|max:100`, `max_restart_count` `integer|min:0`,
    `traefik_certificate_resolver` `['nullable','string','max:255','regex:/^[A-Za-z0-9_-]+$/']`, `gpu_count` regex `/^(all|\d+)$/`, `gpu_options` closure `The gpu_options must be key=value pairs or a YAML mapping.`,
    `custom_container_name_prefix` `string|nullable|max:30`, `manual_webhook_secret_*` `['nullable','string', new ManualWebhookSecret(null)]` (min 16 chars: `The :attribute must be at least 16 characters.` / `The :attribute must be a string.`),
    `http_basic_auth_hash_algorithm` enum `bcrypt|argon2id`, bcrypt cost `integer|min:4|max:14`, argon2id memory `integer|min:8192|max:262144`, time `integer|min:1|max:12`).
    Fail → 422 MessageBag. Note the override keeps the shared position (shared order: `...git_commit_sha, docker_registry_image_name, docker_registry_image_tag, install_command, ..., ports_exposes, ports_mappings...`).
25. If no `name` key → `name = 'docker-image-'.new_public_id()` (`new_public_id()` = `strtolower(Str::random(24))`, `shared.php:130-135`).
26. `validateDataApplications($request, $server)` (`:5025-5105`):
    - `http_basic_auth_hash_algorithm === 'argon2id'` and `!server->caddySupportsArgon2idBasicAuth()` → `422 ... {"http_basic_auth_hash_algorithm":"Argon2id needs a server that runs the Caddy proxy, version 2.11 or newer."}`
    - `has('ports_mappings')`: split `,` then `:`; duplicate first part → `422 ... {"ports_mappings":"The first number before : should be unique between mappings."}`
    - `has('custom_labels')`: not base64, or `decodeBase64EncodedLabels()` null → `422 ... {"custom_labels":"The custom_labels should be base64 encoded."}`; else request value = `base64_encode(decodedLabels)` (`decodeBase64EncodedLabels` peels nested base64 layers, `shared.php:2865-2899`).
    - `has('domains') && server->isProxyShouldRun()` (proxy type ≠ NONE and not build server, `Server.php:2117-2125`):
      `validateApplicationDomains()` errors (list of strings) → `422 {"message":"Validation failed.","errors":[ "Invalid URL: …", … ]}` (**`errors` is a JSON array** here);
      normalize again; `checkIfDomainIsAlreadyUsedViaAPI(urls, teamId, $request->uuid /* null on create */)` (`bootstrap/helpers/domains.php:250-338`):
      `error` → `422 ... {"domains":"Team ID is required."}`; conflicts and `!boolean('force_domain_override')` →
      `409 {"message":"Domain conflicts detected. Use force_domain_override=true to proceed.","conflicts":[...],"warning":"Using the same domain for multiple resources can cause routing conflicts and unpredictable behavior."}`
      (key order: message, conflicts, warning). Conflict item keys in order: `domain, resource_name, resource_uuid, resource_type, message[, service_name]`;
      messages: `Domain {d} is already in use by application '{name}'` (+ ` (service: {svc})`), `Domain {d} is already in use by service '{svcName}'`, `Domain {d} is already in use by this Coolify instance` (resource_name `Coolify Instance`, uuid null, type `instance`). Comparison key strips one trailing `/` and leading `http(s)://` (case-insensitive) on both sides.
27. Image parsing (`:2419-2443`), see 2.3; request `docker_registry_image_name`/`docker_registry_image_tag` replaced.
28. `$application = new Application`; `removeUnnecessaryFieldsFromRequest()` (`api.php:459-513`) unsets:
    `project_uuid, environment_name, environment_uuid, destination_uuid, server_uuid, type, domains, instant_deploy, github_app_uuid, private_key_uuid, use_build_server, use_build_secrets, is_static, is_spa, is_auto_deploy_enabled, is_force_https_enabled, is_preview_deployments_enabled, connect_to_docker_network, force_domain_override, autogenerate_domain, is_container_label_escape_enabled, is_preserve_repository_enabled, include_source_commit_in_build, is_git_submodules_enabled, is_git_lfs_enabled, is_git_shallow_clone_enabled, disable_build_cache, inject_build_args_to_dockerfile, is_env_sorting_enabled, redact_all_env_values_in_logs, include_coolify_default_variables, is_pr_deployments_public_enabled, stop_grace_period, rolling_update_drain_seconds, docker_images_to_keep, is_gzip_enabled, is_stripprefix_enabled, is_permanent_redirect_enabled, traefik_certificate_resolver, is_container_label_readonly_enabled, is_raw_compose_deployment_enabled, is_log_drain_enabled, is_gpu_enabled, gpu_driver, gpu_count, gpu_device_ids, gpu_options, is_consistent_container_name_enabled, custom_internal_name, custom_container_name_prefix, docker_compose_raw, tags`.
29. `fill($request->only($allowedFields))` (only `$fillable` keys stick, `Application.php:153-253`; `noindex_domains`, `ports_exposes`, `custom_labels` (base64 from step 26), health/limits, name/description etc.).
    Then explicit: `fqdn = $fqdn` (step 11 value, may be null), `build_pack = 'dockerimage'`, `destination_id`, `destination_type` (FQCN, no morph map: `App\Models\StandaloneDocker` / `App\Models\SwarmDocker`), `environment_id`, `git_repository = 'coollabsio/coolify'`, `git_branch = 'main'` (overwrites user input).
30. `save()` — see 2.5 for hooks (uuid, webhook secrets, fqdn normalization, settings row, compose_parsing_version).
31. `applyApplicationSettings($application, $applicationSettings)` (`:239-261`): if non-empty `settings->fill($settings)->save()`; label regeneration skipped because `wasRecentlyCreated`.
32. `$application->refresh()` (reloads attributes and loaded relations).
33. `if ($autogenerateDomain && blank($fqdn))` → `fqdn = generateUrl(server: $server, random: $application->uuid)`; `save()` (2.4).
34. Settings writes, each `settings->save()` separately, only when the raw captured value `isset` (i.e. key present and not null):
    `is_auto_deploy_enabled`, `is_force_https_enabled`, `is_preview_deployments_enabled`, `connect_to_docker_network`, `use_build_server`→`is_build_server_enabled`, `use_build_secrets`;
    **always** `is_container_label_escape_enabled = boolean(...,true)`.
    (`is_static`, `is_spa`, `is_preserve_repository_enabled`, `include_source_commit_in_build` are captured but NOT applied in the dockerimage branch except through `$applicationSettings`.)
35. If `settings.is_container_label_readonly_enabled` (default true): `custom_labels = implode("\n", generateLabelsApplication($application))` (`bootstrap/helpers/docker.php:1254`) — stored **plain text, not base64** (user-provided custom_labels are thereby overwritten); `save()`.
36. Tags: `attachTagsToResource` (`HandlesTagsApi.php:130-144`): per normalized name (len ≥ 2) `Tag::createOrFirst(['team_id'=>team,'name'=>name])`, `syncWithoutDetaching`.
37. `$application->isConfigurationChanged(true)` (`Application.php:1483-1492`) → `markDeploymentConfigurationApplied()` (`:1520-1550`): `refresh()` then `config_hash = sha256(json_encode(comparableSnapshot(ApplicationConfigurationSnapshot)))` (`app/Services/DeploymentConfiguration/ApplicationConfigurationSnapshot.php:24-72`) and `save()`. `config_hash` is visible in GET output → needs its own port.
38. If `$instantDeploy` truthy: `queue_application_deployment(application, deployment_uuid: new_public_id(), no_questions_asked: true, is_api: true)`.
    Only `status === 'skipped'` is handled → `200 {"message":<msg>}`; `skipped` cannot happen with `no_questions_asked`. **`queue_full` is ignored** (falls through to 201 though no row was created).
39. `auditLog('api.application.created', {team_id, application_uuid, application_name, application_type: 'dockerimage', build_pack, instant_deploy: (bool)})`.
40. `201` body `serializeApiResponse(['uuid'=>…, 'domains'=>fqdn])` → `{"uuid":"…","domains":"…"}` (`domains` may be `null`; order uuid, domains).

`return response()->json(['message' => 'Invalid type.'], 400)` (`:2534`) is unreachable for dockerimage.

### 2.2 Instant deploy ability vs. `instant_deploy` value
Step 4 uses PHP truthiness of the JSON value (`true`, `1`, `"1"`, `"yes"`… any non-empty non-`"0"` string). Step 38 uses the same raw value.

### 2.3 Docker image name/tag parsing
`DockerImageFormat` rule (`app/Rules/DockerImageFormat.php`) on `docker_registry_image_name` only:
- non-string → `The :attribute format is invalid. Use image:tag or image@sha256:hash format.`
- matches `/:sha256?:/i` → `The :attribute must use @ before sha256 digest (e.g., image@sha256:hash, not image:sha256:hash).`
- if matches `/\A(.+)@sha256:([a-f0-9]{64})\z/i` → name part = group 1, tag null; else split at last `:` if it is after the last `/` (name, tag).
- `isValidDockerImageName(name)` (blank ok, else `DOCKER_IMAGE_NAME_PATTERN`) and `isValidDockerImageTag(tag)` (blank ok, else `DOCKER_IMAGE_TAG_PATTERN`), else first message.
  Patterns (`app/Support/ValidationPatterns.php:144,152`):
  `DOCKER_IMAGE_NAME_PATTERN = /\A(?=.{1,255}\z)(?:(?:(?:[a-z0-9](?:[a-z0-9.-]*[a-z0-9])?|\[[0-9a-f:.]+\])(?::[0-9]+)?\/)?[a-z0-9]+(?:(?:[._]|__|-+)[a-z0-9]+)*(?:\/[a-z0-9]+(?:(?:[._]|__|-+)[a-z0-9]+)*)*)(?:@sha256)?\z/` (lower-case only),
  `DOCKER_IMAGE_TAG_PATTERN = /\A[A-Za-z0-9_][A-Za-z0-9_.-]{0,127}\z/`.
  So a `docker_registry_image_tag` like `sha256:<hex>` fails the tag regex; digests must come through the name.

Then (`ApplicationsController.php:2419-2443`): `$s = tag ? "$name:$tag" : $name` → `DockerImageParser::parse($s)` (`app/Services/DockerImageParser.php`):
- `/^(.+)@sha256:([a-f0-9]{64})$/i` → main = g1, tag = hex, isHash = true.
- else last `:` after last `/` (or no `/`) → main = before, tag = after, isHash = `tag` matches `/^[a-f0-9]{64}$/i`; else main = whole, tag `'latest'`.
- registry split: if >1 `/`-parts and first part contains `.` or `:` → registry = first part. `getFullImageNameWithoutTag()` = `registry/imageName` or `imageName` (i.e. the main part unchanged).
Stored: `docker_registry_image_name = main` + (`'@sha256'` appended if isHash and not already ending with it); `docker_registry_image_tag = tag` (`'latest'` default, or the 64-hex digest without prefix).
Examples: `nginx` → (`nginx`,`latest`); `nginx:1.25` → (`nginx`,`1.25`); name `nginx` + tag `1.25` → same; `ghcr.io/o/a:v1` → (`ghcr.io/o/a`,`v1`);
`registry:5000/app` → (`registry:5000/app`,`latest`); `nginx@sha256:<64hex>` → (`nginx@sha256`,`<64hex>`); `nginx:<64hex>` → (`nginx@sha256`,`<64hex>`).
At deploy time the job re-validates both with the same patterns (`ApplicationDeploymentJob.php:1439-1452`, `DeploymentException` messages `Docker registry image name contains invalid characters.` / `Docker registry image tag contains invalid characters.` / `Docker registry preview image tag contains invalid characters.`).

### 2.4 Domains
`ValidationPatterns` (`app/Support/ValidationPatterns.php`):
- `applicationDomainList(v)` (`:729-747`): blank → []; strip one leading and one trailing `,`, trim, explode `,`, trim each, drop empty.
- `validateApplicationDomains(v)` (`:603-655`): blank → []; non-string → `['The domains field must be a string.']`; per URL, first failing check wins:
  forbidden chars `/[`$;&|<>()\\\r\n]/` → `Invalid URL: {url}`; `!isValidDomainUrl` (`domains.php:24-69`: scheme http/https, host non-empty, `_`→`-` in host then `FILTER_VALIDATE_URL`) → `Invalid URL: {url}`;
  scheme not http/https → `Invalid URL scheme: {scheme} for URL: {url}. Only http and https are supported.`; blank host → `Invalid URL: {url}`;
  port outside 1..65535 → `Invalid port for URL: {url}. The port must be between 1 and 65535.`; host (brackets trimmed) has no `.` and is not an IP → `Invalid URL: {url}. The hostname must be a fully qualified domain name.`
- `applicationDomainRules()` (`:584-598`) = `['nullable','string','max:2048', closure → each error via $fail]` → MessageBag `{"domains":["…"]}`.
- `normalizeApplicationDomains(v)` (`:660-668`): list empty → null; else each URL rebuilt by `normalizeApplicationDomainUrl` (`:677-724`: lower-case scheme + `://`, `user[:pass]@`, lower-case host, `:port`, path, `?query` (if key exists), `#fragment`) joined with `,` (no spaces).

Model `saving` hook (`Application.php:314-327`) when `fqdn` dirty: `''`→null; for non-dockercompose (or filled fqdn) `DomainPortOverrides::normalize(fqdn, domain_port_overrides)` (`app/Support/DomainPortOverrides.php:27-75`):
split `,`, trim, drop empty and `!isValidDomainUrl`; each → portless URL via `DomainUrlParts::compose(scheme,host,path)` (`app/Support/DomainUrlParts.php`: scheme `http` stays, anything else → `https`; path keeps query/fragment); explicit port → int override, else previous override for that portless URL;
dedupe by portless URL (last wins, position of first); a domain without port inherits its `www.` counterpart's port; `fqdn` = portless list joined `,` (or null); `domain_port_overrides` = `{portlessUrl: int}` or null.
Then `syncNoindexDomains()` (keeps only noindex entries still among domains).
**So `https://app.example.com:3000` is stored as fqdn `https://app.example.com` + override `{"https://app.example.com":3000}`.**

Auto-generation (`bootstrap/helpers/shared.php:1477-1491`): `generateUrl(server, random=uuid)`:
`wildcard = server.settings.wildcard_domain`; null/`''` → `sslip(server)` (`shared.php:1514-1522`: dev and server id 0 → `http://127.0.0.1.sslip.io`; ip `host.docker.internal` → `base_ip()` (instance public_ipv4, else public_ipv6, else `localhost`); resolve hostname to IPv4 (then AAAA); IPv6 → `:`→`-` with `0` padding for leading/trailing `::` (`sslipHostLabel`, `:2510-2524`); result `http://<label>.sslip.io`).
Parse with Spatie Url: `"{$scheme}://{$uuid}.{$host}{$path}"` (path `/` → ''). Not forced https. Example: `http://k3x…24chars.203.0.113.7.sslip.io`.

### 2.5 Rows written
`applications` (on first `save()`), hooks in order (`app/Models/BaseModel.php:10-19`, `Application.php:300-418`):
- `creating`: `uuid = new_public_id()` if empty; `manual_webhook_secret_{github,gitlab,bitbucket,gitea} ??= Str::random(40)` (stored encrypted, `casts()` `:277-298`).
- `saving`: fqdn normalization (2.4); trims of `install_command, build_command, start_command, base_directory, publish_directory, git_repository, git_branch, git_commit_sha` when dirty; `status` dirty → `last_online_at = now()` (not on create: status is not set, so **`last_online_at` keeps the DB default, which is the migration-time constant** in `compat/baseline` schema); `custom_nginx_configuration ''`→null.
- attribute setters: `name`/`description` `strip_tags()` (`app/Traits/HasSafeStringAttribute.php`; `description: null` → `""`); `base_directory` → `'/'.ltrim(v,'/')`; `publish_directory` → `'/'.ltrim` or null; `ports_mappings ''`→null; `custom_nginx_configuration` → `base64_encode` (getter decodes); `custom_network_aliases` → JSON array string (getter returns comma list); `watch_paths` normalization (`:2505-2540`); `dockerfile_location` / `docker_compose_location` setters (`:891-923`; `docker_compose_location` null → `/docker-compose.yaml`).
- `$attributes` default `max_restart_count = 0`.
- `created`: `ApplicationSetting::create(['application_id' => id])`; `compose_parsing_version = '5'`; `save()`. (Nixpacks env var only for `build_pack === 'nixpacks'`.)
- `Auditable` trait also records model mutation `api.application.created`/`updated` (`app/Traits/Auditable.php:51-95`) — separate from `auditLog()`.
DB defaults for untouched columns (baseline): `git_commit_sha 'HEAD'`, `static_image 'nginx:alpine'`, `base_directory '/'`, `health_check_path '/'`, `health_check_host 'localhost'`, `health_check_method 'GET'`, `health_check_return_code 200`, `health_check_scheme 'http'`, `health_check_interval 5`, `health_check_timeout 5`, `health_check_retries 10`, `health_check_start_period 5`, `health_check_enabled false`, `health_check_type 'http'`, `limits_memory/'_swap'/'_reservation'/limits_cpus '0'`, `limits_memory_swappiness 60`, `limits_cpu_shares 1024`, `status 'exited'`, `preview_url_template '{{pr_id}}.{{domain}}'`, `docker_compose_location '/docker-compose.yaml'`, `swarm_replicas 1`, `custom_healthcheck_found false`, `redirect 'both'`, `compose_parsing_version '1'`→`'5'`, `is_http_basic_auth_enabled false`, `restart_count 0`, `max_restart_count 0`, `restart_limit_reached false`, `is_maintenance_enabled false`, `http_basic_auth_hash_algorithm 'bcrypt'`, `http_basic_auth_bcrypt_cost 10`, `http_basic_auth_argon2id_memory_cost 65536`, `http_basic_auth_argon2id_time_cost 4`. `ports_exposes` is **not required** for dockerimage and stays null if omitted.

`application_settings` row defaults (baseline): `is_static false, is_git_submodules_enabled true, is_git_lfs_enabled true, is_auto_deploy_enabled true, is_force_https_enabled true, is_debug_enabled false, is_preview_deployments_enabled false, is_log_drain_enabled false, is_gpu_enabled false, gpu_driver 'nvidia', gpu_count/gpu_device_ids/gpu_options null, is_include_timestamps false, is_swarm_only_worker_nodes true, is_raw_compose_deployment_enabled false, is_build_server_enabled false, is_consistent_container_name_enabled false, is_gzip_enabled true, is_stripprefix_enabled true, connect_to_docker_network false, custom_internal_name null, is_container_label_escape_enabled true, is_env_sorting_enabled false, is_container_label_readonly_enabled true, is_preserve_repository_enabled false, disable_build_cache false, is_spa false, is_git_shallow_clone_enabled true, is_pr_deployments_public_enabled false, use_build_secrets false, stop_grace_period null, inject_build_args_to_dockerfile true, include_source_commit_in_build false, docker_images_to_keep 2, custom_container_name_prefix null, redact_all_env_values_in_logs true, traefik_certificate_resolver null, include_coolify_default_variables true, is_permanent_redirect_enabled false, rolling_update_drain_seconds 5`.
`ApplicationSetting::isStatic` setter (`app/Models/ApplicationSetting.php:263-274`) has side effects (sets app `ports_exposes = 80` when truthy and saves the application) — only triggered if `is_static` is assigned (not in dockerimage create).

Save sequence on a plain create: INSERT app → INSERT settings → UPDATE app (compose_parsing_version) → [UPDATE settings] → [UPDATE app fqdn] → UPDATE settings (escape flag, always) → [UPDATE app custom_labels] → UPDATE app config_hash. Each save bumps `updated_at`.

---

## 3. `GET /applications` and `GET /applications/{uuid}` serialization

### 3.1 Endpoints
`applications()` (`ApplicationsController.php:340-366`): `?tag=` filters `whereHas('tags', name = tag)`. Query
`Application::ownedByCurrentTeamAPI(team)` (`Application.php:521-524` = `whereRelation('environment.project.team','id',team)->orderBy('name')`)
+ global scope `withCount(['additional_servers','additional_networks'])` (`:308-313`) + SoftDeletes (excludes deleted);
eager-load `destination.server.settings` only when `can_read_sensitive`. Each → `removeSensitiveData()`. `200` JSON array **ordered by name**. No policy check.

`application_by_uuid()` (`:2584-2602`): missing route uuid → `400 {"message":"UUID is required."}`; query as above `->with('settings')`;
missing → `404 {"message":"Application not found."}`; `authorize('view')`; `200 removeSensitiveData(app)`.

### 3.2 `removeSensitiveData($application)` (`:119-161`)
1. `makeHidden(['id','resourceable','resourceable_id','resourceable_type'])` → **no `id` in output**.
2. If `can_read_sensitive`: `makeVisible(['custom_labels','dockerfile','docker_compose','docker_compose_raw','manual_webhook_secret_bitbucket','manual_webhook_secret_gitea','manual_webhook_secret_github','manual_webhook_secret_gitlab','http_basic_auth_password','value','real_value'])` and `exposeNestedServerSecrets()` (`:273-298`: lazily loads `destination.server`, makes visible `logdrain_axiom_api_key, logdrain_newrelic_license_key` on server and `sentinel_token, sentinel_custom_url, logdrain_newrelic_license_key, logdrain_axiom_api_key, logdrain_custom_config, logdrain_custom_config_parser` on server.settings).
   Else `makeHidden(['private_key_id'])`.
3. `is_shown_once` (env vars only) → hide value/real_value.
4. If `settings` relation loaded: `settings->makeHidden(['id','application_id','created_at','updated_at'])`.
5. `serializeApiResponse()` (0.6) → if `docker_compose` filled: `redactLockedValuesInGeneratedCompose()` (`shared.php:5683`).

### 3.3 Model serialization (`Application::toArray()`)
- Base attributes = all `applications` columns + `additional_servers_count`, `additional_networks_count` (ints).
- Always hidden (`$hidden`, `Application.php:263-275`): `http_basic_auth_password, manual_webhook_secret_github, manual_webhook_secret_gitlab, manual_webhook_secret_bitbucket, manual_webhook_secret_gitea, dockerfile, docker_compose, docker_compose_raw, custom_labels, domain_dns_statuses, domain_port_overrides`
  (`domain_dns_statuses`, `domain_port_overrides` stay hidden even with read:sensitive).
- Casts (`:277-298`): encrypted secrets decrypted when visible; `http_basic_auth_hash_algorithm` → enum value string; `noindex_domains` array; `restart_count`, `max_restart_count` int; `restart_limit_reached`, `container_present`, `is_maintenance_enabled`, `health_check_enabled`, `custom_healthcheck_found` bool; `last_restart_at` datetime ISO.
- Accessors applied to existing columns: `status` getter (`:989-1057`): no additional servers → `value` with `(`: `before('(')` + `:` + inside parens; with `:` → `before(':')` : `after(':')`; else `"{$value}:unhealthy"` (DB default `exited` → **`"exited:unhealthy"`**); with additional servers → degraded/unhealthy aggregation over pivot statuses.
  `custom_nginx_configuration` → base64-decoded text; `custom_network_aliases` → comma list.
- `$appends = ['server_status']` (`:255`, getter `:964-987`): `destination?->server?->isFunctional()` (`Server.php:2163-2172`: `settings.is_reachable && settings.is_usable && settings.force_disabled === false && !hasPlaceholderIp()`) and every loaded additional server functional.
- **Side-effect relations**: the `status` getter lazy-loads `additional_servers` and `server_status` lazy-loads `destination` → `destination.server` → `server.settings`; Laravel's `toArray()` evaluates attributes before relations, so these relations **appear in the output**:
  - `additional_servers`: array of Server objects each with `pivot` `{application_id, server_id, standalone_docker_id, status}` (usually `[]`).
  - `destination`: StandaloneDocker row in column order `id, name, uuid, network, server_id, created_at, updated_at`, + `server`: Server `toArray()` (all columns minus hidden `logdrain_axiom_api_key, logdrain_newrelic_license_key, proxy_error_page`; casts incl. `proxy` SchemalessAttributes → object; accessors `port` int, `user`, `ip`; append `is_coolify_host` = `id === 0`; `Server.php:` casts/hidden/appends) + `settings`: ServerSetting row minus hidden `is_build_server, sentinel_token, sentinel_custom_url, logdrain_newrelic_license_key, logdrain_axiom_api_key, logdrain_custom_config, logdrain_custom_config_parser, geoip_maxmind_license_key` (`app/Models/ServerSetting.php`). Without read:sensitive, `server.proxy.last_saved_proxy_configuration` and `server.validation_logs` are removed (0.6).
  - `settings` only in `GET /applications/{uuid}` (eager-loaded); **absent from the list** (nothing lazy-loads it). Shape: `application_settings` columns in DB order minus `id, application_id, created_at, updated_at`, casts per `ApplicationSetting.php` `$casts` (bools; `docker_images_to_keep`, `stop_grace_period`, `rolling_update_drain_seconds` int; `gpu_*`, `custom_internal_name`, `custom_container_name_prefix`, `traefik_certificate_resolver` strings).
- Nested objects are not key-sorted; only the application's top level is. Final top-level order: `uuid, description (only if non-empty), name, <all other keys byte-sorted, incl. additional_servers, destination, server_status, settings>, created_at, updated_at`.
- Fields needing `read:sensitive` (+ admin): `custom_labels, dockerfile, docker_compose, docker_compose_raw, manual_webhook_secret_*, http_basic_auth_password` (top level), `private_key_id` is shown only with it, server/server-setting secrets listed above, untouched proxy config.

---

## 4. Lifecycle endpoints

### 4.1 `POST /applications/{uuid}/start` → `action_deploy` (`:4638-4691`)
Params (query or body): `force` (`boolean('force', false)`), `instant_deploy` (`boolean(..., false)`).
team → no route uuid `400 {"message":"UUID is required."}` → lookup (`ownedByCurrentTeamAPI`) missing `404 {"message":"Application not found."}` → `authorize('deploy')` (403 per 0.4 with default message) →
`queue_application_deployment(app, new_public_id(), force_rebuild: force, is_api: true, no_questions_asked: instant_deploy)` →
`skipped` → `200 {"message":"Deployment already queued for this commit."}` → else audit `api.application.deployed` `{team_id, application_uuid, application_name, deployment_uuid, force_rebuild, instant_deploy}` →
`200 {"message":"Deployment request queued.","deployment_uuid":"<24 lowercase alnum>"}`.
**`queue_full` is not handled** → same 200 success body with a uuid that has no row. Invalid `git_commit_sha` → `validateGitRef` exception → 500.

### 4.2 `POST /applications/{uuid}/restart` → `action_restart` (`:4838-4882`)
Same lookup/messages; `queue_application_deployment(app, uuid, restart_only: true, is_api: true)`; skipped → `200 {"message":…}`;
audit `api.application.restarted` `{team_id, application_uuid, application_name, deployment_uuid}`; `200 {"message":"Restart request queued.","deployment_uuid":"…"}`.
For `dockerimage`/`dockerfile`/`dockercompose` the job turns `restart_only` into a regular deployment (`ApplicationDeploymentJob.php:260-263`); the row still stores `restart_only = true`.

### 4.3 `POST /applications/{uuid}/stop` → `action_stop` (`:4752-4784`)
Same lookup/messages; `authorize('deploy')`; `docker_cleanup = boolean('docker_cleanup', true)`; `StopApplication::dispatch($application, false, $dockerCleanup)` (queue `high`, `app/Actions/Application/StopApplication.php:16-18`);
audit `api.application.stopped` `{team_id, application_uuid, application_name, docker_cleanup}`; `200 {"message":"Application stopping request queued."}`.

### 4.4 `DELETE /applications/{uuid}` → `delete_by_uuid` (`:3125-3163`)
team → `!$request->uuid` → **`404`** `{"message":"UUID is required."}` → missing → `404 {"message":"Application not found"}` (**no period**) → `authorize('delete')` → soft delete (`deleted_at`) →
`DeleteResourceJob::dispatch(resource, deleteVolumes: boolean('delete_volumes', true), deleteConnectedNetworks: boolean('delete_connected_networks', true), deleteConfigurations: boolean('delete_configurations', true), dockerCleanup: boolean('docker_cleanup', true))` (queue `high`) →
audit `api.application.deleted` `{team_id, application_uuid, application_name}` → `200 {"message":"Application deletion request queued."}`.

### 4.5 `PATCH /applications/{uuid}` → `update_by_uuid` (`:3375-3855`)
Order:
1. team → `validateIncomingRequest` (400s, 2.1.5) → lookup missing `404 {"message":"Application not found"}` (no period) → `authorize('update')` (403 `You need at least admin or owner permissions to update this application.`).
2. `has('redact_all_env_values_in_logs') || has('include_coolify_default_variables')` → `authorize('manageEnvironment')`.
3. `instant_deploy` truthy → abort 403 `Missing required permissions: deploy` (no deploy/root) → `authorize('deploy')`.
4. Build `Validator::make` (plain, default messages) with `array_merge(sharedDataApplications($application), [...])`, specific (`:3409-3429`):
   `name string|max:255`, `github_app_uuid string`, `dockerfile string`, `description string|nullable`, `watch_paths string|nullable`, `docker_compose_domains array|nullable`, `docker_compose_domains.* array:name,domain,redirect`, `.name string|required`, `.domain applicationDomainRules()`, `.redirect nullable|string|in:www,non-www,both`, `custom_nginx_configuration string|nullable`, `is_http_basic_auth_enabled boolean|nullable`, `is_preview_deployments_enabled boolean|nullable`, `http_basic_auth_username string`, `http_basic_auth_password string|max:72`, `include_source_commit_in_build boolean`, `ports_exposes nullable|string|regex:/^(\d+)(,\d+)*$/`;
   message `docker_compose_domains.*.array` → `An item in the docker_compose_domains array has invalid fields. Only name, domain, and redirect fields are supported.`
   **Not evaluated yet.** Note: `docker_registry_image_name` here is `dockerImageNameRules()` = `['nullable','string','max:255','regex:'.DOCKER_IMAGE_NAME_PATTERN]` → `nginx:1.25` is rejected (`The docker registry image name field format is invalid.`) and **no DockerImageParser normalization happens on update**.
5. `filled('ports_exposes')`: any part non-numeric or <1 or >65535 → `422 ... {"ports_exposes":"The ports_exposes should be a comma separated list of numbers."}`.
6. `custom_nginx_configuration` base64 checks (same message as create) → decoded into request.
7. `has('dockerfile')`: app has blank dockerfile → `422 ... {"dockerfile":"The dockerfile can only be updated for applications created from a Dockerfile."}` (always for dockerimage); invalid → `{"dockerfile":"The dockerfile should be base64 encoded."}`.
8. `validateDataApplications($request, $server)` (2.1.26, with `$request->uuid` = route uuid, so the app itself is excluded from conflicts) — **runs before the rule validation and extra-field check**, so a 409/422 from it can pre-empt them.
9. Validator fails or extra fields → 422 (same extra-field messages). Update `$allowedFields` (`:3407`): `name, description, is_static, is_spa, is_auto_deploy_enabled, is_force_https_enabled, is_preview_deployments_enabled, domains, noindex_domains, git_repository, git_branch, git_commit_sha, docker_registry_image_name, docker_registry_image_tag, build_pack, static_image, install_command, build_command, start_command, ports_exposes, ports_mappings, custom_network_aliases, base_directory, publish_directory, health_check_enabled, health_check_type, health_check_command, health_check_path, health_check_port, health_check_host, health_check_method, health_check_return_code, health_check_scheme, health_check_response_text, health_check_interval, health_check_timeout, health_check_retries, health_check_start_period, limits_memory, limits_memory_swap, limits_memory_swappiness, limits_memory_reservation, limits_cpus, limits_cpuset, limits_cpu_shares, custom_labels, custom_docker_run_options, post_deployment_command, post_deployment_command_container, pre_deployment_command, pre_deployment_command_container, watch_paths, manual_webhook_secret_github, manual_webhook_secret_gitlab, manual_webhook_secret_bitbucket, manual_webhook_secret_gitea, dockerfile_location, dockerfile_target_build, docker_compose_location, docker_compose_custom_start_command, docker_compose_custom_build_command, docker_compose_domains, redirect, instant_deploy, use_build_server, use_build_secrets, custom_nginx_configuration, is_http_basic_auth_enabled, http_basic_auth_username, http_basic_auth_password, http_basic_auth_hash_algorithm, http_basic_auth_bcrypt_cost, http_basic_auth_argon2id_memory_cost, http_basic_auth_argon2id_time_cost, connect_to_docker_network, force_domain_override, is_container_label_escape_enabled, is_preserve_repository_enabled, preview_url_template, max_restart_count, github_app_uuid, dockerfile` + `APPLICATION_SETTING_FIELDS`. (No `autogenerate_domain`, `tags`, `project_uuid`...)
10. Manual webhook secret 403 (only if value differs from stored).
11. `has('github_app_uuid')` (block duplicated verbatim at `:3516-3550`): dockerimage → `422 ... {"github_app_uuid":"The github_app_uuid can only be changed for applications deployed from a GitHub App or public repository."}`; else not connected → `404 {"message":"Github App not found."}`.
12. Settings prefix / internal-name 422s; raw compose 422 (requested build pack = input or current).
13. `is_http_basic_auth_enabled === true` (strict) and app lacks username or password: missing request username → `{"http_basic_auth_username":"The http_basic_auth_username is required."}`, password → `{"http_basic_auth_password":"The http_basic_auth_password is required."}` → 422.
14. dockercompose + domains → 422 (not for dockerimage).
15. `has('domains') && proxy should run`: validate (422 `errors` array) → normalize → conflicts (422/409) — repeated.
16. `docker_compose_domains` handling (`:3642-3728`; dockerimage has no `docker_compose_raw` → `422 ... {"docker_compose_domains":"Cannot set docker_compose_domains without docker_compose_raw. Reload the compose file from the git repository first."}`).
17. Settings saves (each separately, only if raw value isset): `use_build_server`→`is_build_server_enabled`, `use_build_secrets`, `is_static` (triggers the isStatic setter side effect), `is_spa`, `is_auto_deploy_enabled`, `is_force_https_enabled`, `is_preview_deployments_enabled`, `connect_to_docker_network`; `has()`-gated: `is_container_label_escape_enabled`, `is_preserve_repository_enabled`, `include_source_commit_in_build` (via `boolean()`); then `applyApplicationSettings()` (regenerates labels when readonly is on and was off, or gzip/stripprefix/permanent_redirect/cert resolver keys sent).
18. `removeUnnecessaryFieldsFromRequest`; `$data = only($allowedFields)`; **`fqdn` is set only if `has('domains')` AND `server->isProxyShouldRun()`** (otherwise a sent `domains` is silently ignored); docker compose domains json; `noindex_domains` removed from fill and applied via `setNoindexDomains()` after fill; github source switch.
19. Labels regenerated (plain text) if readonly and (domains | noindex | any basic-auth key | `ports_exposes` dirty) and proxy should run.
20. `save()` without model audit; dockerfile healthcheck parse if changed; managed DNS cleanup queued (`ManagedDnsRecordCleanup`).
21. `auditLog('api.application.updated', {team_id, application_uuid, application_name, changed_fields: values(array_intersect($allowedFields, keys(request.all())))})` (computed after unsets, so unset keys are missing).
22. instant deploy: `queue_application_deployment(app, uuid, is_api: true)` (no `no_questions_asked` → can be skipped) → skipped `200 {"message":…}`.
23. `200 {"uuid":"…"}` (plain json, not serialized). `config_hash` is not recomputed.

### 4.6 `DeployController` (`app/Http/Controllers/Api/DeployController.php`)
`removeSensitiveData($deployment)` (`:23-36`): no read:sensitive → `makeHidden(['logs'])`, else `makeVisible(['logs'])` → `serializeApiResponse`.

**`GET /deployments`** (`:71-83`): `ApplicationDeploymentQueue::ownedByTeam(team)` (`ApplicationDeploymentQueue.php:167-176`: `application_id IN (ids of team apps incl. soft-deleted, as strings)`) `->whereIn('status',['in_progress','queued'])->get()->sortBy('id')` → map → json.
`sortBy` preserves keys: if the DB returned rows not in id order, keys become non-sequential and **the response is a JSON object `{"1":{…},"0":{…}}` instead of an array**.

**`GET /deployments/{uuid}`** (`:123-140`): no uuid `400 {"message":"UUID is required."}`; `where('deployment_uuid')->first()` missing `404 {"message":"Deployment not found."}`; `$deployment->application` (excludes soft-deleted) null or team mismatch → `404 {"message":"Deployment not found."}`; `200` serialized deployment (6.3).

**`POST /deployments/{uuid}/cancel`** (`:212-357`):
no uuid 400 → not found `404 {"message":"Deployment not found."}` → app via `application()->withTrashed()` null/other team → `403 {"message":"You do not have permission to cancel this deployment."}` →
status not in `['queued','in_progress']` → `400 {"message":"Deployment cannot be cancelled. Current status: {status}"}` →
`$deploymentServer = Server::whereTeamId(team)->find(server_id)` → atomic `UPDATE ... SET status='cancelled-by-user' WHERE id=? AND status IN (queued,in_progress)`; affected ≠ 1 → refresh → same 400 message →
`addLogEntry('Deployment cancelled by user via API.', 'stderr')` → `CleanupCancelledDeployment::run($deployment, team)` (`app/Actions/Application/CleanupCancelledDeployment.php`: on each helper server `docker ps -a --filter name={uuid} --format '{{.Names}}'`; if found `docker rm -f {uuid}` and log `Deployment container stopped.`, else log `Deployment container not yet started. Will be cancelled when job checks status.`; finally `current_process_id = null`) — failures only logged →
audit `api.deployment.cancelled` `{team_id, deployment_uuid, application_id, application_uuid, server_id}` →
`200 {"message":"Deployment cancelled successfully.","deployment_uuid":"…","status":"cancelled-by-user"}`; any other throwable → `500 {"message":"Failed to cancel deployment: <msg>"}`;
`finally` (only if cancelled): `next_after_cancel($deploymentServer, $application)` → `queue_next_deployment(app, server?.id)`.
(The status UPDATE bypasses model events, so the `updated` hook audit in the model does not fire here.)

**`POST /deploy`** (`:359-396`): inputs from query **or** JSON body: `uuid`, `tag`, `force` (`boolean`), `pull_request_id` (fallback `pr`) → `$pr = value ? max((int)v,0) : 0`, `docker_tag` (`string()->trim()` or null). Checks in order, all `400`:
`pr > 2147483647` → `pull_request_id must be between 1 and 2147483647.`; uuid && tag → `You can only use uuid or tag, not both.`; tag && pr → `You can only use tag or pr, not both.`; docker_tag && pr == 0 → `docker_tag requires pull_request_id.`; docker_tag && tag → `You can only use tag or docker_tag, not both.`;
tag → `by_tags`; uuid → `by_uuids`; else `400 {"message":"You must provide uuid or tag."}`. (A JSON array `uuid` → PHP TypeError → 500.)

`by_uuids` (`:398-445`): `explode(',')`, `array_filter`; none → `400 {"message":"No UUIDs provided."}`. Per uuid: `getResourceByUuid` (`shared.php:1661-1686`; lookup order Application, Service, standalone DBs, ServiceDatabase; team check) — unknown/foreign uuids are **silently skipped**.
PR handling (dockerimage: `upsertDockerImagePreview`, `:591-616`; preview missing → item `{"message":"Pull request {pr} not found for this resource.","resource_uuid":uuid}`).
`deploy_resource()` result with `status 429` → immediately `429 {"message":"Deployment queue is full. Please wait for existing deployments to complete."}` + `Retry-After: 60`.
Item: with deployment uuid `{"message","resource_uuid","deployment_uuid"}`, else `{"message","resource_uuid"}`. Items >0 → `200 serializeApiResponse({"deployments":[…]})`; else `404 {"message":"No resources found."}`.

`deploy_resource()` for Application (`:499-540`): `authorize('deploy')` fail → `{message:'Unauthorized to deploy this application.', deployment_uuid:null}`; `docker_tag` on non-dockerimage → `'docker_tag can only be used with Docker Image applications.'`;
`$deployment_uuid = new_public_id()`; `queue_application_deployment(app, uuid, force_rebuild, pull_request_id: pr, is_api: true, git_type, docker_registry_image_tag)`;
`queue_full` → 429 marker; `skipped` → message `Deployment already queued for this commit.` **but the returned `deployment_uuid` is the freshly generated one, which was never persisted**; queued → `"Application {name} deployment queued."` + audit `api.deployment.triggered` `{resource_type:'application', application_uuid, application_name, deployment_uuid, force_rebuild, pull_request_id}`.

`by_tags` (`:447-497`): tags by `Tag where name, team_id`; missing tag skipped silently; tag with no apps/services → message `No resources found for tag {tag}.`; per app deploy (429 short-circuit); `$message` collects strings; `details` gets `{resource_uuid, deployment_uuid}` per queued app;
response `200 serializeApiResponse({"message":[…], "details":[…]?})` → sorted: `details` before `message`; no messages → `404 {"message":"No resources found with this tag."}`.

**`GET /deployments/applications/{uuid}`** (`:685-718`): `$request->validate(['skip'=>['nullable','integer','min:0'],'take'=>['nullable','integer','min:1']])` **runs first** (ValidationException → Laravel default 422 `{"message":…,"errors":…}` when JSON expected, else redirect); defaults skip 0, take 10;
missing app `404 {"message":"Application not found"}` (no period); `authorize('view')`; `Application::deployments($skip,$take)` (`Application.php:1291-1394`: `where application_id`, `count`, `orderBy created_at desc, id desc`, skip/take) → `200 {"count":n,"deployments":[…]}` — **not** passed through `serializeApiResponse` (rows in DB column order, `logs` visible only with read:sensitive, `configuration_*` always hidden).

---

## 5. `queue_application_deployment()` and helpers (`bootstrap/helpers/applications.php`)

Signature (`:19`): `(Application $application, string $deployment_uuid, ?int $pull_request_id = 0, ?string $commit = null, bool $force_rebuild = false, bool $is_webhook = false, bool $is_api = false, bool $restart_only = false, ?string $git_type = null, bool $no_questions_asked = false, ?Server $server = null, ?StandaloneDocker $destination = null, bool $only_this_server = false, bool $rollback = false, ?string $docker_registry_image_tag = null, ?string $parent_deployment_uuid = null)`.

Steps (`:19-135`):
1. `$commit = $commit ?: ($application->git_commit_sha ?: 'HEAD')`; `validateGitRef($commit, 'deployment commit')` (`shared.php:540-559`: trim; `''`/`HEAD` ok; leading `-` → `Exception("Invalid deployment commit: must not start with a hyphen.")`; not `/^[a-zA-Z0-9][a-zA-Z0-9._\-\/]*$/` → `Exception("Invalid deployment commit: contains disallowed characters. Only alphanumeric characters, dots, hyphens, underscores, and slashes are allowed.")`).
2. `deployment_url` = path of `$application->link().'/deployment/'.$uuid` = `/project/{project_uuid}/environment/{environment_uuid}/application/{app_uuid}/deployment/{deployment_uuid}` (`Application.php:730-741`, `routes/web.php:390-419`).
3. `server_id`/`server_name` = app destination server (overridable by `$server`), `destination_id` = app destination id (overridable).
4. `DB::transaction`: `SELECT ... FOR UPDATE` the application row and the server row →
   `queue_limit = server.settings.deployment_queue_limit ?? 25` (default column 25); `queued_count` = rows with `server_id` and `status='queued'`; `>= limit` → return `{status:'queue_full', message:'Deployment queue is full. Please wait for existing deployments to complete.'}`.
   Dedup: first row with same `application_id`, `commit`, `pull_request_id`, `docker_registry_image_tag` (NULL matches NULL via `whereNull`), status in (`in_progress`,`queued`); exists and not (`force_rebuild` or `rollback` or `no_questions_asked`) → return `{status:'skipped', message:'Deployment already queued for this commit.', deployment_uuid: existing.deployment_uuid, existing_deployment: model}`.
   `ApplicationDeploymentQueue::create([application_id, application_name, server_id, server_name, destination_id, deployment_uuid, deployment_url, pull_request_id, docker_registry_image_tag, force_rebuild, is_webhook, is_api, restart_only, commit, rollback, git_type, only_this_server, parent_deployment_uuid])`
   (`status` omitted → DB default `queued`; `application_id`/`destination_id` are varchar columns → stored as strings; `logs` null).
   `$started = $no_questions_asked || next_queuable(server_id, application_id, commit, pull_request_id)`; if started → `update(status='in_progress')`.
5. Non-admission → return that array.
6. UI audit only when `auth()->check() && !is_webhook && !is_api && !rollback` (`ui.application.restarted`/`ui.application.deployed`) — never for API.
7. If started: `ApplicationDeploymentJob::dispatch(application_deployment_queue_id: id)` — queue `deployment_queue()` = `'high'` self-hosted / `'deployments'` cloud (`shared.php:1064-1067`), `ShouldBeEncrypted`, `$tries = 1`, `$timeout = 3600` overwritten in constructor by `server.settings.dynamic_timeout` (`ApplicationDeploymentJob.php:50,87-89,239-279`). **The job constructor runs synchronously at dispatch** (loads app/server/destination, validates image name/tag); a throw → `fail_undispatchable_deployment()` then rethrow (→ 500 for the API caller).
8. Return `{status:'queued', message:'Deployment queued.', deployment_uuid}`.

`next_queuable(server_id, application_id, commit, pull_request_id)` (`:292-321`): false if any row of the app with same `pull_request_id` is `in_progress`; false if server missing; false if `count(in_progress on server) >= server.settings.concurrent_builds` (default 2); else true. (`commit` is unused.)

`queue_next_deployment(app, finished_server_id)` (`:154-173`): re-entrancy guarded by static `$advancing`/`$pending` queue → `start_next_queued_deployments()` (`:175-198`): server ids = {app destination server, finished server} ∪ distinct server_ids of this app's queued rows; all `queued` rows on those servers sorted by `created_at` → `start_queued_deployment()` each (errors logged).
`start_queued_deployment($d, force=false)` (`:206-243`): transaction locking app, server, deployment rows; must still be `queued`; unless force `next_queuable()`; set `in_progress`; dispatch job; dispatch failure → `fail_undispatchable_deployment` and false.
`force_start_deployment($d)` (`:141-144`) = `start_queued_deployment($d, true)`.
`fail_undispatchable_deployment($d, $e)` (`:250-290`): `UPDATE status='failed' WHERE id AND status='in_progress'`; if updated: log three stderr entries `========================================`, `Deployment could not be started: {msg}`, `========================================`; `DeploymentFailed` notification to the team (unless `parent_deployment_uuid`); `queue_next_deployment(app, server_id)`.
`next_after_cancel(server, app)` (`:326-342`): with app → `queue_next_deployment(app, server?.id)`; else start queued rows of the server by `created_at`.

---

## 6. `ApplicationDeploymentQueue` (`app/Models/ApplicationDeploymentQueue.php`)

### 6.1 Columns (baseline, in DB order)
`id bigint`, `application_id varchar NOT NULL`, `deployment_uuid varchar NOT NULL UNIQUE`, `pull_request_id int default 0`, `force_rebuild bool default false`, `commit varchar default 'HEAD'`, `status varchar default 'queued'`, `is_webhook bool default false`, `created_at`, `updated_at`, `logs text`, `current_process_id varchar`, `restart_only bool default false`, `git_type varchar`, `server_id int`, `application_name varchar`, `server_name varchar`, `deployment_url varchar`, `destination_id varchar`, `only_this_server bool default false`, `rollback bool default false`, `commit_message text`, `is_api bool default false`, `build_server_id int`, `horizon_job_id varchar`, `horizon_job_worker varchar`, `finished_at timestamp`, `docker_registry_image_tag varchar`, `configuration_hash varchar`, `configuration_snapshot text (encrypted array)`, `configuration_diff text (encrypted array)`, `parent_deployment_uuid varchar (indexed)`. Table `WITH (fillfactor='90')`.
`$hidden` = `logs, configuration_snapshot, configuration_diff`. `$casts`: `pull_request_id integer`, `finished_at datetime`, `configuration_snapshot`/`configuration_diff` `EncryptedArrayCast`. No `$appends`.
Hooks (`:68-104`): `created` with `rollback` and auth → audit `{api|ui}.application.rollback`; `updated` with `status` changed to `cancelled-by-user` and auth → audit `{api|ui}.deployment.cancelled`.

### 6.2 Status enum (`app/Enums/ApplicationDeploymentStatus.php`)
`QUEUED='queued'`, `IN_PROGRESS='in_progress'`, `FINISHED='finished'`, `FAILED='failed'`, `CANCELLED_BY_USER='cancelled-by-user'`.

### 6.3 API shape of one deployment (`serializeApiResponse`)
`id`, then byte-sorted: `application_id` (string), `application_name`, `build_server_id`, `commit`, `commit_message`, `configuration_hash`, `current_process_id`, `deployment_url`, `deployment_uuid`, `destination_id` (string), `docker_registry_image_tag`, `finished_at`, `force_rebuild`, `git_type`, `horizon_job_id`, `horizon_job_worker`, `is_api`, `is_webhook`, [`logs` only with read:sensitive], `only_this_server`, `parent_deployment_uuid`, `pull_request_id` (int), `restart_only`, `rollback`, `server_id` (int), `server_name`, `status`, then `created_at`, `updated_at` (only moved when non-null). No `uuid`/`name`/`description` keys.
`logs` is the **raw stored JSON string**, not a parsed array (so it is JSON-in-JSON, with its own `\/` escapes re-escaped).

### 6.4 `addLogEntry(string $message, string $type = 'stdout', bool $hidden = false, ?string $command = null, int $batch = 1)` (`:269-323`)
1. `type 'error'` → `'stderr'`.
2. `$message = Str::trim($message)` (Laravel `Str::trim`, also strips Unicode whitespace); if it starts with `╔` prefix `"\n"`.
3. Entry, keys in this order: `command` (null or `redactLogText(command)`), `output` (`redactLogText(message)`), `type`, `timestamp` (`Carbon::now('UTC')` → JSON `YYYY-MM-DDTHH:MM:SS.uuuuuuZ`, real microseconds), `hidden` (bool), `batch` (int); `order` appended later.
4. Hint check: `!hintAdded && hidesUnlockedEnvValues("$message $command")`.
5. Transaction: `SELECT logs ... FOR UPDATE`; decode (blank → `[]`; invalid JSON or non-array → skip write and log a warning once); `order = count(previous) + 1`; append;
   if hint needed and no existing entry has that output → append `{...entry, command:null, output: HIDDEN_ENV_VALUES_HINT, type:'stdout', hidden:false, order: order+1}` (same key order). Hint text (`:59`):
   `Some environment variable values are hidden in this log. To show values that are not locked, go to Environment Variables > Environment values in logs and select "Hide locked values only".`
6. `json_encode($logs, JSON_THROW_ON_ERROR | JSON_INVALID_UTF8_SUBSTITUTE)` — no UNESCAPED flags: `/`→`\/`, non-ASCII→`\uXXXX`, invalid UTF-8 → `�`. Raw `UPDATE ... SET logs=?` (no model events, `updated_at` untouched).
Redaction (`redactLogText`, `:239-253`): secrets from `EnvironmentVariable::logRedactionSecretsFor($application, pr != 0, resolveRemoteSecrets:false)` (cached per instance) + remote secrets; `SensitiveDataRedactor::redactText()` (`app/Services/Security/SensitiveDataRedactor.php`, replacement `<REDACTED>` = `REDACTED` const `bootstrap/helpers/constants.php:13`; failure → `[LOG SUPPRESSED: REDACTION FAILED]`; secret lookup failure → `<REDACTED>` for the whole text). Pattern-based redaction applies even with no env vars — needs its own spec.
`getOutput($name)` reads `logs` entries with `name` key (legacy).

---

## 7. Application model pieces

- `main_port()` (`Application.php:1103-1106`): `settings.is_static ? [80] : ports_exposes_array`.
- `ports_exposes_array` (`:1067-1074`): `null → []`, else `explode(',', ports_exposes)` (strings, untrimmed). `ports_mappings_array` (`:939-947`) same for mappings. `fqdns` (`:2483-2490`) same for fqdn.
- `availableInternalPorts()` (`:1113-1140`): exposed (or 80 static) + override ports + legacy ports in fqdn.
- `isHealthcheckDisabled()` (`:1464-1471`): `data_get($this,'health_check_enabled') === false` (DB default false → disabled by default).
- `customRepository()` (`:1553-1556`): `convertGitUrl(git_repository, deploymentType(), source)`; `deploymentType()` (`:1421-1440`): private_key_id > 0 → `deploy_key`; source → `source`; private_key_id === 0 → `deploy_key`; else `other`. (Docker image apps store `coollabsio/coolify`/`main`, unused.)
- `git_based()` (`:1450-1460`): false for dockerimage.
- `status` mutator: setter normalizes to `status:health` (`running (healthy)` → `running:healthy`; no health → `:unhealthy`) and reads `additional_servers` (lazy load); getter in 3.3. Setting `status` also sets `last_online_at = now()` in `saving`.
- `link()` (`:730-741`), `team()` = `environment.project.team`, `settings()` hasOne, `destination()` morphTo, `additional_servers()` belongsToMany via `additional_destinations` with pivot `standalone_docker_id, status` (`:622-626`).
- `$appends = ['server_status']`; `$hidden` and casts in 3.3; fillable `:153-253`.
- `application_settings` columns relevant to docker image apps (meaning / where used): `is_container_label_readonly_enabled` (default true; regenerates `custom_labels` on create/update), `is_container_label_escape_enabled` (default true; `$`→`$$` in labels; create always writes it), `stop_grace_period` (null → `stopGracePeriodSeconds()` = 30; valid 1..3600, `ApplicationSetting.php` + `constants.php:63-67`; `deploymentStopGracePeriodSeconds()` dev+null → 1), `rolling_update_drain_seconds` (default 5, clamp 0..300), `docker_images_to_keep` (2), `is_force_https_enabled`, `is_gzip_enabled`, `is_stripprefix_enabled`, `is_permanent_redirect_enabled`, `traefik_certificate_resolver`, `connect_to_docker_network`, `is_consistent_container_name_enabled` + `custom_internal_name`, `custom_container_name_prefix` (max 30, slugified, unique per server), `is_log_drain_enabled`, `is_gpu_enabled` + `gpu_*`, `is_auto_deploy_enabled`, `is_preview_deployments_enabled`, `redact_all_env_values_in_logs` (default true), `include_coolify_default_variables` (default true), `is_build_server_enabled`.

---

## 8. Known traps for a faithful port (summary)
1. GET output includes `destination` (+`server`+`settings`) and `additional_servers` only because accessors lazy-load them; `settings` only on show; no `id`.
2. `status` reads as `exited:unhealthy` for a fresh app; `last_online_at` is the migration-time DB default until the first status write.
3. Ports in domains are moved to hidden `domain_port_overrides`; invalid domains silently dropped at save.
4. `queue_full` ignored by `/start` and create `instant_deploy` (success body with phantom uuid); `/deploy` returns 429 + `Retry-After: 60`; `/deploy` skipped items carry a phantom uuid.
5. `GET /deploy` is a 405, not a deploy.
6. Error `errors` values are sometimes strings, sometimes arrays, once a bare JSON array (domain validation in `validateDataApplications`).
7. `GET /deployments` can serialize as an object when rows come back out of id order.
8. `logs` is a JSON string with PHP default escaping; timestamps carry real microseconds.
9. Update path: no image parsing, regex forbids `name:tag`, `domains` ignored when proxy is not running, `validateDataApplications` runs before rule validation.
10. Inconsistent messages: `Application not found` (update/delete/app-deployments) vs `Application not found.`; delete returns 404 for missing uuid.
