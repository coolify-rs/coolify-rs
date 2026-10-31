<script lang="ts">
  // The chart of upstream livewire/traffic/_device-chart.blade.php.
  import { onMount } from 'svelte';
  import { textColor, type ApexChart } from '../../lib/charts';

  let { chartId, labels, series }: { chartId: string; labels: string[]; series: number[] } = $props();

  let el: HTMLDivElement;
  let chart: ApexChart | null = null;
  const palette = ['#3b82f6', '#f59e0b', '#ec4899', '#8b5cf6', '#10b981', '#14b8a6', '#6b7280'];
  const escapeHtml = (value: unknown) =>
    String(value).replace(/[&<>'"]/g, (character) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', "'": '&#039;', '"': '&quot;' })[character] as string);
  // ApexCharts writes legend text with innerHTML; device labels come from request data.
  const legend = () => ({ position: 'bottom', labels: { colors: textColor }, formatter: (seriesName: string) => escapeHtml(seriesName) });

  onMount(() => {
    const frame = requestAnimationFrame(() => {
      chart = new window.ApexCharts(el, {
        chart: { type: 'donut', height: 240, background: 'transparent', animations: { enabled: false } },
        series: [...series],
        labels: [...labels],
        colors: palette,
        stroke: { width: 2 },
        dataLabels: { enabled: false },
        legend: legend(),
        plotOptions: { pie: { donut: { size: '68%' } } },
        tooltip: {
          fillSeriesColor: false,
          custom: ({ series: values, seriesIndex, w }: ApexChart) => {
            const label = escapeHtml(w.globals.labels[seriesIndex] ?? '');
            const requests = Number(values[seriesIndex] ?? 0).toLocaleString();
            return `<div class="apexcharts-tooltip-custom">
                        <div class="apexcharts-tooltip-custom-value">${label}: <span class="apexcharts-tooltip-value-bold">${requests} requests</span></div>
                    </div>`;
          },
        },
        noData: { text: 'Loading devices…', style: { color: textColor } },
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
    const nextLabels = [...labels];
    const nextSeries = [...series];
    if (first) {
      first = false;
      return;
    }
    if (!chart) return;
    chart.updateOptions({ labels: nextLabels, legend: legend() });
    chart.updateSeries(nextSeries);
  });
</script>

<div bind:this={el} id="{chartId}-device" class="min-h-[240px] w-full"></div>
