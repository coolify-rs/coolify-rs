<script lang="ts">
  // Port of upstream components/loading.blade.php.
  let { text, compact = false, class: klass = '', label }: { text?: string; compact?: boolean; class?: string; label?: string } = $props();
</script>

<div class="inline-flex items-center justify-center gap-2 text-[13px] text-neutral-500 dark:text-fg-dim {klass}" role="status" aria-live="polite" aria-label={label}>
  <svg class="loading-indicator shrink-0 animate-spin {compact ? 'size-3' : 'size-4'}" viewBox="0 0 24 24" fill="none" aria-hidden="true">
    <circle class="opacity-20" cx="12" cy="12" r="9" stroke="currentColor" stroke-width="2" />
    <path class="opacity-80" d="M21 12a9 9 0 0 0-9-9" stroke="currentColor" stroke-width="2" stroke-linecap="round" />
  </svg>
  {#if text}
    <span>{text}</span>
  {/if}
</div>
