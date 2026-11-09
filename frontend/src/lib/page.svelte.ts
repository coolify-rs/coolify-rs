// Breadcrumb state for the top bar (port of components/top-breadcrumb.blade.php).

export interface SwitcherItem {
  label: string;
  href: string;
  active: boolean;
  /** Lets a rename on the page update the label (`nameOf`). */
  uuid?: string;
}

/** A resource switcher in the breadcrumb ("Switch project", "Switch environment"). */
export interface Switcher {
  /** Listbox heading, e.g. "Projects". */
  heading: string;
  /** Button title, e.g. "Switch project". */
  title: string;
  label: string;
  uuid?: string;
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
  /** `project.application.server-status-badge`: the title of the warning icon. */
  errorBadge?: string;
  /** Sections of the active page the entry lists below itself (`.nav-children`), by element id. */
  children?: { id: string; label: string }[];
  /** Sub pages the active entry lists below itself (the `children` of components/server/sidebar.blade.php). */
  links?: { label: string; icon: string; href: string; active?: boolean; beta?: boolean }[];
  warning?: boolean;
  /** The red dot of a menu entry (`hasWarning`). */
  warningDot?: boolean;
  /** Wrapped in its own `<div>` with its sections (components/application/configuration-sidebar.blade.php), so no grid gap above them. */
  grouped?: boolean;
}

/** A resource menu the main sidebar shows instead of the main menu
 *  (upstream `registerMainNavContext` + `#main-nav-context-{depth}`). */
export interface NavContext {
  label: string;
  backTitle: string;
  ariaLabel: string;
  groups: NavContextItem[][];
  /** Labels the main menu row that opens this menu again (`registerMainNavContext` `$name`). */
  name?: string;
  /** The menu one Back away (`x-main-nav-menu :depth`, `navOnly`). */
  parent?: NavContext;
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
  uuid?: string;
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
  /** Names saved on the current page, by uuid (the breadcrumb's `renamed`). */
  renamed = $state<Record<string, string>>({});

  /** The breadcrumb's `nameOf(uuid, name)`: the saved name of a renamed resource. */
  nameOf(uuid: string | undefined | null, name: string): string {
    return (uuid && this.renamed[uuid]) ?? name;
  }

  /** The `name-changed` browser event a component dispatches after it saved a new name. */
  nameChanged(uuid: string, name: string): void {
    this.renamed[uuid] = name;
  }
}

export const page = new PageState();

/** Upstream `$dashboardContext`: the section name shown after the team. */
export function dashboardContext(path: string): string | null {
  const p = path.replace(/\/+$/, '') || '/';
  const rules: [RegExp, string][] = [
    [/^\/$/, 'Dashboard'],
    [/^\/projects(\/canvas)?$/, 'Projects'],
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
    [/^\/canvas\/tags$/, 'Tags'],
    [/^\/settings/, 'Settings'],
    [/^\/profile/, 'Profile'],
    [/^\/admin/, 'Admin'],
  ];
  return rules.find(([re]) => re.test(p))?.[1] ?? null;
}

/** The resource menu to keep while the next page loads. Upstream renders each page with its menu, so moving
 *  between entries of one resource menu never shows the main menu in between. Here a page sets its menu only
 *  after its data arrives, so the menu of the page being left stays when `path` is one of its entries (or of
 *  a menu one Back away), with that entry marked active. Anywhere else the main menu shows at once. */
export function carryNavContext(ctx: NavContext | null, path: string): NavContext | null {
  for (let menu = ctx; menu; menu = menu.parent ?? null) {
    const entries = menu.groups.flat();
    const isEntry = entries.some((i) => !i.disabled && (i.href === path || i.links?.some((l) => l.href === path)));
    if (!isEntry) continue;
    return {
      ...menu,
      groups: menu.groups.map((group) =>
        group.map((item) => {
          const links = item.links?.map((l) => ({ ...l, active: l.href === path }));
          // The sections of the page being left belong to that page.
          return { ...item, links, children: undefined, active: item.href === path || !!links?.some((l) => l.active) };
        }),
      ),
    };
  }
  return null;
}
