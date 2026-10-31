// Page endpoints of the S3 storage screens (crates/coolify-api/src/ui_storages.rs).
import { request } from './api';

export interface StorageRow {
  uuid: string;
  name: string;
  description: string | null;
  is_usable: boolean;
}

export interface StorageForm {
  name: string;
  description: string;
  endpoint: string;
  bucket: string;
  region: string;
  key: string;
  secret: string;
}

export interface StorageDetails extends StorageForm {
  uuid: string;
  is_usable: boolean;
  can_update: boolean;
  backup_count: number;
  storages: { name: string; uuid: string }[];
}

export interface StorageResources {
  storage_id: number;
  backups: {
    id: number;
    uuid: string;
    frequency: string;
    enabled: boolean;
    database: { name: string; uuid?: string; link?: string | null };
  }[];
  volume_backups: {
    id: number;
    uuid: string;
    frequency: string;
    enabled: boolean;
    target: { name: string; type: string; resource?: { name: string } | null };
  }[];
  storages: { id: number; name: string; is_usable: boolean }[];
}

const one = (uuid: string) => `/ui/storages/${encodeURIComponent(uuid)}`;

export const storagesUi = {
  list: () => request<{ storages: StorageRow[]; can_create: boolean }>('GET', '/ui/storages'),
  create: (form: StorageForm) => request<{ uuid: string }>('POST', '/ui/storages', form),
  show: (uuid: string) => request<StorageDetails>('GET', one(uuid)),
  update: (uuid: string, form: StorageForm) => request<{ message: string }>('PATCH', one(uuid), form),
  test: (uuid: string, form: StorageForm) => request<{ message: string; details: string; is_usable: boolean }>('POST', `${one(uuid)}/test`, form),
  destroy: (uuid: string) => request<unknown>('DELETE', one(uuid)),
  resources: (uuid: string) => request<StorageResources>('GET', `${one(uuid)}/resources`),
  move: (uuid: string, backup: number, volume: boolean, target: number) =>
    request<{ message: string; details: string }>('POST', `${one(uuid)}/resources/move`, { backup, volume, target }),
  disable: (uuid: string, backup: number, volume: boolean) => request<{ message: string; details: string }>('POST', `${one(uuid)}/resources/disable`, { backup, volume }),
};

export interface EndpointParts {
  scheme: string;
  host: string;
  port: string | number | null;
  path: string;
}

/** `DomainUrlParts::compose`. */
export function composeEndpoint(p: EndpointParts): string {
  const scheme = p.scheme.trim().toLowerCase() === 'http' ? 'http' : 'https';
  const host = p.host.trim();
  const port = String(p.port ?? '').trim();
  let path = p.path.trim();
  if (path !== '' && !path.startsWith('/') && !path.startsWith('?') && !path.startsWith('#')) path = `/${path}`;
  return `${scheme}://${host}${port !== '' ? `:${port}` : ''}${path}`;
}

/** `DomainUrlParts::split`. */
export function splitEndpoint(url: string | null): EndpointParts {
  const empty = { scheme: 'https', host: '', port: '', path: '' };
  if (!url) return empty;
  try {
    const u = new URL(url);
    if (!u.hostname) return empty;
    const rawPath = url.slice(url.indexOf(u.host) + u.host.length);
    return { scheme: u.protocol === 'http:' ? 'http' : 'https', host: u.hostname, port: u.port, path: rawPath };
  } catch {
    return empty;
  }
}
