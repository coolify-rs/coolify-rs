# Application lifecycle outside the deployment job (standalone Docker, single server, Traefik)

Source: `.upstream/snapshot` (Coolify main @ b5823c7, `constants.coolify.version` = `4.5.1`).
All paths below are relative to the snapshot. Strings are verbatim. `U` = application uuid,
`D` = deployment uuid, `S` = server.

---

## 0. Shared plumbing you must reproduce first

### 0.1 How a "command list" reaches the server — `instant_remote_process`
`bootstrap/helpers/remoteProcess.php:219-254`

- `instant_remote_process(array $command, Server $server, bool $throwError = true, bool $no_sudo = false, ?int $timeout = null, ...)`.
- Non-root server and `! $no_sudo` → every line is rewritten by `parseCommandsByLineForSudo` (`bootstrap/helpers/sudo.php:96`).
- Lines are joined with `"\n"` into ONE script (`implode("\n", $command)`), sent as a heredoc to ONE ssh call
  (`app/Helpers/SshMultiplexingHelper.php:243-253`):
  `timeout <T> ssh <opts> <user@host> 'sh -c "if command -v bash >/dev/null 2>&1; then exec bash -se; else exec sh -se; fi"' << \<delimiter>`
  (`remoteShellCommand()` at `SshMultiplexingHelper.php:303`). `-e` ⇒ **the first failing line aborts the rest of the list**.
  `<T>` = `$timeout ?? config('constants.ssh.command_timeout')` (default 3600; `config/constants.php:96`).
- Output: `trim(stdout)`; the literal string `null` becomes PHP `null`.
- Exit code ≠ 0 → `excludeCertainErrors()` (`remoteProcess.php:259-267`) throws `RuntimeException(trim(stderr))`, or
  `"SSH command failed with exit code: $exitCode"` when stderr is empty. With `$throwError=false`, `SshRetryHandler::retry`
  swallows it and returns `null`. Retries (`SshRetryHandler`, `constants.ssh.max_retries`=3) apply only to SSH-level errors.
- `instant_remote_process_with_timeout` (`remoteProcess.php:185-217`) is the same but the local process timeout is 30 s.

### 0.2 Finding an application's containers — `getCurrentApplicationContainerStatus`
`bootstrap/helpers/docker.php:279-325`, `dockerPsByOwnerCommands` `:234-251`, `containersOwnedBy` `:258-263`.

One `instant_remote_process` (throwError **true**) with three lines (filters are `escapeshellarg`ed):
```
docker ps -a --filter 'label=coolify.applicationUuid=U' --format '{{json .}}'
docker ps -a --filter 'label=coolify.applicationId' --filter 'label=com.docker.compose.project=U' --format '{{json .}}'
docker ps -a --filter 'label=coolify.applicationId' --filter 'name=U' --format '{{json .}}'
```
Output parsed by `format_docker_command_output_to_json` (`docker.php:364-380`: split on `PHP_EOL`, drop empty lines,
`json_decode` each line with JSON_THROW_ON_ERROR; **any** bad line ⇒ empty collection), then `->filter()->unique('ID')->values()`.

Per container (the `docker ps` JSON, so `Labels` is a comma-joined string and `Names` has no leading `/`):
- `hasPrLabel = Labels contains "coolify.pullRequestId="`; `prLabelValue` = `/coolify\.pullRequestId=(\d+)/`.
- `isBaseDeploy = !hasPrLabel || (int)prLabelValue === 0`.
- `$pullRequestId` non-null and ≠ 0 and base → excluded.
- base → included. `$includePullrequests` → included. `(int)prLabelValue === $pullRequestId` → included. Else excluded.
- Swarm server → always returns an empty collection.

So: `getCurrentApplicationContainerStatus(S, app, 0)` = base containers only;
`(S, app, includePullrequests: true)` = base + every preview container; `(S, app, N)` = PR N only.

### 0.3 `dockerStopCommand` / `dockerRemoveCommand`
`bootstrap/helpers/docker.php:462-508`, `escapeShellValue` `:524-527`.

- `dockerStopCommand(int $timeout, string $containers, Server $s)` →
  `docker stop --timeout=<t> <containers>` if `server_settings.docker_version` (normalised `X.Y.Z`) `>= 28.0.0`,
  otherwise (including NULL / unparsable version) `docker stop --time=<t> <containers>`. `$containers` is inserted as given
  (callers pass an already `escapeshellarg`ed name). Dev only: `Log::info('docker stop command', ...)`.
- `dockerRemoveCommand($c)` → `output=$(docker rm -f '<c>' 2>&1); exit_code=$?; if [ "$exit_code" -ne 0 ] && printf '%s' "$output" | grep -Eq 'No such container:'; then exit 0; fi; if [ "$exit_code" -ne 0 ]; then printf '%s\n' "$output" >&2; else printf '%s\n' "$output"; fi; exit $exit_code`
  (`dockerCommandIgnoringError`, `docker.php:517-521`).
- Stop grace period: `ApplicationSetting::stopGracePeriodSeconds()` (`app/Models/ApplicationSetting.php:162-172`):
  `stop_grace_period` if `1 <= v <= 3600` else **30** (`bootstrap/helpers/constants.php:63-65`).
  Deployment variant `deploymentStopGracePeriodSeconds()` (`:179-186`): in dev with NULL → 1, else same.

### 0.4 `Server::isFunctional()` (`app/Models/Server.php:2163-2172`)
`settings.is_reachable && settings.is_usable && settings.force_disabled === false && !hasPlaceholderIp()`.
Side effect when false: deletes the SSH mux file (`Storage::disk('ssh-mux')->delete($this->muxFilename())`).

---

## 1. Stop — `App\Actions\Application\StopApplication`
`app/Actions/Application/StopApplication.php`

Signature: `handle(Application $application, bool $previewDeployments = false, bool $dockerCleanup = true, bool $resetRestartCount = true, bool $removeContainers = true): ?string`.
Queued runs go to queue `'high'` (`$jobQueue`). Returns `null` on success, or the joined error string.

### 1.1 Callers
| Caller | Call | file:line |
|---|---|---|
| UI "Stop" button | `StopApplication::dispatch($app, false, $this->docker_cleanup)` (`docker_cleanup` default `true`) | `app/Livewire/Project/Application/Heading.php:229-244` |
| API `POST /api/v1/applications/{uuid}/stop` | `StopApplication::dispatch($app, false, $request->boolean('docker_cleanup', true))` | `app/Http/Controllers/Api/ApplicationsController.php:4752-4784` |
| `DeleteResourceJob` | `StopApplication::run($app, previewDeployments: true, dockerCleanup: $this->dockerCleanup)` (sync) | `app/Jobs/DeleteResourceJob.php:71` |
| Restart-limit hit | `dispatch(application:, previewDeployments: false, dockerCleanup: false, resetRestartCount: false, removeContainers: false)` | `app/Actions/Docker/GetContainersStatus.php:534-540`, `app/Jobs/PushServerUpdateJob.php:619-625` |
| Migrate resource | `StopApplication::run($resource, previewDeployments: false, dockerCleanup: false)` | `app/Actions/Shared/MigrateResourceToDestination.php:197` |
| MCP control | `StopApplication::dispatch($application, false, true)` | `app/Mcp/Tools/Control.php:117` |

UI side: before dispatch the Heading sends toast `info` `Gracefully stopping application.<br/>It could take a while depending on the application.`
then `auditLog('ui.application.stopped', ['team_id', 'application_uuid', 'application_name'])`.
API: `auditLog('api.application.stopped', [... 'docker_cleanup' => $dockerCleanup])`, response 200
`{"message":"Application stopping request queued."}`. Errors: 400 `{"message":"UUID is required."}`, 404
`{"message":"Application not found."}`. `GET .../stop` → `OtherController::post_required` (`routes/api.php:295`).
Abilities: `api.ability:deploy`; policy `deploy`.

### 1.2 Algorithm (exact order)
1. `$containerPresent = ! $removeContainers`.
2. `$servers = [destination.server] + additional_servers` (single-server port: just the one).
3. For each server (errors accumulate in `$errors[]`, never thrown):
   1. `! $server->isFunctional()` → `$errors[] = "Server {$server->name} is not functional."`; continue.
   2. Swarm → `$containerPresent = false`; `docker stack rm '<U>'`; continue. (Not relevant to standalone.)
   3. `$containers = $previewDeployments ? getCurrentApplicationContainerStatus($server, $app, includePullrequests: true) : getCurrentApplicationContainerStatus($server, $app, 0)`.
      An SSH error here is caught by the outer `try` and becomes one error string.
   4. `$timeout = settings->stopGracePeriodSeconds()`.
   5. For each `Names` value, ONE `instant_remote_process` (throwError true) per container:
      ```
      docker stop --time=30 '<name>'      # or --timeout=30 on Docker >= 28
      docker rm -f '<name>'                # only when $removeContainers
      ```
      Note: plain `docker rm -f`, NOT `dockerRemoveCommand`. Because the script runs under `bash -se`, a failing
      `docker stop` (e.g. container vanished) aborts before `docker rm -f`; the exception message (stderr, e.g.
      `Error response from daemon: No such container: ...`) is appended to `$errors`.
   6. `$removeContainers && build_pack === 'dockercompose'` → `$app->deleteConnectedNetworks()` (see §3.4).
   7. `$dockerCleanup` → `CleanupDocker::dispatchAfterStop($server)` (§1.4).
4. `$teamId = $application->environment->project->team->id`.
5. If `$errors !== []`: `$message = implode(' ', $errors)`;
   `Log::warning('Failed to stop application', ['application_uuid' => U, 'error' => $message])`;
   `ApplicationStopFailed::dispatch($teamId, U, $message)`; **return `$message` — no DB write, no ServiceStatusChanged.**
6. Else `$application->update([...])` with
   `['status' => 'exited', 'container_present' => $containerPresent]` plus, when `$resetRestartCount`,
   `['restart_count' => 0, 'last_restart_at' => null, 'last_restart_type' => null, 'restart_limit_reached' => false]`.
   The `status` mutator turns `'exited'` into **`exited:unhealthy`** (§4.5); the `saving` hook sets
   `last_online_at = now()` iff status actually changed (§4.5).
7. `ServiceStatusChanged::dispatch($teamId)`; return `null`.

Zero containers found ⇒ no remote stop commands, still success ⇒ status written `exited:unhealthy`.

### 1.3 Events
- `App\Events\ApplicationStopFailed` (`app/Events/ApplicationStopFailed.php`): `ShouldBroadcastNow`, private channel
  `team.{teamId}`, payload `teamId`, `applicationUuid`, `message`. The Heading listener
  (`Heading.php:130-139`) ignores events for other uuids, else toasts error `Failed to stop the application.` with the message.
- `App\Events\ServiceStatusChanged`: `ShouldBroadcast` (queued), channel `team.{teamId}` (`app/Traits/BroadcastsToTeam.php`),
  no channel when teamId null. Heading listens and calls `checkStatus()` → `GetContainersStatus::dispatch($server)` if the
  server is functional, else toast error `Server is not functional.` (`Heading.php:118-125`). So a browser tab open on the
  app is what triggers the post-stop status refresh.

### 1.4 `CleanupDocker::dispatchAfterStop` (`app/Actions/Server/CleanupDocker.php:107-114`)
- If cache key `docker-cleanup-last-run-<server uuid>` exists → nothing.
- Else `CleanupDocker::dispatch($server, false, false, true)`: queue `maintenance_queue()` (`'high'` self-hosted), delay 60 s,
  unique per server uuid until processing (`jobUniqueFor` 300), middleware `WithoutOverlapping('docker-cleanup-<server uuid>')->shared()->expireAfter(660)->dontRelease()`, `jobTries` 1, timeout 600.
- `handle(..., onlyAboveThreshold: true)` (`:140-207`): skip when a deployment (queued/in_progress, `updated_at` within
  `server_settings.dynamic_timeout ?? 3600` s, server_id or build_server_id = S) exists; skip when
  `df / --output=pcent | tr -cd 0-9` < `server_settings.docker_cleanup_threshold`. Otherwise runs (each command separately,
  throwError false, shared 540 s deadline → `Docker cleanup did not finish within 540 seconds. The remaining cleanup commands were not run.`):
  application image retention (`cleanupApplicationImages`, keeps `docker_images_to_keep ?? 2` per repo, `pr-*` always deleted),
  container prune (`:233-235`), image prune (`:260-306`), `docker builder prune -af`, railpack buildx prune, old helper/realtime/sentinel
  image removal (`:244-252`). Then `Cache::put('docker-cleanup-last-run-<uuid>', true, 3600)`.

### 1.5 Sibling actions
- `StopApplicationOneServer` (`app/Actions/Application/StopApplicationOneServer.php`): used by Destination tab
  (stop on one server / remove additional server). Swarm → return. Not functional → returns `'Server is not functional'`.
  Base containers only; per container one call: `dockerStopCommand(t, '<name>', S)` + `dockerRemoveCommand(<name>)`.
  **No status write, no events.** Exception → returns its message.
- `StopApplicationPreview` (`StopApplicationPreview.php`): containers of PR N; per container
  `[dockerStopCommand, "docker rm -f '<name>'" (if removeContainer)]` with **throwError false**; then
  `$preview->update(['status' => 'exited'])` (preview model has no mutator → raw `exited`), `resetRestartLimit()` if asked,
  `ServiceStatusChanged`.
- Note: `StopApplication` with `previewDeployments: true` stops preview containers but never updates `application_previews.status`.

---

## 2. Restart

There is **no** `RestartApplication` action and no `docker restart` for applications. Restart = a deployment with
`restart_only = true`.

### 2.1 Entry points
- UI `Heading::restart()` (`Heading.php:246-283`): authorize `deploy`; if `additional_servers` and no
  `docker_registry_image_name` → error toast (multi-server only). `deployment_uuid = new_public_id()`;
  `queue_application_deployment(application:, deployment_uuid:, restart_only: true)`.
  Results: `queue_full` → error `Deployment queue full` + message; `skipped` → **success** toast `Deployment skipped` + message;
  else redirect to `project.application.deployment.show`.
- API `POST /applications/{uuid}/restart` (`ApplicationsController.php:4838-4882`): same with `is_api: true`.
  `skipped` → 200 `{"message": "<result message>"}` (no audit). Otherwise `auditLog('api.application.restarted', [team_id, application_uuid, application_name, deployment_uuid])`
  and 200 `{"message":"Restart request queued.","deployment_uuid":"<D>"}`. Note `queue_full` is NOT special-cased in the API
  (falls through to "Restart request queued." with the uuid of a row that was never created).
- `queue_application_deployment` (`bootstrap/helpers/applications.php:19-128`): commit = `git_commit_sha ?: 'HEAD'`;
  in a transaction locking application + server rows: queue limit `server_settings.deployment_queue_limit ?? 25` QUEUED rows
  → `['status'=>'queue_full','message'=>'Deployment queue is full. Please wait for existing deployments to complete.']`;
  existing QUEUED/IN_PROGRESS row with same application/commit/pull_request_id/docker_registry_image_tag (and not force/rollback/
  no_questions_asked) → `['status'=>'skipped','message'=>'Deployment already queued for this commit.', ...]`. Else creates the
  `application_deployment_queues` row with `restart_only => true`. UI-auth (not webhook/api/rollback) audit
  `ui.application.restarted`.

### 2.2 Inside `ApplicationDeploymentJob`
- `app/Jobs/ApplicationDeploymentJob.php:259-263`: `restart_only` is **forced false** for build packs
  `dockerimage`, `dockerfile`, `dockercompose` → those run a full regular deployment (which itself skips the build when the
  image for the commit exists).
- `decide_what_to_do()` (`:591-617`) → `just_restart()` (`:1519-1539`):
  1. log `Restarting {customRepository}:{git_branch} on {server name}.`
  2. temporarily `use_build_server = false`.
  3. `prepare_builder_image()` (§6.3), `check_git_if_build_needed()` (git ls-remote in helper, may update `commit`),
     `generate_image_names()` (`U:<commit[0:128]>` / `U:<commit>-build`, or `<docker_registry_image_name>:...`),
     `check_image_locally_or_remotely()` (`:1698-1717`: `docker images -q <image> 2>/dev/null`; if empty and registry image name
     set → log `Image (<img>) not found on the server. Pulling it from the Docker registry.`, `docker pull '<img>' 2>/dev/null`, re-check).
  4. restore `use_build_server`; `should_skip_build()` (`:1541-1603`): image found and no build-relevant config diff →
     log `No build configuration changed & image found (<img>) with the same Git Commit SHA. Build step skipped.`,
     `generate_compose_file()`, save runtime env, `rolling_update()`. Image missing → log `Image not found (<img>). Building new image.`,
     `restart_only=false`, maybe select build server, recurse into `decide_what_to_do()` (full build + `post_deployment()`).
  5. `completeDeployment()` (status FINISHED). **`just_restart` does not call `post_deployment()`**, so the skip-build restart
     path does NOT dispatch `GetContainersStatus` at the end (the rebuild path does, via `post_deployment`).
- `push_to_docker_registry()` returns early when `restart_only` (`:1389`).

---

## 3. Delete

### 3.1 API `DELETE /api/v1/applications/{uuid}` (`ApplicationsController.php:3125-3163`)
Middleware `api.ability:write` + `api.ability:deploy` (`routes/api.php:257`), policy `delete`.
Query booleans (all default `true`): `delete_configurations`, `delete_volumes`, `docker_cleanup`, `delete_connected_networks`
(read with `$request->boolean(name, true)`).
1. no uuid → **404** `{"message":"UUID is required."}`; not found → 404 `{"message":"Application not found"}` (no trailing period).
2. `$application->delete()` — **soft delete** (`deleted_at`), fires `deleting`/`deleted` (§3.3).
3. `DeleteResourceJob::dispatch(resource:, deleteVolumes:, deleteConnectedNetworks:, deleteConfigurations:, dockerCleanup:)`.
4. `auditLog('api.application.deleted', [team_id, application_uuid, application_name])`.
5. 200 `{"message":"Application deletion request queued."}`.

UI (`app/Livewire/Project/Shared/Danger.php:93-130`): password confirm; the four flags come from checked
`$selectedActions`; `$resource->delete()` then same dispatch (positional args), redirect to `project.resource.index`.

### 3.2 `DeleteResourceJob` for an Application (`app/Jobs/DeleteResourceJob.php`)
`ShouldBeEncrypted, ShouldQueue`, queue `'high'`. Constructor defaults: all four flags `true`, `deleteFromCoolifyOnly=false`.
`handle()` order:
1. (`deleteFromCoolifyOnly` — server-delete "archive" path only: delete volume-backup schedules, `deleteLocalResource()`, return.)
2. `try {`
   - `StopApplication::run($app, previewDeployments: true, dockerCleanup: $this->dockerCleanup)` — base **and preview**
     containers stopped+removed; writes `exited:unhealthy` etc. on the trashed row (or broadcasts ApplicationStopFailed and
     continues — the returned error string is ignored).
   - `if deleteConfigurations` → `$app->deleteConfigurations()`.
   - `if deleteVolumes` → `$app->deleteVolumes()`.
   - `if deleteConnectedNetworks && type()==='application'` → `$app->deleteConnectedNetworks()`.
   `} catch (Throwable)` → `Log::warning('Remote cleanup failed while deleting resource; continuing with local deletion.', [resource_id, resource_type, error])`;
   if `destination.server` exists → team notified `GeneralNotification("'{name}' was removed from Coolify, but its Docker resources could not be removed from server '{server name}' and may still exist there. Error: ".remove_iip($msg), success: false)`.
3. `try { deleteScheduledVolumeBackups() }` (per backup schedule on its persistent/file storages: `DeleteScheduledVolumeBackup::run($backup, $server)`), failure → `Log::warning('Remote backup cleanup failed while deleting resource; continuing with local deletion.', ...)`.
4. `$maintenanceServers = is_maintenance_enabled ? maintenanceServers() : null`.
5. `deleteLocalResource()` in `DB::transaction` (`:151-173`):
   - `if deleteVolumes` → `persistentStorages()->delete()` (query delete: `local_persistent_volumes` where `resource_type=Application, resource_id`; **no model events**).
   - `fileStorages()->delete()` (`local_file_volumes` rows, query delete).
   - `environment_variables()->delete()` (only `is_preview = false`).
   - `forceDelete()` → `forceDeleting` hook (§3.3), then `DELETE FROM applications`.
6. If maintenance was on → `syncMaintenancePage($maintenanceServers)` (rewrites `coolify-maintenance.yaml`), failure logged
   `Maintenance page cleanup failed while deleting resource.`.

### 3.3 Model hooks on Application
Soft delete (`$app->delete()`):
- `deleting`: `HasSecretManager` → `secretManagerLink()->delete()` (`app/Traits/HasSecretManager.php:24-29`).
- `deleted`: `ReleasesManagedDnsRecords` → if any `managed_dns_record_references` for the app or its previews (incl. trashed),
  `ReleaseManagedDnsRecordsJob::dispatch(morphClass, id)->afterCommit()` (`app/Traits/ReleasesManagedDnsRecords.php:19-51`);
  `ClearsGlobalSearchCache` → `GlobalSearch::clearTeamCache($teamId)`; `Auditable` → `recordAuditMutation('deleted')` (only with an authenticated user).
- Soft delete itself is a direct query update (`deleted_at`, `updated_at`), no `saving` hook.

`forceDelete()` (`app/Models/Application.php:419-434`), in order:
1. `$application->update(['fqdn' => null])` — goes through the `saving` hook (fqdn normalisation, `syncNoindexDomains`).
2. `settings()->delete()` → `application_settings` row.
3. `persistentStorages()->delete()` → all `local_persistent_volumes` rows (regardless of `deleteVolumes`; only the server volume removal depended on it).
4. `environment_variables()->delete()`, `environment_variables_preview()->delete()` → all env var rows.
5. `secretManagerLink()->delete()`.
6. each `scheduled_tasks` → `$task->delete()` (`scheduled_task_executions` have no FK cascade → orphaned).
7. `tags()->detach()` → `taggables` rows.
8. `previews()->delete()` → **soft delete** of `application_previews` (`ApplicationPreview` uses SoftDeletes; query delete sets
   `deleted_at`; preview `forceDeleting` hook does NOT run). Rows stay in the DB.
9. each `deployment_queue` → `$deployment->delete()` → **hard delete** of all `application_deployment_queues` rows (history lost).
Then the row is deleted; `deleted` listeners fire again (DNS release, search cache, audit `deleted`).
`additional_destinations` rows go via FK `onDelete('cascade')` (`database/migrations/2024_02_06_132748_add_additional_destinations.php:16`).

Safety net: `cleanup:stucked-resources` (daily 03:17, `app/Console/Kernel.php:65-69`; also on `Init`) re-dispatches
`DeleteResourceJob` (all defaults true) for every `Application::withTrashed()->whereNotNull('deleted_at')`
(`app/Console/Commands/CleanupStuckedResources.php:83-90`).

### 3.4 Remote helpers
- `deleteConfigurations()` (`Application.php:554-561`): `workdir()` = `/data/coolify/applications/U`
  (`application_configuration_dir()` = `base_configuration_dir().'/applications'`, base `/data/coolify`,
  `bootstrap/helpers/shared.php:81-88`; `Application::workdir()` `:1473-1476`). If it ends with U:
  `instant_remote_process(['rm -rf /data/coolify/applications/U'], $server, false)` (unquoted, errors ignored).
- `deleteVolumes()` (`:566-579`): for each persistent storage not `isSharedWithAnotherResource()`
  (`app/Models/LocalPersistentVolume.php:85-102`: true if `standalone_sqlite_id` set, or a SQLite-owned volume used elsewhere):
  `instant_remote_process(["docker volume rm -f '<name>'"], $server, false)`. One SSH call per volume. Bind mounts / file storages untouched on disk.
- `deleteConnectedNetworks()` (`:581-587`), two separate SSH calls, throwError false, uuid NOT quoted:
  `docker network disconnect U coolify-proxy` then `docker network rm U`. For non-compose apps the `U` network normally
  does not exist; errors are swallowed.

---

## 4. Status tracking

### 4.1 Who writes `applications.status`
| Writer | When | file |
|---|---|---|
| `GetContainersStatus` (full server scan) | `ServerCheckJob` (cron every minute when Sentinel is out of sync — i.e. always without Sentinel; `app/Jobs/ServerManagerJob.php:166-182`), after deployments (`post_deployment`, `ApplicationDeploymentJob.php:627`), UI checkStatus | `app/Actions/Docker/GetContainersStatus.php` |
| `PushServerUpdateJob` (Sentinel push) | Sentinel enabled | `app/Jobs/PushServerUpdateJob.php` |
| `StopApplication` | stop/delete/restart-limit | §1 |
| deployment health check | `$this->application->update(['status' => 'running'])` when new container is `healthy` | `ApplicationDeploymentJob.php:2971` (stored as `running:unhealthy` by the mutator!) |
| `Server::markResourcesAsExited()` | server unreachable (`ServerCheckJob` / `isFunctional` checks) | `app/Models/Server.php:2396-2418` |
| `ComplexStatusCheck` | apps with additional servers (n/a single server) | `app/Actions/Shared/ComplexStatusCheck.php` |

`ResourceStatusRefresher` (`app/Services/ResourceStatusRefresher.php`) only has `refreshDatabase` / `refreshService`;
**there is no single-application refresher.** Applications are refreshed only by the full `GetContainersStatus` scan.

### 4.2 `GetContainersStatus::handle(Server $server, ?Collection $containers = null, ?Collection $containerReplicates = null)`
Queue `high`, timeout 120 s, `ShouldBeUnique` per server uuid (`jobUniqueFor` 60, released when processing starts).
1. `! isFunctional()` → return `'Server is not functional.'`.
2. `$this->applications = $server->applications()` (`Server.php:1899-1925`: apps whose `destination` is a StandaloneDocker/SwarmDocker of S, **excluding soft-deleted**, plus `additional_destinations` apps). Apps with `additional_servers` are removed and handed to `ComplexStatusCheck::run`.
3. If `$containers === null`: `$server->getContainers()` (`Server.php:1773-1813`) →
   `instant_remote_process_with_timeout(["docker container inspect $(docker container ls -aq) --format '{{json .}}'"], $server, false)` then
   `format_docker_command_output_to_json`. (No containers → command fails → null → empty collection.)
4. `databases()`, `services()->get()`, `previews()` (`Server.php:2013-2020`: previews of apps on its StandaloneDockers, non-trashed).
5. Per container (inspect JSON):
   - `$labels = Config.Labels`; `$containerStatus = ContainerStatusAggregator::containerStatus($container)` (§4.3).
   - `$flatLabels = format_docker_labels_to_json($labels)`; `$labels = Arr::undot($flatLabels)`.
   - `com.docker.compose.oneoff` truthy → skip.
   - Application container ⇔ `isContainerOfType($flat, 'application')` (`docker.php:188-193`): `coolify.applicationUuid` filled OR `coolify.applicationId` filled.
   - `coolify.pullRequestId` truthy (`"0"` is falsy in PHP) → preview branch: owner id via `resolveContainerApplicationId`
     (includes trashed apps), preview row `where application_id, pull_request_id`; status differs → `update(['status'=>$containerStatus])`
     else `update(['last_online_at'=>now()])`; restart count recorded per `appId:prId`.
   - Else base: `$application = resolveContainerOwner($this->applications, $flat, 'application')` (`docker.php:106-121`):
     owner uuid = `coolify.applicationUuid`; else (legacy, only if `coolify.applicationId` present) `com.docker.compose.project`
     (or `com.docker.stack.namespace`); match by `uuid`. Fallback for legacy containers without uuid label: an app whose uuid
     appears as a dash-delimited part of `com.docker.compose.service` or `coolify.name` (`/(?:^|-)<uuid>(?:-|$)/`).
     **Numeric ids are never matched.** Found →
       - `$foundApplications[] = id`;
       - `container_present !== true` → `update(['container_present' => true])`;
       - `$containerName = com.docker.compose.service` (for a standalone app this is the compose service = container name `U-<timestamp>`; during a rolling update both old and new are recorded);
       - `applicationContainerStatuses[id][containerName] = containerStatus`; `applicationContainerRestartCounts[id][containerName] = RestartCount`.
     Containers without `com.docker.compose.service` still mark the app found but contribute no status.
6. Services / databases branches (not covered here).
7. **Apps with no container** (`$this->applications->pluck('id')->diff($foundApplications)`), per app:
   - if the scan returned zero containers at all → skip (protection against failed `docker` query).
   - current status starts with `exited` → `update(['container_present' => false, 'restart_limit_reached' => false])`; continue.
   - `recentlyRestarted = restart_count > 0 && last_restart_at && last_restart_at > now()-30s` →
     `update(['status' => 'degraded:unhealthy'])`.
   - else `update(['status' => 'exited', 'container_present' => false, 'restart_count' => 0, 'last_restart_at' => null, 'last_restart_type' => null, 'restart_limit_reached' => false])` (stored `exited:unhealthy`).
8. Previews not found and not `exited*` (and scan non-empty) → `update(['status' => 'exited'])`.
9. `trackPreviewRestartCounts` → `ApplicationPreview::trackRestartCount(max)` true → `StopApplicationPreview::dispatch($preview, false, false)` + `RestartLimitReached` notification.
10. Aggregation per found app (`:479-545`), inside `DB::transaction`:
    - `maxRestartCount = max(RestartCount of its containers) ?? 0`.
    - `RestartCountTracker::evaluate(previous: restart_count ?? 0, observed: max, maxRestartCount: max_restart_count ?? 0)`
      (`app/Services/RestartCountTracker.php`): changed iff observed > previous (new-generation flag is never passed here);
      `restart_limit_reached = max_restart_count > 0 && observed >= max_restart_count`.
    - changed → `update(['restart_count' => observed, 'last_restart_at' => observed>0 ? now() : null, 'last_restart_type' => observed>0 ? 'crash' : null])`.
      (restart_count never goes down here; it is reset by stop, by "not found → exited", and by a successful deployment.)
    - `$aggregated = (new ContainerStatusAggregator)->aggregateForCompose($statuses, $app->docker_compose_raw, max)`;
      `$app->status !== $aggregated` → `update(['status' => $aggregated])` else `update(['last_online_at' => now()])`.
      Comparison is against the **getter** output (always `x:y`); an aggregated `exited` therefore always "differs" but the mutator
      turns it back into the same `exited:unhealthy` → nothing dirty → no UPDATE (and `last_online_at` not bumped).
    - After the transaction, if limit reached: atomic claim
      `UPDATE applications SET restart_limit_reached = true WHERE id = ? AND restart_limit_reached = false`; if 1 row →
      `refresh()`, `StopApplication::dispatch(..., dockerCleanup: false, resetRestartCount: false, removeContainers: false)`
      (stop without rm → `container_present` true, status `exited`), notify team `App\Notifications\Application\RestartLimitReached`.
11. Service aggregation; `ServiceChecked::dispatch($server->team->id)` (broadcast, `Silenced`).

### 4.3 Status strings
`ContainerStatusAggregator::containerStatus()` (`app/Services/ContainerStatusAggregator.php:48-56`):
`State.Status === 'exited'` → `exited`; else `"{State.Status}:" . (State.Health.Status ?? 'unknown')`
(e.g. `running:healthy`, `running:unknown` (no healthcheck), `running:starting`, `restarting:unknown`, `created:unknown`, `paused:unknown`, `dead:unknown`).

`aggregateForCompose($statuses, $composeRaw, $max)` (`:65-75`): excluded services = compose services with
`exclude_from_hc: true` or `restart: no` (`app/Traits/CalculatesExcludedStatus.php:106-165`; for a non-compose app
`docker_compose_raw` is NULL ⇒ none). If every status is excluded → `calculateExcludedStatusFromStrings` (suffix rules
`:72-94`: `degraded*`→`degraded:excluded`, `paused*`→`paused:excluded`, `starting*`→`starting:excluded`, `exited*`→`exited`,
else `<status>:excluded`, e.g. `running:healthy:excluded`). Otherwise `aggregateFromStrings(..., preserveRestarting: true)`.

`aggregateFromStrings` (`:85-152`) flag scan per string, first matching `contains` wins:
`degraded` (+`unhealthy`) → `restarting` → `running` (+`unhealthy` → hasUnhealthy, +`unknown` → hasUnknown) → `exited` →
`created`|`starting` → `paused` → `dead`|`removing`. Empty collection → `exited`.
`resolveStatus` (`:243-308`) priority:
1. degraded → `degraded:unhealthy`
2. restarting → `restarting:unknown` (preserveRestarting) / `degraded:unhealthy`
3. running && exited → `degraded:unhealthy`
4. running && starting → `starting:unknown`
5. running → `running:unhealthy` | `running:unknown` | `running:healthy`
6. dead → `degraded:unhealthy`
7. paused → `paused:unknown`
8. starting → `starting:unknown`
9. else `exited`
Quirk: `running:starting` (healthcheck still starting) matches only `running` → aggregates to **`running:healthy`**.
`$maxRestartCount` is accepted but unused by `resolveStatus`.

### 4.4 Sentinel path (`PushServerUpdateJob`, if Sentinel enabled)
Container status from push JSON: `state` (default `exited`), `health_status ?? 'unknown'`, appended as `state:health` unless
`exited` (`:257-264`). Matching uses the same `resolveContainerApplicationId`; base iff label `coolify.pullRequestId` === `'0'`
(missing label defaults to `'0'`). Differences from GetContainersStatus:
- Not-found apps: **query-builder** `Application::whereIn('id', ...)->update(['status' => 'exited', 'container_present' => false, 'restart_limit_reached' => false])` (`:769-783`) → raw `exited` stored (bypasses mutator), no `last_online_at`, no restart reset, no 30 s degraded grace, runs on every push.
- Found apps: `Application::whereIn('id', found)->update(['container_present' => true])` (`:386-387`).
- Aggregation (`:580-671`): restart tracking only if counts present; if `stoppedAfterRestartLimit()` (`Application.php:743-748`: status `exited*` && `container_present === true` && `restart_limit_reached === true`) and all statuses contain `exited` → `update(['status' => 'exited'])`; otherwise `aggregateFromStrings(relevant, max, preserveRestarting: true)` and `$app->status = ...; save()` only when different (no `last_online_at` bump when equal).
- Empty `containers` with `snapshot.complete === false` → whole push ignored.

### 4.5 Application `status` attribute and `saving` hook (`app/Models/Application.php`)
Setter and getter (single-server branch, `:989-1035`), same parse for both:
- contains `(` → status = before `(` trimmed, health = between `(` and `)` trimmed (legacy `running (healthy)` form);
- elif contains `:` → status = before first `:`, health = everything after the first `:` (so `running:healthy:excluded` round-trips);
- else status = value, health = `unhealthy`.
Result `"$status:$health"`. Consequences: `exited` → `exited:unhealthy`; `running` → `running:unhealthy`; `degraded` →
`degraded:unhealthy`; NULL → `:unhealthy`; `running:` stays `running:`. The `?? 'unhealthy'` fallbacks are dead code.
With additional servers the getter returns `degraded` if any pivot status differs (n/a here).
`isRunning()` = starts with `running`; `isExited()` = starts with `exited`; `realStatus()` = raw DB value.
DB default `'exited'` (`database/migrations/2023_03_27_081716_create_applications_table.php:66`) → reads as `exited:unhealthy`.

`saving` hook (`:314-396`): `isDirty('status')` → `last_online_at = now()`. Also on save: fqdn normalisation via
`DomainPortOverrides::normalize`, trims of install/build/start commands, base/publish dirs, git repo/branch/sha, etc.
Columns: `restart_count int default 0`, `max_restart_count int default 10`, `restart_limit_reached bool default false`,
`container_present bool NULL`, `last_restart_at timestamp NULL`, `last_restart_type varchar(10) NULL`, `last_online_at timestamp default now()`.

### 4.6 `ApplicationStatusChanged`
`app/Events/ApplicationStatusChanged.php`: `ShouldBroadcast`, `team.{teamId}`. Only dispatched by
`Destination::removeServer` (`app/Livewire/Project/Shared/Destination.php:216`). Listeners: `Server\Resources::refreshStatus`,
`Destination::loadData`. **Not** dispatched by status changes, stop, or GetContainersStatus. The events that matter for
applications are `ServiceStatusChanged` (after stop), `ServiceChecked` (after every scan), `ApplicationStopFailed`.

---

## 5. Proxy side (Traefik)

### 5.1 Routing is label-only
Applications get routes only from container labels. `generateLabelsApplication()` (`docker.php:1254`) →
`fqdnLabelsForTraefik()` (`docker.php:966`, starts with `traefik.enable=true`) plus `defaultLabels()` (`docker.php:651-672`:
`coolify.managed=true`, `coolify.version=4.5.1`, `coolify.applicationUuid=U`, `coolify.type=application`, `coolify.name=<slug>`,
`coolify.resourceName=`, `coolify.projectName=`, `coolify.serviceName=`, `coolify.environmentName=`, `coolify.pullRequestId=<n>`).
No `traefik.docker.network` label. Coolify writes NO per-application Traefik dynamic file. Files under `<proxyPath>/dynamic/`
written by Coolify are only: `default_redirect_503.yaml` (`Server::setupDefaultRedirect`, `Server.php:706`),
`coolify.yaml` (dashboard, `Server::setupDynamicProxyConfiguration`, `:1084`), `coolify-maintenance.yaml`
(maintenance mode, `:963-975`), plus user files from the Dynamic Configurations UI.

Traefik compose (`bootstrap/helpers/proxy.php:689-810`): container `coolify-proxy`, image `traefik:v3.7`, ports `80:80`,
`443:443`, `443:443/udp`, `8080:8080`, networks = every StandaloneDocker network of the server as `external: true`
(default `coolify`), labels `traefik.enable=false`, `coolify.managed=true`, `coolify.proxy=true`, and among the commands
`--providers.docker=true`, `--providers.docker.exposedbydefault=false`, `--providers.file.directory=/traefik/dynamic/`,
`--providers.file.watch=true`, `--certificatesresolvers.letsencrypt.acme.httpchallenge=true`, `...entrypoint=http`,
`...storage=/traefik/acme.json`, `--api.insecure=false`.

### 5.2 What must already exist on the server
- Docker network of the destination (default `coolify`): the generated app compose declares it
  `external: true, name: <network>, attachable: true` (`ApplicationDeploymentJob.php:4434-4440`), so `docker compose up`
  fails if it is missing. It is created (a) when the `StandaloneDocker` row is created (`app/Models/StandaloneDocker.php:36-48`:
  `docker network inspect '<net>' >/dev/null 2>&1 || docker network create --attachable '<net>' >/dev/null` then
  `docker network connect '<net>' coolify-proxy >/dev/null 2>&1 || true`, throwError false), and (b) by StartProxy
  (`ensureProxyNetworksExist`, `proxy.php:446-460`). The deployment itself does **not** create it for non-compose apps
  (only compose apps get `docker network create --attachable 'U'`, `ApplicationDeploymentJob.php:895-910`).
- `coolify-proxy` attached to that network. The deployment runs, before `docker compose up`, in `start_by_compose_file()`
  (`ApplicationDeploymentJob.php:5269-5278`): `touch /data/coolify/applications/U/.env` and
  `docker network connect '<net>' coolify-proxy >/dev/null 2>&1 || true` (hidden, errors ignored).
- First job step also runs `docker network inspect '<net>' -f '{{json .Containers}}' ` (`:393-394`, throwError true) to build
  `--add-host` flags; a missing network makes that throw → deployment fails.

### 5.3 Proxy checks in the deployment path
None. `ApplicationDeploymentJob` never calls `CheckProxy`/`StartProxy` and never inspects proxy status. Its only proxy-related
actions: the `network connect` above; a warning if the `PORT` env var does not match `ports_exposes`
(`:1930-1940`, `Warning: PORT environment variable (<p>) does not match configured ports_exposes: <list>. It could case "bad gateway" or "no server" errors. Check the "General" page to fix it.`);
mixed-proxy warning for additional servers (`:2490-2502`); and in rolling update `wait_for_proxy_to_route_new_container()`
(`:2879-2900`, log `Waiting {n} seconds for the proxy to send traffic to the new container.` or
`The proxy labels changed, so the old containers stop without waiting.`).

The proxy is kept alive by `ServerCheckJob` (`app/Jobs/ServerCheckJob.php:80-101`): if `proxySet()` (`Server.php:697-700`:
type set and ≠ `NONE`, functional, not swarm worker, can host resources) and not `proxy.force_stop` and no transition in
progress: look for container `Name === '/coolify-proxy'` in the inspect output; absent → `CheckProxy::run($server)` and, if true,
`StartProxy::run($server, async: false)` + `ContainerRestarted('coolify-proxy', ...)` notification (exceptions swallowed);
present → `server.proxy.status = State.Status` and save. Sentinel push does the same via `updateProxyStatus` (`PushServerUpdateJob.php:817+`).

`CheckProxy::handle($server, $fromUI=false)` (`app/Actions/Proxy/CheckProxy.php`): not functional → false; build server → clear
proxy, false; type null/`NONE`/force_stop (non-UI) → false; `docker inspect --format '{{json .}}' 'coolify-proxy'` →
`running` ⇒ set status running, false; cloudflare tunnel ⇒ false; else parse published ports from the proxy compose and test them
for conflicts in parallel; conflict ⇒ false (UI: throws `Port $port is in use.<br>You must stop the process using this port.<br><br>Docs: ...`); otherwise true.

`StartProxy::startCommands` (`app/Actions/Proxy/StartProxy.php:65-120`) non-swarm:
`mkdir -p <proxy>/dynamic`, write base Caddyfile, `echo 'Creating required Docker Compose file.'`, `echo 'Pulling docker image.'`,
`docker compose -f <proxy>/docker-compose.yml pull`, stop/rm existing `coolify-proxy` with a 10×1 s wait loop, (non-Traefik: rm logrotate sidecar),
per network `echo 'Ensuring network '<n>' exists...'` + `dockerNetworkEnsureCommand`, `echo 'Starting coolify-proxy.'`,
`docker compose -f <proxy>/docker-compose.yml up -d --wait --remove-orphans`, `echo 'Successfully started coolify-proxy.'`,
then a shell loop connecting `coolify-proxy` to every network of every `coolify.managed=true` container (`proxy.php:427-436`).
Before running: `GetProxyConfiguration`, `proxy.status='starting'`, `ProxyStatusChangedUI`, `SaveProxyConfiguration`.

---

## 6. Helper image

### 6.1 Name and tag
- `config/constants.php:9` `'helper_version' => '1.0.19'`; `:20` `'registry_url' => env('REGISTRY_URL', 'docker.io')`;
  `:21` `'helper_image' => env('HELPER_IMAGE', env('REGISTRY_URL', 'docker.io').'/coollabsio/coolify-helper')`.
- `coolifyHelperImage()` (`bootstrap/helpers/shared.php:4717-4727`): if configured `helper_image` ≠
  `<config registry_url>/coollabsio/coolify-helper` → use it verbatim; else
  `coolifyRegistryUrl().'/coollabsio/coolify-helper'` where `coolifyRegistryUrl()` = `instance_settings.docker_registry_url ?: 'docker.io'` (`:4708-4715`).
- `getHelperVersion()` (`:4729-4748`): dev → `instance_settings.dev_helper_version` if set; else
  `instance_settings.helper_version` if it is `version_compare(..., '1.0.19', '>')`, else `'1.0.19'`.
- Default result: **`docker.io/coollabsio/coolify-helper:1.0.19`**.

### 6.2 How it gets onto the server
Nothing pulls it explicitly before use. `CheckHelperImageJob` (`app/Jobs/CheckHelperImageJob.php`, every 10 min,
`app/Console/Kernel.php:87`, and on `Init`/`Dev`) only runs `update_helper_version(get_versions_data())`
(`bootstrap/helpers/versions.php:151-158`): reads local `versions.json` (cached 3600 s under `coolify:versions:all`;
snapshot has `coolify.helper.version` = `1.0.18`) and bumps `instance_settings.helper_version` if newer. The image is pulled
implicitly by `docker run` when absent on the server.

### 6.3 `prepare_builder_image()` (`app/Jobs/ApplicationDeploymentJob.php:3112-3160`)
1. `checkForCancellation()`.
2. `serverUserHomeDir = instant_remote_process(['echo $HOME'])`;
   `instant_remote_process(["mkdir -p <home>/.docker/buildx"])`;
   `dockerConfigFileExists = instant_remote_process(["test -f <home>/.docker/config.json && echo 'OK' || echo 'NOK'"])`.
3. `env_flags = generate_docker_env_flags_for_secrets()` (`:5392-5435`): `''` unless `settings.use_build_secrets` and the
   deployment passes build-time vars; else `-e 'KEY=value'` per variable (`generateDockerEnvFlags`, `docker.php:2147-2168`).
4. `buildxMetadataVolume` = `-v <home>/.docker/buildx:/root/.docker/buildx` (dev+localhost: `-v coolify-buildx:/root/.docker/buildx`).
5. Run command (non build server; `seccompMounts` only for `pull_request_id === 0`, from `security_opt seccomp=/abs/path` in custom
   docker options, `docker.php:1872-1882`; usually empty):
   - config.json exists:
     `docker run -d --network '<net>' --name D {env_flags} --rm -v <home>/.docker/config.json:/root/.docker/config.json:ro -v <home>/.docker/buildx:/root/.docker/buildx -v /var/run/docker.sock:/var/run/docker.sock {seccompMounts} docker.io/coollabsio/coolify-helper:1.0.19`
   - otherwise:
     `docker run -d --network '<net>' --name D {env_flags} --rm -v <home>/.docker/buildx:/root/.docker/buildx -v /var/run/docker.sock:/var/run/docker.sock {seccompMounts} docker.io/coollabsio/coolify-helper:1.0.19`
   (empty interpolations leave double spaces — byte-exactness of the string matters only for logs; the command is hidden.)
   Build server variant: no `--network`, requires config.json else DeploymentException
   `Docker config file (~/.docker/config.json) of the SSH user (<user>) not found on the build server (<name>). Log in to your registry under Server → Docker Registries on the build server. On a non-root server, a login made with "sudo docker login" is saved for root and is not used for builds.`
6. Log `Preparing container with helper image: <image:tag>` (first try) / `Preparing container with helper image with updated envs.` (restart after commit resolution, `:3162-3172`).
7. `graceful_shutdown_container(D, skipRemove: true)` (`:5157-5175`): `docker stop --time=<deploy grace> 'D'` hidden, errors ignored.
8. `execute_remote_command([runCommand, hidden, skip_command_log: filled(env_flags)], ['command' => "docker exec D bash -c 'mkdir -p /artifacts/D'"])`
   (`executeInDocker`, `docker.php:543-548`; basedir `/artifacts/D`, `Application::generateBaseDir` `:1558-1561`).
9. `run_pre_deployment_command()`.

---

## 7. Domain generation for new applications

### 7.1 Functions (`bootstrap/helpers/shared.php`)
- `generateUrl(Server $server, string $random, bool $forceHttps = false)` (`:1477-1492`):
  `$wildcard = server_settings.wildcard_domain`; NULL or `''` → `sslip($server)`. Parse with `Spatie\Url\Url::fromString`;
  `$path = getPath() === '/' ? '' : getPath()`; scheme from wildcard (or `https` if forced). Returns
  **`"{scheme}://{random}.{host}{path}"`**. Port of the wildcard is dropped.
- `generateFqdn(...)` (`:1493-1513`): same, but for `parserVersion >= 5` (and version ≥ `4.0.0-beta.420.7`) returns
  `"{random}.{host}{path}"` without scheme. Used only by compose/service parsers, **not** for new applications.
- `sslip(Server $s)` (`:1514-1522`): dev && `server.id === 0` → `http://127.0.0.1.sslip.io`; else
  `$ip = server.ip === 'host.docker.internal' ? base_ip() : server.ip` → `'http://' . sslipHostLabel(resolveHostToIp($ip)) . '.sslip.io'`.
- `base_ip()` (`:799-813`): dev → `localhost`; `instance_settings.public_ipv4` → it; `public_ipv6` → it; else `localhost`.
- `resolveHostToIp($host)` (`:1528-1540`): already an IP (after `normalizeIpAddress`, which strips `[]` and canonicalises IPv6) →
  returned **unchanged**; else `gethostbyname` IPv4; else first AAAA record; else the host itself (live DNS lookup at creation time).
- `sslipHostLabel($ip)` (`:2510-2524`): non-IPv6 → unchanged; IPv6 → canonical form (`inet_ntop(inet_pton)`), leading `::` →
  `0::`, trailing `::` → `::0`, then `:` → `-`.

Examples: `http://<U>.203.0.113.5.sslip.io`; IPv6 `2a01:4f8::1` → `http://<U>.2a01-4f8--1.sslip.io`;
wildcard `https://apps.example.com` → `https://<U>.apps.example.com`.

### 7.2 Where it is applied
- API create endpoints (public / private-gh-app / private-deploy-key / dockerfile / dockerimage,
  `ApplicationsController.php:1730, 1950, 2179, 2327, 2462`):
  `$autogenerateDomain = $request->boolean('autogenerate_domain', true) && $request->build_pack !== 'dockercompose'` (`:1442`);
  after the app is saved and settings applied/refreshed: `if ($autogenerateDomain && blank($fqdn)) { $application->fqdn = generateUrl(server: $server, random: $application->uuid); $application->save(); }`.
- UI creation (`app/Livewire/Project/New/PublicGitRepository.php:362`, `DockerImage.php:152`, `SimpleDockerfile.php:71`, the
  Github/Gitlab/Gitea/DeployKey variants): always `generateUrl(server: $destination->server, random: $application->uuid)` after first save.
  DockerImage also sets `name = 'docker-image-'.U`.
- "Generate domain" in General (`app/Livewire/Project/Application/General.php:706-716`) uses the app uuid; compose per-service and
  Domains page "generate" use a fresh `new_public_id()`.
- `random` = application uuid = `new_public_id()` (`shared.php:130-135`): `Str::lower(Str::random(24))` → 24 chars `[a-z0-9]`,
  assigned in `BaseModel::boot` `creating` if empty (`app/Models/BaseModel.php`).
- Saved through the `saving` hook → `DomainPortOverrides::normalize` (`app/Support/DomainPortOverrides.php:27-75`) which leaves a
  port-less URL unchanged.
