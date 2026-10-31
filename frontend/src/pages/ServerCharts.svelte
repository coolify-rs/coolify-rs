<script lang="ts">
  // Port of upstream livewire/server/charts.blade.php.
  import { onDestroy, tick } from 'svelte';
  import Empty from '../components/Empty.svelte';
  import FormInput from '../components/FormInput.svelte';
  import Listbox from '../components/Listbox.svelte';
  import ServerFrame from '../components/ServerFrame.svelte';
  import SettingsSection from '../components/SettingsSection.svelte';
  import UnsavedBar from '../components/UnsavedBar.svelte';
  import { ApiError } from '../lib/api';
  import { get, post, failure } from '../lib/serverApi';
  import { link } from '../lib/router.svelte';
  import { session } from '../lib/session.svelte';
  import { notify } from '../lib/toast.svelte';
  import {
    chartTooltipTime,
    cpuColor,
    formatNumber,
    formatPercent,
    formatRate,
    loadApexCharts,
    ramColor,
    renderChartLegend,
    syncChartTooltips,
    textColor,
    type ApexChart,
  } from '../lib/charts';

  let { uuid }: { uuid: string } = $props();
  type Point = [number, number];
  type Data = {
    is_metrics_enabled: boolean;
    is_sentinel_enabled: boolean;
    sentinel_metrics_refresh_rate_seconds: number;
    sentinel_metrics_history_days: number;
    sentinel_push_interval_seconds: number;
  };
  type Series = { cpuSeries: Point[] | null; memorySeries: Point[] | null; diskSeries: Point[] | null; loadSeries: Point[] | null; networkRxSeries: Point[]; networkTxSeries: Point[] };
  type Form = { refresh: string; history: string; push: string };

  const chartId = 'server';
  let data = $state<Data | null>(null);
  let form = $state<Form>({ refresh: '', history: '', push: '' });
  let saved = $state<Form>({ refresh: '', history: '', push: '' });
  let saving = $state(false);
  let error = $state<ApiError | null>(null);
  let interval = $state('5');
  let live = $state(true);
  let toggling = $state(false);
  let charts: Record<string, ApexChart> = {};
  let poll: ReturnType<typeof setInterval> | undefined;
  const base = $derived(`/server-page/${encodeURIComponent(uuid)}/metrics`);
  const canEdit = $derived(session.isAdmin);
  const dirty = $derived(JSON.stringify(form) !== JSON.stringify(saved));
  const pollSeconds = $derived(Number(interval) <= 60 ? 5 : 60);
  const INTERVALS = [
    { value: '5', label: 'Last 5 minutes' },
    { value: '10', label: 'Last 10 minutes' },
    { value: '30', label: 'Last 30 minutes' },
    { value: '60', label: 'Last hour' },
    { value: '720', label: 'Last 12 hours' },
    { value: '10080', label: 'Last week' },
    { value: '43200', label: 'Last 30 days' },
  ];
  try {
    const stored = localStorage.getItem('server-metrics-live');
    if (stored === '0') live = false;
  } catch {
    // The default applies.
  }

  function chartOptions(name: string, color: string, loadingText: string, formatter: (v: number) => string = formatPercent) {
    return {
      chart: { height: 240, type: 'area', toolbar: { show: false }, zoom: { enabled: false }, animations: { enabled: true }, background: 'transparent' },
      series: [{ name, data: [] }],
      colors: [color],
      stroke: { curve: 'smooth', width: 2 },
      fill: { type: 'gradient', gradient: { opacityFrom: 0.28, opacityTo: 0.02 } },
      dataLabels: { enabled: false },
      grid: { borderColor: 'rgba(128, 128, 128, 0.14)', strokeDashArray: 4 },
      legend: { show: false },
      xaxis: { type: 'datetime', labels: { datetimeUTC: false, style: { colors: textColor } } },
      yaxis: { min: 0, max: (max: number) => (max > 0 ? max * 1.2 : 1), forceNiceScale: true, tickAmount: 4, labels: { style: { colors: textColor }, formatter } },
      noData: { text: loadingText, style: { color: textColor } },
      tooltip: {
        shared: false,
        intersect: false,
        followCursor: false,
        fixed: { enabled: false },
        marker: { show: false },
        custom: ({ series, seriesIndex, dataPointIndex, w }: { series: number[][]; seriesIndex: number; dataPointIndex: number; w: ApexChart }) => {
          const timestamp = w.globals.seriesX[seriesIndex][dataPointIndex];
          const values = series.map((values, index) =>
            values[dataPointIndex] === undefined || values[dataPointIndex] === null ? '' : `<div class="apexcharts-tooltip-custom-value">${w.globals.seriesNames?.[index] ?? name}: <span class="apexcharts-tooltip-value-bold">${formatter(values[dataPointIndex])}</span></div>`,
          );
          return `<div class="apexcharts-tooltip-custom">${values.join('')}${chartTooltipTime(timestamp)}</div>`;
        },
      },
    };
  }

  async function mountCharts() {
    await loadApexCharts();
    await tick();
    const Apex = window.ApexCharts;
    const el = (key: string) => document.getElementById(`${chartId}-${key}`);
    if (!el('cpu')) return;
    charts = {
      cpu: new Apex(el('cpu'), chartOptions('CPU', cpuColor, 'Loading CPU metrics…')),
      memory: new Apex(el('memory'), chartOptions('Memory', ramColor, 'Loading memory metrics…')),
      disk: new Apex(el('disk'), chartOptions('Disk', '#ef4444', 'Loading disk metrics…', formatPercent)),
      network: new Apex(el('network'), { ...chartOptions('Network', '#10b981', 'Loading network metrics…', formatRate), colors: ['#10b981', '#f59e0b'] }),
      load: new Apex(el('load'), chartOptions('Load', '#14b8a6', 'Loading load metrics…', formatNumber)),
    };
    // Defer to the next frame so the grid cells are laid out before ApexCharts measures them.
    requestAnimationFrame(() => {
      Object.values(charts).forEach((c) => c.render());
      syncChartTooltips(`${chartId}-metrics`, [charts.cpu, charts.memory, charts.network, charts.load, charts.disk]);
      loadData();
    });
  }

  const legend = (key: string, items: Parameters<typeof renderChartLegend>[1]) => renderChartLegend(document.getElementById(`${chartId}-${key}-legend`), items);

  /** `loadData`, then the `refreshChartData-server-metrics` listener. */
  async function loadData() {
    if (!charts.cpu) return;
    try {
      const d = await get<Series>(`${base}/data?interval=${encodeURIComponent(interval)}`);
      const axis = (formatter: (v: number) => string) => ({
        xaxis: { type: 'datetime', labels: { datetimeUTC: false, style: { colors: textColor } } },
        yaxis: { min: 0, max: (max: number) => (max > 0 ? max * 1.2 : 1), forceNiceScale: true, tickAmount: 4, labels: { style: { colors: textColor }, formatter } },
      });
      charts.cpu.updateOptions({ colors: [cpuColor], series: [{ name: 'CPU', data: d.cpuSeries }], ...axis(formatPercent), noData: { text: 'No CPU metrics available', style: { color: textColor } } });
      charts.memory.updateOptions({ colors: [ramColor], series: [{ name: 'Memory', data: d.memorySeries }], ...axis(formatPercent), noData: { text: 'No memory metrics available', style: { color: textColor } } });
      charts.disk.updateOptions({ series: [{ name: 'Disk', data: d.diskSeries || [] }], noData: { text: 'No disk metrics available' } });
      charts.network.updateOptions({ series: [{ name: 'RX', data: d.networkRxSeries || [] }, { name: 'TX', data: d.networkTxSeries || [] }], noData: { text: 'No network metrics available' } });
      charts.load.updateOptions({ series: [{ name: 'Load', data: d.loadSeries || [] }], noData: { text: 'No load metrics available' } });
      legend('cpu', [{ name: 'CPU', color: cpuColor, data: d.cpuSeries, formatter: formatPercent }]);
      legend('memory', [{ name: 'Memory', color: ramColor, data: d.memorySeries, formatter: formatPercent }]);
      legend('network', [
        { name: 'RX', color: '#10b981', data: d.networkRxSeries, formatter: formatRate },
        { name: 'TX', color: '#f59e0b', data: d.networkTxSeries, formatter: formatRate },
      ]);
      legend('load', [{ name: 'Load', color: '#14b8a6', data: d.loadSeries, formatter: formatNumber }]);
      legend('disk', [{ name: 'Disk', color: '#ef4444', data: d.diskSeries, formatter: formatPercent }]);
    } catch (e) {
      failure(e);
    }
  }

  function schedule() {
    clearInterval(poll);
    if (live) poll = setInterval(loadData, pollSeconds * 1000);
  }
  function toggleLive() {
    live = !live;
    try {
      localStorage.setItem('server-metrics-live', live ? '1' : '0');
    } catch {
      // Only the preference is lost.
    }
    schedule();
    if (live) loadData();
  }
  function destroyCharts() {
    Object.values(charts).forEach((c) => c.destroy());
    charts = {};
  }
  onDestroy(() => {
    clearInterval(poll);
    destroyCharts();
  });

  async function load() {
    const d = await get<Data>(base);
    data = d;
    form = { refresh: String(d.sentinel_metrics_refresh_rate_seconds), history: String(d.sentinel_metrics_history_days), push: String(d.sentinel_push_interval_seconds) };
    saved = { ...form };
    if (d.is_metrics_enabled) {
      await tick();
      await mountCharts();
      schedule();
    }
  }
  async function save() {
    saving = true;
    error = null;
    try {
      const r = await post<{ message: string }>(base, {
        sentinel_metrics_refresh_rate_seconds: form.refresh,
        sentinel_metrics_history_days: form.history,
        sentinel_push_interval_seconds: form.push,
      });
      saved = { ...form };
      notify.success(r.message);
    } catch (e) {
      error = failure(e);
    } finally {
      saving = false;
    }
  }
  async function toggle() {
    toggling = true;
    try {
      const r = await post<{ message: string }>(`${base}/toggle`);
      notify.success(r.message);
      clearInterval(poll);
      destroyCharts();
      await load();
    } catch (e) {
      failure(e);
    } finally {
      toggling = false;
    }
  }
</script>

<ServerFrame {uuid} active="metrics" title="Metrics" onloaded={load}>
  {#snippet children()}
    {#if data}
      <div class="application-settings-form flex w-full flex-col gap-6">
        {#if data.is_metrics_enabled}
          <form
            class="contents"
            onsubmit={(e) => {
              e.preventDefault();
              save();
            }}
          >
            <UnsavedBar {dirty} {saving} onsave={save} onreset={() => ((form = { ...saved }), (error = null))} />
            <SettingsSection id="server-metrics-overview-section" title="Metrics" description="Inspect recent CPU and memory usage reported by Sentinel.">
              {#snippet actions()}
                <div class="flex items-center gap-2">
                  <div>
                    <button
                      type="button"
                      onclick={toggleLive}
                      aria-pressed={live}
                      title="Turn live refresh on or off"
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
                  <button type="button" class="button" disabled={!canEdit || toggling} onclick={toggle}>Disable metrics</button>
                </div>
              {/snippet}
              <div class="grid gap-4 lg:grid-cols-3">
                <FormInput type="number" min="1" id="sentinelMetricsRefreshRateSeconds" label="Collection rate" required helper="Seconds between metric samples." disabled={!canEdit} bind:value={form.refresh} error={error?.field('sentinel_metrics_refresh_rate_seconds')} />
                <FormInput type="number" min="1" id="sentinelMetricsHistoryDays" label="History retention" required helper="Days of CPU and memory history to retain." disabled={!canEdit} bind:value={form.history} error={error?.field('sentinel_metrics_history_days')} />
                <FormInput type="number" min="10" id="sentinelPushIntervalSeconds" label="Push interval" required helper="Seconds between health reports sent to Coolify." disabled={!canEdit} bind:value={form.push} error={error?.field('sentinel_push_interval_seconds')} />
              </div>
              <div class="mt-4 max-w-xs">
                <Listbox
                  id="interval"
                  label="Time range"
                  bind:value={interval}
                  options={INTERVALS}
                  onchange={() => {
                    loadData();
                    schedule();
                  }}
                />
              </div>
              <p class="mt-3 text-xs leading-5 text-neutral-500 dark:text-fg-dim">While live, ranges up to one hour refresh every five seconds. Longer ranges refresh every minute.</p>
            </SettingsSection>
          </form>

          <div class="grid gap-6 lg:grid-cols-2">
            {#each [{ key: 'cpu', id: 'server-cpu-metrics-section', title: 'CPU usage', help: 'Percentage of available CPU capacity used by this server.', klass: 'lg:col-span-2' }, { key: 'memory', id: 'server-memory-metrics-section', title: 'Memory usage', help: 'Percentage of physical memory currently used by this server.', klass: '' }, { key: 'network', id: 'server-network-metrics-section', title: 'Network throughput', help: "Receive and transmit rate across the server's network interfaces.", klass: '' }, { key: 'load', id: 'server-load-metrics-section', title: 'Load average', help: 'One minute load average reported by the server.', klass: '' }, { key: 'disk', id: 'server-disk-metrics-section', title: 'Disk usage', help: 'Percentage of the root filesystem used by this server.', klass: '' }] as c (c.key)}
              <SettingsSection id={c.id} title={c.title} description={c.help} class={c.klass}>
                {#snippet actions()}
                  <div id="{chartId}-{c.key}-legend" class="flex flex-wrap items-center gap-x-4 gap-y-1 text-xs"></div>
                {/snippet}
                <div id="{chartId}-{c.key}" class="min-h-[240px] w-full"></div>
              </SettingsSection>
            {/each}
          </div>
        {:else if data.is_sentinel_enabled}
          <SettingsSection id="server-metrics-overview-section" title="Metrics" description="Inspect recent CPU and memory usage reported by Sentinel.">
            <Empty size="sm" title="Metrics are disabled" description="Enable metrics to begin collecting CPU and memory history for this server." iconName="dashboard">
              {#snippet contents()}
                {#if canEdit}
                  <button type="button" class="button" {...{ ishighlighted: '' }} disabled={toggling} onclick={toggle}>Enable metrics</button>
                {/if}
              {/snippet}
            </Empty>
          </SettingsSection>
        {:else}
          <SettingsSection id="server-metrics-overview-section" title="Metrics" description="Inspect recent CPU and memory usage reported by Sentinel.">
            <Empty size="sm" title="Metrics unavailable" description="Sentinel metrics are unavailable on build and Swarm servers." iconName="dashboard">
              {#snippet contents()}
                <a class="button" href="/server/{uuid}/sentinel" use:link>View Sentinel</a>
              {/snippet}
            </Empty>
          </SettingsSection>
        {/if}
      </div>
    {/if}
  {/snippet}
</ServerFrame>
