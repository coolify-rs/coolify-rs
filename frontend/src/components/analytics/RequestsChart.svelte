<script lang="ts">
  // Port of upstream livewire/traffic/_requests-chart.blade.php.
  import { onMount } from 'svelte';
  import Empty from '../Empty.svelte';
  import { chartTooltipTime, textColor, type ApexChart } from '../../lib/charts';
  import type { SeriesBucket } from '../../lib/analyticsApi';

  let { chartId, series, range }: { chartId: string; series: SeriesBucket[]; range: string } = $props();

  let el: HTMLDivElement;
  let chart: ApexChart | null = null;
  const hasData = $derived(series.reduce((sum, b) => sum + b.requests, 0) > 0);
  const cssVar = (name: string) => getComputedStyle(document.documentElement).getPropertyValue(name).trim();
  const accent = () => cssVar('--chart-status-3xx') || '#3b82f6';
  const gridColor = () => cssVar('--chart-geo-empty') || 'rgba(128,128,128,0.15)';
  const pointsOf = (rows: SeriesBucket[]) => rows.map((b) => ({ x: b.bucket, y: b.requests || 0 }));
  // `24h` buckets are hourly, `7d`/`30d` daily: a matching axis/tooltip format.
  const timeFormat = (r: string) => (r === '24h' ? 'HH:mm' : 'dd MMM');

  onMount(() => {
    const frame = requestAnimationFrame(() => {
      const compactNumber = new Intl.NumberFormat(undefined, { notation: 'compact', maximumFractionDigits: 1 });
      chart = new window.ApexCharts(el, {
        chart: { height: 220, type: 'area', toolbar: { show: false }, zoom: { enabled: false }, animations: { enabled: false }, background: 'transparent' },
        series: [{ name: 'Requests', data: pointsOf(series) }],
        colors: [accent()],
        dataLabels: { enabled: false },
        stroke: { width: 2, curve: 'smooth' },
        fill: { type: 'gradient', gradient: { opacityFrom: 0.35, opacityTo: 0.05 } },
        xaxis: {
          type: 'datetime',
          labels: { style: { colors: textColor }, datetimeUTC: false },
          axisBorder: { show: false },
          axisTicks: { show: false },
        },
        yaxis: {
          labels: {
            minWidth: 28,
            maxWidth: 28,
            style: { colors: textColor },
            formatter: (value: number) => compactNumber.format(Math.round(value)),
          },
        },
        grid: { borderColor: gridColor(), padding: { left: 6, right: 12, top: 12, bottom: 0 } },
        legend: { show: false },
        noData: { text: 'Loading requests…', style: { color: textColor } },
        tooltip: {
          shared: true,
          intersect: false,
          marker: { show: false },
          custom: ({ series: values, seriesIndex, dataPointIndex, w }: ApexChart) => {
            const requests = values[seriesIndex][dataPointIndex];
            const timestamp = w.globals.seriesX[seriesIndex][dataPointIndex];
            return `<div class="apexcharts-tooltip-custom">
                        <div class="apexcharts-tooltip-custom-value">Requests: <span class="apexcharts-tooltip-value-bold">${requests.toLocaleString()}</span></div>
                        ${chartTooltipTime(timestamp, { seconds: false })}
                    </div>`;
          },
        },
      });
      chart.render();
    });
    return () => {
      cancelAnimationFrame(frame);
      chart?.destroy();
      chart = null;
    };
  });

  // The `refreshChartData-{chartId}-status` listener.
  let first = true;
  $effect(() => {
    const rows = series;
    const r = range;
    if (first) {
      first = false;
      return;
    }
    if (!chart) return;
    chart.updateOptions({
      colors: [accent()],
      grid: { borderColor: gridColor(), padding: { left: 6, right: 12, top: 12, bottom: 0 } },
      xaxis: { type: 'datetime', labels: { style: { colors: textColor }, datetimeUTC: false, format: timeFormat(r) } },
      tooltip: { x: { format: timeFormat(r) } },
    });
    chart.updateSeries([{ name: 'Requests', data: pointsOf(rows) }]);
  });
</script>

<div class="relative w-full">
  <div bind:this={el} id="{chartId}-requests" class="min-h-[220px] w-full"></div>
  <div id="{chartId}-requests-empty" style="display: {hasData ? 'none' : 'flex'}" class="absolute inset-0 items-center justify-center bg-white dark:bg-base">
    <Empty size="sm" title="No requests in this range" description="No request traffic was recorded for the selected filters and range. Try a wider range or check back later." iconName="analytics" />
  </div>
</div>
