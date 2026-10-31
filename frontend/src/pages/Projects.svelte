<script lang="ts">
  // Port of upstream livewire/project/index.blade.php.
  import Reicon from '../components/Reicon.svelte';
  import ViewToggle from '../components/ViewToggle.svelte';
  import Empty from '../components/Empty.svelte';
  import Dropdown from '../components/Dropdown.svelte';
  import Pager from '../components/Pager.svelte';
  import NewProjectModal from '../components/NewProjectModal.svelte';
  import { uiApi, type DashboardData } from '../lib/api';
  import { link } from '../lib/router.svelte';
  import { listView } from '../lib/listview.svelte';
  import { monthDayYear } from '../lib/format';

  type P = DashboardData['projects'][number];

  let data = $state<DashboardData | null>(null);
  let creating = $state(false);
  let search = $state('');
  let sortBy = $state('name-asc');
  let page = $state(1);
  let pageSize = $state(12);
  const view = listView('projects', 'grid');

  uiApi.dashboard().then((d) => (data = d));

  const sortOptions = [
    { value: 'name-asc', label: 'Name A–Z' },
    { value: 'name-desc', label: 'Name Z–A' },
    { value: 'resources', label: 'Most resources' },
    { value: 'environments', label: 'Most environments' },
  ];

  const filtered = $derived.by(() => {
    const query = search.trim().toLowerCase();
    const list = (data?.projects ?? []).filter((p) => {
      const searchable = [p.name, p.description].filter(Boolean).join(' ').toLowerCase();
      return !query || searchable.includes(query);
    });
    return list.sort((a, b) => {
      if (sortBy === 'name-desc') return b.name.localeCompare(a.name);
      if (sortBy === 'resources') return b.resources - a.resources || a.name.localeCompare(b.name);
      if (sortBy === 'environments') return b.environments.length - a.environments.length || a.name.localeCompare(b.name);
      return a.name.localeCompare(b.name);
    });
  });
  const totalPages = $derived(Math.max(1, Math.ceil(filtered.length / pageSize)));
  $effect.pre(() => {
    if (page > totalPages) page = totalPages;
  });
  const paginated = $derived(filtered.slice((page - 1) * pageSize, page * pageSize));

  const href = (p: P) => (p.environments.length === 1 ? `/project/${p.uuid}/environment/${p.environments[0].uuid}` : `/project/${p.uuid}`);
  const addResourceHref = (p: P) => (p.environments[0] && data?.can_create ? `/project/${p.uuid}/environment/${p.environments[0].uuid}/new` : null);
  const settingsHref = (p: P) => (data?.can_create ? `/project/${p.uuid}/edit` : null);
  const plural = (n: number, one: string, many: string) => (n === 1 ? one : many);
  const glyph = 'resource-glyph flex size-8 shrink-0 items-center justify-center rounded-lg border border-neutral-200 bg-neutral-50 text-neutral-500';
</script>

<div class="application-settings-form w-full">
  <div class="w-full">
    <header class="mb-5 flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
      <h1 class="min-w-0 truncate text-[24px]! leading-7! font-semibold! tracking-tight!">Projects</h1>
      {#if data?.can_create}
        <div class="w-fit shrink-0">
          <button type="button" class="button button-highlighted" onclick={() => (creating = true)}>
            <Reicon name="plus" class="size-3.5" />
            New project
          </button>
        </div>
      {/if}
    </header>

    {#if data && data.projects.length === 0}
      <Empty title="No projects yet" description="Create a project to organize your environments and resources." iconName="projects" />
    {:else if data}
      <div class="mb-3 flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
        <div class="relative w-full sm:max-w-sm">
          <Reicon
            name="search"
            class="pointer-events-none absolute top-1/2 left-2.5 z-10 size-3.5 -translate-y-1/2 text-neutral-400 dark:text-fg-faint"
          />
          <input
            bind:value={search}
            oninput={() => (page = 1)}
            type="search"
            placeholder="Search projects"
            class="h-8! w-full rounded-lg! border-neutral-200! bg-white! py-0! pr-8! pl-8! text-[12px]! shadow-none! placeholder:text-neutral-400 focus:border-accent! focus:ring-0! dark:border-white/[0.08]! dark:bg-white/[0.035]! dark:text-fg! dark:placeholder:text-fg-faint"
          />
          {#if search}
            <button
              onclick={() => {
                search = '';
                page = 1;
              }}
              type="button"
              class="absolute top-1/2 right-2 flex size-5 -translate-y-1/2 items-center justify-center rounded text-neutral-400 transition-colors hover:bg-neutral-100 hover:text-black dark:text-fg-faint dark:hover:bg-white/[0.07] dark:hover:text-fg"
              aria-label="Clear search"
            >
              <span class="text-sm leading-none">×</span>
            </button>
          {/if}
        </div>

        <div class="flex flex-wrap items-center gap-2">
          <Dropdown panelClass="w-52!">
            {#snippet trigger({ open })}
              <button type="button" class="button" aria-haspopup="listbox" aria-expanded={open}>
                <svg class="size-3.5 opacity-65" viewBox="0 0 24 24" fill="none" aria-hidden="true">
                  <path
                    d="M8 5v14m0 0-3-3m3 3 3-3M16 19V5m0 0-3 3m3-3 3 3"
                    stroke="currentColor"
                    stroke-width="1.7"
                    stroke-linecap="round"
                    stroke-linejoin="round"
                  />
                </svg>
                Sort
              </button>
            {/snippet}
            {#snippet children({ close })}
              {#each sortOptions as option (option.value)}
                <button
                  type="button"
                  class="flex h-9 w-full items-center rounded-md px-2 text-left text-[12px] text-neutral-600 transition-colors hover:bg-neutral-100 hover:text-black dark:text-fg-dim dark:hover:bg-white/[0.06] dark:hover:text-fg"
                  onclick={() => {
                    sortBy = option.value;
                    close();
                    page = 1;
                  }}
                >
                  <span class="flex-1">{option.label}</span>
                  {#if sortBy === option.value}
                    <svg class="size-3.5 text-black dark:text-fg" viewBox="0 0 12 12" fill="none" aria-hidden="true">
                      <path d="m2.5 6.25 2.1 2.1 4.9-5" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round" />
                    </svg>
                  {/if}
                </button>
              {/each}
            {/snippet}
          </Dropdown>

          <ViewToggle canvasUrl="/projects/canvas" bind:mode={() => view.mode, (m) => ((view.mode = m), (page = 1))} />
        </div>
      </div>

      {#if view.mode === 'grid'}
        <div>
          <div class="grid grid-cols-1 gap-3 sm:grid-cols-2 lg:grid-cols-4">
            {#each paginated as p (p.uuid)}
              <article
                class="group relative flex min-h-28 flex-col rounded-xl border border-neutral-200 bg-white p-3 shadow-sm transition-all hover:-translate-y-px hover:border-neutral-300 hover:shadow-md dark:border-white/[0.08] dark:bg-white/[0.05] dark:hover:border-white/[0.14]"
              >
                <a href={href(p)} use:link class="absolute inset-0 rounded-xl" aria-label="Open {p.name}"></a>
                <div class="flex items-start gap-3">
                  <div class={glyph}><Reicon name="projects" class="size-4" /></div>
                  <div class="min-w-0 flex-1 self-center">
                    <h2 class="truncate text-[13px]! leading-4! font-semibold! text-black dark:text-fg">{p.name}</h2>
                    {#if p.description?.trim()}
                      <p class="mt-0.5 truncate text-[11px] text-neutral-500 dark:text-fg-faint">{p.description}</p>
                    {/if}
                  </div>
                </div>
                <div class="mt-auto flex items-center justify-between gap-3 border-t border-neutral-100 pt-2.5 dark:border-white/[0.06]">
                  <p class="project-card-stats relative z-10 min-w-0">
                    <b>{p.environments.length}</b>
                    <span>{plural(p.environments.length, 'environment', 'environments')}</span>
                    <span class="project-card-stats-dot" aria-hidden="true">&middot;</span>
                    <b>{p.resources}</b>
                    <span>{plural(p.resources, 'resource', 'resources')}</span>
                  </p>
                  <div class="relative z-10 flex shrink-0 items-center gap-0.5">
                    {#if addResourceHref(p)}
                      <a
                        href={addResourceHref(p)}
                        use:link
                        class="card-icon-button flex size-7.5 items-center justify-center rounded-md text-neutral-400 transition-colors hover:bg-neutral-100 hover:text-black"
                        title="Add resource"
                        aria-label="Add resource to {p.name}"
                      >
                        <Reicon name="plus" class="size-3" />
                      </a>
                    {/if}
                    {#if settingsHref(p)}
                      <a
                        href={settingsHref(p)}
                        use:link
                        class="card-icon-button flex size-7.5 items-center justify-center rounded-md text-neutral-400 transition-colors hover:bg-neutral-100 hover:text-black"
                        title="Project settings"
                        aria-label="Open settings for {p.name}"
                      >
                        <Reicon name="settings" class="size-3" />
                      </a>
                    {/if}
                  </div>
                </div>
              </article>
            {/each}
          </div>
          {#if filtered.length > 0}
            <Pager
              total={filtered.length}
              bind:page
              bind:pageSize
              storageKey="coolify.page-size.projects"
              options={[12, 24, 48, 96]}
              class="mt-3 rounded-xl border border-neutral-200 bg-white shadow-sm dark:border-white/[0.08] dark:bg-white/[0.05]"
            />
          {/if}
        </div>
      {:else}
        <div class="overflow-hidden rounded-xl border border-neutral-200 bg-white shadow-sm dark:border-white/[0.08] dark:bg-white/[0.05]">
          <div
            class="projects-table-grid border-b border-neutral-200 bg-neutral-50 px-4 py-2.5 text-[11px] font-medium text-neutral-500 dark:border-white/[0.08] dark:bg-white/[0.05] dark:text-fg-faint"
          >
            <div>Project</div>
            <div>Contents</div>
            <div>Created</div>
            <div class="project-description">Description</div>
            <div></div>
          </div>
          {#each paginated as p (p.uuid)}
            <div
              class="projects-table-grid group min-h-14 items-center border-b border-neutral-200 px-4 py-2.5 transition-colors last:border-b-0 hover:bg-neutral-50 dark:border-white/[0.07] dark:hover:bg-white/[0.025]"
            >
              <div class="flex min-w-0 items-center gap-3">
                <div class={glyph}><Reicon name="projects" class="size-4" /></div>
                <a href={href(p)} use:link class="truncate text-[13px] font-semibold text-black hover:underline dark:text-fg">{p.name}</a>
              </div>
              <div class="flex items-center gap-3 text-[12px] font-medium text-neutral-600 dark:text-fg-dim">
                <span class="inline-flex items-center gap-1" data-tooltip="Environments" aria-label="Environments">
                  <Reicon name="layers" class="size-3.5 text-neutral-400 dark:text-fg-faint" />
                  <span>{p.environments.length}</span>
                </span>
                <span class="inline-flex items-center gap-1" data-tooltip="Resources" aria-label="Resources">
                  <Reicon name="grid" class="size-3.5 text-neutral-400 dark:text-fg-faint" />
                  <span>{p.resources}</span>
                </span>
              </div>
              <div class="truncate text-[12px] text-neutral-500 dark:text-fg-dim">{monthDayYear(p.created_at)}</div>
              <p class="project-description truncate text-[12px] text-neutral-500 dark:text-fg-dim">{p.description || '-'}</p>
              <div class="flex items-center justify-end gap-0.5">
                {#if addResourceHref(p)}
                  <a
                    href={addResourceHref(p)}
                    use:link
                    class="flex size-7 items-center justify-center rounded-md text-neutral-400 transition-colors hover:bg-neutral-100 hover:text-black dark:text-fg-faint dark:hover:bg-white/[0.06] dark:hover:text-fg"
                    title="Add resource"
                    aria-label="Add resource to {p.name}"
                  >
                    <Reicon name="plus" class="size-3.5" />
                  </a>
                {/if}
                {#if settingsHref(p)}
                  <a
                    href={settingsHref(p)}
                    use:link
                    class="flex size-7 items-center justify-center rounded-md text-neutral-400 transition-colors hover:bg-neutral-100 hover:text-black dark:text-fg-faint dark:hover:bg-white/[0.06] dark:hover:text-fg"
                    title="Project settings"
                    aria-label="Open settings for {p.name}"
                  >
                    <Reicon name="settings" class="size-3.5" />
                  </a>
                {/if}
              </div>
            </div>
          {/each}
          {#if filtered.length > 0}
            <Pager total={filtered.length} bind:page bind:pageSize storageKey="coolify.page-size.projects" options={[12, 24, 48, 96]} />
          {/if}
        </div>
      {/if}

      {#if filtered.length === 0}
        <div
          class="flex min-h-52 flex-col items-center justify-center rounded-xl border border-neutral-200 bg-white px-6 text-center dark:border-white/[0.08] dark:bg-white/[0.05]"
        >
          <Reicon name="search" class="mb-3 size-6 text-neutral-300 dark:text-fg-faint" />
          <p class="text-[13px] font-medium">No matching projects</p>
          <p class="mt-1 text-[12px] text-neutral-500 dark:text-fg-dim">Try a different search.</p>
        </div>
      {/if}
    {/if}
  </div>
</div>

{#if creating}
  <NewProjectModal onclose={() => (creating = false)} />
{/if}
