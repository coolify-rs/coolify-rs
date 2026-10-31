<script lang="ts">
  // Port of upstream components/client-pagination.blade.php with
  // components/page-size-select.blade.php.
  import Reicon from './Reicon.svelte';
  import Dropdown from './Dropdown.svelte';
  import { notify } from '../lib/toast.svelte';

  let {
    total,
    page = $bindable(1),
    pageSize = $bindable(10),
    storageKey,
    options = [10, 25, 50, 100],
    separator = '-',
    sizeSelect = true,
    shownSize,
    class: cls = '',
  }: {
    total: number;
    page?: number;
    pageSize?: number;
    storageKey?: string;
    options?: number[];
    /** components/table-pagination.blade.php uses an en dash. */
    separator?: string;
    sizeSelect?: boolean;
    /** What the select shows until it is used (`page-size-select` starts at its own default). */
    shownSize?: number;
    class?: string;
  } = $props();

  const totalPages = $derived(Math.max(1, Math.ceil(total / pageSize)));
  const rangeStart = $derived(total === 0 ? 0 : (page - 1) * pageSize + 1);
  const rangeEnd = $derived(Math.min(page * pageSize, total));

  let customizing = $state(false);
  let picked = $state(false);
  const displayed = $derived(!picked && shownSize !== undefined ? shownSize : pageSize);
  let custom = $state<number | ''>('');

  function apply(value: number | '' | null) {
    if (value === '' || value === null) {
      customizing = false;
      return;
    }
    if (Number(value) > 100) notify.info('Maximum is 100 rows per page.');
    const size = Math.min(100, Math.max(1, Number(value) || 1));
    pageSize = size;
    picked = true;
    custom = size;
    customizing = false;
    page = 1;
    if (storageKey) {
      try {
        localStorage.setItem(storageKey, String(size));
      } catch {
        // Storage unavailable: the size lasts for this page only.
      }
    }
  }

  // Restore the saved page size once.
  $effect.pre(() => {
    if (!storageKey) return;
    try {
      const saved = Number(localStorage.getItem(storageKey));
      if (saved >= 1 && saved <= 100) pageSize = saved;
    } catch {
      // Storage unavailable: keep the default size.
    }
  });

  function focusInput(node: HTMLInputElement) {
    queueMicrotask(() => node.focus());
  }
  const pagerButton =
    'flex size-7 items-center justify-center rounded-md border border-neutral-200 text-neutral-500 transition-colors hover:bg-neutral-100 hover:text-black disabled:pointer-events-none disabled:opacity-35 dark:border-white/[0.08] dark:text-fg-dim dark:hover:bg-white/[0.06] dark:hover:text-fg';
</script>

<footer
  hidden={sizeSelect ? total <= Math.min(...options) : totalPages <= 1}
  class="pager flex min-h-11 items-center justify-between border-t border-neutral-200 px-4 text-[11px] text-neutral-500 dark:border-white/[0.08] dark:text-fg-faint {cls}"
>
  <div class="flex items-center gap-3">
    <span class="inline-flex h-7 items-center whitespace-nowrap tabular-nums">{rangeStart}{separator}{rangeEnd} of {total}</span>
    {#if sizeSelect}
    <div class="mb-0! flex h-7 items-center gap-1.5 text-[11px] text-neutral-500 dark:text-fg-dim">
      {#if !customizing}
        <Dropdown panelClass="min-w-24!">
          {#snippet trigger({ open })}
            <button
              type="button"
              aria-label="Items per page"
              aria-haspopup="listbox"
              aria-expanded={open}
              class="flex h-7! items-center gap-1 whitespace-nowrap rounded-md border border-neutral-200 pr-1.5 pl-2 text-[11px]! leading-none! tabular-nums text-neutral-500 transition-colors hover:bg-neutral-100 hover:text-black disabled:pointer-events-none disabled:opacity-35 dark:border-white/[0.08] dark:text-fg-dim dark:hover:bg-white/[0.06] dark:hover:text-fg"
            >
              <span>{displayed}</span>
              <span class="text-neutral-400 dark:text-fg-faint">/ page</span>
              <Reicon name="chevron-down" class="size-3.5 shrink-0 text-neutral-400 dark:text-fg-faint" />
            </button>
          {/snippet}
          {#snippet children({ close })}
            {#each options as option (option)}
              <button
                type="button"
                class="listbox-option"
                role="option"
                aria-selected={displayed === option}
                onclick={() => {
                  close();
                  apply(option);
                }}
              >
                <span>{option}</span>
                {#if displayed === option}<Reicon name="check" class="size-3.5" />{/if}
              </button>
            {/each}
            <button
              type="button"
              class="listbox-option"
              role="option"
              aria-selected="false"
              onclick={() => {
                close();
                custom = '';
                customizing = true;
              }}
            >
              Custom…
            </button>
          {/snippet}
        </Dropdown>
      {:else}
        <input
          use:focusInput
          bind:value={custom}
          onkeydown={(e) => {
            if (e.key === 'Enter') {
              e.preventDefault();
              apply(custom);
            } else if (e.key === 'Escape') {
              e.preventDefault();
              customizing = false;
            }
          }}
          onblur={() => customizing && apply(custom)}
          type="number"
          min="1"
          max="100"
          placeholder="1–100"
          inputmode="numeric"
          aria-label="Custom items per page"
          class="mb-0! h-7! w-14! rounded-md! border-neutral-200! bg-transparent! px-1.5! py-0! text-[11px]! tabular-nums shadow-none! focus:border-neutral-300! focus:ring-0! dark:border-white/[0.08]! dark:text-fg-dim!"
        />
      {/if}
    </div>
    {/if}
  </div>
  <div class="flex items-center gap-1">
    <button type="button" onclick={() => (page = Math.max(1, page - 1))} disabled={page === 1} class={pagerButton} aria-label="Previous page">
      <Reicon name="arrow-right" class="size-3.5 rotate-180" />
    </button>
    <button
      type="button"
      onclick={() => (page = Math.min(totalPages, page + 1))}
      disabled={page >= totalPages}
      class={pagerButton}
      aria-label="Next page"
    >
      <Reicon name="arrow-right" class="size-3.5" />
    </button>
  </div>
</footer>
