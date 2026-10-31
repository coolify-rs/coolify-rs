// Breadcrumb state for the top bar (port of components/top-breadcrumb.blade.php).

export interface SwitcherItem {
  label: string;
  href: string;
  active: boolean;
}

/** A resource switcher in the breadcrumb ("Switch project", "Switch environment"). */
export interface Switcher {
  /** Listbox heading, e.g. "Projects". */
  heading: string;
  /** Button title, e.g. "Switch project". */
  title: string;
  label: string;
  items: SwitcherItem[];
  /** The status chip after the button (`x-slot:meta`). */
  meta?: { label: string; dot: string };
}

/** An entry of a resource menu (e.g. upstream components/server/sidebar.blade.php). */
export interface NavContextItem {
  label: string;
  icon: string;
  href: string;
  active?: boolean;
  /** Shown greyed out, not a link ("Validate the server connection first."). */
  disabled?: boolean;
  beta?: boolean;
  /** Sections of the active page the entry lists below itself (`.nav-children`), by element id. */
  children?: { id: string; label: string }[];
  /** Sub pages the active entry lists below itself (the `children` of components/server/sidebar.blade.php). */
  links?: { label: string; icon: string; href: string; active?: boolean; beta?: boolean }[];
  warning?: boolean;
}

/** A resource menu the main sidebar shows instead of the main menu
 *  (upstream `registerMainNavContext` + `#main-nav-context-{depth}`). */
export interface NavContext {
  label: string;
  backTitle: string;
  ariaLabel: string;
  groups: NavContextItem[][];
}

/** Server context in the top bar (switcher and status), see ServerTopbar. */
export interface ServerTopbar {
  uuid: string;
  name: string;
  functional: boolean;
  proxySet: boolean;
  proxyStatus: string | null;
  proxyPending: boolean;
  traefikOutdated: boolean;
  sentinelStatus: 'waiting' | 'in_sync' | 'out_of_sync';
  sentinelEnabled: boolean;
  isCoolifyHost: boolean;
  servers: { uuid: string; name: string; functional: boolean }[];
}

/** `<x-breadcrumb-switcher>` after the section name (a tag, an S3 storage). */
export interface ResourceSwitcher {
  title: string;
  label: string;
  items: SwitcherItem[];
  /** The storage's status pill. */
  usable?: boolean;
}

class PageState {
  switchers = $state<Switcher[]>([]);
  resourceSwitcher = $state<ResourceSwitcher | null>(null);
  navContext = $state<NavContext | null>(null);
  server = $state<ServerTopbar | null>(null);
  /** The source switcher's status badge (`$sourceConnected`); `null` outside source pages. */
  sourceConnected = $state<boolean | null>(null);
  /** Sub-items of the current page under its entry in the main sidebar (`#main-nav-active-children`). */
  navChildren = $state<import('svelte').Snippet | null>(null);
}

export const page = new PageState();

/** Upstream `$dashboardContext`: the section name shown after the team. */
export function dashboardContext(path: string): string | null {
  const p = path.replace(/\/+$/, '') || '/';
  const rules: [RegExp, string][] = [
    [/^\/$/, 'Dashboard'],
    [/^\/projects$/, 'Projects'],
    [/^\/terminal/, 'Terminal'],
    [/^\/servers?(\/|$)/, 'Servers'],
    [/^\/sources?(\/|$)/, 'Sources'],
    [/^\/destinations?(\/|$)/, 'Destinations'],
    [/^\/registries/, 'Registries'],
    [/^\/storages?(\/|$)/, 'S3 Storage'],
    [/^\/shared-variables/, 'Shared Variables'],
    [/^\/team/, 'Team'],
    [/^\/notifications/, 'Notifications'],
    [/^\/security/, 'Keys & Tokens'],
    [/^\/tags/, 'Tags'],
    [/^\/settings/, 'Settings'],
    [/^\/profile/, 'Profile'],
    [/^\/admin/, 'Admin'],
  ];
  return rules.find(([re]) => re.test(p))?.[1] ?? null;
}
