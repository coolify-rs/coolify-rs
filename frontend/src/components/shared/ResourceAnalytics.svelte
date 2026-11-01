<script lang="ts">
  // Port of upstream livewire/project/application/analytics.blade.php with
  // Project\Shared\ResourceTrafficAnalytics, used by the application and service tabs.
  import { onDestroy } from 'svelte';
  import Empty from '../Empty.svelte';
  import Listbox from '../Listbox.svelte';
  import LiveToggle from '../LiveToggle.svelte';
  import SettingsSection from '../SettingsSection.svelte';
  import BreakdownSection from '../analytics/BreakdownSection.svelte';
  import DeviceChart from '../analytics/DeviceChart.svelte';
  import Geo from '../analytics/Geo.svelte';
  import PathsList from '../analytics/PathsList.svelte';
  import RequestsChart from '../analytics/RequestsChart.svelte';
  import Sparkline from '../analytics/Sparkline.svelte';
  import StatusCodes from '../analytics/StatusCodes.svelte';
  import { compactNumber, formatBytes, formatLatency, numberFormat, round1 } from '../../lib/analyticsApi';
  import { loadApexCharts } from '../../lib/charts';
  import { fetchResourceAnalytics, type ResourceAnalyticsData } from '../../lib/metricsApi.svelte';
  import { link } from '../../lib/router.svelte';
  import { failure } from '../../lib/serverApi';
  import { notify } from '../../lib/toast.svelte';

  let { kind, uuid }: { kind: 'application' | 'service'; uuid: string } = $props();

  const rangeOptions = [
    { value: '24h', label: 'Last 24 hours' },
    { value: '7d', label: 'Last 7 days' },
    { value: '30d', label: 'Last 30 days' },
  ];
  const dimensionLabels: [string, string][] = [
    ['referer', 'Referrers'],
    ['browser', 'Browsers'],
    ['os', 'Operating systems'],
  ];
  const skeleton = 'animate-pulse rounded-md bg-neutral-200/80 dark:bg-white/[0.06]';
  const widths = ['w-3/5', 'w-2/5', 'w-1/2', 'w-3/4', 'w-1/3', 'w-2/3'];
  const skeletonLabels = ['Requests', 'Unique visitors', 'Bandwidth', 'Error rate', 'p95 latency'];

  let data = $state<ResourceAnalyticsData | null>(null);
  let range = $state('24h');
  let loading = $state(false);
  let live = $state(false);
  let poll: ReturnType<typeof setInterval> | undefined;
  let sequence = 0;
  const series = $derived(data?.series ?? []);
  const categories = $derived(series.map((b) => b.bucket));
  const breakdown = (dimension: string) => data?.breakdowns?.[dimension] ?? [];
  const ipMode = $derived(data?.ipMode ?? 'full');
  const tile = 'flex flex-col bg-[var(--coollabs-base)] px-4 py-3';
  const tileLabel = 'text-[11px] font-medium tracking-wide text-neutral-500 uppercase dark:text-fg-dim';
  const tileValue = 'mt-1 text-xl font-semibold text-black tabular-nums dark:text-fg';

  async function load() {
    const mine = ++sequence;
    loading = true;
    try {
      const [next] = await Promise.all([fetchResourceAnalytics(kind, uuid, range), loadApexCharts()]);
      if (mine !== sequence) return;
      if (next.error) notify.failure(next.error);
      // `handleError` leaves the properties of the last successful load.
      data = next.error && data ? data : next;
    } catch (e) {
      failure(e);
    } finally {
      if (mine === sequence) loading = false;
    }
  }
  void load();

  // `wire:poll.60s="loadData"`.
  $effect(() => {
    clearInterval(poll);
    poll = live && data?.overview ? setInterval(() => void load(), 60000) : undefined;
  });
  onDestroy(() => clearInterval(poll));
</script>

{#snippet approxBadge(tooltip: string)}
  <span title={tooltip} class="ml-1.5 inline-flex items-center rounded-full bg-amber-100 px-1.5 py-0.5 text-[9px] font-medium tracking-wide text-amber-700 uppercase dark:bg-amber-500/10 dark:text-amber-400">~ approximate</span>
{/snippet}

{#snippet skeletonTable(rows: number)}
  <div class="flex flex-col">
    {#each Array(rows) as _, i (i)}
      <div class="flex min-h-11 items-center gap-3 border-b border-neutral-200 px-4 py-2 last:border-b-0 dark:border-white/[0.07]">
        <div aria-hidden="true" class="{skeleton} h-3 {widths[i % widths.length]} rounded-full"></div>
        <div aria-hidden="true" class="{skeleton} ml-auto h-3 w-10 shrink-0 rounded-full"></div>
      </div>
    {/each}
  </div>
{/snippet}

{#if !data}
  <!-- livewire/project/application/analytics-placeholder.blade.php -->
  <div class="flex flex-col gap-6">
    <SettingsSection id="analytics-range-section" title="Analytics">
      {#snippet actions()}
        <div aria-hidden="true" class="{skeleton} h-8 w-48 rounded-lg"></div>
      {/snippet}
      <div class="grid grid-cols-2 gap-px overflow-hidden rounded-lg bg-neutral-200 sm:grid-cols-3 lg:grid-cols-5 dark:bg-white/[0.07]">
        {#each skeletonLabels as label (label)}
          <div class={tile}>
            <span class={tileLabel}>{label}</span>
            <div aria-hidden="true" class="{skeleton} mt-2 h-6 w-20"></div>
            <div class="mt-auto pt-3"><div aria-hidden="true" class="{skeleton} h-9 w-full rounded"></div></div>
          </div>
        {/each}
      </div>
    </SettingsSection>
    <SettingsSection id="analytics-requests-section" title="Requests">
      <div aria-hidden="true" class="{skeleton} h-[220px] w-full rounded-lg"></div>
    </SettingsSection>
    <SettingsSection id="analytics-status-codes-section" title="Status codes">
      <div aria-hidden="true" class="{skeleton} h-9 w-full rounded-lg"></div>
      <div class="mt-3 flex flex-wrap gap-2">
        <div aria-hidden="true" class="{skeleton} h-4 w-20 rounded-full"></div>
        <div aria-hidden="true" class="{skeleton} h-4 w-16 rounded-full"></div>
        <div aria-hidden="true" class="{skeleton} h-4 w-16 rounded-full"></div>
        <div aria-hidden="true" class="{skeleton} h-4 w-14 rounded-full"></div>
      </div>
    </SettingsSection>
    <SettingsSection id="analytics-paths-section" title="Top paths" flush>
      {@render skeletonTable(6)}
    </SettingsSection>
    <SettingsSection id="analytics-country-section" title="Locations" flush>
      {#snippet actions()}
        <div aria-hidden="true" class="{skeleton} h-9 w-36 rounded-lg"></div>
      {/snippet}
      <div class="grid grid-cols-1 items-start gap-4 p-4 lg:grid-cols-[2fr_3fr]">
        <div class="flex items-center justify-center">
          <div aria-hidden="true" class="{skeleton} aspect-square w-full max-w-[320px] rounded-full"></div>
        </div>
        {@render skeletonTable(7)}
      </div>
    </SettingsSection>
    <div class="grid grid-cols-1 gap-6 lg:grid-cols-2">
      <SettingsSection id="analytics-device-section" title="Requests by device type">
        <div class="flex items-center justify-center py-4">
          <div aria-hidden="true" class="{skeleton} size-40 rounded-full"></div>
        </div>
      </SettingsSection>
      <SettingsSection id="analytics-protocol-section" title="Top HTTP versions">
        {@render skeletonTable(5)}
      </SettingsSection>
    </div>
  </div>
{:else}
  {@const overview = data.overview}
  {@const chartId = data.chartId}
  <div class="flex flex-col gap-6">
    {#if !data.enabled}
      <SettingsSection id="analytics-section" title="Analytics" description="Inspect traffic statistics reported by Sentinel.">
        {#snippet actions()}
          {#if data?.analyticsServerUuid}
            <a class="button" href="/server/{data.analyticsServerUuid}/analytics" use:link>
              Server analytics
              <svg class="inline-flex w-3 h-3 dark:text-neutral-400 text-black" fill="none" stroke="currentColor" viewBox="0 0 24 24">
              <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M10 6H6a2 2 0 00-2 2v10a2 2 0 002 2h10a2 2 0 002-2v-4M14 4h6m0 0v6m0-6L10 14" />
            </svg>
            </a>
          {/if}
        {/snippet}
        <Empty size="sm" title="Traffic analytics is not enabled" description="Enable Sentinel traffic analytics for this server to start collecting request analytics." iconName="network" />
      </SettingsSection>
    {:else if !overview}
      <SettingsSection id="analytics-section" title="Analytics" description="Inspect traffic statistics reported by Sentinel.">
        <Empty size="sm" title="No analytics data yet" description="We could not load traffic analytics for the selected range. Try a different range or check back shortly." iconName="network" />
      </SettingsSection>
    {:else}
      <SettingsSection id="analytics-range-section" title="Analytics" description="Inspect traffic statistics reported by Sentinel.">
        {#snippet actions()}
          <div class="flex items-center gap-2">
            <div class="relative w-full transition-opacity sm:w-44 {loading ? 'pointer-events-none opacity-60' : ''}">
              <Listbox id="range" bind:value={range} options={rangeOptions} onchange={() => void load()} />
              <div class="absolute inset-0 {loading ? 'flex' : 'hidden'} items-center justify-center rounded-lg bg-white/70 dark:bg-base/70">
                <div class="inline-flex items-center justify-center gap-2 text-[13px] text-neutral-500 dark:text-fg-dim" role="status" aria-live="polite" aria-label="Loading analytics">
                  <svg class="loading-indicator shrink-0 animate-spin size-3" viewBox="0 0 24 24" fill="none" aria-hidden="true">
                    <circle class="opacity-20" cx="12" cy="12" r="9" stroke="currentColor" stroke-width="2" />
                    <path class="opacity-80" d="M21 12a9 9 0 0 0-9-9" stroke="currentColor" stroke-width="2" stroke-linecap="round" />
                  </svg>
                </div>
              </div>
            </div>
            <LiveToggle
              bind:live
              storageKey="traffic-live"
              title="Turn live refresh on or off (updates every 60 seconds)"
              onchange={(next) => {
                if (next) void load();
              }}
            />
          </div>
        {/snippet}

        <div class="grid grid-cols-2 gap-px overflow-hidden rounded-lg bg-neutral-200 sm:grid-cols-3 lg:grid-cols-5 dark:bg-white/[0.07]">
          <div class={tile}>
            <span class={tileLabel}>Requests</span>
            <span class={tileValue} title="{numberFormat(overview.requests)} requests">{compactNumber(overview.requests)}</span>
            <div class="mt-auto pt-3">
              <Sparkline id="{chartId}-spark-requests" values={series.map((b) => b.requests)} {categories} colorVar="--chart-status-3xx" label="Requests" />
            </div>
          </div>
          <div class={tile}>
            <span class="flex items-center {tileLabel}">
              Unique visitors
              {#if data.uniquesApproximate}
                {@render approxBadge('Summed across services; visitors of more than one service may be double-counted.')}
              {/if}
            </span>
            <span class={tileValue} title="{numberFormat(overview.uniqueVisitors)} unique visitors">{compactNumber(overview.uniqueVisitors)}</span>
            <div class="mt-auto pt-3">
              <Sparkline id="{chartId}-spark-visitors" values={series.map((b) => b.uniqueVisitors)} {categories} colorVar="--chart-status-2xx" label="Visitors" />
            </div>
          </div>
          <div class={tile}>
            <span class={tileLabel}>Bandwidth</span>
            <span class={tileValue}>{formatBytes(data.bandwidthBytes ?? 0)}</span>
            <div class="mt-auto pt-3">
              <Sparkline id="{chartId}-spark-bandwidth" values={series.map((b) => b.bytesIn + b.bytesOut)} {categories} colorVar="--chart-spark-bandwidth" label="Bandwidth" format="bytes" />
            </div>
          </div>
          <div class={tile}>
            <span class={tileLabel}>Error rate</span>
            <span class={tileValue}>{data.errorRate ?? 0}%</span>
            <div class="mt-auto pt-3">
              <Sparkline id="{chartId}-spark-errors" values={series.map((b) => b.s4xx + b.s5xx)} {categories} colorVar="--chart-status-5xx" label="Errors" />
            </div>
          </div>
          <div class="col-span-2 flex flex-col bg-[var(--coollabs-base)] px-4 py-3 sm:col-span-1">
            <span class="flex items-center {tileLabel}">
              p95 latency
              {#if data.latencyApproximate}
                {@render approxBadge('Highest p95 latency across services; not a true merged percentile.')}
              {/if}
            </span>
            <span class={tileValue}>{formatLatency(overview.latencyP95)}</span>
            <div class="mt-auto pt-3">
              <Sparkline id="{chartId}-spark-latency" values={series.map((b) => round1(b.p95))} {categories} colorVar="--chart-status-4xx" label="p95 latency" format="ms" />
            </div>
          </div>
        </div>
      </SettingsSection>

      <SettingsSection id="analytics-requests-section" title="Requests" flush description="Total request volume over time for the selected range.">
        <RequestsChart {chartId} {series} range={data.range} />
      </SettingsSection>

      <SettingsSection id="analytics-status-codes-section" title="Status codes" description="Share of responses by HTTP status class for the selected range.">
        <StatusCodes {overview} />
      </SettingsSection>

      <SettingsSection id="analytics-paths-section" title="Top paths" description="Most requested paths for the selected range." flush>
        <PathsList paths={data.topPaths ?? []} />
      </SettingsSection>

      <Geo {chartId} countries={data.countries ?? []} cities={data.cities ?? []} attribution={data.attribution ?? null} geoInit={data.geoInit ?? { countries: [], cities: [] }} geo={data.geo ?? { countries: [], cities: [] }} />

      <div class="grid grid-cols-1 gap-6 lg:grid-cols-2">
        <SettingsSection id="analytics-device-section" title="Requests by device type" description="Share of requests by client device class for the selected range.">
          <DeviceChart {chartId} labels={data.device?.labels ?? []} series={data.device?.series ?? []} />
        </SettingsSection>

        <BreakdownSection dimension="protocol" label="Top HTTP versions" rows={breakdown('protocol')} helper="Request volume by negotiated HTTP protocol version." />
      </div>

      <div class="grid grid-cols-1 gap-6 lg:grid-cols-2">
        <BreakdownSection dimension="cache" label="Top cache statuses" rows={breakdown('cache')} helper="Reverse-proxy cache outcome (hit, miss, bypass, …) by request count." />
        <BreakdownSection dimension="status" label="Top status codes" rows={breakdown('status')} helper="Most frequent HTTP response status codes for the selected range." />
      </div>

      <div class="grid grid-cols-1 gap-6 lg:grid-cols-2">
        {#each dimensionLabels as [dimension, label] (dimension)}
          <BreakdownSection {dimension} {label} rows={breakdown(dimension)} />
        {/each}
        <BreakdownSection dimension="agent" label="AI agents & bots" rows={breakdown('agent')} helper="Bot and AI-crawler traffic (GPTBot, ClaudeBot, Googlebot, …) by request count." />
        {#if ipMode !== 'off'}
          <BreakdownSection
            dimension="ip"
            label={ipMode === 'anonymized' ? 'Top networks' : 'Top IPs'}
            rows={breakdown('ip')}
            helper={ipMode === 'anonymized' ? 'Busiest client networks. IPs are anonymized to /24 (IPv4) and /48 (IPv6).' : 'Busiest client IPs (real visitor IP, resolved behind Cloudflare / reverse proxies).'}
          />
        {/if}
      </div>

      <BreakdownSection dimension="useragent" label="Top user agents" rows={breakdown('useragent')} helper="Most frequent raw User-Agent strings for the selected range." />
    {/if}
  </div>
{/if}
