// Stars: the star button of a resource or server page and the starred cards of the dashboard
// (crates/coolify-api/src/ui_stars.rs; upstream App\Livewire\StarToggle and App\Livewire\Dashboard).
import { request } from './api';

/** What a star button is shown for. A database page does not say which engine it is. */
export type StarKind = 'server' | 'application' | 'service' | 'database';

/** A card of the dashboard's "Starred" section (`Dashboard::starredResources()`). */
export interface StarredResource {
  star_id: number;
  type: StarKind;
  name: string;
  /** `Server`, or `{project} / {environment}`. */
  context: string;
  href: string;
  /** `$database->type()`, for the logo. */
  database_type: string | null;
  application: { build_pack: string | null; source_type: string | null; fqdn: string | null; is_http_basic_auth_enabled: boolean } | null;
  service: { logo: { src: string; fallback: string } | null } | null;
}

const resource = (kind: StarKind, uuid: string) => `/ui/stars/resource/${kind}/${encodeURIComponent(uuid)}`;

export const starsUi = {
  show: (kind: StarKind, uuid: string) => request<{ starred: boolean }>('GET', resource(kind, uuid)),
  toggle: (kind: StarKind, uuid: string) => request<{ starred: boolean }>('POST', `${resource(kind, uuid)}/toggle`, {}),
  unstar: (starId: number) => request<unknown>('DELETE', `/ui/stars/star/${starId}`),
  reorder: (starIds: number[]) => request<unknown>('POST', '/ui/stars/reorder', { star_ids: starIds }),
};

// Whether each resource is starred, by `kind:uuid`. Upstream keeps the two star buttons of a page
// (desktop and mobile top bars) in sync with the `star-toggled` event; here both read this state.
class StarState {
  starred = $state<Record<string, boolean>>({});
  private loading = new Set<string>();

  key(kind: StarKind, uuid: string): string {
    return `${kind}:${uuid}`;
  }

  /** `StarToggle::mount()`: asks once per resource; the second button of the page reuses the answer. */
  async load(kind: StarKind, uuid: string): Promise<void> {
    const key = this.key(kind, uuid);
    if (this.loading.has(key)) return;
    this.loading.add(key);
    try {
      this.starred[key] = (await starsUi.show(kind, uuid)).starred;
    } catch {
      // The button stays unstarred when the state cannot be read.
    } finally {
      this.loading.delete(key);
    }
  }

  /** `StarToggle::toggle()`. */
  async toggle(kind: StarKind, uuid: string): Promise<void> {
    const key = this.key(kind, uuid);
    this.starred[key] = (await starsUi.toggle(kind, uuid)).starred;
  }
}

export const stars = new StarState();
