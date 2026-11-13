<script lang="ts">
  // The checkbox of a filter option (`$checkbox($selected)` of the image scanning views).
  let { selected }: { selected: boolean } = $props();
</script>

<span
  class={selected
    ? 'flex size-4 shrink-0 items-center justify-center rounded-[5px] border border-coollabs bg-coollabs text-white dark:border-brand dark:bg-brand dark:text-white'
    : 'flex size-4 shrink-0 items-center justify-center rounded-[5px] border border-neutral-300 bg-white dark:border-white/[0.14] dark:bg-white/[0.045]'}
>
  {#if selected}
    <svg class="size-3" viewBox="0 0 12 12" fill="none" aria-hidden="true">
      <path d="m2.25 6.15 2.35 2.3 5.15-5" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" />
    </svg>
  {/if}
</span>
