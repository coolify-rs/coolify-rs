<script lang="ts">
  // Port of upstream livewire/project/application/advanced.blade.php. The GPU list and its refresh
  // read server metadata, which is shown on the server pages; they are not ported here.
  import Callout from '../Callout.svelte';
  import Empty from '../Empty.svelte';
  import Reicon from '../Reicon.svelte';
  import { link } from '../../lib/router.svelte';
  import FormInput from '../FormInput.svelte';
  import FormTextarea from '../FormTextarea.svelte';
  import Listbox from '../Listbox.svelte';
  import SettingsSection from '../SettingsSection.svelte';
  import SettingsSubsection from '../SettingsSubsection.svelte';
  import UnsavedBar from '../UnsavedBar.svelte';
  import { ApiError, request } from '../../lib/api';
  import { resPath, type ResourceKind } from '../../lib/sharedApi';
  import { diffForHumans } from '../../lib/format';
  import { notify } from '../../lib/toast.svelte';

  let { kind = 'application', uuid, base, onchange }: { kind?: ResourceKind; uuid: string; base: string; onchange?: () => void } = $props();

  let d = $state<any>(null);
  let s = $state<Record<string, any>>({});
  let customName = $state('');
  let savedCustomName = $state('');
  let prefix = $state('');
  let savedPrefix = $state('');
  let stopGrace = $state('');
  let drain = $state('');
  let maxRestart = $state('');
  let resolverMode = $state('server');
  let resolverName = $state('');
  let savedResolverName = $state('');
  let gpu = $state({ gpuDriver: '', gpuCount: '', gpuDeviceIds: '', gpuOptions: '' });
  let savedGpu = $state('');
  let errors = $state<Record<string, string[]>>({});

  const path = $derived(`${resPath(kind, uuid)}/advanced`);
  const canUpdate = $derived(d?.can_update ?? false);
  const onOff = [{ value: true, label: 'Enabled' }, { value: false, label: 'Disabled' }];

  async function load() {
    const r = await request<any>('GET', path);
    d = r;
    s = r.settings;
    s.inject_build_args_to_dockerfile ??= true;
    s.include_source_commit_in_build ??= false;
    s.is_git_shallow_clone_enabled ??= false;
    customName = savedCustomName = s.custom_internal_name ?? '';
    prefix = savedPrefix = s.custom_container_name_prefix ?? '';
    stopGrace = s.stop_grace_period === null || s.stop_grace_period === undefined ? '' : String(s.stop_grace_period);
    drain = String(Math.min(Math.max(s.rolling_update_drain_seconds ?? 5, 0), 300));
    maxRestart = String(r.max_restart_count);
    resolverMode = r.certificate_resolver_mode;
    resolverName = savedResolverName = r.certificate_resolver_name ?? '';
    gpu = { gpuDriver: s.gpu_driver ?? '', gpuCount: s.gpu_count ?? '', gpuDeviceIds: s.gpu_device_ids ?? '', gpuOptions: s.gpu_options ?? '' };
    savedGpu = JSON.stringify(gpu);
  }

  $effect(() => {
    load();
  });

  async function instantSave() {
    try {
      const body = Object.fromEntries(
        ['disable_build_cache', 'inject_build_args_to_dockerfile', 'include_source_commit_in_build', 'is_auto_deploy_enabled', 'is_git_submodules_enabled', 'is_git_lfs_enabled', 'is_git_shallow_clone_enabled', 'is_raw_compose_deployment_enabled', 'connect_to_docker_network', 'is_consistent_container_name_enabled', 'is_log_drain_enabled', 'is_gpu_enabled', 'is_gzip_enabled', 'is_stripprefix_enabled'].map((k) => [k, s[k]]),
      );
      const r = await request<{ message: string }>('POST', `${path}/instant`, body);
      notify.success(r.message);
      onchange?.();
    } catch (err) {
      notify.failure(err instanceof ApiError ? err.message : String(err));
      await load();
    }
  }

  async function save(form: string, body: Record<string, unknown>) {
    errors = {};
    try {
      const r = await request<{ message: string }>('POST', `${path}/save`, { form, ...body });
      notify.success(r.message);
      await load();
      onchange?.();
    } catch (err) {
      const f = err instanceof ApiError ? err : new ApiError(0, String(err));
      if (Object.keys(f.errors).length) errors = f.errors;
      else notify.failure(f.message);
    }
  }

  function saveResolverMode() {
    // "custom" waits for a resolver name before it saves.
    if (resolverMode === 'custom' && resolverName === '') {
      errors = {};
      return;
    }
    save('resolver', { certificateResolverMode: resolverMode, certificateResolverName: resolverName });
  }

  // `refreshGpus()`.
  async function refreshGpus() {
    try {
      const r = await request<{ message: string }>('POST', `${resPath(kind, uuid)}/gpus/refresh`, {});
      notify.success(r.message);
      await load();
    } catch (err) {
      notify.failure(err instanceof ApiError ? err.message : String(err));
    }
  }
</script>

{#if d}
  <div>
    <div class="flex flex-col gap-6">
      <SettingsSection id="advanced-build-section" title="Build & deploy" description="How images are built, how deployments start, and how the repository is cloned.">
        <div class="grid w-full gap-4 sm:grid-cols-2">
          <Listbox id="disableBuildCache" label="Build cache" helper="Disabling the cache forces a completely fresh Docker build on every deployment." disabled={!canUpdate} bind:value={s.disable_build_cache} onchange={instantSave} options={[{ value: false, label: 'Use Docker build cache' }, { value: true, label: 'Rebuild from scratch every time' }]} />
          <Listbox id="injectBuildArgsToDockerfile" label="Build arguments" helper="When injected automatically, coolify-rs adds ARG statements to your Dockerfile for build-time variables. Manage them manually to preserve Docker build cache." disabled={!canUpdate} bind:value={s.inject_build_args_to_dockerfile} onchange={instantSave} options={[{ value: true, label: 'Inject build args automatically' }, { value: false, label: 'Managed manually in Dockerfile' }]} />
          <Listbox id="includeSourceCommitInBuild" label="Source commit availability" helper="SOURCE_COMMIT (git commit hash) is always available at runtime. Making it available during build invalidates the cache on every commit." disabled={!canUpdate} bind:value={s.include_source_commit_in_build} onchange={instantSave} options={[{ value: false, label: 'Runtime only (preserves cache)' }, { value: true, label: 'Available during build' }]} />
        </div>
        {#if d.git_based}
          <SettingsSubsection id="advanced-deployment-section" title="Deployment" helper="Automatic deployments from Git webhooks.">
            <div class="grid w-full gap-4 sm:grid-cols-2">
              <Listbox id="isAutoDeployEnabled" label="Auto deploy" helper="Automatically deploy new commits based on Git webhooks." disabled={!canUpdate} bind:value={s.is_auto_deploy_enabled} onchange={instantSave} options={[{ value: true, label: 'Deploy on push (webhooks)' }, { value: false, label: 'Manual deployments only' }]} />
            </div>
          </SettingsSubsection>
          <SettingsSubsection id="advanced-git-section" title="Git" helper="Options applied while cloning the repository during builds.">
            <div class="grid w-full gap-4 sm:grid-cols-2">
              <Listbox id="isGitSubmodulesEnabled" label="Submodules" helper="Allow Git submodules during the build process." disabled={!canUpdate} bind:value={s.is_git_submodules_enabled} onchange={instantSave} options={[{ value: true, label: 'Clone submodules' }, { value: false, label: 'Skip submodules' }]} />
              <Listbox id="isGitLfsEnabled" label="Git LFS" helper="Allow Git LFS during the build process." disabled={!canUpdate} bind:value={s.is_git_lfs_enabled} onchange={instantSave} options={onOff} />
              <Listbox id="isGitShallowCloneEnabled" label="Clone depth" helper="Shallow cloning (--depth=1) speeds up deployments by only fetching the latest commit, useful for large repositories." disabled={!canUpdate} bind:value={s.is_git_shallow_clone_enabled} onchange={instantSave} options={[{ value: false, label: 'Full history' }, { value: true, label: 'Shallow clone (latest commit only)' }]} />
            </div>
          </SettingsSubsection>
        {/if}
        {#if d.build_pack === 'dockercompose'}
          <SettingsSubsection id="advanced-compose-section" title="Docker compose" helper="Advanced behavior for compose-based deployments.">
            <div class="grid w-full gap-4 sm:grid-cols-2">
              <Listbox id="isRawComposeDeploymentEnabled" label="Compose deployment" helper="WARNING: Advanced use cases only. In raw mode your compose file is deployed as-is. Nothing is modified by coolify-rs and you need to configure the proxy parts. More info in the <a class='underline dark:text-white' href='https://coolify.io/docs/knowledge-base/docker/compose#raw-docker-compose-deployment'>documentation</a>." disabled={!canUpdate} bind:value={s.is_raw_compose_deployment_enabled} onchange={instantSave} options={[{ value: false, label: 'Managed by coolify-rs' }, { value: true, label: 'Raw (deploy file as-is)' }]} />
              <Listbox id="isConnectToDockerNetworkEnabled" label="Predefined network" helper="By default a compose resource only gets its own internal network. Connecting to a coolify-rs predefined network may require different internal DNS names. More info <a class='underline dark:text-white' target='_blank' href='https://coolify.io/docs/knowledge-base/docker/compose#connect-to-predefined-networks'>here</a>." disabled={!canUpdate} bind:value={s.connect_to_docker_network} onchange={instantSave} options={[{ value: false, label: 'Isolated network only' }, { value: true, label: 'Connect to predefined network' }]} />
            </div>
          </SettingsSubsection>
        {/if}
      </SettingsSection>

      <SettingsSection id="advanced-container-section" title="Container" description="Naming, shutdown, logs, and GPU access for the deployed container.">
        <div class="grid w-full gap-4 sm:grid-cols-2">
          <Listbox id="isConsistentContainerNameEnabled" label="Container naming" helper="With a consistent name the container is always called {d.uuid}. <span class='font-bold dark:text-warning'>You will lose the rolling update feature!</span>" disabled={!canUpdate} bind:value={s.is_consistent_container_name_enabled} onchange={instantSave} options={[{ value: false, label: 'Generated name (rolling updates)' }, { value: true, label: 'Consistent name (no rolling updates)' }]} />
          {#if s.is_consistent_container_name_enabled}
            <form class="w-full" onsubmit={(e) => (e.preventDefault(), save('custom_name', { customInternalName: customName }))}>
              <UnsavedBar dirty={customName !== savedCustomName} onsave={() => save('custom_name', { customInternalName: customName })} onreset={() => (customName = savedCustomName)} />
              <FormInput helper="You can add a custom name for your container.<br><br>The name is converted to slug format when saved. <span class='font-bold dark:text-warning'>You will lose the rolling update feature!</span>" id="customInternalName" label="Custom container name" disabled={!canUpdate} bind:value={customName} />
            </form>
          {:else}
            <form class="w-full" onsubmit={(e) => (e.preventDefault(), save('name_prefix', { customContainerNamePrefix: prefix }))}>
              <UnsavedBar dirty={prefix !== savedPrefix} onsave={() => save('name_prefix', { customContainerNamePrefix: prefix })} onreset={() => (prefix = savedPrefix)} />
              <FormInput helper="Optional prefix for generated container names. Containers are named <span class='font-bold'>prefix-timestamp</span>, for example <span class='font-bold'>shop-api-20260908T141530</span>, instead of starting with <span class='font-bold'>{d.uuid}</span>.<br><br>The prefix is converted to slug format when saved, can be up to 40 characters and must be unique on this server. Rolling updates keep working." id="customContainerNamePrefix" label="Container name prefix" placeholder="e.g. my-api" disabled={!canUpdate} bind:value={prefix} />
            </form>
          {/if}
        </div>
        <SettingsSubsection id="advanced-operations-section" title="Operations" helper="Shutdown and restart behavior for this application's containers.">
          <div class="grid w-full gap-4 lg:grid-cols-2">
            <FormInput type="number" id="stopGracePeriod" label="Stop grace period (seconds)" placeholder="30" helper="How long to wait for graceful shutdown during rolling updates, manual stops, and restarts. Applies to all containers for this application. Saved automatically. Default: 30 seconds. Range: 1-3600 seconds (1 hour)." min="1" max="3600" disabled={!canUpdate} bind:value={stopGrace} error={errors.stopGracePeriod?.[0]} onchange={() => save('stop_grace', { stopGracePeriod: stopGrace })} />
            {#if d.build_pack !== 'dockercompose'}
              <FormInput type="number" min="0" max="300" id="rollingUpdateDrainSeconds" label="Rolling update drain time (seconds)" helper="How long to wait after the new container is healthy before the old containers stop during a rolling update. The proxy needs this time to send traffic to the new container. Saved automatically. Default: 5 seconds. Range: 0-300 seconds. Set to 0 to turn off the wait." disabled={!canUpdate} bind:value={drain} error={errors.rollingUpdateDrainSeconds?.[0]} onchange={() => save('drain', { rollingUpdateDrainSeconds: drain })} />
            {/if}
            <FormInput type="number" min="0" id="maxRestartCount" label="Max restart count" helper="Maximum number of restarts before coolify-rs automatically stops the application and sends a notification. Saved automatically. Set to 0 to disable the limit." disabled={!canUpdate} bind:value={maxRestart} onchange={() => save('max_restart', { maxRestartCount: maxRestart })} />
          </div>
        </SettingsSubsection>
        <SettingsSubsection id="advanced-logs-section" title="Logs" helper="Forward container logs to an external endpoint.">
          <div class="grid w-full gap-4 sm:grid-cols-2">
            <Listbox id="isLogDrainEnabled" label="Log drain" helper="Drain logs to the log drain endpoint configured in your Server settings." disabled={!canUpdate} bind:value={s.is_log_drain_enabled} onchange={instantSave} options={[{ value: false, label: 'Disabled' }, { value: true, label: 'Send logs to the log drain endpoint' }]} />
          </div>
        </SettingsSubsection>
        {#if d.build_pack !== 'dockercompose'}
          <SettingsSubsection id="advanced-gpu-section" title="GPU" helper="Give this application access to the host's GPUs. More info <a href='https://docs.docker.com/compose/gpu-support/' class='underline dark:text-white' target='_blank'>here</a>.">
            {#snippet actions()}
              <button type="button" class="button" disabled={!canUpdate || !d.gpu_servers.some((g: any) => g.is_functional)} title={d.gpu_servers.some((g: any) => g.is_functional) ? undefined : 'Validate the server connection first.'} onclick={refreshGpus}>
                <Reicon name="refresh" class="size-3.5" />
                Refresh GPUs
              </button>
            {/snippet}
            <Callout type="info" title="Detected GPUs" class="mb-5">
              {#each d.gpu_servers as server, i (server.uuid)}
                <div class={i > 0 ? 'mt-2' : ''}>
                  <p class="font-medium text-neutral-700 dark:text-fg">
                    <a use:link href="/server/{server.uuid}" class="underline">{server.name}</a>
                    {#if server.collected_at}
                      <span class="font-normal text-neutral-500 dark:text-fg-dim">· checked {diffForHumans(server.collected_at)}</span>
                    {/if}
                  </p>
                  <div class="text-[12px] leading-5 text-neutral-600 dark:text-fg-dim">
                    {#if !server.gpus}
                      <p>Not checked yet. Refresh to find GPUs.</p>
                    {:else if server.gpus.length === 0}
                      <p>No NVIDIA GPU found (<span class="font-mono">nvidia-smi</span> is not installed or shows no GPU).</p>
                    {:else}
                      <ul>
                        {#each server.gpus as gpu (gpu.uuid)}
                          <li>
                            <span class="font-mono font-medium text-neutral-950 dark:text-fg">{gpu.index}</span>
                            · {gpu.name} · {Math.round((gpu.memory_mb / 1024) * 10) / 10} GB ·
                            <span class="font-mono">{gpu.uuid}</span>
                          </li>
                        {/each}
                      </ul>
                      {#if !server.nvidia_container_toolkit}
                        <p class="mt-1 text-warning-900 dark:text-warning">
                          NVIDIA Container Toolkit not found. Containers cannot use these GPUs until you
                          <a href="https://docs.nvidia.com/datacenter/cloud-native/container-toolkit/latest/install-guide.html" class="underline" target="_blank">install it</a> and restart Docker.
                        </p>
                      {/if}
                    {/if}
                  </div>
                </div>
              {/each}
            </Callout>
            <div class="grid w-full gap-4 sm:grid-cols-2">
              <Listbox id="isGpuEnabled" label="GPU access" disabled={!canUpdate} bind:value={s.is_gpu_enabled} onchange={instantSave} options={[{ value: false, label: 'Disabled' }, { value: true, label: 'Enabled' }]} />
            </div>
            {#if s.is_gpu_enabled}
              <form id="gpu-settings-form" class="mt-5 flex w-full flex-col gap-4 border-t border-neutral-200 pt-5 dark:border-white/[0.07]" onsubmit={(e) => (e.preventDefault(), save('gpu', gpu))}>
                <UnsavedBar dirty={JSON.stringify(gpu) !== savedGpu} onsave={() => save('gpu', gpu)} onreset={() => (gpu = JSON.parse(savedGpu))} />
                <div class="grid gap-4 sm:grid-cols-2">
                  <FormInput label="GPU driver" id="gpuDriver" disabled={!canUpdate} bind:value={gpu.gpuDriver} />
                  <FormInput label="GPU count" placeholder="Empty means use all GPUs" helper="A number, or all." id="gpuCount" disabled={!canUpdate} bind:value={gpu.gpuCount} />
                </div>
                <FormInput label="GPU device ids" placeholder="0,2" helper="Comma separated list of device ids. More info <a href='https://docs.docker.com/compose/gpu-support/#access-specific-devices' class='underline dark:text-white' target='_blank'>here</a>." id="gpuDeviceIds" disabled={!canUpdate} bind:value={gpu.gpuDeviceIds} />
                <FormTextarea rows={6} label="GPU options" id="gpuOptions" placeholder="key=value" helper="Driver options. Use one key=value pair per line, or a YAML mapping." disabled={!canUpdate} bind:value={gpu.gpuOptions} />
              </form>
            {/if}
          </SettingsSubsection>
        {/if}
      </SettingsSection>

      <SettingsSection id="advanced-proxy-section" title="Proxy" description="How the proxy serves traffic for this application.">
        {#if s.is_container_label_readonly_enabled}
          <div class="grid w-full gap-4 sm:grid-cols-2">
            <Listbox id="isGzipEnabled" label="Gzip compression" helper="Some services compress data by default. In that case you do not need this." disabled={!canUpdate} bind:value={s.is_gzip_enabled} onchange={instantSave} options={onOff} />
            <Listbox id="isStripprefixEnabled" label="Path prefixes" helper="Strip Prefix removes prefixes from paths, like /api/ to /." disabled={!canUpdate} bind:value={s.is_stripprefix_enabled} onchange={instantSave} options={[{ value: true, label: 'Strip prefixes' }, { value: false, label: 'Keep paths as-is' }]} />
            {#if d.is_traefik}
              <Listbox id="certificateResolverMode" label="Certificate resolver" helper="Traefik certificate resolver for this application's HTTPS routes. Choose <span class='font-bold'>None</span> for domains that use certificates from Traefik's dynamic configuration (for example Cloudflare Origin CA certificates). Redeploy the application to apply the change." disabled={!canUpdate} bind:value={resolverMode} onchange={saveResolverMode} options={[{ value: 'server', label: `Server setting (${d.server_certificate_resolver})` }, { value: 'none', label: 'None (use your own certificates)' }, { value: 'custom', label: 'Custom resolver' }]} />
              {#if resolverMode === 'custom'}
                <form class="w-full" onsubmit={(e) => (e.preventDefault(), saveResolverMode())}>
                  <UnsavedBar dirty={resolverName !== savedResolverName} onsave={saveResolverMode} onreset={() => (resolverName = savedResolverName)} />
                  <FormInput id="certificateResolverName" label="Resolver name" placeholder="cloudflare" required helper="Name of a certificate resolver defined in your Traefik configuration. Letters, numbers, hyphens, and underscores only." disabled={!canUpdate} bind:value={resolverName} error={errors.certificateResolverName?.[0]} />
                </form>
              {/if}
            {/if}
          </div>
        {:else}
          <Empty size="sm" title="Proxy behavior is managed through labels" description="Container labels are managed manually for this application. Switch label management back to coolify-rs to configure the proxy here." iconName="globe">
            {#snippet contents()}
              <a class="button" href="{base}#container-labels-section">Go to Container labels</a>
            {/snippet}
          </Empty>
        {/if}
      </SettingsSection>
    </div>
  </div>
{/if}
