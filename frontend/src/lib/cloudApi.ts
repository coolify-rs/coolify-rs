// Client of `crates/coolify-api/src/ui_server_cloud.rs`, with the PHP helpers the
// computed labels of `Server\New\By*` need.
import { request } from './api';

export type CloudToken = { id: number; uuid: string; name: string | null; description: string | null; created_at?: string };
export type CloudKey = { id: number; name: string };
export type CloudScript = { id: number; name: string; script: string };
export type CloudForm = { tokens: CloudToken[]; keys: CloudKey[]; scripts: CloudScript[]; name: string };
// Provider payloads are passed on as they come.
// eslint-disable-next-line @typescript-eslint/no-explicit-any
export type Item = Record<string, any>;

export const cloudUi = {
  form: (provider: string) => request<CloudForm>('GET', `/ui/cloud/${provider}/form`),
  data: <T>(provider: string, token: string) => request<T & { error?: string }>('GET', `/ui/cloud/${provider}/${encodeURIComponent(token)}/data`),
  create: (provider: string, token: string, body: Record<string, unknown>) =>
    request<{ uuid: string }>('POST', `/ui/cloud/${provider}/${encodeURIComponent(token)}/servers`, body),
  addToken: (body: { provider: string; token: string; name: string; description: string }) =>
    request<{ id: number; uuid: string; message: string }>('POST', '/ui/cloud/tokens', body),
};

/** PHP `number_format($n, $decimals)`. */
export function numberFormat(n: unknown, decimals = 2): string {
  const v = Number(n);
  const factor = 10 ** decimals;
  // `_php_math_round`: half away from zero, tolerant of binary fractions.
  const rounded = Math.round(Math.abs(v) * factor + 1e-9) / factor;
  const [whole, fraction] = rounded.toFixed(decimals).split('.');
  const grouped = whole.replace(/\B(?=(\d{3})+(?!\d))/g, ',');
  return `${v < 0 && rounded !== 0 ? '-' : ''}${grouped}${fraction ? `.${fraction}` : ''}`;
}

/** PHP truthiness of a JSON value. */
export const truthy = (v: unknown) => v !== null && v !== undefined && v !== false && v !== 0 && v !== '' && v !== '0';

/** Laravel `filled()`. */
export const filled = (v: unknown) => v !== null && v !== undefined && String(v).trim() !== '';

/** `str($word)->plural($count)` for the regular words used here. */
export const plural = (word: string, count: number) => (count === 1 ? word : `${word}s`);

/** A listbox value as the integer Livewire's `?int` property holds. */
export const intOrNull = (v: string | number | null | undefined) => (v === null || v === undefined || v === '' ? null : Number(v));

/** The value `<x-forms.listbox>` sends for the selected option: as the provider gave it. */
export function optionValue(options: { value: string | number }[], selected: string | number | null | undefined) {
  return options.find((o) => String(o.value) === String(selected))?.value ?? null;
}
