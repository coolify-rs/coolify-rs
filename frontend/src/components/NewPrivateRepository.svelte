<script lang="ts">
  // Port of upstream livewire/project/new/{github,gitlab,gitea}-private-repository.blade.php
  // (+ Project\New\{Github,Gitlab,Gitea}PrivateRepository).
  import Empty from './Empty.svelte';
  import GitIcon from './GitIcon.svelte';
  import Listbox from './Listbox.svelte';
  import Modal from './Modal.svelte';
  import PrivateBuildConfig from './PrivateBuildConfig.svelte';
  import Reicon from './Reicon.svelte';
  import SourceCreateForm from './SourceCreateForm.svelte';
  import { ApiError } from '../lib/api';
  import { router } from '../lib/router.svelte';
  import { notify } from '../lib/toast.svelte';
  import { privateRepoUi, type Provider } from '../lib/sourcesApi';

  let { provider, project, env, destination }: { provider: Provider; project: string; env: string; destination: string } = $props();

  type Row = { id: number; name: string; html_url: string };
  type Repo = Record<string, any>;

  const name_ = $derived({ github: 'GitHub', gitlab: 'GitLab', gitea: 'Gitea' }[provider]);
  let apps = $state<Row[] | null>(null);
  let meta = $state({ can_create: false, can_make_system_wide: false, random_name: '' });
  let step = $state<'apps' | 'repository'>('apps');
  let current = $state<Row | null>(null);
  let repositories = $state<Repo[]>([]);
  let installationPath = $state<string | null>(null);
  let selectedId = $state('');
  let branches = $state<string[]>([]);
  let branch = $state('main');
  let baseDirectory = $state('/');
  let loadingApp = $state<number | null>(null);
  let loadingBranches = $state(false);
  let submitting = $state(false);
  let creating = $state(false);
  let config: PrivateBuildConfig | undefined = $state();

  $effect(() => {
    privateRepoUi
      .sources(provider)
      .then((r) => {
        const full = r as unknown as { sources: Row[]; can_create: boolean; can_make_system_wide: boolean; random_name: string };
        apps = full.sources;
        meta = { can_create: full.can_create, can_make_system_wide: full.can_make_system_wide, random_name: full.random_name };
      })
      .catch((e) => notify.failure(e instanceof ApiError ? e.message : String(e)));
  });

  /** `selected_repository_owner/repo` (GitHub), `path_with_namespace` (GitLab), `full_name` (Gitea). */
  function repositoryPath(r: Repo | undefined): string {
    if (!r) return '';
    if (provider === 'github') return `${r.owner?.login ?? ''}/${r.name ?? ''}`;
    return provider === 'gitlab' ? r.path_with_namespace : r.full_name;
  }
  const label = (r: Repo) => (provider === 'gitlab' ? r.path_with_namespace : (r.full_name ?? r.name));
  const selected = $derived(repositories.find((r) => String(r.id) === selectedId));

  /** `loadRepositories($id)`. */
  async function loadRepositories(app: Row) {
    loadingApp = app.id;
    try {
      const r = (await privateRepoUi.repositories(provider, app.id, 1)) as unknown as { repositories: Repo[]; installation_path: string | null };
      current = app;
      repositories = r.repositories;
      installationPath = r.installation_path;
      branches = [];
      if (r.repositories.length > 0) selectedId = String(r.repositories[0].id);
      step = 'repository';
    } catch (e) {
      notify.failure(e instanceof ApiError ? e.message : String(e));
    } finally {
      loadingApp = null;
    }
  }

  /** `loadBranches()`. */
  async function loadBranches() {
    if (!current || !selected || loadingBranches) return;
    loadingBranches = true;
    try {
      const r = await privateRepoUi.branches(provider, {
        source_id: current.id,
        repository: repositoryPath(selected),
        project_id: selected.id,
      });
      branches = r.branches.map((b) => b.name);
      const preferred = selected.default_branch ?? 'main';
      branch = branches.includes(preferred) ? preferred : (branches[0] ?? 'main');
      if (provider === 'github') queueMicrotask(() => config?.detect());
    } catch (e) {
      notify.failure(e instanceof ApiError ? e.message : String(e));
    } finally {
      loadingBranches = false;
    }
  }

  function scan() {
    return privateRepoUi.scan(provider, {
      source_id: current?.id,
      repository: repositoryPath(selected),
      git_branch: branch,
      base_directory: baseDirectory,
      destination_uuid: destination,
    });
  }

  /** `submit()`. */
  async function submit() {
    if (submitting || !config) return;
    submitting = true;
    try {
      const { uuid } = await privateRepoUi.create(provider, {
        ...config.values(),
        project_uuid: project,
        environment_uuid: env,
        destination_uuid: destination,
        source_id: current?.id,
        repository: repositoryPath(selected),
        repository_project_id: selected?.id,
      });
      router.go(`/project/${project}/environment/${env}/application/${uuid}`);
    } catch (e) {
      notify.failure(e instanceof ApiError ? e.message : String(e));
      submitting = false;
    }
  }
</script>

{#snippet addApp()}
  <div class="relative w-auto h-auto">
    <button type="button" class="button" onclick={() => (creating = true)}>+ Add {name_} App</button>
  </div>
{/snippet}

{#if apps}
  <div class="mt-8 flex w-full max-w-none flex-col gap-6 lg:mt-3">
    {#if apps.length === 0}
      <section class="application-settings-section">
        <div class="application-settings-section-header">
          <div>
            <h2>{name_} App</h2>
            <p>Connect a {name_} App before selecting a private repository.</p>
          </div>
        </div>
        {#if provider === 'github'}
          <Empty title="No GitHub Apps" description="Create an app to grant coolify-rs access to selected repositories." iconName="sources">
            {#snippet contents()}
              {@render addApp()}
            {/snippet}
          </Empty>
        {:else}
          <div class="application-settings-section-body">
            <Empty title="No {name_} Apps" description="Create an app to grant coolify-rs access to selected repositories." iconName="sources">
              {#snippet contents()}
                {@render addApp()}
              {/snippet}
            </Empty>
          </div>
        {/if}
      </section>
    {:else if step === 'apps'}
      <section class="application-settings-section">
        <div class="application-settings-section-header">
          <div>
            <h2>Choose {name_} App</h2>
            <p>Select the installation that can access the repository you want to deploy.</p>
          </div>
          {@render addApp()}
        </div>
        <div class="application-settings-section-body p-0!">
          {#each apps as app (app.id)}
            <button
              type="button"
              class="group relative flex w-full items-center gap-3 border-b border-neutral-200 px-4 py-3 text-left transition-colors last:border-b-0 hover:bg-neutral-50 dark:border-white/[0.06] dark:hover:bg-white/[0.025]"
              disabled={loadingApp === app.id}
              onclick={() => loadRepositories(app)}
            >
              <div class="flex size-9 shrink-0 items-center justify-center rounded-lg bg-neutral-100 text-neutral-500 dark:bg-white/[0.06] dark:text-fg-dim">
                {#if provider === 'github'}
                  <Reicon name="sources" class="size-4" />
                {:else}
                  <GitIcon class="size-4" git={provider} />
                {/if}
              </div>
              <div class="min-w-0 flex-1">
                <div class="truncate text-sm font-semibold text-black dark:text-fg">{app.name}</div>
                <p class="mt-0.5 truncate text-xs text-neutral-500 dark:text-fg-dim">{app.html_url}</p>
              </div>
            </button>
          {/each}
        </div>
      </section>
    {:else}
      <section class="application-settings-section">
        <div class="application-settings-section-header">
          <div>
            <h2>Choose repository</h2>
            <p>
              {provider === 'github' ? `Search repositories available through ${current?.name}.` : `Search repositories available through the selected ${name_} App.`}
            </p>
          </div>
          {#if provider === 'github'}
            <div class="flex items-center gap-2">
              <button type="button" class="button" onclick={() => current && loadRepositories(current)}>Refresh</button>
              <a target="_blank" class="button" href={installationPath}>
                GitHub access
                <Reicon name="arrow-right" class="size-3.5 -rotate-45" />
              </a>
            </div>
          {:else if current}
            <button type="button" class="button" onclick={() => current && loadRepositories(current)}>Refresh</button>
          {/if}
        </div>
        <div class="application-settings-section-body">
          {#if repositories.length > 0}
            <div class="flex items-end gap-2">
              <div class="min-w-0 flex-1">
                <Listbox
                  searchable
                  id="selected_repository_id"
                  label="Repository"
                  required={provider === 'github'}
                  placeholder={provider === 'github' ? undefined : 'Select a repository'}
                  searchPlaceholder="Search repositories…"
                  bind:value={selectedId}
                  onchange={loadBranches}
                  options={repositories.map((r) => ({ value: String(r.id), label: label(r) }))}
                />
              </div>
              <button type="button" class="button" disabled={loadingBranches} onclick={loadBranches}>Load repository</button>
            </div>
          {:else}
            <Empty size="sm" title="No repositories available" description={provider === 'github' ? 'Review this GitHub App installation and grant access to a repository.' : `Review this ${name_} App configuration and grant access to a repository.`} />
          {/if}
        </div>
      </section>

      {#if branches.length > 0}
        <PrivateBuildConfig bind:this={config} smart={provider === 'github'} {submitting} bind:branch bind:baseDirectory {branches} onsubmit={submit} onscan={scan} />
      {/if}
    {/if}
  </div>
{/if}

{#if creating}
  <Modal title="New {name_} App" onclose={() => (creating = false)}>
    <SourceCreateForm {provider} randomName={meta.random_name} canCreate={meta.can_create} canMakeSystemWide={meta.can_make_system_wide} />
  </Modal>
{/if}
