<script lang="ts">
  // Port of upstream components/status-badge.blade.php (span variant).
  let {
    label,
    type = 'neutral',
    size = 'md',
    class: klass = '',
  }: {
    label: string;
    type?: 'neutral' | 'success' | 'warning' | 'error';
    /** `sm` fits inline next to 12px table text without growing the row. */
    size?: 'sm' | 'md';
    class?: string;
  } = $props();

  const dot = {
    neutral: 'bg-neutral-400 dark:bg-neutral-500',
    success: 'bg-emerald-500',
    warning: 'bg-warning',
    error: 'bg-red-500',
  };
  const sizes = {
    sm: 'h-4 gap-1 px-1.5 text-[10px]',
    md: 'h-6 gap-1.5 px-2 text-xs',
  };
</script>

<span
  class="{klass} inline-flex max-w-full items-center whitespace-nowrap rounded-full border border-neutral-200 bg-neutral-100 font-medium leading-none text-neutral-700 dark:border-white/[0.12] dark:bg-white/[0.07] dark:text-white {sizes[size]}"
>
  <span class="relative flex size-1.5 shrink-0">
    <span class="relative inline-flex size-1.5 rounded-full {dot[type]}"></span>
  </span>
  <span class="truncate">{label}</span>
</span>
