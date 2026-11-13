// Clients of the sources pages (`/ui/sources…`) and the private repository forms.
import { rawRequest as request } from './api';
import { page } from './page.svelte';

export type Provider = 'github' | 'gitlab' | 'gitea';

export interface SourceRow {
  uuid: string;
  name: string;
  type: Provider;
  provider: string;
  organization: string | null;
  is_connected: boolean;
}

export interface SourcesPage {
  sources: SourceRow[];
  can_create: boolean;
  can_make_system_wide: boolean;
  random_name: string;
}

export interface SourceApplication {
  uuid: string;
  name: string;
  git_repository: string | null;
  git_branch: string | null;
  environment: string;
  environment_uuid: string;
  project: string;
  project_uuid: string;
}

export interface Endpoints {
  fqdn: string | null;
  ipv4: string | null;
  ipv6: string | null;
  app_url: string;
}

export interface GithubSource {
  uuid: string;
  name: string;
  display_name: string;
  organization: string | null;
  api_url: string;
  html_url: string;
  custom_user: string;
  custom_port: number;
  app_id: number | null;
  installation_id: number | null;
  client_id: string | null;
  client_secret: string | null;
  webhook_secret: string | null;
  is_system_wide: boolean;
  private_key_id: number | null;
  contents: string | null;
  metadata: string | null;
  pull_requests: string | null;
  deployments: string | null;
  organization_self_hosted_runners: string | null;
  actions: string | null;
  webhook_events: string[] | null;
  missing_deployment_requirements: string[];
  missing_runner_requirements: string[];
  has_app_id: boolean;
  has_installation: boolean;
  is_connected: boolean;
  private_keys: { id: number; name: string }[];
  applications: SourceApplication[];
  endpoints: Endpoints;
  manifest_state: string;
  installation_path: string | null;
  permissions_path: string;
  settings_url: string;
  can_create: boolean;
  can_update: boolean;
  can_delete: boolean;
  can_make_system_wide: boolean;
}

export interface OauthSource {
  uuid: string;
  name: string;
  api_url: string;
  html_url: string;
  custom_user: string;
  custom_port: number;
  client_id: string | null;
  client_secret: string | null;
  has_client_secret: boolean;
  webhook_secret: string | null;
  organization: string | null;
  is_system_wide: boolean;
  private_key_id: number | null;
  redirect_uri: string | null;
  is_connected: boolean;
  private_keys: { id: number; name: string }[];
  applications: SourceApplication[];
  endpoints: Endpoints;
  oauth_state: string;
  can_update: boolean;
  can_delete: boolean;
  can_make_system_wide: boolean;
}

type Message = { message: string };

export const sourcesUi = {
  list: () => request<SourcesPage>('GET', '/ui/sources'),
  create: (p: Provider, body: unknown) => request<{ uuid: string; previews?: number; runners?: number }>('POST', `/ui/sources/${p}`, body),
  github: (uuid: string) => request<GithubSource>('GET', `/ui/sources/github/${uuid}`),
  updateGithub: (uuid: string, body: unknown) => request<Message & { is_connected: boolean }>('PATCH', `/ui/sources/github/${uuid}`, body),
  githubManual: (uuid: string) => request<Message>('POST', `/ui/sources/github/${uuid}/manual`, {}),
  githubPermissions: (uuid: string) => request<Message>('POST', `/ui/sources/github/${uuid}/permissions`, {}),
  test: (p: Provider, uuid: string) => request<Message>('POST', `/ui/sources/${p}/${uuid}/test`, {}),
  githubSyncName: (uuid: string, body: unknown) =>
    request<{ message?: string; info?: string; name?: string }>('POST', `/ui/sources/github/${uuid}/sync-name`, body),
  remove: (p: Provider, uuid: string) => request<Message>('DELETE', `/ui/sources/${p}/${uuid}`),
  oauth: (p: Provider, uuid: string) => request<OauthSource>('GET', `/ui/sources/${p}/${uuid}`),
  updateOauth: (p: Provider, uuid: string, body: unknown) => request<Message>('PATCH', `/ui/sources/${p}/${uuid}`, body),
  persistRedirect: (p: Provider, uuid: string, redirect_uri: string) => request<unknown>('POST', `/ui/sources/${p}/${uuid}/redirect-uri`, { redirect_uri }),
};

export interface PrivateSourceOption {
  id: number;
  uuid: string;
  name: string;
  organization: string | null;
  is_connected: boolean;
  html_url: string;
  group_name?: string | null;
}

export const privateRepoUi = {
  sources: (p: Provider) => request<{ sources: PrivateSourceOption[]; can_create: boolean }>('GET', `/ui/private-repository/${p}/sources`),
  repositories: (p: Provider, sourceId: number, page: number) =>
    request<{ total_count: number; has_more?: boolean; repositories: Record<string, unknown>[]; token?: string; error?: string }>(
      'POST',
      `/ui/private-repository/${p}/repositories`,
      { source_id: sourceId, page },
    ),
  branches: (p: Provider, body: unknown) => request<{ branches: { name: string }[]; error?: string }>('POST', `/ui/private-repository/${p}/branches`, body),
  scan: (p: string, body: unknown) => request<{ detection: unknown; error?: string }>('POST', `/ui/private-repository/${p}/scan`, body),
  create: (p: string, body: unknown) => request<{ uuid: string }>('POST', `/ui/private-repository/${p}`, body),
  keys: () => request<{ keys: { id: number; name: string; uuid: string }[] }>('GET', '/ui/private-repository/keys'),
  dockerfile: (body: unknown) => request<{ uuid: string }>('POST', '/ui/new/dockerfile', body),
  compose: (body: unknown) => request<{ uuid: string }>('POST', '/ui/new/docker-compose', body),
};

/** The source switcher of top-breadcrumb.blade.php. */
export async function setSourceCrumbs(type: Provider, uuid: string, connected: boolean): Promise<void> {
  const { sources } = await sourcesUi.list();
  const sorted = [...sources].sort((a, b) => (a.name < b.name ? -1 : a.name > b.name ? 1 : 0));
  const current = sorted.find((s) => s.uuid === uuid && s.type === type);
  if (!current) return;
  page.switchers = [
    {
      heading: 'Sources',
      title: 'Switch sources',
      label: current.name || 'Source',
      uuid: current.uuid,
      items: sorted.map((s) => ({ label: s.name, uuid: s.uuid, href: `/source/${s.type}/${s.uuid}`, active: s === current })),
    },
  ];
  page.sourceConnected = connected;
}
