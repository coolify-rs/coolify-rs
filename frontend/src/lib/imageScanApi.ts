// Client of the image scanning pages (`crates/coolify-api/src/ui_image_scan*.rs`).
import { request, ApiError } from './api';
import { copyTextToClipboard } from './clipboard';
import { notify } from './toast.svelte';

export type ResourceKind = 'application' | 'service' | 'database';

const base = (kind: ResourceKind, uuid: string) => `/ui/image-scan/${kind}/${encodeURIComponent(uuid)}`;

export interface Checked {
  human: string;
  title: string;
}

export interface WhyRow {
  title: string;
  display_id: string;
  link: string | null;
  score: string;
  cvss: string | null;
  cvssNote: string | null;
  severity: string;
  signal: string | null;
  exploited: boolean;
  package: string;
  version: string;
}

export interface ActionButton {
  label: string;
  title: string;
  submit: string;
  actions: string[];
  warning: string;
}

export interface CardAction {
  key: string;
  type: string;
  sentence: string;
  copy: string | null;
  button: ActionButton | null;
}

export interface ImageRow {
  key: string;
  name: string | null;
  ref: string;
  status: string;
  checking: boolean;
  badge: string | null;
  badgeType: string;
  open: boolean;
  sentence: string;
  attackNote: string | null;
  advice: string | null;
  lines: string[];
  primary: CardAction | null;
  mergedWith: string | null;
  why: WhyRow[];
  whyMore: number;
  provenance: string | null;
  others: CardAction[];
  moreActions: number;
  versionNotes: { text: string; link: string | null }[];
  filter: string | null;
  override: { service: string; from: string; to: string } | null;
}

export interface SecurityCard {
  status: string;
  checking: boolean;
  badge: string | null;
  badgeType: string;
  header: string | null;
  total: number;
  single: boolean;
  rows: ImageRow[];
}

export interface FindingRow {
  key: string;
  id: string;
  url: string | null;
  aliases: { id: string; url: string | null }[];
  package: string;
  version: string;
  fixed_in: string | null;
  severity: string;
  kev: boolean;
  summary: string;
  likelihood: string | null;
  base: string | null;
  image: string;
  owner: string;
  never_allowed: boolean;
  ignore: { reason: string; until: string | null } | null;
}

export interface PackageGroup {
  key: string;
  package: string;
  version: string;
  image: string;
  owner: string;
  severity: string;
  kev: boolean;
  never_allowed: boolean;
  fixed: string | null;
  count: number;
  rows: FindingRow[];
}

export interface Pagination {
  total: number;
  page: number;
  per_page: number;
  last_page: number;
}

export interface VulnerabilitiesData {
  off_state: 'server' | 'application' | null;
  server_name: string;
  server_url: string | null;
  can_update: boolean;
  is_application: boolean;
  is_service: boolean;
  has_scans?: boolean;
  scanning?: boolean;
  checkpoints?: { title: string; status: 'success' | 'running' | 'pending' }[];
  error_messages?: { message: string; detail: string | null }[];
  card?: SecurityCard | null;
  checked_at?: Checked | null;
  truncated?: boolean;
  finding_count?: number;
  finding_count_text?: string;
  state_counts?: { fixed: number; no_fix: number };
  fix_states?: { value: string; label: string }[];
  ignored_count?: number;
  shown_count?: number;
  pagination?: Pagination & { items: (FindingRow | PackageGroup)[] };
  grouped_finding_count?: number | null;
  image_options?: string[];
  shows_image?: boolean;
  can_ignore_for_team?: boolean;
  has_filters?: boolean;
  severity_options?: string[];
  fix_state?: string;
  view?: string;
  per_page?: number;
}

export interface ListState {
  fix_state: string;
  severities: string[];
  images: string[];
  view: string;
  search: string;
  show_ignored: boolean;
  page: number;
  per_page: number;
}

export interface PackagesData {
  has_scans: boolean;
  packages: { key: string; name: string; version: string; type: string; licenses: string[]; purl: string; image: string }[];
  image_options: string[];
  shows_image: boolean;
  truncated: boolean;
  downloads: { image: string; cyclonedx: string | null; spdx: string | null }[];
}

export interface ScanSettingsData {
  scope_type: string;
  can_edit: boolean;
  values: {
    gate_mode: string;
    on_scan_error: string;
    count_medium: string;
    app_scan_enabled: string;
    builds_by_team: string;
    scan_new_servers: string;
  } | null;
  inherit_labels: { gate_mode: string; on_scan_error: string; count_medium: string } | null;
  gate_mode_options: { value: string; label: string; description?: string }[];
  on_scan_error_options: { value: string; label: string }[];
  count_medium_options: { value: string; label: string }[];
  app_scan_on: boolean;
  is_docker_image: boolean;
  shows_ignores: boolean;
  shows_team_settings_link: boolean;
  ignores: {
    uuid: string;
    vulnerability_id: string | null;
    package: string | null;
    reason: string;
    expires: string | null;
    team_wide: boolean;
    set_for: string | null;
  }[];
}

/** A page action: its success toast, or the error toast. `true` when it succeeded. */
export async function run(fn: () => Promise<{ message?: string } | unknown>): Promise<boolean> {
  try {
    const r = (await fn()) as { message?: string } | null;
    if (r && typeof r === 'object' && r.message) notify.success(r.message);
    return true;
  } catch (e) {
    if (e instanceof ApiError) notify.failure(e.message);
    else notify.failure(String(e));
    return false;
  }
}

/** `response()->streamDownload()`: saves the content the server sent. */
export function saveDownload(r: { name: string; mime: string; content: string }): void {
  const a = document.createElement('a');
  a.href = URL.createObjectURL(new Blob([r.content], { type: r.mime }));
  a.download = r.name;
  document.body.appendChild(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(a.href), 1000);
}

/** The Copy entries of `x-image-scan.export-menu`. */
export function copyList(load: () => Promise<{ content: string }>, done: string): void {
  if (!navigator.clipboard) {
    notify.failure('The clipboard needs HTTPS or localhost.');
    return;
  }
  copyTextToClipboard(load().then((r) => r.content))
    .then(() => notify.success(done))
    .catch(() => notify.failure('Could not copy the list.'));
}

/** Parses the submit action of a confirmation button: `rebuild`, `updateImage('ref', 'uuid')`, `updateImages('key')`. */
export function parseSubmit(submit: string): { name: string; args: string[] } {
  const m = /^(\w+)(?:\((.*)\))?$/s.exec(submit.trim());
  if (!m) return { name: submit, args: [] };
  const args: string[] = [];
  const re = /'((?:\\.|[^'\\])*)'/g;
  let a: RegExpExecArray | null;
  while ((a = re.exec(m[2] ?? '')) !== null) args.push(a[1].replace(/\\(.)/g, '$1'));
  return { name: m[1], args };
}

export const vulnerabilitiesApi = {
  render: (kind: ResourceKind, uuid: string, state: ListState) => request<VulnerabilitiesData>('POST', `${base(kind, uuid)}/vulnerabilities`, state),
  scanNow: (kind: ResourceKind, uuid: string) => request<{ message: string }>('POST', `${base(kind, uuid)}/vulnerabilities/scan-now`, {}),
  updateImage: (kind: ResourceKind, uuid: string, candidate_ref: string, owner_uuid: string | null) =>
    request<{ message: string }>('POST', `${base(kind, uuid)}/vulnerabilities/update-image`, { candidate_ref, owner_uuid }),
  updateImages: (kind: ResourceKind, uuid: string, action_key: string) =>
    request<{ message: string }>('POST', `${base(kind, uuid)}/vulnerabilities/update-images`, { action_key }),
  rebuild: (kind: ResourceKind, uuid: string) => request<{ message: string }>('POST', `${base(kind, uuid)}/vulnerabilities/rebuild`, {}),
  removeImageOverride: (kind: ResourceKind, uuid: string, service: string) =>
    request<{ message?: string }>('POST', `${base(kind, uuid)}/vulnerabilities/remove-image-override`, { service }),
  ignore: (kind: ResourceKind, uuid: string, body: { target: string; kind: string; scope: string; expires: string; reason: string; owner: string | null }) =>
    request<{ message: string; saved: boolean }>('POST', `${base(kind, uuid)}/vulnerabilities/ignore`, body),
  export: <T>(kind: ResourceKind, uuid: string, state: ListState, format: string) =>
    request<T>('POST', `${base(kind, uuid)}/vulnerabilities/export`, { ...state, format }),
  packages: (kind: ResourceKind, uuid: string) => request<PackagesData>('GET', `${base(kind, uuid)}/packages`),
  status: (kind: ResourceKind, uuid: string) => request<{ status: string }>('GET', `${base(kind, uuid)}/status`),
};

const settingsPath = (scope: string, uuid?: string | null) =>
  `/ui/image-scan/settings/${scope}${uuid ? `/${encodeURIComponent(uuid)}` : ''}`;

export const scanSettingsApi = {
  get: (scope: string, uuid?: string | null) => request<ScanSettingsData>('GET', settingsPath(scope, uuid)),
  save: (scope: string, uuid: string | null | undefined, field: string, value: string) =>
    request<{ message: string }>('PATCH', settingsPath(scope, uuid), { field, value }),
  removeIgnore: (scope: string, uuid: string | null | undefined, ignore: string) =>
    request<{ message: string }>('DELETE', `${settingsPath(scope, uuid)}/ignores/${encodeURIComponent(ignore)}`),
};

/** The security dot of a resource menu (`SecurityStatus::stored()`); null when it cannot be read. */
export async function securityStatus(kind: ResourceKind, uuid: string): Promise<string | null> {
  try {
    return (await vulnerabilitiesApi.status(kind, uuid)).status;
  } catch {
    return null;
  }
}
