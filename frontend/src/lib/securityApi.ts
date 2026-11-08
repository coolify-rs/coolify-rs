// Page endpoints of the keys and tokens screens (crates/coolify-api/src/ui_security.rs,
// crates/coolify-api/src/ui_api_tokens.rs).
import { request } from './api';

/** `ApiTokenForm::valuesOf`: what the edit modal opens with. */
export interface ApiTokenFormValues {
  id: number;
  description: string;
  permissions: string[];
  scopeMode: 'team' | 'selected';
  selectedScopes: string[];
  expiresInDays: string;
  keepLabel: string;
}

export interface ApiToken {
  id: number;
  name: string;
  abilities: string[];
  /** `null`: every resource of the team. */
  scopes: { scope: string; uuid: string }[] | null;
  paused: boolean;
  last_used_at: string | null;
  created_at: string;
  expires_at: string | null;
  own: boolean;
  edit: ApiTokenFormValues;
}

/** One option of `ApiTokens::scopeOptions`. */
export interface ScopeOption {
  value: string;
  label: string;
  header?: boolean;
  parents?: string[];
}

/** The `canUse*Permissions` and `canUseTeamScope` flags of the page. */
export interface TokenPermissionRights {
  root: boolean;
  write: boolean;
  deploy: boolean;
  sensitive: boolean;
  delete: boolean;
  team_scope: boolean;
}

/** The fields of an `ApiTokenForm`. */
export interface TokenForm {
  description: string;
  expiresInDays: string;
  permissions: string[];
  scopeMode: 'team' | 'selected';
  selectedScopes: string[];
}

export interface ApiTokensPage {
  tokens: ApiToken[];
  api_enabled: boolean;
  instance_admin: boolean;
  can_use: TokenPermissionRights;
  scope_options: ScopeOption[];
  scope_labels: Record<string, string>;
  scopes_supported: boolean;
  pause_supported: boolean;
}

export interface ApiTokenBody {
  description: string;
  expires_in_days: string | null;
  permissions: string[];
  scope_mode: 'team' | 'selected';
  selected_scopes: string[];
}

export interface CloudTokenRow {
  uuid: string;
  name: string;
  provider: string;
  description: string | null;
  created_at: string;
}

export interface ScriptRow {
  uuid: string;
  name: string;
  updated_at: string;
  created_at: string;
  script: string;
}

export interface IntegrationToken {
  uuid: string;
  name: string;
  provider: string;
  capabilities: string[];
  metadata: Record<string, any> | null;
  zones: { id: number; name: string; account_name: string | null; managed_records_count: number }[];
}

const enc = encodeURIComponent;

export const securityUi = {
  apiTokens: () => request<ApiTokensPage>('GET', '/ui/security/api-tokens'),
  createApiToken: (body: ApiTokenBody) => request<{ token: string }>('POST', '/ui/security/api-tokens', body),
  updateApiToken: (id: number, body: ApiTokenBody) => request<{ message: string }>('PATCH', `/ui/security/api-tokens/${id}`, body),
  setApiTokenPaused: (id: number, paused: boolean) => request<{ message: string }>('POST', `/ui/security/api-tokens/${id}/paused`, { paused }),
  rollApiToken: (id: number) => request<{ token: string }>('POST', `/ui/security/api-tokens/${id}/roll`, {}),
  revokeApiToken: (id: number) => request<unknown>('DELETE', `/ui/security/api-tokens/${id}`),

  cloudTokens: () => request<{ tokens: CloudTokenRow[] }>('GET', '/ui/security/cloud-tokens'),
  cloudToken: (uuid: string) => request<CloudTokenRow>('GET', `/ui/security/cloud-tokens/${enc(uuid)}`),
  addCloudToken: (body: { provider: string; token: string; name: string; description: string }) => request<{ message: string }>('POST', '/ui/security/cloud-tokens', body),
  updateCloudToken: (uuid: string, body: { name: string; description: string }) => request<{ message: string }>('PATCH', `/ui/security/cloud-tokens/${enc(uuid)}`, body),
  validateCloudToken: (uuid: string) => request<{ message: string }>('POST', `/ui/security/cloud-tokens/${enc(uuid)}/validate`, {}),
  deleteCloudToken: (uuid: string) => request<unknown>('DELETE', `/ui/security/cloud-tokens/${enc(uuid)}`),

  scripts: () => request<{ scripts: ScriptRow[] }>('GET', '/ui/security/cloud-init-scripts'),
  script: (uuid: string) => request<ScriptRow>('GET', `/ui/security/cloud-init-scripts/${enc(uuid)}`),
  createScript: (body: { name: string; script: string }) => request<{ message: string }>('POST', '/ui/security/cloud-init-scripts', body),
  updateScript: (uuid: string, body: { name: string; script: string }) => request<{ message: string }>('PATCH', `/ui/security/cloud-init-scripts/${enc(uuid)}`, body),
  deleteScript: (uuid: string) => request<unknown>('DELETE', `/ui/security/cloud-init-scripts/${enc(uuid)}`),

  integrationTokens: () => request<{ tokens: IntegrationToken[] }>('GET', '/ui/security/integration-tokens'),
  addIntegrationToken: (body: Record<string, unknown>) => request<{ message: string }>('POST', '/ui/security/integration-tokens', body),
  updateIntegrationToken: (uuid: string, body: Record<string, unknown>) => request<{ message: string }>('PATCH', `/ui/security/integration-tokens/${enc(uuid)}`, body),
  deleteIntegrationToken: (uuid: string) => request<{ message: string }>('DELETE', `/ui/security/integration-tokens/${enc(uuid)}`),
  refreshZones: (uuid: string) => request<{ message: string }>('POST', `/ui/security/integration-tokens/${enc(uuid)}/refresh-zones`, {}),
};

/** Carbon `diffForHumans()`. */
export function ago(iso: string | null): string {
  if (!iso) return '';
  const seconds = Math.max(0, Math.floor((Date.now() - new Date(iso + (/[zZ]|[+-]\d\d:?\d\d$/.test(iso) ? '' : 'Z')).getTime()) / 1000));
  const units: [number, string][] = [
    [31536000, 'year'],
    [2592000, 'month'],
    [604800, 'week'],
    [86400, 'day'],
    [3600, 'hour'],
    [60, 'minute'],
    [1, 'second'],
  ];
  for (const [size, unit] of units) {
    if (seconds >= size) {
      const n = Math.floor(seconds / size);
      return `${n} ${unit}${n === 1 ? '' : 's'} ago`;
    }
  }
  return '0 seconds ago';
}

/** Laravel's `Y-m-d` or `Y-m-d H:i` of a stored timestamp. */
export const dateOf = (iso: string | null) => (iso ?? '').slice(0, 10);
export const dateTimeOf = (iso: string | null) => (iso ?? '').slice(0, 16).replace('T', ' ');
