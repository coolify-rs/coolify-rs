<script lang="ts">
  // Port of upstream livewire/traffic/_breakdown-section.blade.php.
  import Empty from '../Empty.svelte';
  import SettingsSection from '../SettingsSection.svelte';
  import SortHeader from './SortHeader.svelte';
  import TrafficList from './TrafficList.svelte';
  import { compactNumber, formatBytes, numberFormat, round1, type BreakdownRow } from '../../lib/analyticsApi';

  let { dimension, label, rows, helper }: { dimension: string; label: string; rows: BreakdownRow[]; helper?: string } = $props();

  const description = $derived(helper ?? `Top ${label.toLowerCase()} by request count for the selected range.`);
  const maxRequests = $derived(Math.max(1, ...rows.map((row) => row.requests)));
  const list = $derived(
    rows.map((row) => ({
      key: row.value,
      pinned: row.value === '__other__',
      hidable: row.hidable,
      sort: { name: row.display, requests: row.requests, bytesOut: row.bytesOut },
      data: row,
    })),
  );
  function hideImage(e: Event) {
    (e.currentTarget as HTMLImageElement).style.visibility = 'hidden';
  }
</script>

<SettingsSection id="analytics-{dimension}-section" title={label} {description} flush>
  {#if rows.length === 0}
    <Empty size="sm" title="No data" description="No {label.toLowerCase()} data for the selected range." iconName="network" />
  {:else}
    <TrafficList rows={list} hiddenToggle>
      {#snippet header(table)}
        <SortHeader {table} column="name" label="Name" text class="min-w-0 flex-1" />
        <span class="hidden w-16 shrink-0 sm:inline" aria-hidden="true"></span>
        <SortHeader {table} column="requests" label="Requests" class="w-16 shrink-0" />
        <SortHeader {table} column="bytesOut" label="Bandwidth" class="hidden w-16 shrink-0 sm:inline-flex" title="Total response data sent" />
      {/snippet}
      {#snippet row(item)}
        <div class="flex min-h-11 items-center gap-3 border-t border-neutral-200 px-4 py-2 transition-colors hover:bg-neutral-50 dark:border-white/[0.07] dark:hover:bg-white/[0.03]">
          {#if dimension === 'referer' && item.host}
            <img loading="lazy" src={item.favicon} alt="" width="16" height="16" class="size-4 shrink-0 rounded-sm" onerror={hideImage} />
            <a href="https://{item.host}" target="_blank" rel="noopener noreferrer nofollow" class="min-w-0 flex-1 truncate text-[12px] text-black hover:underline dark:text-fg">{item.display}</a>
          {:else}
            <span class="min-w-0 flex-1 truncate text-[12px] text-black dark:text-fg" title={item.display}>{item.display}</span>
          {/if}
          <div class="hidden h-1 w-16 shrink-0 overflow-hidden rounded-full bg-neutral-100 sm:block dark:bg-white/[0.06]">
            <div class="h-full rounded-full bg-[var(--chart-status-3xx)]" style="width: {Math.min(100, round1((item.requests / maxRequests) * 100))}%;"></div>
          </div>
          <span class="w-16 shrink-0 text-right text-[12px] font-medium tabular-nums text-black dark:text-fg" title="{numberFormat(item.requests)} requests">{compactNumber(item.requests)}</span>
          <span class="hidden w-16 shrink-0 text-right text-[11px] tabular-nums text-neutral-400 sm:inline dark:text-fg-faint">{formatBytes(item.bytesOut)}</span>
        </div>
      {/snippet}
    </TrafficList>
  {/if}
</SettingsSection>
