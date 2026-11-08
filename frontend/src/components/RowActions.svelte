<script lang="ts">
  // Port of upstream components/table/row-actions.blade.php: the row action menu of
  // a table, a "..." icon button at the far right of the row. Put <RowAction> items
  // in `children` and destructive items in `danger`: they show in red, below a
  // divider. A click on any item closes the menu, also when the item opens a modal.
  import type { Snippet } from 'svelte';
  import Dropdown from './Dropdown.svelte';
  import Reicon from './Reicon.svelte';

  let {
    label = 'Actions',
    children: items,
    danger,
  }: {
    label?: string;
    children?: Snippet;
    danger?: Snippet;
  } = $props();
</script>

<Dropdown panelClass="w-44!" role="menu">
  {#snippet trigger({ open })}
    <button type="button" class="icon-button" title={label} aria-label={label} aria-haspopup="menu" aria-expanded={open}>
      <Reicon name="dots-horizontal" class="size-4" />
    </button>
  {/snippet}
  {#snippet children({ close })}
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <div class="contents" onclick={(e) => (e.target as Element).closest('[role=menuitem]') && close()}>
      {@render items?.()}
      {#if danger}
        {#if items}
          <div class="my-1 border-t border-neutral-200 dark:border-white/[0.08]"></div>
        {/if}
        <div class="row-actions-danger contents">
          {@render danger()}
        </div>
      {/if}
    </div>
  {/snippet}
</Dropdown>
