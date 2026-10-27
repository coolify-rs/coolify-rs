// Thin client for the coolify-rs session endpoints and the `/ui/api/v1`
// bridge. The bridge exposes the same handlers as the REST API, so the UI has
// no business logic of its own; it renders what the API returns.

export class ApiError extends Error {
  status: number;
  errors: Record<string, string[]>;
  details?: string;

  constructor(status: number, message: string, errors: Record<string, string[]> = {}, details?: string) {
    super(message);
    this.status = status;
    this.errors = errors;
    this.details = details;
  }

  /** First message for a field, if the API reported one. */
  field(name: string): string | undefined {
    return this.errors[name]?.[0];
  }
}

let csrfToken = '';

export function setCsrf(token: string): void {
  csrfToken = token;
}

/** Called when any request finds the session gone. */
let onUnauthenticated: () => void = () => {};
export function setUnauthenticatedHandler(fn: () => void): void {
  onUnauthenticated = fn;
}

async function request<T>(method: string, path: string, body?: unknown): Promise<T> {
  const headers: Record<string, string> = { Accept: 'application/json' };
  if (body !== undefined) headers['Content-Type'] = 'application/json';
  if (method !== 'GET') headers['X-CSRF-Token'] = csrfToken;
  const res = await fetch(path, {
    method,
    headers,
    credentials: 'same-origin',
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const text = await res.text();
  let data: unknown = undefined;
  try {
    data = text ? JSON.parse(text) : undefined;
  } catch {
    data = text;
  }
  if (!res.ok) {
    const d = (data ?? {}) as { message?: string; errors?: Record<string, string[]>; details?: string };
    if (res.status === 401 && !path.startsWith('/ui/session')) onUnauthenticated();
    throw new ApiError(res.status, d.message ?? `Request failed (${res.status})`, d.errors ?? {}, d.details);
  }
  return data as T;
}

const v1 = (p: string) => `/ui/api/v1${p}`;

export const api = {
  get: <T>(p: string) => request<T>('GET', v1(p)),
  post: <T>(p: string, body: unknown) => request<T>('POST', v1(p), body),
  patch: <T>(p: string, body: unknown) => request<T>('PATCH', v1(p), body),
  del: <T>(p: string) => request<T>('DELETE', v1(p)),
};

export const sessionApi = {
  current: () => request<SessionInfo>('GET', '/ui/session'),
  login: (body: { email: string; password: string; code?: string }) =>
    request<SessionInfo | { two_factor: true }>('POST', '/ui/session', body),
  logout: () => request<{ message: string }>('DELETE', '/ui/session'),
  switchTeam: (teamId: number) => request<SessionInfo>('POST', '/ui/session/team', { team_id: teamId }),
};

// ---- API shapes (subset of the upstream contract the UI reads) ----

export type Role = 'owner' | 'admin' | 'member';

export interface SessionInfo {
  user: { id: number; name: string; email: string; two_factor_enabled: boolean };
  teams: { id: number; name: string; role: Role }[];
  current_team: {
    id: number;
    name: string;
    role: Role;
    two_factor_required: boolean;
    two_factor_blocked: boolean;
  } | null;
  csrf_token: string;
  version: string;
  features: { private_keys: boolean; read_only: boolean };
}

export interface ProjectSummary {
  id: number;
  uuid: string;
  name: string;
  description: string | null;
}

export interface Environment {
  id: number;
  uuid: string;
  name: string;
  description: string | null;
  project_id?: number;
  created_at?: string;
  updated_at?: string;
}

export interface Project extends ProjectSummary {
  team_id: number;
  environments: Environment[];
  created_at: string;
  updated_at: string;
}

export interface PrivateKey {
  id: number;
  uuid: string;
  name: string;
  description: string | null;
  fingerprint: string | null;
  public_key: string;
  is_git_related: boolean;
  created_at: string;
  updated_at: string;
}

export interface Team {
  id: number;
  name: string;
  description: string | null;
  personal_team: boolean;
  is_build_server_fallback_enabled: boolean;
  is_two_factor_required?: boolean;
  created_at: string;
}

export interface Member {
  id: number;
  name: string;
  email: string;
  two_factor_confirmed_at: string | null;
  created_at: string;
}

/** `GET /ui/dashboard` (coolify-rs read model for the dashboard page). */
export interface DashboardData {
  projects: {
    uuid: string;
    name: string;
    description: string | null;
    created_at: string | null;
    environments: { uuid: string; name: string }[];
    resources: number;
  }[];
  servers: {
    uuid: string;
    name: string;
    description: string | null;
    status: string;
    status_type: 'success' | 'warning' | 'error';
  }[];
  private_keys: number;
  can_create: boolean;
}

export const uiApi = {
  dashboard: () => request<DashboardData>('GET', '/ui/dashboard'),
  project: (uuid: string) => request<ProjectPageData>('GET', `/ui/projects/${encodeURIComponent(uuid)}`),
};

/** `GET /ui/projects/{uuid}` (coolify-rs read model for project pages). */
export interface ProjectPageData {
  project: { uuid: string; name: string; description: string | null };
  environments: { uuid: string; name: string; description: string | null; created_at: string | null; resources: number }[];
  projects: { uuid: string; name: string }[];
  can_update: boolean;
  can_create: boolean;
}

/** `GET /ui/keys` (coolify-rs read model for the private keys page). */
export interface KeysPageData {
  keys: { uuid: string; name: string; description: string | null; in_use: boolean; can_view: boolean }[];
  can_create: boolean;
}

export const keysUi = {
  list: () => request<KeysPageData>('GET', '/ui/keys'),
  generate: (type: 'ed25519' | 'rsa') => request<{ uuid: string }>('POST', '/ui/keys/generate', { type }),
  cleanup: () => request<{ deleted: number }>('POST', '/ui/keys/cleanup', {}),
  inspect: (privateKey: string) => request<{ public_key: string | null }>('POST', '/ui/keys/inspect', { private_key: privateKey }),
};

/** `GET /ui/team` (coolify-rs read model for the team pages). */
export interface TeamPageData {
  team: {
    id: number;
    name: string;
    description: string | null;
    is_mcp_server_enabled: boolean;
    is_build_server_fallback_enabled: boolean;
    is_two_factor_required: boolean;
  };
  members: { id: number; name: string; email: string; role: Role; two_factor: boolean | null }[];
  me: number;
  my_role: Role;
  can_update: boolean;
  can_manage_members: boolean;
}

export const teamUi = {
  get: () => request<TeamPageData>('GET', '/ui/team'),
  update: (body: Omit<TeamPageData['team'], 'id'>) => request<{ message: string }>('PATCH', '/ui/team', body),
  setRole: (id: number, role: Role) => request<{ message: string }>('POST', `/ui/team/members/${id}/role`, { role }),
  remove: (id: number) => request<{ message: string }>('DELETE', `/ui/team/members/${id}`),
};
