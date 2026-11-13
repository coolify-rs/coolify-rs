// Port of upstream components/service/configuration-sidebar.blade.php: the
// service menu the main sidebar shows on service pages, grouped as upstream
// groups it.
import type { NavContext, NavContextItem } from './page.svelte';

/** URL sections of `/service/{uuid}/{section}`. */
export const SERVICE_SECTIONS = [
  'domains',
  'environment-variables',
  'storages',
  'storage-backups',
  'logs',
  'terminal',
  'files',
  'analytics',
  'vulnerabilities',
  'scheduled-tasks',
  'webhooks',
  'resource-operations',
  'maintenance',
  'tags',
  'danger',
];

export function serviceNavContext(
  base: string,
  active: string,
  opts: { isDeployable: boolean; canAccessTerminal: boolean; storageSections?: { id: string; label: string }[]; security?: string | null },
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
      // `'security' => SecurityStatus::stored($service)->status->value` (the security dot).
      item('Security', 'vulnerabilities', 'shield-alert', { security: opts.security ?? null }),
    ],
    [
      item('Scheduled Tasks', 'scheduled-tasks', 'calendar'),
      item('Webhooks', 'webhooks', 'webhook'),
      item('Backups', 'storage-backups', 'database'),
    ],
    [item('Resource Operations', 'resource-operations', 'server-update'), item('Maintenance Mode', 'maintenance', 'bandage'), item('Tags', 'tags', 'tags')],
    [item('Danger Zone', 'danger', 'danger')],
  ];
  return { label: 'Dashboard', backTitle: 'Back to the main menu', ariaLabel: 'Service settings', groups };
}
