<script lang="ts">
  // Port of upstream components/breadcrumb-switcher.blade.php.
  import Reicon from './Reicon.svelte';
  import { link } from '../lib/router.svelte';
  import { page, type ResourceSwitcher } from '../lib/page.svelte';

  let { switcher }: { switcher: ResourceSwitcher } = $props();

  let open = $state(false);
  let search = $state('');
  let root: HTMLDivElement | undefined = $state();
  // `$href`: the page of the active item.
  const href = $derived(switcher.items.find((item) => item.active)?.href);

  function focus(node: HTMLInputElement) {
    queueMicrotask(() => node.focus());
  }
</script>

<svelte:window
  onclick={(e) => open && root && !root.contains(e.target as Node) && (open = false)}
  onkeydown={(e) => e.key === 'Escape' && (open = false)}
/>

<span class="shrink-0 px-0.5 text-neutral-300 dark:text-fg-faint">/</span>
<div class="relative min-w-0 shrink" bind:this={root}>
  <div class="flex h-8 min-w-0 items-center gap-1">
    <div class="flex h-8 min-w-0 items-center rounded-md opacity-70 transition-opacity hover:opacity-100">
      <a
        {href}
        use:link
        title="Go to {switcher.title.replace(/s$/, '').toLowerCase()}"
        class="flex h-8 min-w-0 items-center rounded-l-md pr-1 pl-2 transition-colors hover:bg-neutral-100 dark:hover:bg-white/[0.05]"
      >
        <span class="min-w-0 truncate font-semibold text-black dark:text-fg">{page.nameOf(switcher.uuid, switcher.label)}</span>
      </a>
      <button
        type="button"
        onclick={() => {
          open = !open;
          search = '';
        }}
        title="Switch resource"
        class="flex h-8 shrink-0 items-center rounded-r-md px-0.5 transition-colors hover:bg-neutral-100 dark:hover:bg-white/[0.05]"
      >
        <svg class="size-4 shrink-0 text-neutral-400 dark:text-fg-faint" viewBox="0 0 24 24" fill="none">
          <path d="M8 9l4-4 4 4M8 15l4 4 4-4" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" />
        </svg>
      </button>
    </div>
    {#if switcher.usable !== undefined}
      <span class="inline-flex h-[22px] shrink-0 items-center gap-1.5 text-xs text-neutral-700 dark:text-fg">
        <span class="size-1.5 rounded-full {switcher.usable ? 'bg-[#3fb950]' : 'bg-red-500'}"></span>
        <span>{switcher.usable ? 'Connected' : 'Not usable'}</span>
      </span>
    {/if}
  </div>

  {#if open}
    <div class="listbox-panel scrollbar left-0! z-[90]! max-h-80! min-w-56 max-w-72">
      <div class="searchable-listbox-search">
        <Reicon name="search" class="pointer-events-none absolute top-1/2 left-4 size-3 -translate-y-1/2 text-neutral-400 dark:text-fg-faint" />
        <input use:focus bind:value={search} type="search" autocomplete="off" placeholder="Search {switcher.title.toLowerCase()}" class="searchable-listbox-search-input" />
      </div>
      <div class="px-2 py-1 text-[10px] font-semibold uppercase tracking-wide text-neutral-400 dark:text-fg-faint">
        {switcher.title}
      </div>
      {#each switcher.items as item (item.href)}
        {#if page.nameOf(item.uuid, item.label).toLowerCase().includes(search.toLowerCase())}
          <a
            href={item.href}
            use:link
            onclick={() => (open = false)}
            class="listbox-option {item.active ? 'bg-neutral-100 font-medium text-black dark:bg-white/[0.07] dark:text-fg' : ''}"
          >
            <span class="min-w-0 flex-1 truncate">{page.nameOf(item.uuid, item.label)}</span>
          </a>
        {/if}
      {/each}
    </div>
  {/if}
</div>
