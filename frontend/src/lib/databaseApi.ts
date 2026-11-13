// Page endpoints of the database screens (crates/coolify-api/src/ui_database_*.rs).
import { csrf, request } from './api';
import { notify } from './toast.svelte';

type Toast = { type: 'success' | 'error' | 'info'; message: string };

/** Answers of a Livewire action: `dispatch('success'|'error'|'info', ...)` are its `toasts`. */
export interface ActionResult {
  toasts?: Toast[];
  [key: string]: unknown;
}

/** Shows the toasts of an answer and hands it back. `true` in `ok` means no error toast came with it. */
export function showToasts<T extends ActionResult>(r: T): T & { ok: boolean } {
  let good = true;
  for (const t of r.toasts ?? []) {
    if (t.type === 'success') notify.success(t.message);
    else if (t.type === 'error') {
      good = false;
      notify.failure(t.message);
    } else notify.info('Info', t.message);
  }
  return { ...r, ok: good };
}

const base = (uuid: string) => `/ui/databases/${encodeURIComponent(uuid)}`;
const post = <T extends ActionResult = ActionResult>(path: string, body: unknown = {}) => request<T>('POST', path, body).then(showToasts);

export interface SqliteConnectState {
  connections: { id: number; mount_path: string; application_name: string | null; href: string | null }[];
  compose_connections: { id: number; name: string; href: string }[];
  application_options: { value: string; label: string }[];
  volume_name: string;
  sqlite_databases: string | null;
}

export const generalUi = {
  /** `General::instantSave`. */
  setPublic: (uuid: string, isPublic: boolean, form: Record<string, unknown>) => post(`${base(uuid)}/public`, { is_public: isPublic, form }),
  /** `General::instantSaveAdvanced`. */
  setLogDrain: (uuid: string, enabled: boolean, form: Record<string, unknown>) => post(`${base(uuid)}/log-drain`, { is_log_drain_enabled: enabled, form }),
  /** `HasDatabaseStatusInfo::instantSaveSSL`. */
  setSsl: (uuid: string, enableSsl: boolean, sslMode: string | null) => post(`${base(uuid)}/ssl`, { enable_ssl: enableSsl, ssl_mode: sslMode }),
  /** `HasDatabaseStatusInfo::regenerateSslCertificate`. */
  regenerateSsl: (uuid: string) => post(`${base(uuid)}/ssl/regenerate`),
  proxyLogs: (uuid: string, lines: number, timestamps: boolean) =>
    request<{ logs: string }>('GET', `${base(uuid)}/proxy-logs?lines=${lines}&timestamps=${timestamps}`),
  /** `save_new_init_script`, `save_init_script` and `delete_init_script`. */
  initScript: (uuid: string, body: { action: 'add' | 'save' | 'delete'; filename: string; content?: string; original_filename?: string }) =>
    post(`${base(uuid)}/init-scripts`, body),
  /** `Health::submit` and `toggleHealthcheck`. */
  health: (uuid: string, body: Record<string, unknown>) => post<ActionResult & { health_check_enabled?: boolean }>(`${base(uuid)}/healthcheck`, body),
  anonymousVolume: (uuid: string) => request<{ volume: string | null }>('GET', `${base(uuid)}/anonymous-volume`),
  keepAnonymousVolume: (uuid: string) => post(`${base(uuid)}/anonymous-volume`),
  sqliteState: (uuid: string) => request<SqliteConnectState>('GET', `${base(uuid)}/sqlite/connect`),
  sqliteConnect: (uuid: string, applicationUuid: string, mountPath: string) =>
    post<ActionResult & { compose_snippet?: string; compose_application_name?: string; redirect?: string }>(`${base(uuid)}/sqlite/connect`, {
      application_uuid: applicationUuid,
      mount_path: mountPath,
    }),
  sqliteUnlink: (uuid: string, volume: number) => post(`${base(uuid)}/sqlite/unlink/${volume}`),
};

export interface BackupSchedule {
  id: number;
  uuid: string;
  frequency: string;
  enabled: boolean;
  save_s3: boolean;
  s3_names: string;
  latest_status: string | null;
  executions_count: number;
}

export interface BackupForm {
  enabled: boolean;
  frequency: string;
  database_backup_retention_amount_locally: string;
  database_backup_retention_days_locally: string;
  database_backup_retention_max_storage_locally: string;
  database_backup_retention_amount_s3: string;
  database_backup_retention_days_s3: string;
  database_backup_retention_max_storage_s3: string;
  save_s3: boolean;
  disable_local_backup: boolean;
  databases_to_backup: string;
  dump_all: boolean;
  timeout: string;
  missing_backup_notification_days: string;
  s3_storage_ids: number[];
}

export interface BackupEdit {
  uuid: string;
  database_type: string;
  /** `$isServiceSqlDatabase` of the view. */
  is_service_sql_database?: boolean;
  form: BackupForm;
  timezone: string;
  storages: { id: number; name: string; is_usable: boolean }[];
}

export interface BackupExecution {
  id: number;
  uuid: string;
  status: string;
  message: string | null;
  size: string | number | null;
  filename: string | null;
  database_name: string | null;
  created_at: string;
  updated_at: string;
  finished_at: string | null;
  local_storage_deleted: boolean;
  s3_uploaded: boolean | null;
  s3_storage_deleted: boolean;
  replicas: { id: number; s3_name: string | null; s3_uploaded: boolean | null; s3_storage_deleted: boolean; message: string | null }[];
}

export const backupsUi = {
  index: (uuid: string) => request<{ schedules?: BackupSchedule[]; unavailable?: boolean }>('GET', `${base(uuid)}/backups`),
  create: (uuid: string, frequency: string) => post<ActionResult & { backup_uuid?: string }>(`${base(uuid)}/backups`, { frequency }),
  edit: (uuid: string, backup: string) => request<BackupEdit>('GET', `${base(uuid)}/backups/${backup}`),
  action: (uuid: string, backup: string, action: 'submit' | 'toggleS3' | 'toggleEnabled' | 'backupNow', form?: BackupForm) =>
    post<ActionResult & { backup?: BackupEdit; redirect_executions?: boolean }>(`${base(uuid)}/backups/${backup}/action`, { action, form }),
  remove: (uuid: string, backup: string, password: string, selected: string[]) =>
    post<ActionResult & { deleted?: boolean }>(`${base(uuid)}/backups/${backup}/delete`, { password, selected }),
  executions: (uuid: string, backup: string, skip: number) =>
    request<{ executions: BackupExecution[]; total: number; server_timezone: string | null; s3_downloads: boolean; dump_all: boolean }>(
      'GET',
      `${base(uuid)}/backups/${backup}/executions?skip=${skip}`,
    ),
  cleanupFailed: (uuid: string, backup: string) => post(`${base(uuid)}/backups/${backup}/executions/cleanup-failed`),
  cleanupDeleted: (uuid: string, backup: string, password: string) => post(`${base(uuid)}/backups/${backup}/executions/cleanup-deleted`, { password }),
  deleteExecution: (uuid: string, backup: string, id: number, password: string, selected: string[]) =>
    post(`${base(uuid)}/backups/${backup}/executions/${id}/delete`, { password, selected }),
};

export interface ImportFormState {
  unsupported: boolean;
  status: string;
  db_type: string;
  storages: { id: number; uuid: string; name: string; description: string | null }[];
  sqlite_files: string[];
  restore_command: string;
}

export const importUi = {
  form: (uuid: string) => request<ImportFormState>('GET', `${base(uuid)}/import`),
  command: (uuid: string, body: Record<string, unknown>) => request<{ restore_command: string }>('POST', `${base(uuid)}/import/command`, body),
  checkFile: (uuid: string, path: string) => post<ActionResult & { exists?: boolean }>(`${base(uuid)}/import/check-file`, { path }),
  checkS3: (uuid: string, s3StorageId: number | null, path: string) =>
    post<ActionResult & { size?: number; size_text?: string; s3_storage_uuid?: string }>(`${base(uuid)}/import/check-s3`, { s3_storage_id: s3StorageId, path }),
  run: (uuid: string, body: Record<string, unknown>) => post<ActionResult & { activity_id?: number }>(`${base(uuid)}/import`, body),
  /** One Dropzone chunk (`POST /databases/{uuid}/imports/uploads`). */
  uploadChunk: async (uuid: string, form: FormData) => {
    const res = await fetch(`/ui/api/v1/databases/${encodeURIComponent(uuid)}/imports/uploads`, {
      method: 'POST',
      body: form,
      credentials: 'same-origin',
      headers: { Accept: 'application/json', 'X-CSRF-Token': csrf() },
    });
    const body = await res.json().catch(() => ({}));
    if (!res.ok) throw new Error(body?.errors?.file?.[0] ?? body?.message ?? 'Upload failed.');
    return body as { upload_id: string; filename?: string; size?: number; done?: number };
  },
  activity: (uuid: string, id: number) =>
    request<{ id: number; status: string; output: string | null }>('GET', `/ui/api/v1/databases/${encodeURIComponent(uuid)}/imports/${id}`),
};
