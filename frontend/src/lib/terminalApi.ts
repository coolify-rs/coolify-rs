// `Project\Shared\Terminal` and `Project\Shared\ExecuteContainerCommand`.
import { request } from './api';

/** What `sendTerminalCommand` leaves on the component; `token` is issued only when `ok`. */
export type SessionAnswer = {
  ok: boolean;
  message?: string;
  token?: string;
  has_shell?: boolean;
  attach_available?: boolean;
  mode?: 'shell' | 'attach';
};

export type TerminalServer = { uuid: string; name: string; terminal_enabled: boolean; functional: boolean };
export type TerminalContainer = { name: string; server_uuid: string; server_name: string; value: string; label: string };
export type TerminalKind = 'application' | 'service' | 'database' | 'server';

export const terminalApi = {
  session: (is_container: boolean, identifier: string, server_uuid: string, requested_mode?: string) =>
    request<SessionAnswer>('POST', '/ui/terminal/session', { is_container, identifier, server_uuid, requested_mode }),
  keepAlive: () => request<void>('POST', '/ui/terminal/keep-alive', {}),
  mount: (kind: TerminalKind, uuid: string) =>
    request<{ type: TerminalKind; name: string; servers: TerminalServer[] }>('GET', `/ui/resources/${kind}/${encodeURIComponent(uuid)}/terminal`),
  containers: (kind: TerminalKind, uuid: string) =>
    request<{ containers: TerminalContainer[] }>('POST', `/ui/resources/${kind}/${encodeURIComponent(uuid)}/terminal/containers`, {}),
  connect: (kind: TerminalKind, uuid: string, selected_container?: string) =>
    request<SessionAnswer>('POST', `/ui/resources/${kind}/${encodeURIComponent(uuid)}/terminal/connect`, { selected_container }),
};
