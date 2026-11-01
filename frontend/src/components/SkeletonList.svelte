<script lang="ts">
  // Port of upstream components/skeleton/list.blade.php.
  let { rows = 4, label = 'Loading' }: { rows?: number; label?: string } = $props();
  const widths = ['w-2/5', 'w-1/3', 'w-1/2', 'w-1/4'];
  const shimmer = 'animate-pulse bg-neutral-200/80 dark:bg-white/[0.06]';
</script>

<div role="status" class="flex flex-col">
  <span class="sr-only">{label}</span>
  {#each { length: rows } as _, i (i)}
    <div aria-hidden="true" class="flex items-center gap-3 border-b border-neutral-200 px-4 py-3.5 last:border-b-0 dark:border-white/[0.07]">
      <div class="{shimmer} size-9 shrink-0 rounded-lg"></div>
      <div class="flex min-w-0 flex-1 flex-col gap-2">
        <div class="{shimmer} h-3 {widths[i % widths.length]} rounded-full"></div>
        <div class="{shimmer} h-2.5 w-32 rounded-full"></div>
      </div>
      <div class="{shimmer} hidden h-8 w-24 shrink-0 rounded-lg sm:block"></div>
    </div>
  {/each}
</div>
