<script lang="ts">
  // Port of the canvas page components: Dashboard\Canvas, Server\Canvas, Project\ProjectCanvas,
  // Project\ProjectsCanvas, Project\EnvironmentCanvas, Tags\Canvas and SharedVariables\Canvas with their Blade views.
  // The page is a shell; upstream's Svelte canvas renders it from /canvas/api.
  import CanvasHost from '../components/CanvasHost.svelte';
  import CanvasViewLink from '../components/CanvasViewLink.svelte';
  import ErrorPage from '../components/ErrorPage.svelte';
  import FormInput from '../components/FormInput.svelte';
  import FormTextarea from '../components/FormTextarea.svelte';
  import Listbox from '../components/Listbox.svelte';
  import Modal from '../components/Modal.svelte';
  import NewProjectModal from '../components/NewProjectModal.svelte';
  import Reicon from '../components/Reicon.svelte';
  import { api, ApiError, uiApi } from '../lib/api';
  import { setProjectCrumbs } from '../lib/crumbs';
  import { loadServerContext } from '../lib/serverPage.svelte';
  import { rememberView } from '../lib/canvasView';
  import { link, router } from '../lib/router.svelte';
  import { get } from '../lib/serverApi';
  import { notify } from '../lib/toast.svelte';
  import { sharedVariablesUi } from '../lib/sharedVariablesApi';
  import { tagsUi } from '../lib/tagsApi';

  type Kind =
    | 'dashboard'
    | 'servers'
    | 'server'
    | 'projects'
    | 'project'
    | 'environment'
    | 'tags'
    | 'team_variables'
    | 'project_variables'
    | 'environment_variables'
    | 'server_variables';
  type Named = { uuid: string; name: string };
  type Shell = { team_id: number; user_id: number; is_admin: boolean; project?: Named; environment?: Named; server?: Named };

  let { kind, uuid = '', environment = '' }: { kind: Kind; uuid?: string; environment?: string } = $props();

  let shell = $state<Shell | null>(null);
  let missing = $state(false);

  const e = encodeURIComponent;
  const remembered: Record<Kind, string> = {
    dashboard: 'dashboard',
    servers: 'servers',
    server: 'server_resources',
    projects: 'projects',
    project: 'project',
    environment: 'environment',
    tags: 'tags',
    team_variables: 'team_variables',
    project_variables: 'project_variables',
    environment_variables: 'environment_variables',
    server_variables: 'server_variables',
  };
  const scopes: Partial<Record<Kind, string>> = {
    team_variables: 'Team',
    project_variables: 'Project',
    environment_variables: 'Environment',
    server_variables: 'Server',
  };

  const query = $derived.by(() => {
    if (kind === 'server' || kind === 'server_variables') return `?server=${e(uuid)}`;
    if (kind === 'project' || kind === 'project_variables') return `?project=${e(uuid)}`;
    if (kind === 'environment' || kind === 'environment_variables') return `?project=${e(uuid)}&environment=${e(environment)}`;
    return '';
  });

  $effect(() => {
    get<Shell>(`/canvas/shell${query}`)
      .then((s) => {
        shell = s;
        rememberView(remembered[kind], 'canvas');
        document.title = pageTitle(s);
        if (kind === 'server') loadServerContext(uuid, 'resources');
        if (kind === 'project' || kind === 'project_variables') uiApi.project(uuid).then((d) => setProjectCrumbs(d));
        if (kind === 'environment' || kind === 'environment_variables') uiApi.project(uuid).then((d) => setProjectCrumbs(d, environment));
      })
      .catch(() => (missing = true));
  });

  function pageTitle(s: Shell): string {
    const scope = scopes[kind];
    if (scope) return `${scope} Variables > Canvas | coolify-rs`;
    switch (kind) {
      case 'dashboard':
        return 'Dashboard > Canvas | coolify-rs';
      case 'servers':
        return 'Servers > Canvas | coolify-rs';
      case 'server':
        return `${s.server?.name} > Canvas | coolify-rs`;
      case 'projects':
        return 'Projects > Canvas | coolify-rs';
      case 'project':
        return `${s.project?.name} > Canvas | coolify-rs`;
      case 'environment':
        return `${s.environment?.name} > Canvas | coolify-rs`;
      default:
        return 'Tags > Canvas | coolify-rs';
    }
  }

  const attrs = $derived.by((): Record<string, string> => {
    const base: Record<string, string> = {
      'data-environment-canvas': '',
      'data-team': String(shell?.team_id ?? ''),
      'data-user': String(shell?.user_id ?? ''),
    };
    const viewOnly = 'Only team admins can move cards and edit links.';
    switch (kind) {
      case 'dashboard':
        return { ...base, 'data-endpoint': '/dashboard/canvas', 'data-poll': '5' };
      case 'servers':
        return { ...base, 'data-endpoint': '/servers/canvas' };
      case 'server':
        return { ...base, 'data-endpoint': `/servers/${uuid}/canvas` };
      case 'projects':
        return { ...base, 'data-endpoint': '/projects/canvas', 'data-empty': 'No resources in your projects yet', 'data-view-only': viewOnly };
      case 'project':
        return { ...base, 'data-endpoint': `/projects/${uuid}/canvas`, 'data-empty': 'No resources in this project yet', 'data-view-only': viewOnly };
      case 'environment':
        return {
          ...base,
          'data-project': uuid,
          'data-environment': environment,
          'data-new': shell?.is_admin ? `/canvas?project=${e(uuid)}&environment=${e(environment)}` : '',
        };
      case 'tags':
        return { ...base, 'data-mode': 'tags', 'data-endpoint': '/tags/canvas' };
      case 'team_variables':
        return { ...base, 'data-mode': 'variables', 'data-endpoint': '/shared-variables/team/canvas' };
      case 'project_variables':
        return { ...base, 'data-mode': 'variables', 'data-endpoint': `/shared-variables/project/${uuid}/canvas` };
      case 'environment_variables':
        return { ...base, 'data-mode': 'variables', 'data-endpoint': `/shared-variables/environment/${environment}/canvas` };
      default:
        return { ...base, 'data-mode': 'variables', 'data-endpoint': `/shared-variables/server/${uuid}/canvas` };
    }
  });

  const flexPage = $derived(['dashboard', 'servers', 'server'].includes(kind) || !!scopes[kind]);

  const listUrl = $derived.by(() => {
    switch (kind) {
      case 'dashboard':
        return '/';
      case 'servers':
        return '/servers';
      case 'server':
        return `/server/${uuid}/resources`;
      case 'projects':
        return '/projects';
      case 'project':
        return `/project/${uuid}`;
      case 'environment':
        return `/project/${uuid}/environment/${environment}`;
      case 'tags':
        return '/tags';
      case 'team_variables':
        return '/shared-variables/team';
      case 'project_variables':
        return `/shared-variables/project/${uuid}`;
      case 'environment_variables':
        return `/shared-variables/environments/project/${uuid}/environment/${environment}`;
      default:
        return `/shared-variables/server/${uuid}`;
    }
  });
  const oneListView = $derived(['dashboard', 'server'].includes(kind) || !!scopes[kind]);

  // New environment, project, tag and shared variable.
  let modal = $state<'project' | 'environment' | 'tag' | 'variable' | null>(null);
  let name = $state('');
  let error = $state<ApiError | null>(null);

  /** `Concerns\CreatesEnvironments::submit`. */
  async function createEnvironment(ev: SubmitEvent) {
    ev.preventDefault();
    try {
      const created = await api.post<{ uuid: string }>(`/projects/${uuid}/environments`, { name });
      router.go(`/project/${uuid}/environment/${created.uuid}`);
    } catch (err) {
      error = err instanceof ApiError ? err : new ApiError(0, String(err));
      if (!Object.keys(error.errors).length) notify.error(error.message);
    }
  }

  async function createTag(ev: SubmitEvent) {
    ev.preventDefault();
    try {
      const r = await tagsUi.create(name);
      router.go(`/tags/${e(r.name)}`);
    } catch (err) {
      const failure = err instanceof ApiError ? err : new ApiError(0, String(err));
      if (!Object.keys(failure.errors).length) notify.failure(failure.message);
    }
  }

  let newKey = $state('');
  let newValue = $state('');
  let newComment = $state('');
  let newMultiline = $state(false);

  /** `saveKey`: then the canvas page opens again, with the new card. */
  async function addVariable(ev: SubmitEvent) {
    ev.preventDefault();
    error = null;
    try {
      const owner = kind.replace('_variables', '') as 'team' | 'project' | 'environment' | 'server';
      await sharedVariablesUi.add(
        { kind: owner, uuid, env: environment },
        { key: newKey, value: newValue, comment: newComment === '' ? null : newComment, is_multiline: newMultiline },
      );
      window.location.reload();
    } catch (err) {
      const failure = err instanceof ApiError ? err : new ApiError(0, String(err));
      if (Object.keys(failure.errors).length) error = failure;
      else notify.failure(failure.message);
    }
  }
</script>

{#snippet actions()}
  <div data-ec-views class="flex items-center gap-2">
    <CanvasViewLink {listUrl} view={oneListView ? 'list' : 'table'} />
    {#if kind === 'environment'}
      <a
        href="/shared-variables/environments/project/{uuid}/environment/{environment}"
        use:link
        class="button whitespace-nowrap"
        title="Shared variables for this environment"
        aria-label="Shared variables for {shell?.environment?.name}"
      >
        <Reicon name="variables" class="size-3.5" />
        Shared variables
      </a>
      {#if shell?.is_admin}
        <a href="/project/{uuid}/environment/{environment}/edit" use:link class="button whitespace-nowrap" title="Environment settings" aria-label="Open settings for {shell?.environment?.name}">
          <Reicon name="settings" class="size-3.5" />
          Settings
        </a>
        <a href="/canvas?project={e(uuid)}&environment={e(environment)}" class="button button-highlighted whitespace-nowrap">
          <Reicon name="plus" class="size-3.5" />
          New resource
        </a>
      {/if}
    {:else if kind === 'project' && shell?.is_admin}
      <a href="/project/{uuid}/edit" use:link class="button whitespace-nowrap" title="Project settings" aria-label="Open settings for {shell?.project?.name}">
        <Reicon name="settings" class="size-3.5" />
        Settings
      </a>
      <button type="button" onclick={() => (modal = 'environment')} class="button button-highlighted whitespace-nowrap">
        <Reicon name="plus" class="size-3.5" />
        New environment
      </button>
    {:else if kind === 'projects' && shell?.is_admin}
      <button type="button" onclick={() => (modal = 'project')} class="button button-highlighted whitespace-nowrap">
        <Reicon name="plus" class="size-3.5" />
        New project
      </button>
    {:else if kind === 'servers' && shell?.is_admin}
      <a href="/servers/new" use:link class="button button-highlighted whitespace-nowrap">
        <Reicon name="plus" class="size-3.5" />
        New server
      </a>
    {:else if kind === 'tags' && shell?.is_admin}
      <button type="button" onclick={() => (modal = 'tag')} class="button button-highlighted whitespace-nowrap">
        <Reicon name="plus" class="size-3.5" />
        New tag
      </button>
    {:else if scopes[kind] && shell?.is_admin}
      <button type="button" onclick={() => (modal = 'variable')} class="button button-highlighted whitespace-nowrap">
        <Reicon name="plus" class="size-3.5" />
        Add variable
      </button>
    {/if}
  </div>
{/snippet}

{#if missing}
  <ErrorPage code={404} />
{:else if shell}
  <div class={flexPage ? 'flex h-full flex-col' : 'h-full'}>
    {#if kind === 'server'}
      <!-- Its menu shows in the main sidebar on xl; below xl a canvas has no room for it. -->
      <aside class="application-settings-navigation in-main-nav min-w-0"></aside>
    {/if}
    <CanvasHost {attrs} class={flexPage ? 'relative min-h-0 flex-1' : 'relative h-full'}>{@render actions()}</CanvasHost>
  </div>
{/if}

{#if modal === 'project'}
  <NewProjectModal onclose={() => (modal = null)} />
{:else if modal === 'environment'}
  <Modal title="New Environment" onclose={() => (modal = null)}>
    <form class="space-y-4" onsubmit={createEnvironment}>
      <FormInput placeholder="staging" id="name" label="Name" required bind:value={name} error={error?.field('name')} />
      <footer class="flex justify-end border-t border-neutral-200 pt-4 dark:border-white/[0.08]">
        <button type="submit" class="button button-highlighted">Create environment</button>
      </footer>
    </form>
  </Modal>
{:else if modal === 'tag'}
  <Modal title="New tag" onclose={() => (modal = null)}>
    <form onsubmit={createTag} class="flex w-full flex-col gap-4">
      <FormInput id="newTagName" label="Name" placeholder="production" required helper="Lowercase, without spaces. You can add resources to the tag later." bind:value={name} />
      <div class="flex justify-end border-t border-neutral-200 pt-4 dark:border-white/[0.08]">
        <button type="submit" class="button button-highlighted">Create tag</button>
      </div>
    </form>
  </Modal>
{:else if modal === 'variable'}
  <Modal title="New Shared Variable" onclose={() => (modal = null)}>
    <form class="flex w-full flex-col gap-4" onsubmit={addVariable}>
      <div class="grid gap-4 sm:grid-cols-2">
        <FormInput placeholder="NODE_ENV" id="key" label="Name" required bind:value={newKey} error={error?.field('key')} />
        <FormInput id="comment" label="Comment" placeholder="Optional note" helper="Add a note to document what this environment variable is used for." maxlength={256} bind:value={newComment} error={error?.field('comment')} />
      </div>
      <div>
        {#if newMultiline}
          <FormTextarea id="value" label="Value" required bind:value={newValue} />
        {:else}
          <FormInput placeholder="production" id="value" label="Value" required bind:value={newValue} />
        {/if}
      </div>
      <div class="grid gap-4 border-t border-neutral-200 pt-4 dark:border-white/[0.07] sm:grid-cols-2">
        <Listbox
          id="is_multiline"
          label="Value type"
          bind:value={newMultiline}
          options={[
            { value: false, label: 'Single line' },
            { value: true, label: 'Multiline' },
          ]}
        />
      </div>
      <div class="flex justify-end border-t border-neutral-200 pt-4 dark:border-white/[0.07]">
        <button type="submit" class="button">Add variable</button>
      </div>
    </form>
  </Modal>
{/if}
