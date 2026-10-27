# ApplicationDeploymentJob: `build_pack = 'dockerimage'` (exact behaviour spec)

Source: Coolify main @ b5823c7, `.upstream/snapshot`. All `file:line` references are relative to the snapshot.
Main file: `app/Jobs/ApplicationDeploymentJob.php` (abbreviated **ADJ**). Command runner: `app/Traits/ExecuteRemoteCommand.php` (**ERC**).

Scope: `pull_request_id = 0`, no build server (`settings.is_build_server_enabled = false`), no additional servers/networks,
standalone Docker destination (not Swarm), Traefik proxy, root SSH user unless noted, `isDev() = false`.

Placeholders used below:

| Placeholder | Value |
|---|---|
| `{dep}` | `application_deployment_queues.deployment_uuid` (also the helper container name) |
| `{U}` | `applications.uuid` |
| `{net}` | `standalone_dockers.network` of the deployment destination (e.g. `coolify`) |
| `{cn}` | container name = `generateApplicationContainerName()` (see section 2) |
| `{workdir}` | `/artifacts/{dep}` + `rtrim(applications.base_directory, '/')`; default base_directory `/`, so `/artifacts/{dep}` |
| `{basedir}` | `/artifacts/{dep}` (`Application::generateBaseDir`, app/Models/Application.php:1558) |
| `{cfg}` | `/data/coolify/applications/{U}` (`application_configuration_dir()` shared.php:85 + `/{U}`, ADJ:290) |
| `{helper}` | `{registry}/coollabsio/coolify-helper:{version}` (section 5.3) |
| `{img}` | production image name (section 5.2) |
| `{T}` | stop timeout seconds = `settings.deploymentStopGracePeriodSeconds()` (default 30) |

---

## 1. Command execution and logging infrastructure

### 1.1 Two ways commands are run

1. **`instant_remote_process(array $commands, Server $server, bool $throwError = true, ...)`** (bootstrap/helpers/remoteProcess.php:219)
   - Joins commands with `"\n"` into one script. Non-root: lines go through `parseCommandsByLineForSudo()` (bootstrap/helpers/sudo.php:96).
   - Runs over SSH, **writes no deployment log entry**, returns `trim(stdout)`; if the trimmed output is exactly `null` it returns PHP `null`. Output is `sanitize_utf8_text()`ed.
   - Non-zero exit: `excludeCertainErrors()` (remoteProcess.php:259) throws `RuntimeException(trim(stderr) ?: "SSH command failed with exit code: {code}", code)`.
   - Wrapped in `SshRetryHandler::retry` (retries only SSH-level failures, see 1.5). With `$throwError = false` failures return `null`.
2. **`$this->execute_remote_command(...$commands)`** (ERC:66). Each argument is an array describing one command:
   - command text: `$cmd['command'] ?? $cmd[0]`
   - `hidden` (default `false`): the produced log entries get `hidden: true` (only shown in debug view).
   - `type` (default `null`): forces the log type of output entries (else `stdout` for process `out`, `stderr` for `err`).
   - `ignore_errors` (default `false`): non-zero exit is not an error.
   - `append` (default `true`): how output is saved into `$this->saved_outputs` (see below).
   - `save` (default `null`): key into `$this->saved_outputs`.
   - `command_hidden` (default `false`): log a separate hidden entry `[CMD]: <redacted command>` first, and store `command: null` on output entries. Not used in the dockerimage path.
   - `skip_command_log` (default `false`): output entries get `command: null` and base64-looking payloads of the command (`[A-Za-z0-9+/]{16,}={0,2}` that decode to printable UTF-8) are replaced in the output by `<REDACTED>`; failure messages show `[command hidden because it contains sensitive data]` (ERC:20, 240).
   - `input` (default `null`): content sent to the remote command over SSH stdin; the SSH command is built with `generateSshStdinCommand` instead of the heredoc form.

Per `execute_remote_command` call (ERC:66-159):
- `static::$batch_counter++` once per call (not per command). **This is a PHP static, it is never reset**: it increases across all jobs processed by the same Horizon worker process. All output entries of the call carry `batch = $batch_counter`.
- Throws `RuntimeException('Server is not set or is not an instance of Server model')` if `$this->server` is not a Server.
- For every command in order:
  - non-root: `docker exec ...` lines become `sudo docker exec ...`; other lines go through `parseLineForSudo()` (sudo.php:204).
  - **cancellation check**: `application_deployment_queue->refresh()`; if `status === 'cancelled-by-user'` throw `RuntimeException('Deployment cancelled by user', 69420)`.
  - Executes with retry loop `max_retries = config('constants.ssh.max_retries')` (default 3, config/constants.php:97). Retry only when `isRetryableSshFailure()` (exit code is 255/0/non-int AND message matches an SSH error pattern, app/Traits/SshRetryable.php). Before a retry it logs (batch = current counter, type stdout, not hidden):
    `SSH connection failed. Retrying... (Attempt {attempt}/{maxRetries}, waiting {delay}s)\nError: {errorMessage}` and sleeps `min(2 * 2^(attempt-1), 30)` seconds; re-checks cancellation (`'Deployment cancelled by user during retry'`, 69420).
  - Non-retryable errors are rethrown immediately. The "set status FAILED after all retries" block (ERC:144-157) is effectively unreachable (the loop rethrows before reaching it), so **execute_remote_command never writes `status = failed` itself**.

`executeCommandWithProcess` (ERC:166-234):
- SSH command = `SshMultiplexingHelper::generateSshCommand($server, $command)` (app/Helpers/SshMultiplexingHelper.php:243):
  `timeout 3600 ssh [mux opts] [-o ProxyCommand='cloudflared access ssh --hostname %h' ] -i '<keyfile>' -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o PasswordAuthentication=no -o ConnectTimeout={conn_timeout} -o ServerAliveInterval=20 -o RequestTTY=no -o LogLevel=ERROR -p '{port}' '{user}'@'{ip}' 'sh -c "if command -v bash >/dev/null 2>&1; then exec bash -se; else exec sh -se; fi"' << \{delim}\n{command}\n{delim}`
  (`{delim}` = base64 of a bcrypt hash of the command). The **remote server runs `{command}` as a bash script read from stdin**.
  With `input`: `... '{user}'@'{ip}' '{escapeshellarg(command)}'` and the input is piped to ssh stdin (SshMultiplexingHelper.php:261).
- Process timeout `config('constants.ssh.command_timeout')` (default 3600), idle timeout 3600.
- Output callback per chunk: chunk appended to a per-stream pending buffer; complete lines (up to the last `\n` or `\r`) are taken with `RunRemoteProcess::takeCompleteOutput` (app/Actions/CoolifyTask/RunRemoteProcess.php:270, also withholds an unterminated `-----BEGIN ... PRIVATE KEY-----` block) and logged as **one entry per callback batch of complete lines**:
  - `message = trim(sanitize_utf8_text(lines))`, prefixed with `"\n"` if it starts with `╔`
  - `type = $customType ?? ('err' => 'stderr', else 'stdout')`, `hidden = $hidden`, `command = ($skip_command_log || $command_hidden) ? null : $command`, `batch = $batch_counter`.
  - After the process ends, any remaining partial buffer is flushed (final=true) as one more entry.
  - **No output means no log entry at all** (the command text is only stored inside output entries).
- `saveCommandOutput` (ERC:294) runs for every raw chunk when `save` is set: `append = true` → concatenates raw chunks (untrimmed); `append = false` → stores `trim(chunk)` (last chunk wins).
- After `start()`: `application_deployment_queues.current_process_id = <local ssh PID>` (DB write for every command).
- Non-zero exit and not `ignore_errors`: refresh queue; if cancelled throw `RuntimeException('Deployment cancelled by user', 69420)`; else throw
  `DeploymentException("Command execution failed (exit code {code}): {redacted command}\nError: {redacted stderr or stdout or 'Command failed with no error output'}" [+ docker IPv6 gateway hint], code = exit code)` (ERC:240-252, 260-267).

### 1.2 Log entry storage format (`ApplicationDeploymentQueue::addLogEntry`, app/Models/ApplicationDeploymentQueue.php:269)

Signature: `addLogEntry(string $message, string $type = 'stdout', bool $hidden = false, ?string $command = null, int $batch = 1)`.
- `type 'error'` is stored as `'stderr'`. Types used: `stdout`, `stderr`, `info`.
- `$message = trim($message)`; if it starts with `╔`, prefix `"\n"`.
- Entry (PHP array, this key order): `command` (redacted or null), `output` (redacted message), `type`, `timestamp` (`Carbon::now('UTC')`, JSON-serialised as `YYYY-MM-DDTHH:MM:SS.uuuuuuZ`), `hidden` (bool), `batch` (int), then `order` = count(existing entries) + 1.
- Inside a DB transaction: `SELECT logs ... FOR UPDATE`, decode; if stored logs are non-blank and not valid JSON the entry is **dropped** (warning to laravel.log once). Append, `json_encode($logs, JSON_THROW_ON_ERROR | JSON_INVALID_UTF8_SUBSTITUTE)` (so `/` is escaped as `\/` and non-ASCII as `\uXXXX`), `UPDATE application_deployment_queues SET logs = ?, updated_at = now()` (Eloquent builder update adds `updated_at`).
- Hidden-values hint: the first time a message/command contains an unlocked known env value (only when redaction secrets are loaded), an extra entry is appended with `output = HIDDEN_ENV_VALUES_HINT`
  (`Some environment variable values are hidden in this log. To show values that are not locked, go to Environment Variables > Environment values in logs and select "Hide locked values only".`), `command: null`, `type: stdout`, `hidden: false`, `order: order+1`.
- Direct `addLogEntry()` calls in the job use batch 1.

### 1.3 Redaction (applies to every `output` and `command`)

`redactLogText()` → `SensitiveDataRedactor::redactText($text, known, locked)` (app/Services/Security/SensitiveDataRedactor.php). Order:
1. normalise: `sanitize_utf8_text`, strip ANSI escapes `\x1B(?:[@-Z\\-_]|\[[0-?]*[ -\/]*[@-~])`, strip control chars except `\t \n \r`.
2. replace known secrets: `known` = values of env vars when `redact_all_env_values_in_logs`; `locked` = locked env values + remote secrets. Each value, its `rawurlencode`, and its JSON-escaped form are replaced by `<REDACTED>` (known only if length >= 8; locked values < 8 chars only as whole tokens).
3. per 256 KiB chunk: patterns (Authorization headers, `x-access-token:`, `oauth2:`, `scheme://user:pass@`, **any email address**, Bearer tokens, GitHub/GitLab tokens, AWS keys, api_key assignments, PRIVATE KEY blocks) then sensitive `KEY=value` / `KEY: value` assignments (keys like `*_PASSWORD`, `*_TOKEN`, `secret`, `database_url`...).
On failure the text becomes `[LOG SUPPRESSED: REDACTION FAILED]`; if loading the secrets throws, the whole text becomes `<REDACTED>`.

### 1.4 `executeInDocker` (bootstrap/helpers/docker.php:543)

```
docker exec {containerId} bash -c '{command with every ' replaced by '\''}'
```

### 1.5 Other shared helpers

- `escapeShellValue($v)` = `'` + `str_replace("'", "'\\''", $v)` + `'` (docker.php:524). `escapeshellarg` (PHP) gives the same for normal strings.
- `dockerStopCommand($timeout, $containers, $server)` (docker.php:472): `docker stop --timeout={T} {containers}` when the server's stored `server_settings.docker_version` parses to >= 28.0.0, else `docker stop --time={T} {containers}` (also when unknown) (docker.php:462).
- `dockerRemoveCommandWithTimeout($c)` (docker.php:495):
  `bash -c '<script>'` where script (then escapeShellValue'd) is
  `if command -v timeout >/dev/null 2>&1; then output=$(timeout -k 10s 60s docker rm -f '{c}' 2>&1); exit_code=$?; else output=''; exit_code=124; fi; if [ "$exit_code" -eq 124 ]; then echo '__COOLIFY_CONTAINER_REMOVE_TIMEOUT__'; elif [ "$exit_code" -ne 0 ] && printf '%s' "$output" | grep -q 'No such container:'; then exit 0; elif [ "$exit_code" -ne 0 ]; then printf '%s\n' "$output" >&2; else printf '%s\n' "$output"; fi; exit $exit_code`
- `graceful_shutdown_container($name, $skipRemove = false)` (ADJ:5157): `execute_remote_command([dockerStopCommand({T}, escapeshellarg($name), $server), hidden, ignore_errors])`; if not skipRemove also `removeContainerWithTimeout($name)`. Any Exception → log `Error stopping container {name}: {msg}` (stderr).
- `removeContainerWithTimeout($name)` (ADJ:5177): `execute_remote_command([dockerRemoveCommandWithTimeout($name), hidden, ignore_errors, save => 'container_remove_'.md5($name), append => false])`; if the saved output contains `__COOLIFY_CONTAINER_REMOVE_TIMEOUT__` log (stderr) `Warning: Removing container {name} timed out after 60 seconds. The deployment will continue and cleanup will be retried in 5 minutes.` and dispatch `RemoveContainerJob($server->id, $name)` delayed 5 min.
- `getCurrentApplicationContainerStatus($server, $app, $pr)` (docker.php:279) → `containersOwnedBy` (docker.php:258) runs **one** `instant_remote_process` with these 3 lines (dockerPsByOwnerCommands, docker.php:234):
  ```
  docker ps -a --filter 'label=coolify.applicationUuid={U}' --format '{{json .}}'
  docker ps -a --filter 'label=coolify.applicationId' --filter 'label=com.docker.compose.project={U}' --format '{{json .}}'
  docker ps -a --filter 'label=coolify.applicationId' --filter 'name={U}' --format '{{json .}}'
  ```
  JSON lines → unique by `ID`. For pr 0: keeps containers whose `Labels` string has no `coolify.pullRequestId=` or has `coolify.pullRequestId=0`. Includes stopped containers (`-a`).

---

## 2. `__construct(int $application_deployment_queue_id)` (ADJ:239-319)

**Runs when the job is dispatched** (in `queue_application_deployment()` or `start_queued_deployment()`, bootstrap/helpers/applications.php), not when the worker starts it. The object (including all private props; models as identifiers re-queried on unserialize) is serialised into the encrypted queue payload (`ShouldBeEncrypted`). Therefore **`{cn}` (with its timestamp) is fixed at dispatch time**.

- `onQueue(deployment_queue())` → `'high'` (self-hosted) / `'deployments'` (cloud) (shared.php:1064). `$tries = 1`, `$timeout = server.settings.dynamic_timeout`.
- loads queue row, application, `build_pack`.
- `commit = validateGitRef(queue.commit)` (trimmed; `''`/`HEAD` allowed; must match `^[a-zA-Z0-9][a-zA-Z0-9._\-\/]*$`, shared.php:540). Default commit written by `queue_application_deployment` = `applications.git_commit_sha ?: 'HEAD'` (applications.php:21).
- `force_rebuild` (true if `settings.disable_build_cache`), `rollback`, `restart_only = queue.restart_only && build_pack not in [dockerimage, dockerfile, dockercompose]` → **always false for dockerimage** (ADJ:263).
- `dockerImagePreviewTag = queue.docker_registry_image_tag`; `validateDockerRegistryImageConfiguration()` (ADJ:1439) throws `DeploymentException` `Docker registry image name contains invalid characters.` / `Docker registry image tag contains invalid characters.` / `Docker registry preview image tag contains invalid characters.` (an exception here aborts the dispatch).
- `server` = queue server; `destination` = that server's destination with `queue.destination_id`; `server = mainServer = destination.server`.
- `is_this_additional_server` (pivot lookup) - false in scope.
- `preserveRepository = settings.is_preserve_repository_enabled`.
- `basedir`, `workdir` (validates `base_directory` with `FILE_PATH_PATTERN` and no `..` unless `/`), `configuration_dir`, `is_debug_enabled`.
- `container_name = generateApplicationContainerName($app, 0)` (docker.php:587):
  - consistent name enabled (`settings.is_consistent_container_name_enabled`): `settings.custom_internal_name ?: {U}`
  - else: `(settings.custom_container_name_prefix ?: {U}) . '-' . now()->format('Ymd\THis')` e.g. `vwk8s4gc0cw8kkw4ow8sgcc0-20261028T101500` (app timezone UTC).
- `saved_outputs = collect()`.

---

## 3. `handle()` (ADJ:358-459)

1. `application_deployment_queue->refresh()`; if `status === 'cancelled-by-user'`: log `Deployment was cancelled before starting.`, `handleCancelledDeployment()` (multi-server notification only), **return** (no finally, no status change).
2. try (catch `Exception` → `$this->fail($e); throw $e;`):
   - `validateDeploymentEnvironmentVariableKeys()` (ADJ:2532): for every non-preview env var (`key, is_buildtime, is_runtime`) whose key is not a valid key: if key is empty or contains `=`, `\n`, `\r`, `\0` → logs the invalid-name block (ADJ:2634) and throws `DeploymentException::alreadyLogged('Invalid environment variable name from the deployment environment: {key}. Names must start with ...')`. Otherwise logs warnings (dockerimage never passes build-time variables, ADJ:2560):
     - buildtime: `⚠️ Build-time variable {key} uses a name that new variables cannot use. This deployment does not build an image, so it is not used as a build-time variable. Rename it before you use it in a build.` (stderr)
     - runtime: `⚠️ Runtime variable {key} uses a name that new variables cannot use. It is still passed to the container, but shell scripts cannot read it.` (stderr)
     - neither: `⚠️ Variable {key} uses a name that new variables cannot use. This deployment does not use it.` (stderr)
     - always: `   Suggested name: {suggested}` (type `info`).
   - `ensureRegistryImageForMultipleServers()`, `warnAboutVolumesOnMultipleServers()`, `warnAboutMixedProxiesOnMultipleServers()` - no-ops without additional servers.
3. DB: `UPDATE application_deployment_queues SET status = 'in_progress', horizon_job_worker = gethostname()`.
4. `if (! $server->isFunctionalAfterRecheck())` (app/Models/Server.php:2178): log `Server is not functional.`, `$this->fail('Server is not functional.')` (string → `ManuallyFailedException`; runs `failed()`), **return** (no finally).
5. try / catch `Exception` / finally:
   - `server->privateKey->storeInFileSystem()` (local file).
   - `instant_remote_process(["docker network inspect '{net}' -f '{{json .Containers}}' "])` (note trailing space). Throws if the network does not exist (→ job fails). Result parsed into `$this->addHosts` via `addHostFlags()` (ADJ:328: `--add-host {name}:{ipv4}` for containers on the network, skipping `coolify-proxy`, generated container names `-\d{8}T\d{6}$`/`-\d{12}$`, and names that are deployment uuids in `application_deployment_queues`). **`addHosts` is never used by the dockerimage path.**
   - `dockerfile_target_build` validation (throws `RuntimeException('Invalid dockerfile_target_build: contains forbidden characters.')`); unused here.
   - `customRepository()` (no effect).
   - `selectBuildServer()` (section 4.1), `detectBuildKitCapabilities()` (section 4.2), `decide_what_to_do()` (section 5).
   - catch `Exception $e`: (PR only: PR update job) → `$this->fail($e)` (runs `failed()` immediately, section 7) → `throw $e`.
   - **finally** (section 6).

Laravel ordering consequence: for an `Exception`, `failed()` log lines are written **before** the finally-block log lines. For an `Error` (e.g. `TypeError`) the catch does not match: finally runs first, then the worker calls `failed()`. A worker timeout kills the process: finally does not run; `failed()` runs with `TimeoutExceededException`.

---

## 4. Pre-deploy checks

### 4.1 `selectBuildServer()` (ADJ:461)
`build_server = server`. Dockerimage is exempt from "must build elsewhere". With `is_build_server_enabled = false` it returns immediately: **no log, no DB write**. (If a dockerimage app has `is_build_server_enabled = true` it would still pick a build server; out of scope.)

### 4.2 `detectBuildKitCapabilities()` (ADJ:514)
`{serverName}` = `deployment server ({server.name})`.
1. `instant_remote_process(["docker version --format '{{.Server.Version}}'"])` → `$dockerVersion` (trimmed). `server->rememberDockerVersion()` → `UPDATE server_settings SET docker_version = parseDockerEngineVersion(v) ('X.Y.Z'), docker_version_checked_at = now()` (Server.php:2570).
2. major < 18 or 18.<9: log `Docker {v} on {serverName} does not support BuildKit (requires 18.09+).` and return.
3. `instant_remote_process(["docker buildx version >/dev/null 2>&1 && echo 'available' || echo 'not-available'"])`
   - `available`: log `Docker {v} with BuildKit and Buildx detected on {serverName}.`
   - else `instant_remote_process(["DOCKER_BUILDKIT=1 docker build --help 2>&1 | grep -q '\-\-progress' && echo 'supported' || echo 'not-supported'"])` → log `Docker {v} with BuildKit support detected on {serverName}.` or `Docker {v} on {serverName} does not support BuildKit. Build output progress will be limited.`
4. If `settings.use_build_secrets` and BuildKit: `instant_remote_process(["docker build --help 2>&1 | grep -q 'secret' && echo 'supported' || echo 'not-supported'"])` → log `Build secrets are enabled and will be used for enhanced security.` or `Docker on {serverName} does not support build secrets. Using traditional build arguments.`
5. Any Exception: log `Could not detect BuildKit capabilities on {serverName}: {msg}` (does not fail).
All these logs: type stdout, visible, batch 1. They appear in every dockerimage deployment.

---

## 5. `decide_what_to_do()` → `deploy_dockerimage_buildpack()` → `post_deployment()`

`decide_what_to_do` (ADJ:591): `restart_only` is false for dockerimage, and the `build_pack === 'dockerimage'` branch is checked before the PR / inline-Dockerfile branches → `deploy_dockerimage_buildpack()` then `post_deployment()`.

### 5.1 `deploy_dockerimage_buildpack()` (ADJ:733-751)
```
dockerImage    = applications.docker_registry_image_name
dockerImageTag = resolveDockerImageTag()
log "Starting deployment of {displayName} to {server.name}."
generate_image_names()
prepare_builder_image()
generate_compose_file()
save_runtime_environment_variables()
rolling_update()
```
`resolveDockerImageTag()` (ADJ:753): (PR tag n/a) → `rollback && commit != ''` ? `commit` : `docker_registry_image_tag` if non-empty : `'latest'`.
`displayName` = tag starts with `sha256-` ? `{image}@sha256:{tag after 'sha256-'}` : `{image}:{tag}`.
Note: for a normal (non-rollback) deploy the queue commit (`HEAD` by default) is **not** the image tag; it only ends up in `SOURCE_COMMIT`.

### 5.2 `generate_image_names()` (ADJ:1454)
First branch wins: **if `applications.dockerfile` is non-empty** (leftover inline Dockerfile) → `production_image_name = {registry_name or U}:latest` (quirk). Otherwise dockerimage branch:
- tag `sha256-{hash}` → `{img} = {dockerImage}@sha256:{hash}`
- else `{img} = {dockerImage}:{dockerImageTag}`. `build_image_name` is not set.

### 5.3 `prepare_builder_image(firstTry = true)` (ADJ:3112-3160)
1. `checkForCancellation()` (ADJ:6197): refresh; if cancelled log `Deployment cancelled by user, stopping execution.` and throw `DeploymentException('Deployment cancelled by user', 69420)`.
2. `{helper}` = `coolifyHelperImage() . ':' . getHelperVersion()`:
   - `coolifyHelperImage()` (shared.php:4717): if `config('constants.coolify.helper_image')` (env `HELPER_IMAGE`, default `{REGISTRY_URL or docker.io}/coollabsio/coolify-helper`) differs from `{registry_url}/coollabsio/coolify-helper` use it; else `{instance_settings.docker_registry_url ?: 'docker.io'}/coollabsio/coolify-helper`.
   - `getHelperVersion()` (shared.php:4729): `config('constants.coolify.helper_version')` = `1.0.19` (config/constants.php:9), unless `instance_settings.helper_version` (id 0) is a greater version.
   - Default: `docker.io/coollabsio/coolify-helper:1.0.19`.
3. `instant_remote_process(['echo $HOME'])` → `{home}` (e.g. `/root`).
4. `instant_remote_process(["mkdir -p {home}/.docker/buildx"])`.
5. `instant_remote_process(["test -f {home}/.docker/config.json && echo 'OK' || echo 'NOK'"])` → `dockerConfigFileExists`.
6. `env_flags = generate_docker_env_flags_for_secrets()` → **always `''` for dockerimage** (ADJ:5395, `deploymentPassesBuildtimeVariables()` false).
7. `buildxMetadataVolume = "-v {home}/.docker/buildx:/root/.docker/buildx"` (dev+localhost: `-v coolify-buildx:/root/.docker/buildx`).
8. `seccompMounts = seccompProfileMountFlags(custom_docker_run_options)` (docker.php:1872): for each `security_opt` `seccomp=/abs/path` / `seccomp:/abs/path` → `--mount 'type=bind,source={p},target={p},readonly'`, space-joined; usually `''`.
9. run command (note the double spaces produced by empty `{env_flags}` / `{seccompMounts}`):
   - config.json exists:
     `docker run -d --network '{net}' --name {dep} {env_flags} --rm -v {home}/.docker/config.json:/root/.docker/config.json:ro {buildxMetadataVolume} -v /var/run/docker.sock:/var/run/docker.sock {seccompMounts} {helper}`
     → typically `docker run -d --network 'coolify' --name {dep}  --rm -v /root/.docker/config.json:/root/.docker/config.json:ro -v /root/.docker/buildx:/root/.docker/buildx -v /var/run/docker.sock:/var/run/docker.sock  docker.io/coollabsio/coolify-helper:1.0.19`
   - no config.json:
     `docker run -d --network '{net}' --name {dep} {env_flags} --rm {buildxMetadataVolume} -v /var/run/docker.sock:/var/run/docker.sock {seccompMounts} {helper}`
10. log `Preparing container with helper image: {helper}` (retry variant: `Preparing container with helper image with updated envs.`).
11. `graceful_shutdown_container({dep}, skipRemove: true)` → `docker stop --time={T} '{dep}'` (hidden, ignore_errors). Usually logs a hidden stderr entry `Error response from daemon: No such container: {dep}`.
12. `execute_remote_command([runCommand, hidden, skip_command_log => filled(env_flags) (false)], ['command' => executeInDocker({dep}, "mkdir -p {basedir}")])`
    → `docker exec {dep} bash -c 'mkdir -p /artifacts/{dep}'` (not hidden; no output → no entry). The `docker run` output (container id, plus pull progress on stderr if the helper image is missing) is logged hidden.
13. `run_pre_deployment_command()` (ADJ:6105), only if `pre_deployment_command` non-empty:
    - `getCurrentApplicationContainerStatus(...)`; 0 containers → log `Pre-deployment command: No running containers found. Skipping.` and return. (Counts stopped containers too, `docker ps -a`.)
    - log `Executing pre-deployment command (see debug log for output/errors).`
    - `resolveCommandContainer` (ADJ:6067): 1 container → it; several → needs `pre_deployment_command_container`; matches name prefix `{container}-{U}`; failure logs `Pre-deployment command: Multiple containers found but no container name specified. Available: {names}` or `Pre-deployment command: Container '{x}' not found. Available: {names}` then throws `DeploymentException('Pre-deployment command: Could not find a valid container. Is the container name correct?')`.
    - validates name `/\A[a-zA-Z0-9][a-zA-Z0-9._-]*\z/` (`RuntimeException('Invalid container name: contains forbidden characters.')`).
    - command: newlines (`\r\n`,`\r`,`\n`) → space; `docker exec {name} sh -c '{cmd with ' → '\''}'`, hidden. Non-zero exit fails the deployment.

### 5.4 `generate_compose_file()` (ADJ:4364-4606)
1. `checkForCancellation()`.
2. `create_workdir()` (ADJ:3082): `execute_remote_command(['command' => executeInDocker({dep}, "mkdir -p {workdir}")], ['command' => "mkdir -p {cfg}"])` (not hidden, no output).
3. `$ports = main_port()` = `settings.is_static ? [80] : explode(',', ports_exposes)` (strings, Application.php:1103/1067).
4. `persistent_storages` = for each `persistentStorages`: `{host_path ?: name}:{mount_path}` (ADJ:4624). `file volumes` = `fileStorages()->get()` → `{fs_path}:{mount_path}`. `volume_names` = for storages without host_path: `[name => ['name' => name, 'external' => false]]` (ADJ:4643). DB order (no ORDER BY).
5. **Labels** (section 8 has the generators):
   - if `applications.custom_labels` non-empty:
     - `parseContainerLabels()` (Application.php:2463): normalises the column (decodes repeatedly-base64-encoded values, raw text gets base64-encoded; undecodable base64 → regenerated from `generateLabelsApplication`) and `save()`s.
     - `labels = preg_split("/\r\n|\n|\r/", base64_decode(custom_labels))` minus lines starting with `coolify.`; **empty lines are kept** (become `''` items); `custom_labels = base64_encode(implode("\n", labels))`; `application->save()` (DB write only if changed).
   - else if `settings.is_container_label_readonly_enabled` → `labels = generateLabelsApplication($app, null)`.
   - else `$labels` is undefined → PHP warning → Laravel `ErrorException` (deployment fails). Quirk.
   - So **in the normal case the labels come from the stored `custom_labels` column, not regenerated at deploy time** (they are regenerated elsewhere when the user saves the app with readonly labels on).
   - `useDestinationNetworkInCaddyLabels()` (ADJ:3178): replaces `caddy_ingress_network={primary net}` with `caddy_ingress_network={this net}` if different (no-op for the primary server).
   - `traefikHostRulesWithoutIpv6Brackets()` (docker.php:957): ``Host(`[v6]`)`` → ``Host(`v6`)`` when v6 is a valid IPv6.
   - if `settings.is_container_label_escape_enabled` (default true): every label `$` → `$$` (`escapeDollarSign`, docker.php:2029). Not applied to the default labels below.
   - append `defaultLabels({U}, {U}, project.name, app.name, environment.name, 0)` (docker.php:651), then `->toArray()` (re-indexed list).
6. Dockerfile HEALTHCHECK parse only when `build_pack === 'dockerfile'` or `applications.dockerfile` set: not for dockerimage.
7. `custom_network_aliases = custom_network_aliases_array ?: []` (JSON-decoded column).
8. Compose array, **in this key order** (ADJ:4411-4443):
   ```php
   [
     'services' => [
       '{cn}' => [
         'image' => '{img}',
         'container_name' => '{cn}',
         'restart' => 'unless-stopped',              // RESTART_MODE constants.php:62
         'expose' => $ports,                          // only if $ports non-empty (strings → YAML '80')
         'networks' => ['{net}' => ['aliases' => ['{cn}', ...custom_network_aliases]]],
         'mem_limit' => limits_memory,                // string, default '0'
         'memswap_limit' => limits_memory_swap,       // string, default '0'
         'mem_swappiness' => limits_memory_swappiness,// int, default 60
         'mem_reservation' => limits_memory_reservation, // string, default '0'
         'cpus' => (float) limits_cpus,               // default 0.0 → YAML 0.0
         'cpu_shares' => limits_cpu_shares,           // int, default 1024
       ],
     ],
     'networks' => ['{net}' => ['external' => true, 'name' => '{net}', 'attachable' => true]],
   ]
   ```
   then appended to the service, in order:
   - `env_file: ['.env']` (always).
   - `stop_grace_period: '{N}s'` if `settings.stop_grace_period !== null` (N = value if 1..3600 else 30).
   - `healthcheck` if `health_check_enabled !== false` and `generate_healthcheck_commands()` returns non-null:
     `['test' => ['CMD-SHELL', $cmd], 'interval' => '{health_check_interval}s', 'timeout' => '{health_check_timeout}s', 'retries' => health_check_retries (int), 'start_period' => '{health_check_start_period}s']`.
   - `cpuset` if `limits_cpuset !== null`.
   - (Swarm branch skipped) `labels` = the label list.
   - `logging` if server log drain enabled and `settings.is_log_drain_enabled`: fluentd `['driver' => 'fluentd', 'options' => ['fluentd-address' => 'tcp://127.0.0.1:24224', 'fluentd-async' => 'true', 'fluentd-sub-second-precision' => 'true', 'env' => 'COOLIFY_APP_NAME,COOLIFY_PROJECT_NAME,COOLIFY_SERVER_IP,COOLIFY_ENVIRONMENT_NAME']]` or awslogs (shared.php:4486/4500).
   - GPU: `deploy.resources.reservations.devices = [settings->gpuDeviceReservation()]` (ApplicationSetting.php:194); log `GPU options are not a valid key=value list or YAML mapping. They are ignored.` if options unparsable.
   - if healthcheck disabled: `healthcheck` removed again.
   - `ports` = `explode(',', ports_mappings)` if non-empty.
   - `volumes` = persistent storages then file volumes.
   - top-level `volumes` = volume_names (after `networks`).
9. Custom Docker options (pr 0) (ADJ:4556-4602): `custom_compose = convertDockerRunToCompose(custom_docker_run_options)` (docker.php:1659, mapping DOCKER_RUN_OPTION_MAPPING constants.php:27). GPU-enabled + `--gpus` → removed, log `GPU access is enabled in the GPU settings, so --gpus in Custom Docker Options is ignored.`. `unsupportedDockerRunOptions()` non-empty → log (stderr) `Warning: These custom Docker options are not supported and were not applied: {a, b}. See https://coolify.io/docs/applications/builds/custom-docker-options for the supported options.`
   - consistent name enabled: service key renamed to `{U}` (keeps `container_name: {cn}` = custom_internal_name/uuid); `ip`/`ip6` → `networks.{net}.ipv4_address/ipv6_address` (aliases kept); then `array_merge_recursive(service, custom_compose)`.
   - else: if `--ip`/`--ip6` present, the whole `networks` key is removed and re-added **at the end of the service** with only `ipv4_address`/`ipv6_address` (aliases are lost); then `array_merge_recursive(service, custom_compose)` (new keys appended at the end of the service, in `custom_compose` order).
   - convertDockerRunToCompose output types: list options (`--add-host`→`extra_hosts`, `--cap-add`→`cap_add`, `--cap-drop`, `--security-opt`→`security_opt`, `--sysctl`→`sysctls`, `--device`→`devices`, `--dns`, `--tmpfs`) → arrays of unique values; `--init`/`--privileged`/`--read-only` → bool (`=false` → false); `--ulimit` → `ulimits: {type: {soft, hard}}` or scalar; `--shm-size`/`--hostname`/`--runtime`/`--entrypoint` → string; `--pids-limit` → int; `--gpus` → `deploy.resources.reservations.devices[0] = {driver: nvidia, capabilities: [gpu], device_ids|count}`; `-i`/`--interactive` → `stdin_open: true`, `-t`/`--tty` → `tty: true`.
10. `docker_compose = Yaml::dump($docker_compose, 10)` (Symfony Yaml 8.1.8 per composer.lock; indent 4, inline only beyond depth 10 = everything in block style); `generated_docker_compose = docker_compose`. Example in section 10.

`generate_healthcheck_commands()` (ADJ:4666):
- `health_check_type === 'cmd'` and command non-empty: newlines → spaces; must match `/^[a-zA-Z0-9 \-_.\/:=@,+]+$/` and be <= 1000 chars, else log `Warning: Health check command contains invalid characters or exceeds max length. Falling back to HTTP healthcheck.`; valid → `full_healthcheck_url = cmd`, return cmd.
- HTTP: port = `health_check_port` or `(int) ports_exposes_array[0]` (none → return null, no healthcheck); static → 80. method `/^[A-Z]+$/` else `GET`; scheme `/^https?$/` else `http`; host valid (`/^[a-zA-Z0-9.\-_]+$/` or IP) → `formatHostForUrl` (IPv6 bracketed) else `localhost`; path non-empty → must match `#^[a-zA-Z0-9/\-_.~%,;]+$#` else `/`.
  - `full_healthcheck_url = "{method}: {scheme}://{host}:{port}{path ?: '/'}"`
  - returns `curl -s -X '{method}' -f '{scheme}://{host}:{port}{path}' > /dev/null || wget -q -O- '{scheme}://{host}:{port}{path}' > /dev/null || exit 1`.

### 5.5 `save_runtime_environment_variables()` (ADJ:2062-2186)
`$envs = generate_runtime_environment_variables()` (section 9). Not empty in practice (always at least `HOST`/coolify vars):
1. log `Creating .env file with runtime variables for container.` **hidden**.
2. `b64 = base64_encode(implode("\n", $envs))` (no trailing newline in the file).
3. `execute_remote_command([executeInDocker({dep}, "rm -f {workdir}/.env && echo '{b64}' | base64 -d | tee {workdir}/.env > /dev/null"), 'skip_command_log' => true])`.
4. (isDev only: hidden `[DEBUG] Variable names in {workdir}/.env: {keys}`.)
5. `execute_remote_command(["rm -f {cfg}/.env", hidden], ["echo '{b64}' | base64 -d | tee {cfg}/.env > /dev/null", 'skip_command_log' => true])`.

Empty-collection branch (dockerimage): log `Creating empty .env file (no environment variables defined).`; `executeInDocker({dep}, "rm -f {workdir}/.env && touch {workdir}/.env")`; `["rm -f {cfg}/.env", hidden]`, `["touch {cfg}/.env"]`.

Files written: `{workdir}/.env` inside the helper container (= `/artifacts/{dep}/.env`, used by compose for interpolation **and** as `env_file`), and `{cfg}/.env` = `/data/coolify/applications/{U}/.env` on the host.

### 5.6 `rolling_update()` (ADJ:2822-2873)
All exceptions are wrapped: `DeploymentException('Rolling update failed ('.get_class($e).'): '.$e->getMessage(), $e->getCode(), $e)`.
1. `checkForCancellation()`.
2. `write_compose_file_to_workdir()` (ADJ:4612):
   `execute_remote_command(["docker exec {dep} rm -f {workdir}/docker-compose.yaml", hidden], ["docker exec -i {dep} tee {workdir}/docker-compose.yaml > /dev/null", 'input' => docker_compose, hidden])` (note: plain `docker exec`, not `executeInDocker`).
3. Not Swarm, no build server. **Stop-first path** if any of: `ports_mappings` non-empty, consistent container name enabled, PR, or `custom_docker_run_options` contains the substring `--ip` (also matches `--ip6`, and e.g. `--ipc=host`):
   - log `----------------------------------------`
   - conditional logs: `Application has ports mapped to the host system, rolling update is not supported.` / `Consistent container name feature enabled, rolling update is not supported.` / (+ custom_internal_name) `Custom internal name is set, rolling update is not supported.` / `Custom IP address is set, rolling update is not supported.`
   - `stop_running_container(force: true)`, then `start_by_compose_file()`. **No `health_check()`**, so `applications.status` is not set and the "healthy" cache flag is not set.
4. Otherwise (**rolling path**):
   - log `----------------------------------------`
   - log `Rolling update started.`
   - `start_by_compose_file()`
   - `health_check()`
   - `wait_for_proxy_to_route_new_container()`
   - `stop_running_container()`
   - log `Rolling update completed.` (logged even when the health check failed and the deployment was already marked failed).

`start_by_compose_file()` (ADJ:5269):
1. `write_missing_content_files()` (ADJ:1166): unless preserve-repository, `EnsureContentFilesOnServer::run(fileStorages, mainServer, log)` (app/Actions/Shared/EnsureContentFilesOnServer.php): for file storages with content, checks remote state; writes missing/empty-dir ones; logs `Writing 1 missing configuration file.` / `Writing {n} missing configuration files.` and warnings. No-op without file storages.
2. `execute_remote_command(["touch {cfg}/.env", hidden], ["docker network connect '{net}' coolify-proxy >/dev/null 2>&1 || true", hidden, ignore_errors])` (StandaloneDocker.php:59).
3. log `Pulling latest images from the registry.`
4. `execute_remote_command(`
   `[executeInDocker({dep}, "docker compose --project-name {U} --project-directory {workdir} pull"), hidden],`
   `[executeInDocker({dep}, "{coolify_variables} docker compose --project-name {U} --project-directory {workdir} up --build -d"), hidden])`
   - `set_coolify_variables()` is **never called** in the dockerimage path, so `{coolify_variables}` is `null` → the second command is `docker exec {dep} bash -c ' docker compose --project-name {U} --project-directory /artifacts/{dep} up --build -d'` (**leading space inside the quotes**).
   - No `-f`: compose finds `{workdir}/docker-compose.yaml`; reads `{workdir}/.env` for interpolation. Registry auth comes from the helper's mounted `/root/.docker/config.json`.
   - The image pull happens here (`pull`); `pull_latest_image()` (ADJ:4748) is only for static images and is **not** used.
5. log `New container started.`
6. Any Exception → `DeploymentException("Failed to start container: {msg}", code, e)`.

`health_check()` (ADJ:2915):
- If `health_check_enabled === false` and `custom_healthcheck_found === false`: `markNewVersionHealthy(true)` and return (no log, **no `applications.status` write**). (Default `health_check_enabled` is false per migration 2024_07_19_132617.)
- If disabled but `custom_healthcheck_found`: log `Using the custom healthcheck from the Dockerfile.` and continue.
- log `Waiting for healthcheck to pass on the new container.`
- if `full_healthcheck_url`: log `Healthcheck command (inside the container): {x}` (type cmd) or `Healthcheck URL (inside the container): {x}`.
- log `Waiting for the start period ({health_check_start_period} seconds) before starting healthcheck.`; sleeps start_period x 1 s.
- loop `counter = 1..health_check_retries`:
  - `execute_remote_command(["docker inspect --format='{{json .State.Health.Status}}' '{cn}'", hidden, save 'health_check', append false], ["docker inspect --format='{{json .State.Health.Log}}' '{cn}'", hidden, save 'health_check_logs', append false])` (run on the host, not in the helper; output also logged as hidden entries).
  - log `Attempt {counter} of {retries} | Healthcheck status: {saved health_check}` (value keeps the JSON quotes, e.g. `"starting"`).
  - last entry of the decoded `.State.Health.Log`: `Output` (empty → `(no logs)`), `ExitCode` (missing → `(no return code)`); if either is present log `Healthcheck logs: {Output} | Return code: {ExitCode}` (message is trimmed by addLogEntry).
  - status (quotes stripped) `healthy`: `markNewVersionHealthy(true)`; `applications.update(['status' => 'running'])` (the Application `saving` hook also sets `last_online_at = now()` when status changes, Application.php:352); log `New container is healthy.`; break.
  - `unhealthy`: `markNewVersionHealthy(false)`; log `New container is unhealthy.` (type `error` → stored `stderr`); `query_logs()`; break.
  - else counter++ and sleep `health_check_interval` x 1 s.
- after the loop, if status is still `starting` → `query_logs()`.
- `query_logs()` (ADJ:3015): log `----------------------------------------`, `Container logs:`, `execute_remote_command(['command' => "docker logs -n 100 '{cn}'", 'type' => 'stderr', 'ignore_errors' => true])` (visible, all output as stderr), log `----------------------------------------`.
- Exceptions → `DeploymentException('Health check failed ('.class.'): '.msg, code, e)`.
- `markNewVersionHealthy` (ADJ:3000) stores **`Cache` key `deployment-new-version-healthy:{dep}` = true for 1 day**, or forgets it. `newVersionIsHealthy()` reads it. It lives in the cache (Redis), because `failed()` runs on a fresh job instance.

`wait_for_proxy_to_route_new_container()` (ADJ:2879):
- `seconds = settings.rollingUpdateDrainSeconds()` = clamp(`rolling_update_drain_seconds ?? 5`, 0, 300). Return if 0 or new version not healthy.
- old = `getCurrentApplicationContainerStatus(...)` where `State === 'running'` and `Names !== {cn}`; none → return.
- `proxyLabelsChanged()`: `instant_remote_process(["docker inspect --format '{{json .Config.Labels}}' '{cn}' '{old1}' ..."], server, throwError: false)`; compares, per container, the labels whose key starts with `traefik.` or `caddy` (sorted by key); if they differ → log `The proxy labels changed, so the old containers stop without waiting.` and return.
- else log `Waiting {seconds} seconds for the proxy to send traffic to the new container.` and sleep.

`stop_running_container(bool $force = false)` (ADJ:5207):
- log `Removing old containers.`
- healthy or force:
  - consistent name: every container from `getCurrentApplicationContainerStatus` plus `{cn}` (unique) → `graceful_shutdown_container(name)` (stop + remove).
  - else: containers from `getCurrentApplicationContainerStatus` (running **and stopped**) except `Names == {cn}` → `graceful_shutdown_container(Names)`: `docker stop --time={T} '{old}'` + `dockerRemoveCommandWithTimeout('{old}')` (both hidden, ignore_errors).
- not healthy (rolling path only):
  - for dockerimage log `----------------------------------------`, `WARNING: Dockerfile or Docker Image based deployment detected. The healthcheck needs a curl or wget command to check the health of the application. Please make sure that it is available in the image or turn off healthcheck on Coolify's UI.`, `----------------------------------------`
  - log `New container is not healthy, rolling back to the old container.`
  - `failDeployment()` → `transitionToStatus(FAILED)` (status `failed`, DeploymentFailed notification, next queued deployment started).
  - `graceful_shutdown_container({cn})` (stop + remove the **new** container). The old container keeps running.
  - **No exception is thrown**: the job continues (`Rolling update completed.`, `post_deployment()`; `completeDeployment()` is then a no-op because the status is terminal; the post-deployment command still runs against the old container).
- Exception: if healthy/force → log hidden stderr `Warning: Could not remove old container: {msg}` and swallow; else throw `DeploymentException("Failed to stop running container: {msg}", code, e)`.

### 5.7 `post_deployment()` (ADJ:619-657)
1. `completeDeployment()` → `transitionToStatus(FINISHED)` (ADJ:6210):
   - `isInTerminalState()`: refresh; `finished`/`failed` → return without doing anything; `cancelled-by-user` → log `Deployment cancelled by user, stopping execution.`, `handleCancelledDeployment()`, throw `DeploymentException('Deployment cancelled by user', 69420)`.
   - `UPDATE application_deployment_queues SET status = 'finished'`.
   - `handleSuccessfulDeployment()` (ADJ:6305), failures logged as `Warning: Post-finished actions failed: {msg}` (stderr):
     - `applications.update(['restart_count' => 0, 'last_restart_at' => null, 'last_restart_type' => null, 'restart_limit_reached' => false])`
     - `application->markDeploymentConfigurationApplied($queue)` (Application.php:1520): refresh app; snapshot = `ApplicationConfigurationSnapshot`; `UPDATE application_deployment_queues SET configuration_hash, configuration_snapshot (encrypted), configuration_diff (vs previous finished deployment with a snapshot, else null)`; `applications.config_hash = hash`.
     - `event(new ApplicationConfigurationChanged(team_id))`.
     - no parent → `deploy_to_additional_destinations()` (0 without `additional_networks`) unless `only_this_server` → `team->notify(new DeploymentSuccess($app, {dep}, null, $queue))`.
   - `queue_next_deployment($app, queue.server_id)` (applications.php:154), failures logged as `Warning: Starting the next queued deployment failed: {msg}`.
2. `GetContainersStatus::dispatch($server)` (updates `applications.status` like `running:healthy` and `last_online_at` asynchronously).
3. `run_post_deployment_command()` (ADJ:6147) if `post_deployment_command` set: log `----------------------------------------`, `Executing post-deployment command (see debug log for output).`; no containers → `Post-deployment command: No running containers found. Skipping.`; container resolution as in 5.3 (`Post-deployment` prefix); `docker exec {name} sh -c '{cmd}'` hidden, save `post-deployment-command-output`; on failure log `Post-deployment command failed.` and the saved output (stderr). A thrown resolution exception is caught by post_deployment and only goes to laravel.log.
4. `is_maintenance_enabled` → `application->syncMaintenancePage()`.

---

## 6. `finally` block of `handle()` (ADJ:421-458)

Runs after success **and** after failure (after `failed()` for Exceptions), each step in its own try/catch:
1. `UPDATE application_deployment_queues SET finished_at = now()` (failure → laravel.log only).
2. not build server → `write_deployment_configurations()` (ADJ:1319); failure → log (stderr) `Warning: Failed to write deployment configurations: {msg}`:
   - preserve-repository only: `mkdir -p {cfg}` + `docker cp {dep}:{workdir}/. {cfg}` + `saveStorageOnServer` for non-host, non-git, non-directory file storages.
   - if `generated_docker_compose` is set (i.e. `generate_compose_file()` ran, even if the deployment failed later): `readme = generate_readme_file(app.name, queue.updated_at)` = `"Resource name: {sanitize_string(name)}\nLatest Deployment Date: {sanitize_string(updated_at 'Y-m-d H:i:s')}"` (`sanitize_string`: strip_tags + htmlspecialchars ENT_QUOTES|ENT_HTML5 + strip control chars + trim, shared.php:110/561). `queue.updated_at` is the in-memory value, i.e. the one set by the `finished_at` update just before.
     ```
     mkdir -p {cfg}
     rm -f {cfg}/docker-compose.yaml {cfg}/README.md        (hidden)
     tee {cfg}/docker-compose.yaml > /dev/null              (input = generated compose YAML)
     echo '{readme}' | tee {cfg}/README.md > /dev/null
     ```
     (`is_raw_compose_deployment_enabled` → `{cfg}` replaced by `application->workdir()`, same path.) So the host config dir holds the compose file of this attempt, written **after** the status became `finished`/`failed`.
3. log `Gracefully shutting down build container: {dep}`; `graceful_shutdown_container({dep}, skipRemove: true)` → `docker stop --time={T} '{dep}'` (hidden; output `{dep}` logged hidden). The helper was started with `--rm`, so stopping removes it.
4. `ServiceStatusChanged::dispatch(team_id)`.

---

## 7. `failed(Throwable $e)` (ADJ:6426-6478)

Runs on a **fresh unserialised job instance** (constructor state from dispatch time; models re-queried; `saved_outputs` empty).
1. `failDeployment()` → `transitionToStatus(FAILED)` (no-op if already `finished`/`failed`): `status = 'failed'`, `DeploymentFailed` notification (or multi-server notification), `queue_next_deployment`.
2. logs (all stderr):
   - `========================================`
   - `TimeoutExceededException`: `Deployment failed: Timeout of {timeout} seconds exceeded. Increase the deployment timeout in the Server settings (Advanced > Builds).`
   - `DeploymentException` with `isMessageAlreadyLogged()`: `Deployment failed.`
   - else `Deployment failed: {message or 'Unknown error occurred'}`
   - hidden: `Error type: {FQCN}`, `Error code: {code}`, `Location: {php file}:{line}`; if previous: hidden `Caused by:`, `  {PrevFQCN}: {prev message or 'No message'}`, `  at {file}:{line}`.
   - `========================================`
3. (not dockercompose) if code !== 69420 and not consistent-name and not PR:
   - new version healthy (cache flag) → log `Deployment failed after the new version became healthy. Keeping the new version of your application running.` (stderr)
   - else → log `Deployment failed. Removing the new version of your application.` (stderr) and `removeContainerWithTimeout({cn})` (`docker rm -f` via the timeout script; the old container is untouched).

Typical wrapped message: `Rolling update failed (App\Exceptions\DeploymentException): Failed to start container: Command execution failed (exit code 1): docker exec {dep} bash -c ' docker compose ... up --build -d'\nError: ...`.

---

## 8. Proxy labels

### 8.1 `generateLabelsApplication($app, null)` (docker.php:1254)
- `ports = settings.is_static ? [80] : ports_exposes_array`; `onlyPort = ports[0] ?? null` (string).
- if `fqdn` set: `domains = explode(',', fqdn)` (no trim).
- `server.settings.generate_exact_labels` (default false):
  - false → `fqdnLabelsForTraefik(...)` **then** `fqdnLabelsForCaddy(...)` merged (both sets on every container).
  - true → only the set for `server.proxyType()` (`TRAEFIK`/`CADDY`).
- Traefik args: `uuid={U}`, `onlyPort`, `is_force_https_enabled = settings.is_force_https_enabled` (default true), `is_permanent_redirect_enabled`, `is_gzip_enabled` (default true), `is_stripprefix_enabled` (default true), `redirect_direction = applications.redirect` (`both` default / `www` / `non-www`), basic auth fields, `noindex_domains`, `domainPortOverrides = applications.domain_port_overrides`, `is_retry_enabled: true`, `certificate_resolver = server.traefikCertificateResolver(settings.traefik_certificate_resolver)` (app override, else `server_settings.traefik_certificate_resolver` default `letsencrypt`; `none`/blank → null, Server.php:1608). In the non-exact branch `escape_redirect_replacement_for_compose: false`.
- Caddy args: `network = destination.network`, same flags, plus traffic analytics / log_append / basic_auth directive support from the server, `is_retry_enabled: true`.
- Returns `$labels->all()`: Traefik labels (sorted) followed by Caddy labels (sorted).

### 8.2 Domain ports
`Application::saving` normalises `fqdn` with `DomainPortOverrides::normalize` (app/Support/DomainPortOverrides.php): **ports are removed from the stored fqdn** and kept in `domain_port_overrides` (`{"https://app.example.com": 8080}`; a www/non-www counterpart inherits the port). The label generators use `$url->getPort() ?? overrides[portless]` and fall back to `onlyPort`. So with ports_exposes set, every domain gets a port label; a port in the domain only changes the value.

### 8.3 `fqdnLabelsForTraefik` (docker.php:966) push order
1. `traefik.enable=true`
2. gzip: `traefik.http.middlewares.gzip-sse.compress.excludedcontenttypes=text/event-stream`
3. `traefik.http.middlewares.redirect-to-https.redirectscheme.scheme=https`
4. permanent redirect: `traefik.http.middlewares.redirect-to-https-permanent.redirectscheme.scheme=https`, `...redirect-to-https-permanent.redirectscheme.permanent=true` (http middleware name becomes `redirect-to-https-permanent`; www redirects get `permanent=true`).
5. basic auth: `traefik.http.middlewares.http-basic-auth-{U}.basicauth.users={user}:{bcrypt hash}` (new hash every generation).
6. retry: `traefik.http.middlewares.retry-{U}.retry.attempts=5`, and `retry-{U}` is appended to every router's middleware list (`middlewares_from_labels`).
7. per domain `{i}` (index in the comma list), `host` = URL host, `traefikHost = trim(host, '[]')`, `path` = URL path (`/` when none), `http-{i}-{U}` / `https-{i}-{U}`:
   - ghost images only: `redir-ghost-{U}` labels.
   - noindex: `traefik.http.middlewares.{i}-{U}-noindex.headers.customresponseheaders.X-Robots-Tag=noindex, nofollow`.
   - **https domain**:
     - ``traefik.http.routers.https-{i}-{U}.rule=Host(`{traefikHost}`) && PathPrefix(`{path}`)``
     - `traefik.http.routers.https-{i}-{U}.entryPoints=https`
     - if port: `traefik.http.routers.https-{i}-{U}.service=https-{i}-{U}`, `traefik.http.services.https-{i}-{U}.loadbalancer.server.port={port}`
     - path != `/` and stripprefix: `traefik.http.middlewares.https-{i}-{U}-stripprefix.stripprefix.prefixes={path}` (and `https-{i}-{U}-stripprefix` first in the middleware list)
     - middleware list order: [stripprefix], `gzip-sse`, [redir-ghost], [to-non-www/to-www with their 3 redirectregex labels merged in], [http-basic-auth-{U}], [noindex], `retry-{U}` → `traefik.http.routers.https-{i}-{U}.middlewares={comma list}`
     - `traefik.http.routers.https-{i}-{U}.tls=true`
     - resolver: `traefik.http.routers.https-{i}-{U}.tls.certresolver={resolver}`
     - ``traefik.http.routers.http-{i}-{U}.rule=Host(`{traefikHost}`) && PathPrefix(`{path}`)``
     - `traefik.http.routers.http-{i}-{U}.entryPoints=http`
     - if port: `traefik.http.services.http-{i}-{U}.loadbalancer.server.port={port}`, `traefik.http.routers.http-{i}-{U}.service=http-{i}-{U}`
     - force https: `traefik.http.routers.http-{i}-{U}.middlewares=[{i}-{U}-noindex,]redirect-to-https` (no gzip/retry); else the same list as the https router.
   - **http domain**: rule, entryPoints=http, port service labels, `[http-{i}-{U}-stripprefix]`, middlewares list as above.
   - www redirect labels: `traefik.http.middlewares.{i}-{U}-to-www.redirectregex.regex=^(http|https)://(?:www\.)?(.+)`, `...replacement=${1}://www.${2}` (`$${1}` when escape_redirect_replacement_for_compose), `...permanent=false|true`; non-www: `...{i}-{U}-to-non-www.redirectregex.regex=^(http|https)://www\.(.+)`, `...replacement=${1}://${2}`.
   - an exception for one domain skips it.
8. `return $labels->sort()` (PHP `asort` SORT_REGULAR = byte order for these strings).

### 8.4 `fqdnLabelsForCaddy` (docker.php:804)
- `caddy_ingress_network={network}`
- per domain `{i}`: site = `caddySiteAddress(scheme, host, force)` (docker.php:795): `https` and not force → `http://{host}, https://{host}`; else `{scheme}://{host}`. handle = `handle_path` (stripprefix) or `handle`.
  - `caddy_{i}={site}`
  - noindex: `caddy_{i}.header.0_-Server=` + `caddy_{i}.header.1_X-Robots-Tag="noindex, nofollow"`; else `caddy_{i}.header=-Server`
  - `caddy_{i}.try_files={path} /index.html /index.php`
  - `caddy_{i}.{handle}.{i}_reverse_proxy={{upstreams {port}}}` (or `{{upstreams}}` without port)
  - retry: `caddy_{i}.{handle}.{i}_reverse_proxy.lb_try_duration=5s`
  - `caddy_{i}.{handle}={path}*`
  - gzip: `caddy_{i}.encode=zstd gzip`
  - redirects (`www`/`non-www`): `caddy_{i}.redir={scheme or {scheme}}://www.{host}{uri}[ permanent]` / `...://{host without www.}{uri}`
  - basic auth, traffic analytics labels (see code).
- `return $labels->sort()`.

### 8.5 `defaultLabels({U}, {U}, project, app, env, 0)` (docker.php:651) - appended last, never `$`-escaped
```
coolify.managed=true
coolify.version={config constants.coolify.version (COOLIFY_VERSION env, default 4.5.1)}
coolify.applicationUuid={U}
coolify.type=application
coolify.name={Str::slug(U)}
coolify.resourceName={Str::slug(app name)}
coolify.projectName={Str::slug(project name)}
coolify.serviceName={Str::slug(app name)}
coolify.environmentName={Str::slug(environment name)}
coolify.pullRequestId=0
```

### 8.6 Worked example
App `{U} = vwk8s4gc0cw8kkw4ow8sgcc0`, `ports_exposes = 80`, fqdn `https://app.example.com,http://api.example.com`, network `coolify`, defaults (force https, gzip, stripprefix, redirect both, resolver letsencrypt, not exact labels). Resulting label list in the compose file (Traefik sorted, Caddy sorted, defaults):
```
traefik.enable=true
traefik.http.middlewares.gzip-sse.compress.excludedcontenttypes=text/event-stream
traefik.http.middlewares.redirect-to-https.redirectscheme.scheme=https
traefik.http.middlewares.retry-vwk8s4gc0cw8kkw4ow8sgcc0.retry.attempts=5
traefik.http.routers.http-0-vwk8s4gc0cw8kkw4ow8sgcc0.entryPoints=http
traefik.http.routers.http-0-vwk8s4gc0cw8kkw4ow8sgcc0.middlewares=redirect-to-https
traefik.http.routers.http-0-vwk8s4gc0cw8kkw4ow8sgcc0.rule=Host(`app.example.com`) && PathPrefix(`/`)
traefik.http.routers.http-0-vwk8s4gc0cw8kkw4ow8sgcc0.service=http-0-vwk8s4gc0cw8kkw4ow8sgcc0
traefik.http.routers.http-1-vwk8s4gc0cw8kkw4ow8sgcc0.entryPoints=http
traefik.http.routers.http-1-vwk8s4gc0cw8kkw4ow8sgcc0.middlewares=gzip-sse,retry-vwk8s4gc0cw8kkw4ow8sgcc0
traefik.http.routers.http-1-vwk8s4gc0cw8kkw4ow8sgcc0.rule=Host(`api.example.com`) && PathPrefix(`/`)
traefik.http.routers.http-1-vwk8s4gc0cw8kkw4ow8sgcc0.service=http-1-vwk8s4gc0cw8kkw4ow8sgcc0
traefik.http.routers.https-0-vwk8s4gc0cw8kkw4ow8sgcc0.entryPoints=https
traefik.http.routers.https-0-vwk8s4gc0cw8kkw4ow8sgcc0.middlewares=gzip-sse,retry-vwk8s4gc0cw8kkw4ow8sgcc0
traefik.http.routers.https-0-vwk8s4gc0cw8kkw4ow8sgcc0.rule=Host(`app.example.com`) && PathPrefix(`/`)
traefik.http.routers.https-0-vwk8s4gc0cw8kkw4ow8sgcc0.service=https-0-vwk8s4gc0cw8kkw4ow8sgcc0
traefik.http.routers.https-0-vwk8s4gc0cw8kkw4ow8sgcc0.tls.certresolver=letsencrypt
traefik.http.routers.https-0-vwk8s4gc0cw8kkw4ow8sgcc0.tls=true
traefik.http.services.http-0-vwk8s4gc0cw8kkw4ow8sgcc0.loadbalancer.server.port=80
traefik.http.services.http-1-vwk8s4gc0cw8kkw4ow8sgcc0.loadbalancer.server.port=80
traefik.http.services.https-0-vwk8s4gc0cw8kkw4ow8sgcc0.loadbalancer.server.port=80
caddy_0.encode=zstd gzip
caddy_0.handle_path.0_reverse_proxy.lb_try_duration=5s
caddy_0.handle_path.0_reverse_proxy={{upstreams 80}}
caddy_0.handle_path=/*
caddy_0.header=-Server
caddy_0.try_files={path} /index.html /index.php
caddy_0=https://app.example.com
caddy_1.encode=zstd gzip
caddy_1.handle_path.1_reverse_proxy.lb_try_duration=5s
caddy_1.handle_path.1_reverse_proxy={{upstreams 80}}
caddy_1.handle_path=/*
caddy_1.header=-Server
caddy_1.try_files={path} /index.html /index.php
caddy_1=http://api.example.com
caddy_ingress_network=coolify
coolify.managed=true
coolify.version=4.5.1
coolify.applicationUuid=vwk8s4gc0cw8kkw4ow8sgcc0
coolify.type=application
coolify.name=vwk8s4gc0cw8kkw4ow8sgcc0
coolify.resourceName=my-app
coolify.projectName=my-project
coolify.serviceName=my-app
coolify.environmentName=production
coolify.pullRequestId=0
```
With the https domain entered as `https://app.example.com:8080` (stored as `https://app.example.com` + override 8080) the only changes are `traefik.http.services.http-0-...port=8080`, `traefik.http.services.https-0-...port=8080` and `caddy_0.handle_path.0_reverse_proxy={{upstreams 8080}}` (the http domain keeps `80` from ports_exposes). Note the sort puts `.tls.certresolver` before `.tls=true` (`.` < `=`) and `entryPoints` before `middlewares`/`rule`/`service`.
With force-https off: `caddy_0=http://app.example.com, https://app.example.com` and the http-0 router gets `middlewares=gzip-sse,retry-{U}`.

---

## 9. Runtime environment (`generate_runtime_environment_variables`, ADJ:1855) and `generate_coolify_env_variables` (ADJ:4194)

Order of lines in the `.env` (joined with `\n`, no trailing newline):

**A. Coolify variables** (`generate_coolify_env_variables(forBuildTime: false)`, pr 0 branch ADJ:4245-4292), each rendered as `KEY=` + `escapeComposeEnvFileValue(value)` (docker.php:2059: `\` → `\\`, `"` → `\"`, `$` → `\$`, wrapped in `"`; null → `""`). "User-defined" checks use **all** non-preview env vars of the app (also build-time-only ones):
1. `SOURCE_COMMIT` = job commit (e.g. `HEAD`; on rollback the image tag) - unless user defines `SOURCE_COMMIT`.
2. compose_parsing_version >= 3 (new apps get `'5'`, Application.php:147/401):
   - unless user defines **`COOLIFY_FQDN`**: `COOLIFY_URL` = `applications.fqdn` raw (comma-separated full URLs, ports already stripped by normalisation; null → `""`).
   - unless user defines **`COOLIFY_URL`**: `COOLIFY_FQDN` = `hostsFromFqdn(fqdn)` = comma-joined hosts of valid http(s) URLs (ADJ:3302).
   - parsing version < 3: `COOLIFY_FQDN` = fqdn, `COOLIFY_URL` = hosts (legacy, same guard keys).
3. `COOLIFY_BRANCH` = `$this->branch` → **always null in the dockerimage path** (only set by git import) → `COOLIFY_BRANCH=""`.
4. `COOLIFY_RESOURCE_UUID` = `{U}`.
5. `COOLIFY_CONTAINER_NAME` = `{cn}`.
6. `coolifyDefaultEnvironmentVariables($app, user keys, mainServer)` (shared.php:4551), unless `settings.include_coolify_default_variables === false`: `COOLIFY_APP_NAME`, `COOLIFY_PROJECT_NAME`, `COOLIFY_ENVIRONMENT_NAME`, `COOLIFY_SERVER_IP` (null values dropped; user-defined keys skipped).

**B. User runtime variables**: `runtime_environment_variables` (non-preview, excluding `NIXPACKS_*`/`RAILPACK_*` except `NIXPACKS_PHP_ROOT_DIR`, `NIXPACKS_PHP_FALLBACK_PATH`, `NIXPACKS_SPA_OUTPUT_DIR`, `RAILPACK_SKIP_MIGRATIONS`), sorted by `key` if `settings.is_env_sorting_enabled` (default false) else by `id`; keep `is_runtime`; stable re-sort putting values that start with `$SERVICE_` or contain `${SERVICE_` last. Each line = `runtimeEnvironmentAssignment()` (ADJ:2022):
- `uses_legacy_escaping` true (saved before exact escaping): `KEY=` + `getResolvedValueWithServer(mainServer)` (EnvironmentVariable.php:336): shared-variable references `{{team|project|environment|server.X}}` resolved; JSON object/array → raw; literal/multiline → `'value'`; else `escapeEnvVariables` (`\`→`\\`, CR→`\r`, TAB→`\t`, NUL→`\0`, `"`→`\"`, `'`→`\'`), **no surrounding quotes**. Remote secret references (`{{vault.KEY}}`) → substituted and formatted with `RemoteSecretValueFormatter::dotenv`.
- exact escaping: `KEY=` + `escapeComposeEnvFileValue(resolved, allowInterpolation)`, where interpolation (unescaped `$`) is allowed only for plain values (not JSON, not literal, not multiline, not secret-manager). Remote-secret references → `escapeComposeEnvFileValue(substituted)`.
- Values are stored Laravel-encrypted and `trim()`med on read (EnvironmentVariable.php:679).
- Missing remote secrets: log (stderr) `Missing secret keys: {k} (referenced by {KEY}). Check the secret manager source of this application.` and fail; fetch failures log `Failed to fetch secrets from {provider} ({token}, {source}): {msg}`; success logs `Fetched {n} secrets from {provider} ({token}, {source}).`

**C. PORT/HOST**:
- If `settings.is_container_label_readonly_enabled` and the user `PORT` value is numeric and not in ports_exposes: log (stderr) `Warning: PORT environment variable ({p}) does not match configured ports_exposes: {a,b}. It could case "bad gateway" or "no server" errors. Check the "General" page to fix it.`
- `PORT={ports[0]}` (unquoted) if no user var named `PORT` and ports non-empty.
- `HOST=0.0.0.0` (unquoted) if no user var named `HOST`.

Example `.env` (app above, no user variables, parsing version 5, include defaults on):
```
SOURCE_COMMIT="HEAD"
COOLIFY_URL="https://app.example.com,http://api.example.com"
COOLIFY_FQDN="app.example.com,api.example.com"
COOLIFY_BRANCH=""
COOLIFY_RESOURCE_UUID="vwk8s4gc0cw8kkw4ow8sgcc0"
COOLIFY_CONTAINER_NAME="vwk8s4gc0cw8kkw4ow8sgcc0-20261028T101500"
COOLIFY_APP_NAME="My App"
COOLIFY_PROJECT_NAME="My Project"
COOLIFY_ENVIRONMENT_NAME="production"
COOLIFY_SERVER_IP="203.0.113.10"
PORT=80
HOST=0.0.0.0
```

---

## 10. Generated compose file (example)

Rendered with Symfony Yaml 7.4 (Coolify locks 8.1.8; same output expected for these types) for the app above, health check enabled with defaults (interval 5, timeout 5, retries 10, start 5 per testing schema), no volumes/custom options:
```yaml
services:
    vwk8s4gc0cw8kkw4ow8sgcc0-20261028T101500:
        image: 'nginx:1.27-alpine'
        container_name: vwk8s4gc0cw8kkw4ow8sgcc0-20261028T101500
        restart: unless-stopped
        expose:
            - '80'
        networks:
            coolify:
                aliases:
                    - vwk8s4gc0cw8kkw4ow8sgcc0-20261028T101500
        mem_limit: '0'
        memswap_limit: '0'
        mem_swappiness: 60
        mem_reservation: '0'
        cpus: 0.0
        cpu_shares: 1024
        env_file:
            - .env
        healthcheck:
            test:
                - CMD-SHELL
                - "curl -s -X 'GET' -f 'http://localhost:80/' > /dev/null || wget -q -O- 'http://localhost:80/' > /dev/null || exit 1"
            interval: 5s
            timeout: 5s
            retries: 10
            start_period: 5s
        labels:
            - traefik.enable=true
            - traefik.http.middlewares.gzip-sse.compress.excludedcontenttypes=text/event-stream
            - traefik.http.middlewares.redirect-to-https.redirectscheme.scheme=https
            - traefik.http.middlewares.retry-vwk8s4gc0cw8kkw4ow8sgcc0.retry.attempts=5
            - traefik.http.routers.http-0-vwk8s4gc0cw8kkw4ow8sgcc0.entryPoints=http
            - traefik.http.routers.http-0-vwk8s4gc0cw8kkw4ow8sgcc0.middlewares=redirect-to-https
            - 'traefik.http.routers.http-0-vwk8s4gc0cw8kkw4ow8sgcc0.rule=Host(`app.example.com`) && PathPrefix(`/`)'
            - traefik.http.routers.http-0-vwk8s4gc0cw8kkw4ow8sgcc0.service=http-0-vwk8s4gc0cw8kkw4ow8sgcc0
            - traefik.http.routers.http-1-vwk8s4gc0cw8kkw4ow8sgcc0.entryPoints=http
            - 'traefik.http.routers.http-1-vwk8s4gc0cw8kkw4ow8sgcc0.middlewares=gzip-sse,retry-vwk8s4gc0cw8kkw4ow8sgcc0'
            - 'traefik.http.routers.http-1-vwk8s4gc0cw8kkw4ow8sgcc0.rule=Host(`api.example.com`) && PathPrefix(`/`)'
            - traefik.http.routers.http-1-vwk8s4gc0cw8kkw4ow8sgcc0.service=http-1-vwk8s4gc0cw8kkw4ow8sgcc0
            - traefik.http.routers.https-0-vwk8s4gc0cw8kkw4ow8sgcc0.entryPoints=https
            - 'traefik.http.routers.https-0-vwk8s4gc0cw8kkw4ow8sgcc0.middlewares=gzip-sse,retry-vwk8s4gc0cw8kkw4ow8sgcc0'
            - 'traefik.http.routers.https-0-vwk8s4gc0cw8kkw4ow8sgcc0.rule=Host(`app.example.com`) && PathPrefix(`/`)'
            - traefik.http.routers.https-0-vwk8s4gc0cw8kkw4ow8sgcc0.service=https-0-vwk8s4gc0cw8kkw4ow8sgcc0
            - traefik.http.routers.https-0-vwk8s4gc0cw8kkw4ow8sgcc0.tls.certresolver=letsencrypt
            - traefik.http.routers.https-0-vwk8s4gc0cw8kkw4ow8sgcc0.tls=true
            - traefik.http.services.http-0-vwk8s4gc0cw8kkw4ow8sgcc0.loadbalancer.server.port=80
            - traefik.http.services.http-1-vwk8s4gc0cw8kkw4ow8sgcc0.loadbalancer.server.port=80
            - traefik.http.services.https-0-vwk8s4gc0cw8kkw4ow8sgcc0.loadbalancer.server.port=80
            - 'caddy_0.encode=zstd gzip'
            - caddy_0.handle_path.0_reverse_proxy.lb_try_duration=5s
            - 'caddy_0.handle_path.0_reverse_proxy={{upstreams 80}}'
            - 'caddy_0.handle_path=/*'
            - caddy_0.header=-Server
            - 'caddy_0.try_files={path} /index.html /index.php'
            - 'caddy_0=https://app.example.com'
            - 'caddy_1.encode=zstd gzip'
            - caddy_1.handle_path.1_reverse_proxy.lb_try_duration=5s
            - 'caddy_1.handle_path.1_reverse_proxy={{upstreams 80}}'
            - 'caddy_1.handle_path=/*'
            - caddy_1.header=-Server
            - 'caddy_1.try_files={path} /index.html /index.php'
            - 'caddy_1=http://api.example.com'
            - caddy_ingress_network=coolify
            - coolify.managed=true
            - coolify.version=4.5.1
            - coolify.applicationUuid=vwk8s4gc0cw8kkw4ow8sgcc0
            - coolify.type=application
            - coolify.name=vwk8s4gc0cw8kkw4ow8sgcc0
            - coolify.resourceName=my-app
            - coolify.projectName=my-project
            - coolify.serviceName=my-app
            - coolify.environmentName=production
            - coolify.pullRequestId=0
networks:
    coolify:
        external: true
        name: coolify
        attachable: true
```
Symfony quoting rules that matter: strings with space, `:`, `{`, `}`, `[`, `]`, `,`, `&`, `*`, `#`, `?`, quotes, or starting with `- ? | < > = ! % @` backtick → single-quoted (`'` doubled); control chars → double-quoted with escapes; numeric strings (`'0'`, `'80'`) quoted; floats always with a decimal (`0.0`, `2.0`); bools `true`/`false`; `null` → `null`. Labels containing a single quote are therefore single-quoted with `''` escaping; labels whose custom text contains newlines cannot occur (split on newlines).

---

## 11. Typical log transcript (default settings, health check disabled, root, Docker 27, config.json present, one old container)

| # | output | type | hidden | command |
|---|---|---|---|---|
| 1 | `Docker 27.3.1 with BuildKit and Buildx detected on deployment server (localhost).` | stdout | no | null |
| 2 | `Starting deployment of nginx:1.27-alpine to localhost.` | stdout | no | null |
| 3 | `Preparing container with helper image: docker.io/coollabsio/coolify-helper:1.0.19` | stdout | no | null |
| 4 | `Error response from daemon: No such container: {dep}` | stderr | yes | `docker stop --time=30 '{dep}'` |
| 5 | `{helper container id}` | stdout | yes | `docker run -d --network 'coolify' --name {dep}  --rm ...` |
| 6 | `Creating .env file with runtime variables for container.` | stdout | yes | null |
| 7 | `----------------------------------------` | stdout | no | null |
| 8 | `Rolling update started.` | stdout | no | null |
| 9 | `Pulling latest images from the registry.` | stdout | no | null |
| 10.. | compose pull progress lines (one entry per output chunk) | stderr | yes | `docker exec {dep} bash -c 'docker compose --project-name {U} --project-directory /artifacts/{dep} pull'` |
| .. | compose up progress (` Container {cn}  Creating` ...) | stderr | yes | `docker exec {dep} bash -c ' docker compose ... up --build -d'` |
| | `New container started.` | stdout | no | null |
| | `Waiting 5 seconds for the proxy to send traffic to the new container.` | stdout | no | null |
| | `Removing old containers.` | stdout | no | null |
| | `{old cn}` | stdout | yes | `docker stop --time=30 '{old cn}'` |
| | `{old cn}` | stdout | yes | `bash -c '...docker rm -f ...'` |
| | `Rolling update completed.` | stdout | no | null |
| | `Gracefully shutting down build container: {dep}` | stdout | no | null |
| | `{dep}` | stdout | yes | `docker stop --time=30 '{dep}'` |

(No log entries for commands without output, e.g. `mkdir`, `tee`, `touch`, `docker network connect ... >/dev/null`.)

---

## 12. DB writes (in order, success path)

| Table | Write | Where |
|---|---|---|
| application_deployment_queues | `status='in_progress'`, `horizon_job_worker=gethostname()` | handle ADJ:379 |
| server_settings | `docker_version`, `docker_version_checked_at` | detectBuildKitCapabilities ADJ:528 |
| application_deployment_queues | `logs` (+`updated_at`) per log entry | addLogEntry |
| application_deployment_queues | `current_process_id` per executed command | ERC:201 |
| applications | `custom_labels` (only if normalised value changed) | parseContainerLabels / ADJ:4379 |
| applications | `status='running'` (+`last_online_at=now()` if changed) only when the health check reports healthy | ADJ:2971 |
| application_deployment_queues | `status='finished'` | transitionToStatus |
| applications | `restart_count=0, last_restart_at=null, last_restart_type=null, restart_limit_reached=false` | handleSuccessfulDeployment |
| application_deployment_queues / applications | `configuration_hash/_snapshot/_diff` / `config_hash` | markDeploymentConfigurationApplied |
| notifications | `DeploymentSuccess` to the team | sendDeploymentNotification |
| application_deployment_queues | next queued deployment → `in_progress` + dispatch | queue_next_deployment |
| application_deployment_queues | `finished_at=now()` | finally ADJ:425 |
| cache | `deployment-new-version-healthy:{dep}` (1 day) | markNewVersionHealthy |

Failure path: `status='failed'` (via `failed()` or the unhealthy branch), DeploymentFailed notification, `finished_at`.
Events: `ApplicationConfigurationChanged` (success), `ServiceStatusChanged` (always, finally), job `GetContainersStatus` (success path).

---

## 13. Functions NOT executed in this path
`set_coolify_variables` (so `$coolify_variables` is null), `pull_latest_image`, `push_to_docker_registry`, `generate_buildtime_environment_variables`/`save_buildtime_environment_variables`, `generate_env_variables`, `generate_docker_env_flags_for_secrets` (returns '' early), `check_git_if_build_needed`, `clone_repository`, `build_image`, `just_restart`, `should_skip_build`, `restart_builder_container_with_actual_commit`, Swarm branches, `deploy_to_additional_destinations` (returns 0).
