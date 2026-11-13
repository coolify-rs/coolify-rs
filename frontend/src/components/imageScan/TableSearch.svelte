<script lang="ts">
  // Port of upstream components/table/search.blade.php with `wire:model.live.debounce.300ms`:
  // `onsearch` runs 300 ms after the last keystroke.
  import Reicon from '../Reicon.svelte';

  let {
    value = $bindable(''),
    placeholder = 'Search',
    loading = false,
    onsearch,
  }: { value?: string; placeholder?: string; loading?: boolean; onsearch: (value: string) => void } = $props();

  let timer: ReturnType<typeof setTimeout> | undefined;
  function input() {
    clearTimeout(timer);
    timer = setTimeout(() => onsearch(value), 300);
  }
  $effect(() => () => clearTimeout(timer));
</script>

<div class="table-search relative min-w-0 w-full">
  <input type="search" {placeholder} aria-label={placeholder} class="input w-full pl-8!" bind:value oninput={input} />
  <div class="pointer-events-none absolute inset-y-0 left-0 flex items-center pl-2.5">
    {#if loading}
      <svg aria-hidden="true" class="size-3.5 animate-spin text-neutral-400 dark:text-fg-dim" fill="none" viewBox="0 0 24 24">
        <circle class="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" stroke-width="4" />
        <path
          class="opacity-75"
          fill="currentColor"
          d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4zm2 5.291A7.962 7.962 0 014 12H0c0 3.042 1.135 5.824 3 7.938l3-2.647z"
        />
      </svg>
    {:else}
      <Reicon name="search" class="size-3.5 text-neutral-400 dark:text-fg-faint" />
    {/if}
  </div>
</div>
