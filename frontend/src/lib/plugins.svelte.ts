// Plugin endpoints of the web UI (crates/coolify-api/src/ui_plugins.rs, ADR 0013). coolify-rs's own:
// Coolify has no plugins, so nothing here is a port. With no plugin installed the UI adds nothing.
import { ApiError, csrf, request } from './api';

export type Slot = 'dashboard' | 'application' | 'server' | 'settings';

/** A panel the session's team can see (`GET /ui/plugins/nav`). */
export interface PanelRef {
  plugin: string;
  name: string;
  verified: boolean;
  slot: Slot;
  /** Position among the plugin's panels in the slot. */
  index: number;
  title: string;
}

export interface NavInfo {
  /** Settings → Plugins (instance admins, when there is something to manage or install). */
  settings: boolean;
  /** Team → Plugins (an installed plugin is switched on for the instance). */
  team: boolean;
  panels: PanelRef[];
}

/** An installed plugin, as `GET /api/v1/plugins` shows it, with its panels. */
export interface InstalledPlugin {
  id: string;
  name: string;
  version: string;
  description: string;
  license: string;
  source: string | null;
  sha256: string;
  origin: string;
  verified: boolean;
  enabled: boolean;
  disabled_reason: string | null;
  capabilities: string[];
  enabled_for_team: boolean;
  /** Holds `deploy` (ADR 0011); on the team page only. */
  deploy?: boolean;
  /** The team allowed the plugin to queue its deployments; on the team page only. */
  deploy_granted_for_team?: boolean;
  panels: { slot: Slot; title: string }[];
}

export interface SettingsData {
  plugins: InstalledPlugin[];
  allow_upload: boolean;
  confirm: { text: boolean; password: boolean };
}

export interface TeamData {
  plugins: InstalledPlugin[];
  can_manage: boolean;
}

/** One plugin of the registry catalogue (`GET /api/v1/plugins/catalog`, docs/PLUGINS.md). */
export interface CatalogEntry {
  id: string;
  name: string;
  description: string;
  license: string;
  source: string | null;
  /** Newest version that is not yanked and runs here; null when none qualifies. */
  latest_version: string | null;
  capabilities: string[];
  verified: boolean;
  installed_version: string | null;
  installed_yanked: boolean;
  update_available: boolean;
  update_requires_approval: boolean;
  pending_update: { version: string; capabilities: string[]; added: string[]; found_at: string } | null;
  revoked: { reason: string } | null;
  versions: { version: string; yanked: boolean; compatible: boolean; capabilities: string[] }[];
}

export interface Catalog {
  /** The last verified index; null until the first refresh succeeds. */
  index: { url: string; key_id: string; generated_at: string; fetched_at: string } | null;
  plugins: CatalogEntry[];
}

/** The plugin a 422 "approve the capabilities" answer describes (uploads). */
export interface PendingPlugin {
  id: string;
  name: string;
  version: string;
  description: string;
  license: string;
  source: string | null;
}

/** What an install or update answers: done, or the capabilities to approve first. */
export type Approval =
  | { done: true }
  | {
      done: false;
      capabilities: string[];
      /** What the version asks for beyond the current grant (registry installs and updates). */
      added?: string[];
      /** The version to approve (registry installs and updates). */
      version?: string;
      plugin?: PendingPlugin;
      message: string;
    };

/** POST that turns the "approve these capabilities" 422 into a value; every other error throws. */
async function approving(path: string, body: unknown): Promise<Approval> {
  const res = await fetch(path, {
    method: 'POST',
    headers: { Accept: 'application/json', 'Content-Type': 'application/json', 'X-CSRF-Token': csrf() },
    credentials: 'same-origin',
    body: JSON.stringify(body),
  });
  let data: {
    message?: string;
    capabilities?: unknown;
    added?: unknown;
    version?: string;
    plugin?: PendingPlugin;
    errors?: Record<string, string[]>;
  } = {};
  try {
    data = await res.json();
  } catch {
    // Not JSON; the status says enough.
  }
  if (res.ok) return { done: true };
  if (res.status === 422 && Array.isArray(data.capabilities)) {
    return {
      done: false,
      capabilities: data.capabilities as string[],
      added: Array.isArray(data.added) ? (data.added as string[]) : undefined,
      version: data.version,
      plugin: data.plugin,
      message: data.message ?? '',
    };
  }
  throw new ApiError(res.status, data.message ?? `Request failed (${res.status})`, data.errors ?? {});
}

const path = (id: string) => `/ui/plugins/${id}`;

export const pluginsUi = {
  nav: () => request<NavInfo>('GET', '/ui/plugins/nav'),
  settings: () => request<SettingsData>('GET', '/ui/plugins'),
  upload: (body: { manifest: string; component: string; grant?: string[] }) => approving('/ui/plugins', body),
  setEnabled: (id: string, enabled: boolean) => request<InstalledPlugin>('PATCH', path(id), { enabled }),
  uninstall: (id: string, password: string) => request<{ message: string }>('DELETE', path(id), { password }),
  team: () => request<TeamData>('GET', '/ui/plugins/team'),
  /** `grantDeploy` is the team's answer for a plugin holding `deploy`; enabling one without it is refused. */
  setTeam: (id: string, enabled: boolean, grantDeploy?: boolean) =>
    request<InstalledPlugin>('POST', `${path(id)}/team`, { enabled, grant_deploy: grantDeploy }),
  catalog: () => request<Catalog>('GET', '/ui/plugins/catalog'),
  refreshCatalog: () =>
    request<{ plugins: number; revoked: string[]; pending_updates: string[] }>('POST', '/ui/plugins/catalog/refresh', {}),
  install: (id: string, grant?: string[], version?: string) => approving('/ui/plugins/catalog/install', { id, version, grant }),
  update: (id: string, grant?: string[], version?: string) => approving(`${path(id)}/update`, { version, grant }),
};

/** What the menus and pages need to know about plugins, for the session's team. */
class PluginNav {
  settings = $state(false);
  team = $state(false);
  panels = $state<PanelRef[]>([]);
  loaded = $state(false);

  of(slot: Slot): PanelRef[] {
    return this.panels.filter((p) => p.slot === slot);
  }

  #pending: Promise<void> | null = null;

  load(): Promise<void> {
    this.#pending = (async () => {
      try {
        const n = await pluginsUi.nav();
        this.settings = n.settings;
        this.team = n.team;
        this.panels = n.panels;
      } catch {
        // Plugins switched off (404), or signed out: nothing to add.
        this.settings = false;
        this.team = false;
        this.panels = [];
      } finally {
        this.loaded = true;
      }
    })();
    return this.#pending;
  }

  /** Resolves once the current load finished (at once when nothing is loading). */
  ready(): Promise<void> {
    return this.#pending ?? Promise.resolve();
  }

  reset(): void {
    this.#pending = null;
    this.settings = false;
    this.team = false;
    this.panels = [];
    this.loaded = false;
  }
}

export const pluginNav = new PluginNav();

/** Reads a file the user picked as text or as base64. */
export function readFile(file: File, as: 'text' | 'base64'): Promise<string> {
  return new Promise((resolve, reject) => {
    const r = new FileReader();
    r.onerror = () => reject(r.error ?? new Error('The file could not be read.'));
    r.onload = () => {
      const v = String(r.result ?? '');
      resolve(as === 'text' ? v : v.slice(v.indexOf(',') + 1));
    };
    if (as === 'text') r.readAsText(file);
    else r.readAsDataURL(file);
  });
}
