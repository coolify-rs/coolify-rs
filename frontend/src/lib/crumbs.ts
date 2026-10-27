import { page } from './page.svelte';
import { strnatcmp } from './natsort';
import type { ProjectPageData } from './api';

/** Upstream `Environment::compareNames`: production, staging, development, then natural order. */
export function compareEnvironmentNames(a: string, b: string): number {
  const order = ['production', 'staging', 'development'];
  const rank = (n: string) => {
    const i = order.indexOf(n.toLowerCase());
    return i === -1 ? order.length : i;
  };
  return rank(a) - rank(b) || strnatcmp(a.toLowerCase(), b.toLowerCase());
}

/** Project (and environment) switchers in the top bar breadcrumb. */
export function setProjectCrumbs(data: ProjectPageData, envUuid?: string): void {
  const switchers = [
    {
      heading: 'Projects',
      title: 'Switch project',
      label: data.project.name,
      items: data.projects.map((p) => ({ label: p.name, href: `/project/${p.uuid}`, active: p.uuid === data.project.uuid })),
    },
  ];
  const env = envUuid ? data.environments.find((e) => e.uuid === envUuid) : undefined;
  if (env) {
    switchers.push({
      heading: 'Environments',
      title: 'Switch environment',
      label: env.name,
      items: data.environments.map((e) => ({
        label: e.name,
        href: `/project/${data.project.uuid}/environment/${e.uuid}`,
        active: e.uuid === env.uuid,
      })),
    });
  }
  page.switchers = switchers;
}
