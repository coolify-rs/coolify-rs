<script lang="ts">
  // Port of upstream components/canvas-view-toggle.blade.php (table / grid
  // switch; coolify-rs has no canvas view).
  import Reicon from './Reicon.svelte';

  let { mode = $bindable('table'), canvasUrl }: { mode?: string; canvasUrl?: string } = $props();

  const views: [string, string, string][] = [
    ['table', 'Table view', 'unordered-list'],
    ['grid', 'Grid view', 'grid'],
  ];
  const item =
    'relative flex h-7.5 shrink-0 items-center gap-1.5 whitespace-nowrap rounded-md px-2.5 text-[12px] font-medium transition-colors text-neutral-400 hover:bg-neutral-100 hover:text-black dark:hover:text-white';
</script>

<div class="view-toggle">
  <span class="flex w-7.5 shrink-0">
    {#each views as [m, label, icon] (m)}
      {#if mode !== m}
        <button type="button" onclick={() => (mode = m)} class="{item} w-7.5 justify-center px-0!" title={label} aria-label={label}>
          <Reicon name={icon} class="size-3.5" />
        </button>
      {/if}
    {/each}
  </span>
  {#if canvasUrl}
    <a href={canvasUrl} class={item} title="Canvas view (beta): see everything and how it connects">
      <Reicon name="deploy-canvas" class="size-3.5" />
      Canvas
      <span class="ml-0.5 inline-flex items-center rounded-full bg-coollabs px-1.5 py-px text-[10px] leading-4 font-semibold text-white"> Try it </span>
    </a>
  {/if}
</div>
