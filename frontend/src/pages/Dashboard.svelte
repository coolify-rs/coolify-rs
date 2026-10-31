<script lang="ts">
  // Port of upstream livewire/dashboard.blade.php (projects and servers;
  // coolify-rs has no deployments or traffic analytics to show beside them).
  import Reicon from '../components/Reicon.svelte';
  import SectionHeading from '../components/SectionHeading.svelte';
  import ActiveDeployments from '../components/ActiveDeployments.svelte';
  import ViewToggle from '../components/ViewToggle.svelte';
  import Empty from '../components/Empty.svelte';
  import NewProjectModal from '../components/NewProjectModal.svelte';
  import { uiApi, type DashboardData } from '../lib/api';
  import { link } from '../lib/router.svelte';
  import { listView } from '../lib/listview.svelte';
  import { sortNatural } from '../lib/natsort';

  const limit = 8;
  let data = $state<DashboardData | null>(null);
  let creating = $state(false);
  const projectsView = listView('dashboard_projects');
  const serversView = listView('dashboard_servers');

  uiApi.dashboard().then((d) => (data = d));

  const projects = $derived(data ? sortNatural(data.projects, (p) => p.name).slice(0, limit) : []);
  const servers = $derived(data ? sortNatural(data.servers, (s) => s.name).slice(0, limit) : []);
  const navigateTo = (p: DashboardData['projects'][number]) =>
    p.environments.length === 1 ? `/project/${p.uuid}/environment/${p.environments[0].uuid}` : `/project/${p.uuid}`;
  const plural = (n: number, one: string, many: string) => (n === 1 ? one : many);

  const iconButton =
    'card-icon-button flex size-6.5 items-center justify-center rounded-md text-neutral-400 transition-colors hover:bg-neutral-100 hover:text-black';
  const titleIconButton =
    'flex size-5 items-center justify-center rounded-md text-neutral-400 transition-colors hover:bg-neutral-100 hover:text-black dark:text-fg-faint dark:hover:bg-white/[0.06] dark:hover:text-fg';
  const tableShell = 'overflow-x-auto rounded-xl border border-neutral-200 bg-white shadow-sm dark:border-white/[0.08] dark:bg-white/[0.05]';
  const tableHead =
    'border-b border-neutral-200 bg-neutral-50 px-4 py-2.5 text-[11px] font-medium text-neutral-500 dark:border-white/[0.08] dark:bg-white/[0.05] dark:text-fg-faint';
  const tableRow =
    'min-h-14 items-center border-b border-neutral-200 px-4 py-2.5 text-[12px] transition-colors last:border-b-0 hover:bg-neutral-50 dark:border-white/[0.07] dark:hover:bg-white/[0.025]';
  const card =
    'group relative flex min-h-28 min-w-0 flex-col rounded-xl border border-neutral-200 bg-white p-3 shadow-sm transition-all hover:-translate-y-px hover:border-neutral-300 hover:shadow-md dark:border-white/[0.08] dark:bg-white/[0.05] dark:hover:border-white/[0.14]';
  const glyph = 'resource-glyph flex size-8 shrink-0 items-center justify-center rounded-lg border border-neutral-200 bg-neutral-50 text-neutral-500';
  const projectCols = 'grid-cols-[minmax(0,1fr)_7rem_4rem] gap-4 @2xl:min-w-[560px] @2xl:grid-cols-[minmax(0,1.2fr)_7rem_minmax(0,1.5fr)_4rem]';
  const serverCols = 'grid-cols-[minmax(0,1fr)_9.5rem] @2xl:grid-cols-[minmax(0,1fr)_minmax(0,1.5fr)_9.5rem]';
</script>

{#snippet projectActions(p: DashboardData['projects'][number])}
  {#if p.environments[0] && data?.can_create}
    <a
      href="/project/{p.uuid}/environment/{p.environments[0].uuid}/new"
      use:link
      class={iconButton}
      title="Add resource"
      aria-label="Add resource to {p.name}"
    >
      <Reicon name="plus" class="size-3" />
    </a>
  {/if}
  {#if data?.can_create}
    <a href="/project/{p.uuid}/edit" use:link class={iconButton} title="Project settings" aria-label="Open settings for {p.name}">
      <Reicon name="settings" class="size-3" />
    </a>
  {/if}
{/snippet}

<div class="application-settings-form w-full">
  <div class="flex min-w-0 flex-col gap-8">
    <div class="grid min-w-0 gap-8 xl:items-start xl:has-[.dashboard-activity_section]:grid-cols-[minmax(0,1fr)_22rem] 2xl:has-[.dashboard-activity_section]:grid-cols-[minmax(0,1fr)_26rem]">
      <div class="dashboard-activity hidden min-w-0 has-[section]:flex flex-col gap-8 xl:order-last">
        <ActiveDeployments />
      </div>
      <div class="flex min-w-0 flex-col gap-8">
        <section class="@container mb-0! min-w-0">
          <SectionHeading title="Projects" href={data && data.projects.length > limit ? '/projects' : null}>
            {#snippet titleAction()}
              {#if data?.can_create}
                <button type="button" class={titleIconButton} title="New project" aria-label="New project" onclick={() => (creating = true)}>
                  <Reicon name="plus" class="size-3" />
                </button>
              {/if}
            {/snippet}
            {#snippet actions()}
              <ViewToggle single={!projects.length} canvasUrl="/dashboard/canvas" bind:mode={projectsView.mode} />
            {/snippet}
          </SectionHeading>

          {#if data && projects.length === 0}
            <Empty title="No projects yet" description="Create your first deployment workspace." iconName="projects" size="sm" />
          {:else if projects.length}
            {#if projectsView.mode === 'grid'}
              <div class="grid min-w-0 grid-cols-1 gap-3 @lg:grid-cols-2 @4xl:grid-cols-4">
                {#each projects as p (p.uuid)}
                  <article class={card}>
                    <a href={navigateTo(p)} use:link class="absolute inset-0 rounded-xl" aria-label="Open {p.name}"></a>
                    <div class="flex min-w-0 items-start gap-3">
                      <div class={glyph}>{#if p.icon_url}<img src={p.icon_url} alt="{p.name} icon" class="h-full w-full rounded-lg object-cover" />{:else}<Reicon name="projects" class="size-4" />{/if}</div>
                      <div class="min-w-0 flex-1 self-center">
                        <h3 class="truncate text-[13px]! leading-4! font-semibold! text-black dark:text-fg">{p.name}</h3>
                        {#if p.description}
                          <p class="mt-0.5 truncate text-[11px] text-neutral-500 dark:text-fg-faint">{p.description}</p>
                        {/if}
                      </div>
                    </div>
                    <div class="mt-auto flex items-center justify-between gap-3 border-t border-neutral-100 pt-2.5 dark:border-white/[0.06]">
                      <p class="project-card-stats relative z-10 min-w-0">
                        <b>{p.environments.length}</b>
                        {plural(p.environments.length, 'environment', 'environments')}
                        <span class="project-card-stats-dot" aria-hidden="true">&middot;</span>
                        <b>{p.resources}</b>
                        {plural(p.resources, 'resource', 'resources')}
                      </p>
                      <div class="relative z-10 flex shrink-0 items-center gap-0.5">{@render projectActions(p)}</div>
                    </div>
                  </article>
                {/each}
              </div>
            {:else}
              <div class={tableShell}>
                <div class="grid {projectCols} {tableHead}">
                  <div>Project</div>
                  <div>Contents</div>
                  <div class="hidden @2xl:block">Description</div>
                  <div></div>
                </div>
                {#each projects as p (p.uuid)}
                  <div class="relative grid {projectCols} {tableRow}">
                    <a href={navigateTo(p)} use:link class="absolute inset-0" aria-label="Open {p.name}"></a>
                    <div class="flex min-w-0 items-center gap-3">
                      <div class={glyph}>{#if p.icon_url}<img src={p.icon_url} alt="{p.name} icon" class="h-full w-full rounded-lg object-cover" />{:else}<Reicon name="projects" class="size-4" />{/if}</div>
                      <p class="truncate text-[13px] font-semibold text-black dark:text-fg">{p.name}</p>
                    </div>
                    <div class="relative z-10 flex w-fit items-center gap-3 font-medium text-neutral-600 dark:text-fg-dim">
                      <span class="inline-flex items-center gap-1" data-tooltip="Environments" aria-label="Environments">
                        <Reicon name="layers" class="size-3.5 text-neutral-400 dark:text-fg-faint" />
                        {p.environments.length}
                      </span>
                      <span class="inline-flex items-center gap-1" data-tooltip="Resources" aria-label="Resources">
                        <Reicon name="grid" class="size-3.5 text-neutral-400 dark:text-fg-faint" />
                        {p.resources}
                      </span>
                    </div>
                    <p class="hidden truncate text-neutral-500 @2xl:block dark:text-fg-dim">{p.description || '-'}</p>
                    <div class="relative z-10 flex items-center justify-end gap-0.5">{@render projectActions(p)}</div>
                  </div>
                {/each}
              </div>
            {/if}
          {/if}
        </section>

        <section class="@container mb-0! min-w-0">
          <SectionHeading title="Servers" href={data && data.servers.length > limit ? '/servers' : null}>
            {#snippet titleAction()}
              {#if data?.can_create}
                <a href="/servers/new" use:link class={titleIconButton} title="New server" aria-label="New server">
                  <Reicon name="plus" class="size-3" />
                </a>
              {/if}
            {/snippet}
            {#snippet actions()}
              {#if servers.length}<ViewToggle bind:mode={serversView.mode} />{/if}
            {/snippet}
          </SectionHeading>

          {#if data && servers.length === 0}
            {#if data.private_keys === 0}
              <Empty
                title="A private key is required"
                description="Add an SSH private key before connecting your first server."
                iconName="keys"
                size="sm"
              >
                {#snippet contents()}
                  {#if data?.can_create}
                    <a href="/security/private-key" use:link class="button button-highlighted">
                      <Reicon name="plus" class="size-3.5" />
                      Add private key
                    </a>
                  {/if}
                {/snippet}
              </Empty>
            {:else}
              <Empty title="No servers yet" description="Connect infrastructure for your deployments." iconName="servers" size="sm">
                {#snippet contents()}
                  {#if data?.can_create}
                    <a href="/servers/new" use:link class="button button-highlighted">
                      <Reicon name="plus" class="size-3.5" />
                      New server
                    </a>
                  {/if}
                {/snippet}
              </Empty>
            {/if}
          {:else if servers.length}
            {#if serversView.mode === 'grid'}
              <div class="grid min-w-0 grid-cols-1 gap-3 @lg:grid-cols-2 @4xl:grid-cols-4">
                {#each servers as s (s.uuid)}
                  <a href="/server/{s.uuid}" use:link aria-label="Open {s.name}" class={card}>
                    <div class="relative z-10 flex min-w-0 items-start gap-3">
                      <div class={glyph}><Reicon name="servers" class="size-4" /></div>
                      <div class="min-w-0 flex-1 self-center">
                        <h3 class="truncate text-[13px]! leading-4! font-semibold! text-black dark:text-fg">{s.name}</h3>
                        {#if s.description}
                          <p class="mt-0.5 truncate text-[11px] text-neutral-500 dark:text-fg-faint">{s.description}</p>
                        {/if}
                      </div>
                      {#if s.status_type !== 'success'}
                        <span
                          data-tooltip={s.status}
                          aria-label="Server status: {s.status}"
                          class="flex size-6 shrink-0 items-center justify-center rounded-md {s.status_type === 'warning'
                            ? 'text-orange-500 dark:text-warning'
                            : 'text-red-500 dark:text-red-400'}"
                        >
                          <Reicon name="alert-triangle" class="size-4" />
                        </span>
                      {/if}
                    </div>
                  </a>
                {/each}
              </div>
            {:else}
              <div class={tableShell}>
                <div class="grid {serverCols} gap-4 @2xl:min-w-[480px] {tableHead}">
                  <div>Server</div>
                  <div class="hidden @2xl:block">Description</div>
                  <div>Status</div>
                </div>
                {#each servers as s (s.uuid)}
                  <a href="/server/{s.uuid}" use:link class="grid {serverCols} gap-4 @2xl:min-w-[480px] hover:no-underline {tableRow}">
                    <div class="flex min-w-0 items-center gap-3">
                      <div class={glyph}><Reicon name="servers" class="size-4" /></div>
                      <p class="truncate text-[13px] font-semibold text-black dark:text-fg">{s.name}</p>
                    </div>
                    <p class="hidden truncate text-neutral-500 @2xl:block dark:text-fg-dim">{s.description || '-'}</p>
                    <div class="flex items-center gap-2 text-[11px] font-medium text-neutral-600 dark:text-fg-dim">
                      <span
                        class="size-2 shrink-0 rounded-full {s.status_type === 'success'
                          ? 'bg-green-500 dark:bg-green-400'
                          : s.status_type === 'warning'
                            ? 'bg-orange-500 dark:bg-warning'
                            : 'bg-red-500 dark:bg-red-400'}"
                      ></span>
                      {s.status}
                    </div>
                  </a>
                {/each}
              </div>
            {/if}
          {/if}
        </section>
      </div>
    </div>
  </div>
</div>

{#if creating}
  <NewProjectModal onclose={() => (creating = false)} />
{/if}
