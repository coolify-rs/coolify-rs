// Page endpoints of the notification screens (crates/coolify-api/src/ui_notifications.rs).
import { request } from './api';
import type { EmailProvider, SmtpPreset, SmtpServerOption } from './smtpServers';

export type Values = Record<string, any>;

export interface ChannelData {
  settings: Values;
  enabled: boolean;
  can_update: boolean;
  instance_admin?: boolean;
  email?: string;
  message?: string;
  /** Email: `activeEmailProvider()`, the provider that sends the team's email now. */
  active_provider?: EmailProvider;
  /** Email: `$settings->smtpServerName()`. */
  smtp_server_name?: string;
  /** Email: `smtpServerOptions()`. */
  smtp_server_options?: SmtpServerOption[];
  smtp_presets?: SmtpPreset[];
  /** Email: the selected provider is not complete yet, so nothing was saved. */
  pending?: boolean;
  /** Email: the fields `addSmtpServer` starts a new server with. */
  new_server?: Values;
}

export const notificationsUi = {
  get: (channel: string) => request<ChannelData>('GET', `/ui/notifications/${channel}`),
  /** `extra` carries the arguments of an action that are not form fields (a server id or name). */
  action: (channel: string, action: string, values: Values, extra: Values = {}) =>
    request<ChannelData>('POST', `/ui/notifications/${channel}`, { ...extra, action, values }),
  test: (channel: string, email?: string) => request<{ message: string }>('POST', `/ui/notifications/${channel}/test`, { email: email ?? null }),
};
