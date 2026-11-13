<script lang="ts">
  // Port of upstream components/table/filter.blade.php: the Filter button with its panel of
  // options and the "Reset filters" entry.
  import type { Snippet } from 'svelte';
  import Dropdown from '../Dropdown.svelte';
  import Reicon from '../Reicon.svelte';

  let {
    activeCount = 0,
    onreset,
    children: content,
  }: { activeCount?: number; onreset: () => void; children: Snippet } = $props();
</script>

<div class="table-filter">
  <Dropdown panelClass="w-44! overflow-hidden! p-0!">
    {#snippet trigger({ open })}
      <button type="button" aria-haspopup="listbox" aria-expanded={open} class="button max-w-80 min-w-0 {activeCount > 0 ? 'button-highlighted' : ''}">
        <Reicon name="filter" class="size-3.5 shrink-0" />
        <span class="truncate">Filter</span>
        {#if activeCount > 0}
          <span class="shrink-0 rounded-full bg-neutral-100 px-1.5 py-0.5 text-[10px] font-medium text-neutral-500 dark:bg-white/[0.07] dark:text-fg-dim">{activeCount}</span>
        {/if}
      </button>
    {/snippet}
    {#snippet children({ close })}
      <div class="max-h-80 overflow-y-auto p-1" aria-multiselectable="true" role="listbox">{@render content()}</div>
      <div class="border-t border-neutral-200 bg-white p-1 dark:border-white/10 dark:bg-raised">
        <button
          type="button"
          class="listbox-option text-neutral-500 dark:text-fg-dim"
          disabled={activeCount === 0}
          onclick={() => {
            onreset();
            close();
          }}
        >
          <span>Reset filters</span>
          <Reicon name="x" class="size-3.5" />
        </button>
      </div>
    {/snippet}
  </Dropdown>
</div>
