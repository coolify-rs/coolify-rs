<script lang="ts">
  // Port of upstream livewire/project/application/general.blade.php (+
  // Project\Application\General) with livewire/project/application/internal-access.blade.php,
  // for the build packs coolify-rs deploys: Docker image, Dockerfile, Static,
  // Nixpacks and Railpack (build strategy, site type, web server, paths,
  // build commands, Nginx configuration, image registry). HTTP Basic
  // Authentication, custom Docker options, deployment commands and build
  // servers have no backend in coolify-rs yet: they render as upstream renders
  // them and say so when used. Docker Compose sections are not ported.
  import { untrack } from 'svelte';
  import Reicon from './Reicon.svelte';
  import Modal from './Modal.svelte';
  import Empty from './Empty.svelte';
  import Callout from './Callout.svelte';
  import Listbox from './Listbox.svelte';
  import Helper from './Helper.svelte';
  import CopyInput from './CopyInput.svelte';
  import FormInput from './FormInput.svelte';
  import MonacoEditor from './MonacoEditor.svelte';
  import FormTextarea from './FormTextarea.svelte';
  import RepositoryPaths from './RepositoryPaths.svelte';
  import UnsavedBar from './UnsavedBar.svelte';
  import ConfirmModal from './ConfirmModal.svelte';
  import SettingsSection from './SettingsSection.svelte';
  import DatabaseSubsection from './DatabaseSubsection.svelte';
  import { appUi, ApiError, type ApplicationRecord, type ApplicationState } from '../lib/api';
  import { link } from '../lib/router.svelte';
  import { notify } from '../lib/toast.svelte';

  let {
    record,
    appState,
    base,
    onsaved,
  }: { record: ApplicationRecord; appState: ApplicationState; base: string; onsaved: () => Promise<void> } = $props();

  type Form = {
    name: string;
    description: string;
    imageName: string;
    imageTag: string;
    portsExposes: string;
    portsMappings: string;
    customNetworkAliases: string;
    customDockerRunOptions: string;
    preDeploymentCommand: string;
    postDeploymentCommand: string;
    customLabels: string;
    staticImage: string;
    baseDirectory: string;
    publishDirectory: string;
    installCommand: string;
    buildCommand: string;
    startCommand: string;
    dockerfileLocation: string;
    dockerfileTargetBuild: string;
    watchPaths: string;
    customNginxConfiguration: string;
    dockerfile: string;
  };
  const str = (v: unknown) => (v == null ? '' : String(v));
  const setting = (k: string) => !!record.settings?.[k];
  const fromRecord = (r: ApplicationRecord, s: ApplicationState): Form => ({
    name: r.name ?? '',
    description: str(r.description),
    imageName: str(r.docker_registry_image_name),
    imageTag: str(r.docker_registry_image_tag),
    portsExposes: str(r.ports_exposes),
    portsMappings: str(r.ports_mappings),
    customNetworkAliases: str(r.custom_network_aliases),
    customDockerRunOptions: str(r.custom_docker_run_options),
    preDeploymentCommand: str(r.pre_deployment_command),
    postDeploymentCommand: str(r.post_deployment_command),
    customLabels: str(s.custom_labels),
    staticImage: str(r.static_image),
    baseDirectory: str(r.base_directory),
    publishDirectory: str(r.publish_directory),
    installCommand: str(r.install_command),
    buildCommand: str(r.build_command),
    startCommand: str(r.start_command),
    // syncData(): `dockerfile_location ?? ('/Dockerfile' for the Dockerfile build pack)`.
    dockerfileLocation: str(r.dockerfile_location ?? (r.build_pack === 'dockerfile' ? '/Dockerfile' : null)),
    dockerfileTargetBuild: str(r.dockerfile_target_build),
    watchPaths: str(r.watch_paths),
    customNginxConfiguration: str(r.custom_nginx_configuration),
    dockerfile: str(r.dockerfile),
  });

  // The form starts from the record once; status refreshes of the record do not reset typing.
  let form = $state<Form>(untrack(() => fromRecord(record, appState)));
  let saved = $state<Form>(untrack(() => fromRecord(record, appState)));
  let saving = $state(false);
  let error = $state<ApiError | null>(null);
  let networking = $state(false);
  let resetting = $state(false);
  let labelsManaged = $state(untrack(() => setting('is_container_label_readonly_enabled')));
  let labelsEscaped = $state(untrack(() => setting('is_container_label_escape_enabled')));
  let basicAuth = $state(untrack(() => !!record.is_http_basic_auth_enabled));
  let internalHostname = $state<string | null>(null);
  let internalHostnameLoaded = $state(false);

  const canUpdate = $derived(appState.can_update);
  // The `buildPack` property: the listbox saves a new value right away (`updatedBuildPack()`).
  let buildPack = $state(untrack(() => record.build_pack ?? ''));
  // `$isStatic` / `$siteType` (`settings.is_static` / `is_spa`).
  let isStatic = $state(untrack(() => setting('is_static')));
  let siteType = $state<'dynamic' | 'static' | 'spa'>(
    untrack(() => (setting('is_static') ? (setting('is_spa') ? 'spa' : 'static') : 'dynamic')),
  );
  let buildServer = $state(untrack(() => setting('is_build_server_enabled')));
  let generatingNginx = $state(false);
  const storedPack = $derived(record.build_pack ?? '');
  // could_set_build_commands()
  const canSetBuildCommands = $derived(storedPack === 'nixpacks' || storedPack === 'railpack');
  const hasDockerfile = $derived(str(record.dockerfile) !== '');
  // is_github_based() && !is_public_repository()
  const privateGit = $derived(!!record.source_type && !appState.source_is_public);
  const registryRequired = $derived(
    !!appState.server_is_swarm || setting('is_build_server_enabled') || appState.server_can_build === false,
  );
  const buildServerFallback = $derived(
    appState.build_server_fallback
      ? 'If no usable build server is available, Coolify builds on the deployment server.'
      : 'If no usable build server is available, the deployment fails.',
  );
  const NIXPACKS_HELPER = (pack: string) => `If you modify this, you probably need to have a ${pack === 'railpack' ? 'railpack.json' : 'nixpacks.toml'}`;
  const compose = $derived(buildPack === 'dockercompose');
  const dirty = $derived(JSON.stringify(form) !== JSON.stringify(saved));
  const err = (k: string) => error?.field(k);

  const domains = $derived(
    str(record.fqdn)
      .split(',')
      .map((d) => d.trim())
      .filter((d) => d !== ''),
  );
  const primaryDomain = $derived(domains[0] ?? null);
  const additionalDomains = $derived(Math.max(0, domains.length - 1));
  const domainsUrl = $derived(`${base}/domains`);
  const domainLabel = $derived(domains.length > 0 ? 'Manage application domains' : 'Add an application domain');
  const exposedPorts = $derived(
    str(record.ports_exposes)
      .split(',')
      .map((p) => p.trim())
      .filter((p) => p !== '')
      .join(', '),
  );
  const networkAliases = $derived(
    str(record.custom_network_aliases)
      .split(',')
      .map((a) => a.trim())
      .filter((a) => a !== '')
      .join(', '),
  );
  const portsExposesDomainHint = $derived(
    `You can also set a different internal port for each domain on the <a class='underline dark:text-white' href='${domainsUrl}'>Domains</a> page.`,
  );

  $effect(() => {
    // `wire:init="loadCurrentInternalHostname"`
    appUi
      .internalHostname(record.uuid)
      .then((r) => (internalHostname = r.hostname))
      .catch(() => {})
      .finally(() => (internalHostnameLoaded = true));
  });

  function unavailable(what: string) {
    notify.info('Not available in coolify-rs yet', `${what} is not supported by coolify-rs yet.`);
  }

  const orNull = (v: string) => (v.trim() === '' ? null : v);
  // An untouched empty field stays null, like a Livewire property the page never changed.
  const keep = (original: unknown, v: string) => (original == null && v === '' ? null : v);

  /** The build fields `syncData(toModel: true)` writes. */
  function buildBody(): Record<string, unknown> {
    return {
      build_pack: buildPack,
      static_image: form.staticImage,
      base_directory: form.baseDirectory,
      publish_directory: keep(record.publish_directory, form.publishDirectory),
      install_command: keep(record.install_command, form.installCommand),
      build_command: keep(record.build_command, form.buildCommand),
      start_command: keep(record.start_command, form.startCommand),
      dockerfile_location: keep(record.dockerfile_location, form.dockerfileLocation),
      dockerfile_target_build: keep(record.dockerfile_target_build, form.dockerfileTargetBuild),
      watch_paths: keep(record.watch_paths, form.watchPaths),
      custom_nginx_configuration: keep(record.custom_nginx_configuration, form.customNginxConfiguration),
      dockerfile: keep(record.dockerfile, form.dockerfile),
    };
  }

  /** After a save that ran `syncData()`: the form follows the stored record. */
  async function reloadForm() {
    await onsaved();
    const fresh = await appUi.state(record.uuid);
    const next = fromRecord(record, fresh);
    form = next;
    saved = JSON.parse(JSON.stringify(next));
    buildPack = record.build_pack ?? '';
    isStatic = setting('is_static');
    siteType = setting('is_static') ? (setting('is_spa') ? 'spa' : 'static') : 'dynamic';
  }

  function showToasts(list: string[] | undefined) {
    for (const t of list ?? []) notify.success(t);
  }

  /** `updatedBuildPack()`: saves the whole form with the new build pack and its side effects. */
  async function updatedBuildPack() {
    if (!canUpdate) return;
    saving = true;
    error = null;
    try {
      const r = await appUi.update(record.uuid, {
        name: form.name,
        description: orNull(form.description),
        ports_exposes: form.portsExposes,
        ports_mappings: orNull(form.portsMappings),
        custom_network_aliases: orNull(form.customNetworkAliases),
        docker_registry_image_name: orNull(form.imageName),
        docker_registry_image_tag: orNull(form.imageTag),
        ...buildBody(),
      });
      showToasts(r.toasts);
      notify.success(r.message);
      await reloadForm();
    } catch (e) {
      error = e instanceof ApiError ? e : new ApiError(0, String(e));
      buildPack = record.build_pack ?? '';
      if (error.status === 501) notify.info('Not available in coolify-rs yet', error.message);
      else notify.failure(error.message);
    } finally {
      saving = false;
    }
  }

  /** `setSiteType()` → `instantSave()`. */
  async function setSiteType() {
    const values = { is_static: siteType !== 'dynamic', is_spa: siteType === 'spa' };
    try {
      const r = await appUi.settings(record.uuid, values);
      showToasts(r.toasts);
      notify.success('Settings saved.');
      // refresh() + syncData(): the form shows the stored values.
      await reloadForm();
    } catch (e) {
      siteType = setting('is_static') ? (setting('is_spa') ? 'spa' : 'static') : 'dynamic';
      notify.failure(e instanceof ApiError ? e.message : String(e));
    }
  }

  /** `generateNginxConfiguration($type)`: saves the form with the default configuration. */
  async function generateNginx(): Promise<string | void> {
    const r = await appUi.update(record.uuid, {
      name: form.name,
      description: orNull(form.description),
      ...buildBody(),
      generate_nginx: setting('is_spa') ? 'spa' : 'static',
    });
    showToasts(r.toasts);
    generatingNginx = false;
    await reloadForm();
  }

  async function submit() {
    if (!canUpdate) return;
    saving = true;
    error = null;
    const body: Record<string, unknown> = {
      name: form.name,
      description: orNull(form.description),
      ports_exposes: form.portsExposes,
      ports_mappings: orNull(form.portsMappings),
      custom_network_aliases: orNull(form.customNetworkAliases),
      custom_docker_run_options: orNull(form.customDockerRunOptions),
      pre_deployment_command: orNull(form.preDeploymentCommand),
      post_deployment_command: orNull(form.postDeploymentCommand),
    };
    body.docker_registry_image_name = orNull(form.imageName);
    body.docker_registry_image_tag = orNull(form.imageTag);
    if (storedPack !== 'dockerimage') Object.assign(body, buildBody());
    try {
      if (!labelsManaged && form.customLabels !== saved.customLabels) {
        await appUi.settings(record.uuid, { custom_labels: form.customLabels });
      }
      const r = await appUi.update(record.uuid, body);
      showToasts(r.toasts);
      notify.success(r.message);
      networking = false;
      await onsaved();
      const fresh = await appUi.state(record.uuid);
      form.portsExposes = form.portsExposes.replaceAll(' ', '').trim();
      form.portsMappings = form.portsMappings.replaceAll(' ', '').trim();
      form.customLabels = str(fresh.custom_labels);
      // `submit()` normalises the paths (`rtrim('/')`) and `syncData()` shows the stored values.
      const stored = fromRecord(record, fresh);
      for (const k of ['baseDirectory', 'publishDirectory', 'installCommand', 'buildCommand', 'startCommand', 'dockerfileLocation', 'watchPaths', 'customNginxConfiguration'] as const)
        form[k] = stored[k];
      saved = JSON.parse(JSON.stringify(form));
    } catch (e) {
      const failure = e instanceof ApiError ? e : new ApiError(0, String(e));
      // submit()'s catch: refresh() + syncData() put the stored values back, handleError() toasts.
      form = JSON.parse(JSON.stringify(saved));
      if (failure.status === 501) notify.info('Not available in coolify-rs yet', failure.message);
      else notify.failure(failure.message);
    } finally {
      saving = false;
    }
  }

  async function instantSave(values: Record<string, unknown>) {
    try {
      await appUi.settings(record.uuid, values);
      notify.success('Settings saved.');
      const fresh = await appUi.state(record.uuid);
      form.customLabels = str(fresh.custom_labels);
      saved.customLabels = form.customLabels;
      await onsaved();
    } catch (e) {
      notify.failure(e instanceof ApiError ? e.message : String(e));
    }
  }

  async function resetLabels(): Promise<string | void> {
    await appUi.settings(record.uuid, { reset_labels: true });
    const fresh = await appUi.state(record.uuid);
    form.customLabels = str(fresh.custom_labels);
    saved.customLabels = form.customLabels;
    resetting = false;
    await onsaved();
  }
</script>

<div>
  <form
    class="application-settings-form flex flex-col"
    onsubmit={(e) => {
      e.preventDefault();
      submit();
    }}
  >
    <UnsavedBar {dirty} {saving} onsave={submit} onreset={() => (form = JSON.parse(JSON.stringify(saved)))} />
    <div class="application-settings-grid flex flex-col gap-6">
      <SettingsSection
        id="application-details-section"
        title="Application details"
        description="Name the application and choose the build strategy Coolify should use to deploy it."
      >
        <div class="grid gap-4">
          <FormInput label="Name" id="name" required disabled={!canUpdate} bind:value={form.name} error={err('name')} />
          <FormInput label="Description" id="description" disabled={!canUpdate} bind:value={form.description} error={err('description')} />
        </div>
      </SettingsSection>

      <SettingsSection
        id="access-section"
        title="Access"
        description="Manage how this application is reached publicly, who can access it, and how it is reached from the Docker network."
      >
        <section id="public-access-section" class={compose ? '' : 'border-b border-neutral-200 pb-5 dark:border-white/[0.07]'}>
          <h3 class="mb-3 text-sm font-semibold text-black dark:text-fg">Public access</h3>
          <div
            class="group relative flex items-center gap-3 rounded-lg border border-neutral-200 bg-neutral-50/60 px-4 py-3 transition-colors hover:bg-neutral-100 focus-within:ring-2 focus-within:ring-coollabs/40 dark:border-white/[0.07] dark:bg-white/[0.05] dark:hover:bg-white/[0.08] dark:focus-within:ring-brand-light/40"
          >
            <a
              class="flex min-w-0 flex-1 items-center gap-3 after:absolute after:inset-0 after:content-[''] focus-visible:outline-none"
              aria-label={domainLabel}
              href={domainsUrl}
              use:link
            >
              <div class="flex size-9 shrink-0 items-center justify-center rounded-md bg-neutral-200/70 text-neutral-600 dark:bg-white/[0.07] dark:text-fg-dim">
                <Reicon name="globe" class="size-4" />
              </div>
              <div class="min-w-0">
                <p class="text-sm font-medium text-black dark:text-fg">
                  {#if primaryDomain}
                    <span class="block truncate">{primaryDomain}</span>
                  {:else}
                    No public domain configured
                  {/if}
                </p>
                <p class="text-xs text-neutral-500 dark:text-fg-dim">
                  {#if additionalDomains > 0}
                    +{additionalDomains} more {additionalDomains === 1 ? 'domain' : 'domains'}
                  {:else if domains.length === 0}
                    Make this application available from a URL
                  {:else}
                    Manage DNS checks and redirect settings
                  {/if}
                </p>
              </div>
            </a>
            <a class="button relative z-10 ml-auto shrink-0" aria-label={domainLabel} href={domainsUrl} use:link>
              {domains.length > 0 ? 'Manage domains' : 'Add domain'}
              <Reicon name="arrow-right" class="size-4" />
            </a>
          </div>
        </section>

        {#if !compose}
          <DatabaseSubsection id="security-section" title="Authentication" first class="border-b border-neutral-200 py-5 dark:border-white/[0.07]">
            {#if !labelsManaged}
              <Empty
                size="sm"
                title="Authentication is managed through labels"
                description="Authentication is managed via manual proxy labels. Switch label management back to Coolify to configure it here."
                iconName="admin"
              >
                {#snippet contents()}
                  <button
                    type="button"
                    class="button"
                    onclick={() => document.getElementById('container-labels-section')?.scrollIntoView({ behavior: 'smooth', block: 'start' })}
                  >
                    Go to Container labels
                  </button>
                {/snippet}
              </Empty>
            {:else}
              <Listbox
                id="isHttpBasicAuthEnabled"
                label="Method"
                helper="HTTP Basic Authentication adds the required authentication labels to the proxy. Coolify currently supports a single username and password."
                disabled={!canUpdate}
                bind:value={basicAuth}
                onchange={() => {
                  basicAuth = !!record.is_http_basic_auth_enabled;
                  unavailable('HTTP Basic Authentication');
                }}
                options={[
                  { value: false, label: 'None' },
                  { value: true, label: 'HTTP Basic Authentication' },
                ]}
              />
            {/if}
          </DatabaseSubsection>

          <section id="internal-access-section" class="pt-5">
            <h3 class="mb-4 text-sm font-semibold text-black dark:text-fg">Internal access</h3>
            <div class="grid gap-4 sm:grid-cols-2 lg:grid-cols-4">
              {#if internalHostname}
                <CopyInput label="Internal hostname" text={internalHostname} />
              {:else}
                <div class="w-full">
                  <!-- svelte-ignore a11y_label_has_associated_control -->
                  <label class="mb-1 flex items-center gap-1 text-sm font-medium text-black dark:text-white">Internal hostname</label>
                  <input
                    type="text"
                    value={internalHostnameLoaded ? 'No deployed container found' : 'Loading…'}
                    class="input input-with-copy-button bg-white dark:bg-coolgray-100 dark:read-only:bg-coolgray-100 dark:read-only:text-white"
                    readonly
                    aria-live="polite"
                  />
                </div>
              {/if}
              <CopyInput label="Docker network" text={record.destination?.network ?? ''} />
              <CopyInput label="Exposed ports" text={exposedPorts || 'None'} />
              <CopyInput label="Network aliases" text={networkAliases || 'None'} />
            </div>
            <div
              class="mt-4 flex flex-col gap-3 border-t border-neutral-200 pt-4 sm:flex-row sm:items-center sm:justify-between dark:border-white/[0.07]"
            >
              <p class="text-sm text-neutral-500 dark:text-fg-dim">Internal hostnames are only reachable by resources connected to this Docker network.</p>
              <button type="button" class="button shrink-0" onclick={() => (networking = true)}>Edit networking</button>
            </div>
          </section>
        {/if}
      </SettingsSection>

      <SettingsSection
        id="build-pipeline-section"
        title="Build & deploy"
        description="How Coolify builds the application, where it pushes the image, and the commands it runs around each deployment."
      >
        {#if !hasDockerfile && storedPack !== 'dockerimage'}
          <div class="application-build-pack-options mb-5 border-b border-neutral-200 pb-5 dark:border-white/[0.07]">
            <div class="grid gap-4 sm:grid-cols-2">
              <Listbox
                id="buildPack"
                label="Build strategy"
                disabled={!canUpdate || saving}
                bind:value={buildPack}
                onchange={updatedBuildPack}
                options={[
                  { value: 'railpack', label: 'Railpack' },
                  { value: 'nixpacks', label: 'Nixpacks (deprecated)' },
                  { value: 'static', label: 'Static' },
                  { value: 'dockerfile', label: 'Dockerfile' },
                  { value: 'dockercompose', label: 'Compose' },
                ]}
              />
              {#if isStatic || buildPack === 'static'}
                <Listbox
                  id="staticImage"
                  label="Web server"
                  required
                  disabled={!canUpdate}
                  bind:value={form.staticImage}
                  options={[
                    { value: 'nginx:alpine', label: 'nginx:alpine' },
                    { value: 'apache:alpine', label: 'apache:alpine', disabled: true },
                  ]}
                />
              {/if}
            </div>
          </div>
        {/if}
        {#if canSetBuildCommands || (isStatic && buildPack !== 'static')}
          <div class="mb-5 w-full border-b border-neutral-200 pb-5 dark:border-white/[0.07]">
            <div class="grid gap-4 sm:grid-cols-2">
              <Listbox
                id="siteType"
                label="Site type"
                helper="Static: the final build assets are served as a static site. SPA: a static site with single-page-app routing."
                disabled={!canUpdate}
                bind:value={siteType}
                onchange={setSiteType}
                options={[
                  { value: 'dynamic', label: 'Dynamic' },
                  { value: 'static', label: 'Static' },
                  { value: 'spa', label: 'SPA (single-page application)' },
                ]}
              />
            </div>
          </div>
        {/if}
        <div class="flex flex-col gap-5">
          {#if storedPack === 'dockerimage'}
            <p class="text-sm text-neutral-500 dark:text-fg-dim">Nothing to build. This application deploys a prebuilt Docker image.</p>
          {:else}
            <div class="flex flex-col gap-5">
              {#if buildPack === 'dockercompose'}
                <p class="text-sm text-neutral-500 dark:text-fg-dim">The Docker Compose settings are not available in coolify-rs yet.</p>
              {:else}
                <RepositoryPaths
                  class="lg:grid-cols-2"
                  bind:base={form.baseDirectory}
                  bind:file={() => form.dockerfileLocation, (v) => (form.dockerfileLocation = v ?? '')}
                  showFile={buildPack === 'dockerfile' && !hasDockerfile}
                  fileLabel="Dockerfile location"
                  defaultFile="/Dockerfile"
                  disabled={!canUpdate}
                  baseError={err('base_directory')}
                  fileError={err('dockerfile_location')}
                >
                  {#if canSetBuildCommands}
                    {#if setting('is_static')}
                      <FormInput
                        placeholder="/dist"
                        id="publishDirectory"
                        label="Publish directory"
                        required
                        disabled={!canUpdate}
                        bind:value={form.publishDirectory}
                        error={err('publish_directory')}
                      />
                    {:else}
                      <FormInput
                        placeholder="/"
                        id="publishDirectory"
                        label="Publish directory"
                        disabled={!canUpdate}
                        bind:value={form.publishDirectory}
                        error={err('publish_directory')}
                      />
                    {/if}
                  {/if}
                </RepositoryPaths>
                {#if privateGit}
                  <div class="pb-4">
                    <FormTextarea
                      helper="Order-based pattern matching to filter Git webhook deployments. Enter one pattern per line. Commas do not separate patterns. Supports wildcards (*, **, ?) and negation (!). Last matching pattern wins."
                      placeholder="src/pages/**"
                      id="watchPaths"
                      label="Watch paths"
                      disabled={!canUpdate}
                      bind:value={form.watchPaths}
                    />
                  </div>
                {/if}
                {#if canSetBuildCommands && (buildPack === 'nixpacks' || buildPack === 'railpack')}
                  <div class="grid gap-4 lg:grid-cols-3">
                    <FormInput
                      helper={NIXPACKS_HELPER(buildPack)}
                      id="installCommand"
                      label="Install command"
                      disabled={!canUpdate}
                      bind:value={form.installCommand}
                      error={err('install_command')}
                    />
                    <FormInput
                      helper={NIXPACKS_HELPER(buildPack)}
                      id="buildCommand"
                      label="Build command"
                      disabled={!canUpdate}
                      bind:value={form.buildCommand}
                      error={err('build_command')}
                    />
                    <FormInput
                      helper={NIXPACKS_HELPER(buildPack)}
                      id="startCommand"
                      label="Start command"
                      disabled={!canUpdate}
                      bind:value={form.startCommand}
                      error={err('start_command')}
                    />
                  </div>
                {/if}
                <div class="grid gap-4 pt-2 sm:grid-cols-2">
                  {#if buildPack === 'dockerfile'}
                    <FormInput
                      id="dockerfileTargetBuild"
                      label="Docker build stage target"
                      helper="Useful if you have multi-staged dockerfile."
                      disabled={!canUpdate}
                      bind:value={form.dockerfileTargetBuild}
                      error={err('dockerfile_target_build')}
                    />
                  {/if}
                  <Listbox
                    id="isBuildServerEnabled"
                    label="Builder selection"
                    helper="Build your application on a dedicated build server. If several build servers are connected, Coolify picks an available one automatically. {buildServerFallback} More info in the <a href='https://coolify.io/docs/knowledge-base/server/build-server' class='underline' target='_blank'>documentation</a>."
                    disabled={!canUpdate}
                    bind:value={buildServer}
                    onchange={async () => {
                      try {
                        await appUi.settings(record.uuid, { is_build_server_enabled: buildServer });
                        notify.success('Settings saved.');
                        await onsaved();
                      } catch (e) {
                        buildServer = setting('is_build_server_enabled');
                        if (e instanceof ApiError && e.status === 501) unavailable('Building on a build server');
                        else notify.failure(e instanceof ApiError ? e.message : String(e));
                      }
                    }}
                    options={[
                      { value: false, label: 'Deployment server' },
                      appState.has_build_servers
                        ? { value: true, label: 'Available build server (auto-select)' }
                        : { value: true, label: 'No build servers connected', disabled: true },
                    ]}
                  />
                </div>
              {/if}
            </div>
          {/if}
        </div>
        {#if isStatic || buildPack === 'static'}
          <div class="mt-5 border-t border-neutral-200 pt-5 dark:border-white/[0.07]">
            <div class="mb-1.5 flex items-center justify-between gap-3">
              <!-- svelte-ignore a11y_label_has_associated_control -->
              <label class="flex w-fit items-center gap-1.5" style="margin-bottom: 0">
                Custom Nginx configuration
                <Helper helper="You can add custom Nginx configuration here." />
              </label>
              {#if canUpdate}
                <button type="button" class="button" onclick={() => (generatingNginx = true)}>Generate default</button>
              {/if}
            </div>
            <MonacoEditor language="nginx" rows={10} readonly={!canUpdate} bind:value={form.customNginxConfiguration} />
          </div>
        {/if}
        {#if hasDockerfile}
          <div class="mt-6">
            <div class="w-full">
              <!-- svelte-ignore a11y_label_has_associated_control -->
              <label class="flex items-center gap-1 mb-1 text-sm font-medium">Dockerfile</label>
              <MonacoEditor language="dockerfile" rows={6} readonly={!canUpdate} bind:value={form.dockerfile} />
            </div>
          </div>
        {/if}
        {#if !compose}
          <DatabaseSubsection id="container-image-section" title={storedPack === 'dockerimage' ? 'Image' : 'Image registry'}>
            {#if appState.server_is_swarm && storedPack !== 'dockerimage'}
              <div>
                Docker Swarm requires the image to be available in a registry. More info <a
                  class="underline"
                  href="https://coolify.io/docs/knowledge-base/docker/registry"
                  target="_blank">here</a
                >.
              </div>
            {/if}
            <div class="grid gap-4 lg:grid-cols-2">
              {#if storedPack === 'dockerimage'}
                <FormInput
                  label="Image"
                  id="dockerRegistryImageName"
                  placeholder="nginx"
                  required={!!appState.server_is_swarm}
                  disabled={!canUpdate}
                  bind:value={form.imageName}
                  error={err('docker_registry_image_name')}
                />
                <FormInput
                  label="Tag"
                  id="dockerRegistryImageTag"
                  placeholder="alpine"
                  helper="Enter a tag (e.g., 'latest', 'v1.2.3') or SHA256 hash (e.g., 'sha256-59e02939b1bf39f16c93138a28727aec520bb916da021180ae502c61626b3cf0')"
                  disabled={!canUpdate}
                  bind:value={form.imageTag}
                  error={err('docker_registry_image_tag')}
                />
              {:else if registryRequired}
                <FormInput
                  id="dockerRegistryImageName"
                  required
                  label="Image"
                  placeholder="ghcr.io/your-org/your-app"
                  disabled={!canUpdate}
                  bind:value={form.imageName}
                  error={err('docker_registry_image_name')}
                />
                <FormInput
                  id="dockerRegistryImageTag"
                  helper="If set, it will tag the built image with this tag too. <br><br>Example: If you set it to 'latest', it will push the image with the commit sha tag + with the latest tag."
                  placeholder="latest"
                  label="Tag"
                  disabled={!canUpdate}
                  bind:value={form.imageTag}
                  error={err('docker_registry_image_tag')}
                />
              {:else}
                <FormInput
                  id="dockerRegistryImageName"
                  helper="Empty means it won't push the image to a docker registry. Pre-tag the image with your registry url if you want to push it to a private registry (default: Dockerhub). <br><br>Example: ghcr.io/myimage"
                  placeholder="ghcr.io/your-org/your-app"
                  label="Image"
                  disabled={!canUpdate}
                  bind:value={form.imageName}
                  error={err('docker_registry_image_name')}
                />
                <FormInput
                  id="dockerRegistryImageTag"
                  placeholder="latest"
                  helper="If set, it will tag the built image with this tag too. <br><br>Example: If you set it to 'latest', it will push the image with the commit sha tag + with the latest tag."
                  label="Tag"
                  disabled={!canUpdate}
                  bind:value={form.imageTag}
                  error={err('docker_registry_image_tag')}
                />
              {/if}
            </div>
          </DatabaseSubsection>
        {/if}
        <DatabaseSubsection id="deployment-lifecycle-section" title="Deployment commands">
          <div class="grid gap-4 sm:grid-cols-2">
            <div class="flex flex-col gap-4">
              <FormInput
                label="Pre-deployment"
                id="preDeploymentCommand"
                placeholder="php artisan migrate"
                helper="An optional script or command to execute in the existing container before the deployment begins.<br>It is always executed with 'sh -c', so you do not need add it manually."
                disabled={!canUpdate}
                bind:value={form.preDeploymentCommand}
                error={err('pre_deployment_command')}
              />
            </div>
            <div class="flex flex-col gap-4">
              <FormInput
                label="Post-deployment"
                id="postDeploymentCommand"
                placeholder="php artisan migrate"
                helper="An optional script or command to execute in the newly built container after the deployment completes.<br>It is always executed with 'sh -c', so you do not need add it manually."
                disabled={!canUpdate}
                bind:value={form.postDeploymentCommand}
                error={err('post_deployment_command')}
              />
            </div>
          </div>
        </DatabaseSubsection>
      </SettingsSection>

      {#if !compose}
        <SettingsSection id="container-section" title="Container" description="Options and labels applied to the container when it starts.">
          <FormInput
            label="Custom Docker options"
            id="customDockerRunOptions"
            placeholder="--cap-add SYS_ADMIN --device=/dev/fuse --security-opt apparmor:unconfined --ulimit nofile=1024:1024 --tmpfs /run:rw,noexec,nosuid,size=65536k --hostname=myapp"
            helper="You can add custom docker run options that will be used when your container is started.<br>Note: Not all options are supported, as they could mess up Coolify's automation and could cause bad experience for users.<br><br>Check the <a class='underline dark:text-white' href='https://coolify.io/docs/knowledge-base/docker/custom-commands'>docs.</a>"
            disabled={!canUpdate}
            bind:value={form.customDockerRunOptions}
            error={err('custom_docker_run_options')}
          />
          <DatabaseSubsection id="container-labels-section" title="Labels">
            <div class="grid w-full gap-4 sm:grid-cols-2">
              <Listbox
                id="isContainerLabelReadonlyEnabled"
                label="Label management"
                helper="When Coolify manages the labels, they are regenerated automatically and manual edits can be lost.<br><br>If you edit them yourself, be careful: a wrong label set can break the proxy configuration after a restart (you can always reset to the Coolify defaults)."
                disabled={!canUpdate}
                bind:value={labelsManaged}
                onchange={() => instantSave({ is_container_label_readonly_enabled: labelsManaged })}
                options={[
                  { value: true, label: 'Managed by Coolify (auto-generated)' },
                  { value: false, label: 'Managed manually (edit labels yourself)' },
                ]}
              />
              <Listbox
                id="isContainerLabelEscapeEnabled"
                label="Special characters"
                helper="By default, $ (and other special characters) are escaped: writing $ saves it as $$.<br><br>Keep them unescaped if you want to use environment variables inside the labels."
                disabled={!canUpdate}
                bind:value={labelsEscaped}
                onchange={() => instantSave({ is_container_label_escape_enabled: labelsEscaped })}
                options={[
                  { value: true, label: 'Escape special characters ($ becomes $$)' },
                  { value: false, label: 'Keep unescaped (allow env variables)' },
                ]}
              />
            </div>
            <div class="mt-5 border-t border-neutral-200 pt-5 dark:border-white/[0.07]">
              <div class="mb-1.5 flex items-center justify-between gap-3">
                <!-- svelte-ignore a11y_label_has_associated_control -->
                <label class="flex w-fit items-center gap-1.5" style="margin-bottom: 0">Active labels</label>
                {#if canUpdate}
                  <button type="button" class="button" onclick={() => (resetting = true)}>Reset to defaults</button>
                {/if}
              </div>
              <MonacoEditor language="ini" rows={15} readonly={labelsManaged || !canUpdate} bind:value={form.customLabels} />
            </div>
          </DatabaseSubsection>
        </SettingsSection>
      {/if}
    </div>
  </form>
</div>

{#if networking}
  <Modal title="Networking" subtitle="Ports the container exposes, host port mappings and internal network aliases." onclose={() => (networking = false)}>
    <form
      class="flex flex-col"
      onsubmit={(e) => {
        e.preventDefault();
        submit();
      }}
    >
      {#if (form.portsExposes.trim() === '' || form.portsExposes === '0') && domains.length > 0}
        <Callout type="info" title="No ports exposed" class="mb-4">
          This application does not expose any ports and will not be reachable through the proxy or your domains. This behavior is normal for background
          workers, bots, or scheduled tasks. If your application needs to handle HTTP traffic, please specify the port(s) it listens on.
        </Callout>
      {/if}
      <div class="grid gap-4 lg:grid-cols-[14rem_16rem_minmax(0,1fr)]">
        <div class="min-w-0">
          {#if isStatic || buildPack === 'static'}
            <FormInput
              label="Ports exposes"
              id="portsExposes"
              readonly
              helper={portsExposesDomainHint}
              disabled={!canUpdate}
              bind:value={form.portsExposes}
              error={err('ports_exposes')}
            />
          {:else if !labelsManaged}
            <FormInput
              label="Ports exposes"
              id="portsExposes"
              placeholder="3000,3001"
              readonly
              helper={'Readonly labels are disabled. You can set the ports manually in the labels section.<br><br>' + portsExposesDomainHint}
              disabled={!canUpdate}
              bind:value={form.portsExposes}
              error={err('ports_exposes')}
            />
          {:else}
            <FormInput
              label="Ports exposes"
              id="portsExposes"
              placeholder="3000,3001"
              helper={'A comma separated list of ports your application uses. The first port will be used as default healthcheck port if nothing defined in the Healthcheck menu. Be sure to set this correctly.<br><br>' +
                portsExposesDomainHint}
              disabled={!canUpdate}
              bind:value={form.portsExposes}
              error={err('ports_exposes')}
            />
          {/if}
          <p class="mt-1.5 text-xs text-neutral-500 dark:text-fg-dim">
            You can also set an internal port per domain on
            <a class="underline dark:text-white" href={domainsUrl} use:link>Domains</a>.
          </p>
        </div>
        <FormInput
          label="Port mappings"
          id="portsMappings"
          placeholder="3000:3000"
          helper="A comma separated list of ports you would like to map to the host system. Useful when you do not want to use domains.<br><br><span class='inline-block font-bold dark:text-brand-light'>Format:</span> host:container<br><br><span class='inline-block font-bold dark:text-brand-light'>Example:</span> 3000:3000,3002:3002<br><br>Rolling update is not supported if you have a port mapped to the host."
          disabled={!canUpdate}
          bind:value={form.portsMappings}
          error={err('ports_mappings')}
        />
        <FormInput
          label="Network aliases"
          id="customNetworkAliases"
          helper="A comma separated list of custom network aliases you would like to add for container in Docker network.<br><br><span class='inline-block font-bold dark:text-brand-light'>Example:</span><br>api.internal,api.local"
          disabled={!canUpdate}
          bind:value={form.customNetworkAliases}
          error={err('custom_network_aliases')}
        />
      </div>
      <div class="mt-5 flex justify-end gap-2 border-t border-neutral-200 pt-4 dark:border-white/[0.07]">
        <button class="button" type="button" onclick={() => (networking = false)}>Close</button>
        <button class="button button-highlighted" type="submit" disabled={!canUpdate || saving}>Save</button>
      </div>
    </form>
  </Modal>
{/if}

{#if generatingNginx}
  <ConfirmModal
    title="Confirm Nginx Configuration Generation?"
    actions={[
      'This will overwrite your current custom Nginx configuration.',
      'The default configuration will be generated based on your application type (' + (setting('is_spa') ? 'SPA' : 'static') + ').',
    ]}
    confirmWithPassword
    onconfirm={generateNginx}
    onclose={() => (generatingNginx = false)}
  />
{/if}

{#if resetting}
  <ConfirmModal
    title="Confirm Labels Reset to Coolify Defaults?"
    actions={['All your custom proxy labels will be lost.', 'Proxy labels (traefik, caddy, etc) will be reset to the coolify defaults.']}
    confirmationText={str(record.fqdn) + '/'}
    confirmationLabel="Please confirm the execution of the actions by entering the Application URL below"
    shortConfirmationLabel="Application URL"
    step2ButtonText="Permanently Reset Labels"
    onconfirm={resetLabels}
    onclose={() => (resetting = false)}
  />
{/if}
