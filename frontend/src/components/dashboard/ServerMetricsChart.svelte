<script lang="ts">
  // Port of upstream livewire/dashboard/server-metrics-chart.blade.php: the background of a server card.
  import { onMount } from 'svelte';
  import { chartTooltipTime, loadApexCharts, type ApexChart } from '../../lib/charts';
  import type { ResourceSeries } from '../../lib/metricsApi.svelte';

  let { uuid, data }: { uuid: string; data: ResourceSeries | undefined } = $props();
  let el: HTMLDivElement;
  let chart: ApexChart | null = null;

  const series = (d: ResourceSeries | undefined) => [
    { name: 'CPU', data: d?.cpu ?? [] },
    { name: 'Memory', data: d?.memory ?? [] },
  ];

  onMount(() => {
    let destroyed = false;
    loadApexCharts().then(() => {
      if (destroyed) return;
      chart = new window.ApexCharts(el, {
        chart: { height: '100%', type: 'area', toolbar: { show: false }, zoom: { enabled: false }, animations: { enabled: true }, sparkline: { enabled: true }, background: 'transparent' },
        series: series(data),
        colors: ['rgba(30, 144, 255, 0.52)', 'rgba(168, 85, 247, 0.42)'],
        stroke: { curve: 'smooth', width: 1.5 },
        fill: { type: 'gradient', gradient: { opacityFrom: 0.18, opacityTo: 0.02, stops: [0, 100] } },
        dataLabels: { enabled: false },
        grid: { show: false },
        legend: { show: false },
        markers: { size: 0 },
        xaxis: { type: 'datetime', labels: { show: false }, axisBorder: { show: false }, axisTicks: { show: false } },
        yaxis: { min: 0, max: 100, labels: { show: false } },
        tooltip: {
          shared: true,
          intersect: false,
          marker: { show: false },
          custom: ({ series, seriesIndex, dataPointIndex, w }: ApexChart) => {
            const cpu = series[0][dataPointIndex];
            const memory = series[1][dataPointIndex];
            const timestamp = w.globals.seriesX[seriesIndex][dataPointIndex];
            const formatPercent = (value: number) => (Number.isFinite(value) ? `${Number(value.toFixed(1))}%` : '—');
            const dot = (color: string) => `<span style="display:inline-block;width:8px;height:8px;border-radius:9999px;margin-right:6px;flex:none;background:${color}"></span>`;
            return `<div class="apexcharts-tooltip-custom">
                                    <div class="apexcharts-tooltip-custom-value" style="display:flex;align-items:center">${dot('rgb(30, 144, 255)')}CPU: <span class="apexcharts-tooltip-value-bold" style="margin-left:4px">${formatPercent(cpu)}</span></div>
                                    <div class="apexcharts-tooltip-custom-value" style="display:flex;align-items:center">${dot('rgb(168, 85, 247)')}Memory: <span class="apexcharts-tooltip-value-bold" style="margin-left:4px">${formatPercent(memory)}</span></div>
                                    ${chartTooltipTime(timestamp)}
                                </div>`;
          },
        },
      });
      chart.render();
    });
    return () => {
      destroyed = true;
      chart?.destroy();
      chart = null;
    };
  });

  $effect(() => {
    const next = series(data);
    chart?.updateSeries(next);
  });
</script>

<div class="absolute right-0 bottom-0 h-2/3 w-full rounded-b-xl [&_.apexcharts-svg]:overflow-hidden [&_.apexcharts-svg]:rounded-b-xl [&_.apexcharts-tooltip]:z-30!">
  <div bind:this={el} id="dashboard-server-metrics-{uuid}" class="h-full w-full"></div>
</div>
