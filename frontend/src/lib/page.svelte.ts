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
}

class PageState {
  switchers = $state<Switcher[]>([]);
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
