// Shared loading for server pages: the server's facts (for the menu, the
// top bar and permissions) and the switcher list.
import { serversUi, type ServerFacts, type ServersPageData } from './api';
import { page } from './page.svelte';
import { session } from './session.svelte';
import { serverNavContext } from './serverMenu';

export async function loadServerContext(uuid: string, active: string, sub?: string): Promise<{ facts: ServerFacts; list: ServersPageData } | null> {
  const list = await serversUi.list();
  const facts = list.servers.find((s) => s.uuid === uuid);
  if (!facts) return null;
  page.navContext = serverNavContext(facts, active, session.isAdmin, sub);
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
