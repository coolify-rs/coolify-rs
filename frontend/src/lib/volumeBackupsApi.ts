// Page endpoints of the storage backup pages (crates/coolify-api/src/ui_volume_backups.rs).
import { request } from './api';
import { notify } from './toast.svelte';

export type BackupOwner = 'application' | 'service';

export interface S3Label {
  name: string;
  bucket: string;
  team_id: number;
}

export interface VolumeSchedule {
  id: number;
  uuid: string;
  target_type: string;
  target_name: string;
  frequency: string;
  enabled: boolean;
  save_s3: boolean;
  s3: S3Label[];
  latest_status: string | null;
  latest_finished_at: string | null;
  executions_count: number;
  created_at: string;
}

export interface ServiceDatabaseSchedule extends Omit<VolumeSchedule, 'target_type'> {
  database_uuid: string;
  database_status: string | null;
}

export interface BackupTargetOption {
  key: string;
  type: string;
  name: string;
  frequency: string | null;
}

export interface BackupsIndex {
  can_update: boolean;
  team_id: number;
  name: string;
  backups: VolumeSchedule[];
  database_backups: ServiceDatabaseSchedule[];
  database_targets: { uuid: string; name: string }[];
  databases: { uuid: string; backup_available: boolean; is_migrated: boolean }[];
  targets: BackupTargetOption[];
}

export interface VolumeBackupForm {
  frequency: string;
  enabled: boolean;
  save_s3: boolean;
  disable_local_backup: boolean;
  stop_during_backup: boolean;
  s3_storage_ids: number[];
  retention_amount_locally: string;
  retention_days_locally: string;
  retention_max_storage_locally: string;
  retention_amount_s3: string;
  retention_days_s3: string;
  retention_max_storage_s3: string;
  timeout: string;
  missing_backup_notification_days: string;
}

export interface VolumeBackupState {
  uuid: string;
  target_type: string;
  target_name: string;
  timezone: string;
  form: VolumeBackupForm;
  storages: { id: number; name: string; is_usable: boolean }[];
}

export interface VolumeReplica {
  id: number;
  s3_uploaded: boolean | null;
  s3_storage_deleted: boolean;
  message: string | null;
  s3_name: string | null;
}

export interface VolumeExecution {
  id: number;
  uuid: string;
  status: string;
  filename: string | null;
  message: string | null;
  size: string | number | null;
  created_at: string;
  updated_at: string;
  finished_at: string | null;
  local_storage_deleted: boolean;
  s3_uploaded: boolean | null;
  s3_storage_deleted: boolean;
  stop_recovery_pending: boolean;
  s3_cleanup_pending: boolean;
  recovery_needs_attention: boolean;
  recovery_error: string | null;
  replicas: VolumeReplica[];
}

export interface ServiceExecution {
  id: string;
  uuid: string;
  target: string;
  type: string;
  schedule: string;
  s3_tooltip: string;
  status: string;
  started_at: string;
  size: string | number | null;
  message: string | null;
  filename: string | null;
  download_url: string | null;
}

interface Toasted {
  toasts?: { type: string; message: string }[];
}

/** The toasts an endpoint answers with, shown the way `$this->dispatch('success', ...)` does. */
export function showToasts(r: Toasted) {
  for (const t of r.toasts ?? []) {
    if (t.type === 'success') notify.success(t.message);
    else if (t.type === 'error') notify.failure(t.message);
    else notify.info(t.message);
  }
}

async function post<T extends Toasted>(path: string, body: unknown = {}): Promise<T> {
  const r = await request<T>('POST', path, body);
  showToasts(r);
  return r;
}

const base = (kind: BackupOwner, uuid: string) => `/ui/volume-backups/${kind}/${encodeURIComponent(uuid)}`;

export const volumeBackupsUi = {
  index: (kind: BackupOwner, uuid: string) => request<BackupsIndex>('GET', base(kind, uuid)),
  create: (kind: BackupOwner, uuid: string, target_key: string, frequency: string) =>
    post<Toasted & { backup_uuid: string }>(base(kind, uuid), { target_key, frequency }),
  show: (kind: BackupOwner, uuid: string, backup: string) => request<VolumeBackupState>('GET', `${base(kind, uuid)}/${backup}`),
  action: (kind: BackupOwner, uuid: string, backup: string, action: string, form?: unknown) =>
    post<Toasted & { backup?: VolumeBackupState; ok?: boolean; redirect_executions?: boolean }>(`${base(kind, uuid)}/${backup}/action`, { action, form }),
  remove: (kind: BackupOwner, uuid: string, backup: string, password: string, selected: string[]) =>
    post<Toasted & { deleted?: boolean }>(`${base(kind, uuid)}/${backup}/delete`, { password, selected }),
  executions: (kind: BackupOwner, uuid: string, backup: string, page: number, perPage: number) =>
    request<{ executions: VolumeExecution[]; total: number; page: number; per_page: number; server_timezone: string | null }>(
      'GET',
      `${base(kind, uuid)}/${backup}/executions?page=${page}&per_page=${perPage}`,
    ),
  cleanupFailed: (kind: BackupOwner, uuid: string, backup: string) => post(`${base(kind, uuid)}/${backup}/executions/cleanup-failed`),
  cleanupDeleted: (kind: BackupOwner, uuid: string, backup: string, password: string) =>
    post(`${base(kind, uuid)}/${backup}/executions/cleanup-deleted`, { password }),
  retryRecovery: (kind: BackupOwner, uuid: string, backup: string, id: number) => post(`${base(kind, uuid)}/${backup}/executions/${id}/retry-recovery`),
  deleteExecution: (kind: BackupOwner, uuid: string, backup: string, id: number, password: string, selected: string[]) =>
    post<Toasted & { deleted?: boolean }>(`${base(kind, uuid)}/${backup}/executions/${id}/delete`, { password, selected }),
  backupNow: (service: string, type: 'database' | 'storage', backup: string) => post(`/ui/services/${encodeURIComponent(service)}/backup-now`, { type, backup }),
  serviceExecutions: (service: string, page: number, perPage: number) =>
    request<{ executions: ServiceExecution[]; total: number; page: number; per_page: number; last_page: number; server_timezone: string | null }>(
      'GET',
      `/ui/services/${encodeURIComponent(service)}/backup-executions?page=${page}&per_page=${perPage}`,
    ),
  importDatabases: (service: string) => request<{ databases: { uuid: string; name: string }[] }>('GET', `/ui/services/${encodeURIComponent(service)}/import-databases`),
};

/** `formatBytes`. */
export function formatBytes(value: string | number | null): string {
  const bytes = Number(value);
  if (!bytes) return '-';
  const units = ['B', 'KB', 'MB', 'GB', 'TB', 'PB'];
  const exp = Math.min(Math.floor(Math.log(bytes) / Math.log(1024)), units.length - 1);
  return `${Math.round((bytes / 1024 ** exp) * 100) / 100} ${units[exp]}`;
}

/** The label and type of the schedule rows' status badge. */
export function scheduleStatus(status: string | null, enabled: boolean): [string, 'neutral' | 'success' | 'warning' | 'error'] {
  switch (status) {
    case 'running':
      return ['In progress', 'warning'];
    case 'success':
      return ['Success', 'success'];
    case 'failed':
      return ['Failed', 'error'];
    default:
      return [enabled ? 'Waiting' : 'Disabled', 'neutral'];
  }
}
