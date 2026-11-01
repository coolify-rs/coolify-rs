// Client of crates/coolify-api/src/ui_fleet_metrics.rs, the charts of `Dashboard\ServerMetricsChart`
// and `Project\Shared\Metrics`, and the dashboard and resource traffic analytics.
import { get, post } from './serverApi';
import type { AnalyticsData } from './analyticsApi';
import { onDestroy } from 'svelte';

export type Point = [number, number];
export type FleetRow = {
  uuid: string;
  name: string;
  online: boolean;
  reporting: boolean;
  cpu: number | null;
  memUsed: number | null;
  memTotal: number | null;
  diskUsed: number | null;
  diskTotal: number | null;
  diskPercent: number | null;
  load1: number | null;
  netRx: number | null;
  netTx: number | null;
  containers: number | null;
};
export type FleetContainer = {
  id: string;
  cpu: number | null;
  memUsed: number | null;
  memPercent: number | null;
  diskBytes: number | null;
  net: number | null;
  server: string;
  serverUuid: string;
  name: string;
  image: string | null;
  link: string | null;
};
export type ChartData = {
  range: string;
  cpu: Point[];
  memory: Point[];
  disk: Point[];
  load: Point[];
  networkRx: Point[];
  networkTx: Point[];
  sparkCategories: number[];
  cpuSpark: (number | null)[];
  memSpark: (number | null)[];
  diskSpark: (number | null)[];
  netSpark: (number | null)[];
};
export type FleetKpis = {
  serversOnline: number;
  serversReporting: number;
  serversTotal: number;
  needsAttention: number;
  cpuAvg: number;
  cpuApproximate: boolean;
  memPercent: number;
  diskPercent: number;
  netRx: number;
  netTx: number;
  containers: number;
};
export type FleetData = {
  hasServers: boolean;
  enableableServers: string[];
  serverOptions: { value: string; label: string }[];
  range: string;
  serverUuid: string;
  containerMetric: string;
  kpis?: FleetKpis;
  serverRows?: FleetRow[];
  containersRaw?: FleetContainer[];
  chartData?: ChartData;
};

export function fetchFleet(filters: { range: string; server: string; cmetric: string }): Promise<FleetData> {
  const query = new URLSearchParams();
  if (filters.range) query.set('range', filters.range);
  if (filters.server) query.set('server', filters.server);
  if (filters.cmetric) query.set('cmetric', filters.cmetric);
  return get<FleetData>(`/metrics?${query}`);
}

export const enableFleetMetrics = () => post<{ success: string | null; error: string | null }>('/metrics/enable', {});

/** `FleetMetricsAggregator::rankContainers`. */
export function rankContainers(containers: FleetContainer[], metric: string, limit = 20): FleetContainer[] {
  const key = metric === 'memory' ? 'memUsed' : metric === 'disk' ? 'diskBytes' : metric === 'network' ? 'net' : 'cpu';
  return [...containers].sort((a, b) => (b[key] ?? 0) - (a[key] ?? 0)).slice(0, limit);
}

/** `PressureLevel`. */
export function pressureLevel(percent: number | null, kind: string): 'ok' | 'warning' | 'error' {
  if (percent === null) return 'ok';
  const [warn, error] = kind === 'disk' ? [80, 90] : [70, 90];
  return percent >= error ? 'error' : percent >= warn ? 'warning' : 'ok';
}
export const pressureText = { error: 'text-red-500 dark:text-red-400', warning: 'text-orange-500 dark:text-warning', ok: 'text-black dark:text-fg' };
export const pressureBar = { error: 'bg-red-500 dark:bg-red-400', warning: 'bg-orange-500 dark:bg-warning', ok: 'bg-neutral-400 dark:bg-white/30' };

export type ResourceSeries = { cpu: Point[] | null; memory: Point[] | null };
export const fetchResourceMetrics = (kind: string, uuid: string, interval: string) =>
  get<ResourceSeries>(`/resources/${encodeURIComponent(kind)}/${encodeURIComponent(uuid)}/metrics/data?interval=${encodeURIComponent(interval)}`);

export type ResourceAnalyticsData = Omit<AnalyticsData, 'scopedServerUuid' | 'hasServers' | 'serverOptions' | 'appOptions' | 'serverUuid' | 'appUuid' | 'eligibleDisabledServers' | 'nudgeKey'> & {
  enabled: boolean;
  analyticsServerUuid: string | null;
  error?: string;
};
export const fetchResourceAnalytics = (kind: string, uuid: string, range: string) =>
  get<ResourceAnalyticsData>(`/resources/${encodeURIComponent(kind)}/${encodeURIComponent(uuid)}/analytics?range=${encodeURIComponent(range)}`);

export type DashboardTraffic = Pick<AnalyticsData, 'overview' | 'range' | 'series' | 'errorRate' | 'bandwidthBytes' | 'uniquesApproximate'> & { serverCount: number };
export const fetchDashboardTraffic = (range: string) => get<DashboardTraffic>(`/dashboard/traffic?range=${encodeURIComponent(range)}`);

const fetchServerChart = (uuid: string) => get<ResourceSeries | null>(`/dashboard/server-metrics/${encodeURIComponent(uuid)}`);

/**
 * The polling of livewire/dashboard/server-metrics-chart.blade.php, one request per server: every
 * minute while the page is visible, and when it comes back after a minute or more in the background.
 * Must be created during component setup.
 */
export function serverMetricsPoll() {
  const charts = $state<Record<string, ResourceSeries>>({});
  let uuids: string[] = [];
  let hiddenAt: number | null = null;

  const load = () => {
    for (const uuid of uuids) {
      fetchServerChart(uuid)
        .then((data) => {
          if (data) charts[uuid] = data;
        })
        .catch(() => {
          // `report($e)`: the chart keeps what it shows.
        });
    }
  };
  const onVisibility = () => {
    if (document.hidden) {
      hiddenAt = Date.now();
      return;
    }
    if (hiddenAt && Date.now() - hiddenAt >= 60000) load();
    hiddenAt = null;
  };
  document.addEventListener('visibilitychange', onVisibility);
  const timer = setInterval(() => {
    if (!document.hidden) load();
  }, 60000);
  onDestroy(() => {
    clearInterval(timer);
    document.removeEventListener('visibilitychange', onVisibility);
  });

  return {
    charts,
    start(next: string[]) {
      uuids = next;
      load();
    },
  };
}
