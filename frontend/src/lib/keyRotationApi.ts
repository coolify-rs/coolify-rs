// Client of the session endpoints behind `Livewire\Security\PrivateKey\Rotate`
// (`crates/coolify-api/src/ui_key_rotation.rs`).
import { get, post } from './serverApi';

export type RotationStatus =
  | 'moved'
  | 'moved_old_pending'
  | 'moved_old_not_removed'
  | 'failed'
  | 'old_key_not_found'
  | 'unreachable'
  | 'skipped';

export interface RotationRow {
  uuid: string;
  name: string;
  status?: RotationStatus | null;
  message?: string;
  reachable: boolean;
  transferred?: boolean;
}

export interface RotationData {
  rows: RotationRow[];
  has_servers: boolean;
  used_by_others: boolean;
  state: { status: 'running' | 'finished' | 'failed' | null; error?: string | null } | null;
  new_key: { uuid: string; name: string } | null;
  is_running: boolean;
  private_key_id: number;
  message?: string;
}

const base = (uuid: string) => `/keys/${encodeURIComponent(uuid)}`;

export const loadRotation = (uuid: string) => get<RotationData>(`${base(uuid)}/rotation`);
export const rotateKey = (uuid: string, key_type: 'ed25519' | 'rsa') => post<RotationData>(`${base(uuid)}/rotate`, { key_type });
