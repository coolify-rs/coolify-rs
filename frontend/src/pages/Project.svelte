<script lang="ts">
  // Port of upstream livewire/project/show.blade.php (environments list).
  import Reicon from '../components/Reicon.svelte';
  import ViewToggle from '../components/ViewToggle.svelte';
  import Empty from '../components/Empty.svelte';
  import Dropdown from '../components/Dropdown.svelte';
  import Pager from '../components/Pager.svelte';
  import Modal from '../components/Modal.svelte';
  import FormInput from '../components/FormInput.svelte';
  import { api, uiApi, ApiError, type ProjectPageData } from '../lib/api';
  import { link, router } from '../lib/router.svelte';
  import { listView } from '../lib/listview.svelte';
  import { compareEnvironmentNames, setProjectCrumbs } from '../lib/crumbs';
  import { notify } from '../lib/toast.svelte';

  let { uuid }: { uuid: string } = $props();

  type E = ProjectPageData['environments'][number];
  let data = $state<ProjectPageData | null>(null);
  let missing = $state(false);
  let search = $state('');
  let sortBy = $state('default');
  let page = $state(1);
  let pageSize = $state(12);
  const view = listView('project', 'grid');

  // New environment form.
  let creating = $state(false);
  let newName = $state('');
  let busy = $state(false);
  let error = $state<ApiError | null>(null);

  $effect(() => {
    uiApi
      .project(uuid)
      .then((d) => {
        data = d;
        setProjectCrumbs(d);
        document.title = `${d.project.name.slice(0, 10)} > Environments | coolify-rs`;
      })
      .catch((e) => {
        if (e instanceof ApiError && e.status === 404) missing = true;
        else throw e;
      });
  });

  // Server order: created_at, then production, staging, development, then by name.
  const ordered = $derived(
    [...(data?.environments ?? [])]
      .sort((a, b) => (a.created_at ?? '').localeCompare(b.created_at ?? ''))
      .sort((a, b) => compareEnvironmentNames(a.name, b.name)),
  );
  const sortOptions = [
    { value: 'default', label: 'Default' },
    { value: 'name-asc', label: 'Name A–Z' },
    { value: 'name-desc', label: 'Name Z–A' },
    { value: 'resources', label: 'Most resources' },
  ];
  const filtered = $derived.by(() => {
    const query = search.trim().toLowerCase();
    const list = ordered.filter((e) => {
      const searchable = [e.name, e.description].filter(Boolean).join(' ').toLowerCase();
      return !query || searchable.includes(query);
    });
    return list.sort((a, b) => {
      if (sortBy === 'name-desc') return b.name.localeCompare(a.name);
      if (sortBy === 'resources') return b.resources - a.resources || a.name.localeCompare(b.name);
      if (sortBy === 'name-asc') return a.name.localeCompare(b.name);
      return 0;
    });
  });
  const totalPages = $derived(Math.max(1, Math.ceil(filtered.length / pageSize)));
  $effect.pre(() => {
    if (page > totalPages) page = totalPages;
  });
  const paginated = $derived(filtered.slice((page - 1) * pageSize, page * pageSize));

  const href = (e: E) => `/project/${uuid}/environment/${e.uuid}`;
  const settingsHref = (e: E) => (data?.can_update ? `/project/${uuid}/environment/${e.uuid}/edit` : null);
  const addResourceHref = (e: E) => (data?.can_create ? `/project/${uuid}/environment/${e.uuid}/new` : null);
  const iconLink =
    'flex items-center justify-center rounded-md text-neutral-400 transition-colors hover:bg-neutral-100 hover:text-black dark:text-fg-faint dark:hover:bg-white/[0.06] dark:hover:text-fg';

  /** `Concerns\CreatesEnvironments::submit`. */
  async function createEnvironment(e: SubmitEvent) {
    e.preventDefault();
    busy = true;
    error = null;
    try {
      const res = await api.post<{ uuid: string }>(`/projects/${encodeURIComponent(uuid)}/environments`, { name: newName });
      creating = false;
      router.go(`/project/${uuid}/environment/${res.uuid}`);
    } catch (err) {
      error = err instanceof ApiError ? err : new ApiError(0, String(err));
      if (!Object.keys(error.errors).length) notify.error(error.message);
    } finally {
      busy = false;
    }
  }
</script>

{#if missing}
  <div class="application-settings-form w-full">
    <Empty title="Project not found" description="It may have been deleted, or it belongs to another team." iconName="projects" size="lg" />
  </div>
{:else if data}
  <div>
    <div class="w-full">
      <header class="mb-5 flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
        <div class="min-w-0">
          <div class="flex min-w-0 items-center gap-1.5">
            <h1 class="truncate text-[24px]! leading-7! font-semibold! tracking-tight!">{data.project.name}</h1>
            {#if data.can_update}
              <a
                href="/project/{uuid}/edit"
                use:link
                class="flex size-7 shrink-0 {iconLink}"
                title="Project settings"
                aria-label="Open settings for {data.project.name}"
              >
                <Reicon name="settings" class="size-4" />
              </a>
            {/if}
          </div>
          <p class="mt-1 text-[13px] text-neutral-500 dark:text-fg-dim">
            <span>{data.environments.length} {data.environments.length === 1 ? 'environment' : 'environments'}</span>
            in this project
          </p>
        </div>
        {#if data.can_update}
          <div class="flex w-fit shrink-0 items-center gap-2">
            <button type="button" class="button button-highlighted" onclick={() => ((creating = true), (newName = ''), (error = null))}>
              <Reicon name="plus" class="size-3.5" />
              New environment
            </button>
          </div>
        {/if}
      </header>

      {#if data.environments.length === 0}
        <Empty title="No environments yet" description="Add an environment to start organizing this project's resources." iconName="layers" />
      {:else}
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
              placeholder="Search environments"
              class="h-8! w-full rounded-lg! border-neutral-200! bg-white! py-0! pr-8! pl-8! text-[12px]! shadow-none! placeholder:text-neutral-400 focus:border-accent! focus:ring-0! dark:border-white/[0.08]! dark:bg-white/[0.035]! dark:text-fg! dark:placeholder:text-fg-faint"
            />
            {#if search}
              <button
                onclick={() => ((search = ''), (page = 1))}
                type="button"
                class="absolute top-1/2 right-2 flex size-5 -translate-y-1/2 items-center justify-center rounded text-neutral-400 transition-colors hover:bg-neutral-100 hover:text-black dark:text-fg-faint dark:hover:bg-white/[0.07] dark:hover:text-fg"
                aria-label="Clear search"
              >
                <span class="text-sm leading-none">×</span>
              </button>
            {/if}
          </div>
          <div class="flex flex-wrap items-center gap-2">
            <Dropdown panelClass="w-48!">
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
                    onclick={() => ((sortBy = option.value), close(), (page = 1))}
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
            <ViewToggle canvasUrl="/project/{uuid}/canvas" bind:mode={() => view.mode, (m) => ((view.mode = m), (page = 1))} />
          </div>
        </div>

        {#if view.mode === 'grid'}
          <div>
            <div class="grid grid-cols-1 gap-3 sm:grid-cols-2 lg:grid-cols-4">
              {#each paginated as e (e.uuid)}
                <article
                  class="group relative flex min-h-28 flex-col rounded-xl border border-neutral-200 bg-white p-3 shadow-sm transition-all hover:-translate-y-px hover:border-neutral-300 hover:shadow-md dark:border-white/[0.08] dark:bg-white/[0.05] dark:hover:border-white/[0.14]"
                >
                  <a href={href(e)} use:link class="absolute inset-0 rounded-xl" aria-label="Open {e.name}"></a>
                  <div class="flex items-start gap-3">
                    <div
                      class="flex size-8 shrink-0 items-center justify-center rounded-lg border border-neutral-200 bg-neutral-50 text-neutral-500 dark:border-white/[0.08] dark:bg-white/[0.04] dark:text-fg-dim"
                    >
                      <Reicon name="layers" class="size-4" />
                    </div>
                    <div class="min-w-0 flex-1">
                      <h2 class="truncate text-[13px]! leading-4! font-semibold! text-black dark:text-fg">{e.name}</h2>
                      <p class="mt-0.5 truncate text-[11px] text-neutral-500 dark:text-fg-faint">{e.description || 'Environment'}</p>
                    </div>
                  </div>
                  <div class="mt-auto flex items-center justify-between gap-3 pt-4">
                    <p class="min-w-0 truncate text-[11px] text-neutral-500 dark:text-fg-dim">
                      {e.resources}
                      {e.resources === 1 ? 'resource' : 'resources'}
                    </p>
                    <div class="relative z-10 flex shrink-0 items-center gap-0.5">
                      {#if addResourceHref(e)}
                        <a href={addResourceHref(e)} use:link class="size-7.5 {iconLink}" title="Add resource" aria-label="Add resource to {e.name}">
                          <Reicon name="plus" class="size-3" />
                        </a>
                      {/if}
                      {#if settingsHref(e)}
                        <a
                          href={settingsHref(e)}
                          use:link
                          class="size-7.5 {iconLink}"
                          title="Environment settings"
                          aria-label="Open settings for {e.name}"
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
                storageKey="coolify.page-size.project-environments"
                options={[12, 24, 48, 96]}
                class="mt-3 rounded-xl border border-neutral-200 bg-white shadow-sm dark:border-white/[0.08] dark:bg-white/[0.05]"
              />
            {/if}
          </div>
        {:else}
          <div class="overflow-hidden rounded-xl border border-neutral-200 bg-white shadow-sm dark:border-white/[0.08] dark:bg-white/[0.05]">
            <div
              class="environments-table-grid border-b border-neutral-200 bg-neutral-50 px-4 py-2.5 text-[11px] font-medium text-neutral-500 dark:border-white/[0.08] dark:bg-white/[0.05] dark:text-fg-faint"
            >
              <div>Environment</div>
              <div class="environment-resource-count">Resources</div>
              <div class="environment-description">Description</div>
              <div></div>
            </div>
            {#each paginated as e (e.uuid)}
              <div
                class="environments-table-grid group relative min-h-14 items-center border-b border-neutral-200 px-4 py-2.5 transition-colors last:border-b-0 hover:bg-neutral-50 dark:border-white/[0.07] dark:hover:bg-white/[0.025]"
              >
                <a href={href(e)} use:link class="absolute inset-0" aria-label="Open {e.name}"></a>
                <div class="flex min-w-0 items-center gap-3">
                  <div
                    class="flex size-8 shrink-0 items-center justify-center rounded-lg border border-neutral-200 bg-neutral-50 text-neutral-500 dark:border-white/[0.08] dark:bg-white/[0.035] dark:text-fg-dim"
                  >
                    <Reicon name="layers" class="size-4" />
                  </div>
                  <a href={href(e)} use:link class="relative truncate text-[13px] font-semibold text-black hover:underline dark:text-fg">{e.name}</a>
                </div>
                <div class="environment-resource-count text-[12px] text-neutral-600 dark:text-fg-dim">{e.resources}</div>
                <p class="environment-description truncate text-[12px] text-neutral-500 dark:text-fg-dim">{e.description || '-'}</p>
                <div class="relative flex items-center justify-end gap-0.5">
                  {#if addResourceHref(e)}
                    <a href={addResourceHref(e)} use:link class="size-7 {iconLink}" title="Add resource" aria-label="Add resource to {e.name}">
                      <Reicon name="plus" class="size-3.5" />
                    </a>
                  {/if}
                  {#if settingsHref(e)}
                    <a href={settingsHref(e)} use:link class="size-7 {iconLink}" title="Environment settings" aria-label="Open settings for {e.name}">
                      <Reicon name="settings" class="size-3.5" />
                    </a>
                  {/if}
                </div>
              </div>
            {/each}
            {#if filtered.length > 0}
              <Pager
                total={filtered.length}
                bind:page
                bind:pageSize
                storageKey="coolify.page-size.project-environments"
                options={[12, 24, 48, 96]}
              />
            {/if}
          </div>
        {/if}

        {#if filtered.length === 0}
          <div
            class="flex min-h-52 flex-col items-center justify-center rounded-xl border border-neutral-200 bg-white px-6 text-center dark:border-white/[0.08] dark:bg-white/[0.05]"
          >
            <Reicon name="search" class="mb-3 size-6 text-neutral-300 dark:text-fg-faint" />
            <p class="text-[13px] font-medium">No matching environments</p>
            <p class="mt-1 text-[12px] text-neutral-500 dark:text-fg-dim">Try a different search.</p>
          </div>
        {/if}
      {/if}
    </div>
  </div>
{/if}

{#if creating}
  <Modal title="New Environment" onclose={() => (creating = false)}>
    <form class="space-y-4" onsubmit={createEnvironment}>
      <FormInput placeholder="staging" id="name" label="Name" required bind:value={newName} error={error?.field('name')} />
      <footer class="flex justify-end border-t border-neutral-200 pt-4 dark:border-white/[0.08]">
        <button type="submit" class="button button-highlighted" disabled={busy}>Create environment</button>
      </footer>
    </form>
  </Modal>
{/if}
