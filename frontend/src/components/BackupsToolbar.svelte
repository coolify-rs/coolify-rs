<script lang="ts">
  // The search box and the Filter and Sort menus of upstream application/backup/index.blade.php
  // and service/volume-backup/index.blade.php.
  import Dropdown from './Dropdown.svelte';
  import Reicon from './Reicon.svelte';

  let {
    search = $bindable(''),
    typeFilter = $bindable('all'),
    sortBy = $bindable('target_asc'),
    filterOptions,
  }: {
    search?: string;
    typeFilter?: string;
    sortBy?: string;
    filterOptions: { value: string; label: string }[];
  } = $props();

  const sortOptions = [
    { value: 'target_asc', label: 'Target A–Z' },
    { value: 'target_desc', label: 'Target Z–A' },
    { value: 'newest', label: 'Newest first' },
    { value: 'oldest', label: 'Oldest first' },
  ];
</script>

{#snippet option(label: string, selected: boolean, pick: () => void)}
  <button
    type="button"
    class="flex h-8 w-full items-center rounded-md px-2 text-left text-[12px] text-neutral-600 transition-colors hover:bg-neutral-100 hover:text-black dark:text-fg-dim dark:hover:bg-white/[0.06] dark:hover:text-fg"
    onclick={pick}
  >
    <span class="flex-1">{label}</span>
    {#if selected}
      <svg class="size-3.5 text-black dark:text-fg" viewBox="0 0 12 12" fill="none" aria-hidden="true">
        <path d="m2.5 6.25 2.1 2.1 4.9-5" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round" />
      </svg>
    {/if}
  </button>
{/snippet}

<div class="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
  <div class="relative w-full sm:max-w-sm">
    <Reicon name="search" class="pointer-events-none absolute top-1/2 left-2.5 z-10 size-3.5 -translate-y-1/2 text-neutral-400 dark:text-fg-faint" />
    <input type="search" bind:value={search} placeholder="Search backups" aria-label="Search backups" class="input h-8! w-full py-0! pr-8! pl-8!" />
    {#if search}
      <button
        onclick={() => (search = '')}
        type="button"
        class="absolute top-1/2 right-2 flex size-5 -translate-y-1/2 items-center justify-center rounded text-neutral-400 transition-colors hover:bg-neutral-100 hover:text-black dark:text-fg-faint dark:hover:bg-white/[0.07] dark:hover:text-fg"
        aria-label="Clear search"
      >
        <span class="text-sm leading-none">×</span>
      </button>
    {/if}
  </div>

  <div class="flex items-center gap-2">
    <Dropdown panelClass="w-48!">
      {#snippet trigger({ open })}
        <button type="button" class="button" aria-haspopup="listbox" aria-expanded={open}>
          <svg class="size-3.5 opacity-65" viewBox="0 0 24 24" fill="none" aria-hidden="true">
            <path d="M4 6h16M7 12h10M10 18h4" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" />
          </svg>
          Filter
        </button>
      {/snippet}
      {#snippet children({ close })}
        {#each filterOptions as o (o.value)}
          {@render option(o.label, typeFilter === o.value, () => {
            typeFilter = o.value;
            close();
          })}
        {/each}
      {/snippet}
    </Dropdown>

    <Dropdown panelClass="w-48!">
      {#snippet trigger({ open })}
        <button type="button" class="button" aria-haspopup="listbox" aria-expanded={open}>
          <svg class="size-3.5 opacity-65" viewBox="0 0 24 24" fill="none" aria-hidden="true">
            <path d="M8 5v14m0 0-3-3m3 3 3-3M16 19V5m0 0-3 3m3-3 3 3" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" />
          </svg>
          Sort
        </button>
      {/snippet}
      {#snippet children({ close })}
        {#each sortOptions as o (o.value)}
          {@render option(o.label, sortBy === o.value, () => {
            sortBy = o.value;
            close();
          })}
        {/each}
      {/snippet}
    </Dropdown>
  </div>
</div>
