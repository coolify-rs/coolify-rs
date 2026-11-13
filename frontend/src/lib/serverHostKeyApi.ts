// Client of the session endpoints behind `Livewire\Server\Security\HostKey`
// (`crates/coolify-api/src/ui_server_host_key.rs`).
import { get, post } from './serverApi';

export type HostKeyMode = 'warn' | 'enforce' | 'off';

export interface HostKeyData {
  server_name: string;
  /** `SshMultiplexingHelper::hostKeyCheckingMode($server)`. */
  mode: HostKeyMode;
  /** `config('constants.ssh.host_key_checking')`. */
  instance_mode: string;
  fingerprints: string[];
  pending_fingerprints: string[];
  /** SHA-256 of the pending key that the page shows. */
  pending_key_hash: string | null;
  source_label: string;
  trusted_at: string | null;
  is_check_disabled: boolean;
  check_mode: 'warn' | 'enforce';
  can_update: boolean;
  confirm?: { text: boolean; password: boolean };
  message?: string;
}

const base = (uuid: string) => `/server-page/${encodeURIComponent(uuid)}/security/host-key`;

export const loadHostKey = (uuid: string) => get<HostKeyData>(base(uuid));
export const trustPendingKey = (uuid: string, pending_key_hash: string, password: string) =>
  post<HostKeyData>(`${base(uuid)}/trust`, { pending_key_hash, password });
export const resetHostKey = (uuid: string, password: string) => post<HostKeyData>(`${base(uuid)}/reset`, { password });
export const setHostKeyCheckDisabled = (uuid: string, disabled: boolean, password: string) =>
  post<HostKeyData>(`${base(uuid)}/check-disabled`, { disabled, password });
export const setHostKeyCheckMode = (uuid: string, mode: 'warn' | 'enforce') =>
  post<HostKeyData | { saved: false }>(`${base(uuid)}/check-mode`, { mode });
