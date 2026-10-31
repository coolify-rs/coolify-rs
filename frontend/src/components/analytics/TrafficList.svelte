<script lang="ts" generics="T">
  // The `x-data="trafficTable()"` block of the traffic lists with livewire/traffic/_pager.blade.php.
  import type { Snippet } from 'svelte';
  import { TrafficTable, type TableRow } from '../../lib/trafficTable.svelte';

  let {
    rows,
    header,
    row,
    hiddenToggle = false,
  }: { rows: TableRow<T>[]; header: Snippet<[TrafficTable<T>]>; row: Snippet<[T]>; hiddenToggle?: boolean } = $props();

  const table = new TrafficTable<T>();
  $effect.pre(() => {
    table.rows = rows;
  });
</script>

<div>
  <div class="flex items-center gap-3 px-4 py-2 text-[11px] font-medium text-neutral-500 dark:text-fg-dim">
    {@render header(table)}
  </div>
  <div class="flex flex-col">
    {#each table.visible as item (item.key)}
      {@render row(item.data)}
    {/each}
  </div>
  {#if hiddenToggle && table.hiddenCount > 0}
    <div class="flex items-center border-t border-neutral-200 px-4 py-2 dark:border-white/[0.07]">
      <button type="button" onclick={() => table.toggleHidden()} class="text-[11px] font-medium text-neutral-500 transition-colors hover:text-black dark:text-fg-faint dark:hover:text-fg">
        {table.showHidden ? 'Hide container tooling' : `Show ${table.hiddenCount} container tooling ${table.hiddenCount === 1 ? 'agent' : 'agents'} (Docker, BuildKit, …)`}
      </button>
    </div>
  {/if}
  {#if table.total > table.per}
    <div class="flex items-center justify-between gap-3 border-t border-neutral-200 px-4 py-2 dark:border-white/[0.07]">
      <span class="text-[11px] text-neutral-500 dark:text-fg-faint">{`${table.page * table.per + 1}–${Math.min((table.page + 1) * table.per, table.total)} of ${table.total.toLocaleString()}`}</span>
      <div class="flex items-center gap-1">
        <button
          type="button"
          onclick={() => (table.pageIndex = Math.max(0, table.page - 1))}
          disabled={table.page === 0}
          class="flex h-6 items-center rounded-md px-2 text-[11px] font-medium text-neutral-500 transition-colors hover:text-black disabled:cursor-not-allowed disabled:opacity-30 dark:text-fg-faint dark:hover:text-fg"
        >
          Prev
        </button>
        <button
          type="button"
          onclick={() => (table.pageIndex = Math.min(Math.ceil(table.total / table.per) - 1, table.page + 1))}
          disabled={(table.page + 1) * table.per >= table.total}
          class="flex h-6 items-center rounded-md px-2 text-[11px] font-medium text-neutral-500 transition-colors hover:text-black disabled:cursor-not-allowed disabled:opacity-30 dark:text-fg-faint dark:hover:text-fg"
        >
          Next
        </button>
      </div>
    </div>
  {/if}
</div>
