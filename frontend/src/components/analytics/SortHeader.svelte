<script lang="ts">
  // Port of upstream livewire/traffic/_sort-header.blade.php.
  import type { TrafficTable } from '../../lib/trafficTable.svelte';

  let {
    table,
    column,
    label,
    class: klass = '',
    title,
    text = false,
    dir,
  }: {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    table: TrafficTable<any>;
    column: string;
    label: string;
    class?: string;
    title?: string;
    text?: boolean;
    dir?: 'asc' | 'desc';
  } = $props();

  const firstDir = $derived(dir ?? (text ? 'asc' : 'desc'));
  // `hidden` and `inline-flex` on one element conflict, so add the display only when the caller sets none.
  const display = $derived(klass.includes('hidden') ? '' : 'inline-flex');
  const state = $derived(table.sorted(column));
</script>

<button
  type="button"
  onclick={() => table.sortBy(column, firstDir)}
  title={title ?? `Sort by ${label.toLowerCase()}`}
  aria-label="Sort by {label.toLowerCase()}{state ? `, sorted ${state === 'asc' ? 'ascending' : 'descending'}` : ''}"
  class="group {display} items-center gap-1 whitespace-nowrap rounded-sm transition-colors hover:text-black focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-accent dark:hover:text-fg {text ? 'justify-start text-left' : 'flex-row-reverse justify-start text-right'} {klass} {state ? 'text-black dark:text-fg' : ''}"
>
  <span>{label}</span>
  <svg class="size-3 shrink-0 transition-[opacity,transform] {state ? 'opacity-100' : 'opacity-0 group-hover:opacity-50'} {state === 'asc' ? 'rotate-180' : ''}" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" aria-hidden="true">
    <path stroke-linecap="round" stroke-linejoin="round" d="M6 9l6 6 6-6" />
  </svg>
</button>
