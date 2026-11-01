// Page endpoints of the preview deployments section (crates/coolify-api/src/ui_shared/previews.rs).
import { request } from './api';
import { resPath } from './sharedApi';

export interface DomainParts {
  scheme: string;
  host: string;
  port: string;
  path: string;
}

export interface PreviewDomainRow {
  url: string;
  service: string | null;
  public_url: string;
  redirect: string;
  internal_port: number | null;
  has_port_override: boolean;
  dns_status: string;
  dns_message: string;
  key: string;
  editing_parts: DomainParts;
}

export interface PreviewRow {
  id: number;
  pull_request_id: number;
  status: string | null;
  fqdn: string | null;
  pull_request_html_url: string | null;
  docker_registry_image_tag: string | null;
  restart_limit: number | null;
  rows: PreviewDomainRow[];
}

export interface PreviewsPage {
  can_update: boolean;
  git_based: boolean;
  is_github_app_source: boolean;
  is_compose: boolean;
  is_docker_image: boolean;
  enabled: boolean;
  public_enabled: boolean;
  preview_url_template: string;
  has_additional_servers: boolean;
  server_name: string | null;
  force_https: boolean;
  compose_services: string[];
  previews: PreviewRow[];
}

export interface PullRequest {
  number: number;
  title: string;
  html_url: string;
}

export interface DeployAnswer {
  deployment_uuid?: string;
  toast?: 'success' | 'error';
  title?: string;
  message?: string;
}

export interface DomainAnswer {
  message?: string;
  port_warning?: number;
}

type Answer = { message: string };
const base = (uuid: string) => `${resPath('application', uuid)}/previews`;

export const previewsUi = {
  page: (uuid: string) => request<PreviewsPage>('GET', base(uuid)),
  settings: (uuid: string, body: { enabled?: boolean; public_enabled?: boolean }) => request<Answer>('POST', `${base(uuid)}/settings`, body),
  template: (uuid: string, template: string) => request<Answer & { preview_url_template: string }>('POST', `${base(uuid)}/template`, { template }),
  resetTemplate: (uuid: string) => request<Answer & { preview_url_template: string }>('POST', `${base(uuid)}/template/reset`, {}),
  pullRequests: (uuid: string) => request<{ rate_limit_remaining: string | null; pull_requests: PullRequest[] }>('POST', `${base(uuid)}/pull-requests`, {}),
  add: (uuid: string, pull_request_id: number, html_url: string) => request<{ message?: string }>('POST', `${base(uuid)}/add`, { pull_request_id, html_url }),
  deploy: (uuid: string, body: { pull_request_id: number; html_url?: string | null; tag?: string | null; add?: boolean }) =>
    request<DeployAnswer>('POST', `${base(uuid)}/deploy`, body),
  rebuild: (uuid: string, pr: number) => request<DeployAnswer>('POST', `${base(uuid)}/${pr}/rebuild`, {}),
  manual: (uuid: string, pull_request_id: string, tag: string) => request<DeployAnswer>('POST', `${base(uuid)}/manual`, { pull_request_id, tag }),
  stop: (uuid: string, pr: number) => request<Answer>('POST', `${base(uuid)}/${pr}/stop`, {}),
  remove: (uuid: string, pr: number) => request<Answer>('DELETE', `${base(uuid)}/${pr}`),
  saveTag: (uuid: string, pr: number, tag: string) => request<Answer>('POST', `${base(uuid)}/${pr}/tag`, { tag }),
  domainAdd: (uuid: string, pr: number, body: DomainParts & { service?: string | null; force_port?: boolean }) =>
    request<DomainAnswer>('POST', `${base(uuid)}/${pr}/domains/add`, body),
  domainGenerate: (uuid: string, pr: number, service: string | null) => request<Answer>('POST', `${base(uuid)}/${pr}/domains/generate`, { service }),
  domainUpdate: (uuid: string, pr: number, body: DomainParts & { old_url: string; service: string | null; force_port?: boolean }) =>
    request<DomainAnswer>('POST', `${base(uuid)}/${pr}/domains/update`, body),
  domainRemove: (uuid: string, pr: number, key: string) => request<DomainAnswer>('POST', `${base(uuid)}/${pr}/domains/remove`, { key }),
  regenerateHost: (uuid: string) => request<{ host: string }>('POST', `${resPath('application', uuid)}/domains/regenerate`, {}),
};
