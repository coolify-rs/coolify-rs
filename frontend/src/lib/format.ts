const MONTHS = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'];

/** PHP `format('M j, Y')` of a `YYYY-MM-DD ...` / ISO timestamp, read as UTC. */
export function monthDayYear(ts: string | null | undefined): string {
  const m = ts?.match(/^(\d{4})-(\d{2})-(\d{2})/);
  if (!m) return '-';
  return `${MONTHS[Number(m[2]) - 1]} ${Number(m[3])}, ${m[1]}`;
}

/** A stored timestamp (`YYYY-MM-DD HH:MM:SS`, ISO, with or without a zone) as a Date, read as UTC. */
export function parseTimestamp(ts: string | null | undefined): Date | null {
  if (!ts) return null;
  let s = ts.trim().replace(' ', 'T');
  if (!/(Z|[+-]\d{2}:?\d{2})$/.test(s)) s += 'Z';
  const d = new Date(s);
  return Number.isNaN(d.getTime()) ? null : d;
}

function zoned(d: Date, timeZone: string): Record<string, string> {
  let fmt: Intl.DateTimeFormat;
  try {
    fmt = new Intl.DateTimeFormat('en-US', { timeZone, hourCycle: 'h23', year: 'numeric', month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit', second: '2-digit', timeZoneName: 'short' });
  } catch {
    return zoned(d, 'UTC');
  }
  const parts: Record<string, string> = {};
  for (const p of fmt.formatToParts(d)) parts[p.type] = p.value;
  return parts;
}

/** `formatDateInServerTimezone()`: `Y-m-d H:i:s T` in the server's timezone. */
export function serverTime(ts: string | null | undefined, timeZone: string): string {
  const d = parseTimestamp(ts);
  if (!d) return '';
  const p = zoned(d, timeZone || 'UTC');
  return `${p.year}-${p.month}-${p.day} ${p.hour}:${p.minute}:${p.second} ${p.timeZoneName}`;
}

/** `decode_remote_command_output()`'s line timestamp: `Y-M-d H:i:s` in the server's timezone. */
export function logTime(ts: string | null | undefined, timeZone: string): string {
  const d = parseTimestamp(ts);
  if (!d) return '';
  const p = zoned(d, timeZone || 'UTC');
  return `${p.year}-${MONTHS[Number(p.month) - 1]}-${p.day} ${p.hour}:${p.minute}:${p.second}`;
}

const WEEKDAYS = ['Sun', 'Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat'];

/** Carbon `toDayDateTimeString()` in UTC: `Thu, Dec 25, 1975 2:15 PM`. Empty without a timestamp. */
export function dayDateTime(ts: string | null | undefined): string {
  const d = parseTimestamp(ts);
  if (!d) return '';
  const h = d.getUTCHours();
  return `${WEEKDAYS[d.getUTCDay()]}, ${MONTHS[d.getUTCMonth()]} ${d.getUTCDate()}, ${d.getUTCFullYear()} ${h % 12 || 12}:${String(d.getUTCMinutes()).padStart(2, '0')} ${h < 12 ? 'AM' : 'PM'}`;
}

/** Carbon `toIso8601String()` in UTC: `1975-12-25T14:15:16+00:00`. */
export function iso8601(ts: string | null | undefined): string {
  return parseTimestamp(ts)?.toISOString().replace(/\.\d{3}Z$/, '+00:00') ?? '';
}

const UNITS: [string, string, number][] = [
  ['year', 'yr', 365 * 86400],
  ['month', 'mo', 30 * 86400],
  ['week', 'w', 7 * 86400],
  ['day', 'd', 86400],
  ['hour', 'h', 3600],
  ['minute', 'm', 60],
  ['second', 's', 1],
];

/** Carbon `diffForHumans()` against now: `5 minutes ago`, or `5m` with `short` (absolute). */
export function diffForHumans(ts: string | null | undefined, short = false): string {
  const d = parseTimestamp(ts);
  if (!d) return '';
  const diff = Math.round((Date.now() - d.getTime()) / 1000);
  const abs = Math.abs(diff);
  // Carbon counts months and years on the calendar.
  const [early, late] = diff >= 0 ? [d, new Date()] : [new Date(), d];
  let months = (late.getUTCFullYear() - early.getUTCFullYear()) * 12 + late.getUTCMonth() - early.getUTCMonth();
  const anniversary = new Date(early);
  anniversary.setUTCMonth(early.getUTCMonth() + months);
  if (anniversary > late) months--;
  const calendar: [string, string, number] | undefined = months >= 12 ? UNITS[0] : months >= 1 ? UNITS[1] : undefined;
  const [unit, abbr, size] = calendar ?? UNITS.slice(2).find(([, , s]) => abs >= s) ?? UNITS[UNITS.length - 1];
  const count = months >= 12 ? Math.floor(months / 12) : months >= 1 ? months : Math.floor(abs / size);
  const n = Math.max(1, count);
  // Carbon's short English units: only `mo` and `yr` take a plural.
  if (short) return `${n}${abbr}${n > 1 && (abbr === 'mo' || abbr === 'yr') ? 's' : ''}`;
  const text = `${n} ${unit}${n === 1 ? '' : 's'}`;
  return diff >= 0 ? `${text} ago` : `${text} from now`;
}

/** `calculateDuration()`: `%Im %Ss`, with hours and days when present. */
export function duration(start: string | null | undefined, end: string | null | undefined): string | null {
  const a = parseTimestamp(start);
  const b = end === 'now' ? new Date() : parseTimestamp(end);
  if (!a || !b) return null;
  let secs = Math.floor(Math.abs(b.getTime() - a.getTime()) / 1000);
  const days = Math.floor(secs / 86400);
  secs -= days * 86400;
  const h = Math.floor(secs / 3600);
  const m = Math.floor((secs % 3600) / 60);
  const s = secs % 60;
  const pad = (n: number) => String(n).padStart(2, '0');
  if (days > 0) return `${days}d ${pad(h)}h ${pad(m)}m ${pad(s)}s`;
  if (h > 0) return `${pad(h)}h ${pad(m)}m ${pad(s)}s`;
  return `${pad(m)}m ${pad(s)}s`;
}
