<script lang="ts">
  // Port of upstream livewire/traffic/_status-codes.blade.php.
  import Empty from '../Empty.svelte';
  import { compactNumber, numberFormat, round1, type Overview } from '../../lib/analyticsApi';

  let { overview }: { overview: Overview } = $props();
  const codes = $derived([
    { label: '2xx', count: overview.s2xx, color: '#3b82f6' },
    { label: '3xx', count: overview.s3xx, color: '#eab308' },
    { label: '4xx', count: overview.s4xx, color: '#ec4899' },
    { label: '5xx', count: overview.s5xx, color: '#a855f7' },
  ]);
  const total = $derived(codes.reduce((sum, code) => sum + code.count, 0));
  let tip = $state({ show: false, x: 0, y: 0, label: '', count: '', pct: '', color: '' });
  const width = (count: number) => Math.round((count / total) * 100 * 1000) / 1000;
</script>

<div class="flex flex-col gap-3">
  <div class="flex flex-wrap items-center gap-x-5 gap-y-2">
    {#each codes as code (code.label)}
      <div class="flex items-center gap-1.5">
        <span class="h-2 w-2 shrink-0 rounded-full" style="background-color: {code.color};" aria-hidden="true"></span>
        <span class="text-[12px] text-neutral-500 dark:text-fg-dim">{code.label}</span>
        <span class="text-[12px] font-semibold tabular-nums text-black dark:text-fg" title="{numberFormat(code.count)} responses">{compactNumber(code.count)}</span>
      </div>
    {/each}
  </div>

  {#if total > 0}
    <div class="flex h-7 w-full overflow-hidden rounded-md bg-neutral-100 dark:bg-white/[0.06]">
      {#each codes as code (code.label)}
        {#if code.count > 0}
          <div
            role="presentation"
            class="h-full cursor-default"
            style="width: {width(code.count)}%; background-color: {code.color}; transition: filter 0.12s ease;"
            onmouseenter={(e) => {
              tip = { show: true, x: e.clientX, y: e.clientY, label: code.label, count: numberFormat(code.count), pct: `${round1((code.count / total) * 100)}%`, color: code.color };
              e.currentTarget.style.filter = 'brightness(1.12)';
            }}
            onmousemove={(e) => {
              tip.x = e.clientX;
              tip.y = e.clientY;
            }}
            onmouseleave={(e) => {
              tip.show = false;
              e.currentTarget.style.filter = '';
            }}
          ></div>
        {/if}
      {/each}
    </div>

    {#if tip.show}
      <div style="left: {tip.x + 14}px; top: {tip.y + 14}px;" class="pointer-events-none fixed z-[100] rounded-lg border border-neutral-700 bg-neutral-900 px-2.5 py-1.5 text-white shadow-lg">
        <div class="flex items-center gap-1.5 text-[12px] font-medium">
          <span class="h-2 w-2 shrink-0 rounded-full" style="background-color: {tip.color};" aria-hidden="true"></span>
          <span>{tip.label}</span>
        </div>
        <div class="mt-0.5 text-[11px] text-neutral-300"><span>{tip.count}</span> responses · <span>{tip.pct}</span></div>
      </div>
    {/if}
  {:else}
    <Empty size="sm" title="No status data" description="No responses were recorded for the selected range." iconName="network" />
  {/if}
</div>
