// Port of the `$navSections` array in upstream components/navbar.blade.php
// (labels, icons, URLs, active patterns and visibility rules unchanged).

export interface NavLeaf {
  label: string;
  icon?: string;
  /** Brand icons are masks of upstream public/svgs/<name>.svg. */
  brandIcon?: string;
  href: string;
  /** Path patterns in Laravel `request()->is()` syntax (`*` wildcard). */
  active: string[];
  visible?: (ctx: NavContext) => boolean;
  sectionStart?: boolean;
  /** Ported in coolify-rs (others show the "not available yet" page). */
  ported?: boolean;
}

export interface NavParent {
  label: string;
  icon: string;
  children: NavLeaf[];
  visible?: (ctx: NavContext) => boolean;
}

export type NavItem = NavLeaf | NavParent;

export interface NavContext {
  isAdmin: boolean;
  isOwner: boolean;
  isInstanceAdmin: boolean;
  /** `$teamWide`: members limited to some projects only see project pages. */
  teamWide?: boolean;
  isCloud?: boolean;
  /** coolify-rs plugins (ADR 0013): the Settings and Team entries, only when there is something to show. */
  pluginSettings?: boolean;
  pluginTeam?: boolean;
}

export const navSections: [string, NavItem[]][] = [
  [
    'Workspace',
    [
      { label: 'Dashboard', icon: 'dashboard', href: '/', active: ['', 'dashboard/*', 'canvas'], ported: true },
      { label: 'Projects', icon: 'projects', href: '/projects', active: ['projects', 'projects/*', 'project/*'], ported: true },
      {
        label: 'Monitoring',
        icon: 'graph',
        visible: (c) => c.teamWide !== false,
        children: [
          { label: 'Analytics', icon: 'analytics', href: '/analytics', active: ['analytics'] },
          { label: 'Metrics', icon: 'graph', href: '/metrics', active: ['metrics'] },
        ],
      },
      { label: 'Terminal', icon: 'browser-terminal', href: '/terminal', active: ['terminal*'], visible: (c) => c.teamWide !== false && c.isAdmin, ported: true },
    ],
  ],
  [
    'Infrastructure',
    [
      { label: 'Servers', icon: 'servers', href: '/servers', active: ['server/*', 'servers', 'servers/*'], visible: (c) => c.teamWide !== false, ported: true },
      {
        label: 'Integrations',
        icon: 'link',
        visible: (c) => c.teamWide !== false,
        children: [
          { label: 'Sources', icon: 'sources', href: '/sources', active: ['sources', 'source/*'] },
          { label: 'Destinations', icon: 'destinations', href: '/destinations', active: ['destinations', 'destination/*'] },
          { label: 'Registries', icon: 'layers', href: '/registries', active: ['registries'], visible: (c) => c.isAdmin },
          { label: 'S3 Storage', icon: 'storages', href: '/storages', active: ['storages', 'storages/*'], ported: true },
        ],
      },
      {
        label: 'Shared Variables',
        icon: 'variables',
        children: [
          { label: 'Overview', icon: 'dashboard', href: '/shared-variables', active: ['shared-variables'], visible: (c) => c.teamWide !== false, ported: true },
          { label: 'Team', icon: 'teams', href: '/shared-variables/team', active: ['shared-variables/team*'], visible: (c) => c.teamWide !== false, ported: true },
          { label: 'Projects', icon: 'projects', href: '/shared-variables/projects', active: ['shared-variables/projects', 'shared-variables/project/*'], ported: true },
          { label: 'Environments', icon: 'layers', href: '/shared-variables/environments', active: ['shared-variables/environments*'], ported: true },
          { label: 'Servers', icon: 'servers', href: '/shared-variables/servers', active: ['shared-variables/servers', 'shared-variables/server/*'], visible: (c) => c.teamWide !== false, ported: true },
        ],
      },
    ],
  ],
  [
    'Manage',
    [
      {
        label: 'Team',
        icon: 'teams',
        visible: (c) => c.teamWide !== false,
        children: [
          { label: 'General', icon: 'settings', href: '/team', active: ['team'], ported: true },
          { label: 'Members', icon: 'teams', href: '/team/members', active: ['team/members*'], ported: true },
          { label: 'Audit log', icon: 'time-back', href: '/team/audit-log', active: ['team/audit-log'], visible: (c) => c.isAdmin, ported: true },
          { label: 'Admin View', icon: 'admin', href: '/team/admin', active: ['team/admin'], visible: (c) => c.isInstanceAdmin, ported: true },
          { label: 'Plugins', icon: 'browser-code', href: '/team/plugins', active: ['team/plugins'], visible: (c) => !!c.pluginTeam, ported: true },
          { label: 'Danger Zone', icon: 'danger', href: '/team/danger', active: ['team/danger'], visible: (c) => c.isOwner, sectionStart: true, ported: true },
        ],
      },
      {
        label: 'Notifications',
        icon: 'notifications',
        visible: (c) => c.teamWide !== false,
        children: [
          { label: 'Email', icon: 'mail', href: '/notifications/email', active: ['notifications/email'], ported: true },
          { label: 'Discord', brandIcon: 'discord', href: '/notifications/discord', active: ['notifications/discord'], ported: true },
          { label: 'Telegram', brandIcon: 'telegram', href: '/notifications/telegram', active: ['notifications/telegram'], ported: true },
          { label: 'Slack', brandIcon: 'slack', href: '/notifications/slack', active: ['notifications/slack'], ported: true },
          { label: 'Pushover', brandIcon: 'pushover', href: '/notifications/pushover', active: ['notifications/pushover'], ported: true },
          { label: 'Webhook', icon: 'destinations', href: '/notifications/webhook', active: ['notifications/webhook'], ported: true },
        ],
      },
      {
        label: 'Keys & Tokens',
        icon: 'keys',
        children: [
          { label: 'Private Keys', icon: 'keys', href: '/security/private-key', active: ['security/private-key*'], visible: (c) => c.teamWide !== false, ported: true },
          { label: 'API Tokens', icon: 'code', href: '/security/api-tokens', active: ['security/api-tokens'], ported: true },
          { label: 'Cloud Tokens', icon: 'cloud', href: '/security/cloud-tokens', active: ['security/cloud-tokens*'], sectionStart: true, visible: (c) => c.teamWide !== false && c.isAdmin, ported: true },
          { label: 'Integration Tokens', icon: 'network', href: '/security/integration-tokens', active: ['security/integration-tokens'], visible: (c) => c.teamWide !== false && c.isAdmin, ported: true },
          { label: 'Cloud-Init Scripts', icon: 'file-content', href: '/security/cloud-init-scripts', active: ['security/cloud-init-scripts*'], sectionStart: true, visible: (c) => c.teamWide !== false && c.isAdmin, ported: true },
        ],
      },
      { label: 'Subscription', icon: 'subscription', href: '/subscription', active: ['subscription*'], visible: (c) => c.teamWide !== false && !!c.isCloud && c.isAdmin, ported: true },
      { label: 'Tags', icon: 'tags', href: '/tags', active: ['tags*', 'canvas/tags'], visible: (c) => c.teamWide !== false, ported: true },
      {
        label: 'Settings',
        icon: 'settings',
        visible: (c) => c.teamWide !== false && c.isInstanceAdmin,
        children: [
          { label: 'General', icon: 'settings', href: '/settings', active: ['settings'], ported: true },
          { label: 'Advanced', icon: 'grid', href: '/settings/advanced', active: ['settings/advanced'], ported: true },
          { label: 'Updates', icon: 'refresh3', href: '/settings/updates', active: ['settings/updates'], ported: true },
          { label: 'Authentication', icon: 'keys', href: '/settings/oauth', active: ['settings/oauth*'], sectionStart: true, ported: true },
          { label: 'Email', icon: 'mail', href: '/settings/email', active: ['settings/email'], ported: true },
          { label: 'Backup', icon: 'database', href: '/settings/backup', active: ['settings/backup'], ported: true },
          { label: 'Plugins', icon: 'browser-code', href: '/settings/plugins', active: ['settings/plugins'], visible: (c) => !!c.pluginSettings, ported: true },
        ],
      },
    ],
  ],
];

/** Laravel `request()->is($pattern)`: `*` matches anything (including `/`). */
export function pathIs(path: string, patterns: string[]): boolean {
  const p = path.replace(/^\/+|\/+$/g, '');
  return patterns.some((pat) => {
    const re = new RegExp('^' + pat.split('*').map((s) => s.replace(/[.+?^${}()|[\]\\]/g, '\\$&')).join('.*') + '$');
    return re.test(p);
  });
}

export function isParent(item: NavItem): item is NavParent {
  return 'children' in item;
}

/** Visible sections, children filtered, as upstream's `->filter()->map()` does. */
export function visibleSections(ctx: NavContext): [string, NavItem[]][] {
  return navSections.map(([label, items]) => [
    label,
    items
      .filter((i) => i.visible?.(ctx) ?? true)
      .map((i) => (isParent(i) ? { ...i, children: i.children.filter((c) => c.visible?.(ctx) ?? true) } : i))
      .filter((i) => !isParent(i) || i.children.length > 0),
  ]);
}

/** Every URL the nav links to that coolify-rs implements. */
export function isPortedPath(path: string): boolean {
  for (const [, items] of navSections) {
    for (const i of items) {
      const leaves = isParent(i) ? i.children : [i];
      for (const l of leaves) if (l.ported && pathIs(path, l.active)) return true;
    }
  }
  return false;
}
