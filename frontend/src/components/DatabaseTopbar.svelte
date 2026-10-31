<script lang="ts">
  // Port of the resource part of upstream components/top-breadcrumb.blade.php
  // (components/breadcrumb-switcher.blade.php with the database status as its
  // meta). It is placed after the project and environment switchers, in the
  // top bar's `#server-topbar-context`.
  import Reicon from './Reicon.svelte';
  import DatabaseStatusSummary from './DatabaseStatusSummary.svelte';
  import { link } from '../lib/router.svelte';

  let {
    label,
    items,
    status,
    healthcheckUrl,
    restartLimit = null,
    statusTitle = 'Database status',
    containerName = 'Container',
  }: {
    statusTitle?: string;
    containerName?: string;
    label: string;
    items: { label: string; href: string; active: boolean }[];
    status: string;
    healthcheckUrl: string;
    restartLimit?: number | null;
  } = $props();

  let open = $state(false);
  let search = $state('');
  let root: HTMLDivElement | undefined = $state();
  let input: HTMLInputElement | undefined = $state();

  const shown = $derived(items.filter((i) => i.label.toLowerCase().includes(search.toLowerCase())));
</script>

<svelte:window
  onclick={(e) => open && root && !root.contains(e.target as Node) && (open = false)}
  onkeydown={(e) => e.key === 'Escape' && (open = false)}
/>

<div class="flex min-w-0 items-center gap-0.5 text-[13px]">
  <span class="shrink-0 px-0.5 text-neutral-300 dark:text-fg-faint">/</span>
  <div class="relative min-w-0 shrink" bind:this={root}>
    <div class="flex h-8 min-w-0 items-center gap-1">
      <button
        type="button"
        onclick={() => {
          open = !open;
          if (open) {
            search = '';
            queueMicrotask(() => input?.focus());
          }
        }}
        title="Switch resource"
        class="flex h-8 min-w-0 items-center gap-1.5 rounded-md px-2 opacity-70 transition-[background-color,opacity] hover:bg-neutral-100 hover:opacity-100 dark:hover:bg-white/[0.05]"
      >
        <span class="min-w-0 truncate font-semibold text-black dark:text-fg">{label}</span>
        <svg class="size-4 shrink-0 text-neutral-400 dark:text-fg-faint" viewBox="0 0 24 24" fill="none">
          <path d="M8 9l4-4 4 4M8 15l4 4 4-4" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" />
        </svg>
      </button>
      <!-- Below xl, the page heading shows the status. -->
      <div class="hidden xl:flex">
        <DatabaseStatusSummary {status} {healthcheckUrl} {restartLimit} title={statusTitle} {containerName} />
      </div>
    </div>

    {#if open}
      <div class="listbox-panel scrollbar left-0! z-[90]! max-h-80! min-w-56 max-w-72">
        <div class="searchable-listbox-search">
          <Reicon name="search" class="pointer-events-none absolute top-1/2 left-4 size-3 -translate-y-1/2 text-neutral-400 dark:text-fg-faint" />
          <input
            bind:this={input}
            bind:value={search}
            type="search"
            autocomplete="off"
            placeholder="Search resources"
            class="searchable-listbox-search-input"
          />
        </div>
        <div class="px-2 py-1 text-[10px] font-semibold uppercase tracking-wide text-neutral-400 dark:text-fg-faint">Resources</div>
        {#each shown as item (item.href)}
          <a
            href={item.href}
            use:link
            onclick={() => (open = false)}
            class="listbox-option {item.active ? 'bg-neutral-100 font-medium text-black dark:bg-white/[0.07] dark:text-fg' : ''}"
          >
            <span class="min-w-0 flex-1 truncate">{item.label}</span>
          </a>
        {/each}
      </div>
    {/if}
  </div>
</div>
