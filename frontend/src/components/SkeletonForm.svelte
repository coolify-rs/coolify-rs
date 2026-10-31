<script lang="ts">
  // Port of upstream components/skeleton/form.blade.php.
  let { fields = 4, label = 'Loading' }: { fields?: number; label?: string } = $props();
  const shimmer = 'animate-pulse rounded-md bg-neutral-200/80 dark:bg-white/[0.06]';
</script>

<div role="status" class="grid gap-4 md:grid-cols-2">
  <span class="sr-only">{label}</span>
  {#each { length: fields } as _, i (i)}
    <div aria-hidden="true" class="flex flex-col gap-2">
      <div aria-hidden="true" class="{shimmer} h-2.5 {i % 2 === 0 ? 'w-24' : 'w-32'} rounded-full"></div>
      <div aria-hidden="true" class="{shimmer} h-9 w-full rounded-lg"></div>
    </div>
  {/each}
</div>
