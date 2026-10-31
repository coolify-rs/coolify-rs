// Page endpoints of the shared variables screens (crates/coolify-api/src/ui_shared_variables.rs).
import { request } from './api';

export interface ScopeRef {
  kind: string;
  uuid?: string;
  env?: string;
}

export interface SharedVariable {
  id: number;
  key: string;
  comment: string | null;
  is_multiline: boolean;
  is_literal: boolean;
  is_shown_once: boolean;
  read_only: boolean;
  value: string | null;
}

export interface ScopeData {
  found: boolean;
  title: string;
  project_uuid: string | null;
  environment_uuid: string | null;
  can_update: boolean;
  variables: SharedVariable[];
  dev_view: string;
}

export interface OverviewRow {
  key: string;
  scope: string;
  owner: string | null;
  link: string;
  resources: number;
  updated_at: string | null;
}

export interface Overview {
  scopes: Record<string, { variables: number; owners: number }>;
  recent: OverviewRow[];
  used: OverviewRow[];
  unused: OverviewRow[];
  missing: number;
}

export interface Owners {
  projects: {
    uuid: string;
    name: string;
    description: string | null;
    variables: number;
    environments: { uuid: string; name: string; description: string | null }[];
  }[];
  servers: { uuid: string; name: string; description: string | null; ip: string; functional: boolean }[];
}

const query = (s: ScopeRef) => {
  const q = new URLSearchParams({ kind: s.kind });
  if (s.uuid) q.set('uuid', s.uuid);
  if (s.env) q.set('env', s.env);
  return q.toString();
};

export const sharedVariablesUi = {
  overview: () => request<Overview>('GET', '/ui/shared-variables/overview'),
  owners: () => request<Owners>('GET', '/ui/shared-variables/owners'),
  scope: (s: ScopeRef) => request<ScopeData>('GET', `/ui/shared-variables/scope?${query(s)}`),
  add: (s: ScopeRef, body: { key: string; value: string; comment: string | null; is_multiline: boolean }) =>
    request<unknown>('POST', `/ui/shared-variables/scope/add?${query(s)}`, body),
  bulk: (s: ScopeRef, variables: string) => request<{ message: string | null }>('POST', `/ui/shared-variables/scope/bulk?${query(s)}`, { variables }),
  value: (id: number) => request<{ value: string | null }>('GET', `/ui/shared-variables/variable/${id}`),
  update: (id: number, body: { key: string; value: string | null; comment: string | null; is_multiline: boolean }) =>
    request<{ message: string }>('PATCH', `/ui/shared-variables/variable/${id}`, body),
  lock: (id: number) => request<unknown>('POST', `/ui/shared-variables/variable/${id}/lock`, {}),
  destroy: (id: number) => request<{ message: string }>('DELETE', `/ui/shared-variables/variable/${id}`),
};
