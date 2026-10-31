// Port of upstream components/server/sidebar.blade.php: the server menu the
// main sidebar shows on server pages, grouped as upstream groups it.
import type { NavContext, NavContextItem } from './page.svelte';
import type { ServerFacts } from './api';

interface Entry extends NavContextItem {
  group: string;
  visible?: boolean;
}

export function serverNavContext(s: ServerFacts, active: string, isAdmin: boolean): NavContext {
  const base = `/server/${s.uuid}`;
  const off = !s.functional;
  const canHost = !s.is_build;
  const entries: Entry[] = [
    { label: 'General', icon: 'settings', href: base, active: active === 'general', group: 'Settings' },
    { label: 'Advanced', icon: 'grid', href: `${base}/advanced`, active: active === 'advanced', group: 'Settings', disabled: off },
    { label: 'Private Key', icon: 'keys', href: `${base}/private-key`, active: active === 'private-key', group: 'Settings' },
    { label: 'Cloud Token', icon: 'subscription', href: `${base}/cloud-provider-token`, active: active === 'cloud-provider-token', group: 'Settings', visible: s.cloud },
    { label: 'CA Certificate', icon: 'file', href: `${base}/ca-certificate`, active: active === 'ca-certificate', group: 'Settings', disabled: off },
    { label: 'Cloudflare Tunnel', icon: 'cloudflare', href: `${base}/cloudflare-tunnel`, active: active === 'cloudflare-tunnel', group: 'Networking', visible: !s.is_localhost },
    { label: 'Proxy', icon: 'network', href: `${base}/proxy`, active: active === 'proxy', group: 'Platform', visible: !s.is_swarm_worker && canHost, disabled: off },
    { label: 'Sentinel', icon: 'shield-star', href: `${base}/sentinel`, active: active === 'sentinel', group: 'Platform', visible: !s.is_swarm && canHost && isAdmin, disabled: off },
    { label: 'Resources', icon: 'projects', href: `${base}/resources`, active: active === 'resources', group: 'Platform', disabled: off },
    { label: 'Terminal', icon: 'browser-terminal', href: `${base}/terminal`, active: active === 'terminal', group: 'Operations', visible: isAdmin, disabled: off },
    { label: 'Destinations', icon: 'destinations', href: `${base}/destinations`, active: active === 'destinations', group: 'Networking', disabled: off },
    { label: 'Images', icon: 'layers', href: `${base}/images`, active: active === 'images', group: 'Operations', disabled: off },
    { label: 'Docker Cleanup', icon: 'broom', href: `${base}/docker-cleanup`, active: active === 'docker-cleanup', group: 'Operations', disabled: off },
    { label: 'GitHub Runners', icon: 'play-circle', href: `${base}/github-runners`, active: active === 'github-runners', group: 'Operations', visible: !s.is_localhost, beta: true, disabled: off },
    { label: 'Registries', icon: 'layers', href: `${base}/registries`, active: active === 'registries', group: 'Operations', visible: isAdmin, disabled: off },
    { label: 'Log Drains', icon: 'notifications', href: `${base}/log-drains`, active: active === 'log-drains', group: 'Operations', disabled: off },
    { label: 'Metrics', icon: 'graph', href: `${base}/metrics`, active: active === 'metrics', group: 'Operations', disabled: off },
    { label: 'Analytics', icon: 'analytics', href: `${base}/analytics`, active: active === 'analytics', group: 'Operations', visible: !s.is_swarm && !s.is_build, disabled: off },
    { label: 'Security', icon: 'shield-alert', href: `${base}/security/patches`, active: active === 'security', group: 'Security', visible: isAdmin, disabled: off },
    { label: 'Transfer', icon: 'arrow-right', href: `${base}/transfer`, active: active === 'transfer', group: 'Operations', visible: !s.is_localhost && isAdmin, beta: true, disabled: off },
    { label: 'Danger Zone', icon: 'danger', href: `${base}/danger`, active: active === 'danger', group: 'Danger Zone', visible: !s.is_coolify_host },
  ];
  // Collection::groupBy keeps groups in first-seen order.
  const groups = new Map<string, NavContextItem[]>();
  for (const { group, visible, ...item } of entries) {
    if (visible === false) continue;
    if (!groups.has(group)) groups.set(group, []);
    groups.get(group)?.push(item);
  }
  return { label: 'Servers', backTitle: 'Back to the main menu', ariaLabel: 'Server configuration sections', groups: [...groups.values()] };
}

/** Menu key for a server URL section (`/server/{uuid}/{section}`). */
export function serverSection(section: string | undefined): string {
  const map: Record<string, string> = { 'security': 'security', 'proxy': 'proxy', 'sentinel': 'sentinel' };
  return section ? (map[section] ?? section) : 'general';
}
