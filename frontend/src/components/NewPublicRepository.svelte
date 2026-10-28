<script lang="ts">
  // Port of upstream livewire/project/new/public-git-repository.blade.php (+
  // Project\New\PublicGitRepository and the HasRepositoryDetection trait), with
  // partials/smart-scan-summary.blade.php and partials/env-import-modal.blade.php.
  // The component state lives here and is sent with each request, like the
  // Livewire component's public properties.
  import Callout from './Callout.svelte';
  import CheckpointItem from './CheckpointItem.svelte';
  import FormInput from './FormInput.svelte';
  import Listbox from './Listbox.svelte';
  import Modal from './Modal.svelte';
  import Reicon from './Reicon.svelte';
  import RepositoryPaths from './RepositoryPaths.svelte';
  import { appUi, ApiError, type RepositoryDetection } from '../lib/api';
  import { router } from '../lib/router.svelte';
  import { notify } from '../lib/toast.svelte';

  let { project, env, destination }: { project: string; env: string; destination: string } = $props();

  const REPOSITORY_HELPER =
    "<span class='text-helper'>Examples</span><br>For Public repositories, use <span class='text-helper'>https://...</span>.<br>For Private repositories, use <span class='text-helper'>git@...</span>.<br><br>https://github.com/coollabsio/coolify-examples <span class='text-helper'>main</span> branch will be selected<br>https://github.com/coollabsio/coolify-examples/tree/nodejs-fastify <span class='text-helper'>nodejs-fastify</span> branch will be selected.<br>https://gitea.com/sedlav/expressjs.git <span class='text-helper'>main</span> branch will be selected.<br>https://gitlab.com/andrasbacsai/nodejs-example.git <span class='text-helper'>main</span> branch will be selected.";
  const NIXPACKS_DEPRECATED =
    'Nixpacks is deprecated and is in maintenance mode upstream. We recommend Railpack, the successor of Nixpacks, for new and existing applications.';

  // PublicGitRepository
  let repositoryUrl = $state('');
  let port = $state('3000');
  let branchFound = $state(false);
  let isStatic = $state(false);
  let publishDirectory = $state<string | null>(null);
  let baseDirectory = $state('/');
  let dockerComposeLocation = $state<string | null>('/docker-compose.yaml');
  let gitBranch = $state('main');
  let rateLimitRemaining = $state(0);
  let rateLimitReset = $state<string | number>(0);
  let gitSource = $state<'github' | 'other'>('other');
  let gitRepository = $state('');
  let buildPack = $state('railpack');
  let showIsStatic = $state(true);

  // HasRepositoryDetection
  let detectionRan = $state(false);
  let detectionFailed = $state(false);
  let suggestedBuildPack = $state<string | null>(null);
  let detectedDockerfiles = $state<string[]>([]);
  let detectedDockerComposeFiles = $state<string[]>([]);
  let selectedDockerfile = $state<string | null>(null);
  let selectedDockerComposeFile = $state<string | null>(null);
  let detectedPort = $state<number | null>(null);
  let dockerfilePorts = $state<Record<string, number | null>>({});
  let detectedEnvFiles = $state<string[]>([]);
  let selectedEnvFile = $state<string | null>(null);
  let parsedEnvFiles = $state<Record<string, [string, string][]>>({});
  let envExampleVars = $state<[string, string][]>([]);
  let envImported = $state(false);
  let selectedDockerfileError = $state<string | undefined>(undefined);

  let checking = $state(false);
  let scanning = $state(false);
  let submitting = $state(false);
  let envModal = $state(false);

  const FILE_PATH = /^\/[a-zA-Z0-9._\-/~@+]+$/;
  const plural = (n: number, word: string) => (n === 1 ? word : `${word}s`);

  function autofocus(node: HTMLElement) {
    queueMicrotask(() => node.querySelector('input')?.focus());
  }

  /** `updatedBuildPack()`. */
  function updatedBuildPack() {
    if (buildPack === 'nixpacks' || buildPack === 'railpack') {
      showIsStatic = true;
      if (!isStatic) port = '3000';
    } else if (buildPack === 'static') {
      showIsStatic = false;
      isStatic = false;
      port = '80';
    } else {
      showIsStatic = false;
      isStatic = false;
    }
  }

  /** `instantSave()` of the output type. */
  function instantSave() {
    if (isStatic) {
      port = '80';
      publishDirectory = '/dist';
    } else {
      port = '3000';
      publishDirectory = null;
    }
    notify.success('Application settings updated!');
  }

  /** `applyDetectionResult()`. */
  function applyDetectionResult(r: RepositoryDetection | null) {
    detectedDockerfiles = r?.dockerfiles ?? [];
    detectedDockerComposeFiles = r?.docker_compose_files ?? [];
    dockerfilePorts = r?.dockerfile_ports ?? {};
    selectedDockerfile = null;
    selectedDockerComposeFile = null;
    detectedPort = null;
    detectedEnvFiles = [];
    parsedEnvFiles = {};
    selectedEnvFile = null;
    envExampleVars = [];
    envImported = false;
    suggestedBuildPack = r?.suggested_build_pack ?? null;
    if (suggestedBuildPack && suggestedBuildPack !== buildPack) {
      buildPack = suggestedBuildPack;
      updatedBuildPack();
    }
    if (detectedDockerfiles.length > 0) {
      selectedDockerfile = detectedDockerfiles[0];
      const p = dockerfilePorts[selectedDockerfile] ?? null;
      port = String(p ?? 3000);
      detectedPort = p;
    }
    if (detectedDockerComposeFiles.length > 0) {
      selectedDockerComposeFile = detectedDockerComposeFiles[0];
      dockerComposeLocation = '/' + selectedDockerComposeFile;
    }
    if (r && r.env_files.length > 0) {
      detectedEnvFiles = r.env_files.map((f) => f.file);
      const parsed: Record<string, [string, string][]> = {};
      for (const f of r.env_files) parsed[f.file] = f.variables;
      parsedEnvFiles = parsed;
      selectedEnvFile = detectedEnvFiles[0];
      envExampleVars = (parsedEnvFiles[selectedEnvFile] ?? []).map(([k, v]) => [k, v]);
    }
  }

  /** The end of `detectRepository()`. */
  function finishDetection(d: RepositoryDetection | null | undefined) {
    if (!d) return;
    detectionRan = false;
    detectionFailed = false;
    applyDetectionResult(null);
    if (d.failed) {
      detectionFailed = true;
      if (d.toast) notify.failure(d.toast);
    } else {
      applyDetectionResult(d);
    }
    detectionRan = true;
  }

  /** `loadBranch()`. */
  async function loadBranch(e: SubmitEvent) {
    e.preventDefault();
    if (checking) return;
    checking = true;
    try {
      const r = await appUi.publicCheck({
        repository_url: repositoryUrl,
        destination_uuid: destination,
        rate_limit_remaining: rateLimitRemaining,
        rate_limit_reset: rateLimitReset,
      });
      if (r.repository_url !== undefined) {
        repositoryUrl = r.repository_url;
        branchFound = !!r.branch_found;
        gitBranch = r.git_branch ?? gitBranch;
        baseDirectory = r.base_directory ?? baseDirectory;
        gitRepository = r.git_repository ?? gitRepository;
        gitSource = r.git_source ?? gitSource;
        rateLimitRemaining = r.rate_limit_remaining ?? rateLimitRemaining;
        rateLimitReset = r.rate_limit_reset ?? rateLimitReset;
        finishDetection(r.detection);
      }
      if (r.error) notify.failure(r.error);
    } catch (err) {
      notify.failure(err instanceof ApiError ? err.message : String(err));
    } finally {
      checking = false;
    }
  }

  /** `detectRepository()` from the Smart Scan button. */
  async function detectRepository() {
    if (scanning) return;
    scanning = true;
    try {
      const r = await appUi.publicScan({
        repository_url: repositoryUrl,
        git_branch: gitBranch,
        base_directory: baseDirectory,
        destination_uuid: destination,
      });
      finishDetection(r.detection);
      if (r.error) notify.failure(r.error);
    } catch (err) {
      notify.failure(err instanceof ApiError ? err.message : String(err));
    } finally {
      scanning = false;
    }
  }

  const normalizePath = (p: string | null) => {
    const v = (p ?? '').trim().replace(/^\/+/, '');
    return v === '' ? null : v;
  };

  /** `updatedSelectedDockerfile()`. */
  function updatedSelectedDockerfile() {
    selectedDockerfile = normalizePath(selectedDockerfile);
    selectedDockerfileError = undefined;
    if (selectedDockerfile !== null) {
      const location = '/' + selectedDockerfile;
      if (location.length > 255) {
        selectedDockerfileError = 'The selected dockerfile field must not be greater than 255 characters.';
        return;
      }
      if (!FILE_PATH.test(location)) {
        selectedDockerfileError =
          'The Dockerfile location must be a valid path starting with / and containing only alphanumeric characters, dots, hyphens, underscores, slashes, @, ~, and +.';
        return;
      }
    }
    const p = dockerfilePorts[selectedDockerfile ?? ''] ?? null;
    port = String(p ?? 3000);
    detectedPort = p;
  }

  /** `updatedSelectedDockerComposeFile()`. */
  function updatedSelectedDockerComposeFile() {
    selectedDockerComposeFile = normalizePath(selectedDockerComposeFile);
    if (selectedDockerComposeFile) dockerComposeLocation = '/' + selectedDockerComposeFile;
  }

  /** `updatedSelectedEnvFile()`. */
  function updatedSelectedEnvFile() {
    if (selectedEnvFile && parsedEnvFiles[selectedEnvFile]) {
      envExampleVars = parsedEnvFiles[selectedEnvFile].map(([k, v]) => [k, v]);
      envImported = false;
    }
  }

  function clearEnvVars() {
    envImported = false;
    envExampleVars = (parsedEnvFiles[selectedEnvFile ?? ''] ?? []).map(([k, v]) => [k, v]);
    envModal = false;
  }

  /** `submit()`. */
  async function submit(e: SubmitEvent) {
    e.preventDefault();
    if (submitting) return;
    submitting = true;
    try {
      const { uuid } = await appUi.createPublic({
        project_uuid: project,
        environment_uuid: env,
        destination_uuid: destination,
        repository_url: repositoryUrl,
        git_branch: gitBranch,
        port,
        is_static: isStatic,
        publish_directory: publishDirectory,
        build_pack: buildPack,
        base_directory: baseDirectory,
        docker_compose_location: dockerComposeLocation,
        selected_dockerfile: selectedDockerfile,
        env_imported: envImported,
        env_vars: envExampleVars,
      });
      router.go(`/project/${project}/environment/${env}/application/${uuid}`);
    } catch (err) {
      notify.failure(err instanceof ApiError ? err.message : String(err));
      submitting = false;
    }
  }

  // smart-scan-summary.blade.php
  const buildPackLabels: Record<string, string> = { dockercompose: 'Docker Compose', dockerfile: 'Dockerfile' };
  const suggestedFile = $derived(
    suggestedBuildPack === 'dockercompose'
      ? (detectedDockerComposeFiles[0] ?? null)
      : suggestedBuildPack === 'dockerfile'
        ? (detectedDockerfiles[0] ?? null)
        : null,
  );
  const nestedFiles = $derived(
    [
      detectedDockerfiles.length ? `${detectedDockerfiles.length} ${plural(detectedDockerfiles.length, 'Dockerfile')}` : null,
      detectedDockerComposeFiles.length
        ? `${detectedDockerComposeFiles.length} ${plural(detectedDockerComposeFiles.length, 'Compose file')}`
        : null,
    ]
      .filter(Boolean)
      .join(' and '),
  );
  const showPort = $derived(
    detectionRan && buildPack === 'dockerfile' && selectedDockerfile !== null && detectedDockerfiles.includes(selectedDockerfile),
  );
  const isStaticOptions: { value: boolean; label: string }[] = [
    { value: false, label: 'Web application' },
    { value: true, label: 'Static site' },
  ];
</script>

<div class="mt-8 flex w-full max-w-none flex-col gap-6 lg:mt-3">
  <form onsubmit={loadBranch}>
    <section class="application-settings-section">
      <div class="application-settings-section-header">
        <div>
          <h2>Public Git repository</h2>
          <p>Connect a public repository over HTTPS and inspect its default branch.</p>
        </div>
      </div>
      <div class="application-settings-section-body">
        <div class="flex flex-col gap-2 sm:flex-row sm:items-end">
          <div class="min-w-0 flex-1" use:autofocus>
            <FormInput
              required
              id="repository_url"
              label="Repository URL"
              helper={REPOSITORY_HELPER}
              placeholder="https://github.com/owner/repository"
              bind:value={repositoryUrl}
            />
          </div>
          <button type="submit" class="button w-full justify-center sm:w-auto" disabled={checking}>
            {#if checking}
              <svg class="size-3.5 shrink-0 animate-spin" viewBox="0 0 24 24" fill="none" aria-hidden="true">
                <circle class="opacity-25" cx="12" cy="12" r="9" stroke="currentColor" stroke-width="3" />
                <path class="opacity-75" d="M21 12a9 9 0 0 0-9-9" stroke="currentColor" stroke-width="3" stroke-linecap="round" />
              </svg>
            {/if}
            Check repository
          </button>
        </div>
        <p class="mt-2 text-xs text-neutral-500 dark:text-fg-dim">
          Need a sample? Browse
          <a class="font-medium text-coollabs hover:underline dark:text-brand-light" href="https://github.com/coollabsio/coolify-examples/" target="_blank">
            Coolify Examples{' '}
          </a>.
        </p>
      </div>
    </section>
  </form>

  {#if branchFound}
    <form onsubmit={submit}>
      <section class="application-settings-section">
        <div class="application-settings-section-header">
          <div>
            <h2>Build configuration</h2>
            <p>Choose how Coolify builds and runs this repository.</p>
          </div>
          <button type="submit" class="button button-highlighted" disabled={submitting}>Continue</button>
        </div>
        <div class="application-settings-section-body space-y-5">
          {#if rateLimitRemaining && rateLimitReset}
            <Callout type="info" title="Git provider rate limit">
              {rateLimitRemaining} requests remain. The limit resets at
              {rateLimitReset} UTC.
            </Callout>
          {/if}

          <div class="overflow-hidden rounded-[10px] border border-neutral-200 dark:border-white/[0.08]">
            <div class="flex min-h-10 items-center justify-between gap-3 border-b border-neutral-200 py-1.5 pr-1.5 pl-4 dark:border-white/[0.08]">
              <p class="text-[10px] font-semibold uppercase tracking-[0.08em] text-neutral-400 dark:text-fg-faint">Smart Scan</p>
              <button type="button" class="button" disabled={scanning} onclick={detectRepository}>
                <Reicon name="search" class="size-3.5" />
                {detectionRan ? 'Scan again' : 'Scan repository'}
              </button>
            </div>

            {#if scanning}
              <div>
                <CheckpointItem status="running" title="Scanning the repository" description="Looking for Dockerfiles, Compose files, and env files." />
              </div>
            {:else}
              <div class="divide-y divide-neutral-200 dark:divide-white/[0.07]">
                {#if !detectionRan}
                  <CheckpointItem icon="search" title="Not scanned yet" description="Scan the repository to find Dockerfiles, Compose files, and env files." />
                {:else if detectionFailed}
                  <CheckpointItem
                    status="error"
                    title="Could not read the repository"
                    description="Check the repository access and the branch, or select the build pack manually."
                  />
                {:else if suggestedBuildPack}
                  <CheckpointItem
                    status="success"
                    title="{buildPackLabels[suggestedBuildPack]} {buildPack === suggestedBuildPack ? 'selected' : 'suggested'}"
                    description="Found {suggestedFile} in the base directory."
                  />
                {:else if nestedFiles !== ''}
                  <CheckpointItem icon="brain" title="Found {nestedFiles} in subdirectories" />
                {:else}
                  <CheckpointItem icon="brain" title="No Dockerfile or Compose file found" description="Select the build pack that fits the repository." />
                {/if}

                {#if showPort}
                  {#if detectedPort}
                    <CheckpointItem status="success" title="Port {detectedPort}" description="From EXPOSE in {selectedDockerfile}." />
                  {:else}
                    <CheckpointItem
                      icon="globe"
                      title="Port {port}"
                      description="{selectedDockerfile} has no EXPOSE instruction, so the default port is used."
                    />
                  {/if}
                {/if}

                {#if detectionRan && envExampleVars.length > 0}
                  <div class="flex items-center justify-between gap-3 pr-4">
                    <CheckpointItem
                      status={envImported ? 'success' : 'idle'}
                      icon="variables"
                      title={selectedEnvFile ?? ''}
                      description="{envExampleVars.length} {plural(envExampleVars.length, 'variable')}{envImported ? ', imported when you continue' : ''}."
                    />
                    <div class="relative h-auto w-auto">
                      <div>
                        <button type="button" class="button" onclick={() => (envModal = true)}>{envImported ? 'Edit' : 'Review'}</button>
                      </div>
                    </div>
                  </div>
                {/if}
              </div>
            {/if}
          </div>

          <div class="grid gap-4 sm:grid-cols-2">
            <FormInput
              id="git_branch"
              label="Branch"
              disabled={gitSource !== 'other'}
              helper="You can choose another branch after the application is created."
              bind:value={gitBranch}
            />
            <Listbox
              id="build_pack"
              label="Build pack"
              required
              bind:value={buildPack}
              onchange={updatedBuildPack}
              options={[
                { value: 'railpack', label: 'Railpack' },
                { value: 'nixpacks', label: 'Nixpacks (deprecated)' },
                { value: 'static', label: 'Static' },
                { value: 'dockerfile', label: 'Dockerfile' },
                { value: 'dockercompose', label: 'Docker Compose' },
              ]}
            />
            {#if showIsStatic}
              <Listbox id="isStatic" label="Output type" bind:value={isStatic} onchange={instantSave} options={isStaticOptions} />
              <FormInput
                type="number"
                id="port"
                label="Port"
                readonly={isStatic || buildPack === 'static'}
                helper="Port the application listens on."
                bind:value={port}
              />
            {/if}
            {#if isStatic}
              <FormInput
                id="publish_directory"
                label="Publish directory"
                helper="Directory containing the generated static assets."
                bind:value={() => publishDirectory ?? '', (v) => (publishDirectory = v)}
              />
            {/if}
          </div>

          {#if buildPack === 'nixpacks'}
            <Callout type="warning" title="Nixpacks is deprecated">{NIXPACKS_DEPRECATED}</Callout>
          {/if}

          {#if buildPack === 'dockerfile'}
            <Listbox
              id="selectedDockerfile"
              label="Dockerfile"
              searchable
              allowCustom
              placeholder="/Dockerfile"
              emptyText="No Dockerfiles detected. Type a path in the search field."
              searchPlaceholder="Search or type a path…"
              helper="Select a detected Dockerfile, or type a path relative to the base directory."
              bind:value={() => selectedDockerfile ?? '', (v) => (selectedDockerfile = v)}
              onchange={updatedSelectedDockerfile}
              error={selectedDockerfileError}
              options={detectedDockerfiles.map((f) => ({ value: f, label: f }))}
            />
          {/if}

          {#if buildPack === 'dockercompose' && detectedDockerComposeFiles.length > 0}
            <Listbox
              id="selectedDockerComposeFile"
              label="Docker Compose File"
              searchable
              allowCustom
              searchPlaceholder="Search or type a path…"
              helper="Select a detected Docker Compose file, or type a path relative to the base directory."
              bind:value={() => selectedDockerComposeFile ?? '', (v) => (selectedDockerComposeFile = v)}
              onchange={updatedSelectedDockerComposeFile}
              options={detectedDockerComposeFiles.map((f) => ({ value: f, label: f }))}
            />
          {/if}

          {#if buildPack === 'dockercompose'}
            <RepositoryPaths
              class="sm:grid-cols-2"
              bind:base={baseDirectory}
              bind:file={dockerComposeLocation}
              showFile
              fileLabel="Compose file"
              defaultFile="/docker-compose.yaml"
            />
          {:else}
            <FormInput id="base_directory" label="Base directory" helper="Repository directory used as the build root." bind:value={baseDirectory} />
          {/if}
        </div>
      </section>
    </form>
  {/if}
</div>

{#if envModal}
  <Modal title="Import environment variables" subtitle="The variables are added to the application when you continue." onclose={() => (envModal = false)}>
    <div class="flex flex-col gap-4">
      {#if detectedEnvFiles.length > 1}
        <Listbox
          id="selectedEnvFile"
          label="Env file"
          bind:value={() => selectedEnvFile ?? '', (v) => (selectedEnvFile = v)}
          onchange={updatedSelectedEnvFile}
          options={detectedEnvFiles.map((f) => ({ value: f, label: f }))}
        />
      {/if}
      <div class="flex flex-col gap-2">
        {#each envExampleVars as pair, i (`${selectedEnvFile}-${pair[0]}`)}
          <div class="grid grid-cols-1 items-center gap-1 sm:grid-cols-[minmax(0,1fr)_minmax(0,2fr)] sm:gap-3">
            <span class="truncate font-mono text-[13px] text-neutral-600 dark:text-fg-dim" title={pair[0]}>{pair[0]}</span>
            <FormInput id="envExampleVars.{pair[0]}" bind:value={envExampleVars[i][1]} />
          </div>
        {/each}
      </div>
    </div>
    {#snippet footer()}
      {#if envImported}
        <button type="button" class="button" onclick={clearEnvVars}>Remove import</button>
      {/if}
      <button
        type="button"
        class="button button-highlighted"
        onclick={() => {
          envImported = true;
          envModal = false;
        }}
      >
        {envImported ? 'Update' : 'Import'}
        {envExampleVars.length}
        {plural(envExampleVars.length, 'variable')}
      </button>
    {/snippet}
  </Modal>
{/if}
