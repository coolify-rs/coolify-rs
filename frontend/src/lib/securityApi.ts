// Page endpoints of the keys and tokens screens (crates/coolify-api/src/ui_security.rs).
import { request } from './api';

export interface ApiToken {
  id: number;
  name: string;
  abilities: string[];
  last_used_at: string | null;
  created_at: string;
  expires_at: string | null;
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
  apiTokens: () => request<{ tokens: ApiToken[]; elevated: boolean; api_enabled: boolean; instance_admin: boolean }>('GET', '/ui/security/api-tokens'),
  createApiToken: (body: { description: string; expires_in_days: string | null; permissions: string[] }) =>
    request<{ token: string }>('POST', '/ui/security/api-tokens', body),
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
