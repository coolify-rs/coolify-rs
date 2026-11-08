<script lang="ts" module>
  // Status of a resource row on the environment page: upstream
  // livewire/project/resource/index.blade.php (statusState / statusLabel /
  // statusTitle / statusTone / statusDotClass) rendered in a dynamic
  // components/status-badge.blade.php.
  export interface ResourceStatusInput {
    status: string;
    restartLimitReached?: boolean;
    restartCount?: number;
    maxRestartCount?: number;
  }

  export function statusState(item: ResourceStatusInput): string {
    if (item.restartLimitReached) return 'restart-limit';
    return String(item.status || 'unknown')
      .split(':')[0]
      .toLowerCase();
  }

  export function statusLabel(item: ResourceStatusInput): string {
    if (item.restartLimitReached) return 'Restart limit reached';
    const state = statusState(item);
    return state.charAt(0).toUpperCase() + state.slice(1);
  }

  export function statusTitle(item: ResourceStatusInput): string {
    if (item.restartLimitReached) return `${item.restartCount ?? 0}/${item.maxRestartCount ?? 0} restarts. Container preserved.`;
    return statusLabel(item);
  }

  function statusTone(item: ResourceStatusInput): 'success' | 'warning' | 'error' | 'neutral' {
    if (item.restartLimitReached) return 'warning';
    const state = statusState(item);
    if (state === 'running') return 'success';
    if (['starting', 'restarting', 'degraded'].includes(state)) return 'warning';
    if (['exited', 'stopped', 'failed'].includes(state)) return 'error';
    return 'neutral';
  }

  export function statusDotClass(item: ResourceStatusInput): string {
    if (item.restartLimitReached) return 'bg-warning';
    return {
      success: 'bg-emerald-500',
      warning: 'bg-warning',
      error: 'bg-red-500',
      neutral: 'bg-neutral-400 dark:bg-neutral-500',
    }[statusTone(item)];
  }
</script>

<script lang="ts">
  // `onfilter`: the table variant, a button that filters the table by this status.
  let { item, onfilter }: { item: ResourceStatusInput; onfilter?: () => void } = $props();
</script>

{#if onfilter}
  <button
    type="button"
    class="inline-flex h-6 max-w-full items-center gap-1.5 whitespace-nowrap rounded-full border border-neutral-200 bg-neutral-100 px-2 text-xs font-medium leading-none text-neutral-700 dark:border-white/[0.12] dark:bg-white/[0.07] dark:text-white transition-colors table-filter-value table-filter-badge"
    title="{statusTitle(item)} · Filter by {statusLabel(item)}"
    onclick={onfilter}
  >
    <span class="size-1.5 shrink-0 rounded-full {statusDotClass(item)}"></span>
    <span class="truncate">{statusLabel(item)}</span>
  </button>
{:else}
  <span
    class="inline-flex h-6 max-w-full items-center gap-1.5 whitespace-nowrap rounded-full border border-neutral-200 bg-neutral-100 px-2 text-xs font-medium leading-none text-neutral-700 dark:border-white/[0.12] dark:bg-white/[0.07] dark:text-white"
    title={statusTitle(item)}
  >
    <span class="size-1.5 shrink-0 rounded-full {statusDotClass(item)}"></span>
    <span class="truncate">{statusLabel(item)}</span>
  </span>
{/if}
