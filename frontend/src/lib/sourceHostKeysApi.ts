// Client of "SSH host keys" on the application Source page (`Project\Application\Source`).
import { rawRequest as request } from './api';

/** A row of the host key table: a saved key, or one the provider publishes. */
export interface HostKeyFingerprint {
  id: string;
  published: boolean;
  host: string;
  type: string;
  fingerprint: string;
}

/** A key that the Git host sends now, which the user can accept or decline. */
export interface FetchedHostKey {
  host: string;
  type: string;
  fingerprint: string;
}

/** What `GET /ui/applications/{uuid}/source` adds for the section. */
export interface SourceHostKeys {
  /** `$application->gitSshHost() !== null`. */
  uses_git_ssh: boolean;
  /** `$usesGitSsh || filled($application->git_ssh_known_hosts)`. */
  show_ssh_host_keys: boolean;
  git_ssh_known_hosts: string | null;
  host_key_fingerprints: HostKeyFingerprint[];
}

const path = (uuid: string) => `/ui/applications/${encodeURIComponent(uuid)}/source/host-keys`;

export const sourceHostKeysUi = {
  /** `fetchHostKeys()`. */
  fetch: (uuid: string) => request<{ success: string[]; error: string | null; fetched_host_keys: FetchedHostKey[] }>('POST', `${path(uuid)}/fetch`, {}),
  /** `acceptFetchedHostKeys()`. */
  accept: (uuid: string) => request<{ message: string }>('POST', `${path(uuid)}/accept`, {}),
  /** `declineFetchedHostKeys()`. */
  decline: (uuid: string) => request<unknown>('POST', `${path(uuid)}/decline`, {}),
  /** `removeHostKey($id)`. */
  remove: (uuid: string, id: string) => request<{ message: string }>('DELETE', `${path(uuid)}/${encodeURIComponent(id)}`),
};
