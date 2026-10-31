// What upstream layouts/base.blade.php gives its chart pages: the vendored ApexCharts,
// `chartTooltipTime`, `syncChartTooltips` and `renderChartLegend`.

/* eslint-disable @typescript-eslint/no-explicit-any */
export type ApexChart = any;
declare global {
  interface Window {
    ApexCharts?: any;
  }
}

let loading: Promise<void> | null = null;

/** `<script src="js/apexcharts.js">`, the same file upstream ships. */
export function loadApexCharts(): Promise<void> {
  if (window.ApexCharts) return Promise.resolve();
  loading ??= new Promise((resolve, reject) => {
    const script = document.createElement('script');
    script.src = '/js/apexcharts.js';
    script.onload = () => resolve();
    script.onerror = () => reject(new Error('Could not load the charts.'));
    document.head.appendChild(script);
  });
  return loading;
}

export const cpuColor = '#1e90ff';
export const ramColor = '#00ced1';
export const textColor = 'currentColor';

export function chartTooltipTime(timestamp: number, { seconds = true } = {}): string {
  const format = (timeZone?: string) =>
    new Date(timestamp).toLocaleString(undefined, {
      month: 'short',
      day: 'numeric',
      hour: '2-digit',
      minute: '2-digit',
      ...(seconds ? { second: '2-digit' } : {}),
      hourCycle: 'h23',
      ...(timeZone ? { timeZone, timeZoneName: 'short' } : {}),
    } as Intl.DateTimeFormatOptions);
  let times: string | null = null;
  try {
    times = localStorage.getItem('chartTimes');
  } catch {
    // The default below applies.
  }
  if (times !== 'utc') return `<div class="apexcharts-tooltip-custom-title">${format()}</div>`;
  const local = Intl.DateTimeFormat().resolvedOptions().timeZone;
  return `<div class="apexcharts-tooltip-custom-title">Your time: ${format(local)}</div>
                <div class="apexcharts-tooltip-custom-title">UTC: ${format('UTC')}</div>`;
}

const groups: Record<string, ApexChart[]> = {};

/** Shows the tooltip at the same time on every chart of a group while one of them is hovered. */
export function syncChartTooltips(group: string, charts: ApexChart[]): void {
  const grid = (chart: ApexChart) => chart.el.querySelector('.apexcharts-grid')?.getBoundingClientRect();
  const hoverArea = (chart: ApexChart) => chart.el.querySelector('.apexcharts-svg');
  const hide = (chart: ApexChart) => hoverArea(chart)?.dispatchEvent(new MouseEvent('mouseout', { clientX: 0, clientY: -1 }));
  const peers = (source: ApexChart) => groups[group].filter((chart) => chart !== source && chart.el.isConnected);
  groups[group] = [...(groups[group] ?? []).filter((chart) => chart.el.isConnected), ...charts];
  for (const source of charts) {
    source.el.addEventListener('mousemove', (event: MouseEvent) => {
      const rect = grid(source);
      const { minX, maxX } = source.w.globals;
      if (!event.isTrusted || !rect?.width || !(maxX > minX)) return;
      if (event.clientX < rect.left || event.clientX > rect.right || event.clientY < rect.top || event.clientY > rect.bottom) {
        peers(source).forEach(hide);
        return;
      }
      const timestamp = minX + ((event.clientX - rect.left) / rect.width) * (maxX - minX);
      peers(source).forEach((chart) => {
        const target = grid(chart);
        const { minX: min, maxX: max } = chart.w.globals;
        if (!target?.width || !(max > min) || timestamp < min || timestamp > max) {
          hide(chart);
          return;
        }
        hoverArea(chart)?.dispatchEvent(
          new MouseEvent('mousemove', {
            clientX: target.left + ((timestamp - min) / (max - min)) * target.width,
            clientY: target.top + target.height / 2,
          }),
        );
      });
    });
    source.el.addEventListener('mouseleave', () => peers(source).forEach(hide));
  }
}

export type LegendItem = { name: string; color: string; data: [number, number][] | null | undefined; formatter: (v: number) => string };

export function renderChartLegend(element: HTMLElement | null, items: LegendItem[]): void {
  if (!element) return;
  element.replaceChildren(
    ...items.map(({ name, color, data, formatter }) => {
      const last = [...(data || [])].reverse().find((point) => point?.[1] !== null && point?.[1] !== undefined);
      const item = document.createElement('span');
      item.className = 'inline-flex items-center gap-1.5';
      const dot = document.createElement('span');
      dot.className = 'size-2 shrink-0 rounded-full';
      dot.style.backgroundColor = color;
      const label = document.createElement('span');
      label.className = 'text-neutral-500 dark:text-fg-dim';
      label.textContent = name;
      item.append(dot, label);
      if (last) {
        const value = document.createElement('span');
        value.className = 'font-medium tabular-nums text-black dark:text-fg';
        value.textContent = formatter(last[1]);
        item.append(value);
      }
      return item;
    }),
  );
}

export const formatPercent = (value: number) => {
  const number = Number(value);
  const precision = Math.abs(number) < 1 ? 2 : 1;
  return `${Number(number.toFixed(precision))}%`;
};
export const formatNumber = (value: number) => Number(Number(value).toFixed(2)).toLocaleString();
export const formatBytes = (value: number) => {
  const units = ['B', 'KB', 'MB', 'GB', 'TB'];
  let amount = Number(value) || 0;
  let unit = 0;
  while (amount >= 1024 && unit < units.length - 1) {
    amount /= 1024;
    unit++;
  }
  return `${Number(amount.toFixed(unit === 0 ? 0 : 2))} ${units[unit]}`;
};
export const formatRate = (value: number) => `${formatBytes(value)}/s`;
