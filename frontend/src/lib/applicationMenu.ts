// Port of upstream components/application/configuration-sidebar.blade.php for
// an application on a standalone Docker server: the grouped menu the main
// sidebar shows on application pages, and the in-page anchors of the active page.
import type { NavContext, NavContextItem } from './page.svelte';

interface Entry extends NavContextItem {
  visible?: boolean;
}

/** The URL section of an application page (`/application/{uuid}/{section}`); deployments share one entry. */
/** The sections `routes/web.php` has under `/application/{uuid}`; any other path is `Route::any('/{any}')`. */
export const APPLICATION_SECTIONS = [
  'domains',
  'swarm',
  'advanced',
  'environment-variables',
  'persistent-storage',
  'backups',
  'source',
  'servers',
  'scheduled-tasks',
  'webhooks',
  'preview-deployments',
  'healthcheck',
  'rollback',
  'resource-limits',
  'resource-operations',
  'metrics',
  'analytics',
  'tags',
  'danger',
  'deployment',
  'logs',
  'terminal',
  'files',
  // coolify-rs plugins (ADR 0013); only shown, and only kept, when the team has an application panel.
  'plugins',
];

export function applicationSection(section: string | undefined): string {
  if (!section || section === 'general') return 'general';
  if (section.startsWith('deployment')) return 'deployment';
  if (section.startsWith('backups')) return 'backups';
  if (section.startsWith('scheduled-tasks') || section.startsWith('tasks')) return 'scheduled-tasks';
  return section;
}

/** Port of upstream components/backup-sidebar.blade.php for context `application`. */
export function applicationBackupNavContext(base: string, backup: string, section: string, parent: NavContext): NavContext {
  const url = `${base}/backups/${backup}`;
  return {
    label: 'Application',
    backTitle: 'Back to the application menu',
    ariaLabel: 'Backup settings',
    name: 'Backup',
    parent,
    groups: [
      [
        { label: 'General', icon: 'settings', href: url, active: section === 'general' },
        { label: 'Executions', icon: 'browser-terminal', href: `${url}/executions`, active: section === 'executions' },
      ],
      [{ label: 'Danger Zone', icon: 'danger', href: `${url}/danger`, active: section === 'danger' }],
    ],
  };
}

export function applicationNavContext(
  base: string,
  active: string,
  opts: { canAccessTerminal: boolean; gitBased: boolean; dockerImage: boolean; compose: boolean; swarm?: boolean; serverBadge?: string; plugins?: boolean },
): NavContext {
  const item = (label: string, section: string, icon: string, extra: Partial<Entry> = {}): Entry => ({
    label,
    icon,
    href: section === 'general' ? base : `${base}/${section}`,
    active: active === section,
    ...extra,
  });
  const items: Record<string, Entry> = {
    'General': item('General', 'general', 'settings', {
      children: [
        { id: 'application-details-section', label: 'Application details' },
        { id: 'access-section', label: 'Access' },
        { id: 'build-pipeline-section', label: 'Build & deploy' },
        ...(opts.compose ? [] : [{ id: 'container-section', label: 'Container' }]),
      ],
    }),
    'Domains': item('Domains', 'domains', 'globe'),
    'Environment Variables': item('Environment Variables', 'environment-variables', 'variables'),
    'Persistent Storage': item('Persistent Storage', 'persistent-storage', 'storages'),
    'Advanced': item('Advanced', 'advanced', 'grid', {
      children: [
        { id: 'advanced-build-section', label: 'Build & deploy' },
        { id: 'advanced-container-section', label: 'Container' },
        { id: 'advanced-proxy-section', label: 'Proxy' },
      ],
    }),
    'Swarm': item('Swarm', 'swarm', 'destinations', { visible: !!opts.swarm }),
    'Healthcheck': item('Healthcheck', 'healthcheck', 'feedback', {
      visible: !opts.compose,
      children: [
        { id: 'healthcheck-configuration-section', label: 'Configuration' },
        { id: 'healthcheck-timing-section', label: 'Timing and retries' },
      ],
    }),
    'Runtime Logs': item('Runtime Logs', 'logs', 'unordered-list'),
    'Deployment Logs': item('Deployment Logs', 'deployment', 'time-back'),
    'Terminal': item('Terminal', 'terminal', 'browser-terminal', { visible: opts.canAccessTerminal && !opts.swarm }),
    'Files': item('Files', 'files', 'folder', { visible: opts.canAccessTerminal && !opts.swarm, beta: true }),
    'Metrics': item('Metrics', 'metrics', 'graph'),
    'Analytics': item('Analytics', 'analytics', 'analytics'),
    'Git Source': item('Git Source', 'source', 'sources', { visible: opts.gitBased }),
    'Servers': item('Servers', 'servers', 'servers', { errorBadge: opts.serverBadge }),
    'Preview Deployments': item('Preview Deployments', 'preview-deployments', 'eye', {
      visible: opts.gitBased || opts.dockerImage,
      children: [
        { id: 'preview-template-section', label: 'URL template' },
        ...(opts.dockerImage ? [{ id: 'manual-preview-section', label: 'Manual preview' }] : []),
        { id: 'preview-deployments-section', label: 'Deployments' },
      ],
    }),
    'Scheduled Tasks': item('Scheduled Tasks', 'scheduled-tasks', 'calendar'),
    'Webhooks': item('Webhooks', 'webhooks', 'webhook', {
      children: [
        { id: 'deploy-webhook-section', label: 'Deploy webhook' },
        { id: 'manual-git-webhooks-section', label: 'Manual Git webhooks' },
      ],
    }),
    'Backups': item('Backups', 'backups', 'database'),
    'Resource Operations': item('Resource Operations', 'resource-operations', 'server-update', {
      children: [
        { id: 'clone-resource-section', label: 'Clone resource' },
        { id: 'move-resource-section', label: 'Move resource' },
      ],
    }),
    'Resource Limits': item('Resource Limits', 'resource-limits', 'cpu'),
    'Rollback': item('Rollback', 'rollback', 'time-back'),
    'Tags': item('Tags', 'tags', 'tags'),
    'Plugins': item('Plugins', 'plugins', 'browser-code', { visible: !!opts.plugins }),
    'Danger Zone': item('Danger Zone', 'danger', 'danger'),
  };
  const menuGroups = [
    ['General', 'Domains', 'Environment Variables', 'Persistent Storage', 'Advanced', 'Swarm', 'Healthcheck'],
    ['Runtime Logs', 'Deployment Logs', 'Terminal', 'Files', 'Metrics', 'Analytics'],
    ['Git Source', 'Servers', 'Preview Deployments'],
    ['Scheduled Tasks', 'Webhooks', 'Backups'],
    ['Resource Operations', 'Resource Limits', 'Rollback', 'Tags'],
    ['Plugins'],
    ['Danger Zone'],
  ];
  const groups = menuGroups
    .map((labels) =>
      labels
        .map((l) => items[l])
        .filter((e) => e.visible !== false)
        .map(({ visible: _visible, ...rest }) => ({ ...rest, grouped: true, children: rest.active ? rest.children : undefined })),
    )
    .filter((g) => g.length > 0);
  return { label: 'Dashboard', backTitle: 'Back to the main menu', ariaLabel: 'Application settings', groups };
}
