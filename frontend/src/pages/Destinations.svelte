<script lang="ts">
  // Port of upstream livewire/destination/index.blade.php.
  import Reicon from '../components/Reicon.svelte';
  import Empty from '../components/Empty.svelte';
  import Modal from '../components/Modal.svelte';
  import StatusBadge from '../components/StatusBadge.svelte';
  import ViewToggle from '../components/ViewToggle.svelte';
  import NewDestination from '../components/NewDestination.svelte';
  import { get, type DestinationListData } from '../lib/serverApi';
  import { link } from '../lib/router.svelte';
  import { listView } from '../lib/listview.svelte';

  let data = $state<DestinationListData | null>(null);
  let search = $state('');
  let creating = $state(false);
  const view = listView('destinations', 'table');
  get<DestinationListData>('/destination-list').then((d) => (data = d));

  const typeOf = (swarm: boolean) => (swarm ? 'Docker Swarm' : 'Standalone Docker');
  const filtered = $derived.by(() => {
    const q = search.trim().toLowerCase();
    const all = data?.destinations ?? [];
    if (!q) return all;
    return all.filter((d) => [d.name, d.server, typeOf(d.is_swarm)].some((v) => String(v || '').toLowerCase().includes(q)));
  });
</script>

<div class="application-settings-form w-full">
  <header class="mb-5 flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
    <div class="min-w-0">
      <h1 class="truncate text-[24px]! leading-7! font-semibold! tracking-tight!">Destinations</h1>
      {#if data}
        <p class="mt-1 text-[13px] text-neutral-500 dark:text-fg-dim">
          {data.destinations.length}
          {data.destinations.length === 1 ? 'network endpoint' : 'network endpoints'}
        </p>
      {/if}
    </div>
    {#if data}
      {#if data.servers.length > 0 && data.can_create}
        <div class="w-fit shrink-0">
          <button type="button" class="button button-highlighted" onclick={() => (creating = true)}>
            <Reicon name="plus" class="size-3.5" />
            New destination
          </button>
        </div>
      {/if}
    {/if}
  </header>

  {#if data}
    {#if data.destinations.length === 0}
      <Empty title="No destinations yet" description="Add a Docker network endpoint to choose where your resources are deployed." iconName="destinations" />
    {:else}
      <div>
        <div class="mb-4 flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
          <div class="relative w-full sm:max-w-sm">
            <Reicon name="search" class="pointer-events-none absolute top-1/2 left-2.5 z-10 size-3.5 -translate-y-1/2 text-neutral-400 dark:text-fg-faint" />
            <input
              bind:value={search}
              type="search"
              placeholder="Search destinations"
              class="h-8! w-full rounded-lg! border-neutral-200! bg-white! py-0! pr-8! pl-8! text-[12px]! shadow-none! placeholder:text-neutral-400 focus:border-accent! focus:ring-0! dark:border-white/[0.08]! dark:bg-white/[0.035]! dark:text-fg! dark:placeholder:text-fg-faint"
            />
            {#if search}
              <button
                onclick={() => (search = '')}
                type="button"
                class="absolute top-1/2 right-2 flex size-5 -translate-y-1/2 items-center justify-center rounded text-neutral-400 transition-colors hover:bg-neutral-100 hover:text-black dark:text-fg-faint dark:hover:bg-white/[0.07] dark:hover:text-fg"
                aria-label="Clear search"
              >
                <Reicon name="x" class="size-3" />
              </button>
            {/if}
          </div>
          <div class="flex flex-wrap items-center gap-3 sm:justify-end">
            <ViewToggle bind:mode={view.mode} />
          </div>
        </div>

        {#if view.mode === 'grid'}
          <div class="grid grid-cols-1 gap-3 sm:grid-cols-2 lg:grid-cols-4">
            {#each filtered as d (d.uuid)}
              <a
                href="/destination/{d.uuid}"
                use:link
                class="group flex min-h-28 flex-col rounded-xl border border-neutral-200 bg-white p-3 shadow-sm transition-all hover:-translate-y-px hover:border-neutral-300 hover:no-underline hover:shadow-md dark:border-white/[0.08] dark:bg-white/[0.05] dark:hover:border-white/[0.14]"
              >
                <div class="flex items-start gap-3">
                  <div class="flex size-8 shrink-0 items-center justify-center rounded-lg border border-neutral-200 bg-neutral-50 text-neutral-500 dark:border-white/[0.08] dark:bg-white/[0.04] dark:text-fg-dim">
                    <Reicon name="destinations" class="size-4" />
                  </div>
                  <div class="min-w-0 flex-1">
                    <h2 class="truncate text-[13px]! leading-4! font-semibold! text-black dark:text-fg">{d.name}</h2>
                    <p class="mt-0.5 truncate text-[11px] text-neutral-500 dark:text-fg-faint">{d.server}</p>
                  </div>
                </div>
                <div class="mt-auto flex items-center gap-2 pt-4">
                  <StatusBadge label={typeOf(d.is_swarm)} type={d.is_swarm ? 'warning' : 'success'} />
                </div>
              </a>
            {/each}
          </div>
        {:else}
          <div class="overflow-x-auto rounded-xl border border-neutral-200 bg-white shadow-sm dark:border-white/[0.08] dark:bg-white/[0.05]">
            <div class="grid min-w-[620px] grid-cols-[minmax(0,1fr)_minmax(10rem,.7fr)_10rem] border-b border-neutral-200 bg-neutral-50 px-4 py-2.5 text-[11px] font-medium text-neutral-500 dark:border-white/[0.08] dark:bg-white/[0.05] dark:text-fg-faint">
              <div>Destination</div>
              <div>Server</div>
              <div>Type</div>
            </div>
            {#each filtered as d (d.uuid)}
              <a
                href="/destination/{d.uuid}"
                use:link
                class="grid min-h-14 min-w-[620px] grid-cols-[minmax(0,1fr)_minmax(10rem,.7fr)_10rem] items-center border-b border-neutral-200 px-4 py-2.5 text-[12px] transition-colors last:border-b-0 hover:bg-neutral-50 hover:no-underline dark:border-white/[0.07] dark:hover:bg-white/[0.025]"
              >
                <div class="truncate font-semibold text-black dark:text-fg">{d.name}</div>
                <div class="truncate text-neutral-500 dark:text-fg-dim">{d.server}</div>
                <div><StatusBadge label={typeOf(d.is_swarm)} type={d.is_swarm ? 'warning' : 'success'} /></div>
              </a>
            {/each}
          </div>
        {/if}
        {#if filtered.length === 0}
          <div class="flex min-h-52 flex-col items-center justify-center rounded-xl border border-neutral-200 bg-white px-6 text-center dark:border-white/[0.08] dark:bg-white/[0.05]">
            <Reicon name="search" class="mb-3 size-6 text-neutral-300 dark:text-fg-faint" />
            <p class="text-[13px] font-medium">No matching destinations</p>
            <p class="mt-1 text-[12px] text-neutral-500 dark:text-fg-dim">Try a different search.</p>
          </div>
        {/if}
      </div>
    {/if}
  {/if}
</div>

{#if creating && data}
  <Modal title="New Destination" onclose={() => (creating = false)}>
    <NewDestination servers={data.servers} network={data.network} />
  </Modal>
{/if}
