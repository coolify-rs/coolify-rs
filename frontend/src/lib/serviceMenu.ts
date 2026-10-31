// Port of upstream components/service/configuration-sidebar.blade.php: the
// service menu the main sidebar shows on service pages, grouped as upstream
// groups it, and of the compose resource menu of livewire/project/service/index.blade.php.
import type { NavContext, NavContextItem } from './page.svelte';

/** URL sections of `/service/{uuid}/{section}`; anything else is a compose resource uuid. */
export const SERVICE_SECTIONS = [
  'domains',
  'environment-variables',
  'storages',
  'storage-backups',
  'import-backup',
  'logs',
  'terminal',
  'files',
  'analytics',
  'scheduled-tasks',
  'webhooks',
  'resource-operations',
  'tags',
  'danger',
];

export function serviceNavContext(
  base: string,
  active: string,
  opts: { isDeployable: boolean; canAccessTerminal: boolean; storageSections?: { id: string; label: string }[] },
): NavContext {
  const item = (label: string, section: string, icon: string, extra: Partial<NavContextItem> = {}): NavContextItem => ({
    label,
    icon,
    href: section === 'general' ? base : `${base}/${section}`,
    active: active === section,
    ...extra,
  });
  const groups: NavContextItem[][] = [
    [
      item('General', 'general', 'settings'),
      item('Domains', 'domains', 'globe'),
      item('Environment Variables', 'environment-variables', 'variables', { warningDot: !opts.isDeployable }),
      item('Persistent Storage', 'storages', 'storages', { children: opts.storageSections ?? [] }),
    ],
    [
      item('Runtime Logs', 'logs', 'unordered-list'),
      ...(opts.canAccessTerminal ? [item('Terminal', 'terminal', 'browser-terminal'), item('Files', 'files', 'folder', { beta: true })] : []),
      item('Analytics', 'analytics', 'analytics'),
    ],
    [
      item('Scheduled Tasks', 'scheduled-tasks', 'calendar'),
      item('Webhooks', 'webhooks', 'webhook'),
      item('Backups', 'storage-backups', 'database'),
      item('Import Backup', 'import-backup', 'upload'),
    ],
    [item('Resource Operations', 'resource-operations', 'server-update'), item('Tags', 'tags', 'tags')],
    [item('Danger Zone', 'danger', 'danger')],
  ];
  return { label: 'Dashboard', backTitle: 'Back to the main menu', ariaLabel: 'Service settings', groups };
}

/** `<x-main-nav-menu :depth="1" back-label="Service">` of the compose resource page. */
export function serviceResourceNavContext(href: string): NavContext {
  return {
    label: 'Service',
    backTitle: 'Back to the service menu',
    ariaLabel: 'Compose resource settings',
    groups: [[{ label: 'General', icon: 'settings', href, active: true }]],
  };
}
