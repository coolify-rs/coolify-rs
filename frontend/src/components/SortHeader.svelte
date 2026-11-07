<script lang="ts">
  // Port of upstream components/table/sort-header.blade.php: a column header that
  // sorts its table. The first click sorts by `first`, the next click flips the
  // direction. An arrow shows the active direction. `server` is the Livewire
  // variant (the sort runs on the server): while `loading`, the arrow is a spinner.
  let {
    label,
    asc,
    desc,
    first = 'asc',
    current,
    onsort,
    server = false,
    loading = false,
    class: klass = '',
    title,
  }: {
    label: string;
    asc: string;
    desc: string;
    first?: 'asc' | 'desc';
    current: string;
    onsort: (value: string) => void;
    server?: boolean;
    loading?: boolean;
    class?: string;
    title?: string;
  } = $props();

  const firstValue = $derived(first === 'desc' ? desc : asc);
  const otherValue = $derived(first === 'desc' ? asc : desc);
  const direction = $derived(current === asc ? 'asc' : current === desc ? 'desc' : null);
  const ariaBase = $derived(`Sort by ${label.toLowerCase()}`);
</script>

<button
  type="button"
  class="table-sort-header group {klass}"
  class:is-sorted={direction !== null}
  aria-label="{ariaBase}{direction ? `, sorted ${direction === 'asc' ? 'ascending' : 'descending'}` : ''}"
  aria-busy={server && loading ? 'true' : undefined}
  {title}
  onclick={() => onsort(current === firstValue ? otherValue : firstValue)}
>
  <span class="truncate">{label}</span>
  {#if server && loading}
    <svg class="size-3 shrink-0 animate-spin" viewBox="0 0 24 24" fill="none" aria-hidden="true">
      <circle class="opacity-20" cx="12" cy="12" r="9" stroke="currentColor" stroke-width="3" />
      <path d="M21 12a9 9 0 0 0-9-9" stroke="currentColor" stroke-width="3" stroke-linecap="round" />
    </svg>
  {:else}
    <svg
      class="table-sort-header-icon"
      class:rotate-180={direction === 'desc'}
      class:is-visible={direction !== null}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      stroke-width="2.5"
      aria-hidden="true"
    >
      <path stroke-linecap="round" stroke-linejoin="round" d="M6 15l6-6 6 6" />
    </svg>
  {/if}
</button>
