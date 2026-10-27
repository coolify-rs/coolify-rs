// Port of upstream components/database/configuration-sidebar.blade.php: the
// database menu the main sidebar shows on database pages, grouped as upstream
// groups it. Also the teleport targets of the database heading (upstream
// `@teleport('#resource-action-hud-slot')` and friends in layouts/app.blade.php).
import type { NavContext, NavContextItem } from './page.svelte';

interface Entry extends NavContextItem {
  visible?: boolean;
}

/** URL sections that render inside the configuration page (`project.database.*`). */
export const CONFIGURATION_SECTIONS = [
  'general',
  'environment-variables',
  'servers',
  'import-backup',
  'persistent-storage',
  'healthcheck',
  'webhooks',
  'resource-limits',
  'resource-operations',
  'metrics',
  'tags',
  'danger',
];

export function databaseNavContext(
  base: string,
  active: string,
  opts: { canUpdate: boolean; canAccessTerminal: boolean; backups: boolean; initScripts: boolean },
): NavContext {
  const item = (label: string, section: string, icon: string, extra: Partial<Entry> = {}): Entry => ({
    label,
    icon,
    href: section === 'general' ? base : `${base}/${section}`,
    active: active === section,
    ...extra,
  });
  const items: Record<string, Entry> = {
    // `$pageSections` of components/database/configuration-sidebar.blade.php (init scripts: PostgreSQL only).
    'General': item('General', 'general', 'settings', {
      children: [
        { id: 'database-details-section', label: 'Database details' },
        { id: 'access-section', label: 'Access' },
        { id: 'container-section', label: 'Container' },
        ...(opts.initScripts ? [{ id: 'initialization-scripts-section', label: 'Initialization scripts' }] : []),
      ],
    }),
    'Environment Variables': item('Environment Variables', 'environment-variables', 'variables'),
    'Persistent Storage': item('Persistent Storage', 'persistent-storage', 'storages'),
    'Backups': item('Backups', 'backups', 'database', { visible: opts.backups }),
    'Import Backup': item('Import Backup', 'import-backup', 'upload', { visible: opts.canUpdate }),
    'Servers': item('Servers', 'servers', 'servers'),
    'Runtime Logs': item('Runtime Logs', 'logs', 'unordered-list'),
    'Terminal': item('Terminal', 'terminal', 'browser-terminal', { visible: opts.canAccessTerminal }),
    'Files': item('Files', 'files', 'folder', { visible: opts.canAccessTerminal, beta: true }),
    'Webhooks': item('Webhooks', 'webhooks', 'webhook'),
    'Healthcheck': item('Healthcheck', 'healthcheck', 'feedback'),
    'Resource Limits': item('Resource Limits', 'resource-limits', 'cpu'),
    'Resource Operations': item('Resource Operations', 'resource-operations', 'server-update'),
    'Metrics': item('Metrics', 'metrics', 'graph'),
    'Tags': item('Tags', 'tags', 'tags'),
    'Danger Zone': item('Danger Zone', 'danger', 'danger'),
  };
  const menuGroups = [
    ['General', 'Environment Variables', 'Persistent Storage', 'Healthcheck'],
    ['Runtime Logs', 'Terminal', 'Files', 'Metrics'],
    ['Servers'],
    ['Webhooks', 'Backups', 'Import Backup'],
    ['Resource Operations', 'Resource Limits', 'Tags'],
    ['Danger Zone'],
  ];
  const groups = menuGroups
    .map((labels) =>
      labels
        .map((l) => items[l])
        .filter((e) => e.visible !== false)
        .map(({ visible: _visible, ...rest }) => rest),
    )
    .filter((g) => g.length > 0);
  return { label: 'Dashboard', backTitle: 'Back to the main menu', ariaLabel: 'Database settings', groups };
}

/** Menu key for a URL section (`/database/{uuid}/{section}`); backups pages share one entry. */
export function databaseSection(section: string | undefined): string {
  if (!section || section === 'general') return 'general';
  if (section.startsWith('backups')) return 'backups';
  return section;
}

/** Moves the element into a top bar slot, like Livewire's `@teleport('#id')`. Without the slot the element stays in place. */
export function teleport(node: HTMLElement, id: string) {
  const target = document.getElementById(id);
  if (target) target.appendChild(node);
  return {
    destroy() {
      node.remove();
    },
  };
}
