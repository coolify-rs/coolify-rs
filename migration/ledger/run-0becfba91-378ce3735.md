# Migration run `run-0becfba91-378ce3735`

- Upstream range: `0becfba91130..378ce3735c45` (210 first-parent commits)
- Stage: Analyzed
- Branch: (analysis only)
- Verdict: **not validated**
- Recommended next action: analysis only: re-run with generation to produce a patch

## Plan

- Risk: **High**
  - security-sensitive (148 signals)
  - touches authentication/authorization
  - migration 2026_10_29_100000_create_image_scanning_tables changes mapped tables ["server_settings", "teams"]
  - migration 2026_10_30_120000_add_ssh_host_key_to_server_settings_table changes mapped tables ["server_settings"]
  - migration 2026_11_03_194650_add_notification_sender_to_instance_settings_table changes mapped tables ["instance_settings"]
  - migration 2026_11_04_080000_add_auto_update_attempt_to_instance_settings_table changes mapped tables ["instance_settings"]
  - migration 2026_11_06_113843_set_valkey_as_standalone_redis_default_image changes mapped tables ["standalone_redis"]
  - migration 2026_11_06_194613_add_ssh_host_key_check_enforced_to_server_settings_table changes mapped tables ["server_settings"]
  - migration 2026_11_08_120000_drop_image_scanning_columns_from_instance_settings changes mapped tables ["instance_settings"]
  - migration 2026_11_08_120000_drop_image_scanning_columns_from_instance_settings is destructive/raw-SQL/encryption (destructive=true, raw_sql=false, encryption=false)
  - migration 2026_11_09_120000_drop_unused_failure_columns_from_scheduled_tasks is destructive/raw-SQL/encryption (destructive=true, raw_sql=false, encryption=false)
  - migration 2026_11_09_130000_add_image_scan_memory_mb_to_server_settings_table changes mapped tables ["server_settings"]
  - migration 2026_11_09_140000_add_image_scanner_time_and_cpu_limits_to_server_settings_table changes mapped tables ["server_settings"]
  - API routes changed in an implemented component
  - category ApiChange
  - category DomainLogic
  - category DatabaseSchema
  - category BackgroundJobs
  - category DependencyUpgrade
  - category ExternalIntegrations
  - category Configuration
  - category DeploymentEngine
  - category ContainerIntegration
  - deployment/container behaviour in an implemented component
- Security sensitive: true
- Categories: [Documentation, Tests, Frontend, ApiChange, DatabaseSchema, DomainLogic, AuthenticationAuthorization, DeploymentEngine, ContainerIntegration, Configuration, ExternalIntegrations, BackgroundJobs, DependencyUpgrade, BuildRelease, ServiceTemplates, Unclassified]
- Mapping coverage of behaviour-relevant files: 16% (heuristic, uncalibrated)

### Escalation

Decision required: approve or reject the generated patch and assign the semantic port of the listed components

- high risk: requires explicit human approval before integration
- 334 behaviour-relevant files are not covered by the mapping registry

### Affected Rust components

- `api.auth-chain` (Verified): path app/Http/Kernel.php; path app/Http/Middleware/ApiAbility.php; path app/Http/Middleware/ApiSensitiveData.php; upstream test tests/Feature/ApiTokenLivewireAuthorizationTest.php; table instance_settings; table instance_settings; table instance_settings
- `api.teams` (Verified): path app/Http/Controllers/Api/TeamController.php; table teams
- `api.projects` (Verified): path app/Traits/HasSafeStringAttribute.php; upstream test tests/Feature/ResourceAccessConsistencyTest.php; symbol app/Support/ValidationPatterns.php::nameRules()
- `api.route-inventory` (Verified): path app/Http/Kernel.php; path routes/api.php
- `db.schema-guard` (Implemented): path database/migrations/2026_10_29_100000_create_image_scanning_tables.php; path database/migrations/2026_10_30_120000_add_git_ssh_known_hosts_to_applications_table.php; path database/migrations/2026_10_30_120000_add_ssh_host_key_to_server_settings_table.php; path database/migrations/2026_10_31_161621_add_type_to_s3_storages_table.php; path database/migrations/2026_11_01_170003_add_sftp_host_key_to_s3_storages_table.php; path database/migrations/2026_11_03_072701_add_auto_cancel_superseded_deployments.php; path database/migrations/2026_11_03_120133_add_is_mcp_to_application_deployment_queues_table.php; path database/migrations/2026_11_03_171936_add_skip_tls_verification_to_s3_storages_table.php; path database/migrations/2026_11_03_192547_add_is_removed_from_compose_to_service_parts.php; path database/migrations/2026_11_03_192735_add_failure_tracking_to_scheduled_tasks_table.php; path database/migrations/2026_11_03_194650_add_notification_sender_to_instance_settings_table.php; path database/migrations/2026_11_03_195902_add_github_deployments_columns.php; path database/migrations/2026_11_03_202635_add_use_shell_to_scheduled_tasks_table.php; path database/migrations/2026_11_03_225251_add_named_smtp_servers_to_email_settings.php; path database/migrations/2026_11_04_080000_add_auto_update_attempt_to_instance_settings_table.php; path database/migrations/2026_11_04_103949_add_resend_sender_to_email_settings.php; path database/migrations/2026_11_05_163404_add_include_network_hosts_in_build_to_application_settings.php; path database/migrations/2026_11_05_224732_drop_unused_indexes_from_audit_events_table.php; path database/migrations/2026_11_06_083240_update_standalone_database_default_images.php; path database/migrations/2026_11_06_113843_set_valkey_as_standalone_redis_default_image.php; path database/migrations/2026_11_06_145819_create_server_host_scans_table.php; path database/migrations/2026_11_06_165709_create_stars_table.php; path database/migrations/2026_11_06_194613_add_ssh_host_key_check_enforced_to_server_settings_table.php; path database/migrations/2026_11_08_120000_drop_image_scanning_columns_from_instance_settings.php; path database/migrations/2026_11_09_120000_drop_unused_failure_columns_from_scheduled_tasks.php; path database/migrations/2026_11_09_130000_add_image_scan_memory_mb_to_server_settings_table.php; path database/migrations/2026_11_09_140000_add_image_scanner_time_and_cpu_limits_to_server_settings_table.php
- `api.servers.subroutes` (Planned): path app/Http/Controllers/Api/ServerImageScanningController.php; path app/Http/Controllers/Api/ServerTransferController.php; path app/Http/Controllers/Api/ServersController.php; path app/Policies/ServerPolicy.php
- `api.private-keys` (Verified): path app/Http/Controllers/Api/SecurityController.php; path app/Models/PrivateKey.php; upstream test tests/Feature/Api/PrivateKeyUpdateApiTest.php; upstream test tests/Feature/PrivateKeyFingerprintTeamScopeTest.php; symbol bootstrap/helpers/shared.php::generate_random_name()
- `ui.session` (Implemented): path app/Providers/FortifyServiceProvider.php; table teams
- `api.servers` (Verified): path app/Actions/Server/DeleteServer.php; path app/Http/Controllers/Api/ServersController.php; path app/Models/Server.php; path app/Models/ServerSetting.php; path app/Models/SharedEnvironmentVariable.php; table server_settings; table server_settings; table server_settings; table server_settings; table server_settings
- `ui.styles` (Verified): path resources/css/app.css; path resources/views/components/reicon.blade.php
- `ui.frontend` (Implemented): path resources/views/components/backup-s3-storage.blade.php; path resources/views/components/forms/domain-input.blade.php; path resources/views/components/forms/inherited-listbox.blade.php; path resources/views/components/forms/input.blade.php; path resources/views/components/forms/repository-paths.blade.php; path resources/views/components/modal-confirmation.blade.php; path resources/views/components/modal-input.blade.php; path resources/views/components/navbar.blade.php; path resources/views/components/server/remove-unused-packages-button.blade.php; path resources/views/components/server/sidebar.blade.php; path resources/views/components/server/status-summary.blade.php; path resources/views/components/theme-controls.blade.php; path resources/views/components/top-breadcrumb.blade.php; path resources/views/components/top-user-menu.blade.php; path resources/views/layouts/app.blade.php; path resources/views/livewire/dashboard.blade.php; path resources/views/livewire/partials/smtp-server-picker.blade.php; path resources/views/livewire/project/application/general.blade.php; path resources/views/livewire/project/resource/index.blade.php; path resources/views/livewire/project/service/import-backup.blade.php; path resources/views/livewire/project/service/volume-backup/index.blade.php; path resources/views/livewire/project/shared/resource-operations.blade.php; path resources/views/livewire/security/private-key/index.blade.php; path resources/views/livewire/security/private-key/partials/key-field.blade.php; path resources/views/livewire/security/private-key/rotate.blade.php; path resources/views/livewire/security/private-key/show.blade.php; path resources/views/livewire/server/delete.blade.php; path resources/views/livewire/server/index.blade.php; path resources/views/livewire/server/navbar.blade.php; path resources/views/livewire/server/new/by-ip.blade.php; path resources/views/livewire/server/partials/force-disabled-callout.blade.php; path resources/views/livewire/server/security/host-key.blade.php; path resources/views/livewire/server/security/patches.blade.php; path resources/views/livewire/server/sentinel.blade.php; path resources/views/livewire/server/show.blade.php; path resources/views/livewire/settings-backup.blade.php; path resources/views/livewire/star-toggle.blade.php; path resources/views/livewire/storage/server-endpoint-fields.blade.php; path resources/views/livewire/storage/sftp-login-fields.blade.php
- `ui.read-models` (Verified): path app/Livewire/Concerns/ManagesServerPackageUpdates.php; path app/Livewire/Dashboard.php; path app/Livewire/Project/CloneMe.php; path app/Livewire/Project/Service/ImportBackup.php; path app/Livewire/Project/Shared/ResourceOperations.php; path app/Livewire/Security/PrivateKey/Rotate.php; path app/Livewire/Security/PrivateKey/Show.php; path app/Livewire/Server/Delete.php; path app/Livewire/Server/New/ByIp.php; path app/Livewire/Server/Security/Patches.php; path app/Livewire/Server/Show.php; path app/Livewire/StarToggle.php; path app/Livewire/Team/Member.php; symbol app/Models/PrivateKey.php::generateNewKeyPair(); symbol bootstrap/helpers/shared.php::generateSSHKey(); table server_settings; table teams; table server_settings; table server_settings; table server_settings; table server_settings
- `deployment.engine` (Planned): path app/Http/Controllers/Api/DeployController.php; path app/Jobs/ApplicationDeploymentJob.php; path app/Traits/ExecuteRemoteCommand.php; path bootstrap/helpers/applications.php; table application_deployment_queues; table application_settings; table applications; table applications; table application_deployment_queues; table application_settings; table application_deployment_queues; table application_deployment_queues; table application_settings
- `remote.execution` (Verified): path app/Helpers/SshMultiplexingHelper.php; path app/Traits/SshRetryable.php; path bootstrap/helpers/remoteProcess.php
- `server.validation` (Verified): path resources/views/components/process-dialog.blade.php; table server_settings; table server_settings; table server_settings; table server_settings; table server_settings
- `databases.standalone` (Verified): path app/Actions/Database/StartClickhouse.php; path app/Actions/Database/StartDragonfly.php; path app/Actions/Database/StartKeydb.php; path app/Actions/Database/StartMariadb.php; path app/Actions/Database/StartMongodb.php; path app/Actions/Database/StartMysql.php; path app/Actions/Database/StartPostgresql.php; path app/Actions/Database/StartRedis.php; path app/Actions/Database/StopDatabase.php; path app/Actions/Docker/GetContainersStatus.php; path app/Http/Controllers/Api/DatabasesController.php; path app/Livewire/Project/New/Select.php; path app/Livewire/Project/Resource/Create.php; path app/Livewire/Project/Resource/Index.php; path app/Livewire/Project/Shared/Danger.php; path app/Models/StandalonePostgresql.php; path app/Traits/ExecutesDatabaseStartCommands.php; path app/Traits/HasSecretManager.php; path resources/views/components/database/configuration-sidebar.blade.php; path resources/views/livewire/project/database/backup-edit/general.blade.php; path resources/views/livewire/project/database/backup-edit/retention.blade.php; path resources/views/livewire/project/database/backup-executions.blade.php; path resources/views/livewire/project/database/backup/index.blade.php; path resources/views/livewire/project/database/configuration.blade.php; path resources/views/livewire/project/database/heading.blade.php; path resources/views/livewire/project/database/import-form.blade.php; path resources/views/livewire/project/database/import.blade.php; path resources/views/livewire/project/database/redis/general.blade.php; path resources/views/livewire/project/new/select.blade.php; path resources/views/livewire/project/resource/index.blade.php; table standalone_redis

### Upstream migrations

| migration | tables | mapped | destructive | raw SQL | encryption |
|---|---|---|---|---|---|
| `2026_10_29_100000_create_image_scanning_tables` | application_deployment_queues, application_settings, applications, image_sboms, image_scan_ignores, image_scan_notification_states, image_scan_policies, image_scans, server_settings, teams, telegram_notification_settings | server_settings, teams | false | false | false |
| `2026_10_30_120000_add_git_ssh_known_hosts_to_applications_table` | applications |  | false | false | false |
| `2026_10_30_120000_add_ssh_host_key_to_server_settings_table` | server_settings | server_settings | false | false | false |
| `2026_10_31_161621_add_type_to_s3_storages_table` | s3_storages |  | false | false | false |
| `2026_11_01_170003_add_sftp_host_key_to_s3_storages_table` | s3_storages |  | false | false | false |
| `2026_11_03_072701_add_auto_cancel_superseded_deployments` | application_deployment_queues, application_settings |  | false | false | false |
| `2026_11_03_120133_add_is_mcp_to_application_deployment_queues_table` | application_deployment_queues |  | false | false | false |
| `2026_11_03_171936_add_skip_tls_verification_to_s3_storages_table` | s3_storages |  | false | false | false |
| `2026_11_03_192547_add_is_removed_from_compose_to_service_parts` | service_applications, service_databases |  | false | false | false |
| `2026_11_03_192735_add_failure_tracking_to_scheduled_tasks_table` | scheduled_tasks |  | false | false | false |
| `2026_11_03_194650_add_notification_sender_to_instance_settings_table` | instance_settings | instance_settings | false | false | false |
| `2026_11_03_195902_add_github_deployments_columns` | application_deployment_queues, github_apps |  | false | false | false |
| `2026_11_03_202635_add_use_shell_to_scheduled_tasks_table` | scheduled_tasks |  | false | false | false |
| `2026_11_03_225251_add_named_smtp_servers_to_email_settings` |  |  | false | false | false |
| `2026_11_04_080000_add_auto_update_attempt_to_instance_settings_table` | instance_settings | instance_settings | false | false | false |
| `2026_11_04_103949_add_resend_sender_to_email_settings` |  |  | false | false | false |
| `2026_11_05_163404_add_include_network_hosts_in_build_to_application_settings` | application_settings |  | false | false | false |
| `2026_11_05_224732_drop_unused_indexes_from_audit_events_table` | audit_events |  | false | false | false |
| `2026_11_06_083240_update_standalone_database_default_images` |  |  | false | false | false |
| `2026_11_06_113843_set_valkey_as_standalone_redis_default_image` | standalone_redis | standalone_redis | false | false | false |
| `2026_11_06_145819_create_server_host_scans_table` | server_host_scans |  | false | false | false |
| `2026_11_06_165709_create_stars_table` | stars |  | false | false | false |
| `2026_11_06_194613_add_ssh_host_key_check_enforced_to_server_settings_table` | server_settings | server_settings | false | false | false |
| `2026_11_08_120000_drop_image_scanning_columns_from_instance_settings` | instance_settings | instance_settings | true | false | false |
| `2026_11_09_120000_drop_unused_failure_columns_from_scheduled_tasks` | scheduled_tasks |  | true | false | false |
| `2026_11_09_130000_add_image_scan_memory_mb_to_server_settings_table` | server_settings | server_settings | false | false | false |
| `2026_11_09_140000_add_image_scanner_time_and_cpu_limits_to_server_settings_table` | server_settings | server_settings | false | false | false |

### API route changes

- added `GET api/v1/applications/{uuid}/vulnerabilities` → App\Http\Controllers\Api\VulnerabilitiesController@application
- added `GET api/v1/databases/{uuid}/vulnerabilities` → App\Http\Controllers\Api\VulnerabilitiesController@database
- added `GET api/v1/gitea-apps` → App\Http\Controllers\Api\GiteaController@list_gitea_apps
- added `GET api/v1/servers/{uuid}/image-scanning` → App\Http\Controllers\Api\ServerImageScanningController@show
- added `GET api/v1/services/{uuid}/vulnerabilities` → App\Http\Controllers\Api\VulnerabilitiesController@service
- added `GET api/v1/vulnerabilities` → App\Http\Controllers\Api\VulnerabilitiesController@index
- added `PATCH api/v1/servers/{uuid}/image-scanning` → App\Http\Controllers\Api\ServerImageScanningController@update
- added `POST api/v1/applications/private-gitea-app` → App\Http\Controllers\Api\ApplicationsController@create_private_gitea_app_application
- added `POST api/v1/servers/{uuid}/host-key/reset` → App\Http\Controllers\Api\ServersController@reset_host_key

### Actions

- [A] `regenerate_route_inventory`: regenerate compat/api/upstream-routes.json
- [A] `regenerate_migration_inventory`: regenerate compat/baseline/supported-migrations.txt
- [B] `update_schema_mapping:server_settings`: update the Rust column set / serializers for `server_settings` (Alter: boolean:is_image_scan_enabled, unsignedInteger:image_candidate_scans_today, date:image_candidate_scans_date)
- [B] `update_schema_mapping:teams`: update the Rust column set / serializers for `teams` (Alter: boolean:scan_images_on_new_servers)
- [B] `update_schema_mapping:server_settings`: update the Rust column set / serializers for `server_settings` (Alter: text:ssh_host_key, string:ssh_host_key_source, timestamp:ssh_host_key_trusted_at, text:ssh_host_key_pending, boolean:is_ssh_host_key_check_disabled)
- [B] `update_schema_mapping:instance_settings`: update the Rust column set / serializers for `instance_settings` (Alter: text:notification_from_address, text:notification_from_name)
- [B] `update_schema_mapping:instance_settings`: update the Rust column set / serializers for `instance_settings` (Alter: string:auto_update_target_version, timestamp:auto_update_started_at)
- [B] `update_schema_mapping:standalone_redis`: update the Rust column set / serializers for `standalone_redis` (Alter: string:image)
- [B] `update_schema_mapping:server_settings`: update the Rust column set / serializers for `server_settings` (Alter: boolean:is_ssh_host_key_check_enforced)
- [B] `update_schema_mapping:instance_settings`: update the Rust column set / serializers for `instance_settings` (Alter: )
- [B] `update_schema_mapping:server_settings`: update the Rust column set / serializers for `server_settings` (Alter: unsignedInteger:image_scan_memory_mb)
- [B] `update_schema_mapping:server_settings`: update the Rust column set / serializers for `server_settings` (Alter: unsignedInteger:image_scan_syft_timeout, unsignedInteger:image_scan_grype_timeout, decimal:image_scan_cpus)
- [D] `review_component:api.auth-chain`: port behaviour change into api.auth-chain (path app/Http/Kernel.php; path app/Http/Middleware/ApiAbility.php; path app/Http/Middleware/ApiSensitiveData.php; upstream test tests/Feature/ApiTokenLivewireAuthorizationTest.php; table instance_settings; table instance_settings; table instance_settings)
- [D] `review_component:api.teams`: port behaviour change into api.teams (path app/Http/Controllers/Api/TeamController.php; table teams)
- [D] `review_component:api.projects`: port behaviour change into api.projects (path app/Traits/HasSafeStringAttribute.php; upstream test tests/Feature/ResourceAccessConsistencyTest.php; symbol app/Support/ValidationPatterns.php::nameRules())
- [D] `review_component:api.route-inventory`: port behaviour change into api.route-inventory (path app/Http/Kernel.php; path routes/api.php)
- [D] `review_component:db.schema-guard`: port behaviour change into db.schema-guard (path database/migrations/2026_10_29_100000_create_image_scanning_tables.php; path database/migrations/2026_10_30_120000_add_git_ssh_known_hosts_to_applications_table.php; path database/migrations/2026_10_30_120000_add_ssh_host_key_to_server_settings_table.php; path database/migrations/2026_10_31_161621_add_type_to_s3_storages_table.php; path database/migrations/2026_11_01_170003_add_sftp_host_key_to_s3_storages_table.php; path database/migrations/2026_11_03_072701_add_auto_cancel_superseded_deployments.php; path database/migrations/2026_11_03_120133_add_is_mcp_to_application_deployment_queues_table.php; path database/migrations/2026_11_03_171936_add_skip_tls_verification_to_s3_storages_table.php; path database/migrations/2026_11_03_192547_add_is_removed_from_compose_to_service_parts.php; path database/migrations/2026_11_03_192735_add_failure_tracking_to_scheduled_tasks_table.php; path database/migrations/2026_11_03_194650_add_notification_sender_to_instance_settings_table.php; path database/migrations/2026_11_03_195902_add_github_deployments_columns.php; path database/migrations/2026_11_03_202635_add_use_shell_to_scheduled_tasks_table.php; path database/migrations/2026_11_03_225251_add_named_smtp_servers_to_email_settings.php; path database/migrations/2026_11_04_080000_add_auto_update_attempt_to_instance_settings_table.php; path database/migrations/2026_11_04_103949_add_resend_sender_to_email_settings.php; path database/migrations/2026_11_05_163404_add_include_network_hosts_in_build_to_application_settings.php; path database/migrations/2026_11_05_224732_drop_unused_indexes_from_audit_events_table.php; path database/migrations/2026_11_06_083240_update_standalone_database_default_images.php; path database/migrations/2026_11_06_113843_set_valkey_as_standalone_redis_default_image.php; path database/migrations/2026_11_06_145819_create_server_host_scans_table.php; path database/migrations/2026_11_06_165709_create_stars_table.php; path database/migrations/2026_11_06_194613_add_ssh_host_key_check_enforced_to_server_settings_table.php; path database/migrations/2026_11_08_120000_drop_image_scanning_columns_from_instance_settings.php; path database/migrations/2026_11_09_120000_drop_unused_failure_columns_from_scheduled_tasks.php; path database/migrations/2026_11_09_130000_add_image_scan_memory_mb_to_server_settings_table.php; path database/migrations/2026_11_09_140000_add_image_scanner_time_and_cpu_limits_to_server_settings_table.php)
- [D] `review_component:api.private-keys`: port behaviour change into api.private-keys (path app/Http/Controllers/Api/SecurityController.php; path app/Models/PrivateKey.php; upstream test tests/Feature/Api/PrivateKeyUpdateApiTest.php; upstream test tests/Feature/PrivateKeyFingerprintTeamScopeTest.php; symbol bootstrap/helpers/shared.php::generate_random_name())
- [D] `review_component:ui.session`: port behaviour change into ui.session (path app/Providers/FortifyServiceProvider.php; table teams)
- [D] `review_component:api.servers`: port behaviour change into api.servers (path app/Actions/Server/DeleteServer.php; path app/Http/Controllers/Api/ServersController.php; path app/Models/Server.php; path app/Models/ServerSetting.php; path app/Models/SharedEnvironmentVariable.php; table server_settings; table server_settings; table server_settings; table server_settings; table server_settings)
- [D] `review_component:ui.styles`: port behaviour change into ui.styles (path resources/css/app.css; path resources/views/components/reicon.blade.php)
- [D] `review_component:ui.frontend`: port behaviour change into ui.frontend (path resources/views/components/backup-s3-storage.blade.php; path resources/views/components/forms/domain-input.blade.php; path resources/views/components/forms/inherited-listbox.blade.php; path resources/views/components/forms/input.blade.php; path resources/views/components/forms/repository-paths.blade.php; path resources/views/components/modal-confirmation.blade.php; path resources/views/components/modal-input.blade.php; path resources/views/components/navbar.blade.php; path resources/views/components/server/remove-unused-packages-button.blade.php; path resources/views/components/server/sidebar.blade.php; path resources/views/components/server/status-summary.blade.php; path resources/views/components/theme-controls.blade.php; path resources/views/components/top-breadcrumb.blade.php; path resources/views/components/top-user-menu.blade.php; path resources/views/layouts/app.blade.php; path resources/views/livewire/dashboard.blade.php; path resources/views/livewire/partials/smtp-server-picker.blade.php; path resources/views/livewire/project/application/general.blade.php; path resources/views/livewire/project/resource/index.blade.php; path resources/views/livewire/project/service/import-backup.blade.php; path resources/views/livewire/project/service/volume-backup/index.blade.php; path resources/views/livewire/project/shared/resource-operations.blade.php; path resources/views/livewire/security/private-key/index.blade.php; path resources/views/livewire/security/private-key/partials/key-field.blade.php; path resources/views/livewire/security/private-key/rotate.blade.php; path resources/views/livewire/security/private-key/show.blade.php; path resources/views/livewire/server/delete.blade.php; path resources/views/livewire/server/index.blade.php; path resources/views/livewire/server/navbar.blade.php; path resources/views/livewire/server/new/by-ip.blade.php; path resources/views/livewire/server/partials/force-disabled-callout.blade.php; path resources/views/livewire/server/security/host-key.blade.php; path resources/views/livewire/server/security/patches.blade.php; path resources/views/livewire/server/sentinel.blade.php; path resources/views/livewire/server/show.blade.php; path resources/views/livewire/settings-backup.blade.php; path resources/views/livewire/star-toggle.blade.php; path resources/views/livewire/storage/server-endpoint-fields.blade.php; path resources/views/livewire/storage/sftp-login-fields.blade.php)
- [D] `review_component:ui.read-models`: port behaviour change into ui.read-models (path app/Livewire/Concerns/ManagesServerPackageUpdates.php; path app/Livewire/Dashboard.php; path app/Livewire/Project/CloneMe.php; path app/Livewire/Project/Service/ImportBackup.php; path app/Livewire/Project/Shared/ResourceOperations.php; path app/Livewire/Security/PrivateKey/Rotate.php; path app/Livewire/Security/PrivateKey/Show.php; path app/Livewire/Server/Delete.php; path app/Livewire/Server/New/ByIp.php; path app/Livewire/Server/Security/Patches.php; path app/Livewire/Server/Show.php; path app/Livewire/StarToggle.php; path app/Livewire/Team/Member.php; symbol app/Models/PrivateKey.php::generateNewKeyPair(); symbol bootstrap/helpers/shared.php::generateSSHKey(); table server_settings; table teams; table server_settings; table server_settings; table server_settings; table server_settings)
- [D] `review_component:remote.execution`: port behaviour change into remote.execution (path app/Helpers/SshMultiplexingHelper.php; path app/Traits/SshRetryable.php; path bootstrap/helpers/remoteProcess.php)
- [D] `review_component:server.validation`: port behaviour change into server.validation (path resources/views/components/process-dialog.blade.php; table server_settings; table server_settings; table server_settings; table server_settings; table server_settings)
- [D] `review_component:databases.standalone`: port behaviour change into databases.standalone (path app/Actions/Database/StartClickhouse.php; path app/Actions/Database/StartDragonfly.php; path app/Actions/Database/StartKeydb.php; path app/Actions/Database/StartMariadb.php; path app/Actions/Database/StartMongodb.php; path app/Actions/Database/StartMysql.php; path app/Actions/Database/StartPostgresql.php; path app/Actions/Database/StartRedis.php; path app/Actions/Database/StopDatabase.php; path app/Actions/Docker/GetContainersStatus.php; path app/Http/Controllers/Api/DatabasesController.php; path app/Livewire/Project/New/Select.php; path app/Livewire/Project/Resource/Create.php; path app/Livewire/Project/Resource/Index.php; path app/Livewire/Project/Shared/Danger.php; path app/Models/StandalonePostgresql.php; path app/Traits/ExecutesDatabaseStartCommands.php; path app/Traits/HasSecretManager.php; path resources/views/components/database/configuration-sidebar.blade.php; path resources/views/livewire/project/database/backup-edit/general.blade.php; path resources/views/livewire/project/database/backup-edit/retention.blade.php; path resources/views/livewire/project/database/backup-executions.blade.php; path resources/views/livewire/project/database/backup/index.blade.php; path resources/views/livewire/project/database/configuration.blade.php; path resources/views/livewire/project/database/heading.blade.php; path resources/views/livewire/project/database/import-form.blade.php; path resources/views/livewire/project/database/import.blade.php; path resources/views/livewire/project/database/redis/general.blade.php; path resources/views/livewire/project/new/select.blade.php; path resources/views/livewire/project/resource/index.blade.php; table standalone_redis)

### Unmapped behaviour-relevant files (334)

- `app/Actions/Application/CleanupPreviewDeployment.php`
- `app/Actions/Application/GetRollbackImages.php`
- `app/Actions/Application/StopApplication.php`
- `app/Actions/Application/StopApplicationPreview.php`
- `app/Actions/Database/StartDatabaseImport.php`
- `app/Actions/Database/StartSqlite.php`
- `app/Actions/Development/SeedDevelopmentQemuServer.php`
- `app/Actions/Fortify/CreateNewUser.php`
- `app/Actions/ImageScan/RefreshCandidates.php`
- `app/Actions/ImageScan/ScanHostPackages.php`
- `app/Actions/ImageScan/ScanImage.php`
- `app/Actions/ImageScan/SetServerImageScanning.php`
- `app/Actions/ImageScan/UpdateResourceImage.php`
- `app/Actions/IntegrationToken/UpdateIntegrationToken.php`
- `app/Actions/Proxy/DeleteTraefikCertificate.php`
- `app/Actions/Proxy/GetTraefikCertificates.php`
- `app/Actions/Server/AuthorizedKeys.php`
- `app/Actions/Server/CheckUpdates.php`
- `app/Actions/Server/InstallDocker.php`
- `app/Actions/Server/RemoveUnusedPackages.php`
- `app/Actions/Server/StartLogDrain.php`
- `app/Actions/Server/StartSentinel.php`
- `app/Actions/Server/UpdateCoolify.php`
- `app/Actions/Server/UpdatePackage.php`
- `app/Actions/Service/DeployServiceApplication.php`
- `app/Actions/Service/RestartServiceApplication.php`
- `app/Actions/Service/StartService.php`
- `app/Actions/Service/StopService.php`
- `app/Actions/Service/StopServiceApplication.php`
- `app/Actions/Service/UpdateServiceApplicationFromApi.php`
- `app/Actions/Shared/CheckDomainDns.php`
- `app/Actions/Stripe/SyncStripeSubscriptions.php`
- `app/Console/Commands/CleanupDatabase.php`
- `app/Console/Commands/CleanupStuckedResources.php`
- `app/Console/Commands/CleanupUnreachableServers.php`
- `app/Console/Commands/Cloud/CleanupUnsubscribedTeams.php`
- `app/Console/Commands/Cloud/CloudFixSubscription.php`
- `app/Console/Commands/Cloud/ExportUsers.php`
- `app/Console/Commands/Cloud/SyncStripeSubscriptions.php`
- `app/Console/Commands/Emails.php`
- `app/Console/Commands/Init.php`
- `app/Console/Kernel.php`
- `app/Enums/BackupStorageType.php`
- `app/Enums/CancelPreviousDeployments.php`
- `app/Enums/ImageScanErrorCode.php`
- `app/Enums/ImageScanFixState.php`
- `app/Enums/ImageScanGateMode.php`
- `app/Enums/ImageScanOnError.php`
- `app/Enums/ImageScanStatus.php`
- `app/Enums/SecurityActionType.php`
- … 284 more

### Unresolved questions

- some files matched no classification rule; extend PATH_RULES

## Commits

| commit | subject | categories | security |
|---|---|---|---|
| `33c0f6e57` | fix(release): sync the preview channel of the CDN before the stable one | [BuildRelease] |  |
| `fa128d86c` | feat(release): name the stable CDN pull request after its version | [Documentation, BuildRelease] |  |
| `80574bc16` | ci: run the nightly and main builds during working hours | [BuildRelease] |  |
| `db1cf006d` | ci: upload the list of the manifests to delete in every run | [BuildRelease] |  |
| `67b29ff24` | feat(scripts): delete the unused untagged manifests on docker.io | [BuildRelease] |  |
| `64d001562` | fix(scripts): delete the unreferenced images before the other indexes | [BuildRelease] |  |
| `6d9c027d1` | fix(scripts): count the docker.io layers once where ghcr.io has the manifest | [BuildRelease] |  |
| `156f99874` | docs(scripts): mark the Docker Hub manifests endpoint as undocumented | [BuildRelease] |  |
| `82d89589a` | fix(scripts): delete a shared image after the last index that references it | [BuildRelease] |  |
| `1da3124e1` | docs(readme): add Bragly to small sponsors | [Documentation] |  |
| `c3a21ecee` | fix(dns): replace unused Coolify records and show conflicts on automatic create | [Documentation, Frontend, DomainLogic, BackgroundJobs] |  |
| `b063a4162` | fix(mcp): save MCP deployments as MCP, not as API deployments | [Documentation, Frontend, ApiChange, DatabaseSchema, DomainLogic, DeploymentEngine] |  |
| `37b4a8b4e` | fix(mcp): keep the deployment source on additional servers and audit MCP calls as MCP | [Documentation, DomainLogic, DeploymentEngine] |  |
| `daf0e1a70` | fix(dns): add the Cloudflare record when a domain is edited to a managed zone | [Documentation, Frontend, ContainerIntegration] |  |
| `cf1e0e70c` | feat(deployments): cancel superseded deployments when a newer one is queued | [Documentation, Frontend, ApiChange, DatabaseSchema, DomainLogic, DeploymentEngine, BackgroundJobs] | yes |
| `1a888610a` | fix(scripts): delete the docker.io manifests deleted on ghcr.io | [BuildRelease] |  |
| `ef88ea07d` | ci: disable the manifest cleanup after a failed run | [BuildRelease] |  |
| `75f0ad745` | feat(scripts): use the undocumented docker.io endpoint only for untagged manifests | [BuildRelease] |  |
| `c42222a88` | refactor(scripts): move the manifest types to a variable | [BuildRelease] |  |
| `19f82b6e0` | Merge branch 'main' of https://github.com/coollabsio/coolify | [Documentation, Tests, Frontend, DomainLogic, BackgroundJobs] |  |
| `ed03a31b3` | fix(server): keep ip and user as strings after save | [Documentation, Tests, DomainLogic] |  |
| `6254f44f1` | fix(metrics): show disk usage when Sentinel names the data disk by another mount | [Documentation, Tests, DomainLogic] |  |
| `688134a99` | feat(backups): import backups from a dialog on the backups page | [Documentation, Tests, Frontend] | yes |
| `c94411fb9` | fix(clone): do not copy preview records when an application is cloned | [Documentation, DeploymentEngine] |  |
| `777362cd9` | feat(proxy): mark unused Traefik certificates and delete them in one step | [Documentation, Tests, Frontend, DomainLogic, ContainerIntegration] | yes |
| `bb6dfd147` | fix(mcp): queue MCP deployments behind a deployment in progress | [Documentation, Tests, DomainLogic, DeploymentEngine] |  |
| `9c6138fe1` | fix(cleanup): stop loading deployment logs and unused backup executions | [Documentation, Tests, Frontend, DomainLogic, BackgroundJobs] |  |
| `f057aaf2e` | fix(rollback): list the commit images of Docker Compose applications | [Documentation, Tests, DeploymentEngine] |  |
| `7ec000ab8` | fix(service): pin usesend template to v1.10.0 | [Documentation, ServiceTemplates] |  |
| `b7defddc3` | fix(dns): verify the Cloudflare origin and add records for clones and API-created resources | [Documentation, Tests, ApiChange, DomainLogic, DeploymentEngine] |  |
| `83a566738` | fix(server): detach additional-destination apps instead of deleting them on server delete | [Documentation, Tests, Frontend, ApiChange, DomainLogic] |  |
| `ccaa04ce3` | fix(service): use postgresql in the bugsink template | [Documentation, ServiceTemplates] |  |
| `e9028e133` | fix(service): allow the redirect_to header in appflowy gotrue cors | [Documentation, ServiceTemplates] |  |
| `8f7d1be7a` | fix(service): limit supabase vector to the logs of its own stack | [Documentation, ServiceTemplates] |  |
| `dd8ea3dc9` | fix(service): use postgres 16 in the outline template | [Documentation, ServiceTemplates] |  |
| `a487d7c9d` | fix(service): prefix the litellm master key with sk- and add a salt key | [Documentation, ServiceTemplates] |  |
| `7f521e496` | fix(s3): add a setting to skip TLS verification for S3 storages | [Documentation, Tests, Frontend, ApiChange, DatabaseSchema, DomainLogic, ContainerIntegration, BackgroundJobs] | yes |
| `b7a2c7573` | fix(github): check clock skew only from failed GitHub App responses | [Documentation, Tests, Frontend, DomainLogic, ExternalIntegrations, BackgroundJobs] | yes |
| `d70801aaa` | fix(github): retry GitHub API reads on server and connection errors | [Documentation, Tests, DomainLogic] |  |
| `e20e652c0` | fix(dns): check the closest Cloudflare wildcard for deeper domain names | [Documentation, Tests, DomainLogic] |  |
| `f21acdc3e` | fix(notifications): notify when a resource stops unexpectedly | [Documentation, Tests, Frontend, DomainLogic, DeploymentEngine, ContainerIntegration, ExternalIntegrations, BackgroundJobs] |  |
| `d3ef4d1c5` | fix(service): route the rybbit /api path to the backend | [Documentation, Tests, DomainLogic, ServiceTemplates] |  |
| `71814b454` | fix(deployment): use one COOLIFY_BRANCH value for preview deployments | [Documentation, Tests, DomainLogic, DeploymentEngine] | yes |
| `52a78cdb0` | fix(env): keep the trailing newline of multiline environment variables | [Documentation, Tests, DomainLogic, AuthenticationAuthorization] | yes |
| `8c6774e17` | fix(deploy): keep the helper Docker CLI on its buildx plugin | [Documentation, Tests, DeploymentEngine] |  |
| `e13652ee1` | chore(changelog): remove scheduled task entries added by mistake | [Documentation] |  |
| `d6405d554` | fix(deployment): remove the helper container before restarting it with the same name | [Documentation, Tests, DeploymentEngine] |  |
| `e29dcf70d` | fix(service): mark parts that are no longer in the Compose file | [Documentation, Tests, Frontend, DatabaseSchema, DomainLogic] |  |
| `a05fe524f` | fix(storage): confirm volume renames because the data stays in the old volume | [Documentation, Tests, Frontend] | yes |
| `fc027be24` | fix(compose): warn when the Compose file is in a subdirectory of the base directory | [Documentation, Frontend] |  |
| `d6acd9a3c` | fix(docker): use the Ubuntu and Debian repositories in the Docker install fallback | [Documentation, ContainerIntegration] |  |
| `5431d4954` | fix(server): run the connection check Docker commands through SSH mux | [Documentation, Tests, DomainLogic, BackgroundJobs] |  |
| `1ff99b0dc` | fix(logs): start the log copy during the click so it works on iOS | [Documentation, Tests, Frontend] | yes |
| `06c41f57d` | feat(notifications): throttle repeated scheduled job failures and disable failing tasks | [Documentation, Frontend, ApiChange, DatabaseSchema, DomainLogic, ExternalIntegrations, BackgroundJobs] |  |
| `3f929edfc` | chore(dev): reach Mailpit as coolify-mail in scripts/dev instances | [BuildRelease] |  |
| `2fbe9d2ab` | fix(proxy): notify once for each new Traefik patch version | [Documentation, Tests, BackgroundJobs] |  |
| `6497cc73a` | docs(agents): tell agents not to edit CHANGELOG.md | [Documentation] |  |
| `fcf92b751` | fix(healthcheck): read the Dockerfile HEALTHCHECK only from the built stage | [Tests, DomainLogic] |  |
| `3895ca05f` | fix(service): pull Convex images from the renamed ghcr.io/convex namespace | [ServiceTemplates] |  |
| `666ac84ef` | fix(status): show running:starting while a health check is in its start period | [Tests, Frontend, DomainLogic] |  |
| `50af95a70` | fix(service): show a starting health check as a warning on the resource card | [Frontend] |  |
| `b919af5f7` | docs(readme): remove Best Consultant from big sponsors | [Documentation] |  |
| `a5b4e1d32` | feat(email): add a separate notification sender to the instance email settings | [Frontend, ApiChange, DatabaseSchema, DomainLogic, ExternalIntegrations, BackgroundJobs] | yes |
| `33cb5e3c6` | test: cover OAuth redirect URI, multi-domain previews, server clones | [Tests, ServiceTemplates] |  |
| `3dc89cfe0` | fix(oauth): require redirect URI to point at the provider callback route | [Frontend] |  |
| `21d1ad487` | fix(backups): upload to S3 endpoints with a path, like Supabase Storage | [Tests, DomainLogic, ContainerIntegration, BackgroundJobs] | yes |
| `ecb14e6cd` | fix(server): allow servers with the same IP and different SSH ports | [Tests, Frontend, ApiChange, DomainLogic] |  |
| `5c7106748` | fix(backups): limit the full backup run to its timeout | [Documentation, Tests, Frontend, BackgroundJobs] |  |
| `780d67ccb` | fix(storage): stop a deleted volume from coming back after a redeploy | [Documentation, Tests, Frontend, DeploymentEngine, ContainerIntegration] | yes |
| `3c87879e6` | fix(compose): keep service names as hostnames in preview deployments | [Tests, DomainLogic] |  |
| `f4a1e3e42` | fix(proxy): keep Caddy WebSockets open across config reloads | [Tests, DomainLogic, ContainerIntegration] |  |
| `4b9fe035d` | fix(deployment): trust the server CA bundle in the helper container | [Tests, DeploymentEngine] |  |
| `e0b94662e` | fix(deployments): apply the concurrent build limit of build servers | [Tests, DomainLogic, DeploymentEngine] |  |
| `31c3fcf50` | feat(scheduled-tasks): add an option to run a task without a shell | [Documentation, Tests, Frontend, ApiChange, DatabaseSchema, DomainLogic, BackgroundJobs] | yes |
| `dec776025` | fix(upgrade): finish the upgrade modal when the new version answers | [Tests, Frontend] |  |
| `1b571f450` | fix(logs): cap streamed log reads so dockerd does not scan the full log file | [Tests, Frontend] |  |
| `618d377d8` | fix(ui): keep the dark theme during page loads and wire:navigate | [Frontend] |  |
| `0b89b857e` | fix(backups): make the database backup executions table fit and readable | [Frontend] |  |
| `8126a5c12` | fix(backups): limit S3 uploads to 1000 parts for providers like Scaleway | [Tests, DomainLogic, BackgroundJobs] | yes |
| `960471984` | fix(cleanup): delete sessions older than the session lifetime | [Tests, BackgroundJobs] |  |
| `c3e5b4ba7` | test(env): cover spaced shared variable inside a longer value (#7202) | [Tests] |  |
| `a7b7aa350` | fix(telegram): send button links as text when Telegram rejects the button URL | [Tests, BackgroundJobs] |  |
| `0b45d8dd6` | fix(env): quote old multi-line JSON values in the .env file | [Tests, DomainLogic] |  |
| `19e391883` | test(webhook): cover manual webhooks for two applications of one repository | [Tests] |  |
| `e0ae59dec` | fix(storage): explain the UUID prefix of new volume names | [Frontend] |  |
| `b83b97f7e` | fix(server): show the SSH error when a new private key cannot connect | [Tests, Frontend, DomainLogic] |  |
| `8d07d178f` | fix(log-drain): send Coolify labels to Fluent Bit drains | [Tests, DomainLogic, ContainerIntegration] |  |
| `da7779d06` | fix(env): keep locked variables locked but allow a new value | [Tests, Frontend, ApiChange, DomainLogic] |  |
| `232e8b861` | fix(backups): back up all databases by default for Odoo services | [Tests, Frontend] |  |
| `927e3f0b4` | fix(service): route Ente public album links and fix Ente startup | [ServiceTemplates] |  |
| `997396b92` | feat(cloud): save deleted users' emails and keep unsubscribed teams for 60 days | [Tests, Configuration, BackgroundJobs] |  |
| `d09480973` | fix(previews): give each Compose preview its own project name | [Tests, DeploymentEngine] |  |
| `860e78648` | fix(backups): back up the instance database on localhost and show why the pre-update backup failed | [Frontend, ContainerIntegration, BackgroundJobs] |  |
| `ca2722462` | fix(server): refresh the page after the localhost connection is validated | [Frontend] |  |
| `25b4236e3` | fix(api): check deploy access before a deploy writes a Docker image preview | [Tests, ApiChange] |  |
| `f9bde1cd7` | fix(env): prevent duplicate environment variables and allow removing extra copies | [Tests, Frontend, ApiChange, DomainLogic] |  |
| `455b22f26` | fix(deploy): explain SSH exit 255 and handle non-map compose services | [Tests, DomainLogic, DeploymentEngine] |  |
| `4d932b307` | fix(services): update resource card status together with the heading badge | [Frontend] |  |
| `84d21b8bf` | Merge remote-tracking branch 'origin/main' | [BuildRelease] |  |
| `9e64998d6` | refactor(ui): drop deprecated -webkit-overflow-scrolling declarations | [Frontend] |  |
| `59481c7bb` | fix(api-tokens): keep token search in sync after create, edit, and revoke | [Tests, Frontend] |  |
| `5eee9933c` | fix(theme): use sidebar text color for menu entries when the sidebar scheme differs | [Frontend] |  |
| `651873493` | fix(theme): keep custom accent text and fills readable | [Frontend] |  |
| `51a7e9761` | fix(healthcheck): import Dockerfile HEALTHCHECK options like Docker | [Tests, DomainLogic] |  |
| `4bdf03db6` | fix(keys): generate 4096-bit RSA keys | [Tests, DomainLogic] |  |
| `3bc82eb71` | feat(api): add Gitea App endpoints for sources and applications | [Tests, ApiChange, DomainLogic] | yes |
| `32541a32c` | fix(api): validate resource names and descriptions like the UI | [Tests, ApiChange, DomainLogic] | yes |
| `05e52fc41` | fix(compose): keep dotted service names literal and validate service names | [Tests, DomainLogic, ContainerIntegration] |  |
| `6ab538d0f` | fix(deploy): keep short-lived containers out of build --add-host | [Documentation, Tests, DeploymentEngine] |  |
| `cc996a1d9` | fix(api): accept gitea_app_uuid when creating and updating applications | [Tests, ApiChange] |  |
| `30a8e3357` | fix(server): refresh the server page after background validation | [Frontend, DomainLogic] |  |
| `19762a795` | fix(ui): keep volume data checkboxes next to their labels | [Frontend] |  |
| `9a6cc8316` | fix(horizon): route Sentinel push jobs to a dedicated cloud sentinel queue | [Documentation, Tests, DomainLogic, Configuration, BackgroundJobs, Unclassified] |  |
| `5c34a1b3d` | fix(sentinel): stop rejected Sentinel pushes and reduce their audit rows | [ApiChange, DomainLogic, ContainerIntegration] |  |
| `e7e952fca` | fix(deploy): keep preview and build server hosts out of build --add-host | [Frontend, ApiChange, DatabaseSchema, DomainLogic, DeploymentEngine] | yes |
| `0e01f2149` | fix(sentinel): send Retry-After for unreachable servers and invalid tokens | [ApiChange] |  |
| `13298591d` | fix(api): throttle API clients that keep failing and notify the token's team | [Tests, Frontend, DomainLogic, AuthenticationAuthorization, Configuration, ExternalIntegrations] | yes |
| `b7d0e6be2` | fix(audit): stop duplicate and empty audit rows | [ApiChange, DatabaseSchema, DomainLogic] |  |
| `3379fa448` | fix(audit): keep personal data and credentials out of audit events | [Tests, Frontend, ApiChange, DomainLogic] |  |
| `70adcc2b0` | feat(databases): use newer default images for standalone databases | [Frontend, DatabaseSchema, DomainLogic] |  |
| `7fbc5d779` | fix(ui): align service backup tables and sort executions by column | [Frontend] |  |
| `3115980c7` | fix(ui): copy service backup path and show S3 badge only for runs stored in S3 | [Tests, Frontend] |  |
| `ba966846e` | fix(ui): show relative start time with exact date tooltip in service backup executions | [Frontend] |  |
| `b12c965fb` | fix(ui): use bare status badges and show backup S3 storage as plain text | [Documentation, Frontend] |  |
| `c8c639e3d` | fix(realtime): keep Reverb handshake under its size limit with large cookie headers | [Tests, AuthenticationAuthorization, Configuration] | yes |
| `bd499de1f` | feat(ui): add UI scale option to appearance settings | [Frontend] |  |
| `693cf0486` | fix(ui): link secret manager token forms to their provider dashboards | [Frontend] |  |
| `00d0b4be1` | fix(ui): add coolify.io ref to integration token provider links | [Frontend, DomainLogic] |  |
| `106a6d7ac` | fix(env): resolve and redact server-scoped shared variables in compose | [Tests, DomainLogic] |  |
| `91d233be5` | feat(ui): add list view to the new resource page | [Tests, Frontend] |  |
| `5960814df` | fix(ui): match deploy canvas card border to resource tiles | [Frontend] |  |
| `878d77bd6` | fix(database): make the Redis health check ping with the password | [ContainerIntegration] |  |
| `41877198b` | feat(database): offer Valkey as an image of the Redis database type | [Frontend, DomainLogic] |  |
| `52d7d88fd` | docs(api): document Valkey images for Redis databases | [ApiChange] |  |
| `813bc72af` | fix(ui): shorten database descriptions on the new resource page | [Frontend] |  |
| `145569bea` | fix(db): bump Supabase and PostGIS default images | [Frontend] |  |
| `b7dc2e435` | feat(database): default new Redis databases to the Valkey image | [Frontend, ApiChange, DatabaseSchema] |  |
| `033f0c84d` | v4.7 (#12478) | [Documentation, Tests, Frontend, ApiChange, DatabaseSchema, DomainLogic, AuthenticationAuthorization, DeploymentEngine, ContainerIntegration, Configuration, ExternalIntegrations, BackgroundJobs, DependencyUpgrade, BuildRelease, Unclassified] | yes |
| `757a04900` | docs(changelog): add breaking changes and security notes to Unreleased | [Documentation] |  |
| `54da97404` | chore(upstream): bump Sentinel to 1.1.5 | [Unclassified] |  |
| `71d33863a` | docs(changelog): condense Unreleased breaking and security entries | [Documentation] |  |
| `978192121` | fix(dev): keep QEMU servers that still have resources on seed | [DomainLogic] |  |
| `e614e680c` | docs(agents): document the changelog format | [Documentation] |  |
| `b1a20fabd` | fix(ui): hide tooltips on close and dismiss buttons | [Frontend] |  |
| `53e21abe9` | fix(services): do not warn about the port when a domain has no port | [Tests, Frontend, DomainLogic] |  |
| `17a9fa624` | feat(ui): move maintenance mode to its own page with a confirmed toggle | [Tests, Frontend] |  |
| `abea0747e` | feat(service): add Orbit template | [Frontend, ServiceTemplates] |  |
| `973cd8684` | feat(ui): show the logo and name on the resource creation screen | [Frontend] |  |
| `d640aa15f` | docs(changelog): add Orbit template, maintenance page, and port fix | [Documentation] |  |
| `1652a1d09` | feat(ui): show the finish time of recent deployments on the dashboard | [Frontend] |  |
| `cab5ae990` | feat(ui): show server usage on mobile in the dashboard server table | [Frontend] |  |
| `c648f4ec6` | feat(ui): star servers and resources for quick access on the dashboard | [Frontend, DatabaseSchema, DomainLogic] | yes |
| `5af4715b8` | feat(ui): put the star button after the status in the top bar | [Frontend] |  |
| `f7ea96214` | fix(server): enable disabled self-hosted servers again on validation | [Tests, Frontend, DomainLogic, BackgroundJobs] |  |
| `eb5f238a4` | fix(server): show the disabled server message on the localhost page | [Frontend] |  |
| `29aa42b98` | chore(docker): use the multi-platform Node.js image again (#12483) | [BuildRelease] |  |
| `8011d0d6a` | fix(access): check server and credential access for scoped members (#12488) | [Documentation, Tests, Frontend, ApiChange, DomainLogic, AuthenticationAuthorization] | yes |
| `3b64cbc37` | fix(access): clearer credential message and backup S3 defaults | [Tests, Frontend, ApiChange, AuthenticationAuthorization] | yes |
| `1d21a2781` | refactor(image-scan): remove the instance switch and the vulnerability DB URL | [Tests, Frontend, ApiChange, DatabaseSchema, DomainLogic, BackgroundJobs] | yes |
| `3c588d482` | fix(access): apply API token limits in more permission checks | [Documentation, Tests, DomainLogic, AuthenticationAuthorization] | yes |
| `c75e7078b` | fix(access): restrict preview changes, rollbacks, and repo URL updates | [Tests, Frontend, ApiChange, DomainLogic, AuthenticationAuthorization, DeploymentEngine] | yes |
| `61b10664c` | refactor(security): remove published APP_KEY request blocking middleware | [Tests, DomainLogic, AuthenticationAuthorization] | yes |
| `3f51c251c` | fix(git): show the full Git error reason on a failed repository read | [DomainLogic] |  |
| `91985acf4` | build(image): remove the unused MinIO client from the coolify image (#12491) | [BuildRelease] |  |
| `cd975b49a` | docs(release): merge next into main only on the day of the release | [Documentation] |  |
| `b2617e753` | feat(release): write the changelog from change files | [Documentation, BuildRelease, Unclassified] |  |
| `32d41d035` | docs: describe the changes of v4.7 in change files | [Documentation] |  |
| `9667e070c` | docs(agents): link only issues in change files | [Documentation] |  |
| `6eeea653f` | docs: remove the pull request links from the change files | [Documentation] |  |
| `de75bbe00` | fix(upload): return 404 for a database the team cannot open | [Documentation, Tests, DomainLogic] |  |
| `a70e85688` | test: remove tests that do not test behavior and strengthen weak ones | [Tests] |  |
| `acacbf974` | fix(compose): report an empty Compose file as empty, not as not found | [Documentation, Tests, DomainLogic] |  |
| `2466ccb8f` | fix(service): check images before stopping a service on restart | [Documentation, Tests, ContainerIntegration] | yes |
| `769bddb32` | fix(api): require write permission to choose a preview commit | [Documentation, Tests, ApiChange, DomainLogic] |  |
| `f4cbf63e7` | fix(api): validate the docker tag of deploy requests | [Documentation, Tests, ApiChange] |  |
| `132dd0e34` | test(api): cover member access to the cloud provider token list | [Tests] |  |
| `5ff94dbd7` | fix(api): limit nested server data to team-wide tokens | [Documentation, Tests, ApiChange, DomainLogic, AuthenticationAuthorization] | yes |
| `a44193b6e` | fix(mcp): return only service fields from get_service | [Documentation, Tests, DomainLogic] | yes |
| `b31c06a7e` | fix(team): build invite links from instance URL, not request host | [Documentation, Tests, Frontend] |  |
| `09f4aedb4` | docs(agents): add security advisory comment workflow via gh api | [Documentation] |  |
| `2e63c2e57` | docs(agents): document editing advisory comments and linking commit SHAs | [Documentation] |  |
| `c589449c3` | fix(ui): make banners readable in light mode | [Documentation, Frontend] |  |
| `bf648b9a3` | WIP (#12493) | [Documentation, Tests, Frontend, DatabaseSchema, DomainLogic, ContainerIntegration, ExternalIntegrations, BackgroundJobs, Unclassified] | yes |
| `3c349abde` | fix(ui): make banners readable in light mode (#12495) | [Unclassified] |  |
| `eaaaf7a71` | feat(release): bump coolify version to v4.7.0 | [Documentation, Unclassified] |  |
| `b370c7ea6` | fix(image-scan): show the Vulnerabilities page again (#12497) | [Documentation, Frontend] |  |
| `79dabc197` | chore(marketing): add the release update video kit | [Documentation, Unclassified] |  |
| `ab4964ea8` | fix(ui): keep warning callouts yellow in custom themes | [Documentation, Frontend] |  |
| `957c01fcc` | fix(image-scan): ship the scanner templates and show the scan error log | [Documentation, Frontend, DomainLogic, BuildRelease] |  |
| `c93b76597` | fix(proxy): route maintenance mode through custom container labels | [Documentation, Tests, DomainLogic] |  |
| `4f56c8123` | feat(release): bump coolify version to v4.7.1 | [Documentation, Unclassified] |  |
| `ae1147003` | fix(updates): make the nightly update channel easier to find (#12502) | [Documentation, Frontend] |  |
| `dc7a5946e` | fix(image-scan): let a server set the scanner memory limit | [Documentation, Tests, Frontend, ApiChange, DatabaseSchema, DomainLogic] | yes |
| `b81cc4116` | fix(breadcrumb): link breadcrumb names to their page | [Documentation, Tests, Frontend] |  |
| `aa016e685` | fix(cache): accept numbers that Redis returns as strings | [Documentation, Frontend, ApiChange, DomainLogic, BackgroundJobs] |  |
| `c9b9fd073` | fix(destination): limit stop and redeploy to the app's own servers | [Documentation, Frontend] |  |
| `0d0f49b34` | fix(release): make an unsigned release commit when git cannot sign | [Documentation, BuildRelease] |  |
| `312fcbfd8` | feat(release): bump coolify version to v4.7.2 | [Documentation, Unclassified] |  |
| `f41047785` | fix(backups): let root team admins manage and download the instance backup | [Documentation, Tests, Frontend, DomainLogic, AuthenticationAuthorization, ExternalIntegrations, BackgroundJobs] | yes |
| `75690c923` | Merge remote-tracking branch 'origin/main' | [BuildRelease] |  |
| `4f22b5d1e` | feat(image-scan): per-server scanner time and CPU limits, and export of vulnerability lists (#12504) | [Documentation, Tests, Frontend, ApiChange, DatabaseSchema, DomainLogic, BackgroundJobs] | yes |
| `30335d808` | feat(release): bump coolify version to v4.7.3 | [Documentation, Unclassified] |  |
| `1d70487b3` | fix: delete failed database backup records and restore Cloud backups with orphan replica rows (#12507) | [Documentation, BackgroundJobs, BuildRelease] |  |
| `4feb08682` | fix(scripts): find the IP of the restore VMs and show the wait progress | [BuildRelease] |  |
| `edfa45aae` | fix(scripts): stop the restore VM script on a full disk or a broken install | [BuildRelease] |  |
| `c49d9f623` | fix(previews): pass production runtime variables without a preview copy | [Documentation, Tests, DeploymentEngine] |  |
| `6bdc780af` | feat(release): bump coolify version to v4.7.4 | [Documentation, Unclassified] |  |
| `989c2238f` | fix(storages): connect to SFTP hosts on IPv6 and show the network error | [Documentation, DomainLogic] |  |
| `879bd5c42` | fix(api-tokens): open the tokens page when a token has unreadable scopes | [Documentation, Frontend] |  |
| `378ce3735` | feat(release): bump coolify version to v4.7.5 | [Documentation, Unclassified] |  |
