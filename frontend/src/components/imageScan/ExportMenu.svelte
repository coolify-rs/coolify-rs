<script lang="ts">
  // Port of upstream components/image-scan/export-menu.blade.php: copy the shown list as
  // Markdown or IDs, or download it as CSV or JSON, for the rows of all pages.
  import Dropdown from '../Dropdown.svelte';
  import Reicon from '../Reicon.svelte';
  import { copyList, saveDownload } from '../../lib/imageScanApi';
  import { notify } from '../../lib/toast.svelte';
  import { ApiError } from '../../lib/api';

  let {
    count,
    noun = 'finding',
    nouns,
    withIds = true,
    load,
  }: {
    count: number;
    noun?: string;
    /** `str($noun)->plural()`. */
    nouns?: string;
    withIds?: boolean;
    /** The export of the page: `{content}` for markdown and ids, `{name, mime, content}` for csv and json. */
    load: (format: string) => Promise<{ content: string; name?: string; mime?: string }>;
  } = $props();

  const countText = $derived(`${count.toLocaleString('en-US')} ${count === 1 ? noun : (nouns ?? `${noun}s`)}`);

  async function download(format: string) {
    try {
      const r = await load(format);
      saveDownload({ name: r.name ?? `export.${format}`, mime: r.mime ?? 'text/plain', content: r.content });
    } catch (e) {
      notify.failure(e instanceof ApiError ? e.message : String(e));
    }
  }
</script>

<Dropdown role="menu" panelClass="w-64! p-1!">
  {#snippet trigger({ open })}
    <button type="button" class="button normal-case!" aria-haspopup="menu" aria-expanded={open} disabled={count === 0}>
      <Reicon name="download" class="size-3.5" />
      Export
      <Reicon name="chevron-down" class="size-3.5" />
    </button>
  {/snippet}
  {#snippet children({ close })}
    <p class="px-2 pb-1.5 pt-1 text-[11px] leading-4 text-neutral-500 dark:text-fg-faint">
      The {countText} in this list, from all pages.
    </p>
    <button
      type="button"
      class="listbox-option"
      role="menuitem"
      onclick={() => {
        close();
        copyList(() => load('markdown'), `Copied ${countText} as Markdown.`);
      }}
    >
      <span class="flex-1 text-left">Copy as Markdown table</span>
    </button>
    {#if withIds}
      <button
        type="button"
        class="listbox-option"
        role="menuitem"
        onclick={() => {
          close();
          copyList(() => load('ids'), `Copied the IDs of ${countText}.`);
        }}
      >
        <span class="flex-1 text-left">Copy IDs</span>
      </button>
    {/if}
    <div class="my-1 border-t border-neutral-200 dark:border-white/[0.08]" role="separator"></div>
    <button
      type="button"
      class="listbox-option"
      role="menuitem"
      onclick={() => {
        close();
        void download('csv');
      }}
    >
      <span class="flex-1 text-left">Download CSV</span>
    </button>
    <button
      type="button"
      class="listbox-option"
      role="menuitem"
      onclick={() => {
        close();
        void download('json');
      }}
    >
      <span class="flex-1 text-left">Download JSON</span>
    </button>
  {/snippet}
</Dropdown>
