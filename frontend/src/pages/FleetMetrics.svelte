<script lang="ts">
  // Port of upstream livewire/fleet/metrics.blade.php with Fleet\Metrics and its placeholder.
  import { onDestroy, tick } from 'svelte';
  import Empty from '../components/Empty.svelte';
  import Listbox from '../components/Listbox.svelte';
  import LiveToggle from '../components/LiveToggle.svelte';
  import SettingsSection from '../components/SettingsSection.svelte';
  import SortHeader from '../components/analytics/SortHeader.svelte';
  import Sparkline from '../components/analytics/Sparkline.svelte';
  import { compactNumber, formatBytes as formatWholeBytes, numberFormat, round1, strLimit } from '../lib/analyticsApi';
  import { chartTooltipTime, loadApexCharts, renderChartLegend, syncChartTooltips, textColor, type ApexChart } from '../lib/charts';
  import { enableFleetMetrics, fetchFleet, pressureBar, pressureLevel, pressureText, rankContainers, type ChartData, type FleetContainer, type FleetData, type FleetRow, type Point } from '../lib/metricsApi.svelte';
  import { link, router } from '../lib/router.svelte';
  import { failure } from '../lib/serverApi';
  import { notify } from '../lib/toast.svelte';
  import { TrafficTable, type TableRow } from '../lib/trafficTable.svelte';

  const chartId = 'fleet-metrics';
  const rangeOptions = [
    { value: '24h', label: 'Last 24 hours' },
    { value: '7d', label: 'Last 7 days' },
    { value: '30d', label: 'Last 30 days' },
  ];
  const tile = 'flex flex-col bg-[var(--coollabs-base)] px-4 py-3';
  const tileLabel = 'text-[11px] font-medium tracking-wide text-neutral-500 uppercase dark:text-fg-dim';
  const tileValue = 'mt-1 text-xl font-semibold tabular-nums text-black dark:text-fg';
  const skeleton = 'animate-pulse rounded-md bg-neutral-200/80 dark:bg-white/[0.06]';
  const widths = ['w-3/5', 'w-2/5', 'w-1/2', 'w-3/4', 'w-1/3', 'w-2/3'];
  const sections: [string, string][] = [
    ['cpu', 'CPU'],
    ['memory', 'Memory used'],
    ['network', 'Network throughput'],
    ['load', 'Load average'],
    ['disk', 'Disk usage'],
  ];
  const containerMetrics: [keyof FleetContainer, string][] = [
    ['cpu', 'CPU'],
    ['memUsed', 'Memory'],
    ['diskBytes', 'Disk'],
    ['net', 'Network'],
  ];

  let data = $state<FleetData | null>(null);
  let range = $state(router.query('range') ?? '24h');
  let serverUuid = $state(router.query('server') ?? '');
  let cmetric = $state(router.query('cmetric') ?? 'cpu');
  let loading = $state<string | null>(null);
  let live = $state(false);
  let enabling = $state(false);
  let sequence = 0;
  let poll: ReturnType<typeof setInterval> | undefined;
  let charts: Record<string, ApexChart> = {};
  let chartsFor = '';

  /** `formatBytes(?int)`: PHP drops the fraction of a rate on the way in. */
  const formatBytes = (value: number) => formatWholeBytes(Math.trunc(value));

  const scoped = $derived(serverUuid !== '');
  const cpuHelper = $derived(
    scoped
      ? 'Average usage of all CPU cores on this server. 100% means that all cores are fully busy.'
      : 'Average CPU usage of the servers. The value of each server is the average of all its CPU cores, so 100% means that all cores are fully busy. Servers are not weighted by their number of cores.',
  );
  const kpis = $derived(data?.kpis);
  const topContainers = $derived(rankContainers(data?.containersRaw ?? [], cmetric));
  const serverOptions = $derived([{ value: '', label: 'All servers' }, ...(data?.serverOptions ?? [])]);

  const serverTable = new TrafficTable<FleetRow>();
  serverTable.per = Number.MAX_SAFE_INTEGER;
  serverTable.key = 'cpu';
  const containerTable = new TrafficTable<{ row: FleetContainer; iteration: number }>();
  containerTable.per = Number.MAX_SAFE_INTEGER;
  containerTable.key = 'cpu';

  const memPercent = (row: FleetRow) => (row.memTotal ? round1(((row.memUsed ?? 0) / row.memTotal) * 100) : null);
  $effect.pre(() => {
    const rows = [...(data?.serverRows ?? [])].sort((a, b) => (b.cpu ?? -1) - (a.cpu ?? -1));
    serverTable.rows = rows.map(
      (row): TableRow<FleetRow> => ({
        key: row.uuid,
        // Servers without metrics have no values, so they stay last in each sort.
        pinned: !row.reporting,
        sort: {
          name: row.name,
          cpu: row.cpu,
          memory: memPercent(row),
          disk: row.diskPercent,
          load: row.load1,
          network: row.netRx === null ? null : row.netRx + (row.netTx ?? 0),
          containers: row.containers,
        },
        data: row,
      }),
    );
    containerTable.rows = topContainers.map((row, index) => ({
      key: `${row.serverUuid}-${row.id}`,
      sort: { name: row.name, server: row.server, cpu: row.cpu ?? 0, memUsed: row.memUsed ?? 0, diskBytes: row.diskBytes ?? 0, net: row.net ?? 0 },
      data: { row, iteration: index + 1 },
    }));
  });

  /** `#[Url]`: the filters are kept in the address, without their defaults. */
  function syncUrl() {
    const query = new URLSearchParams(window.location.search);
    const set = (key: string, value: string, fallback: string) => {
      if (value && value !== fallback) query.set(key, value);
      else query.delete(key);
    };
    set('range', range, '24h');
    set('server', serverUuid, '');
    set('cmetric', cmetric, 'cpu');
    const qs = query.toString();
    const search = qs ? `?${qs}` : '';
    history.replaceState(null, '', `${window.location.pathname}${search}`);
    router.search = search;
  }

  const fmtPercent = (v: number) => `${Number(Number(v).toFixed(1))}%`;
  const fmtNumber = (v: number) => Number(Number(v).toFixed(2)).toLocaleString();
  const fmtBytes = (v: number) => {
    const units = ['B', 'KB', 'MB', 'GB', 'TB'];
    let amount = Number(v) || 0;
    let unit = 0;
    while (amount >= 1024 && unit < units.length - 1) {
      amount /= 1024;
      unit++;
    }
    return `${Number(amount.toFixed(unit === 0 ? 0 : 2))} ${units[unit]}`;
  };
  const fmtRate = (v: number) => `${fmtBytes(v)}/s`;

  const customTooltip =
    (formatter: (v: number) => string) =>
    ({ series, dataPointIndex, w }: ApexChart) => {
      const timestamp = w.globals.seriesX[0]?.[dataPointIndex];
      let rows = '';
      series.forEach((s: number[], i: number) => {
        const value = s[dataPointIndex];
        if (value === null || value === undefined) return;
        rows += `<div class="apexcharts-tooltip-custom-value">${w.globals.seriesNames[i]}: <span class="apexcharts-tooltip-value-bold">${formatter(value)}</span></div>`;
      });
      return `<div class="apexcharts-tooltip-custom">${rows}
                        ${chartTooltipTime(timestamp)}
                    </div>`;
    };

  const baseOptions = (series: { name: string; data: Point[] }[], colors: string[], formatter: (v: number) => string) => ({
    chart: { height: 240, type: 'area', toolbar: { show: false }, zoom: { enabled: false }, background: 'transparent', animations: { enabled: true } },
    series,
    colors,
    stroke: { curve: 'smooth', width: 2 },
    fill: { type: 'gradient', gradient: { opacityFrom: 0.28, opacityTo: 0.02, stops: [0, 90, 100] } },
    dataLabels: { enabled: false },
    grid: { borderColor: 'rgba(128, 128, 128, 0.14)', strokeDashArray: 4 },
    legend: { show: false },
    xaxis: { type: 'datetime', labels: { datetimeUTC: true, style: { colors: textColor } } },
    yaxis: { min: 0, max: (max: number) => (max > 0 ? max * 1.2 : 1), forceNiceScale: true, tickAmount: 4, labels: { style: { colors: textColor }, formatter } },
    noData: { text: 'No data for this range', style: { color: textColor } },
    tooltip: { shared: true, intersect: false, marker: { show: false }, custom: customTooltip(formatter) },
  });

  const build = (d: Partial<ChartData>) => ({
    cpu: { series: [{ name: 'CPU %', data: d.cpu || [] }], colors: ['#3b82f6'], formatter: fmtPercent },
    memory: { series: [{ name: 'Memory', data: d.memory || [] }], colors: ['#8b5cf6'], formatter: fmtBytes },
    network: {
      series: [
        { name: 'RX', data: d.networkRx || [] },
        { name: 'TX', data: d.networkTx || [] },
      ],
      colors: ['#10b981', '#f59e0b'],
      formatter: fmtRate,
    },
    load: { series: [{ name: 'Load', data: d.load || [] }], colors: ['#14b8a6'], formatter: fmtNumber },
    disk: { series: [{ name: 'Disk %', data: d.disk || [] }], colors: ['#ef4444'], formatter: fmtPercent },
  });
  type Specs = ReturnType<typeof build>;

  function renderLegends(specs: Specs) {
    for (const [key, spec] of Object.entries(specs)) {
      renderChartLegend(
        document.getElementById(`fleet-chart-${key}-legend`),
        spec.series.map((series, i) => ({ name: series.name, color: spec.colors[i], data: series.data, formatter: spec.formatter })),
      );
    }
  }

  /** The `@script` block and the `refreshChartData-fleet-metrics` listener. */
  async function drawCharts(d: FleetData) {
    await tick();
    const specs = build(d.chartData ?? {});
    if (chartsFor !== chartId) {
      chartsFor = chartId;
      // Defer to the next frame so the chart containers are laid out before ApexCharts measures them.
      requestAnimationFrame(() => {
        for (const [key, spec] of Object.entries(specs)) {
          const el = document.getElementById(`fleet-chart-${key}`);
          if (!el) continue;
          const chart = new window.ApexCharts(el, baseOptions(spec.series, spec.colors, spec.formatter));
          chart.render();
          charts[key] = chart;
        }
        syncChartTooltips(chartId, Object.values(charts));
        renderLegends(specs);
      });
      return;
    }
    for (const [key, chart] of Object.entries(charts)) chart.updateSeries(specs[key as keyof Specs].series);
    renderLegends(specs);
  }

  /** `loadData`. */
  async function load(target: string | null = null) {
    const mine = ++sequence;
    loading = target;
    try {
      const [next] = await Promise.all([fetchFleet({ range, server: serverUuid, cmetric }), loadApexCharts()]);
      if (mine !== sequence) return;
      data = next;
      range = next.range;
      serverUuid = next.serverUuid;
      cmetric = next.containerMetric;
      syncUrl();
      if (next.hasServers) void drawCharts(next);
    } catch (e) {
      failure(e);
    } finally {
      if (mine === sequence) loading = null;
    }
  }
  void load();

  async function enableMetrics() {
    enabling = true;
    try {
      const answer = await enableFleetMetrics();
      await load();
      if (answer.success) notify.success(answer.success);
      if (answer.error) notify.failure(answer.error);
    } catch (e) {
      failure(e);
    } finally {
      enabling = false;
    }
  }

  // `wire:poll.60s="loadData"`.
  $effect(() => {
    clearInterval(poll);
    poll = live ? setInterval(() => void load(), 60000) : undefined;
  });
  onDestroy(() => {
    clearInterval(poll);
    Object.values(charts).forEach((chart) => chart.destroy());
    charts = {};
  });

  const hottestValue = (c: FleetContainer) => {
    if (cmetric === 'memory') return formatBytes(c.memUsed ?? 0);
    if (cmetric === 'disk') return formatBytes(c.diskBytes ?? 0);
    if (cmetric === 'network') return `${formatBytes(c.net ?? 0)}/s`;
    return `${Math.round(c.cpu ?? 0)}%`;
  };
  const maxMetric = (key: keyof FleetContainer) => Math.max(1, ...topContainers.map((c) => Number(c[key] ?? 0)));
  const metricDisplay = (c: FleetContainer, key: keyof FleetContainer) => {
    const value = Number(c[key] ?? 0);
    if (key === 'cpu') return `${Math.round(value)}%`;
    if (key === 'net') return `${formatBytes(value)}/s`;
    return formatBytes(value);
  };
</script>

<svelte:head><title>Metrics | coolify-rs</title></svelte:head>

{#snippet usageCell(percent: number | null, kind: string)}
  {@const level = pressureLevel(percent, kind)}
  <div class="flex items-center gap-2">
    <span class="tabular-nums {pressureText[level]}">{percent === null ? '-' : `${Math.round(percent)}%`}</span>
    <div class="hidden h-1 w-16 shrink-0 overflow-hidden rounded-full bg-neutral-100 sm:block dark:bg-white/[0.06]">
      <div class="h-full rounded-full transition-[width] duration-700 ease-out {pressureBar[level]}" style="width: {percent === null ? 0 : Math.min(100, round1(percent))}%;"></div>
    </div>
  </div>
{/snippet}

{#snippet filterOverlay(target: string)}
  <div class="absolute inset-0 {loading === target ? 'flex' : 'hidden'} items-center justify-center rounded-lg bg-white/70 dark:bg-base/70">
    <div class="inline-flex items-center justify-center gap-2 text-[13px] text-neutral-500 dark:text-fg-dim" role="status" aria-live="polite" aria-label="Loading metrics">
      <svg class="loading-indicator shrink-0 animate-spin size-3" viewBox="0 0 24 24" fill="none" aria-hidden="true">
        <circle class="opacity-20" cx="12" cy="12" r="9" stroke="currentColor" stroke-width="2" />
        <path class="opacity-80" d="M21 12a9 9 0 0 0-9-9" stroke="currentColor" stroke-width="2" stroke-linecap="round" />
      </svg>
    </div>
  </div>
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

<div class="flex w-full min-w-0 flex-col gap-6">
  <div class="flex flex-col gap-4">
    <div class="min-w-0">
      <h1 class="min-w-0 text-[24px]! leading-7! font-semibold! tracking-tight!">Metrics</h1>
      <p class="mt-1 text-[13px] text-neutral-500 dark:text-fg-dim">Resource usage across every server, reported by Sentinel.</p>
    </div>

    {#if !data}
      <div class="flex flex-wrap items-center gap-2">
        <div aria-hidden="true" class="{skeleton} h-9 w-full rounded-lg sm:w-52"></div>
        <div aria-hidden="true" class="{skeleton} h-9 w-52 rounded-lg sm:ml-auto"></div>
      </div>
    {:else if data.hasServers}
      <div class="flex flex-wrap items-center gap-2">
        <div class="relative w-full transition-opacity sm:w-52 {loading === 'serverUuid' ? 'pointer-events-none opacity-60' : ''}">
          <Listbox id="serverUuid" bind:value={serverUuid} options={serverOptions} placeholder="All servers" onchange={() => void load('serverUuid')} />
          {@render filterOverlay('serverUuid')}
        </div>

        <div class="flex w-full items-center gap-2 sm:ml-auto sm:w-auto">
          <div class="relative w-full transition-opacity sm:w-44 {loading === 'setRange' ? 'pointer-events-none opacity-60' : ''}">
            <Listbox id="range" bind:value={range} options={rangeOptions} onchange={() => void load('setRange')} />
            {@render filterOverlay('setRange')}
          </div>
          <LiveToggle
            bind:live
            storageKey="metrics-live"
            title="Turn live refresh on or off (updates every 60 seconds)"
            onchange={(next) => {
              if (next) void load();
            }}
          />
        </div>
      </div>
    {/if}
  </div>

  {#if !data}
    <SettingsSection id="metrics-overview-section" title="Overview">
      <div class="grid grid-cols-2 sm:grid-cols-3 lg:grid-cols-6 gap-px overflow-hidden rounded-lg bg-neutral-200 dark:bg-white/[0.07]">
        {#each ['Servers online', 'Fleet CPU', 'Fleet memory', 'Fleet disk', 'Fleet network', 'Containers'] as label (label)}
          <div class={tile}>
            <span class={tileLabel}>{label}</span>
            <div aria-hidden="true" class="{skeleton} mt-2 h-6 w-20"></div>
            <div class="mt-auto pt-3"><div aria-hidden="true" class="{skeleton} h-9 w-full rounded"></div></div>
          </div>
        {/each}
      </div>
    </SettingsSection>
    <div class="grid gap-6 lg:grid-cols-2">
      {#each ['CPU', 'Memory used', 'Network throughput', 'Load average', 'Disk usage'] as label (label)}
        <SettingsSection title={label} class={label === 'CPU' ? 'lg:col-span-2' : ''}>
          <div aria-hidden="true" class="{skeleton} h-[240px] w-full rounded-lg"></div>
        </SettingsSection>
      {/each}
    </div>
    <SettingsSection title="Servers" flush>{@render skeletonTable(4)}</SettingsSection>
    <SettingsSection title="Hottest containers" flush>{@render skeletonTable(6)}</SettingsSection>
  {:else if !data.hasServers}
    <Empty
      size="sm"
      title="No server metrics yet"
      description={data.enableableServers.length === 0 ? 'Enable metrics on a server to see fleet resource usage here.' : 'Enable metrics to collect CPU, memory, disk, and network history. This starts Sentinel; your applications keep running.'}
      iconName="graph"
    >
      {#snippet contents()}
        <div class="flex flex-wrap items-center justify-center gap-3">
          {#if data && data.enableableServers.length > 0}
            <button type="button" class="button" {...{ ishighlighted: '' }} disabled={enabling} onclick={enableMetrics}>
              {data.enableableServers.length === 1 ? `Enable metrics on ${strLimit(data.enableableServers[0], 24)}` : `Enable metrics on ${data.enableableServers.length} servers`}
            </button>
          {/if}
          <a class="button" href="/servers" use:link>View servers</a>
        </div>
      {/snippet}
    </Empty>
  {:else}
    {@const sTotal = Math.max(1, kpis?.serversTotal ?? 0)}
    {@const sOnline = kpis?.serversOnline ?? 0}
    {@const sReporting = Math.min(sOnline, kpis?.serversReporting ?? 0)}
    {@const sAttention = Math.min(sReporting, kpis?.needsAttention ?? 0)}
    {@const sHealthy = Math.max(0, sReporting - sAttention)}
    {@const sNoMetrics = sOnline - sReporting}
    {@const sOffline = Math.max(0, sTotal - sOnline)}
    {@const categories = data.chartData?.sparkCategories ?? []}
    <SettingsSection id="metrics-overview-section" title="Overview" description={scoped ? 'Current resource usage for the selected server.' : 'Current resource usage across the selected servers.'}>
      <div class="grid grid-cols-2 gap-px overflow-hidden rounded-lg bg-neutral-200 sm:grid-cols-3 lg:grid-cols-6 dark:bg-white/[0.07]">
        <div class={tile}>
          <span class={tileLabel}>Servers online</span>
          <span class={tileValue}>{kpis?.serversOnline ?? 0}<span class="text-neutral-400 dark:text-fg-faint">/{kpis?.serversTotal ?? 0}</span></span>
          <div class="mt-auto pt-3">
            <div class="flex h-9 flex-col justify-center gap-1.5">
              <div class="flex h-2 overflow-hidden rounded-full bg-neutral-200 dark:bg-white/[0.08] [&>div]:transition-[width] [&>div]:duration-700 [&>div]:ease-out">
                {#if sHealthy > 0}<div class="bg-emerald-500" style="width: {(sHealthy / sTotal) * 100}%"></div>{/if}
                {#if sAttention > 0}<div class="bg-orange-400 dark:bg-warning" style="width: {(sAttention / sTotal) * 100}%"></div>{/if}
                {#if sNoMetrics > 0}<div class="bg-neutral-400 dark:bg-white/30" style="width: {(sNoMetrics / sTotal) * 100}%"></div>{/if}
                {#if sOffline > 0}<div class="bg-red-500 dark:bg-red-400" style="width: {(sOffline / sTotal) * 100}%"></div>{/if}
              </div>
              <div class="flex flex-wrap items-center gap-x-2 text-[10px] text-neutral-500 dark:text-fg-dim">
                <span>{sHealthy} healthy</span>
                {#if sAttention > 0}<span class="text-orange-500 dark:text-warning">{sAttention} attention</span>{/if}
                {#if sNoMetrics > 0}<span title="coolify-rs can reach these servers, but Sentinel did not return current metrics.">{sNoMetrics} no metrics</span>{/if}
                {#if sOffline > 0}<span class="text-red-500 dark:text-red-400">{sOffline} offline</span>{/if}
              </div>
            </div>
          </div>
        </div>
        <div class={tile}>
          <span class="flex items-center {tileLabel}">
            {scoped ? 'CPU' : 'Fleet CPU'}
            {#if kpis?.cpuApproximate}
              <span class="ml-1.5 cursor-help text-neutral-400 dark:text-fg-faint" title="Averaged across servers; not core-weighted">~</span>
            {/if}
          </span>
          <span class={tileValue}>{Math.round(kpis?.cpuAvg ?? 0)}%</span>
          <div class="mt-auto pt-3">
            <Sparkline id="{chartId}-spark-cpu" values={data.chartData?.cpuSpark ?? []} {categories} color="#3b82f6" label={scoped ? 'CPU' : 'Fleet CPU'} format="percent" />
          </div>
        </div>
        <div class={tile}>
          <span class={tileLabel}>{scoped ? 'Memory' : 'Fleet memory'}</span>
          <span class={tileValue}>{Math.round(kpis?.memPercent ?? 0)}%</span>
          <div class="mt-auto pt-3">
            <Sparkline id="{chartId}-spark-memory" values={data.chartData?.memSpark ?? []} {categories} color="#8b5cf6" label="Memory used" format="bytes" />
          </div>
        </div>
        <div class={tile}>
          <span class={tileLabel}>{scoped ? 'Disk' : 'Fleet disk'}</span>
          <span class={tileValue}>{Math.round(kpis?.diskPercent ?? 0)}%</span>
          <div class="mt-auto pt-3">
            <Sparkline id="{chartId}-spark-disk" values={data.chartData?.diskSpark ?? []} {categories} color="#ef4444" label={scoped ? 'Disk' : 'Fleet disk'} format="percent" />
          </div>
        </div>
        <div class={tile}>
          <span class={tileLabel}>{scoped ? 'Network' : 'Fleet network'}</span>
          <span class={tileValue}>{formatBytes((kpis?.netRx ?? 0) + (kpis?.netTx ?? 0))}/s</span>
          <div class="mt-auto pt-3">
            <Sparkline id="{chartId}-spark-network" values={data.chartData?.netSpark ?? []} {categories} color="#10b981" label="Network" format="bytes" />
          </div>
        </div>
        <div class="col-span-2 flex flex-col bg-[var(--coollabs-base)] px-4 py-3 sm:col-span-1">
          <span class={tileLabel}>Containers</span>
          <span class={tileValue}>{compactNumber(kpis?.containers ?? 0)}</span>
          <div class="mt-auto pt-3">
            <div class="flex h-9 flex-col justify-center">
              {#if topContainers.length > 0}
                <span class="truncate text-[13px] font-medium text-black dark:text-fg">{topContainers[0].name}</span>
                <span class="text-[10px] text-neutral-500 dark:text-fg-dim">hottest, {hottestValue(topContainers[0])}</span>
              {/if}
            </div>
          </div>
        </div>
      </div>
    </SettingsSection>

    <div class="grid gap-6 lg:grid-cols-2">
      {#each sections as [key, label] (key)}
        <SettingsSection title={label} description={key === 'cpu' ? cpuHelper : undefined} class={key === 'cpu' ? 'lg:col-span-2' : ''}>
          {#snippet actions()}
            <div id="fleet-chart-{key}-legend" class="flex flex-wrap items-center gap-x-4 gap-y-1 text-xs"></div>
          {/snippet}
          <div>
            <div id="fleet-chart-{key}" class="min-h-[240px] w-full"></div>
          </div>
        </SettingsSection>
      {/each}
    </div>

    <SettingsSection id="metrics-servers-section" title="Servers" flush description="Per-server resource pressure. Select a column header to sort.">
      <div class="data-table">
        <div class="data-table-header fleet-servers-table-grid border-b-0!">
          <SortHeader table={serverTable} column="name" label="Server" text class="justify-self-start" />
          {#each [['cpu', 'CPU'], ['memory', 'Memory'], ['disk', 'Disk'], ['load', 'Load'], ['network', 'Network']] as [column, label] (column)}
            <SortHeader table={serverTable} {column} {label} text dir="desc" class="justify-self-start" title={column === 'cpu' ? 'Sort by CPU. 100% means that all cores of the server are fully busy.' : undefined} />
          {/each}
          <SortHeader table={serverTable} column="containers" label="Containers" class="justify-self-end" />
        </div>
        <div class="flex flex-col">
          {#each serverTable.visible as item (item.key)}
            {@const row = item.data}
            <div class="data-table-row fleet-servers-table-grid border-t border-neutral-200 dark:border-white/[0.07]">
              <div class="flex min-w-0 items-center gap-2">
                <a href="/server/{row.uuid}/metrics" use:link class="min-w-0 truncate text-[13px] font-medium text-black hover:underline dark:text-fg">{row.name}</a>
                {#if !row.online}
                  <span class="table-badge table-badge-warning shrink-0">Offline</span>
                {:else if !row.reporting}
                  <span class="table-badge shrink-0" title="coolify-rs can reach this server, but Sentinel did not return current metrics.">No metrics</span>
                {/if}
              </div>
              <div>{@render usageCell(row.cpu, 'cpu')}</div>
              <div>{@render usageCell(memPercent(row), 'memory')}</div>
              <div>{@render usageCell(row.diskPercent, 'disk')}</div>
              <div class="text-[13px] tabular-nums text-neutral-600 dark:text-fg-dim">{row.load1 === null ? '-' : numberFormat(row.load1, 2)}</div>
              <div class="text-[13px] tabular-nums text-neutral-600 dark:text-fg-dim">{row.netRx === null ? '-' : `${formatBytes(row.netRx + (row.netTx ?? 0))}/s`}</div>
              <div class="text-right text-[13px] tabular-nums text-neutral-600 dark:text-fg-dim">{row.containers ?? '-'}</div>
            </div>
          {/each}
        </div>
      </div>
    </SettingsSection>

    <SettingsSection id="metrics-containers-section" title="Hottest containers" flush description={scoped ? 'Busiest containers on this server, sortable by resource usage.' : 'Busiest containers across the fleet, sortable by resource usage.'}>
      {#if topContainers.length === 0}
        <div class="p-4">
          <Empty size="sm" title="No container metrics" description="Container metrics need a Sentinel build with the bulk endpoints." />
        </div>
      {:else}
        <div class="data-table">
          <div class="data-table-header fleet-containers-table-grid border-b-0!">
            <span class="text-center">#</span>
            <SortHeader table={containerTable} column="name" label="Container" text class="justify-self-start" />
            <SortHeader table={containerTable} column="server" label="Server" text class="justify-self-start" />
            {#each containerMetrics as [key, label] (key)}
              <SortHeader table={containerTable} column={key} {label} class="justify-self-end" title={key === 'cpu' ? 'Sort by CPU. 100% means one CPU core is fully busy, so a container can go above 100%.' : undefined} />
            {/each}
          </div>
          <div class="flex flex-col">
            {#each containerTable.visible as item (item.key)}
              {@const c = item.data.row}
              <div class="data-table-row fleet-containers-table-grid border-t border-neutral-200 dark:border-white/[0.07]">
                <div class="self-center text-center text-[12px] tabular-nums text-neutral-400 dark:text-fg-faint">{item.data.iteration}</div>
                <div class="flex min-w-0 flex-col">
                  {#if c.link}
                    <a href={c.link} use:link class="truncate text-[13px] font-medium text-black hover:underline dark:text-fg">{c.name}</a>
                  {:else}
                    <span class="truncate text-[13px] font-medium text-black dark:text-fg">{c.name}</span>
                  {/if}
                  {#if c.image}
                    <span class="truncate font-mono text-[11px] text-neutral-400 dark:text-fg-faint">{c.image}</span>
                  {/if}
                </div>
                <div class="min-w-0 self-center truncate text-[12px] text-neutral-500 dark:text-fg-dim">{c.server}</div>
                {#each containerMetrics as [key] (key)}
                  {@const pct = Math.min(100, round1((Number(c[key] ?? 0) / maxMetric(key)) * 100))}
                  <div class="flex items-center justify-end gap-2.5 self-center">
                    <div class="hidden h-1.5 w-10 shrink-0 overflow-hidden rounded-full bg-neutral-100 xl:block dark:bg-white/[0.06]" title="{pct}% of the busiest">
                      <div class="h-full rounded-full bg-[var(--chart-status-3xx)] transition-[width] duration-700 ease-out" style="width: {pct}%"></div>
                    </div>
                    <span class="whitespace-nowrap text-right text-[13px] tabular-nums text-black dark:text-fg">{metricDisplay(c, key)}</span>
                  </div>
                {/each}
              </div>
            {/each}
          </div>
        </div>
      {/if}
    </SettingsSection>
  {/if}
</div>
