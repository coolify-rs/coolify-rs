// Client of `crates/coolify-api/src/ui_server_analytics.rs` with the display helpers of
// upstream bootstrap/helpers/shared.php the traffic views call.
import { get } from './serverApi';

export type Overview = {
  requests: number;
  bytesIn: number;
  bytesOut: number;
  s2xx: number;
  s3xx: number;
  s4xx: number;
  s5xx: number;
  latencyP50: number;
  latencyP95: number;
  latencyP99: number;
  uniqueVisitors: number;
};
export type SeriesBucket = { bucket: number; s2xx: number; s3xx: number; s4xx: number; s5xx: number; requests: number; bytesIn: number; bytesOut: number; uniqueVisitors: number; p95: number };
export type AppRow = { uuid: string; name: string; domain: string | null; link: string | null; isService: boolean; requests: number; bandwidth: number };
export type HostRow = { host: string; requests: number; bandwidth: number };
export type PathRow = { path: string; domain: string | null; requests: number; bytesOut: number; s4xx: number; s5xx: number; p95: number };
export type BreakdownRow = { value: string; display: string; host: string | null; favicon: string | null; hidable: boolean; requests: number; bytesOut: number };
export type CountryRow = { value: string; name: string; requests: number; bytesOut: number };
export type CityRow = { key: string; country: string; label: string; name: string; detail: string; requests: number; bytesOut: number; lat: number | null; lon: number | null };
export type GeoMarkers = { countries: { code: string; requests: number }[]; cities: { lat: number; lon: number; requests: number }[] };
export type Option = { value: string; label: string; header?: boolean };

export type AnalyticsData = {
  chartId: string;
  scopedServerUuid: string | null;
  hasServers: boolean;
  serverOptions: Option[];
  appOptions: Option[];
  range: string;
  serverUuid: string;
  appUuid: string;
  eligibleDisabledServers: { uuid: string; name: string }[];
  nudgeKey: string;
  overview: Overview | null;
  ipMode: string;
  errorRate?: number;
  bandwidthBytes?: number;
  latencyApproximate?: boolean;
  uniquesApproximate?: boolean;
  topApps?: AppRow[];
  topHosts?: HostRow[];
  topPaths?: PathRow[];
  breakdowns?: Record<string, BreakdownRow[]>;
  attribution?: string | null;
  series?: SeriesBucket[];
  geoInit?: GeoMarkers;
  geo?: GeoMarkers;
  countries?: CountryRow[];
  cities?: CityRow[];
  device?: { labels: string[]; series: number[] };
};

export function fetchAnalytics(scoped: string | null, filters: { range: string; server: string; app: string }): Promise<AnalyticsData> {
  const query = new URLSearchParams();
  if (filters.range) query.set('range', filters.range);
  if (filters.server) query.set('server', filters.server);
  if (filters.app) query.set('app', filters.app);
  const qs = query.toString();
  const base = scoped ? `/server-page/${encodeURIComponent(scoped)}/analytics/data` : '/analytics';
  return get<AnalyticsData>(qs ? `${base}?${qs}` : base);
}

const round = (value: number, places: number) => {
  const factor = 10 ** places;
  return Math.round(value * factor) / factor;
};
export const round1 = (value: number) => round(value, 1);

/** `number_format`. */
export function numberFormat(value: number, decimals = 0): string {
  return value.toLocaleString('en-US', { minimumFractionDigits: decimals, maximumFractionDigits: decimals });
}

/** `compactNumber`. */
export function compactNumber(n: number): string {
  if (n < 1000) return String(n);
  const [divisor, suffix] = n >= 1_000_000_000 ? [1_000_000_000, 'B'] : n >= 1_000_000 ? [1_000_000, 'M'] : [1000, 'K'];
  return `${(n / divisor).toFixed(2).replace(/0+$/, '').replace(/\.$/, '')}${suffix}`;
}

/** `formatBytes`. */
export function formatBytes(bytes: number): string {
  if (!bytes || bytes < 0) return '0 B';
  const units = ['B', 'KB', 'MB', 'GB', 'TB', 'PB'];
  const exponent = Math.min(Math.floor(Math.log(bytes) / Math.log(1024)), units.length - 1);
  return `${round(bytes / 1024 ** exponent, 2)} ${units[exponent]}`;
}

/** `formatLatency`. */
export function formatLatency(ms: number): string {
  if (ms >= 60_000) return `${numberFormat(ms / 60_000, 1)} min`;
  if (ms >= 1000) return `${numberFormat(ms / 1000, 1)} s`;
  return `${numberFormat(ms, 1)} ms`;
}

/** `countryFlagUrl`. */
export function countryFlagUrl(a2: string, size = '24x18'): string | null {
  const code = a2.trim().toLowerCase();
  return /^[a-z]{2}$/.test(code) ? `https://flagcdn.com/${size}/${code}.png` : null;
}

/** `Str::limit`. */
export function strLimit(value: string, limit: number): string {
  return value.length <= limit ? value : `${value.slice(0, limit).trimEnd()}...`;
}
