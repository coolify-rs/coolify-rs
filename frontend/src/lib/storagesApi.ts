// Page endpoints of the backup storage screens (crates/coolify-api/src/ui_storages.rs).
import { request } from './api';

/** `App\Enums\BackupStorageType`. */
export type StorageType = 's3' | 'sftp' | 'ftps' | 'webdav';

export interface StorageRow {
  uuid: string;
  name: string;
  description: string | null;
  type: StorageType;
  type_label: string;
  is_usable: boolean;
}

/** Host, port, and directory of an SFTP or FTPS storage. */
export interface ServerParts {
  host: string;
  port: string | number | null;
  path: string;
}

/** A private key that an SFTP storage can log in with, and its public key for authorized_keys. */
export interface PrivateKeyOption {
  value: number;
  label: string;
  public_key: string | null;
}

export interface StorageForm {
  /** Only `Create` sends it: the type cannot change after creation. */
  type?: StorageType;
  name: string;
  description: string;
  endpoint: string;
  server_parts: ServerParts;
  bucket: string;
  region: string;
  key: string;
  /** A new secret key or password; empty keeps the saved one. */
  secret: string;
  use_path_style_endpoint: boolean;
  skip_tls_verification: boolean;
  /** "password" or "key". */
  sftp_auth: string;
  private_key_id: number | null;
}

export interface StorageDetails extends StorageForm {
  uuid: string;
  type: StorageType;
  type_label: string;
  default_port: number | null;
  has_saved_secret: boolean;
  private_keys: PrivateKeyOption[];
  /** The SHA256 fingerprint of the saved SFTP host key, as `ssh-keygen -l` prints it. */
  sftp_host_key_fingerprint: string | null;
  is_usable: boolean;
  /** The saved URL style is not the one detection picks: a user choice, kept when the endpoint changes. */
  use_path_style_endpoint_changed: boolean;
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
  list: () =>
    request<{ storages: StorageRow[]; can_create: boolean; types: { value: StorageType; label: string }[]; private_keys: PrivateKeyOption[] }>('GET', '/ui/storages'),
  create: (form: StorageForm) => request<{ uuid: string }>('POST', '/ui/storages', form),
  show: (uuid: string) => request<StorageDetails>('GET', one(uuid)),
  update: (uuid: string, form: StorageForm) => request<{ message: string; use_path_style_endpoint: boolean }>('PATCH', one(uuid), form),
  test: (uuid: string, form: StorageForm) =>
    request<{ message: string; details: string; is_usable: boolean; use_path_style_endpoint: boolean; sftp_host_key_fingerprint: string | null }>('POST', `${one(uuid)}/test`, form),
  resetHostKey: (uuid: string) => request<{ message: string; details: string; is_usable: boolean }>('POST', `${one(uuid)}/reset-host-key`),
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

/** Endpoint host suffixes of providers that reject path-style URLs (`S3Storage::VIRTUAL_HOSTED_STYLE_HOST_SUFFIXES`). */
const VIRTUAL_HOSTED_STYLE_HOST_SUFFIXES = ['aliyuncs.com', 'myqcloud.com', 'volces.com', 'bytepluses.com'];

/** `S3Storage::detectPathStyleEndpoint`: virtual-hosted style for providers that reject path-style URLs, else path-style. */
export function detectPathStyleEndpoint(endpoint: string | null): boolean {
  let host = '';
  try {
    host = endpoint ? new URL(endpoint).hostname.toLowerCase() : '';
  } catch {
    host = '';
  }
  return !VIRTUAL_HOSTED_STYLE_HOST_SUFFIXES.some((suffix) => host === suffix || host.endsWith(`.${suffix}`));
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
