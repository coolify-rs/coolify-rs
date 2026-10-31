<script lang="ts">
  // Port of upstream resources/views/source/all.blade.php.
  import Empty from '../components/Empty.svelte';
  import GitIcon from '../components/GitIcon.svelte';
  import Modal from '../components/Modal.svelte';
  import Reicon from '../components/Reicon.svelte';
  import SourceCreateForm from '../components/SourceCreateForm.svelte';
  import StatusBadge from '../components/StatusBadge.svelte';
  import ViewToggle from '../components/ViewToggle.svelte';
  import { ApiError } from '../lib/api';
  import { listView } from '../lib/listview.svelte';
  import { link, router } from '../lib/router.svelte';
  import { notify } from '../lib/toast.svelte';
  import { sourcesUi, type Provider, type SourcesPage, type SourceRow } from '../lib/sourcesApi';

  const view = listView('sources', 'table');
  let data = $state<SourcesPage | null>(null);
  let search = $state('');
  let menuOpen = $state(false);
  let menuRoot: HTMLDivElement | undefined = $state();
  let creating = $state<Provider | null>(null);

  $effect(() => {
    sourcesUi
      .list()
      .then((d) => (data = d))
      .catch((e) => notify.failure(e instanceof ApiError ? e.message : String(e)));
    const flash = router.query('error');
    if (flash) notify.failure(flash);
  });

  const label = (s: SourceRow) => (s.is_connected ? 'Connected' : 'Setup incomplete');
  function matches(s: SourceRow): boolean {
    const q = search.trim().toLowerCase();
    return !q || [s.name, s.provider, s.organization, label(s)].some((v) => String(v ?? '').toLowerCase().includes(q));
  }
  const sources = $derived(data?.sources ?? []);
  const filtered = $derived(sources.filter(matches));
  const titles: Record<Provider, string> = { github: 'New GitHub App', gitlab: 'New GitLab App', gitea: 'New Gitea App' };
  const providers: [Provider, string, string][] = [
    ['github', 'App\\Models\\GithubApp', 'GitHub App'],
    ['gitlab', 'App\\Models\\GitlabApp', 'GitLab App'],
    ['gitea', 'App\\Models\\GiteaApp', 'Gitea App'],
  ];
</script>

<svelte:window
  onclick={(e) => {
    if (menuOpen && menuRoot && !menuRoot.contains(e.target as Node)) menuOpen = false;
  }}
  onkeydown={(e) => {
    if (e.key === 'Escape') menuOpen = false;
  }}
/>

{#if data}
  <div class="application-settings-form w-full">
    <header class="mb-5 flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
      <div class="min-w-0">
        <h1 class="truncate text-[24px]! leading-7! font-semibold! tracking-tight!">Sources</h1>
        <p class="mt-1 text-[13px] text-neutral-500 dark:text-fg-dim">
          {sources.length}
          {sources.length === 1 ? 'Git source' : 'Git sources'} connected to your team
        </p>
      </div>
      {#if data.can_create}
        <div class="relative w-fit shrink-0" bind:this={menuRoot}>
          <button type="button" onclick={() => (menuOpen = !menuOpen)} class="button button-highlighted" aria-haspopup="menu" aria-expanded={menuOpen}>
            <Reicon name="plus" class="size-3.5" />
            New source
            <Reicon name="chevron-down" class="size-3 opacity-55" />
          </button>
          {#if menuOpen}
            <div role="menu" class="listbox-panel left-auto! right-0! z-[90]! w-52! min-w-52!">
              {#each providers as [p, morph, text] (p)}
                <div class="relative w-auto h-auto">
                  <button
                    type="button"
                    onclick={() => {
                      menuOpen = false;
                      creating = p;
                    }}
                    class="listbox-option justify-start! gap-2.5!"
                    role="menuitem"
                  >
                    <GitIcon class="size-3.5 shrink-0 opacity-70" git={morph} />
                    {text}
                  </button>
                </div>
              {/each}
            </div>
          {/if}
        </div>
      {/if}
    </header>

    {#if sources.length === 0}
      <Empty title="No sources yet" description="Connect a Git provider to deploy applications directly from your repositories." iconName="sources" />
    {:else}
      <div>
        <div class="mb-4 flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
          <div class="w-full sm:max-w-sm">
            <div class="relative">
              <Reicon name="search" class="pointer-events-none absolute top-1/2 left-2.5 z-10 size-3.5 -translate-y-1/2 text-neutral-400 dark:text-fg-faint" />
              <input
                bind:value={search}
                type="search"
                placeholder="Search sources"
                class="h-8! w-full rounded-lg! border-neutral-200! bg-white! py-0! pr-8! pl-8! text-[12px]! shadow-none! placeholder:text-neutral-400 focus:border-accent! focus:ring-0! dark:border-white/[0.08]! dark:bg-white/[0.035]! dark:text-fg! dark:placeholder:text-fg-faint"
              />
            </div>
          </div>
          <ViewToggle bind:mode={view.mode} />
        </div>

        {#if view.mode === 'grid'}
          <div class="grid grid-cols-1 gap-3 sm:grid-cols-2 lg:grid-cols-4">
            {#each filtered as s (`${s.type}-${s.uuid}`)}
              <a
                use:link
                href="/source/{s.type}/{s.uuid}"
                class="group flex min-h-28 flex-col rounded-xl border border-neutral-200 bg-white p-3 shadow-sm transition-all hover:-translate-y-px hover:border-neutral-300 hover:no-underline hover:shadow-md dark:border-white/[0.08] dark:bg-white/[0.05] dark:hover:border-white/[0.14]"
              >
                <div class="flex items-start gap-3">
                  <div
                    class="flex size-8 shrink-0 items-center justify-center rounded-lg border border-neutral-200 bg-neutral-50 text-neutral-500 dark:border-white/[0.08] dark:bg-white/[0.04] dark:text-fg-dim"
                  >
                    <GitIcon class="size-4" git={s.type} />
                  </div>
                  <div class="min-w-0 flex-1">
                    <h2 class="truncate text-[13px]! leading-4! font-semibold! text-black dark:text-fg">{s.name}</h2>
                    <p class="mt-0.5 truncate text-[11px] text-neutral-500 dark:text-fg-faint">
                      {s.organization ? `${s.provider} · ${s.organization}` : s.provider}
                    </p>
                  </div>
                </div>
                <div class="mt-auto pt-4">
                  {#if s.is_connected}
                    <StatusBadge label="Connected" type="success" />
                  {:else}
                    <StatusBadge label={s.type === 'github' ? 'Setup incomplete' : 'Setup required'} type="warning" />
                  {/if}
                </div>
              </a>
            {/each}
          </div>
        {:else}
          <div class="overflow-x-auto rounded-xl border border-neutral-200 bg-white shadow-sm dark:border-white/[0.08] dark:bg-white/[0.05] {filtered.length === 0 ? 'hidden' : ''}">
            <div
              class="grid min-w-[620px] grid-cols-[minmax(0,1fr)_minmax(10rem,.7fr)_9rem] border-b border-neutral-200 bg-neutral-50 px-4 py-2.5 text-[11px] font-medium text-neutral-500 dark:border-white/[0.08] dark:bg-white/[0.05] dark:text-fg-faint"
            >
              <div>Source</div>
              <div>Provider</div>
              <div>Status</div>
            </div>
            {#each filtered as s (`${s.type}-${s.uuid}`)}
              <a
                use:link
                href="/source/{s.type}/{s.uuid}"
                class="grid min-h-14 min-w-[620px] grid-cols-[minmax(0,1fr)_minmax(10rem,.7fr)_9rem] items-center border-b border-neutral-200 px-4 py-2.5 text-[12px] transition-colors last:border-b-0 hover:bg-neutral-50 hover:no-underline dark:border-white/[0.07] dark:hover:bg-white/[0.025]"
              >
                <div class="truncate font-semibold text-black dark:text-fg">{s.name}</div>
                <div class="truncate text-neutral-500 dark:text-fg-dim">{s.organization ? `${s.provider} · ${s.organization}` : s.provider}</div>
                <div><StatusBadge label={label(s)} type={s.is_connected ? 'success' : 'warning'} /></div>
              </a>
            {/each}
          </div>
        {/if}
        {#if filtered.length === 0}
          <div class="flex min-h-52 flex-col items-center justify-center rounded-xl border border-neutral-200 bg-white px-6 text-center dark:border-white/[0.08] dark:bg-white/[0.05]">
            <Reicon name="search" class="mb-3 size-6 text-neutral-300 dark:text-fg-faint" />
            <p class="text-[13px] font-medium">No matching sources</p>
            <p class="mt-1 text-[12px] text-neutral-500 dark:text-fg-dim">Try a different search.</p>
          </div>
        {/if}
      </div>
    {/if}
  </div>
{/if}

{#if creating && data}
  <Modal title={titles[creating]} onclose={() => (creating = null)}>
    <SourceCreateForm provider={creating} randomName={data.random_name} canCreate={data.can_create} canMakeSystemWide={data.can_make_system_wide} />
  </Modal>
{/if}
