<script lang="ts">
  // Port of the `#server-topbar-context` part of upstream
  // livewire/server/navbar.blade.php (server switcher) with
  // components/server/status-summary.blade.php (status chip). Sentinel status
  // and Traefik update checks are not part of coolify-rs yet.
  import Reicon from './Reicon.svelte';
  import { link } from '../lib/router.svelte';
  import type { ServerTopbar } from '../lib/page.svelte';

  let { server }: { server: ServerTopbar } = $props();

  let open = $state(false);
  let statusOpen = $state(false);
  let search = $state('');
  let switcher: HTMLSpanElement | undefined = $state();
  let status: HTMLDivElement | undefined = $state();

  const proxyNeedsAttention = $derived(server.proxySet && server.proxyStatus !== 'running');
  const summary = $derived<[string, 'success' | 'warning' | 'error']>(
    !server.functional ? ['Unavailable', 'error'] : proxyNeedsAttention ? ['Attention required', 'warning'] : ['Ready', 'success'],
  );
  const dot = { success: 'bg-success', warning: 'bg-warning', error: 'bg-error' };
  const headline = (s: string | null) =>
    (s || 'unknown')
      .replace(/[_-]+/g, ' ')
      .replace(/\b\w/g, (c) => c.toUpperCase());
  const transitional = $derived(['starting', 'restarting', 'stopping'].includes(server.proxyStatus ?? ''));
  const filtered = $derived(server.servers.filter((s) => s.name.toLowerCase().includes(search.toLowerCase())));
</script>

<svelte:window
  onclick={(e) => {
    if (open && switcher && !switcher.contains(e.target as Node)) open = false;
    if (statusOpen && status && !status.contains(e.target as Node)) statusOpen = false;
  }}
  onkeydown={(e) => e.key === 'Escape' && ((open = false), (statusOpen = false))}
/>

<div data-testid="server-topbar-context" class="flex min-w-0 items-center gap-1 text-[13px]">
  <span class="shrink-0 px-0.5 text-neutral-300 dark:text-fg-faint">/</span>
  <span class="relative flex min-w-0 shrink items-center gap-2 px-1" bind:this={switcher}>
    <button
      type="button"
      class="flex h-8 max-w-56 min-w-0 items-center gap-1.5 rounded-md px-2 transition-colors hover:bg-neutral-100 dark:hover:bg-white/[0.05] xl:max-w-72"
      onclick={() => (open = !open)}
      aria-expanded={open}
      aria-label="Switch server"
    >
      <span class="min-w-0 truncate font-semibold text-black dark:text-fg">{server.name}</span>
      <svg class="size-4 shrink-0 text-neutral-400 dark:text-fg-faint" viewBox="0 0 24 24" fill="none" aria-hidden="true">
        <path d="M8 9l4-4 4 4M8 15l4 4 4-4" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" />
      </svg>
    </button>
    {#if open}
      <div class="listbox-panel top-9! left-1! z-[90]! w-64! min-w-0!">
        <div class="border-b border-neutral-200 p-1.5 dark:border-white/[0.08]">
          <div class="relative">
            <Reicon name="search" class="pointer-events-none absolute top-1/2 left-2 size-3 -translate-y-1/2 text-neutral-400 dark:text-fg-faint" />
            <input
              type="search"
              bind:value={search}
              placeholder="Filter servers…"
              class="input h-7! w-full rounded-md! border-neutral-200! bg-white! py-0! pr-2! pl-7! text-[11px]! text-black! placeholder:text-neutral-400! dark:border-white/[0.08]! dark:bg-white/[0.04]! dark:text-fg!"
            />
          </div>
        </div>
        {#each filtered as option (option.uuid)}
          <a href="/server/{option.uuid}" use:link class="listbox-option gap-2.5!" onclick={() => (open = false)}>
            <span class="size-1.5 shrink-0 rounded-full {option.functional ? 'bg-success' : 'bg-error'}"></span>
            <span class="min-w-0 flex-1 truncate">{option.name}</span>
            {#if option.uuid === server.uuid}<Reicon name="check" class="size-3.5 shrink-0 text-black dark:text-fg" />{/if}
          </a>
        {/each}
      </div>
    {/if}
  </span>
  <div class="relative shrink-0" bind:this={status}>
    <button
      type="button"
      onclick={() => (statusOpen = !statusOpen)}
      aria-expanded={statusOpen}
      aria-haspopup="menu"
      class="inline-flex h-6 max-w-full items-center gap-1.5 whitespace-nowrap rounded-full border border-neutral-200 bg-neutral-100 px-2 text-xs font-medium leading-none text-neutral-700 dark:border-white/[0.12] dark:bg-white/[0.07] dark:text-white transition-colors cursor-pointer"
    >
      <span class="size-1.5 shrink-0 rounded-full {dot[summary[1]]}"></span>
      <span class="truncate">{summary[0]}</span>
      <span class="inline-flex transition-transform {statusOpen ? 'rotate-180' : ''}">
        <Reicon name="chevron-down" class="size-3 opacity-55" />
      </span>
    </button>
    {#if statusOpen}
      <div class="listbox-panel top-8! right-auto! left-0! z-[90]! w-64! min-w-64!" role="menu">
        <div class="flex items-center gap-1 px-3 py-2 text-[11px] font-medium text-neutral-400 dark:text-fg-faint">
          <span>System status</span>
        </div>
        <div class="listbox-option cursor-default! gap-2.5!">
          <span class="size-1.5 shrink-0 rounded-full {server.functional ? 'bg-success' : 'bg-error'}"></span>
          <span class="flex-1">Server</span>
          <span>{server.functional ? 'Ready' : 'Unavailable'}</span>
        </div>
        {#if server.proxySet}
          <a href="/server/{server.uuid}/proxy" use:link class="listbox-option gap-2.5!" onclick={() => (statusOpen = false)} role="menuitem">
            <span
              class="size-1.5 shrink-0 rounded-full {server.proxyStatus === 'running'
                ? 'bg-success'
                : transitional
                  ? 'bg-warning'
                  : 'bg-error'}"
            ></span>
            <span class="flex-1">Proxy</span>
            <span>{headline(server.proxyStatus)}</span>
          </a>
        {/if}
      </div>
    {/if}
  </div>
</div>
