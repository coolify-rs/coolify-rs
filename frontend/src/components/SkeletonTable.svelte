<script lang="ts">
  // Port of upstream components/skeleton/table.blade.php.
  let { rows = 6, flush = false }: { rows?: number; flush?: boolean } = $props();
  // Vary the leading-column widths so rows read as content, not a solid block.
  const widths = ['w-3/5', 'w-2/5', 'w-1/2', 'w-3/4', 'w-1/3', 'w-2/3'];
  const shimmer = 'animate-pulse rounded-md bg-neutral-200/80 dark:bg-white/[0.06]';
</script>

{#if flush}
  <div class="flex flex-col">
    {#each { length: rows } as _, i (i)}
      <div class="flex min-h-11 items-center gap-3 border-b border-neutral-200 px-4 py-2 last:border-b-0 dark:border-white/[0.07]">
        <div aria-hidden="true" class="{shimmer} h-3 {widths[i % widths.length]} rounded-full"></div>
        <div aria-hidden="true" class="{shimmer} ml-auto h-3 w-10 shrink-0 rounded-full"></div>
      </div>
    {/each}
  </div>
{:else}
  <div class="flex flex-col gap-2.5">
    {#each { length: rows } as _, i (i)}
      <div class="flex items-center justify-between gap-4">
        <div aria-hidden="true" class="{shimmer} h-3 {widths[i % widths.length]} rounded-full"></div>
        <div aria-hidden="true" class="{shimmer} h-3 w-10 shrink-0 rounded-full"></div>
      </div>
    {/each}
  </div>
{/if}
