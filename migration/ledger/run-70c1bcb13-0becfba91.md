# Migration run `run-70c1bcb13-0becfba91`

- Upstream range: `70c1bcb1381d..0becfba91130` (74 first-parent commits)
- Stage: Analyzed
- Branch: (analysis only)
- Verdict: **not validated**
- Recommended next action: analysis only: re-run with generation to produce a patch

## Plan

- Risk: **High**
  - security-sensitive (124 signals)
  - touches authentication/authorization
  - migration 2026_10_25_100000_add_access_grants_to_team_members_and_tokens changes mapped tables ["personal_access_tokens", "team_user"]
  - migration 2026_10_27_142736_add_paused_at_to_personal_access_tokens_table changes mapped tables ["personal_access_tokens"]
  - migration 2026_10_28_211105_add_indexes_for_team_deployments_and_docker_cleanup_history is destructive/raw-SQL/encryption (destructive=false, raw_sql=true, encryption=false)
  - migration 2026_10_28_212642_add_indexes_for_server_settings_and_team_membership_lookups changes mapped tables ["server_settings", "team_user"]
  - migration 2026_10_28_212642_add_indexes_for_server_settings_and_team_membership_lookups is destructive/raw-SQL/encryption (destructive=false, raw_sql=true, encryption=false)
  - migration 2026_10_28_214500_add_index_for_application_deployment_history is destructive/raw-SQL/encryption (destructive=false, raw_sql=true, encryption=false)
  - migration 2026_10_28_214953_add_index_for_interrupted_docker_cleanup_executions is destructive/raw-SQL/encryption (destructive=false, raw_sql=true, encryption=false)
  - migration 2026_10_28_224219_add_index_for_active_github_runner_executions is destructive/raw-SQL/encryption (destructive=false, raw_sql=true, encryption=false)
  - migration 2026_10_28_224423_add_index_for_dashboard_recent_deployments is destructive/raw-SQL/encryption (destructive=false, raw_sql=true, encryption=false)
  - migration 2026_10_28_232000_add_index_for_active_servers_by_private_key changes mapped tables ["servers"]
  - migration 2026_10_28_232000_add_index_for_active_servers_by_private_key is destructive/raw-SQL/encryption (destructive=false, raw_sql=true, encryption=false)
  - migration 2026_11_01_142702_add_update_channel_to_instance_settings_table changes mapped tables ["instance_settings"]
  - API routes changed in an implemented component
  - category ApiChange
  - category DomainLogic
  - category DatabaseSchema
  - category BackgroundJobs
  - category ExternalIntegrations
  - category Configuration
  - category DeploymentEngine
  - category ContainerIntegration
  - deployment/container behaviour in an implemented component
- Security sensitive: true
- Categories: [Documentation, Tests, Frontend, ApiChange, DatabaseSchema, DomainLogic, AuthenticationAuthorization, DeploymentEngine, ContainerIntegration, Configuration, ExternalIntegrations, BackgroundJobs, BuildRelease, ServiceTemplates, Unclassified]
- Mapping coverage of behaviour-relevant files: 17% (heuristic, uncalibrated)

### Escalation

Decision required: approve or reject the generated patch and assign the semantic port of the listed components

- high risk: requires explicit human approval before integration
- 191 behaviour-relevant files are not covered by the mapping registry

### Affected Rust components

- `api.auth-chain` (Verified): path app/Http/Kernel.php; path app/Http/Middleware/ApiAbility.php; path app/Http/Middleware/EnsureTokenBelongsToCurrentTeamMember.php; path app/Models/PersonalAccessToken.php; upstream test tests/Feature/ApiTokenExpirationTest.php; upstream test tests/Feature/ApiTokenExpirationWarningTest.php; upstream test tests/Feature/ApiTokenLivewireAuthorizationTest.php; upstream test tests/Feature/Authorization/LegacyMemberTokenTest.php; upstream test tests/Feature/Security/ApiTokenCreationSecurityTest.php; table personal_access_tokens; table team_user; table personal_access_tokens; table team_user; table instance_settings
- `api.teams` (Verified): path app/Policies/TeamPolicy.php; table team_user; table team_user
- `api.projects` (Verified): path app/Http/Controllers/Api/ProjectController.php; path app/Models/Environment.php; path app/Models/Project.php; path app/Policies/EnvironmentPolicy.php; path app/Policies/ProjectPolicy.php; route DELETE api/v1/projects/{uuid}; route DELETE api/v1/projects/{uuid}/environments/{environment_name_or_uuid}; route DELETE api/v1/projects/{uuid}/environments/{environment_name_or_uuid}/envs/{env_id}; route DELETE api/v1/projects/{uuid}/envs/{env_id}
- `api.route-inventory` (Verified): path app/Http/Kernel.php; path routes/api.php
- `db.schema-guard` (Implemented): path database/migrations/2026_10_25_100000_add_access_grants_to_team_members_and_tokens.php; path database/migrations/2026_10_26_081954_add_git_https_credentials_to_applications_table.php; path database/migrations/2026_10_27_142736_add_paused_at_to_personal_access_tokens_table.php; path database/migrations/2026_10_28_204953_add_worker_host_to_scheduled_job_executions.php; path database/migrations/2026_10_28_211105_add_indexes_for_team_deployments_and_docker_cleanup_history.php; path database/migrations/2026_10_28_212642_add_indexes_for_server_settings_and_team_membership_lookups.php; path database/migrations/2026_10_28_214500_add_index_for_application_deployment_history.php; path database/migrations/2026_10_28_214953_add_index_for_interrupted_docker_cleanup_executions.php; path database/migrations/2026_10_28_224219_add_index_for_active_github_runner_executions.php; path database/migrations/2026_10_28_224423_add_index_for_dashboard_recent_deployments.php; path database/migrations/2026_10_28_232000_add_index_for_active_servers_by_private_key.php; path database/migrations/2026_10_29_221743_add_checkout_reminder_sent_at_to_subscriptions_table.php; path database/migrations/2026_10_30_144551_add_access_to_team_invitations_table.php; path database/migrations/2026_10_30_171359_drop_image_default_from_standalone_sqlites_table.php; path database/migrations/2026_11_01_142702_add_update_channel_to_instance_settings_table.php
- `api.servers.subroutes` (Planned): path app/Http/Controllers/Api/ServerDockerRegistriesController.php; path app/Http/Controllers/Api/ServerTransferController.php; path app/Policies/ServerPolicy.php
- `api.private-keys` (Verified): route DELETE api/v1/security/keys/{uuid}
- `ui.session` (Implemented): table team_user; table team_user
- `api.servers` (Verified): path app/Models/Server.php; path app/Models/ServerSetting.php; table server_settings; table servers
- `ui.styles` (Verified): path resources/css/app.css; path resources/views/components/reicon.blade.php
- `ui.frontend` (Implemented): path resources/views/auth/two-factor-challenge.blade.php; path resources/views/components/forms/listbox.blade.php; path resources/views/components/modal-confirmation.blade.php; path resources/views/components/navbar.blade.php; path resources/views/components/server/sidebar.blade.php; path resources/views/livewire/dashboard.blade.php; path resources/views/livewire/project/index.blade.php; path resources/views/livewire/project/resource/index.blade.php; path resources/views/livewire/project/show.blade.php; path resources/views/livewire/security/private-key/index.blade.php; path resources/views/livewire/server/index.blade.php; path resources/views/livewire/server/show.blade.php; path resources/views/livewire/team/index.blade.php; path resources/views/livewire/team/member.blade.php; path resources/views/livewire/team/member/index.blade.php
- `ui.read-models` (Verified): path app/Livewire/Dashboard.php; path app/Livewire/Project/Show.php; path app/Livewire/Server/Show.php; path app/Livewire/Team/Member.php; table personal_access_tokens; table team_user; table personal_access_tokens; table server_settings; table team_user; table servers
- `deployment.engine` (Planned): path app/Http/Controllers/Api/DeployController.php; path app/Jobs/ApplicationDeploymentJob.php; path app/Traits/ExecuteRemoteCommand.php; table applications; table applications; table application_deployment_queues; table application_deployment_queues
- `remote.execution` (Verified): path app/Helpers/SshMultiplexingHelper.php; path bootstrap/helpers/remoteProcess.php
- `server.validation` (Verified): table server_settings; table servers
- `databases.standalone` (Verified): path app/Http/Controllers/Api/DatabasesController.php; path app/Livewire/Project/Database/Postgresql/General.php; path app/Livewire/Project/Resource/Create.php; path app/Models/StandalonePostgresql.php; path app/Services/DatabaseStartCommandExecutor.php; path app/Traits/HasSecretManager.php; path resources/views/components/database/configuration-sidebar.blade.php; path resources/views/livewire/project/database/backup-edit/general.blade.php; path resources/views/livewire/project/database/scheduled-backups.blade.php; path resources/views/livewire/project/database/sqlite/connect-application.blade.php; path resources/views/livewire/project/resource/index.blade.php

### Upstream migrations

| migration | tables | mapped | destructive | raw SQL | encryption |
|---|---|---|---|---|---|
| `2026_10_25_100000_add_access_grants_to_team_members_and_tokens` | personal_access_tokens, team_user | personal_access_tokens, team_user | false | false | false |
| `2026_10_26_081954_add_git_https_credentials_to_applications_table` | applications |  | false | false | false |
| `2026_10_27_142736_add_paused_at_to_personal_access_tokens_table` | personal_access_tokens | personal_access_tokens | false | false | false |
| `2026_10_28_204953_add_worker_host_to_scheduled_job_executions` |  |  | false | false | false |
| `2026_10_28_211105_add_indexes_for_team_deployments_and_docker_cleanup_history` | applications, docker_cleanup_executions |  | false | true | false |
| `2026_10_28_212642_add_indexes_for_server_settings_and_team_membership_lookups` | server_settings, team_user | server_settings, team_user | false | true | false |
| `2026_10_28_214500_add_index_for_application_deployment_history` | application_deployment_queues |  | false | true | false |
| `2026_10_28_214953_add_index_for_interrupted_docker_cleanup_executions` | docker_cleanup_executions |  | false | true | false |
| `2026_10_28_224219_add_index_for_active_github_runner_executions` | github_runner_executions |  | false | true | false |
| `2026_10_28_224423_add_index_for_dashboard_recent_deployments` | application_deployment_queues |  | false | true | false |
| `2026_10_28_232000_add_index_for_active_servers_by_private_key` | servers | servers | false | true | false |
| `2026_10_29_221743_add_checkout_reminder_sent_at_to_subscriptions_table` | subscriptions |  | false | false | false |
| `2026_10_30_144551_add_access_to_team_invitations_table` | team_invitations |  | false | false | false |
| `2026_10_30_171359_drop_image_default_from_standalone_sqlites_table` | standalone_sqlites |  | false | false | false |
| `2026_11_01_142702_add_update_channel_to_instance_settings_table` | instance_settings | instance_settings | false | false | false |

### API route changes

- added `DELETE api/v1/applications/{uuid}/dns/{hostname}` → App\Http\Controllers\Api\DnsController@releaseApplicationRecord
- added `DELETE api/v1/security/integration-tokens/{uuid}` → App\Http\Controllers\Api\IntegrationTokensController@destroy
- added `DELETE api/v1/services/{uuid}/dns/{hostname}` → App\Http\Controllers\Api\DnsController@releaseServiceRecord
- added `DELETE api/v1/{resource_type}/{uuid}/secret-manager` → App\Http\Controllers\Api\SecretManagerController@destroy
- added `GET api/v1/applications/{uuid}/dns` → App\Http\Controllers\Api\DnsController@applicationStatus
- added `GET api/v1/dns/zones` → App\Http\Controllers\Api\DnsController@zones
- added `GET api/v1/security/integration-tokens` → App\Http\Controllers\Api\IntegrationTokensController@index
- added `GET api/v1/security/integration-tokens/{uuid}` → App\Http\Controllers\Api\IntegrationTokensController@show
- added `GET api/v1/services/{uuid}/dns` → App\Http\Controllers\Api\DnsController@serviceStatus
- added `GET api/v1/{resource_type}/{uuid}/secret-manager` → App\Http\Controllers\Api\SecretManagerController@show
- added `GET api/v1/{resource_type}/{uuid}/secret-manager/keys` → App\Http\Controllers\Api\SecretManagerController@keys
- added `PATCH api/v1/security/integration-tokens/{uuid}` → App\Http\Controllers\Api\IntegrationTokensController@update
- added `PATCH api/v1/{resource_type}/{uuid}/secret-manager` → App\Http\Controllers\Api\SecretManagerController@update
- added `POST api/v1/applications/{uuid}/dns` → App\Http\Controllers\Api\DnsController@createApplicationRecord
- added `POST api/v1/applications/{uuid}/dns/replace` → App\Http\Controllers\Api\DnsController@replaceApplicationRecord
- added `POST api/v1/security/integration-tokens/{uuid}/sync-zones` → App\Http\Controllers\Api\IntegrationTokensController@syncZones
- added `POST api/v1/services/{uuid}/dns` → App\Http\Controllers\Api\DnsController@createServiceRecord
- added `POST api/v1/services/{uuid}/dns/replace` → App\Http\Controllers\Api\DnsController@replaceServiceRecord
- added `POST api/v1/{resource_type}/{uuid}/secret-manager/import` → App\Http\Controllers\Api\SecretManagerController@import
- added `POST api/v1/{resource_type}/{uuid}/secret-manager/references` → App\Http\Controllers\Api\SecretManagerController@addReference
- removed `PATCH api/v1/applications/{uuid}/secret-manager`
- changed `DELETE api/v1/applications/{uuid}`: abilities ["write", "deploy"] → ["delete"], action App\Http\Controllers\Api\ApplicationsController@delete_by_uuid → App\Http\Controllers\Api\ApplicationsController@delete_by_uuid
- changed `DELETE api/v1/applications/{uuid}/destinations/{destination_uuid}`: abilities ["write"] → ["delete"], action App\Http\Controllers\Api\ApplicationsController@remove_destination → App\Http\Controllers\Api\ApplicationsController@remove_destination
- changed `DELETE api/v1/applications/{uuid}/envs/{env_uuid}`: abilities ["write"] → ["delete"], action App\Http\Controllers\Api\ApplicationsController@delete_env_by_uuid → App\Http\Controllers\Api\ApplicationsController@delete_env_by_uuid
- changed `DELETE api/v1/applications/{uuid}/previews/{pull_request_id}`: abilities ["write", "deploy"] → ["delete"], action App\Http\Controllers\Api\ApplicationsController@delete_preview_by_pull_request_id → App\Http\Controllers\Api\ApplicationsController@delete_preview_by_pull_request_id
- changed `DELETE api/v1/applications/{uuid}/scheduled-tasks/{task_uuid}`: abilities ["write"] → ["delete"], action App\Http\Controllers\Api\ScheduledTasksController@delete_scheduled_task_by_application_uuid → App\Http\Controllers\Api\ScheduledTasksController@delete_scheduled_task_by_application_uuid
- changed `DELETE api/v1/applications/{uuid}/storages/{storage_uuid}`: abilities ["write"] → ["delete"], action App\Http\Controllers\Api\ApplicationsController@delete_storage → App\Http\Controllers\Api\ApplicationsController@delete_storage
- changed `DELETE api/v1/applications/{uuid}/storages/{storage_uuid}/backups`: abilities ["write"] → ["delete"], action App\Http\Controllers\Api\VolumeBackupsController@destroy → App\Http\Controllers\Api\VolumeBackupsController@destroy
- changed `DELETE api/v1/applications/{uuid}/tags/{tag_uuid}`: abilities ["write"] → ["delete"], action App\Http\Controllers\Api\ApplicationsController@delete_tag → App\Http\Controllers\Api\ApplicationsController@delete_tag
- changed `DELETE api/v1/cloud-init-scripts/{uuid}`: abilities ["write"] → ["delete"], action App\Http\Controllers\Api\CloudInitScriptsController@destroy → App\Http\Controllers\Api\CloudInitScriptsController@destroy
- changed `DELETE api/v1/cloud-tokens/{uuid}`: abilities ["write"] → ["delete"], action App\Http\Controllers\Api\CloudProviderTokensController@destroy → App\Http\Controllers\Api\CloudProviderTokensController@destroy
- changed `DELETE api/v1/databases/{uuid}`: abilities ["write", "deploy"] → ["delete"], action App\Http\Controllers\Api\DatabasesController@delete_by_uuid → App\Http\Controllers\Api\DatabasesController@delete_by_uuid
- changed `DELETE api/v1/databases/{uuid}/backups/{scheduled_backup_uuid}`: abilities ["write"] → ["delete"], action App\Http\Controllers\Api\DatabasesController@delete_backup_by_uuid → App\Http\Controllers\Api\DatabasesController@delete_backup_by_uuid
- changed `DELETE api/v1/databases/{uuid}/backups/{scheduled_backup_uuid}/executions/{execution_uuid}`: abilities ["write"] → ["delete"], action App\Http\Controllers\Api\DatabasesController@delete_execution_by_uuid → App\Http\Controllers\Api\DatabasesController@delete_execution_by_uuid
- changed `DELETE api/v1/databases/{uuid}/envs/{env_uuid}`: abilities ["write"] → ["delete"], action App\Http\Controllers\Api\DatabasesController@delete_env_by_uuid → App\Http\Controllers\Api\DatabasesController@delete_env_by_uuid
- changed `DELETE api/v1/databases/{uuid}/storages/{storage_uuid}`: abilities ["write"] → ["delete"], action App\Http\Controllers\Api\DatabasesController@delete_storage → App\Http\Controllers\Api\DatabasesController@delete_storage
- changed `DELETE api/v1/databases/{uuid}/storages/{storage_uuid}/backups`: abilities ["write"] → ["delete"], action App\Http\Controllers\Api\VolumeBackupsController@destroy → App\Http\Controllers\Api\VolumeBackupsController@destroy
- changed `DELETE api/v1/databases/{uuid}/tags/{tag_uuid}`: abilities ["write"] → ["delete"], action App\Http\Controllers\Api\DatabasesController@delete_tag → App\Http\Controllers\Api\DatabasesController@delete_tag
- changed `DELETE api/v1/destinations/{uuid}`: abilities ["write"] → ["delete"], action App\Http\Controllers\Api\DestinationsController@delete → App\Http\Controllers\Api\DestinationsController@delete
- changed `DELETE api/v1/github-apps/{github_app_id}`: abilities ["write"] → ["delete"], action App\Http\Controllers\Api\GithubController@delete_github_app → App\Http\Controllers\Api\GithubController@delete_github_app
- changed `DELETE api/v1/gitlab-apps/{gitlab_app_id}`: abilities ["write"] → ["delete"], action App\Http\Controllers\Api\GitlabController@delete_gitlab_app → App\Http\Controllers\Api\GitlabController@delete_gitlab_app
- changed `DELETE api/v1/projects/{uuid}`: abilities ["write"] → ["delete"], action App\Http\Controllers\Api\ProjectController@delete_project → App\Http\Controllers\Api\ProjectController@delete_project
- changed `DELETE api/v1/projects/{uuid}/environments/{environment_name_or_uuid}`: abilities ["write"] → ["delete"], action App\Http\Controllers\Api\ProjectController@delete_environment → App\Http\Controllers\Api\ProjectController@delete_environment
- changed `DELETE api/v1/projects/{uuid}/environments/{environment_name_or_uuid}/envs/{env_id}`: abilities ["write"] → ["delete"], action App\Http\Controllers\Api\SharedEnvironmentVariablesController@environment_delete_env → App\Http\Controllers\Api\SharedEnvironmentVariablesController@environment_delete_env
- changed `DELETE api/v1/projects/{uuid}/envs/{env_id}`: abilities ["write"] → ["delete"], action App\Http\Controllers\Api\SharedEnvironmentVariablesController@project_delete_env → App\Http\Controllers\Api\SharedEnvironmentVariablesController@project_delete_env
- changed `DELETE api/v1/s3-storages/{uuid}`: abilities ["write"] → ["delete"], action App\Http\Controllers\Api\S3StoragesController@destroy → App\Http\Controllers\Api\S3StoragesController@destroy
- changed `DELETE api/v1/security/keys/{uuid}`: abilities ["write"] → ["delete"], action App\Http\Controllers\Api\SecurityController@delete_key → App\Http\Controllers\Api\SecurityController@delete_key
- changed `DELETE api/v1/servers/{uuid}`: abilities ["write"] → ["delete"], action App\Http\Controllers\Api\ServersController@delete_server → App\Http\Controllers\Api\ServersController@delete_server
- changed `DELETE api/v1/servers/{uuid}/envs/{env_id}`: abilities ["write"] → ["delete"], action App\Http\Controllers\Api\SharedEnvironmentVariablesController@server_delete_env → App\Http\Controllers\Api\SharedEnvironmentVariablesController@server_delete_env
- changed `DELETE api/v1/servers/{uuid}/registries/{registry}`: abilities ["write"] → ["delete"], action App\Http\Controllers\Api\ServerDockerRegistriesController@logout → App\Http\Controllers\Api\ServerDockerRegistriesController@logout
- changed `DELETE api/v1/services/{uuid}`: abilities ["write", "deploy"] → ["delete"], action App\Http\Controllers\Api\ServicesController@delete_by_uuid → App\Http\Controllers\Api\ServicesController@delete_by_uuid
- changed `DELETE api/v1/services/{uuid}/envs/{env_uuid}`: abilities ["write"] → ["delete"], action App\Http\Controllers\Api\ServicesController@delete_env_by_uuid → App\Http\Controllers\Api\ServicesController@delete_env_by_uuid
- changed `DELETE api/v1/services/{uuid}/scheduled-tasks/{task_uuid}`: abilities ["write"] → ["delete"], action App\Http\Controllers\Api\ScheduledTasksController@delete_scheduled_task_by_service_uuid → App\Http\Controllers\Api\ScheduledTasksController@delete_scheduled_task_by_service_uuid
- changed `DELETE api/v1/services/{uuid}/storages/{storage_uuid}`: abilities ["write"] → ["delete"], action App\Http\Controllers\Api\ServicesController@delete_storage → App\Http\Controllers\Api\ServicesController@delete_storage
- changed `DELETE api/v1/services/{uuid}/storages/{storage_uuid}/backups`: abilities ["write"] → ["delete"], action App\Http\Controllers\Api\VolumeBackupsController@destroy → App\Http\Controllers\Api\VolumeBackupsController@destroy
- changed `DELETE api/v1/services/{uuid}/tags/{tag_uuid}`: abilities ["write"] → ["delete"], action App\Http\Controllers\Api\ServicesController@delete_tag → App\Http\Controllers\Api\ServicesController@delete_tag
- changed `DELETE api/v1/tags/{uuid}`: abilities ["write"] → ["delete"], action App\Http\Controllers\Api\TagsController@delete → App\Http\Controllers\Api\TagsController@delete
- changed `DELETE api/v1/team/envs/{env_id}`: abilities ["write"] → ["delete"], action App\Http\Controllers\Api\SharedEnvironmentVariablesController@team_delete_env → App\Http\Controllers\Api\SharedEnvironmentVariablesController@team_delete_env
- changed `POST api/v1/servers/{uuid}/registries/{registry}/check`: abilities ["write"] → ["write", "deploy"], action App\Http\Controllers\Api\ServerDockerRegistriesController@check → App\Http\Controllers\Api\ServerDockerRegistriesController@check

### Actions

- [A] `regenerate_route_inventory`: regenerate compat/api/upstream-routes.json
- [A] `regenerate_migration_inventory`: regenerate compat/baseline/supported-migrations.txt
- [B] `update_schema_mapping:team_user`: update the Rust column set / serializers for `team_user` (Alter: json:access)
- [B] `update_schema_mapping:personal_access_tokens`: update the Rust column set / serializers for `personal_access_tokens` (Alter: json:scopes)
- [B] `update_schema_mapping:personal_access_tokens`: update the Rust column set / serializers for `personal_access_tokens` (Alter: timestamp:paused_at)
- [B] `update_schema_mapping:server_settings`: update the Rust column set / serializers for `server_settings` (Alter: )
- [B] `update_schema_mapping:team_user`: update the Rust column set / serializers for `team_user` (Alter: )
- [B] `update_schema_mapping:servers`: update the Rust column set / serializers for `servers` (Alter: )
- [B] `update_schema_mapping:instance_settings`: update the Rust column set / serializers for `instance_settings` (Alter: string:update_channel, string:update_custom_tag, string:previous_update_channel)
- [D] `review_component:api.auth-chain`: port behaviour change into api.auth-chain (path app/Http/Kernel.php; path app/Http/Middleware/ApiAbility.php; path app/Http/Middleware/EnsureTokenBelongsToCurrentTeamMember.php; path app/Models/PersonalAccessToken.php; upstream test tests/Feature/ApiTokenExpirationTest.php; upstream test tests/Feature/ApiTokenExpirationWarningTest.php; upstream test tests/Feature/ApiTokenLivewireAuthorizationTest.php; upstream test tests/Feature/Authorization/LegacyMemberTokenTest.php; upstream test tests/Feature/Security/ApiTokenCreationSecurityTest.php; table personal_access_tokens; table team_user; table personal_access_tokens; table team_user; table instance_settings)
- [D] `review_component:api.teams`: port behaviour change into api.teams (path app/Policies/TeamPolicy.php; table team_user; table team_user)
- [D] `review_component:api.projects`: port behaviour change into api.projects (path app/Http/Controllers/Api/ProjectController.php; path app/Models/Environment.php; path app/Models/Project.php; path app/Policies/EnvironmentPolicy.php; path app/Policies/ProjectPolicy.php; route DELETE api/v1/projects/{uuid}; route DELETE api/v1/projects/{uuid}/environments/{environment_name_or_uuid}; route DELETE api/v1/projects/{uuid}/environments/{environment_name_or_uuid}/envs/{env_id}; route DELETE api/v1/projects/{uuid}/envs/{env_id})
- [D] `review_component:api.route-inventory`: port behaviour change into api.route-inventory (path app/Http/Kernel.php; path routes/api.php)
- [D] `review_component:db.schema-guard`: port behaviour change into db.schema-guard (path database/migrations/2026_10_25_100000_add_access_grants_to_team_members_and_tokens.php; path database/migrations/2026_10_26_081954_add_git_https_credentials_to_applications_table.php; path database/migrations/2026_10_27_142736_add_paused_at_to_personal_access_tokens_table.php; path database/migrations/2026_10_28_204953_add_worker_host_to_scheduled_job_executions.php; path database/migrations/2026_10_28_211105_add_indexes_for_team_deployments_and_docker_cleanup_history.php; path database/migrations/2026_10_28_212642_add_indexes_for_server_settings_and_team_membership_lookups.php; path database/migrations/2026_10_28_214500_add_index_for_application_deployment_history.php; path database/migrations/2026_10_28_214953_add_index_for_interrupted_docker_cleanup_executions.php; path database/migrations/2026_10_28_224219_add_index_for_active_github_runner_executions.php; path database/migrations/2026_10_28_224423_add_index_for_dashboard_recent_deployments.php; path database/migrations/2026_10_28_232000_add_index_for_active_servers_by_private_key.php; path database/migrations/2026_10_29_221743_add_checkout_reminder_sent_at_to_subscriptions_table.php; path database/migrations/2026_10_30_144551_add_access_to_team_invitations_table.php; path database/migrations/2026_10_30_171359_drop_image_default_from_standalone_sqlites_table.php; path database/migrations/2026_11_01_142702_add_update_channel_to_instance_settings_table.php)
- [D] `review_component:api.private-keys`: port behaviour change into api.private-keys (route DELETE api/v1/security/keys/{uuid})
- [D] `review_component:ui.session`: port behaviour change into ui.session (table team_user; table team_user)
- [D] `review_component:api.servers`: port behaviour change into api.servers (path app/Models/Server.php; path app/Models/ServerSetting.php; table server_settings; table servers)
- [D] `review_component:ui.styles`: port behaviour change into ui.styles (path resources/css/app.css; path resources/views/components/reicon.blade.php)
- [D] `review_component:ui.frontend`: port behaviour change into ui.frontend (path resources/views/auth/two-factor-challenge.blade.php; path resources/views/components/forms/listbox.blade.php; path resources/views/components/modal-confirmation.blade.php; path resources/views/components/navbar.blade.php; path resources/views/components/server/sidebar.blade.php; path resources/views/livewire/dashboard.blade.php; path resources/views/livewire/project/index.blade.php; path resources/views/livewire/project/resource/index.blade.php; path resources/views/livewire/project/show.blade.php; path resources/views/livewire/security/private-key/index.blade.php; path resources/views/livewire/server/index.blade.php; path resources/views/livewire/server/show.blade.php; path resources/views/livewire/team/index.blade.php; path resources/views/livewire/team/member.blade.php; path resources/views/livewire/team/member/index.blade.php)
- [D] `review_component:ui.read-models`: port behaviour change into ui.read-models (path app/Livewire/Dashboard.php; path app/Livewire/Project/Show.php; path app/Livewire/Server/Show.php; path app/Livewire/Team/Member.php; table personal_access_tokens; table team_user; table personal_access_tokens; table server_settings; table team_user; table servers)
- [D] `review_component:remote.execution`: port behaviour change into remote.execution (path app/Helpers/SshMultiplexingHelper.php; path bootstrap/helpers/remoteProcess.php)
- [D] `review_component:server.validation`: port behaviour change into server.validation (table server_settings; table servers)
- [D] `review_component:databases.standalone`: port behaviour change into databases.standalone (path app/Http/Controllers/Api/DatabasesController.php; path app/Livewire/Project/Database/Postgresql/General.php; path app/Livewire/Project/Resource/Create.php; path app/Models/StandalonePostgresql.php; path app/Services/DatabaseStartCommandExecutor.php; path app/Traits/HasSecretManager.php; path resources/views/components/database/configuration-sidebar.blade.php; path resources/views/livewire/project/database/backup-edit/general.blade.php; path resources/views/livewire/project/database/scheduled-backups.blade.php; path resources/views/livewire/project/database/sqlite/connect-application.blade.php; path resources/views/livewire/project/resource/index.blade.php)

### Unmapped behaviour-relevant files (191)

- `app/Actions/CoolifyTask/RunRemoteProcess.php`
- `app/Actions/Database/StartDatabaseImport.php`
- `app/Actions/Fortify/ResetUserPassword.php`
- `app/Actions/IntegrationToken/CreateIntegrationToken.php`
- `app/Actions/IntegrationToken/UpdateIntegrationToken.php`
- `app/Actions/Proxy/CheckProxy.php`
- `app/Actions/Server/UpdateCoolify.php`
- `app/Actions/Stripe/CreateCheckoutSession.php`
- `app/Console/Commands/SyncCdn.php`
- `app/Console/Commands/UpdateChannel.php`
- `app/Console/Kernel.php`
- `app/Enums/AccessLevel.php`
- `app/Enums/AccessScope.php`
- `app/Events/ApplicationStopFailed.php`
- `app/Events/DnsRecordConfigurationFinished.php`
- `app/Events/DockerCleanupDone.php`
- `app/Http/Controllers/Api/ApplicationSecretManagerController.php`
- `app/Http/Controllers/Api/ApplicationsController.php`
- `app/Http/Controllers/Api/DnsController.php`
- `app/Http/Controllers/Api/IntegrationTokensController.php`
- `app/Http/Controllers/Api/OtherController.php`
- `app/Http/Controllers/Api/ResourcesController.php`
- `app/Http/Controllers/Api/ScheduledTasksController.php`
- `app/Http/Controllers/Api/SecretManagerController.php`
- `app/Http/Controllers/Api/SentinelController.php`
- `app/Http/Controllers/Api/ServicesController.php`
- `app/Http/Controllers/Api/SharedEnvironmentVariablesController.php`
- `app/Http/Controllers/Api/SharedVariableCanvasController.php`
- `app/Http/Controllers/Api/VolumeBackupsController.php`
- `app/Http/Controllers/Controller.php`
- `app/Http/Controllers/Webhook/Concerns/MatchesManualWebhookApplications.php`
- `app/Http/Middleware/CanCreateResources.php`
- `app/Http/Middleware/EnsureTeamWideAccess.php`
- `app/Jobs/ApiTokenExpirationWarningJob.php`
- `app/Jobs/CheckForUpdatesJob.php`
- `app/Jobs/CleanupHelperContainersJob.php`
- `app/Jobs/DatabaseBackupJob.php`
- `app/Jobs/DockerCleanupJob.php`
- `app/Jobs/ReconcileGithubRunnersJob.php`
- `app/Jobs/ScheduledJobManager.php`
- `app/Jobs/ServerConnectionCheckJob.php`
- `app/Jobs/ServerManagerJob.php`
- `app/Jobs/StripeProcessJob.php`
- `app/Jobs/SubscriptionCheckoutReminderJob.php`
- `app/Jobs/VolumeBackupJob.php`
- `app/Jobs/VolumeCloneJob.php`
- `app/Mcp/Concerns/BuildsResponse.php`
- `app/Mcp/Concerns/CallsRestApi.php`
- `app/Mcp/Concerns/ResolvesTeam.php`
- `app/Mcp/Resources/InfrastructureOverviewResource.php`
- … 141 more

### Unresolved questions

- 2026_10_28_211105_add_indexes_for_team_deployments_and_docker_cleanup_history: raw SQL cannot be analysed statically; review manually
- 2026_10_28_212642_add_indexes_for_server_settings_and_team_membership_lookups: raw SQL cannot be analysed statically; review manually
- 2026_10_28_214500_add_index_for_application_deployment_history: raw SQL cannot be analysed statically; review manually
- 2026_10_28_214953_add_index_for_interrupted_docker_cleanup_executions: raw SQL cannot be analysed statically; review manually
- 2026_10_28_224219_add_index_for_active_github_runner_executions: raw SQL cannot be analysed statically; review manually
- 2026_10_28_224423_add_index_for_dashboard_recent_deployments: raw SQL cannot be analysed statically; review manually
- 2026_10_28_232000_add_index_for_active_servers_by_private_key: raw SQL cannot be analysed statically; review manually
- some files matched no classification rule; extend PATH_RULES

## Commits

| commit | subject | categories | security |
|---|---|---|---|
| `24648d4b2` | fix(docker-cleanup): skip broadcast for executions of deleted servers | [Tests, DomainLogic] |  |
| `57cdd92f7` | fix(upgrade): count only scheduled jobs running on this host | [Tests, DatabaseSchema, DomainLogic] |  |
| `06747f995` | chore(release): bump Coolify version to 4.5.4 | [Configuration, BuildRelease, Unclassified] |  |
| `0951c5daa` | fix(database): index team deployment and Docker cleanup history lookups | [DatabaseSchema] |  |
| `42ba6ec66` | fix(docker): preserve storage directory .gitignore files | [Unclassified] |  |
| `2b6af2744` | fix(database): index server settings and team membership lookups | [DatabaseSchema] |  |
| `2616b4bba` | fix(deployments): index history ordering and skip payload columns in lists | [Tests, Frontend, DatabaseSchema, DomainLogic] |  |
| `68a08170e` | fix(docker): index interrupted cleanup lookup and process it in batches | [Tests, DatabaseSchema, BackgroundJobs] |  |
| `c23d3363f` | fix(deployments): load only commit details for the application heading | [Tests, Frontend, DomainLogic] |  |
| `7324b5ca7` | fix(webhooks): filter manual webhook applications by repository in SQL | [Tests, ExternalIntegrations] | yes |
| `55605f651` | fix(github-runners): select runner servers with a UNION of server ids | [Tests, DatabaseSchema, BackgroundJobs] |  |
| `c34f5f8e3` | fix(servers): index active server lookups by private key | [Tests, Frontend, DatabaseSchema] |  |
| `3d956c110` | fix(servers): skip patch check results in bulk settings loads | [Documentation, Tests, DatabaseSchema, DomainLogic, BackgroundJobs] |  |
| `a12f62ed4` | fix(dashboard): bound recent deployments query per application | [Tests, Frontend, DatabaseSchema, DomainLogic] |  |
| `3607abf7e` | fix(servers): load server manager servers in batches by id | [Tests, BackgroundJobs] |  |
| `f77894263` | docs(ai): document hidden counts in application queries | [Documentation] |  |
| `a3e2b9347` | Merge branch 'fix-bulk-env-flags-proxy-event' | [ApiChange] |  |
| `cd992e72e` | feat(tables): filter tables by clicking a cell value | [Frontend] |  |
| `af28ba8bb` | feat(tables): sort tables by clicking a column header | [Tests, Frontend] |  |
| `103af830e` | feat(dashboard): sort projects and servers by clicking a column header | [Tests, Frontend] |  |
| `c6f589994` | feat(tables): show loading state while a server-side sort runs | [Frontend] |  |
| `6e3ab703b` | fix(application): show watch paths for deploy key applications | [Tests, Frontend, DomainLogic] |  |
| `6f8cf5040` | feat(env): show compose defaults for empty variables in developer view | [Tests, Frontend] |  |
| `623d367a2` | fix(env): keep preview variable order when sorting is disabled | [Tests, DomainLogic] |  |
| `3048e9948` | fix(analytics): start Locations card on resource analytics tabs | [Tests, Frontend] |  |
| `b41ba87ff` | fix(application): reject Docker image names shared by building applications | [Tests, Frontend, ApiChange, DomainLogic, DeploymentEngine] |  |
| `6030d096b` | fix(team): send working invitation links to users without a password | [Tests, Frontend, DomainLogic] | yes |
| `a39370dbb` | fix(postgresql): prevent init script path traversal | [Tests, Frontend] |  |
| `96ed09f0c` | fix(auth): clear forced password reset after a password reset | [Tests, AuthenticationAuthorization] | yes |
| `607daf80b` | feat(auth): show two-factor code as separate digit boxes | [Tests, Frontend] |  |
| `43933ee6c` | fix(upgrade): quote root user credentials with spaces in the .env file | [Tests, BuildRelease, Unclassified] |  |
| `7faa46bbd` | fix(upgrade): ignore whitespace around root user values before quoting | [Tests, BuildRelease, Unclassified] |  |
| `f2ece40dd` | fix(terminal): fall back to usable shell for nologin and false (#12242) | [Tests, DomainLogic] | yes |
| `69bfb24ab` | feat(templates): add Hi.Events event management service (#12380) | [Frontend, ServiceTemplates] |  |
| `28c5af9b1` | fix(avatar): change avatar URL when an existing avatar is replaced | [Tests, DomainLogic] |  |
| `0d398ced2` | fix(servers): open patches page from the patches badge | [Frontend] |  |
| `72c820928` | feat(subscription): email checkout reminders for expired sessions | [Tests, Frontend, DatabaseSchema, DomainLogic, BackgroundJobs] |  |
| `4b541011c` | fix(api): hide locked values in resource and environment lists | [Tests, ApiChange, DomainLogic] | yes |
| `3e2fff060` | fix(deployments): stop the build server helper when a deployment is cancelled | [Tests, DeploymentEngine] | yes |
| `98b7de0ea` | fix(api): accept the same port mappings as the UI | [Tests, ApiChange, DomainLogic] | yes |
| `b3ec26c3b` | fix(proxy): stop routing the Reverb HTTP API through the dashboard domain | [Tests, DomainLogic] |  |
| `49346aeb2` | fix(ui): restore the main menu row that reopens the resource menu | [Documentation, Tests, Frontend, DomainLogic] |  |
| `a80f0c7ca` | fix(ui): name the deepest menu on the resource menu row | [Documentation, Tests, Frontend] |  |
| `e36c54c23` | fix(logs): style lines by their declared level, not by error keywords | [Frontend] |  |
| `a567a4b65` | fix(analytics): key traffic tables so the pager works after lazy load | [Frontend] |  |
| `751ecb91c` | ci: add a workflow to cleanup unused manifests (#12362) | [BuildRelease, Unclassified] |  |
| `8af0606a3` | docs(readme): add martiinii to small sponsors | [Documentation] |  |
| `690331e44` | docs(readme): remove Context.dev from huge sponsors | [Documentation] |  |
| `8178fa16b` | fix(ui): use red style for unused key deletion | [Frontend] |  |
| `759fbe5b3` | feat(deployments): show spinners for in-progress status | [Frontend] |  |
| `868c0f06b` | fix(api): require deploy ability for registry check and logout | [ApiChange] | yes |
| `23eebbf2a` | fix(servers): refresh provider status with server details action | [Tests, Frontend] |  |
| `faf0b6fac` | fix(runners): preserve unsaved settings during Sysbox checks | [Tests, Frontend] | yes |
| `d8c1b926c` | docs(readme): remove OpenElements from small sponsors | [Documentation] |  |
| `51029aa8b` | fix(new-resource): accept a base directory without a leading slash | [Tests, Frontend, DomainLogic] | yes |
| `f2f89ccfa` | fix(canvas): add SQLite to database engines | [Frontend] |  |
| `31653c27d` | fix(clone): add Compose labels to cloned volumes | [Tests, BackgroundJobs] | yes |
| `33a668279` | fix(backups): keep non-deployment helper containers during cleanup | [Tests, BackgroundJobs] | yes |
| `dbd211486` | fix(backups): let service databases choose the databases to back up | [Tests, Frontend, BackgroundJobs] |  |
| `b9ad48e10` | fix(deployments): pass port-suffixed compose service URL variables | [Tests, DeploymentEngine] |  |
| `972ab7f6f` | fix(deployments): keep build secret values out of Compose runtime | [Tests, DeploymentEngine, ContainerIntegration] | yes |
| `be7a99754` | fix(metrics): stop unreachable servers from blocking web workers | [Tests, DomainLogic] |  |
| `0b3ca7946` | fix(scheduled-tasks): show disabled state in task and backup lists | [Frontend] |  |
| `6c650dd12` | fix(api): document request bodies for shared env create and update endpoints | [Tests, ApiChange] |  |
| `dce02411a` | fix(service): keep WordPress URLs in sync with the Coolify domain | [ServiceTemplates] |  |
| `1af7de8a8` | fix(backups): pin a reachable IP for S3 uploads on IPv6-only servers | [Tests, DomainLogic, ContainerIntegration, BackgroundJobs] | yes |
| `8dea52609` | Revert "fix(service): keep WordPress URLs in sync with the Coolify domain" | [ServiceTemplates] |  |
| `bc855fdc6` | fix(transfer): resolve IPv6-only server host names | [ApiChange, DomainLogic] |  |
| `5cf1abc0f` | fix(env): keep pre-upgrade values for legacy compose variables | [Tests, DomainLogic] |  |
| `6977caaa9` | fix(service): store the Compose default for bare variable references | [Tests, DomainLogic] |  |
| `6ef29c64a` | fix(cdn): block releases sync before the current release is published | [Tests, BackgroundJobs] |  |
| `697d559c7` | v4.6 (#12401) | [Documentation, Tests, Frontend, ApiChange, DatabaseSchema, DomainLogic, AuthenticationAuthorization, DeploymentEngine, ContainerIntegration, Configuration, ExternalIntegrations, BackgroundJobs, BuildRelease, Unclassified] | yes |
| `2b4a4bff0` | fix(release): name the local tags that differ from origin | [BuildRelease] |  |
| `0becfba91` | feat(release): bump coolify version to v4.6.0 | [Documentation, Unclassified] |  |
