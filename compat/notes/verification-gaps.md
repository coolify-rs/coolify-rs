# Verification gaps

What has not been verified against upstream, or only against fakes, and upstream defects seen while porting. User-facing
differences are in `docs/KNOWN_LIMITATIONS.md`; this file is for the team.

## General

- Upstream's own Pest suite was not run (PHP 8.4 unavailable locally; upstream CI does not run it either).
- Differential tests cover the routes with fixtures for one instance; no fuzzing.
- `environment_variables.value` storage format (`encryptString` vs serialized `encrypt`) is inferred from source, not
  confirmed at runtime; the decrypter handles both.
- Migration engine classification is uncalibrated; its risk rules are hand-written heuristics.
- No end-to-end deployment of coolify-rs beside a live upstream instance beyond the local reference environment.
- Performance has not been measured for either implementation.
- Reproducible builds are not claimed: `Cargo.lock` and the toolchain are pinned, but bit-for-bit reproducibility was
  not verified.

## External providers

Provider base URLs change only through the test-only setting `COOLIFY_RS_TEST_ONLY_FAKE_PROVIDERS` (a loopback `http`
origin). Everything below ran against local fakes or `Http::fake` under tinker on the reference, never the real service.

- Secret managers, Cloudflare, DNS and GitHub runners: `compat/diff/fake_providers.py`, `fake_git_provider.py`;
  `dns_flows_diff.py`, `dns_transfer_diff.py` and `github_runners_reconcile_diff.py` run the reference with `Http::fake`
  and `Bus::fake`. The browser never drives those reference flows. `compat/ui/dns_screens.mjs` shows the reference's key
  and tunnel conflict states as component HTML placed on the real page.
- Not provable with fakes: GitHub's manifest conversion, JWT clock checks, real installation permissions, real runner
  registration (the JIT config was fake, the runner image a stub), Cloudflare's real answers and propagation, Doppler,
  Infisical, Vault, Have I Been Pwned, GitLab's real OAuth. Gitea was a real container. A real GitHub App test is still
  to do.
- Git sources and webhooks: `git_sources_diff.py`, `webhooks_diff.py`, `git_source_deploy_diff.py`,
  `compat/ui/source_flows.mjs`, `compat/ui/private_forms.mjs` against `fake_git_provider.py`. Deploy keys ran against a
  real SSH mirror. The GitHub App path of application create is verified only up to the App lookup.
  `ApplicationPullRequestUpdateJob` ran against a fake GitHub API only.
- "Load pull requests" (`githubApi`) never ran against GitHub or a fake; comparisons stop at `SafeWebhookUrl`, which
  refuses local addresses on both sides.
- Cloud token "Validate" and "Validate and add", and the cloud provider UI, were exercised only up to the form
  validation or `Http::fake`. `DeleteServer` with providers ran only against a local fake.
- Pushover and Telegram send-test and notifications were compared only through `Http::fake` on the reference.
- OIDC: HTTPS issuers are required, so the local fake could not be used; id_token checks have unit tests only. GitHub,
  Google, GitLab, Azure, Discord, Bitbucket, Infomaniak, Clerk and Zitadel were compared up to the redirect; only
  Authentik did a full round trip (against a fake). Provider user mappings have unit tests only.
- Coolify Cloud path (`SELF_HOSTED=false`) of Admin and Subscription ran only against a Stripe fake
  (`compat/diff/stripe_fake.py`).
- Instance settings "Check now", "Detect" and Help without mail settings are verified against local fakes only
  (`COOLIFY_RS_VERSIONS_URL`, `COOLIFY_RS_IFCONFIG_URL`, `COOLIFY_RS_FEEDBACK_URL`).

## S3 and downloads

- S3 uploads and restores could not run against the local MinIO in the page comparison: both sides refuse a private
  endpoint unless `webhook_allowed_internal_hosts` names it, and that setting stayed unchanged. The refusal matches.
- `PreparesBackupDownloads` with S3 download storage and S3 file downloads have never run: they need
  `file_download_s3_storage_id` in `instance_settings`.
- `/download/backup/{id}` and `/download/volume-backup/{id}` read over `cat` on SSH instead of SFTP. Through the
  `scripts/ssh-via-orb.sh` wrapper, `orb` truncates large output (about 4 to 5 MB) with exit code 0, so large downloads
  were not verified; small ones were byte-identical.
- `s3_backup_diff.py` needs `scripts/dev-server-container.sh`: the trusted host `coolify-minio` only resolves on the
  reference's Docker network.

## Areas run partly or not at all

- Databases: reading MongoDB credentials from the container and the MySQL and MariaDB backup branches are ported but
  only PostgreSQL ran on the VM. `clone_volumes` across servers was only checked for the new volume existing, not its
  contents; the application path of cross-server volume clones was not run (services were).
- Scheduler: `dump_all` backups, S3 and every provider check were not run by the scheduler on the VM;
  `cleanup_stucked_resources`, `RegenerateSslCertJob`, `CleanupOrphanedPreviewContainersJob` and
  `ServerCloudProviderStatusCheckJob` ran only in tests or not at all.
- Scheduled tasks: "Execute now" and running task executions were not compared (fixtures were not running
  applications).
- Server transfer: no transfer between two real instances with a second Docker host. Export, job and source side ran
  against a recorded fake target; import and claim ran on the reference and on coolify-rs against the SSH testing host.
- Traffic analytics: Traefik verified; the Caddy proxy not run. The dashboard widget stays empty in Chrome on both
  sides (upstream's lazy placeholder has no box), so its data is compared directly.
- Maintenance mode: the Caddy configuration ran only against Traefik on the VM.
- File browser: the helper container path for images without a shell was not run on the VM.
- Terminal for a non-root SSH user: `sudo docker exec` / `sudo docker attach` checked as strings against
  `containerCommand()`, not run (the test VM only has root).
- Multi-server: the combined multi-server notification's delivery was not compared. The destination picker of the
  create forms was not compared with a Swarm server. Deregistering GitHub runners on the force-delete path was not run.
  The response of application UI actions and the Destination section when a row's destination is gone was read from
  the PHP, not compared.
- Instance backup page beyond "Localhost is not ready" was built from the Blade views and not compared: localhost is
  not reachable in the test setup.
- `git_deploy_diff.py`, `force_start_diff.py` and `git_source_deploy_diff.py` clone from local git servers on the test
  VM, not GitHub. Bitbucket exists upstream only as the `git_type` of a preview.
- Compose: the `content` column of a file storage from the repository (`is_based_on_git`) can be filled on the reference
  and empty here after a preserved-repository deployment; the cause on the reference was not established and
  `git_deploy_diff.py` does not compare it. `ServerFilesFromServerJob` runs beside the deployment on both sides, so when
  that content fills depends on timing.
- Link Provider: its searches and link are verified only against a local fake.

## Upstream defects observed

- `Project::deleting` / `Environment::deleting` iterate a relation object instead of its results, so shared environment
  variables are likely not deleted (orphans). coolify-rs matches upstream's end state pending runtime confirmation.
- `POST`/`PATCH /applications/{uuid}/storages` responses include the owning application with its server, including
  `proxy.last_saved_proxy_configuration`, also for tokens without `read:sensitive`. coolify-rs keeps this for parity
  (unlike database storages, see KNOWN_LIMITATIONS).
- Service clone with volumes on the same server can die in its `StopService` step with "Nesting level too deep"
  (`Closure (services.php:783)`), leaving nothing copied and the source stopped. coolify-rs completes it, so
  `multi_server_diff.py` reports those two reference checks as failed.
- `StatusChanged`'s mail view reads a variable `toMail` does not pass: tinker renders an empty link, a queue worker
  would throw. Upstream never sends it.
- `SslExpirationNotification` has no caller that passes a team, upstream or here.
- Upstream glitches reproduced on purpose: the Issued tokens list hides tokens created since the page load, and the
  new-tag modal shows no validation error (`wire:ignore`).
- Dead code at the baseline: nothing sets `addDomainDnsFailed` or `editDomainDnsFailed`, and nothing calls
  `buildSuggestedWwwRows`, so the DNS gate and suggested www rows of the application domain dialogs never appear.
- A cron step below zero (`*/-5`) loops forever in upstream's parser; `validate_cron_expression()` refuses it first.
