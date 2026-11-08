import { sessionApi, setCsrf, type SessionInfo } from './api';

/** Reactive session state shared by every page. */
class SessionState {
  info = $state<SessionInfo | null>(null);
  loaded = $state(false);

  get isAdmin(): boolean {
    const role = this.info?.current_team?.role;
    return role === 'owner' || role === 'admin';
  }

  /** `AccessResolver::coversWholeTeam` for the session team. */
  get coversWholeTeam(): boolean {
    return this.info?.current_team?.covers_whole_team !== false;
  }

  get isOwner(): boolean {
    return this.info?.current_team?.role === 'owner';
  }

  /** Upstream `isInstanceAdmin()`: admin or owner of the root team (id 0). */
  get isInstanceAdmin(): boolean {
    const root = this.info?.teams.find((t) => t.id === 0);
    return root?.role === 'owner' || root?.role === 'admin';
  }

  set(info: SessionInfo | null): void {
    this.info = info;
    setCsrf(info?.csrf_token ?? '');
    this.loaded = true;
  }

  async refresh(): Promise<void> {
    try {
      this.set(await sessionApi.current());
    } catch {
      this.set(null);
    }
  }
}

export const session = new SessionState();

/** Upstream's `redirect()->route('login')->with('error', ...)`: one message for the login page. */
let loginError: string | null = null;
export function setLoginError(message: string): void {
  loginError = message;
}
export function takeLoginError(): string | null {
  const m = loginError;
  loginError = null;
  return m;
}
