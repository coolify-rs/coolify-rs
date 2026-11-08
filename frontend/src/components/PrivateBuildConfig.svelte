<script lang="ts">
  // The "Build configuration" form the private repository forms share, with
  // partials/smart-scan-summary.blade.php, partials/env-import-modal.blade.php and the
  // HasRepositoryDetection trait. The parent gives the repository and sends `values()` on submit.
  import type { Snippet } from 'svelte';
  import Callout from './Callout.svelte';
  import CheckpointItem from './CheckpointItem.svelte';
  import FormInput from './FormInput.svelte';
  import Listbox from './Listbox.svelte';
  import Modal from './Modal.svelte';
  import Reicon from './Reicon.svelte';
  import RepositoryPaths from './RepositoryPaths.svelte';
  import { ApiError, type RepositoryDetection } from '../lib/api';
  import { notify } from '../lib/toast.svelte';

  let {
    title = 'Build configuration',
    description = 'Choose the branch and build strategy for this application.',
    submitting = false,
    branch = $bindable(''),
    branches = null,
    baseDirectory = $bindable('/'),
    freeBranch = false,
    onsubmit,
    onscan,
    branchLabel = 'Branch',
    deployKey = false,
    smart = true,
    head,
  }: {
    title?: string;
    description?: string;
    submitting?: boolean;
    branch?: string;
    /** The Branch listbox; `null` shows a text input. */
    branches?: string[] | null;
    baseDirectory?: string;
    freeBranch?: boolean;
    onsubmit: () => void;
    /** `detectRepository()`. */
    onscan: () => Promise<{ detection: unknown; error?: string }>;
    branchLabel?: string;
    /** The field order and required marks of github-private-repository-deploy-key.blade.php. */
    deployKey?: boolean;
    /** GitLab and Gitea forms have no Smart Scan. */
    smart?: boolean;
    head?: Snippet;
  } = $props();

  const NIXPACKS_DEPRECATED =
    'Nixpacks is deprecated and is in maintenance mode upstream. We recommend Railpack, the successor of Nixpacks, for new and existing applications.';

  let port = $state('3000');
  let isStatic = $state(false);
  let publishDirectory = $state<string | null>(null);
  let dockerComposeLocation = $state<string | null>('/docker-compose.yaml');
  let buildPack = $state('railpack');
  let showIsStatic = $state(true);

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
  let scanning = $state(false);
  let envModal = $state(false);

  const FILE_PATH = /^\/[a-zA-Z0-9._\-/~@+]+$/;
  const plural = (n: number, word: string) => (n === 1 ? word : `${word}s`);

  /** The Livewire state `submit()` reads. */
  export function values() {
    return {
      git_branch: branch,
      port,
      is_static: isStatic,
      publish_directory: publishDirectory,
      build_pack: buildPack,
      base_directory: baseDirectory,
      docker_compose_location: dockerComposeLocation,
      selected_dockerfile: selectedDockerfile,
      env_imported: envImported,
      env_vars: envExampleVars,
    };
  }

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

  /** `detectRepository()`. */
  export async function detect() {
    if (scanning) return;
    scanning = true;
    try {
      const r = await onscan();
      const d = r.detection as RepositoryDetection | null;
      if (d) {
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

  function updatedSelectedDockerComposeFile() {
    selectedDockerComposeFile = normalizePath(selectedDockerComposeFile);
    if (selectedDockerComposeFile) dockerComposeLocation = '/' + selectedDockerComposeFile;
  }

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
      detectedDockerComposeFiles.length ? `${detectedDockerComposeFiles.length} ${plural(detectedDockerComposeFiles.length, 'Compose file')}` : null,
    ]
      .filter(Boolean)
      .join(' and '),
  );
  const showPort = $derived(detectionRan && buildPack === 'dockerfile' && selectedDockerfile !== null && detectedDockerfiles.includes(selectedDockerfile));
  const isStaticOptions: { value: boolean; label: string }[] = [
    { value: false, label: 'Web application' },
    { value: true, label: 'Static site' },
  ];
</script>

{#snippet smartScan()}
      <div class="overflow-hidden rounded-[10px] border border-neutral-200 dark:border-white/[0.08]">
        <div class="flex min-h-10 items-center justify-between gap-3 border-b border-neutral-200 py-1.5 pr-1.5 pl-4 dark:border-white/[0.08]">
          <p class="text-[10px] font-semibold uppercase tracking-[0.08em] text-neutral-400 dark:text-fg-faint">Smart Scan</p>
          <button type="button" class="button" disabled={scanning} onclick={detect}>
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
              <CheckpointItem status="error" title="Could not read the repository" description="Check the repository access and the branch, or select the build pack manually." />
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
                <CheckpointItem icon="globe" title="Port {port}" description="{selectedDockerfile} has no EXPOSE instruction, so the default port is used." />
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
{/snippet}

{#snippet dockerfilePickers()}
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

{/snippet}

{#snippet paths()}
  
      {#if buildPack === 'dockercompose'}
        <RepositoryPaths class="sm:grid-cols-2" bind:base={baseDirectory} bind:file={dockerComposeLocation} showFile fileLabel="Compose file" defaultFile="/docker-compose.yaml" />
      {:else}
        <FormInput id="base_directory" label="Base directory" placeholder="/" helper="Repository directory used as the build root." bind:value={baseDirectory} />
      {/if}

{/snippet}

<form
  onsubmit={(e) => {
    e.preventDefault();
    onsubmit();
  }}
>
  <section class="application-settings-section">
    <div class="application-settings-section-header">
      <div>
        <h2>{title}</h2>
        <p>{description}</p>
      </div>
      <button type="submit" class="button button-highlighted" disabled={submitting}>Continue</button>
    </div>
    <div class="application-settings-section-body space-y-5">
      {@render head?.()}
      {#if !deployKey && smart}
        {@render smartScan()}
      {/if}

      <div class="grid gap-4 sm:grid-cols-2">
        {#if branches}
          <Listbox
            searchable
            id="selected_branch_name"
            label={branchLabel}
            required
            searchPlaceholder="Search branches…"
            bind:value={branch}
            options={branches.map((b) => ({ value: b, label: b }))}
          />
        {:else}
          <FormInput id="branch" label={branchLabel} required={freeBranch} bind:value={branch} />
        {/if}
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
          <Listbox id="is_static" label="Output type" bind:value={isStatic} onchange={instantSave} options={isStaticOptions} />
          <FormInput type="number" required={deployKey} id="port" label="Port" helper={deployKey ? undefined : 'Port the application listens on.'} readonly={isStatic || buildPack === 'static'} bind:value={port} />
        {/if}
        {#if isStatic}
          <FormInput id="publish_directory" required={deployKey} label="Publish directory" helper={deployKey ? undefined : 'Directory containing the generated static assets.'} bind:value={() => publishDirectory ?? '', (v) => (publishDirectory = v)} />
        {/if}
      </div>

      {#if buildPack === 'nixpacks'}
        <Callout type="warning" title="Nixpacks is deprecated">{NIXPACKS_DEPRECATED}</Callout>
      {/if}

      {#if deployKey}
        {@render paths()}
        {@render smartScan()}
        {@render dockerfilePickers()}
      {:else}
        {#if smart}
          {@render dockerfilePickers()}
        {/if}
        {@render paths()}
      {/if}

    </div>
  </section>
</form>

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
