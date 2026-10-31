<script lang="ts">
  // Port of upstream livewire/traffic/_sparkline.blade.php.
  import { onMount } from 'svelte';
  import { chartTooltipTime, type ApexChart } from '../../lib/charts';

  let {
    id,
    values,
    categories,
    colorVar,
    label,
    format = 'number',
  }: { id: string; values: number[]; categories: number[]; colorVar: string; label: string; format?: 'number' | 'bytes' | 'ms' } = $props();

  let el: HTMLDivElement;
  let chart: ApexChart | null = null;
  let tip: HTMLDivElement | null = null;

  function showTip(html: string, pointX: number) {
    if (!tip) {
      tip = document.createElement('div');
      tip.className = 'pointer-events-none fixed z-50';
      document.body.appendChild(tip);
    }
    tip.innerHTML = html;
    tip.hidden = false;
    const rect = el.getBoundingClientRect();
    const width = tip.offsetWidth;
    const left = Math.min(Math.max(rect.left + pointX - width / 2, 8), window.innerWidth - width - 8);
    tip.style.left = `${left}px`;
    tip.style.top = `${rect.bottom + 8}px`;
  }
  const hideTip = () => {
    if (tip) tip.hidden = true;
  };
  const cssVar = (name: string) => getComputedStyle(document.documentElement).getPropertyValue(name).trim();
  const accent = () => cssVar(colorVar) || '#3b82f6';
  const muted = () => cssVar('--chart-geo-empty') || 'rgba(128,128,128,0.45)';
  const isFlat = (a: number[]) => !Array.isArray(a) || a.length === 0 || a.every((v) => !Number(v));
  const points = (data: number[], cats: number[]) => data.map((value, index) => (cats[index] === undefined ? value : { x: cats[index], y: value }));
  function opts(data: number[], cats: number[]) {
    const flat = isFlat(data);
    const len = Array.isArray(data) && data.length ? data.length : 12;
    const shown = flat ? Array(len).fill(0) : data;
    return {
      series: [{ name: label, data: points(shown, cats) }],
      colors: [flat ? muted() : accent()],
      stroke: { width: flat ? 1 : 1.5, curve: 'smooth' },
      fill: { type: 'gradient', gradient: { opacityFrom: flat ? 0 : 0.35, opacityTo: 0 } },
      tooltip: { enabled: !flat },
      yaxis: { labels: { show: false }, ...(flat ? { min: -1, max: 1 } : {}) },
    };
  }
  function formatValue(value: number | null | undefined) {
    if (value === null || value === undefined) return '—';
    if (format === 'bytes') {
      const units = ['B', 'KB', 'MB', 'GB', 'TB'];
      let amount = Number(value) || 0;
      let unit = 0;
      while (amount >= 1024 && unit < units.length - 1) {
        amount /= 1024;
        unit++;
      }
      return `${Number(amount.toFixed(unit === 0 ? 0 : 2))} ${units[unit]}`;
    }
    if (format === 'ms') return `${Number(value).toLocaleString()} ms`;
    return Number(value).toLocaleString();
  }

  onMount(() => {
    const o = opts(values, categories);
    chart = new window.ApexCharts(el, {
      chart: { type: 'area', height: 36, sparkline: { enabled: true }, animations: { enabled: false }, background: 'transparent' },
      series: o.series,
      colors: o.colors,
      stroke: o.stroke,
      fill: o.fill,
      markers: { size: 0, hover: { size: 5, sizeOffset: 2 } },
      yaxis: o.yaxis,
      xaxis: { type: 'datetime', labels: { show: false }, axisBorder: { show: false }, axisTicks: { show: false } },
      grid: { padding: { left: 4, right: 4, top: 0, bottom: 0 } },
      tooltip: {
        enabled: o.tooltip.enabled,
        shared: true,
        intersect: false,
        fixed: { enabled: false },
        x: { show: false },
        marker: { show: false },
        custom: ({ series, dataPointIndex, w }: ApexChart) => {
          const value = series[0][dataPointIndex];
          const timestamp = w.globals.seriesX[0][dataPointIndex];
          showTip(
            `<div class='apexcharts-tooltip-custom'>
                            <div class='apexcharts-tooltip-custom-value'>${label}: <span class='apexcharts-tooltip-value-bold'>${formatValue(value)}</span></div>
                            ${chartTooltipTime(timestamp, { seconds: false })}
                        </div>`,
            w.globals.translateX + w.globals.seriesXvalues[0][dataPointIndex],
          );
          return '';
        },
      },
    });
    chart.render();
    el.addEventListener('mouseleave', hideTip);
    window.addEventListener('scroll', hideTip, { capture: true, passive: true });
    return () => {
      chart?.destroy();
      chart = null;
      tip?.remove();
      window.removeEventListener('scroll', hideTip, { capture: true });
    };
  });

  // The `refreshChartData-{chartId}-status` listener.
  let first = true;
  $effect(() => {
    const data = values;
    const cats = categories;
    if (first) {
      first = false;
      return;
    }
    if (!chart) return;
    const u = opts(data, cats);
    chart.updateOptions({ colors: u.colors, stroke: u.stroke, fill: u.fill, tooltip: { enabled: u.tooltip.enabled }, yaxis: u.yaxis }, false, false);
    chart.updateSeries(u.series);
  });
</script>

<div bind:this={el} {id} class="h-9 w-full [&_.apexcharts-svg]:overflow-visible!"></div>
