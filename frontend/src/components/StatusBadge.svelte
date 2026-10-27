<script lang="ts">
  // Port of upstream components/status-badge.blade.php (span variant).
  let { label, type = 'neutral' }: { label: string; type?: 'neutral' | 'success' | 'warning' | 'error' } = $props();

  const dot = {
    neutral: 'bg-neutral-400 dark:bg-neutral-500',
    success: 'bg-emerald-500',
    warning: 'bg-warning',
    error: 'bg-red-500',
  };
</script>

<span
  class="inline-flex h-6 max-w-full items-center gap-1.5 whitespace-nowrap rounded-full border border-neutral-200 bg-neutral-100 px-2 text-xs font-medium leading-none text-neutral-700 dark:border-white/[0.12] dark:bg-white/[0.07] dark:text-white"
>
  <span class="relative flex size-1.5 shrink-0">
    <span class="relative inline-flex size-1.5 rounded-full {dot[type]}"></span>
  </span>
  <span class="truncate">{label}</span>
</span>
