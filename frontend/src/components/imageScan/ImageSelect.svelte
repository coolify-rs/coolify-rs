<script lang="ts">
  // Port of upstream components/image-scan/image-select.blade.php: the image picker of a list,
  // rendered only for more than one image.
  import Dropdown from '../Dropdown.svelte';
  import Reicon from '../Reicon.svelte';

  let { options = [], selected = [], onselect }: { options?: string[]; selected?: string[]; onselect: (image: string) => void } = $props();

  const current = $derived(selected.length === 1 ? selected[0] : null);
</script>

{#if options.length > 1}
  <Dropdown panelClass="min-w-56! max-w-80! p-1!">
    {#snippet trigger({ open })}
      <button type="button" aria-haspopup="listbox" aria-expanded={open} class="button max-w-64 min-w-0 normal-case! {current !== null ? 'button-highlighted' : ''}">
        <Reicon name="layers" class="size-3.5 shrink-0" />
        <span class="truncate" title={current ?? 'All images'}>{current ?? 'All images'}</span>
        <Reicon name="chevron-down" class="size-3.5 shrink-0" />
      </button>
    {/snippet}
    {#snippet children({ close })}
      {#each ['', ...options] as value (value)}
        <button
          type="button"
          class="listbox-option"
          role="option"
          aria-selected={(current ?? '') === value ? 'true' : 'false'}
          onclick={() => {
            onselect(value);
            close();
          }}
        >
          <span class="truncate {value !== '' ? 'font-mono text-[12px]' : ''}" title={value || 'All images'}>{value || 'All images'}</span>
          {#if (current ?? '') === value}
            <Reicon name="check" class="size-3.5 shrink-0" />
          {/if}
        </button>
      {/each}
    {/snippet}
  </Dropdown>
{/if}
