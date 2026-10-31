// Page endpoints of the team screens (crates/coolify-api/src/ui_teams.rs).
import { ApiError, request } from './api';

export interface DangerData {
  name: string;
  personal_team: boolean;
  team_count: number;
  has_subscription: boolean;
  blockers: { projects: number; servers: number; sources: number };
  projects: { uuid: string; name: string }[];
  servers: { uuid: string; name: string }[];
}

export interface InvitationRow {
  id: number;
  email: string;
  via: string;
  role: string;
  link: string;
}

export interface InvitationsData {
  invitations: InvitationRow[];
  can_manage: boolean;
  email_enabled?: boolean;
  instance_admin?: boolean;
}

export interface AuditChange {
  old: unknown;
  new: unknown;
}

export interface AuditEventRow {
  id: number;
  event: string;
  source: string;
  action: string;
  actor_type: string;
  actor_name: string | null;
  actor_email: string | null;
  actor_token_name: string | null;
  description: string;
  changes: Record<string, AuditChange> | null;
  created_at: string;
}

export interface AuditData {
  actions: { value: string; label: string }[];
  events: AuditEventRow[];
  total: number;
  per_page: number;
  page: number;
}

export interface AdminData {
  users: { id: number; name: string; email: string }[];
  total: number;
  per_page: number;
  page: number;
  confirm: { text: boolean; password: boolean };
}

export interface InvitationView {
  team: string;
  role: string;
  already_member: boolean;
}

const query = (params: Record<string, string | number>) => new URLSearchParams(Object.entries(params).map(([k, v]) => [k, String(v)])).toString();

/** Sends an avatar as the raw JPEG the browser made. */
export async function uploadImage(path: string, file: Blob, csrf: string): Promise<{ message: string }> {
  const res = await fetch(path, {
    method: 'POST',
    headers: { Accept: 'application/json', 'Content-Type': 'image/jpeg', 'X-CSRF-Token': csrf },
    credentials: 'same-origin',
    body: file,
  });
  const data = await res.json().catch(() => ({}));
  if (!res.ok) throw new ApiError(res.status, data.message ?? `Request failed (${res.status})`, data.errors ?? {});
  return data;
}

/** A data URL: the page's Content-Security-Policy allows `data:` images, not `blob:`. */
export function dataUrl(file: Blob): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(reader.result as string);
    reader.onerror = reject;
    reader.readAsDataURL(file);
  });
}

/** `prepareAvatar` of the Blade views: centre crop to a 256 pixel JPEG on white. */
export async function compressAvatar(file: File): Promise<Blob> {
  const source = await dataUrl(file);
  const image = await new Promise<HTMLImageElement>((resolve, reject) => {
    const element = new Image();
    element.onload = () => resolve(element);
    element.onerror = reject;
    element.src = source;
  });
  const cropSize = Math.min(image.naturalWidth, image.naturalHeight);
  const canvas = document.createElement('canvas');
  canvas.width = 256;
  canvas.height = 256;
  const context = canvas.getContext('2d');
  if (!context) throw new Error('JPEG compression failed');
  context.fillStyle = '#ffffff';
  context.fillRect(0, 0, 256, 256);
  context.drawImage(image, (image.naturalWidth - cropSize) / 2, (image.naturalHeight - cropSize) / 2, cropSize, cropSize, 0, 0, 256, 256);
  return new Promise((resolve, reject) => canvas.toBlob((v) => (v ? resolve(v) : reject(new Error('JPEG compression failed'))), 'image/jpeg', 0.8));
}

export const teamsUi = {
  create: (name: string, description: string) => request<{ team_id: number }>('POST', '/ui/team/new', { name, description }),
  danger: () => request<DangerData>('GET', '/ui/team/danger'),
  remove: () => request<{ message: string }>('POST', '/ui/team/danger', {}),
  invitations: () => request<InvitationsData>('GET', '/ui/team/invitations'),
  invite: (email: string, role: string, via: 'email' | 'link') => request<{ message: string }>('POST', '/ui/team/invitations', { email, role, via }),
  revoke: (id: number) => request<{ message: string }>('DELETE', `/ui/team/invitations/${id}`),
  removeAvatar: () => request<{ message: string }>('DELETE', '/ui/team/avatar'),
  audit: (params: { search: string; action: string; source: string; per_page: number; page: number }) => request<AuditData>('GET', `/ui/team/audit-log?${query(params)}`),
  admin: (params: { search: string; team_filter: string; sort: string; per_page: number; page: number }) => request<AdminData>('GET', `/ui/team/admin?${query(params)}`),
  adminDelete: (id: number, password: string) => request<{ message: string }>('POST', `/ui/team/admin/${id}/delete`, { password }),
  invitation: (uuid: string) => request<InvitationView>('GET', `/ui/invitations/${encodeURIComponent(uuid)}`),
  acceptInvitation: (uuid: string) => request<{ message: string }>('POST', `/ui/invitations/${encodeURIComponent(uuid)}`, {}),
  link: (token: string) => request<InvitationView>('GET', `/ui/auth/link?${query({ token })}`),
  acceptLink: (token: string) => request<{ message: string }>('POST', '/ui/auth/link', { token }),
};
