// Client of the session endpoints behind the server and destination pages
// (`crates/coolify-api/src/ui_server_*.rs`).
import { request, ApiError } from './api';
import { notify } from './toast.svelte';

export const get = <T>(p: string) => request<T>('GET', `/ui${p}`);
export const post = <T = { message?: string }>(p: string, body: unknown = {}) => request<T>('POST', `/ui${p}`, body);
export const patch = <T = { message?: string }>(p: string, body: unknown = {}) => request<T>('PATCH', `/ui${p}`, body);
export const del = <T = { message?: string }>(p: string) => request<T>('DELETE', `/ui${p}`);

/** `handleError()`: the message as an "Error" toast. Field errors stay with the form. */
export function failure(e: unknown): ApiError | null {
  if (e instanceof ApiError) {
    if (!Object.keys(e.errors).length) notify.failure(e.message);
    return e;
  }
  notify.failure(String(e));
  return null;
}

/** Runs a Livewire action: its `success` toast, or the error toast. */
export async function action<T = { message?: string }>(run: () => Promise<T>, success?: string): Promise<T | null> {
  try {
    const r = await run();
    const message = success ?? (r as { message?: string } | null)?.message;
    if (message) notify.success(message);
    return r;
  } catch (e) {
    failure(e);
    return null;
  }
}

export interface DestinationListData {
  servers: { id: number; uuid: string; name: string }[];
  destinations: { uuid: string; name: string; server: string; is_swarm: boolean }[];
  network: string;
  can_create: boolean;
}

export interface DestinationData {
  uuid: string;
  name: string;
  network: string;
  is_swarm: boolean;
  server_ip: string | null;
  server_name: string | null;
  can_update: boolean;
  swarm_notice: string;
}

export interface DestinationResourceRow {
  uuid: string;
  type: string;
  name: string;
  project: string | null;
  environment: string | null;
  url: string | null;
  search: string;
}

/** `Str::kebab()` (`Str::snake($value, '-')`). */
export function kebab(value: string): string {
  const words = value.replace(/\s+/gu, '');
  return words
    .replace(/(.)(?=\p{Lu})/gu, '$1-')
    .toLowerCase();
}
