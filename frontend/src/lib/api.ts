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

/** One server as `GET /ui/servers` describes it (facts the server pages need). */
export interface ServerFacts {
  uuid: string;
  name: string;
  description: string | null;
  status: string;
  status_type: 'success' | 'warning' | 'error';
  functional: boolean;
  transferred: boolean;
  is_coolify_host: boolean;
  is_localhost: boolean;
  is_build: boolean;
  is_swarm: boolean;
  is_swarm_worker: boolean;
  proxy_set: boolean;
  proxy_status: string | null;
  cloud: boolean;
  resources: number;
}

export interface ServersPageData {
  servers: ServerFacts[];
  can_create: boolean;
  confirm: { text: boolean; password: boolean };
}

/** `GET /servers/{uuid}` (bridge) fields the general page edits. */
export interface ServerDetail {
  uuid: string;
  name: string;
  description: string | null;
  ip: string;
  user: string;
  port: number;
  settings: {
    connection_timeout: number;
    server_timezone: string;
    wildcard_domain: string | null;
    server_role: 'deployment' | 'build' | 'both' | null;
    is_build_server: boolean;
  };
  validation_logs?: string | null;
}

/** Checkpoints of a validation (`ValidateAndInstall` properties); `null` = not reached. */
export interface ValidationProgress {
  running: boolean;
  uptime: boolean | null;
  supported_os_type: boolean | null;
  prerequisites_installed: boolean | null;
  docker_installed: boolean | null;
  docker_compose_installed: boolean | null;
  docker_version: boolean | null;
  error: string | null;
}

export interface ValidationState {
  progress: ValidationProgress | null;
  validation_logs: string | null;
  available: boolean;
}

export const serversUi = {
  timezones: () => request<string[]>('GET', '/ui/timezones'),
  list: () => request<ServersPageData>('GET', '/ui/servers'),
  form: () => request<{ keys: { id: number; name: string }[]; name: string }>('GET', '/ui/servers/form'),
  create: (body: Record<string, unknown>) => request<{ uuid: string }>('POST', '/ui/servers', body),
  validation: (uuid: string) => request<ValidationState>('GET', `/ui/servers/${encodeURIComponent(uuid)}/validation`),
  validate: (uuid: string) => request<{ started: boolean }>('POST', `/ui/servers/${encodeURIComponent(uuid)}/validation`),
  remove: (uuid: string, password: string, actions: string[]) =>
    request<{ message: string }>('POST', `/ui/servers/${encodeURIComponent(uuid)}/delete`, { password, actions }),
};

/** A standalone database as `GET /databases/{uuid}` presents it (PostgreSQL only so far). */
export interface DatabaseRecord {
  uuid: string;
  name: string;
  description: string | null;
  /** `status:health`, e.g. `running:healthy`, `exited:unhealthy`. */
  status: string;
  database_type: string;
  image: string;
  postgres_user: string;
  postgres_password?: string;
  postgres_db: string;
  postgres_initdb_args: string | null;
  postgres_host_auth_method: string | null;
  postgres_conf: string | null;
  init_scripts?: { index: number; filename: string; content: string }[] | null;
  ports_mappings: string | null;
  is_public: boolean;
  public_port: number | null;
  public_port_timeout: number | null;
  is_log_drain_enabled: boolean;
  custom_docker_run_options: string | null;
  enable_ssl: boolean;
  ssl_mode: string;
  internal_db_url?: string;
  external_db_url?: string | null;
  server_status: boolean;
  restart_count: number;
  last_online_at: string | null;
  created_at: string;
  updated_at: string;
  destination?: {
    uuid: string;
    name: string;
    network: string;
    server?: { uuid: string; name: string; ip: string; settings: Record<string, unknown> };
  };
  environment?: { uuid: string; name: string; project: { uuid: string; name: string } };
  [key: string]: unknown;
}

/** One log entry of a start (`activity_log.description`). */
export interface ActivityEntry {
  type: 'out' | 'err' | 'stdout' | 'stderr';
  output: string;
  timestamp: number;
  order: number;
}

export interface DatabaseState {
  /** Latest start (or restart) of the database. */
  activity: { id: number; status: 'queued' | 'in_progress' | 'finished' | 'error'; entries: ActivityEntry[] } | null;
  /** The saved configuration differs from the one the container was started with (`isConfigurationChanged`). */
  configuration_changed: boolean;
  /** `can('update', $database)`: admins and owners. */
  can_update: boolean;
  /** The database type: `postgresql`, `redis`, `keydb`, ... */
  kind?: string;
  /** What the General form and Connection block read besides the record. */
  form?: DatabaseFormExtras;
}

export interface DatabaseFormExtras {
  /** `isCredentialLocked()`: the password variable is shown once (Redis, KeyDB, Dragonfly). */
  credential_locked: boolean;
  /** Redis: the REDIS_USERNAME / REDIS_PASSWORD values (admins only; the password is null when locked). */
  redis_username?: string;
  redis_password?: string | null;
  /** Redis: the image version is 6.0 or later (the username field is shown). */
  redis_version_6?: boolean;
  shared_redis_username?: boolean;
  shared_redis_password?: boolean;
}

export interface DestinationServer {
  uuid: string;
  name: string;
  description: string | null;
  ip: string;
  functional: boolean;
  is_build: boolean;
  destinations: { uuid: string; name: string; network: string }[];
}

export const dbUi = {
  /** Resources of an environment. `unported` counts applications, services and database kinds coolify-rs cannot show yet. */
  resources: (project: string, env: string) =>
    request<{ databases: DatabaseRecord[]; unported: number }>(
      'GET',
      `/ui/projects/${encodeURIComponent(project)}/environments/${encodeURIComponent(env)}/resources`,
    ),
  /** Servers of the team that can host resources, with their destinations (the new-resource picker). */
  destinations: () => request<{ servers: DestinationServer[] }>('GET', '/ui/destinations'),
  /** `Project\Resource\Create` for a database: creates it and returns its UUID. Only `postgresql` is ported (others: 501). */
  create: (body: { project_uuid: string; environment_uuid: string; destination_uuid: string; type: string; database_image?: string }) =>
    request<{ uuid: string }>('POST', '/ui/databases', body),
  get: (uuid: string) => api.get<DatabaseRecord>(`/databases/${encodeURIComponent(uuid)}`),
  state: (uuid: string) => request<DatabaseState>('GET', `/ui/databases/${encodeURIComponent(uuid)}/state`),
  /** The General form (`<Type>\General::submit`). 422 errors are keyed by the snake_case field names. */
  update: (uuid: string, body: Record<string, unknown>) =>
    request<{ message: string }>('PATCH', `/ui/databases/${encodeURIComponent(uuid)}`, body),
  start: (uuid: string) => api.post<{ message: string }>(`/databases/${encodeURIComponent(uuid)}/start`, {}),
  stop: (uuid: string) => api.post<{ message: string }>(`/databases/${encodeURIComponent(uuid)}/stop`, {}),
  restart: (uuid: string) => api.post<{ message: string }>(`/databases/${encodeURIComponent(uuid)}/restart`, {}),
  /** Danger zone delete with password confirmation (`ConfirmModal` steps). */
  remove: (
    uuid: string,
    body: { password: string; delete_volumes: boolean; delete_configurations: boolean; docker_cleanup: boolean },
  ) => request<{ message: string }>('POST', `/ui/databases/${encodeURIComponent(uuid)}/delete`, body),
};
