// Page endpoints of /profile (crates/coolify-api/src/ui_profile.rs) and passkey registration.
import { ApiError, request } from './api';

export interface PasskeyRow {
  id: number;
  name: string;
  authenticator: string | null;
  created_at: string | null;
  last_used_at: string | null;
}

export interface ProfileData {
  id: number;
  name: string;
  email: string;
  avatar_url: string | null;
  uses_sso: boolean;
  sso_provider_label: string | null;
  show_verification: boolean;
  new_email: string | null;
  code_expiry_minutes: number;
  two_factor_confirmed: boolean;
  status: string | null;
  has_errors: boolean;
  two_factor: { qr_svg?: string; secret?: string; url?: string; recovery_codes?: string[] };
  passkeys: { enabled: boolean; available: boolean; show_form: boolean; list: PasskeyRow[] };
  account_blockers: string[];
  account_actions: string[];
  confirm: { text: boolean; password: boolean };
}

type Message = { message: string };

export const profileUi = {
  get: () => request<ProfileData>('GET', '/ui/profile'),
  submit: (name: string) => request<Message>('POST', '/ui/profile', { name }),
  removeAvatar: () => request<Message>('DELETE', '/ui/profile/avatar'),
  requestEmail: (new_email: string) => request<Message>('POST', '/ui/profile/email/request', { new_email }),
  verifyEmail: (code: string) => request<Message>('POST', '/ui/profile/email/verify', { code }),
  resend: () => request<Message>('POST', '/ui/profile/email/resend', {}),
  cancelEmail: () => request<Message>('POST', '/ui/profile/email/cancel', {}),
  password: (current_password: string, new_password: string, new_password_confirmation: string) =>
    request<Message>('POST', '/ui/profile/password', { current_password, new_password, new_password_confirmation }),
  deleteAccount: (password: string) => request<Message>('POST', '/ui/profile/delete', { password }),
  confirmPassword: (password: string) => request<Message>('POST', '/ui/profile/confirm-password', { password }),
  enable2fa: () => request<Message>('POST', '/ui/profile/two-factor', {}),
  disable2fa: () => request<Message>('DELETE', '/ui/profile/two-factor'),
  confirm2fa: (code: string) => request<Message>('POST', '/ui/profile/two-factor/confirm', { code }),
  recoveryCodes: () => request<Message>('POST', '/ui/profile/two-factor/recovery-codes', {}),
  passkeyConfirm: () => request<Message>('POST', '/ui/profile/passkeys/confirm', {}),
  renamePasskey: (id: number, name: string) => request<Message>('POST', `/ui/profile/passkeys/${id}/rename`, { name }),
  deletePasskey: (id: number, password: string) => request<Message>('POST', `/ui/profile/passkeys/${id}/delete`, { password }),
};

const toBuffer = (s: string): ArrayBuffer => {
  const b = s.replace(/-/g, '+').replace(/_/g, '/');
  const raw = atob(b.padEnd(b.length + ((4 - (b.length % 4)) % 4), '='));
  const out = new Uint8Array(raw.length);
  for (let i = 0; i < raw.length; i++) out[i] = raw.charCodeAt(i);
  return out.buffer;
};

const fromBuffer = (buffer: ArrayBuffer): string => {
  let s = '';
  for (const byte of new Uint8Array(buffer)) s += String.fromCharCode(byte);
  return btoa(s).replace(/\+/g, '-').replace(/\//g, '_').replace(/=/g, '');
};

/** `window.passkeyDeviceName` of upstream resources/js/passkeys.js. */
export function passkeyDeviceName(): string {
  const ua = navigator.userAgent;
  const browser = /Edg\//.test(ua) ? 'Edge' : /Firefox\//.test(ua) ? 'Firefox' : /Chrome\//.test(ua) ? 'Chrome' : /Safari\//.test(ua) ? 'Safari' : 'Browser';
  const os = /iPhone|iPad/.test(ua) ? 'iOS' : /Android/.test(ua) ? 'Android' : /Mac/.test(ua) ? 'Mac' : /Windows/.test(ua) ? 'Windows' : /Linux/.test(ua) ? 'Linux' : 'device';
  return `${browser} on ${os}`;
}

/** `Passkeys.register({ name })`. A cancelled prompt throws an error named `UserCancelledError`. */
export async function registerPasskey(name: string): Promise<void> {
  if (typeof PublicKeyCredential !== 'function') throw new Error('This browser does not support passkeys.');
  const { options } = await request<{ options: Record<string, any> }>('GET', '/ui/profile/passkeys/options');
  let credential: PublicKeyCredential;
  try {
    credential = (await navigator.credentials.create({
      publicKey: {
        ...options,
        challenge: toBuffer(options.challenge),
        user: { ...options.user, id: toBuffer(options.user.id) },
        excludeCredentials: options.excludeCredentials.map((c: { type: 'public-key'; id: string }) => ({ ...c, id: toBuffer(c.id) })),
      } as PublicKeyCredentialCreationOptions,
    })) as PublicKeyCredential;
  } catch (e) {
    if (e instanceof Error && e.name === 'NotAllowedError') {
      const cancelled = new Error(e.message);
      cancelled.name = 'UserCancelledError';
      throw cancelled;
    }
    throw e;
  }
  const response = credential.response as AuthenticatorAttestationResponse;
  await request('POST', '/ui/profile/passkeys', {
    name,
    credential: {
      id: credential.id,
      rawId: fromBuffer(credential.rawId),
      type: credential.type,
      response: {
        attestationObject: fromBuffer(response.attestationObject),
        clientDataJSON: fromBuffer(response.clientDataJSON),
        transports: response.getTransports?.() ?? [],
      },
      clientExtensionResults: credential.getClientExtensionResults(),
      authenticatorAttachment: credential.authenticatorAttachment,
    },
  });
}

/** `password.confirm` said no: the caller goes to the confirmation page and comes back here. */
export function needsPasswordConfirmation(e: unknown): boolean {
  return e instanceof ApiError && e.status === 423;
}
