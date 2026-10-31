<script lang="ts">
  // Port of upstream livewire/analytics.blade.php with app/Livewire/Analytics.php's client side:
  // the filters kept in the URL, the "Live" poll, and `trafficAnalyticsStateChanged`.
  import { onDestroy, untrack } from 'svelte';
  import Empty from '../Empty.svelte';
  import Listbox from '../Listbox.svelte';
  import SettingsSection from '../SettingsSection.svelte';
  import BreakdownSection from './BreakdownSection.svelte';
  import DeviceChart from './DeviceChart.svelte';
  import Geo from './Geo.svelte';
  import HostsList from './HostsList.svelte';
  import PathsList from './PathsList.svelte';
  import Placeholder from './Placeholder.svelte';
  import RequestsChart from './RequestsChart.svelte';
  import SortHeader from './SortHeader.svelte';
  import Sparkline from './Sparkline.svelte';
  import StatusCodes from './StatusCodes.svelte';
  import TrafficList from './TrafficList.svelte';
  import { compactNumber, fetchAnalytics, formatBytes, formatLatency, numberFormat, round1, strLimit, type AnalyticsData, type AppRow } from '../../lib/analyticsApi';
  import { loadApexCharts } from '../../lib/charts';
  import { link, router } from '../../lib/router.svelte';
  import { failure } from '../../lib/serverApi';

  let {
    scopedServerUuid = null,
    stateVersion = 0,
    hideSkeleton = false,
  }: { scopedServerUuid?: string | null; stateVersion?: number; hideSkeleton?: boolean } = $props();

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

  let data = $state<AnalyticsData | null>(null);
  let range = $state(router.query('range') ?? '24h');
  let serverUuid = $state(router.query('server') ?? '');
  let appUuid = $state(router.query('app') ?? '');
  let loading = $state<string | null>(null);
  let live = $state(false);
  let dismissed = $state(false);
  let sequence = 0;

  try {
    if (localStorage.getItem('traffic-live') === '1') live = true;
  } catch {
    // Off by default.
  }

  /** `#[Url]`: the filters are kept in the address, without their defaults. */
  function syncUrl() {
    const query = new URLSearchParams(window.location.search);
    const set = (key: string, value: string, fallback: string) => {
      if (value && value !== fallback) query.set(key, value);
      else query.delete(key);
    };
    set('range', range, '24h');
    if (!scopedServerUuid) set('server', serverUuid, '');
    set('app', appUuid, '');
    const qs = query.toString();
    const search = qs ? `?${qs}` : '';
    history.replaceState(null, '', `${window.location.pathname}${search}`);
    router.search = search;
  }

  /** `loadData`. */
  async function load(target: string | null = null) {
    const mine = ++sequence;
    loading = target;
    try {
      const [next] = await Promise.all([fetchAnalytics(scopedServerUuid, { range, server: serverUuid, app: appUuid }), loadApexCharts()]);
      if (mine !== sequence) return;
      data = next;
      serverUuid = next.serverUuid;
      appUuid = next.appUuid;
      syncUrl();
      try {
        dismissed = next.nudgeKey !== '' && localStorage.getItem(`traffic-nudge-${next.nudgeKey}`) === '1';
      } catch {
        dismissed = false;
      }
    } catch (e) {
      failure(e);
    } finally {
      if (mine === sequence) loading = null;
    }
  }

  // Mount, and `refreshTrafficAnalyticsState` after the settings toggled analytics.
  $effect(() => {
    void stateVersion;
    untrack(() => void load());
  });

  function setLive(next: boolean) {
    live = next;
    try {
      localStorage.setItem('traffic-live', next ? '1' : '0');
    } catch {
      // Kept for this page only.
    }
    if (next) void load();
  }
  // `wire:poll.60s="loadData"`.
  let poll: ReturnType<typeof setInterval> | undefined;
  $effect(() => {
    clearInterval(poll);
    poll = live && data?.overview ? setInterval(() => void load(), 60000) : undefined;
  });
  onDestroy(() => clearInterval(poll));

  function dismissNudge() {
    dismissed = true;
    try {
      localStorage.setItem(`traffic-nudge-${data?.nudgeKey}`, '1');
    } catch {
      // Dismissed for this page only.
    }
  }

  const series = $derived(data?.series ?? []);
  const categories = $derived(series.map((b) => b.bucket));
  const breakdown = (dimension: string) => data?.breakdowns?.[dimension] ?? [];
  const topApps = $derived(data?.topApps ?? []);
  const maxAppRequests = $derived(Math.max(1, ...topApps.map((row) => row.requests)));
  const appRows = $derived(
    topApps.map((row) => ({
      key: row.uuid,
      sort: { name: row.name, requests: row.requests, bytesOut: row.bandwidth },
      data: row,
    })),
  );
  const tile = 'flex flex-col bg-[var(--coollabs-base)] px-4 py-3';
  const tileLabel = 'text-[11px] font-medium tracking-wide text-neutral-500 uppercase dark:text-fg-dim';
  const tileValue = 'mt-1 text-xl font-semibold text-black tabular-nums dark:text-fg';
  const ipMode = $derived(data?.ipMode ?? 'full');
  const rowClass = 'flex min-h-11 items-center gap-3 border-t border-neutral-200 px-4 py-2 transition-colors hover:bg-neutral-50 dark:border-white/[0.07] dark:hover:bg-white/[0.03]';
</script>

{#snippet approxBadge(tooltip: string)}
  <span title={tooltip} class="ml-1.5 inline-flex items-center rounded-full bg-amber-100 px-1.5 py-0.5 text-[9px] font-medium tracking-wide text-amber-700 uppercase dark:bg-amber-500/10 dark:text-amber-400">~ approximate</span>
{/snippet}

{#snippet filterOverlay(target: string)}
  <div class="absolute inset-0 {loading === target ? 'flex' : 'hidden'} items-center justify-center rounded-lg bg-white/70 dark:bg-base/70">
    <div class="inline-flex items-center justify-center gap-2 text-[13px] text-neutral-500 dark:text-fg-dim" role="status" aria-live="polite" aria-label="Loading analytics">
      <svg class="loading-indicator shrink-0 animate-spin size-3" viewBox="0 0 24 24" fill="none" aria-hidden="true">
        <circle class="opacity-20" cx="12" cy="12" r="9" stroke="currentColor" stroke-width="2" />
        <path class="opacity-80" d="M21 12a9 9 0 0 0-9-9" stroke="currentColor" stroke-width="2" stroke-linecap="round" />
      </svg>
    </div>
  </div>
{/snippet}

{#snippet appRowBody(row: AppRow)}
  <span class="flex min-w-0 flex-1 items-baseline gap-1.5">
    <span class="truncate text-[12px] text-black dark:text-fg">{row.name}</span>
    {#if row.isService}
      <span class="shrink-0 text-[10px] font-medium tracking-wide text-neutral-400 uppercase dark:text-fg-faint">Service</span>
    {/if}
    {#if row.domain}
      <span class="hidden truncate text-[11px] text-neutral-400 sm:inline dark:text-fg-faint">{row.domain}</span>
    {/if}
  </span>
  <div class="hidden h-1 w-16 shrink-0 overflow-hidden rounded-full bg-neutral-100 sm:block dark:bg-white/[0.06]">
    <div class="h-full rounded-full bg-[var(--chart-status-3xx)]" style="width: {Math.min(100, round1((row.requests / maxAppRequests) * 100))}%;"></div>
  </div>
  <span class="w-16 shrink-0 text-right text-[12px] font-medium tabular-nums text-black dark:text-fg" title="{numberFormat(row.requests)} requests">{compactNumber(row.requests)}</span>
  <span class="hidden w-16 shrink-0 text-right text-[11px] tabular-nums text-neutral-400 sm:inline dark:text-fg-faint">{formatBytes(row.bandwidth)}</span>
  {#if row.link}
    <svg class="size-3.5 shrink-0 text-neutral-300 dark:text-fg-faint" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true">
      <path stroke-linecap="round" stroke-linejoin="round" d="M9 5l7 7-7 7" />
    </svg>
  {:else}
    <span class="size-3.5 shrink-0" aria-hidden="true"></span>
  {/if}
{/snippet}

{#if !data}
  <Placeholder scoped={!!scopedServerUuid} {hideSkeleton} />
{:else}
  {@const overview = data.overview}
  {@const chartId = data.chartId}
  <div class="flex w-full min-w-0 flex-col gap-6">
    <div class="flex flex-col gap-4">
      {#if !scopedServerUuid}
        <div class="min-w-0">
          <h1 class="min-w-0 text-[24px]! leading-7! font-semibold! tracking-tight!">Analytics</h1>
          <p class="mt-1 text-[13px] text-neutral-500 dark:text-fg-dim">Request traffic across every application and server, reported by Sentinel.</p>
        </div>
      {/if}

      {#if data.hasServers && overview}
        <div class="flex flex-wrap items-center gap-2">
          {#if !scopedServerUuid}
            <div class="relative w-full transition-opacity sm:w-52 {loading === 'serverUuid' ? 'pointer-events-none opacity-60' : ''}">
              <Listbox id="serverUuid" bind:value={serverUuid} options={[{ value: '', label: 'All servers' }, ...data.serverOptions]} placeholder="All servers" onchange={() => load('serverUuid')} />
              {@render filterOverlay('serverUuid')}
            </div>
          {/if}
          {#key serverUuid}
            <div class="relative w-full transition-opacity sm:w-52 {loading === 'appUuid' ? 'pointer-events-none opacity-60' : ''}">
              <Listbox id="appUuid" bind:value={appUuid} options={[{ value: '', label: 'All resources' }, ...data.appOptions]} placeholder="All resources" onchange={() => load('appUuid')} />
              {@render filterOverlay('appUuid')}
            </div>
          {/key}

          <div class="flex w-full items-center gap-2 sm:ml-auto sm:w-auto">
            <div class="relative w-full transition-opacity sm:w-44 {loading === 'setRange' ? 'pointer-events-none opacity-60' : ''}">
              <Listbox id="range" bind:value={range} options={rangeOptions} onchange={() => load('setRange')} />
              {@render filterOverlay('setRange')}
            </div>
            <div>
              <button
                type="button"
                onclick={() => setLive(!live)}
                aria-pressed={live ? 'true' : 'false'}
                title="Turn live refresh on or off (updates every 60 seconds)"
                class="inline-flex h-7 items-center gap-1.5 rounded-md px-2.5 text-[12px] font-medium ring-1 ring-neutral-200 transition-colors dark:ring-white/[0.08] {live ? 'bg-white text-black shadow-sm dark:bg-white/[0.09] dark:text-fg' : 'text-neutral-500 hover:text-black dark:text-fg-faint dark:hover:text-fg'}"
              >
                {#if live}
                  <span class="relative flex h-1.5 w-1.5 shrink-0" aria-hidden="true">
                    <span class="absolute inline-flex h-full w-full rounded-full bg-emerald-500 opacity-75 motion-safe:animate-ping"></span>
                    <span class="relative inline-flex h-1.5 w-1.5 rounded-full bg-emerald-500"></span>
                  </span>
                {:else}
                  <svg class="h-3 w-3 shrink-0" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
                    <path d="M8 5.5v13a1 1 0 0 0 1.5.86l11-6.5a1 1 0 0 0 0-1.72l-11-6.5A1 1 0 0 0 8 5.5Z" />
                  </svg>
                {/if}
                <span>{live ? 'Live' : 'Paused'}</span>
              </button>
            </div>
          </div>
        </div>
      {/if}
    </div>

    {#if !scopedServerUuid && data.eligibleDisabledServers.length > 0 && !dismissed}
      {@const eligible = data.eligibleDisabledServers}
      <div class="flex items-start gap-3 rounded-xl border border-neutral-200 bg-white px-4 py-3 shadow-sm dark:border-white/[0.08] dark:bg-white/[0.025]">
        <div class="min-w-0 flex-1">
          <p class="text-[12px] font-semibold text-black dark:text-fg">
            {eligible.length === 1 ? '1 server can start collecting traffic analytics' : `${eligible.length} servers can start collecting traffic analytics`}
          </p>
          <p class="mt-0.5 text-[11px] text-neutral-500 dark:text-fg-dim">
            Enabling regenerates the proxy config and restarts the proxy + Sentinel (a brief blip). Works with Traefik &amp; Caddy.
          </p>
        </div>
        <div class="flex shrink-0 items-center gap-2">
          {#if eligible.length === 1}
            <a class="button" href="/server/{eligible[0].uuid}/analytics" use:link>Set up on {strLimit(eligible[0].name, 16)}</a>
          {:else}
            <a class="button" href="/servers" use:link>View servers</a>
          {/if}
          <button type="button" title="Dismiss" aria-label="Dismiss" onclick={dismissNudge} class="flex h-6 w-6 items-center justify-center rounded-md text-neutral-400 transition-colors hover:text-black dark:text-fg-faint dark:hover:text-fg">
            <svg class="h-3.5 w-3.5" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true">
              <path stroke-linecap="round" stroke-linejoin="round" d="M6 6l12 12M6 18L18 6" />
            </svg>
          </button>
        </div>
      </div>
    {/if}

    {#if !data.hasServers}
      {#if !scopedServerUuid}
        <Empty size="sm" title="Traffic analytics is not enabled" description="Enable traffic analytics on a server to see request analytics here." iconName="analytics">
          {#snippet contents()}
            <a class="button" href="/servers" use:link>View servers</a>
          {/snippet}
        </Empty>
      {/if}
    {:else if !overview}
      <Empty size="sm" title="No analytics data yet" description="We could not load traffic analytics for the selected filters and range. Try a different range or check back shortly." iconName="analytics" />
    {:else}
      <SettingsSection id="analytics-overview-section" title="Overview" description="Aggregate request volume for the selected filters and range.">
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
                {@render approxBadge('Summed across servers or services; visitors seen on more than one may be double-counted.')}
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
                {@render approxBadge('Highest p95 latency across servers or services; not a true merged percentile.')}
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

      <div class="grid grid-cols-1 gap-6 {data.appUuid === '' ? 'lg:grid-cols-2' : ''}">
        {#if data.appUuid === ''}
          <SettingsSection id="analytics-hosts-section" title="Top hosts" description="Served hostnames ranked by request volume." flush>
            <HostsList hosts={data.topHosts ?? []} />
          </SettingsSection>

          <SettingsSection id="analytics-apps-section" title="Top applications" description="Applications and services ranked by request volume. Open one for its analytics." flush>
            {#if topApps.length === 0}
              <Empty size="sm" title="No application data" description="No per-application requests were recorded for the selected range." iconName="unordered-list" />
            {:else}
              <TrafficList rows={appRows}>
                {#snippet header(table)}
                  <SortHeader {table} column="name" label="Application / service" text class="min-w-0 flex-1" />
                  <span class="hidden w-16 shrink-0 sm:inline" aria-hidden="true"></span>
                  <SortHeader {table} column="requests" label="Requests" class="w-16 shrink-0" />
                  <SortHeader {table} column="bytesOut" label="Bandwidth" class="hidden w-16 shrink-0 sm:inline-flex" title="Total response data sent" />
                  <span class="size-3.5 shrink-0" aria-hidden="true"></span>
                {/snippet}
                {#snippet row(item)}
                  {#if item.link}
                    <a href={item.link} use:link class={rowClass}>{@render appRowBody(item)}</a>
                  {:else}
                    <div class={rowClass}>{@render appRowBody(item)}</div>
                  {/if}
                {/snippet}
              </TrafficList>
            {/if}
          </SettingsSection>
        {/if}
      </div>

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
