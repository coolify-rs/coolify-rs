// Minimal history-API router using upstream Coolify's URLs, so bookmarks and
// links work the same against either server.

export interface Match {
  name: string;
  params: Record<string, string>;
}

const routes: { name: string; pattern: string }[] = [
  { name: 'login', pattern: '/login' },
  { name: 'register', pattern: '/register' },
  { name: 'forgot-password', pattern: '/forgot-password' },
  { name: 'reset-password', pattern: '/reset-password/:token' },
  { name: 'two-factor-challenge', pattern: '/two-factor-challenge' },
  { name: 'force-password-reset', pattern: '/force-password-reset' },
  { name: 'two-factor-required', pattern: '/two-factor-required' },
  { name: 'verify', pattern: '/verify' },
  { name: 'email-verify', pattern: '/email/verify/:id/:hash' },
  { name: 'oauth-denied', pattern: '/auth/:provider/redirect' },
  // Answered by the server (`OauthController::callback`); this only keeps the route on the list.
  { name: 'oauth-denied', pattern: '/auth/:provider/callback' },
  { name: 'onboarding', pattern: '/onboarding' },
  { name: 'settings-general', pattern: '/settings' },
  { name: 'settings-advanced', pattern: '/settings/advanced' },
  { name: 'settings-updates', pattern: '/settings/updates' },
  { name: 'settings-email', pattern: '/settings/email' },
  { name: 'settings-backup', pattern: '/settings/backup' },
  { name: 'settings-oauth', pattern: '/settings/oauth' },
  { name: 'settings-oauth', pattern: '/settings/oauth/:provider' },
  { name: 'dashboard', pattern: '/' },
  { name: 'analytics', pattern: '/analytics' },
  { name: 'projects', pattern: '/projects' },
  { name: 'project', pattern: '/project/:uuid' },
  { name: 'project-edit', pattern: '/project/:uuid/edit' },
  { name: 'environment', pattern: '/project/:uuid/environment/:env' },
  { name: 'environment-edit', pattern: '/project/:uuid/environment/:env/edit' },
  { name: 'resource-new', pattern: '/project/:uuid/environment/:env/new' },
  { name: 'application', pattern: '/project/:uuid/environment/:env/application/:app' },
  { name: 'application-deployment', pattern: '/project/:uuid/environment/:env/application/:app/deployment/:dep' },
  { name: 'application-section', pattern: '/project/:uuid/environment/:env/application/:app/:section' },
  { name: 'database', pattern: '/project/:uuid/environment/:env/database/:db' },
  { name: 'database-backup', pattern: '/project/:uuid/environment/:env/database/:db/backups/:backup' },
  { name: 'database-backup', pattern: '/project/:uuid/environment/:env/database/:db/backups/:backup/:backupSection' },
  { name: 'database-section', pattern: '/project/:uuid/environment/:env/database/:db/:section' },
  { name: 'service', pattern: '/project/:uuid/environment/:env/service/:service' },
  { name: 'service-section', pattern: '/project/:uuid/environment/:env/service/:service/:section' },
  { name: 'keys', pattern: '/security/private-key' },
  { name: 'key', pattern: '/security/private-key/:uuid' },
  { name: 'team', pattern: '/team' },
  { name: 'members', pattern: '/team/members' },
  { name: 'team-danger', pattern: '/team/danger' },
  { name: 'team-audit', pattern: '/team/audit-log' },
  { name: 'team-admin', pattern: '/team/admin' },
  { name: 'select-team', pattern: '/select-team' },
  { name: 'invitation', pattern: '/invitations/:uuid' },
  { name: 'auth-link', pattern: '/auth/link' },
  { name: 'confirm-password', pattern: '/user/confirm-password' },
  { name: 'profile', pattern: '/profile' },
  { name: 'profile-passkey-confirm', pattern: '/profile/passkeys/confirm' },
  { name: 'appearance', pattern: '/profile/appearance' },
  { name: 'registries', pattern: '/registries' },
  { name: 'destinations', pattern: '/destinations' },
  { name: 'destination', pattern: '/destination/:uuid' },
  { name: 'destination', pattern: '/destination/:uuid/:section' },
  { name: 'sources', pattern: '/sources' },
  { name: 'source-github', pattern: '/source/github/:uuid' },
  { name: 'source-github', pattern: '/source/github/:uuid/:tab' },
  { name: 'source-gitlab', pattern: '/source/gitlab/:uuid' },
  { name: 'source-gitea', pattern: '/source/gitea/:uuid' },
  { name: 'notification-email', pattern: '/notifications/email' },
  { name: 'notification-channel', pattern: '/notifications/:channel' },
  { name: 'terminal', pattern: '/terminal' },
  { name: 'tags', pattern: '/tags' },
  { name: 'tags', pattern: '/tags/:tagName' },
  { name: 'shared-overview', pattern: '/shared-variables' },
  { name: 'shared-team', pattern: '/shared-variables/team' },
  { name: 'shared-projects', pattern: '/shared-variables/projects' },
  { name: 'project-shared-variables', pattern: '/shared-variables/project/:uuid' },
  { name: 'shared-environments', pattern: '/shared-variables/environments' },
  { name: 'environment-shared-variables', pattern: '/shared-variables/environments/project/:uuid/environment/:env' },
  { name: 'shared-servers', pattern: '/shared-variables/servers' },
  { name: 'shared-server', pattern: '/shared-variables/server/:uuid' },
  { name: 'storages', pattern: '/storages' },
  { name: 'storage', pattern: '/storages/:uuid' },
  { name: 'storage', pattern: '/storages/:uuid/:section' },
  { name: 'api-tokens', pattern: '/security/api-tokens' },
  { name: 'cloud-tokens', pattern: '/security/cloud-tokens' },
  { name: 'cloud-token', pattern: '/security/cloud-tokens/:uuid' },
  { name: 'cloud-init-scripts', pattern: '/security/cloud-init-scripts' },
  { name: 'cloud-init-script', pattern: '/security/cloud-init-scripts/:uuid' },
  { name: 'integration-tokens', pattern: '/security/integration-tokens' },
  { name: 'servers', pattern: '/servers' },
  { name: 'server-new', pattern: '/servers/new' },
  { name: 'server-new-type', pattern: '/servers/new/:type' },
  { name: 'server-new-type', pattern: '/servers/new/:type/:token' },
  { name: 'server', pattern: '/server/:uuid' },
  { name: 'server-section', pattern: '/server/:uuid/:section' },
  { name: 'server-section', pattern: '/server/:uuid/:section/:sub' },
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
  return { name: 'unported', params: {} };
}

class Router {
  path = $state(window.location.pathname);
  /** The query string (`?type=docker-image&…`), or ''. */
  search = $state(window.location.search);
  current = $derived(match(this.path));

  constructor() {
    window.addEventListener('popstate', () => {
      this.path = window.location.pathname;
      this.search = window.location.search;
    });
  }

  go(to: string, replace = false): void {
    const q = to.indexOf('?');
    const path = q === -1 ? to : to.slice(0, q);
    const search = q === -1 ? '' : to.slice(q);
    if (path === this.path && search === this.search) return;
    if (replace) history.replaceState(null, '', to);
    else history.pushState(null, '', to);
    this.path = path;
    this.search = search;
    window.scrollTo(0, 0);
  }

  query(key: string): string | null {
    return new URLSearchParams(this.search).get(key);
  }
}

export const router = new Router();

/** Click handler for `<a href>` that keeps navigation client-side. */
export function link(node: HTMLAnchorElement) {
  const onClick = (e: MouseEvent) => {
    if (e.defaultPrevented || e.button !== 0 || e.metaKey || e.ctrlKey || e.shiftKey || e.altKey) return;
    const href = node.getAttribute('href');
    if (!href || !href.startsWith('/') || node.target) return;
    e.preventDefault();
    router.go(href);
  };
  node.addEventListener('click', onClick);
  return { destroy: () => node.removeEventListener('click', onClick) };
}
