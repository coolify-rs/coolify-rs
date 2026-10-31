<script lang="ts">
  // Port of upstream livewire/traffic/_paths-list.blade.php.
  import Empty from '../Empty.svelte';
  import SortHeader from './SortHeader.svelte';
  import TrafficList from './TrafficList.svelte';
  import { compactNumber, formatBytes, formatLatency, numberFormat, round1, type PathRow } from '../../lib/analyticsApi';

  let { paths }: { paths: PathRow[] } = $props();
  const maxRequests = $derived(Math.max(1, ...paths.map((row) => row.requests)));
  const muted = 'text-neutral-300 dark:text-fg-faint';
  const errorRate = (row: PathRow) => (row.requests > 0 ? round1(((row.s4xx + row.s5xx) / row.requests) * 100) : 0);
  const rows = $derived(
    paths.map((row) => ({
      key: `${row.domain ?? ''}\n${row.path}`,
      pinned: row.path === '__other__',
      sort: { name: `${row.path} ${row.domain ?? ''}`, requests: row.requests, s4xx: row.s4xx, s5xx: row.s5xx, errorRate: errorRate(row), bytesOut: row.bytesOut, p95: row.p95 },
      data: row,
    })),
  );
</script>

{#if paths.length === 0}
  <Empty size="sm" title="No path data" description="No requests were recorded for the selected range." iconName="unordered-list" />
{:else}
  <TrafficList {rows}>
    {#snippet header(table)}
      <SortHeader {table} column="name" label="Path" text class="min-w-0 flex-1" />
      <span class="hidden w-16 shrink-0 sm:inline" aria-hidden="true"></span>
      <SortHeader {table} column="requests" label="Requests" class="w-16 shrink-0" />
      <SortHeader {table} column="s4xx" label="4xx" class="hidden w-12 shrink-0 sm:inline-flex" title="HTTP 4xx client-error responses" />
      <SortHeader {table} column="s5xx" label="5xx" class="w-12 shrink-0" title="HTTP 5xx server-error responses" />
      <SortHeader {table} column="errorRate" label="Error %" class="hidden w-14 shrink-0 lg:inline-flex" title="Percentage of requests with a 4xx or 5xx response" />
      <SortHeader {table} column="bytesOut" label="Bandwidth" class="hidden w-16 shrink-0 sm:inline-flex" title="Total response data sent" />
      <SortHeader {table} column="p95" label="p95" class="hidden w-16 shrink-0 md:inline-flex" title="95% of requests completed within this response time" />
    {/snippet}
    {#snippet row(item)}
      {@const rate = errorRate(item)}
      <div class="flex min-h-11 items-center gap-3 border-t border-neutral-200 px-4 py-2 transition-colors hover:bg-neutral-50 dark:border-white/[0.07] dark:hover:bg-white/[0.03]">
        {#if item.path === '__other__'}
          <span class="min-w-0 flex-1 truncate text-[12px] text-neutral-500 dark:text-fg-dim" title="All paths outside the top paths that Sentinel tracks">Other paths</span>
        {:else}
          <span class="flex min-w-0 flex-1 items-baseline gap-1.5">
            {#if item.domain}
              <a href="https://{item.domain}{item.path}" target="_blank" rel="noopener noreferrer nofollow" class="truncate font-mono text-[12px] text-black hover:underline dark:text-fg">{item.path}</a>
              <span class="max-w-48 shrink-0 truncate text-[11px] text-neutral-400 dark:text-fg-faint" title={item.domain}>{item.domain}</span>
            {:else}
              <span class="truncate font-mono text-[12px] text-black dark:text-fg">{item.path}</span>
            {/if}
          </span>
        {/if}
        <div class="hidden h-1 w-16 shrink-0 overflow-hidden rounded-full bg-neutral-100 sm:block dark:bg-white/[0.06]" title="Request volume relative to the busiest row in this list">
          <div class="h-full rounded-full bg-[var(--chart-status-3xx)]" style="width: {Math.min(100, round1((item.requests / maxRequests) * 100))}%;"></div>
        </div>
        <span class="w-16 shrink-0 text-right text-[12px] font-medium tabular-nums text-black dark:text-fg" title="{numberFormat(item.requests)} requests">{compactNumber(item.requests)}</span>
        <span class="hidden w-12 shrink-0 text-right text-[12px] tabular-nums sm:inline {item.s4xx > 0 ? 'font-medium text-pink-600 dark:text-pink-400' : muted}" title="{numberFormat(item.s4xx)} client-error responses">{compactNumber(item.s4xx)}</span>
        <span class="w-12 shrink-0 text-right text-[12px] tabular-nums {item.s5xx > 0 ? 'font-medium text-purple-600 dark:text-purple-400' : muted}" title="{numberFormat(item.s5xx)} server-error responses">{compactNumber(item.s5xx)}</span>
        <span class="hidden w-14 shrink-0 text-right text-[11px] tabular-nums lg:inline {rate > 0 ? 'text-neutral-500 dark:text-fg-dim' : muted}" title="Combined 4xx and 5xx response rate">{rate}%</span>
        <span class="hidden w-16 shrink-0 text-right text-[11px] tabular-nums text-neutral-400 sm:inline dark:text-fg-faint">{formatBytes(item.bytesOut)}</span>
        <span class="hidden w-16 shrink-0 text-right text-[11px] tabular-nums text-neutral-400 md:inline dark:text-fg-faint" title="p95 latency: {numberFormat(item.p95, 1)} ms">{formatLatency(item.p95)}</span>
      </div>
    {/snippet}
  </TrafficList>
{/if}
