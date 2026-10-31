// Page endpoints of the tags screens (crates/coolify-api/src/ui_tags.rs).
import { request } from './api';

export interface TagRow {
  id: number;
  uuid: string;
  name: string;
  applications_count: number;
  services_count: number;
}

export interface TagResource {
  id: number;
  uuid: string;
  name: string;
  description: string | null;
  project: string;
  project_uuid: string;
  environment: string;
  environment_uuid: string;
  build_pack?: string | null;
  source_type?: string | null;
  fqdn?: string | null;
  is_http_basic_auth_enabled?: boolean;
}

export interface TagDetails {
  found: boolean;
  tag: TagRow;
  webhook: string;
  applications: TagResource[];
  services: TagResource[];
  deployments: { id: number; application_name: string; deployment_url: string | null; server_name: string; status: string }[];
  can_delete: boolean;
}

export interface Connectable {
  context: string;
  uuid: string;
  name: string;
  tagged: boolean;
  can_update: boolean;
}

const tag = (name: string) => `/ui/tags/${encodeURIComponent(name)}`;

export const tagsUi = {
  index: () => request<{ tags: TagRow[]; can_create: boolean }>('GET', '/ui/tags'),
  create: (name: string) => request<{ name: string }>('POST', '/ui/tags', { name }),
  show: (name: string) => request<TagDetails>('GET', tag(name)),
  destroy: (name: string) => request<unknown>('DELETE', tag(name)),
  redeploy: (name: string) => request<{ message: string }>('POST', `${tag(name)}/redeploy`, {}),
  connectable: (name: string) => request<{ resources: Connectable[] }>('GET', `${tag(name)}/connectable`),
  toggle: (name: string, uuid: string) => request<{ tagged: boolean }>('POST', `${tag(name)}/toggle`, { uuid }),
};
