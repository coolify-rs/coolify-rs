<script lang="ts">
  // Port of upstream components/table/sort-header.blade.php: a column header that sorts its
  // table. The first click sorts by `first`, the next click flips the direction.
  let {
    label,
    asc,
    desc,
    first = 'asc',
    value = $bindable(),
    onsort,
    class: klass = '',
  }: {
    label: string;
    asc: string;
    desc: string;
    first?: 'asc' | 'desc';
    /** The sort variable (`model`); bound for client-side tables. */
    value: string | null;
    /** `then` (or `action` for a server-side sort): runs after the change. */
    onsort?: (next: string) => void;
    class?: string;
  } = $props();

  const firstValue = $derived(first === 'desc' ? desc : asc);
  const otherValue = $derived(first === 'desc' ? asc : desc);
  const sorted = $derived(value === asc || value === desc);
  const ariaBase = $derived(`Sort by ${label.toLowerCase()}`);
</script>

<button
  type="button"
  class="table-sort-header group {sorted ? 'is-sorted' : ''} {klass}"
  aria-label={ariaBase + (value === asc ? ', sorted ascending' : value === desc ? ', sorted descending' : '')}
  onclick={() => {
    const next = value === firstValue ? otherValue : firstValue;
    value = next;
    onsort?.(next);
  }}
>
  <span class="truncate">{label}</span>
  <svg class="table-sort-header-icon {sorted ? 'is-visible' : ''} {value === desc ? 'rotate-180' : ''}" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" aria-hidden="true">
    <path stroke-linecap="round" stroke-linejoin="round" d="M6 15l6-6 6 6" />
  </svg>
</button>
