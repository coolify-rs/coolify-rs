import { sessionApi, setCsrf, type SessionInfo } from './api';

/** Reactive session state shared by every page. */
class SessionState {
  info = $state<SessionInfo | null>(null);
  loaded = $state(false);

  get isAdmin(): boolean {
    const role = this.info?.current_team?.role;
    return role === 'owner' || role === 'admin';
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
