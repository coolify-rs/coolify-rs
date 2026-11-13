<script lang="ts">
  // Port of upstream components/backup-s3-storage.blade.php. S3 is a setting, not a state: plain
  // text keeps the status badge the only pill in the row.
  import Reicon from './Reicon.svelte';

  let { enabled = false, names = [], tooltip = null }: { enabled?: boolean; names?: string[]; tooltip?: string | null } = $props();
</script>

<!-- svelte-ignore a11y_no_noninteractive_tabindex -->
<span class="inline-flex max-w-full min-w-0 items-center gap-1.5" data-tooltip={tooltip ?? undefined} aria-label={tooltip ?? undefined} tabindex={tooltip ? 0 : undefined}>
  {#if !enabled}
    <span class="text-neutral-400 dark:text-fg-faint">Local only</span>
  {:else if names.length === 0}
    <Reicon name="alert-triangle" class="size-3.5 shrink-0 text-red-500" />
    <span class="text-red-600 dark:text-red-400">Unavailable</span>
  {:else}
    <span class="truncate">{names[0]}</span>
    {#if names.length > 1}
      <span class="shrink-0 text-neutral-400 dark:text-fg-faint">+{names.length - 1}</span>
    {/if}
  {/if}
</span>
