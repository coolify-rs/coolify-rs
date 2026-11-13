// Page endpoints of the shared resource sections (crates/coolify-api/src/ui_shared).
import { request } from './api';

/** The route segment of a resource: what the shared section components take as `kind`. */
export type ResourceKind = 'application' | 'service' | 'database' | 'service-application' | 'service-database';

export const resPath = (kind: ResourceKind, uuid: string) => `/ui/resources/${kind}/${encodeURIComponent(uuid)}`;

export interface ComposeInfo {
  services: string[];
  default: string | null;
  operator: string | null;
  required: boolean;
}

export interface ManagedEnvRow {
  kind: 'managed';
  row_id: string;
  scope: 'production' | 'preview';
  id: number;
  uuid: string;
  key: string;
  comment: string | null;
  is_preview: boolean;
  is_multiline: boolean;
  is_literal: boolean;
  is_runtime: boolean;
  is_buildtime: boolean;
  is_required: boolean;
  is_shown_once: boolean;
  is_shared: boolean;
  is_magic: boolean;
  is_redis_credential: boolean;
  compose_info: ComposeInfo | null;
}

export interface HardcodedEnvRow {
  kind: 'hardcoded';
  row_id: string;
  scope: 'production' | 'preview';
  key: string;
  value: string | null;
  is_value_hidden: boolean;
  comment: string | null;
  service_name: string;
  compose_type: 'literal' | 'derived';
  references: string[];
}

export type EnvRow = ManagedEnvRow | HardcodedEnvRow;

export interface EnvList {
  rows: EnvRow[];
  total: number;
  page: number;
  last_page: number;
  per_page: number;
  service_options: string[];
  show_preview: boolean;
  can_manage: boolean;
  is_application: boolean;
  build_pack: string | null;
  is_env_sorting_enabled: boolean;
  use_build_secrets: boolean;
  include_coolify_default_variables: boolean;
  has_log_redaction: boolean;
  redact_all_env_values_in_logs: boolean;
}

export interface EnvListQuery {
  search?: string;
  filters?: string[];
  services?: string[];
  env?: string;
  sort?: string;
  page?: number;
  per_page?: number;
}

export interface EnvValues {
  value: string | null;
  real_value: string | null;
  is_shared: boolean;
  is_really_required: boolean;
  legacy_escaping_changes_value: boolean;
}

export type AvailableVars = Record<'team' | 'project' | 'environment' | 'server', string[]> & { vault_keys_url?: string | null };

export interface EnvInput {
  key: string;
  value: string | null;
  /** A new value for a locked variable (`$newLockedValue`); null keeps the locked value. */
  new_locked_value?: string | null;
  comment: string | null;
  is_multiline: boolean;
  is_literal: boolean;
  is_runtime: boolean;
  is_buildtime: boolean;
}

export function envsUi(kind: ResourceKind, uuid: string) {
  const base = `${resPath(kind, uuid)}/envs`;
  return {
    list: (q: EnvListQuery) => {
      const params = new URLSearchParams();
      if (q.search) params.set('search', q.search);
      if (q.filters?.length) params.set('filters', q.filters.join(','));
      if (q.services?.length) params.set('services', q.services.join(','));
      if (q.env) params.set('env', q.env);
      if (q.sort) params.set('sort', q.sort);
      if (q.page) params.set('page', String(q.page));
      if (q.per_page) params.set('per_page', String(q.per_page));
      return request<EnvList>('GET', `${base}?${params}`);
    },
    available: () => request<AvailableVars>('GET', `${base}/available`),
    dev: () => request<{ variables: string; variables_preview: string | null }>('GET', `${base}/dev`),
    add: (v: EnvInput) => request<{ message: string }>('POST', base, v),
    bulk: (variables: string, variablesPreview: string | null) =>
      request<{ message: string | null; error: string | null }>('POST', `${base}/bulk`, { variables, variables_preview: variablesPreview }),
    settings: (values: Record<string, boolean>) => request<{ message: string }>('POST', `${base}/settings`, values),
    values: (id: number) => request<EnvValues>('GET', `${base}/${id}`),
    copy: (id: number) => request<{ value: string | null }>('GET', `${base}/${id}/copy`),
    update: (id: number, v: EnvInput) => request<{ message: string }>('PATCH', `${base}/${id}`, v),
    composeDefault: (id: number, v: EnvInput) => request<{ message: string | null }>('POST', `${base}/${id}/compose-default`, v),
    lock: (id: number) => request<{ message: string | null }>('POST', `${base}/${id}/lock`, {}),
    destroy: (id: number) => request<{ message: string }>('DELETE', `${base}/${id}`),
    resolve: (value: string | null, isPreview: boolean) =>
      request<{ value: string | null }>('POST', `${base}/resolve`, { value, is_preview: isPreview }),
  };
}
