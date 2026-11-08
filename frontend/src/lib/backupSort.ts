// Table sort of upstream application/backup/index.blade.php and
// service/volume-backup/index.blade.php (`filteredBackups`): the sort values of a
// row, sorted by `column_direction` with the target name as tie-breaker.
import { parseTimestamp } from './format';

export interface BackupSortValues {
  /** Lowercased target name. */
  name: string;
  /** Lowercased target type. */
  type: string;
  /** Lowercased frequency. */
  frequency: string;
  status: string;
  /** Number of S3 storages, -1 when S3 is off. */
  s3: number;
  /** Seconds of the last finished run, now while one runs, else 0. */
  lastRunAt: number;
  executions: number;
}

/** The `status` sort value: the latest run, else waiting or disabled. */
export function backupSortStatus(latest: string | null, enabled: boolean): string {
  if (latest === 'running') return 'in progress';
  if (latest === 'success') return 'success';
  if (latest === 'failed') return 'failed';
  return enabled ? 'waiting' : 'disabled';
}

/** The `lastRunAt` sort value. */
export function backupSortLastRun(finishedAt: string | null, latest: string | null): number {
  const finished = finishedAt ? parseTimestamp(finishedAt)?.getTime() : undefined;
  if (finished !== undefined) return Math.floor(finished / 1000);
  return latest === 'running' ? Math.floor(Date.now() / 1000) : 0;
}

export function sortBackups<T>(rows: T[], sortBy: string, values: (row: T) => BackupSortValues): T[] {
  const [column, direction] = sortBy.split('_');
  const pick: Record<string, (v: BackupSortValues) => string | number> = {
    target: (v) => v.name,
    type: (v) => v.type,
    schedule: (v) => v.frequency,
    status: (v) => v.status,
    s3: (v) => v.s3,
    lastrun: (v) => v.lastRunAt,
    executions: (v) => v.executions,
  };
  const sortValue = pick[column] ?? pick.target;
  const order = direction === 'desc' ? -1 : 1;
  return [...rows].sort((l, r) => {
    const lv = values(l);
    const rv = values(r);
    const a = sortValue(lv);
    const b = sortValue(rv);
    const diff = typeof a === 'number' && typeof b === 'number' ? a - b : String(a).localeCompare(String(b));
    return order * diff || lv.name.localeCompare(rv.name);
  });
}
