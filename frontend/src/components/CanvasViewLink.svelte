<script lang="ts">
  // Port of upstream components/canvas-view-toggle.blade.php with `on-canvas`: the labelled link
  // back to the list. `?view=` makes the list the remembered view again.
  import Reicon from './Reicon.svelte';
  import { link } from '../lib/router.svelte';

  let { listUrl, view = 'table' }: { listUrl: string; view?: string } = $props();
  const icon = $derived(view === 'list' ? 'unordered-list' : 'unordered-list');
</script>

<div class="view-toggle">
  <a
    href="{listUrl}?view={view}"
    use:link
    class="relative flex h-7.5 shrink-0 items-center gap-1.5 whitespace-nowrap rounded-md px-2.5 text-[12px] font-medium transition-colors text-neutral-400 hover:bg-neutral-100 hover:text-black dark:hover:text-white"
    title="List view"
  >
    <Reicon name={icon} class="size-3.5" />
    List view
  </a>
</div>
