// Page endpoints of the service screens (crates/coolify-api/src/ui_services.rs).
import { request, type DatabaseState } from './api';

export interface ServiceField {
  serviceName: string;
  key: string;
  name: string;
  value: string | null;
  isPassword: boolean;
  rules: string;
  customHelper: string | null;
  isLocked: boolean;
}

export interface ServicePart {
  id: number;
  uuid: string;
  name: string;
  human_name: string | null;
  description: string | null;
  image: string | null;
  status: string | null;
  fqdn: string | null;
  configuration_required: boolean;
  /** The part is no longer in the Docker Compose file. */
  is_removed_from_compose?: boolean;
  container_present: boolean | null;
  restart_limit_reached: boolean;
  restart_count: number | null;
  max_restart_count: number | null;
  is_migrated: boolean;
  backup_available: boolean;
}

export type ServiceActivity = NonNullable<DatabaseState['activity']>;

export interface ServicePage {
  service: {
    id: number;
    uuid: string;
    name: string;
    description: string | null;
    status: string | null;
    service_type: string | null;
    compose_parsing_version: string | null;
    connect_to_docker_network: boolean;
    is_container_label_escape_enabled: boolean;
    docker_compose_raw: string | null;
    docker_compose: string | null;
    created_at: string | null;
    updated_at: string | null;
  };
  documentation: string;
  logo: { src: string; fallback: string } | null;
  links: string[];
  is_deployable: boolean;
  server_functional: boolean;
  configuration_changed: boolean;
  activity: ServiceActivity | null;
  can_update: boolean;
  can_access_terminal: boolean;
  is_password_hidden: boolean;
  fields: ServiceField[];
  applications: ServicePart[];
  databases: ServicePart[];
}

export interface ServiceResourceDetails extends ServicePart {
  kind: 'application' | 'database';
  exclude_from_status: boolean;
  is_log_drain_enabled: boolean;
  url?: string | null;
  domain_count?: number;
  hides_domains?: boolean;
  is_gzip_enabled?: boolean;
  is_stripprefix_enabled?: boolean;
  certificate_resolver_mode?: 'server' | 'none' | 'custom';
  certificate_resolver_name?: string | null;
  show_certificate_resolver?: boolean;
  server_certificate_resolver?: string;
  required_port?: number | null;
  is_public?: boolean;
  public_port?: number | null;
  public_port_timeout?: number | null;
  db_url_public?: string | null;
}

export interface ServiceFileMount {
  id: number;
  fs_path: string;
  confirm_path: string;
  mount_path: string;
  is_directory: boolean;
  is_host_file: boolean;
  is_based_on_git: boolean;
  is_read_only: boolean;
  is_too_large: boolean;
  is_binary: boolean;
  has_backup: boolean;
  can_delete_from_server: boolean;
  content: string | null;
}

export interface ServiceStoragePart {
  id: number;
  uuid: string;
  name: string;
  kind: 'application' | 'database';
  volume_count: number;
  files: ServiceFileMount[];
  directories: ServiceFileMount[];
  external_volumes: { key: string; dockerName: string; service: string; mountPaths: string[] }[];
}

export interface ServiceTemplate {
  id: string;
  name: string;
  docsSlug: string;
  templateLastUpdated: string | null;
  website: string | null;
  logo: string;
  logo_cdn_url: string;
  logo_default_url: string;
  slogan: string | null;
  description: string | null;
  documentation: string | null;
  category: string | null;
  tags: string[] | null;
  amd_only: boolean | null;
  arm_only: boolean | null;
}

export interface ServiceListItem {
  uuid: string;
  name: string;
  description: string | null;
  status: string | null;
  logo: { src: string; fallback: string } | null;
  server: string | null;
  tags: { id: number | string; name: string }[];
}

export interface DomainParts {
  scheme: string;
  host: string;
  port: string;
  path: string;
}

export interface ServiceDomainApp {
  id: number;
  uuid: string;
  name: string;
  image: string | null;
  required_port: number | null;
  redirect: string;
  force_https: boolean;
  permanent_redirect: boolean;
}

export interface ServiceDomainRow {
  service_application_id: number;
  url: string;
  public_url: string;
  internal_port: number | null;
  has_port_override: boolean;
  dns_status: string;
  dns_message: string;
  is_noindexed: boolean;
  editing_parts: DomainParts;
}

export interface ServiceDomains {
  apps: ServiceDomainApp[];
  rows: ServiceDomainRow[];
  can_update: boolean;
}

interface DomainAnswer {
  message?: string;
  warnings?: string[];
  domain_conflicts?: { domain: string; resource_name: string; resource_link?: string | null; resource_type: string }[];
  port_warning?: number;
  /** The port of the domain that does not use the required port. */
  requested_port?: number | null;
}

const base = (uuid: string) => `/ui/services/${encodeURIComponent(uuid)}`;
const part = (uuid: string, resource: string) => `${base(uuid)}/resources/${encodeURIComponent(resource)}`;

export const servicesApi = {
  templates: () => request<{ services: ServiceTemplate[]; categories: string[]; last_updated: string | null }>('GET', '/ui/service-templates'),
  create: (body: { project_uuid: string; environment_uuid: string; destination_uuid: string; type: string }) =>
    request<{ uuid: string }>('POST', '/ui/services', body),
  page: (uuid: string) => request<ServicePage>('GET', base(uuid)),
  submit: (uuid: string, body: Record<string, unknown>) => request<{ message: string }>('PATCH', base(uuid), body),
  saveNetwork: (uuid: string, connect: boolean) =>
    request<{ message: string }>('POST', `${base(uuid)}/network`, { connect_to_docker_network: connect }),
  validateCompose: (uuid: string, compose: string) =>
    request<{ message: string }>('POST', `${base(uuid)}/compose/validate`, { docker_compose_raw: compose }),
  saveLabelEscape: (uuid: string, enabled: boolean) =>
    request<{ message: string }>('POST', `${base(uuid)}/compose/label-escape`, { enabled }),
  action: (uuid: string, action: string, body: Record<string, unknown> = {}) =>
    request<{ message: string }>('POST', `${base(uuid)}/action/${action}`, body),
  resource: (uuid: string, resource: string) =>
    request<{ service: { uuid: string; name: string }; resource: ServiceResourceDetails; can_update: boolean }>('GET', part(uuid, resource)),
  saveResource: (uuid: string, resource: string, body: Record<string, unknown>) =>
    request<{
      message?: string;
      warning?: string | null;
      domain_conflicts?: { domain: string; resource_name: string; resource_type: string; resource_link?: string | null; message: string }[];
      port_warning?: number;
      /** The port of the domain that does not use the required port. */
      requested_port?: number | null;
    }>('PATCH', part(uuid, resource), body),
  deleteResource: (uuid: string, resource: string, password: string) =>
    request<{ message: string; warning: boolean }>('DELETE', part(uuid, resource), { password }),
  restartResource: (uuid: string, resource: string) => request<{ message: string }>('POST', `${part(uuid, resource)}/restart`, {}),
  removeContainer: (uuid: string, resource: string) => request<{ message: string }>('POST', `${part(uuid, resource)}/remove-container`, {}),
  convertResource: (uuid: string, resource: string) => request<{ message: string; uuid: string }>('POST', `${part(uuid, resource)}/convert`, {}),
  resourceSettings: (uuid: string, resource: string, body: Record<string, unknown>) =>
    request<{ message: string }>('POST', `${part(uuid, resource)}/settings`, body),
  publicAccess: (uuid: string, resource: string, body: Record<string, unknown>) =>
    request<{ message: string }>('POST', `${part(uuid, resource)}/public`, body),
  domains: (uuid: string) => request<ServiceDomains>('GET', `${base(uuid)}/domains`),
  addDomain: (uuid: string, body: Record<string, unknown>) => request<DomainAnswer>('POST', `${base(uuid)}/domains`, body),
  updateDomain: (uuid: string, body: Record<string, unknown>) => request<DomainAnswer>('PATCH', `${base(uuid)}/domains`, body),
  removeDomain: (uuid: string, body: Record<string, unknown>) => request<DomainAnswer>('DELETE', `${base(uuid)}/domains`, body),
  domainSettings: (uuid: string, body: Record<string, unknown>) =>
    request<{ message: string }>('POST', `${base(uuid)}/domains/settings`, body),
  generateDomain: (uuid: string, appId?: number | null) => request<{ parts: DomainParts }>('POST', `${base(uuid)}/domains/generate`, appId ? { app_id: appId } : {}),
  storages: (uuid: string) => request<{ parts: ServiceStoragePart[] }>('GET', `${base(uuid)}/storages`),
  saveFile: (uuid: string, id: number, content: string | null) =>
    request<{ message: string }>('PATCH', `${base(uuid)}/file-storages/${id}`, { content }),
  loadFile: (uuid: string, id: number) => request<{ message: string }>('POST', `${base(uuid)}/file-storages/${id}/load`, {}),
  convertFile: (uuid: string, id: number, to: 'directory' | 'file') =>
    request<{ message: string }>('POST', `${base(uuid)}/file-storages/${id}/convert`, { to }),
  deleteFile: (uuid: string, id: number, password: string, selected: string[]) =>
    request<{ message: string }>('DELETE', `${base(uuid)}/file-storages/${id}`, { password, selected }),
};

/** `formatContainerStatus($status)`: `running:healthy` as `Running (healthy)`. */
export function formatContainerStatus(status: string | null | undefined): string {
  const s = status ?? '';
  const parts = s.split(':');
  const head = (v: string) =>
    v
      .replace(/[_-]+/g, ' ')
      .trim()
      .replace(/(^|[^a-zA-Z])([a-z])/g, (_, before, c) => before + c.toUpperCase());
  if (s.includes('(') || parts.length === 1) return head(s);
  const rest = parts.slice(1).filter((p) => p !== 'excluded');
  if (parts.includes('excluded')) return `${head(parts[0])}${rest.length ? ` (${rest[0]})` : ''} (excluded)`;
  return `${head(parts[0])} (${rest[0]})`;
}

/** `Str::headline`. */
export function headline(v: string): string {
  return v
    .replace(/([a-z0-9])([A-Z])/g, '$1 $2')
    .replace(/[_-]+/g, ' ')
    .trim()
    .replace(/\s+/g, ' ')
    .replace(/\b\w/g, (c) => c.toUpperCase());
}
