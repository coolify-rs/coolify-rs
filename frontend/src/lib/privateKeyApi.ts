// Client of the session endpoints behind `Livewire\Security\PrivateKey\Show` (v4.7.5)
// (`crates/coolify-api/src/ui_private_key.rs`). The stored key is never part of the editor's data.
import { request } from './api';

export interface KeyUsage {
  type: string;
  name: string;
  url: string | null;
}

export interface PrivateKeyEditor {
  id: number;
  uuid: string;
  name: string;
  description: string | null;
  fingerprint: string | null;
  public_key: string;
  is_git_related: boolean;
  in_use: boolean;
  can_update: boolean;
  can_delete: boolean;
  /** Uses per type, types without a use left out, in the order of the list. */
  counts: { type: string; count: number }[];
  /** Rows that match the search, all types together. */
  matches: number;
  /** The first 20 of them. */
  rows: KeyUsage[];
}

export const privateKeyUi = {
  show: (uuid: string, usageSearch = '') =>
    request<PrivateKeyEditor>('GET', `/ui/keys/${encodeURIComponent(uuid)}${usageSearch ? `?usage_search=${encodeURIComponent(usageSearch)}` : ''}`),
  reveal: (uuid: string, password: string) => request<{ private_key: string }>('POST', `/ui/keys/${encodeURIComponent(uuid)}/reveal`, { password }),
};
