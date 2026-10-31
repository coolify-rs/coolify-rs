<script lang="ts">
  // Port of upstream livewire/server/index.blade.php (no canvas view; patch
  // status and Sentinel checks are not part of coolify-rs yet).
  import Reicon from '../components/Reicon.svelte';
  import Empty from '../components/Empty.svelte';
  import ViewToggle from '../components/ViewToggle.svelte';
  import ServerMetricsChart from '../components/dashboard/ServerMetricsChart.svelte';
  import { serversUi, type ServersPageData } from '../lib/api';
  import { serverMetricsPoll } from '../lib/metricsApi.svelte';
  import { link } from '../lib/router.svelte';
  import { listView } from '../lib/listview.svelte';

  let data = $state<ServersPageData | null>(null);
  let search = $state('');
  const view = listView('servers', 'table');
  const metrics = serverMetricsPoll();
  serversUi.list().then((d) => {
    data = d;
    metrics.start(d.servers.filter((s) => s.is_metrics_enabled).map((s) => s.uuid));
  });

  const filtered = $derived.by(() => {
    const q = search.trim().toLowerCase();
    const all = data?.servers ?? [];
    if (!q) return all;
    return all.filter((s) => [s.name, s.description, s.status].some((v) => String(v || '').toLowerCase().includes(q)));
  });
  const visible = (uuid: string) => filtered.some((s) => s.uuid === uuid);
  const glyph = 'resource-glyph flex size-8 shrink-0 items-center justify-center rounded-lg border border-neutral-200 bg-neutral-50 text-neutral-500';
</script>

<div class="application-settings-form w-full">
  <div class="mb-5 flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
    <h1 class="min-w-0 text-[24px]! leading-7! font-semibold! tracking-tight!">Servers</h1>
    <div class="flex flex-wrap items-center gap-2">
      {#if data?.can_create}
        <a href="/servers/new" use:link class="button w-fit shrink-0 whitespace-nowrap button-highlighted">
          <Reicon name="plus" class="size-3.5" />
          New server
        </a>
      {/if}
    </div>
  </div>

  <div>
    {#if data && data.servers.length === 0}
      <Empty title="No servers yet" description="Add a server to deploy applications, databases, and services." iconName="servers" />
    {:else if data}
      <div class="mb-4 flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
        <div class="relative w-full sm:max-w-sm">
          <Reicon
            name="search"
            class="pointer-events-none absolute top-1/2 left-2.5 z-10 size-3.5 -translate-y-1/2 text-neutral-400 dark:text-fg-faint"
          />
          <input
            bind:value={search}
            type="search"
            placeholder="Search servers"
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
          {#each data.servers as s (s.uuid)}
            {#if visible(s.uuid)}
              <a
                href="/server/{s.uuid}"
                use:link
                class="group relative flex min-h-28 flex-col rounded-xl border border-neutral-200 bg-white p-3 shadow-sm transition-all hover:-translate-y-px hover:border-neutral-300 hover:no-underline hover:shadow-md dark:border-white/[0.08] dark:bg-white/[0.05] dark:hover:border-white/[0.14]"
              >
                {#if s.is_metrics_enabled}
                  <ServerMetricsChart uuid={s.uuid} data={metrics.charts[s.uuid]} />
                {/if}
                <div class="relative z-10 flex items-start gap-3">
                  <div class={glyph}><Reicon name="servers" class="size-4" /></div>
                  <div class="min-w-0 flex-1 self-center">
                    <h2 class="truncate text-[13px]! leading-4! font-semibold! text-black dark:text-fg">{s.name}</h2>
                    {#if s.description}
                      <p class="mt-0.5 truncate text-[11px] text-neutral-500 dark:text-fg-faint">{s.description}</p>
                    {/if}
                  </div>
                  {#if s.status_type !== 'success'}
                    <span
                      data-tooltip={s.status}
                      aria-label="Server status: {s.status}"
                      class="ml-auto flex size-6 shrink-0 items-center justify-center rounded-md {s.status_type === 'warning'
                        ? 'text-orange-500 dark:text-warning'
                        : 'text-red-500 dark:text-red-400'}"
                    >
                      <Reicon name="alert-triangle" class="size-4" />
                    </span>
                  {/if}
                </div>
              </a>
            {/if}
          {/each}
        </div>
      {:else}
        <div class="overflow-x-auto rounded-xl border border-neutral-200 bg-white shadow-sm dark:border-white/[0.08] dark:bg-white/[0.05]">
          <div
            class="grid min-w-[480px] grid-cols-[minmax(0,1fr)_9.5rem] border-b border-neutral-200 bg-neutral-50 px-4 py-2.5 text-[11px] font-medium text-neutral-500 md:min-w-[560px] md:grid-cols-[minmax(0,1fr)_6rem_9.5rem] dark:border-white/[0.08] dark:bg-white/[0.05] dark:text-fg-faint"
          >
            <div>Server</div>
            <div class="hidden md:block">Resources</div>
            <div>Status</div>
          </div>
          {#each filtered as s (s.uuid)}
            <a
              href="/server/{s.uuid}"
              use:link
              class="grid min-h-14 min-w-[480px] grid-cols-[minmax(0,1fr)_9.5rem] items-center border-b border-neutral-200 px-4 py-2.5 text-[12px] transition-colors last:border-b-0 hover:bg-neutral-50 hover:no-underline md:min-w-[560px] md:grid-cols-[minmax(0,1fr)_6rem_9.5rem] dark:border-white/[0.07] dark:hover:bg-white/[0.025]"
            >
              <div class="flex min-w-0 items-center gap-3">
                <div class={glyph}><Reicon name="servers" class="size-4" /></div>
                <div class="min-w-0">
                  <p class="truncate text-[13px] font-semibold text-black dark:text-fg">{s.name}</p>
                  <p class="truncate text-[11px] text-neutral-500 dark:text-fg-faint">{s.description ?? ''}</p>
                </div>
                {#if s.status_type !== 'success'}
                  <span
                    data-tooltip={s.status}
                    aria-label="Server status: {s.status}"
                    class="ml-auto flex size-6 shrink-0 items-center justify-center rounded-md md:hidden {s.status_type === 'warning'
                      ? 'text-orange-500 dark:text-warning'
                      : 'text-red-500 dark:text-red-400'}"
                  >
                    <Reicon name="alert-triangle" class="size-4" />
                  </span>
                {/if}
              </div>
              <div class="hidden text-[12px] font-medium text-neutral-600 md:block dark:text-fg-dim">
                <span class="inline-flex items-center gap-1" title="{s.resources} {s.resources === 1 ? 'resource' : 'resources'}">
                  <Reicon name="grid" class="size-3.5 text-neutral-400 dark:text-fg-faint" />
                  <span>{s.resources}</span>
                </span>
              </div>
              <div class="flex items-center gap-2 text-[11px] font-medium text-neutral-600 dark:text-fg-dim">
                {#if s.status_type !== 'success'}
                  <span
                    class="hidden size-2 shrink-0 rounded-full md:inline-block {s.status_type === 'warning'
                      ? 'bg-orange-500 dark:bg-warning'
                      : 'bg-red-500 dark:bg-red-400'}"
                  ></span>
                {:else}
                  <span class="hidden size-2 shrink-0 rounded-full bg-green-500 md:inline-block dark:bg-green-400"></span>
                {/if}
                <span>{s.status}</span>
              </div>
            </a>
          {/each}
        </div>
      {/if}

      {#if filtered.length === 0}
        <div
          class="flex min-h-52 flex-col items-center justify-center rounded-xl border border-neutral-200 bg-white px-6 text-center dark:border-white/[0.08] dark:bg-white/[0.05]"
        >
          <Reicon name="search" class="mb-3 size-6 text-neutral-300 dark:text-fg-faint" />
          <p class="text-[13px] font-medium">No matching servers</p>
          <p class="mt-1 text-[12px] text-neutral-500 dark:text-fg-dim">Try a different search.</p>
        </div>
      {/if}
    {/if}
  </div>
</div>
