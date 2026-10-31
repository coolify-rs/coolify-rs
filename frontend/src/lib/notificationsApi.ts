// Page endpoints of the notification screens (crates/coolify-api/src/ui_notifications.rs).
import { request } from './api';

export type Values = Record<string, any>;

export interface ChannelData {
  settings: Values;
  enabled: boolean;
  can_update: boolean;
  instance_admin?: boolean;
  email?: string;
  message?: string;
}

export const notificationsUi = {
  get: (channel: string) => request<ChannelData>('GET', `/ui/notifications/${channel}`),
  action: (channel: string, action: string, values: Values) =>
    request<ChannelData>('POST', `/ui/notifications/${channel}`, { action, values }),
  test: (channel: string, email?: string) => request<{ message: string }>('POST', `/ui/notifications/${channel}/test`, { email: email ?? null }),
};
