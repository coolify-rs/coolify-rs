// Page endpoints of the instance settings (crates/coolify-api/src/ui_settings.rs).
import { request } from './api';

export type Values = Record<string, any>;

export interface ActionResult {
  redirect?: string;
  message?: string;
  error?: string;
  errors?: Record<string, string[]>;
  settings?: Values;
  dns_error?: string | null;
  conflicts?: DomainConflict[];
  instance_timezone?: string;
  auto_update_frequency?: string;
  file_download_storage?: string;
  can_send_test?: boolean;
  upgrade?: UpgradeState;
  overrides?: Values;
}

export interface DomainConflict {
  domain: string;
  resource_name: string;
  resource_link: string;
  resource_type: string;
  service_name?: string;
}

export interface UpgradeState {
  available: boolean;
  latest_version: string;
  current_version: string;
}

export interface BackupPayload extends ActionResult {
  server_uuid: string | null;
  functional: boolean;
  is_backup_before_update_enabled: boolean;
  database: { name: string; description: string | null; uuid: string; postgres_user: string; status: string } | null;
  backup: { uuid: string; form: Values; storages: { id: number; name: string; is_usable: boolean }[] } | null;
}

const post = <T = ActionResult>(page: string, action: string, values: Values = {}) =>
  request<T>('POST', `/ui/settings/${page}`, { action, values });

export const settingsUi = {
  general: () => request<ActionResult & { timezones: string[] }>('GET', '/ui/settings/general'),
  generalAction: (action: string, values: Values) => post('general', action, values),
  advanced: () =>
    request<ActionResult & { storage_options: { value: string; label: string }[]; mcp_url: string }>('GET', '/ui/settings/advanced'),
  advancedAction: (action: string, values: Values) => post('advanced', action, values),
  updates: () => request<ActionResult & { autoupdate: unknown; upgrade: UpgradeState }>('GET', '/ui/settings/updates'),
  updatesAction: (action: string, values: Values = {}) => post('updates', action, values),
  upgradeState: () => request<UpgradeState>('GET', '/ui/settings/upgrade'),
  upgrade: () => request<ActionResult>('POST', '/ui/settings/upgrade', {}),
  email: () => request<ActionResult & { test_email: string }>('GET', '/ui/settings/email'),
  emailAction: (action: string, values: Values) => post('email', action, values),
  backup: () => request<BackupPayload>('GET', '/ui/settings/backup'),
  backupAction: (action: string, values: Values = {}) => post<BackupPayload>('backup', action, values),
  backupExecutions: (skip: number) => request<{ executions: Values[]; total: number }>('GET', `/ui/settings/backup/executions?skip=${skip}`),
};
