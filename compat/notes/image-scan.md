# Image vulnerability scanning: map of the engine

Port of upstream Coolify v4.7.5 image scanning (`app/Services/ImageScan`, `app/Actions/ImageScan`, the scan jobs).
The engine is in `crates/coolify-jobs/src/image_scan/`, its queries in `crates/coolify-db/src/image_scans/`.
Nothing in it was compiled or run when it was written; see "Not done and not verified" at the end.

How it works: Syft writes the package list (SBOM) of an image on the server that runs it
(`/data/coolify/image-scans/sboms/{image id hex}.json`), Grype matches the SBOM against its database (Docker volume
`coolify-grype-db`), and Coolify stores the findings per resource in `image_scans`. All scanner containers are pinned
by digest (`commands::SCANNER_IMAGES`). One scan or scan data change runs per server at a time (`scan_image::server_lock`).

## Words

- **Resource**: an application, a service, or a standalone database (what has a page).
- **Scan owner**: what owns `image_scans` rows: an application, a service application, a service database, or a
  standalone database. A service owns none; it shows the scans of its applications and databases.
  Rust type for both: `coolify_db::image_scans::ScanOwner` (`morph`, `id`, `uuid`, `team_id`, `main_server_id`,
  `service_id`, the whole row in `record`, `settings` = `application_settings` for an application).
- **Current scans** of an owner: the completed scans of its latest deployment that is not a preview, else its latest
  scan without a deployment (`image_scans::current_for`).
- **Unsaved scan**: `ScanImage` returns an `ImageScanRow` with `id` 0 (`exists()` is false) for a failure that is not
  stored. Its `error_code` says why.

## Database module `coolify_db::image_scans`

| What | Functions and types |
|---|---|
| Owners | `ScanOwner`, `find_owner(db, morph, id)`, `find_resource(db, kind, uuid, team_ids)` (`ResourceScans::find`), `owners(db, resource)`, `service_owners`, `owner_service`, `owners_on_server`, `resource_table(morph)`, `delete_resource_data(db, morph, id)` (`HasImageScans`) |
| Morph classes | `APPLICATION_MORPH`, `SERVICE_MORPH`, `SERVICE_APPLICATION_MORPH`, `SERVICE_DATABASE_MORPH`, `TEAM_MORPH`, `SERVER_MORPH`; standalone databases: `coolify_db::databases::Kind::morph()` |
| Server settings | `ServerScanSettings`, `server_scan_settings(db, server_id)`, `set_server_scan_enabled`, `scanning_server_ids`, `team_scans_new_servers` |
| `image_scans` | `ImageScanRow` (`exists()`, `is_completed()`, `owner_key()`, `candidate_entries()`, `candidate_status()`), `find_scan`, `find_scan_by_uuid`, `find_scan_of_image`, `scans_of_resource`, `scans_on_server` (light), `has_completed_scan`, `first_or_create_scan`, `update_scan(db, id, &[(column, ScanValue)], now)`, `delete_scan`, `current_for`, `current_scan_ids`, `scans_by_ids`, `deployment_scan_rows`, `scan_owner_keys_of_team` |
| Deployments | `DeploymentFacts`, `deployment_facts`, `latest_finished_deployment`, `save_gate_result`, `gate_result` |
| `image_sboms` | `ImageSbomRow`, `find_sbom`, `find_sbom_by_id`, `sboms_on_server`, `create_sbom`, `update_sbom`, `touch_sbom`, `delete_sboms` |
| `image_scan_policies` | `ImageScanPolicyRow`, `instance_policy`, `scope_policy`, `scope_policies`, `save_policy(db, scope, gate_mode, on_scan_error, count_medium, now)` |
| `image_scan_ignores` | `ImageScanIgnoreRow`, `active_ignores_for`, `active_ignores_of_team`, `ignores_of_team`, `find_ignore`, `create_ignore(db, &NewIgnore, now)`, `delete_ignore` |
| Submodules | `notifications` (`image_scan_notification_states`), `host` (`server_host_scans`), `candidates`, `gate`, `jobs`, `update`: the queries of that part of the engine |

Statuses are the constants `STATUS_QUEUED`, `STATUS_RUNNING`, `STATUS_COMPLETED`, `STATUS_FAILED`. JSON columns are
`serde_json::Value`. `findings` is a list of `Finding::to_value()`, `image_sboms.packages` a list of `Package::to_value()`.

## Engine modules `coolify_jobs::image_scan`

| Module | Ports | Main entry points |
|---|---|---|
| `mod.rs` | | `ScanServer` (`load(jobs, server_id)`, `host()`, `commands(run_id)`, `run(commands, timeout_seconds)` = `instant_remote_process(throwError: false, timeout:)`, `is_scan_enabled()`, `is_transferred_away()`), `new_run_id()`, `lock_retry_delay(attempt)`, `MAX_ATTEMPTS` |
| `error` | `ImageScanErrorCode`, `ScanFailed`, `ScanLockBusy`, `ImageScanningRefused`, `ImageUpdateRefused` | `ErrorCode::{value, from_value, message(context)}`, `ScanFailed::{new, detail, with, error_context, output_tail, log}` |
| `image_ref` | `ImageRef` | `ImageRef::parse`, `to_string()`, `qualified_name()`, `with_tag`, `is_docker_official`, `is_valid_tag`, `is_valid_digest` |
| `text` | `ScannerText` | `clean`, `clean_str`, `title`, `tidy_title`, `stored_title`, `fallback_title` |
| `finding` | `Finding`, `Package` | `to_value`, `from_value`, `Finding::list_from_value`, `Finding::ids()` |
| `commands` | `ScannerCommands`, scanner image pins, templates | `ScannerHost`, `ScannerCommands::{prepare, scanner_images, db_update, db_status, pull, inspect, disk_free, disk_usage, sbom_from_local, host_packages_size, sbom_from_host, sbom_from_registry, grype, packages, tags, digest, cleanup, delete_scanner_data, running_containers, delete_sboms}`, `parse_output` -> `ParsedOutput` |
| `grype` | `GrypeReportParser`, `SyftPackageParser` | `parse_grype_report[_with_limit]` -> `GrypeReport {findings, truncated, eol}`, `parse_syft_packages[_with_limit]`, `EolDistro` |
| `sbom_document` | `SbomDocument` | `cyclone_dx(image_ref, packages, now)`, `spdx(...)`, `FORMATS` |
| `rules` | `SecurityRules`, `RelevanceRules`, `IgnoreMatcher` | `exploit_signal`, `is_relevant`, `is_cpe_only_os_match`, `relevant_ids`, `IgnoreMatcher::{from_ignores, for_resource, matches}`, `RelevanceRules::{for_resource, from_ignores, with_all_mediums}` |
| `policy` | `ImageScanPolicyResolver`, `ResolvedImageScanPolicy`, `ImageScanGateMode`, `ImageScanOnError` | `resolve(db, resource, scan_server_id)`, `count_medium_for`, `count_medium_for_many`, `for_scope`, `inherited_for`, `off_state`, `off_state_of`, `is_allowed_for`, `is_scan_on`, `resolve_levels` |
| `cache` | Laravel cache and `Cache::lock` | `put`, `get`, `has`, `forget`, `add`, `increment`, `CacheLock` (in memory, one process) |
| `events` | `ImageScanUpdated` | `set_event_sink`, `dispatch(team_id, resource_uuid, scan_uuid, is_final)` |
| `resource_scans` | `ResourceScans`, `RefreshImageScanSummariesJob` | `scan_targets`, `current`, `current_for`, `current_for_many`, `owners_of_many`, `unchecked_compose_refs`, `compose_deployment_rows`, `compose_images`, `compose_service_names`, `summary_for`, `refresh_summaries(jobs, scans)`, `current_for_team`, `findings(scan)`, `runs_other_image`, `configured_image`, `current_image_ref`, `advisory_url`, `exploit_likelihood`, `stored_fixed_count`, `Jobs::dispatch_refresh_image_scan_summaries(team_id)` |
| `scan_image` | `ScanImage` | `handle(jobs, server, resource, image_ref, ScanOptions)`, `rescan(jobs, server, image_id, targets, lock_wait_seconds, retry_when_busy)`, `server_lock`, `acquire_server_lock`, `lock_ttl(host)`, `run_step`, `step_result`, `assert_succeeded`, `read_payload`, `check_disk_space`, `database_built_at`, `builds_image`, `image_name`, `progress_cache_key`, `last_error_cache_key`, `lock_was_busy`, `is_gate_waiting` |
| `prepare` | `PrepareImageScannerJob`, `SetServerImageScanning` | `set_server_image_scanning(jobs, server, enabled)`, `prepare_scanner`, `Jobs::dispatch_prepare_image_scanner(server_id)`, `state(server_id)`, `failed_step`, `database_built_at`, `disk_used_bytes`, `forget`, `store_details` |
| `compose_image_variable` | `ComposeImageVariable` | `parse`, `resolve`, `value_for`, `has_variable` |
| `scan_jobs` | `ScanResourceImagesJob`, `ImageRescanJob` | see "Interfaces" |
| `host` | `ScanHostPackages`, `ScanHostPackagesJob` | see "Interfaces" |
| `gate` | `SecurityGate`, `DeploymentImageScan` | see "Interfaces" |
| `candidates` | `RefreshCandidates`, `RefreshCandidatesJob`, `CandidateEvaluator`, `TagSelector`, `BaseImageCheck` | see "Interfaces" |
| `update` | `UpdateResourceImage`, `DockerfileBaseImage`, `ImageStartPin` | see "Interfaces" |
| `notify` | `ScanNotifications`, `ImageScanAlertCheckJob`, `ImageScanDigestJob`, `Notifications\ImageScan\*` | see "Interfaces" |
| `schedule` | scan entries of `Console\Kernel` and `ServerManagerJob` | |

Cache keys (all in `cache`, in memory): `image-scan:{server id}` (lock), `image-scan:gate-waiting:{server id}`,
`image-scan:progress:{morph}:{id}` = `{run_id, stage, started_at}`, `image-scan:last-error:{morph}:{id}` =
`{code, context, at}`, `image-scan-prepare:{server id}` and its `:built-at`, `:disk-used`, `:failed-step`.

## Interfaces between engine modules

These signatures are fixed, because the modules were written at the same time by different agents. A function that
reads the database or a server is `async`, takes `jobs: &Jobs` first, and returns `Result<T, String>` (the `Err` is an
error that is not a scan problem). `scan: &ImageScanRow` stands for upstream's `ImageScan $scan`; load its resource
with `image_scans::find_owner(db, &scan.resource_type, scan.resource_id)` where upstream reads `$scan->resource`.

`notify`:
- `pub async fn after_scan(jobs: &Jobs, scan: &ImageScanRow, deployment_id: Option<i64>, candidate_check: bool, check_gave_up: bool) -> Result<(), String>` (`ScanNotifications::afterScan`)
- `pub async fn reevaluate(jobs: &Jobs, scan: &ImageScanRow, check_gave_up: bool) -> Result<(), String>` (`ScanNotifications::reevaluate`)

`candidates`:
- `pub fn mark_queued(scan: &ImageScanRow, seconds: u64)`, `pub fn is_queued(scan: &ImageScanRow) -> bool`, `pub fn forget_queued(scan: &ImageScanRow)`
- `pub async fn run(jobs: &Jobs, scan: &ImageScanRow, lock_wait_seconds: u64, retry_when_busy: bool) -> Result<(), RefreshError>` (`RefreshCandidates::run`), with `pub enum RefreshError { LockBusy, Other(String) }`
- `pub async fn needs_candidates(jobs: &Jobs, scan: &ImageScanRow) -> Result<bool, String>`
- `pub async fn relevant_findings(jobs: &Jobs, scan: &ImageScanRow, resource: &ScanOwner, rules: Option<&RelevanceRules>) -> Result<Vec<Finding>, String>`
- `pub async fn is_fresh(jobs: &Jobs, scan: &ImageScanRow) -> Result<bool, String>`
- `pub async fn should_refresh(jobs: &Jobs, scan: &ImageScanRow) -> Result<bool, String>`
- `pub fn queue_check(jobs: &Jobs, scan: &ImageScanRow)` (`RefreshCandidatesJob::queueCheck`)
- `pub fn message(scan: &ImageScanRow, base: bool) -> Option<String>`
- `pub fn better_base(scan: &ImageScanRow) -> Option<BetterBase>` with `pub struct BetterBase { pub from: String, pub to: String, pub entry: serde_json::Value }`
- `pub async fn base_image(jobs: &Jobs, scan: &ImageScanRow) -> Result<Option<BaseImageUpdate>, String>` with `pub struct BaseImageUpdate { pub from: String, pub to: String, pub fixed: usize, pub total: usize }`

`scan_jobs`:
- `pub struct ScanResourceImages { pub resource_type: String, pub resource_id: i64, pub deployment_queue_id: Option<i64>, pub image_refs: Vec<String>, pub retry_when_image_missing: bool, pub base_image_ref: Option<String>, pub pull_when_missing: bool }` with `ScanResourceImages::new(resource: &ScanOwner) -> Self` (the other fields at their defaults)
- `impl Jobs { pub fn dispatch_scan_resource_images(&self, job: ScanResourceImages, delay_seconds: u64) }`
- `pub async fn dispatch_after_start(jobs: &Jobs, resource: &ScanOwner)` (`ScanResourceImagesJob::dispatchAfterStart`; never fails)
- `pub async fn mark_queued(jobs: &Jobs, resource: &ScanOwner, image_refs: &[String]) -> Result<(), String>` (`ScanResourceImagesJob::markQueued`)
- `impl Jobs { pub fn dispatch_image_rescan(&self, server_id: i64) }` (`ImageRescanJob::dispatch`)
- `pub fn can_run_on(server: &ScanServer) -> bool` (`ImageRescanJob::canRunOn`)
- `pub fn failed_days(server_id: i64) -> i64`, `pub fn is_failure_notified(server_id: i64) -> bool`, `pub fn mark_failure_notified(server_id: i64)`

`host`:
- `impl Jobs { pub fn dispatch_scan_host_packages(&self, server_id: i64) }` (`ScanHostPackagesJob::dispatch`)
- `pub fn dispatch_manual(jobs: &Jobs, server: &ScanServer)` (`ScanHostPackagesJob::dispatchManual`)
- `pub fn running_cache_key(server_id: i64) -> String`

`update`:
- `pub async fn blocked_reason(jobs: &Jobs, scan: &ImageScanRow, owner: &ScanOwner) -> Result<Option<String>, String>` (`UpdateResourceImage::blockedReason`)
- `pub mod image_start_pin` with `put(owner: &ScanOwner, reference: &str, digest: &str) -> Result<(), InvalidArgument>`, `take(owner: &ScanOwner, server_id: Option<i64>) -> Pins`, `forget(owner: &ScanOwner)`, `for_images(pins: &Pins, images: &[String]) -> Pins`, `commands(pins: &Pins, prefix: &str) -> Vec<String>`, and `pub type Pins = Vec<(String, Pin)>` (keyed by the normalized image ref, in insertion order) with `pub struct Pin { pub reference: String, pub pull: String }`
- `pub mod dockerfile_base_image` with `pub fn final_stage(dockerfile: &str, target: Option<&str>) -> Option<String>`

`gate`:
- `pub mod security_gate` (`SecurityGate::evaluate`, `logLines`, `stored`) and the deployment entry point (`DeploymentImageScan`), named in the module documentation of `gate`.

The engine calls the view models (`views`, see "View models and API") directly: `resource_scans::store_security_status`
(`SecurityStatus::store`), the alert and digest helpers of `notify`, and the gate (`SecurityCard::blockedImageAction`).

## What the scan pages and API (scanui) call

- Read: `image_scans::find_resource`, `resource_scans::{current, current_for_many, findings, unchecked_compose_refs,
  runs_other_image, advisory_url, exploit_likelihood}`, `policy::{resolve, off_state, inherited_for, for_scope}`,
  `rules::{RelevanceRules, is_relevant, exploit_signal}`, `prepare::{state, failed_step, database_built_at,
  disk_used_bytes}`, `cache::get(&scan_image::progress_cache_key(owner))`, `cache::get(&scan_image::last_error_cache_key(owner))`.
- Write: `image_scans::{save_policy, create_ignore, delete_ignore}` then `Jobs::dispatch_refresh_image_scan_summaries(team_id)`,
  `prepare::set_server_image_scanning`, `scan_jobs` (`mark_queued` then `dispatch_scan_resource_images` for "Scan now"),
  `host::dispatch_manual`, `update` (switch to a fixed version), `sbom_document::{cyclone_dx, spdx}` for downloads.
- `resource_scans::store_security_status` is the one place where `SecurityStatus::store($scan)` must write
  `summary.security`; every path that writes a summary calls it.
- `events::set_event_sink` at startup, to forward `ImageScanUpdated` to the browser.

## View models and API

The view models are in `coolify_jobs::image_scan::views` (one module per upstream class), their queries in
`coolify_db::image_scans::views`. They take `jobs: &Jobs` first when they read the database, a scan together with its
owner (`views::OwnedScan`, or `(&ImageScanRow, &ScanOwner)`), and `now` (`views::now()`) where a text says how long
ago something happened. Statuses serialize as upstream stores them (`ResourceSecurity::to_value`, `from_value`), the
card rows with serde (upstream's array keys).

| Upstream | Rust |
|---|---|
| `SecurityStatusType`, `SecurityActionType` | `views::{SecurityStatusType, SecurityActionType}` (`value()`, `from_value()`) |
| `SecurityAction`, `ResourceSecurity` (`whyEntry`) | `views::{SecurityAction, ActionPart, ResourceSecurity, Why}`; `ResourceSecurity::{primary, fact, fact_int, action_stats}` |
| `SecurityActions` | `views::actions::{for_scan, fix_states, image_kind, BUILDER_TEXT, COOLIFY_TEXT}`; `ScanActions::{for_finding, eol_action}`; `FixStates::state` |
| `SecurityStatus` | `views::status::{for_scan, for_resource, stored, stored_many, store, why_for, top_why, versions_checked, is_blocked_check}` and the `REASON_*` constants |
| `SecuritySentences` | `views::sentences::{headline, lines, check_reason, why_more, badge, database_update_confirmation, image_sentence, version_check_running, no_fix_action, attack_note, no_fix_advice, provenance, registry_name, images_needing_update, known_issues, short_digest, CHECKING_BADGE}` |
| `SecurityCard` | `views::card::{card, images, why_row, cvss_severity, version_check_notes, key, running_deployment, gate_action, blocked_image_action, knows_running_commit, short_commit}`; `CardUser` carries `can('update')`, `can('deploy')`, and `can('update', $server)` |
| `TeamVulnerabilities` | `views::team::{TeamVulnerabilities::{load, retain, search, search_was_capped}, images_line, status_label, action, update_now_resources, servers_not_scanning, search_term, severity_of, vulnerabilities_path, vulnerabilities_url, STATUS_ORDER, STATE_LABELS, NO_FIX_ACTION, MAX_DECODED_SCANS}` |
| `VulnerabilityMarkers` | `views::markers::{markers_for, marker}` |
| `ExportCells`, `FindingsExport`, `FleetExport` | `views::export::{markdown_cell, csv_cell, csv_document, markdown_table, markdown_link, findings_markdown, findings_ids, findings_csv, findings_json, fleet_entries, fleet_markdown, fleet_csv, fleet_json, FIX_STATES}` |
| `ResourceScans::current($resource)` with owners | `views::current_owned(jobs, resource)` |

Differences a page must make up for:

- `TeamVulnerabilities::load()` loads every resource of the team; upstream's models hide what a member or token
  limited to some projects cannot see. Call `retain(|e| can_access(ctx, &e.resource, AccessLevel::Read))`
  (`coolify_api::routes::vulnerabilities::can_access`) before showing, searching, or exporting.
  `update_now_resources` returns the resources (sorted by uuid); keep the visible ones and take their uuids.
- `route()` URLs are paths here (`team::vulnerabilities_path`, the registry login link `/server/{uuid}/registries`);
  `export::fleet_entries` takes the instance URL to make them absolute.
- `card::CardUser` replaces `$user->can(...)`; the page computes it with the policies of coolify-api.

### Per page

- **`ImageScan\Vulnerabilities`** (Security page of an application, service, or database):
  `image_scans::find_resource`, `image_scans::owners`, `views::current_owned`, `RelevanceRules::for_resource` per owner,
  `status::for_scan` per scan, `status::for_resource`, `card::card`; the findings list with `actions::fix_states`,
  `rules::{exploit_signal, is_cpe_only_os_match}`, `team::severity_of`, `resource_scans::advisory_url`; the progress
  and last error of a scan from `cache::get(&scan_image::progress_cache_key(owner))` and
  `last_error_cache_key(owner)`; off state with `policy::off_state`. Actions: "Scan now" (`scan_jobs::mark_queued`,
  `Jobs::dispatch_scan_resource_images`), `updateImage` and `updateImages` through
  `coolify_api::routes::image_scan::{update_resource_image, update_resource_images}` (the page finds the action with
  `status::for_resource` and `card::key`, and makes the `updateRefusal` checks itself), `rebuild` with
  `card::{running_deployment, knows_running_commit}`, `ignore` with `image_scans::create_ignore` then
  `resource_scans::refresh_summaries` / `Jobs::dispatch_refresh_image_scan_summaries`, `removeImageOverride`;
  copy and download with `export::{findings_ids, findings_markdown, findings_csv, findings_json}` and
  `export::FIX_STATES`. The SBOM links go to the route of `coolify_api::routes::image_scan` (below).
- **`ImageScan\Packages`**: `image_scans::find_resource`, `views::current_owned`, `image_scans::find_sbom_by_id`,
  `sbom_document::packages`.
- **`ImageScan\FleetVulnerabilities`** (team Security page `/vulnerabilities`): `TeamVulnerabilities::load` (then
  `retain`), `search` for a vulnerability ID or package, `team::{status_label, action, images_line,
  servers_not_scanning, STATUS_ORDER, STATE_LABELS}`, `markers::marker`; copy and download with
  `export::{fleet_entries, fleet_markdown, fleet_csv, fleet_json}`.
- **`ImageScan\ScanSettings`** (team, server, and app rules, ignores): `policy::{for_scope, inherited_for, resolve}`,
  `image_scans::{save_policy, ignores_of_team, delete_ignore}`, `resource_scans::{refresh_summaries, current}`,
  `Jobs::dispatch_refresh_image_scan_summaries`.
- **`ImageScan\ServerImageScanning`**, **`TeamImageScanning`**, **`InstanceImageScanning`**: `prepare::{state,
  failed_step, database_built_at, disk_used_bytes, set_server_image_scanning, prepare_scanner, ensure_managed_here}`,
  `ScanServer::host().{memory_options_mb, cpu_options}`, `image_scans::views::update_scanner_limits`,
  `scan_image::{server_lock, acquire_server_lock}` and `commands` for "Delete scanner data".
- **`Server\Security\HostVulnerabilities`**: `coolify_db::image_scans::host` (the stored host scan),
  `host::dispatch_manual`, and the exports `export::{findings_ids, findings_markdown, findings_csv, findings_json}`
  with `FIX_STATES` (resource `{"type": "server", "name": …, "uuid": …}`). The page reloads on
  `events::ScanEventSink::server_patch_check_finished`.
- **`Server\Security\Index`**: the server Security page; it only links the host-key, terminal-access, and
  vulnerabilities sections (no view model).
- **Resource lists, canvas, menus** (`<x-image-scan.marker>`, the security dot): `markers::markers_for` for many
  resources, `status::stored` for one. **Dashboard callout**: `team::update_now_resources`. **Deploy gate popover**:
  `card::gate_action`.
- **Notifications**: `notify` calls `actions::for_scan`, `status::{versions_checked, why_for, top_why}`,
  `team::{TeamVulnerabilities::load, action, vulnerabilities_url}` and `sentences::badge`; both notifications are
  `Notification::ImageExploitedProblem` and `Notification::ImageVulnerabilityDigest` (event `image_vulnerabilities`).

### API and routes (coolify-api)

| Upstream | Rust |
|---|---|
| `GET /api/v1/vulnerabilities` | `routes::vulnerabilities::index` |
| `GET /api/v1/{applications,services,databases}/{uuid}/vulnerabilities` | `routes::vulnerabilities::{application, service, database}` |
| `GET` / `PATCH /api/v1/servers/{uuid}/image-scanning` | `routes::server_image_scanning::{show, update}` |
| `project.{application,service,database}.vulnerabilities.sbom` | `routes::image_scan::routes` (session guard) |
| `UpdateResourceImage` authorization and `Auditable` rows | `routes::image_scan::{update_resource_image, update_resource_images}` |

The API routes need the background jobs (`AppState::jobs`) and answer 501 without them.

## Schedule and the deployment job

- `schedule::spawn_server_rescan` (daily rescan per server, cron `image_rescan_cron`) and `schedule::spawn_digests`
  (daily digest per team, cron `image_scan_digest_cron`) are called from `server_ops::scheduler` (two marked lines).
  The host package scan runs from the rescan job; upstream's `Console\Kernel` has no scan entry.
- The deployment job calls `gate::DeploymentImageScan`; the module documentation of `gate` lists each upstream call
  site and the method to call there. The job implements `gate::DeploymentContext`.

## Not done and not verified

Nothing here was compiled, run, or compared with the reference. The scanner commands are tested as strings (and
the host filter `awk` with `sh`, when present); no scan ran on a server.

- **Plugin events**: the two image notifications have no plugin event name (`plugin_events::event_kind` answers
  `None` for them); coolify-domain would need one.
- **Carbon**: `views::diff_for_humans` follows Carbon's defaults (one part, cut, never zero) with months counted by
  the calendar; not compared with Carbon for every edge.
- **Deletion of a scan owner**: `image_scans::delete_resource_data` (`HasImageScans`) runs in the delete jobs of
  applications, services (for each service application and database) and databases, before their rows go; upstream
  runs it in the model's `deleting` hook, when the row is soft deleted.
- **Event delivery**: `events::set_event_sink` is not set: coolify-rs has no Echo/websocket channel to the browser,
  so `ImageScanUpdated` and `ServerPatchCheckFinished` go nowhere and the scan pages must poll.
- **The deployment job** implements `DeploymentContext` in `applications/deploy/image_scan.rs` and calls the steps
  at upstream's call sites; none of it ran.
- **Cache**: progress, last errors, prepare state, start pins, queued marks and the server lock live in process memory
  (`cache`), not in Redis: they are lost on restart, and an upstream instance on the same database does not see them
  (two instances scanning one server can run at the same time).
- **Columns of `server_settings`** are read with their own query (`server_scan_settings`); `ServerRow.settings` drops
  them unless the schema agent adds them to `records::SERVER_SETTING.known`.
