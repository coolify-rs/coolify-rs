// Shared loading for server pages: the server's facts (for the menu, the
// top bar and permissions) and the switcher list.
import { serversUi, type ServerFacts, type ServersPageData } from './api';
import { page } from './page.svelte';
import { session } from './session.svelte';
import { serverNavContext } from './serverMenu';
import { pluginNav } from './plugins.svelte';
import { router } from './router.svelte';

export async function loadServerContext(uuid: string, active: string, sub?: string): Promise<{ facts: ServerFacts; list: ServersPageData } | null> {
  const [list] = await Promise.all([serversUi.list(), pluginNav.ready()]);
  const facts = list.servers.find((s) => s.uuid === uuid);
  if (!facts) return null;
  page.navContext = { ...serverNavContext(facts, active, session.isAdmin, sub, pluginNav.of('server').length > 0), name: facts.name };
  page.server = {
    uuid: facts.uuid,
    name: facts.name,
    functional: facts.functional,
    proxySet: facts.proxy_set,
    proxyStatus: facts.proxy_status,
    proxyPending: facts.proxy_pending,
    traefikOutdated: facts.traefik_outdated,
    sentinelStatus: facts.sentinel_status,
    sentinelEnabled: facts.sentinel_enabled,
    isCoolifyHost: facts.is_coolify_host,
    servers: list.servers.map((s) => ({ uuid: s.uuid, name: s.name, functional: s.functional })),
  };
  return { facts, list };
}

/**
 * `$showProxyActions` of livewire/server/navbar.blade.php (v4.7.5): the proxy actions show on
 * `server.proxy` and the `server.proxy.*` pages only.
 */
export function showProxyActions(uuid: string): boolean {
  const base = `/server/${uuid}/proxy`;
  return router.path === base || router.path.startsWith(`${base}/`);
}
