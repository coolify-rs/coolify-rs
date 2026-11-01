// Port of upstream Concerns/RemembersCanvasView: remembers per user and page whether the list or
// the canvas was used last, so a list page opens the canvas again. A cookie holds the choice, as
// upstream; here the browser reads it, because the pages render in the browser.
import { router } from './router.svelte';
import { session } from './session.svelte';

const LIST_VIEWS = ['table', 'grid', 'list'];

function cookieName(page: string): string {
  return `coolify_${session.info?.user.id ?? 0}_${page}_view`;
}

export function rememberView(page: string, view: string): void {
  document.cookie = `${cookieName(page)}=${view}; path=/; max-age=${5 * 365 * 24 * 3600}; samesite=lax`;
}

/** `prefersCanvasView`: a `?view=` link picks the list again and is remembered. */
export function prefersCanvasView(page: string): boolean {
  const view = router.query('view');
  if (view && LIST_VIEWS.includes(view)) {
    rememberView(page, view);
    return false;
  }
  const name = cookieName(page);
  return document.cookie.split('; ').some((c) => c === `${name}=canvas`);
}

/** The canvas a list route opens when the user left it there: [remembered page, canvas URL]. */
function canvasOf(name: string, p: Record<string, string>): [string, string] | null {
  const e = encodeURIComponent;
  const env = p['env'];
  switch (name) {
    case 'dashboard':
      return ['dashboard', '/dashboard/canvas'];
    case 'servers':
      return ['servers', '/servers/canvas'];
    case 'server-section':
      return p.section === 'resources' && !p.sub ? ['server_resources', `/server/${e(p.uuid)}/canvas`] : null;
    case 'tags':
      return p.tagName ? null : ['tags', '/canvas/tags'];
    case 'projects':
      return ['projects', '/projects/canvas'];
    case 'project':
      return ['project', `/project/${e(p.uuid)}/canvas`];
    case 'environment':
      return ['environment', `/project/${e(p.uuid)}/environment/${e(env)}/canvas`];
    case 'shared-team':
      return ['team_variables', '/shared-variables/team/canvas'];
    case 'project-shared-variables':
      return ['project_variables', `/shared-variables/project/${e(p.uuid)}/canvas`];
    case 'environment-shared-variables':
      return ['environment_variables', `/shared-variables/environments/project/${e(p.uuid)}/environment/${e(env)}/canvas`];
    case 'shared-server':
      return ['server_variables', `/shared-variables/server/${e(p.uuid)}/canvas`];
    default:
      return null;
  }
}

/** The redirect of the list component's `mount`, or null. */
export function canvasRedirect(name: string, params: Record<string, string>): string | null {
  const target = canvasOf(name, params);
  return target && prefersCanvasView(target[0]) ? target[1] : null;
}
