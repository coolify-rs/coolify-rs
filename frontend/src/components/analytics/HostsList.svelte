<script lang="ts">
  // Port of upstream livewire/traffic/_hosts-list.blade.php.
  import Empty from '../Empty.svelte';
  import SortHeader from './SortHeader.svelte';
  import TrafficList from './TrafficList.svelte';
  import { compactNumber, formatBytes, numberFormat, round1, type HostRow } from '../../lib/analyticsApi';

  let { hosts }: { hosts: HostRow[] } = $props();
  const maxRequests = $derived(Math.max(1, ...hosts.map((row) => row.requests)));
  const rows = $derived(
    hosts.map((row) => ({
      key: row.host,
      pinned: row.host === '',
      sort: { name: row.host, requests: row.requests, bytesOut: row.bandwidth },
      data: row,
    })),
  );
</script>

{#if hosts.length === 0}
  <Empty size="sm" title="No host data" description="No hostnames were recorded for the selected range." iconName="unordered-list" />
{:else}
  <TrafficList {rows}>
    {#snippet header(table)}
      <SortHeader {table} column="name" label="Host" text class="min-w-0 flex-1" />
      <span class="hidden w-16 shrink-0 sm:inline" aria-hidden="true"></span>
      <SortHeader {table} column="requests" label="Requests" class="w-16 shrink-0" />
      <SortHeader {table} column="bytesOut" label="Bandwidth" class="hidden w-16 shrink-0 sm:inline-flex" title="Total response data sent" />
    {/snippet}
    {#snippet row(item)}
      <div class="flex min-h-11 items-center gap-3 border-t border-neutral-200 px-4 py-2 transition-colors hover:bg-neutral-50 dark:border-white/[0.07] dark:hover:bg-white/[0.03]">
        {#if item.host !== ''}
          <a href="https://{item.host}" target="_blank" rel="noopener noreferrer nofollow" class="min-w-0 flex-1 truncate font-mono text-[12px] text-black hover:underline dark:text-fg">{item.host}</a>
        {:else}
          <span class="min-w-0 flex-1 truncate text-[12px] text-neutral-500 dark:text-fg-dim">Unknown host</span>
        {/if}
        <div class="hidden h-1 w-16 shrink-0 overflow-hidden rounded-full bg-neutral-100 sm:block dark:bg-white/[0.06]" title="Request volume relative to the busiest row in this list">
          <div class="h-full rounded-full bg-[var(--chart-status-3xx)]" style="width: {Math.min(100, round1((item.requests / maxRequests) * 100))}%;"></div>
        </div>
        <span class="w-16 shrink-0 text-right text-[12px] font-medium tabular-nums text-black dark:text-fg" title="{numberFormat(item.requests)} requests">{compactNumber(item.requests)}</span>
        <span class="hidden w-16 shrink-0 text-right text-[11px] tabular-nums text-neutral-400 sm:inline dark:text-fg-faint">{formatBytes(item.bandwidth)}</span>
      </div>
    {/snippet}
  </TrafficList>
{/if}
