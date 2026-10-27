// Minimal history-API router: patterns like `/projects/:uuid`.

export interface Match {
  name: string;
  params: Record<string, string>;
}

const routes: { name: string; pattern: string }[] = [
  { name: 'login', pattern: '/login' },
  { name: 'projects', pattern: '/projects' },
  { name: 'project', pattern: '/projects/:uuid' },
  { name: 'environment', pattern: '/projects/:uuid/environments/:env' },
  { name: 'keys', pattern: '/security/keys' },
  { name: 'team', pattern: '/team' },
  { name: 'home', pattern: '/' },
];

function match(path: string): Match {
  const parts = path.replace(/\/+$/, '').split('/').filter(Boolean);
  for (const r of routes) {
    const pat = r.pattern.split('/').filter(Boolean);
    if (pat.length !== parts.length) continue;
    const params: Record<string, string> = {};
    const ok = pat.every((seg, i) => {
      if (seg.startsWith(':')) {
        params[seg.slice(1)] = decodeURIComponent(parts[i]);
        return true;
      }
      return seg === parts[i];
    });
    if (ok) return { name: r.name, params };
  }
  return { name: 'not-found', params: {} };
}

class Router {
  path = $state(window.location.pathname);
  current = $derived(match(this.path));

  constructor() {
    window.addEventListener('popstate', () => (this.path = window.location.pathname));
  }

  go(to: string, replace = false): void {
    if (to === this.path) return;
    if (replace) history.replaceState(null, '', to);
    else history.pushState(null, '', to);
    this.path = to;
    window.scrollTo(0, 0);
  }
}

export const router = new Router();

/** Click handler for `<a href>` that keeps navigation client-side. */
export function link(node: HTMLAnchorElement) {
  const onClick = (e: MouseEvent) => {
    if (e.defaultPrevented || e.button !== 0 || e.metaKey || e.ctrlKey || e.shiftKey || e.altKey) return;
    const href = node.getAttribute('href');
    if (!href || !href.startsWith('/')) return;
    e.preventDefault();
    router.go(href);
  };
  node.addEventListener('click', onClick);
  return { destroy: () => node.removeEventListener('click', onClick) };
}
