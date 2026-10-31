// Endpoints of the sign-in screens (crates/coolify-api/src/ui_auth.rs) and `Passkeys.verify` /
// `Passkeys.autofill` of upstream's resources/js/passkeys.js.
import { ApiError, request, type SessionInfo } from './api';
import { fromBuffer, toBuffer } from './profileApi';

export interface GuestState {
  is_registration_enabled: boolean;
  can_register_with_oauth: boolean;
  enabled_oauth_providers: { provider: string; label: string }[];
  passkeys_available: boolean;
  transactional_emails: boolean;
  redirect_to_register: boolean;
  is_first_user: boolean;
  is_root_setup_window_closed: boolean;
  root_setup_minutes: number;
  two_factor_challenge: boolean;
  status: string | null;
  error: string | null;
  errors: string[];
}

export interface TwoFactorRequiredData {
  redirect?: string;
  team_name: string | null;
  confirmed: boolean;
  code_error: string | null;
  recovery_codes?: string[];
  qr_svg?: string;
  secret?: string;
  url?: string;
}

export interface OauthProvider {
  id: number;
  provider: string;
  enabled: boolean;
  client_id: string | null;
  client_secret: string | null;
  redirect_uri: string | null;
  tenant: string | null;
  base_url: string | null;
  custom_label: string | null;
  scopes: string;
  allow_registration: boolean;
  auto_join_root_team: boolean;
  require_email_verified: boolean;
  use_pkce: boolean;
  clock_skew_seconds: number | string;
  label: string;
  default_redirect_uri: string;
}

export interface OauthSettings {
  redirect?: string;
  disable_registration_when_oauth_enabled: boolean;
  providers: OauthProvider[];
}

type Message = { message: string };

export const authUi = {
  state: () => request<GuestState>('GET', '/ui/auth/state'),
  register: (body: { name: string; email: string; password: string; password_confirmation: string }) =>
    request<SessionInfo & { redirect: string }>('POST', '/ui/auth/register', body),
  forgotPassword: (email: string) => request<{ status: string }>('POST', '/ui/auth/forgot-password', { email }),
  resetPassword: (body: { token: string; email: string; password: string; password_confirmation: string }) =>
    request<Message>('POST', '/ui/auth/reset-password', body),
  twoFactor: (body: { code?: string; recovery_code?: string }) => request<SessionInfo>('POST', '/ui/session/two-factor', body),
  forceReset: (body: { email: string; password: string; password_confirmation: string }) =>
    request<Message & { signed_out: boolean }>('POST', '/ui/auth/force-password-reset', body),
  resendVerification: () => request<Message>('POST', '/ui/auth/verify/resend', {}),
  emailVerify: (path: string, query: string) => request<Message>('POST', '/ui/auth/email-verify', { path, query }),
  twoFactorRequired: () => request<TwoFactorRequiredData>('GET', '/ui/auth/two-factor-required'),
  confirmState: () => request<{ can_confirm_with_passkey: boolean }>('GET', '/ui/auth/confirm-password/state'),
  skipBoarding: () => request<Message>('POST', '/ui/onboarding/skip', {}),
  oauthSettings: () => request<OauthSettings>('GET', '/ui/settings/oauth'),
  saveOauthAll: (body: { providers: OauthProvider[]; disable_registration_when_oauth_enabled: boolean }) =>
    request<Message & { error: string | null; providers: OauthProvider[] }>('POST', '/ui/settings/oauth', body),
  saveOauth: (provider: string, data: OauthProvider) =>
    request<Message & { setting: OauthProvider }>('POST', `/ui/settings/oauth/${provider}`, data),
  toggleOauth: (provider: string, data: OauthProvider) =>
    request<Message & { setting: OauthProvider }>('POST', `/ui/settings/oauth/${provider}/toggle`, data),
  saveRegistrationPolicy: (value: boolean) => request<Message>('POST', '/ui/settings/oauth/registration-policy', { value }),
};

/** Every message of a failed request, in the order of `$errors->all()`. */
export function errorList(err: unknown): string[] {
  if (err instanceof ApiError) {
    const all = Object.values(err.errors).flat();
    return all.length ? all : [err.message];
  }
  return [err instanceof Error ? err.message : String(err)];
}

export interface PasskeyRoutes {
  options: string;
  submit: string;
}

export const passkeyLogin: PasskeyRoutes = { options: '/ui/auth/passkeys/login/options', submit: '/ui/auth/passkeys/login' };
export const passkeyConfirm: PasskeyRoutes = { options: '/ui/auth/passkeys/confirm/options', submit: '/ui/auth/passkeys/confirm' };

export const passkeysSupported = (): boolean => typeof PublicKeyCredential === 'function';

let pending: AbortController | null = null;

async function assertion(routes: PasskeyRoutes, mediation?: 'conditional'): Promise<SessionInfo | Message | null> {
  pending?.abort();
  const controller = new AbortController();
  pending = controller;
  const { options } = await request<{ options: Record<string, any> }>('GET', routes.options);
  let credential: PublicKeyCredential | null;
  try {
    credential = (await navigator.credentials.get({
      mediation,
      signal: controller.signal,
      publicKey: {
        ...options,
        challenge: toBuffer(options.challenge),
        allowCredentials: (options.allowCredentials ?? []).map((c: { type: 'public-key'; id: string }) => ({ ...c, id: toBuffer(c.id) })),
      } as PublicKeyCredentialRequestOptions,
    })) as PublicKeyCredential | null;
  } catch (e) {
    if (e instanceof Error && (e.name === 'NotAllowedError' || e.name === 'AbortError')) {
      const cancelled = new Error(e.message);
      cancelled.name = 'UserCancelledError';
      throw cancelled;
    }
    throw e;
  }
  if (!credential) return null;
  const response = credential.response as AuthenticatorAssertionResponse;
  return request<SessionInfo | Message>('POST', routes.submit, {
    credential: {
      id: credential.id,
      rawId: fromBuffer(credential.rawId),
      type: credential.type,
      response: {
        authenticatorData: fromBuffer(response.authenticatorData),
        clientDataJSON: fromBuffer(response.clientDataJSON),
        signature: fromBuffer(response.signature),
        userHandle: response.userHandle ? fromBuffer(response.userHandle) : null,
      },
      clientExtensionResults: credential.getClientExtensionResults(),
      authenticatorAttachment: credential.authenticatorAttachment,
    },
  });
}

/** `Passkeys.verify`. */
export const verifyPasskey = (routes: PasskeyRoutes) => assertion(routes);

/** `Passkeys.autofill`: waits on the email field's passkey suggestion. */
export async function autofillPasskey(routes: PasskeyRoutes): Promise<SessionInfo | Message | null> {
  if (!(await PublicKeyCredential.isConditionalMediationAvailable?.())) return null;
  try {
    return await assertion(routes, 'conditional');
  } catch (e) {
    if (e instanceof Error && e.name === 'UserCancelledError') return null;
    throw e;
  }
}

export function abortPasskey(): void {
  pending?.abort();
  pending = null;
}
