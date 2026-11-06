<script lang="ts">
  // Port of upstream livewire/project/shared/metrics.blade.php with Project\Shared\Metrics.
  import { onDestroy, tick } from 'svelte';
  import Empty from '../Empty.svelte';
  import Listbox from '../Listbox.svelte';
  import LiveToggle from '../LiveToggle.svelte';
  import SettingsSection from '../SettingsSection.svelte';
  import StatusBadge from '../StatusBadge.svelte';
  import { link } from '../../lib/router.svelte';
  import { request } from '../../lib/api';
  import { failure } from '../../lib/serverApi';
  import { cpuColor, loadApexCharts, ramColor, renderChartLegend, syncChartTooltips, textColor, type ApexChart } from '../../lib/charts';
  import { fetchResourceMetrics } from '../../lib/metricsApi.svelte';
  import { resPath, type ResourceKind } from '../../lib/sharedApi';

  let { kind, uuid }: { kind: ResourceKind; uuid: string } = $props();
  const chartId = 'metrics';
  const INTERVALS = [
    { value: '5', label: 'Last 5 minutes' },
    { value: '10', label: 'Last 10 minutes' },
    { value: '30', label: 'Last 30 minutes' },
    { value: '60', label: 'Last hour' },
    { value: '720', label: 'Last 12 hours' },
    { value: '10080', label: 'Last week' },
    { value: '43200', label: 'Last 30 days' },
  ];
  let d = $state<any>(null);
  let interval = $state('5');
  let live = $state(true);
  let cpu: ApexChart | null = null;
  let memory: ApexChart | null = null;
  let poll: ReturnType<typeof setInterval> | undefined;
  const showCharts = $derived(d && !(d.is_application && d.build_pack === 'dockercompose') && d.metrics_enabled && d.running);
  const pollSeconds = $derived(Number(interval) <= 60 ? 5 : 60);

  $effect(() => {
    request<any>('GET', `${resPath(kind, uuid)}/metrics`).then((r) => (d = r));
  });

  const formatCpu = (value: number) => `${Number(value.toFixed(Math.abs(value) < 1 ? 2 : 1))}%`;
  const formatMemory = (value: number) => (value >= 1024 ? `${Number((value / 1024).toFixed(1))} GB` : `${Math.round(value)} MB`);
  const xaxis = () => ({ type: 'datetime', labels: { datetimeUTC: true, style: { colors: textColor } } });
  const yaxis = (formatter: (v: number) => string) => ({ min: 0, max: (max: number) => (max > 0 ? max * 1.2 : 1), forceNiceScale: true, labels: { style: { colors: textColor }, formatter } });

  function options(name: string, color: string, loading: string, formatter: (v: number) => string, unit: string) {
    return {
      chart: { height: 240, type: 'area', toolbar: { show: false }, zoom: { enabled: false }, animations: { enabled: true }, background: 'transparent' },
      series: [{ name, data: [] }],
      colors: [color],
      stroke: { curve: 'smooth', width: 2 },
      fill: { type: 'gradient', gradient: { opacityFrom: 0.28, opacityTo: 0.02, stops: [0, 90, 100] } },
      dataLabels: { enabled: false },
      grid: { borderColor: 'rgba(128, 128, 128, 0.14)', strokeDashArray: 4 },
      legend: { show: false },
      xaxis: xaxis(),
      yaxis: yaxis(formatter),
      noData: { text: loading, style: { color: textColor } },
      tooltip: {
        shared: false,
        intersect: false,
        followCursor: false,
        fixed: { enabled: false },
        marker: { show: false },
        custom: ({ series, seriesIndex, dataPointIndex, w }: ApexChart) => {
          const value = series[seriesIndex][dataPointIndex];
          const date = new Date(w.globals.seriesX[seriesIndex][dataPointIndex]);
          return `<div class="apexcharts-tooltip-custom">
                                    <div class="apexcharts-tooltip-custom-value">${name}: <span class="apexcharts-tooltip-value-bold">${value}${unit}</span></div>
                                    <div class="apexcharts-tooltip-custom-title">${date.toLocaleString(undefined, { timeZone: 'UTC', hour12: false })} UTC</div>
                                </div>`;
        },
      },
    };
  }

  /** `loadData`, then the `refreshChartData-metrics-*` listeners. */
  async function loadData() {
    if (!cpu || !memory) return;
    try {
      const data = await fetchResourceMetrics(kind, uuid, interval);
      if (!cpu || !memory) return;
      cpu.updateOptions({ colors: [cpuColor], series: [{ name: 'CPU', data: data.cpu ?? [] }], xaxis: xaxis(), yaxis: yaxis(formatCpu), noData: { text: 'No CPU metrics available', style: { color: textColor } } });
      renderChartLegend(document.getElementById(`${chartId}-cpu-legend`), [{ name: 'CPU', color: cpuColor, data: data.cpu ?? [], formatter: formatCpu }]);
      memory.updateOptions({ colors: [ramColor], series: [{ name: 'Memory', data: data.memory ?? [] }], xaxis: xaxis(), yaxis: yaxis(formatMemory), noData: { text: 'No memory metrics available', style: { color: textColor } } });
      renderChartLegend(document.getElementById(`${chartId}-memory-legend`), [{ name: 'Memory', color: ramColor, data: data.memory ?? [], formatter: formatMemory }]);
    } catch (e) {
      failure(e);
    }
  }

  function schedule() {
    clearInterval(poll);
    poll = live ? setInterval(loadData, pollSeconds * 1000) : undefined;
  }

  let mounted = false;
  $effect(() => {
    if (!showCharts || mounted) return;
    mounted = true;
    void (async () => {
      await loadApexCharts();
      await tick();
      const cpuEl = document.getElementById(`${chartId}-cpu`);
      const memoryEl = document.getElementById(`${chartId}-memory`);
      if (!cpuEl || !memoryEl) return;
      cpu = new window.ApexCharts(cpuEl, options('CPU', cpuColor, 'Loading CPU metrics…', formatCpu, '%'));
      memory = new window.ApexCharts(memoryEl, options('Memory', ramColor, 'Loading memory metrics…', formatMemory, ' MB'));
      cpu.render();
      memory.render();
      syncChartTooltips(chartId, [cpu]);
      syncChartTooltips(chartId, [memory]);
      void loadData();
      schedule();
    })();
  });

  onDestroy(() => {
    clearInterval(poll);
    cpu?.destroy();
    memory?.destroy();
    cpu = memory = null;
  });
</script>

{#if d}
  <div class="flex flex-col gap-6">
    {#if d.is_application && d.build_pack === 'dockercompose'}
      <SettingsSection id="metrics-overview-section" title="Metrics" description="Inspect CPU and memory usage for this application.">
        <Empty size="sm" title="Metrics unavailable" description="Container metrics are not currently available for Docker Compose applications." iconName="dashboard" />
      </SettingsSection>
    {:else if !d.metrics_enabled}
      <SettingsSection id="metrics-overview-section" title="Metrics" description="Inspect CPU and memory usage for this application.">
        {#snippet actions()}
          <a use:link class="button" href="/server/{d.server_uuid}/metrics">
            Server metrics
            <svg class="inline-flex w-3 h-3 dark:text-neutral-400 text-black" fill="none" stroke="currentColor" viewBox="0 0 24 24">
              <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M10 6H6a2 2 0 00-2 2v10a2 2 0 002 2h10a2 2 0 002-2v-4M14 4h6m0 0v6m0-6L10 14" />
            </svg>
          </a>
        {/snippet}
        <Empty size="sm" title="Metrics are not enabled" description="Enable metrics for this server before collecting application usage data." iconName="dashboard" />
      </SettingsSection>
    {:else if !d.running}
      <SettingsSection id="metrics-overview-section" title="Metrics" description="Inspect CPU and memory usage for this application.">
        {#snippet actions()}
          <StatusBadge label="Not running" type="neutral" />
        {/snippet}
        <Empty size="sm" title="Container is not running" description="Start the application to begin collecting CPU and memory metrics." iconName="dashboard" />
      </SettingsSection>
    {:else}
      <SettingsSection id="metrics-overview-section" title="Metrics" description="Inspect recent CPU and memory usage reported by Sentinel.">
        {#snippet actions()}
          <LiveToggle
            bind:live
            storageKey="resource-metrics-live"
            onchange={(next) => {
              if (next) void loadData();
              schedule();
            }}
          />
        {/snippet}
        <div class="max-w-xs">
          <Listbox
            id="interval"
            label="Time range"
            bind:value={interval}
            options={INTERVALS}
            onchange={() => {
              void loadData();
              schedule();
            }}
          />
        </div>
        <p class="mt-3 text-xs leading-5 text-neutral-500 dark:text-fg-dim">While live, ranges up to one hour refresh every five seconds. Longer ranges refresh every minute.</p>
      </SettingsSection>

      <SettingsSection id="cpu-metrics-section" title="CPU usage" description="CPU usage of the container, as in docker stats. 100% means one CPU core is fully busy, so a container that uses more than one core can go above 100%. The maximum is the number of cores × 100%.">
        {#snippet actions()}
          <div id="{chartId}-cpu-legend" class="flex flex-wrap items-center gap-x-4 gap-y-1 text-xs"></div>
        {/snippet}
        <div id="{chartId}-cpu" class="min-h-[240px] w-full"></div>
      </SettingsSection>

      <SettingsSection id="memory-metrics-section" title="Memory usage" description="Memory consumed by the application container, measured in megabytes.">
        {#snippet actions()}
          <div id="{chartId}-memory-legend" class="flex flex-wrap items-center gap-x-4 gap-y-1 text-xs"></div>
        {/snippet}
        <div id="{chartId}-memory" class="min-h-[240px] w-full"></div>
      </SettingsSection>
    {/if}
  </div>
{/if}
