// Layout preferences, stored under the same localStorage keys as upstream
// (layouts/app.blade.php and layouts/base.blade.php) so they carry over.

function read(key: string): string | null {
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
}

function write(key: string, value: string): void {
  try {
    localStorage.setItem(key, value);
  } catch {
    // Storage can be unavailable (private mode); the setting still applies.
  }
}

export type Theme = 'dark' | 'light' | 'system';

/** Port of upstream `window.applyStoredTheme` for the built-in themes. */
export function applyStoredTheme(): void {
  const root = document.documentElement;
  const stored = read('theme');
  const theme: Theme = stored === 'light' || stored === 'system' ? stored : 'dark';
  root.dataset.navStyle = read('navStyle') === 'floating' ? 'floating' : 'default';
  root.dataset.navItems = read('navItems') === 'flat' ? 'flat' : '3d';
  const isDark = theme === 'dark' || (theme === 'system' && matchMedia('(prefers-color-scheme: dark)').matches);
  root.classList.toggle('dark', isDark);
  root.dataset.theme = isDark ? 'dark' : 'light';
  document.getElementById('theme-color-meta')?.setAttribute('content', isDark ? '#101010' : '#ffffff');
}

class LayoutState {
  /** Desktop sidebar collapsed to icons. New users start expanded. */
  collapsed = $state(read('sidebarCollapsed') === 'true');
  /** 'centered' caps the page at 1400px; 'full' fills the window. */
  pageWidth = $state(read('pageWidth') || 'centered');
  /** Mobile slide-over sidebar. */
  mobileOpen = $state(false);
  theme = $state<Theme>((read('theme') as Theme) || 'dark');
  navStyle = $state(read('navStyle') === 'floating' ? 'floating' : 'default');
  navItems = $state(read('navItems') === 'flat' ? 'flat' : '3d');

  toggleSidebar(): void {
    this.collapsed = !this.collapsed;
    write('sidebarCollapsed', String(this.collapsed));
  }

  setPageWidth(width: string): void {
    this.pageWidth = width;
    write('pageWidth', width);
  }

  setNavStyle(style: string): void {
    this.navStyle = style;
    write('navStyle', style);
    applyStoredTheme();
  }

  setNavItems(items: string): void {
    this.navItems = items;
    write('navItems', items);
    applyStoredTheme();
  }

  setTheme(theme: Theme): void {
    this.theme = theme;
    write('theme', theme);
    applyStoredTheme();
  }
}

export const layout = new LayoutState();
