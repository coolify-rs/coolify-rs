// Port of the functions of upstream resources/js/runtime-logs.js.

export const MAX_CLIENT_LOG_LINES = 50000;
export const STREAM_INTERVAL_MS = 2000;
export const ESTIMATED_ROW_HEIGHT = 30;
export const ESTIMATED_TEXT_ROW_HEIGHT = 20;

const TIMESTAMP_PATTERN = /^(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?Z) (.*)$/s;
const SOURCE_PREFIX = /^([^\t]+)\t(.*)$/s;
const SOURCE_COLORS = ['#6f6c78', '#6d7ea8', '#6e8b74', '#a98462', '#8d7398', '#6f8f95'];

export type LogLevel = 'error' | 'warning' | 'debug' | 'info';
export type LogFilters = Record<LogLevel, boolean>;
export interface LogLine {
  readonly id: number;
  readonly ts: string;
  readonly text: string;
  readonly source: string;
  readonly sourceColor: string;
  readonly lower: string;
  readonly level: LogLevel;
}

function sourceColor(label: string): string {
  let hash = 0;
  for (let i = 0; i < label.length; i++) {
    hash = (hash * 31 + label.charCodeAt(i)) >>> 0;
  }
  return SOURCE_COLORS[hash % SOURCE_COLORS.length];
}

export function formatLogDetails(content: string): string {
  const trimmed = String(content ?? '').trim();
  if (!trimmed.startsWith('{') && !trimmed.startsWith('[')) return content;
  try {
    return JSON.stringify(JSON.parse(trimmed), null, 2);
  } catch {
    return content;
  }
}

/** A row can expand only when the detail shows more than the row. */
export function isExpandableLog(content: string, truncated: boolean): boolean {
  return truncated || formatLogDetails(content) !== content;
}

export function getLogLevel(content: string): LogLevel {
  if (/\b(error|err|failed|failure|exception|fatal|panic|critical)\b/.test(content)) return 'error';
  if (/\b(warn|warning|wrn|caution)\b/.test(content)) return 'warning';
  if (/\b(debug|dbg|trace|verbose)\b/.test(content)) return 'debug';
  return 'info';
}

export function parseLogLines(output: string, nextId: () => number): LogLine[] {
  const lines: LogLine[] = [];
  for (const rawLine of String(output ?? '').split('\n')) {
    if (rawLine.trim() === '') continue;
    const sourced = rawLine.match(SOURCE_PREFIX);
    const source = sourced ? sourced[1] : '';
    const body = sourced ? sourced[2] : rawLine;
    const match = body.match(TIMESTAMP_PATTERN);
    const ts = match ? match[1] : '';
    const text = match ? match[2] : body;
    const lower = `${source}\n${text}`.toLowerCase();
    lines.push(
      Object.freeze({
        id: nextId(),
        ts,
        text,
        source,
        sourceColor: source ? sourceColor(source) : '',
        lower,
        level: getLogLevel(text.toLowerCase()),
      }),
    );
  }
  return lines;
}

function linesSinceBoundary(existing: LogLine[], incoming: LogLine[]): LogLine[] {
  let lastTs = '';
  for (let i = existing.length - 1; i >= 0; i--) {
    if (existing[i].ts) {
      lastTs = existing[i].ts;
      break;
    }
  }
  if (!lastTs) return incoming;

  const seenAtBoundary = new Map<string, number>();
  for (let i = existing.length - 1; i >= 0 && existing[i].ts === lastTs; i--) {
    const key = existing[i].text;
    seenAtBoundary.set(key, (seenAtBoundary.get(key) ?? 0) + 1);
  }

  return incoming.filter((line) => {
    if (!line.ts) return true;
    if (line.ts < lastTs) return false;
    if (line.ts > lastTs) return true;
    const seen = seenAtBoundary.get(line.text) ?? 0;
    if (seen === 0) return true;
    seenAtBoundary.set(line.text, seen - 1);
    return false;
  });
}

function groupLinesBySource(entries: LogLine[]): Map<string, LogLine[]> {
  const groups = new Map<string, LogLine[]>();
  for (const line of entries) {
    const key = line.source || '';
    if (!groups.has(key)) groups.set(key, []);
    groups.get(key)!.push(line);
  }
  return groups;
}

export function newLinesSince(existing: LogLine[], incoming: LogLine[]): LogLine[] {
  const sourced = existing.some((line) => line.source) || incoming.some((line) => line.source);
  if (!sourced) return linesSinceBoundary(existing, incoming);

  const existingBySource = groupLinesBySource(existing);
  const fresh: LogLine[] = [];
  for (const [source, group] of groupLinesBySource(incoming)) {
    fresh.push(...linesSinceBoundary(existingBySource.get(source) ?? [], group));
  }
  fresh.sort((left, right) => {
    if (left.ts === right.ts) return (left.source || '').localeCompare(right.source || '');
    if (!left.ts) return 1;
    if (!right.ts) return -1;
    return left.ts < right.ts ? -1 : 1;
  });
  return fresh;
}

/** Ctrl+F or Cmd+F. The viewer takes it, because browser find cannot see rows that are not rendered. */
export function isFindShortcut(event: KeyboardEvent): boolean {
  return Boolean(event.ctrlKey || event.metaKey) && !event.altKey && !event.shiftKey && event.key?.toLowerCase() === 'f';
}

const DEFAULT_LOG_FILTERS: LogFilters = { error: true, warning: true, debug: true, info: true };

export function readLogFilters(saved: string | null): LogFilters {
  const filters = { ...DEFAULT_LOG_FILTERS };
  let parsed: Partial<LogFilters> | null;
  try {
    parsed = JSON.parse(saved ?? '');
  } catch {
    return filters;
  }
  for (const level of Object.keys(filters) as LogLevel[]) {
    const v = parsed?.[level];
    if (typeof v === 'boolean') filters[level] = v;
  }
  return filters;
}

export function filterLogLines(lines: LogLine[], filters: LogFilters, query: string): LogLine[] {
  const needle = query.trim().toLowerCase();
  return lines.filter((line) => filters[line.level] !== false && (!needle || line.lower.includes(needle)));
}

export function highlightSegments(text: string, query: string): { text: string; match: boolean }[] {
  const needle = query.trim().toLowerCase();
  if (!needle) return [{ text, match: false }];

  const segments: { text: string; match: boolean }[] = [];
  const lowerText = text.toLowerCase();
  let lastIndex = 0;
  let index = lowerText.indexOf(needle);
  while (index !== -1) {
    if (index > lastIndex) segments.push({ text: text.substring(lastIndex, index), match: false });
    segments.push({ text: text.substring(index, index + needle.length), match: true });
    lastIndex = index + needle.length;
    index = lowerText.indexOf(needle, lastIndex);
  }
  if (lastIndex < text.length) segments.push({ text: text.substring(lastIndex), match: false });
  return segments;
}

/** Format a Docker UTC timestamp like the server-side log viewer: 2025-Dec-04 11:48:39. */
export function createTimestampFormatter(timeZone: string): (ts: string) => string {
  let formatter: Intl.DateTimeFormat;
  const options: Intl.DateTimeFormatOptions = { year: 'numeric', month: 'short', day: '2-digit', hour: '2-digit', minute: '2-digit', second: '2-digit', hourCycle: 'h23' };
  try {
    formatter = new Intl.DateTimeFormat('en-US', { timeZone: timeZone || 'UTC', ...options });
  } catch {
    formatter = new Intl.DateTimeFormat('en-US', { timeZone: 'UTC', ...options });
  }
  return (ts: string) => {
    const date = new Date(ts.slice(0, 19) + 'Z');
    if (Number.isNaN(date.getTime())) return '';
    const parts = Object.fromEntries(formatter.formatToParts(date).map((part) => [part.type, part.value]));
    return `${parts.year}-${parts.month}-${parts.day} ${parts.hour}:${parts.minute}:${parts.second}`;
  };
}

export function downloadText(content: string, filename: string): void {
  const blob = new Blob([content], { type: 'text/plain' });
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url;
  a.download = filename;
  a.click();
  URL.revokeObjectURL(url);
}

export function prefersTextView(stored: string | null): boolean {
  return stored !== 'false';
}
