# ADR 0008: Port upstream's UI markup and stylesheet

Status: accepted (2026-10-27); supersedes the "look" part of ADR 0007

## Decision

- The web UI reproduces upstream Coolify's interface (main @ b5823c7) so it works as a drop-in replacement: same
  layout, navigation, URLs, markup and classes. Each Svelte page and component names the Blade view it ports.
- Upstream's stylesheet (`resources/css/{app,fonts,utilities}.css`) and fonts are vendored unmodified in
  `frontend/src/upstream/` and built with Tailwind 4, as upstream does. coolify-rs components only add their own
  sources for Tailwind to scan; they do not restyle anything.
- Upstream's icon set (`components/reicon.blade.php`) is generated into `frontend/src/upstream/reicons.ts` by
  `frontend/scripts/gen-reicons.mjs`, so icons come from the snapshot, not from copies by hand.
- Alpine and Livewire behaviour is rewritten in Svelte 5. Routing uses upstream's URLs (`/`, `/projects`,
  `/project/{uuid}`, `/project/{uuid}/environment/{uuid}`, `/security/private-key`, `/team`, `/team/members`,
  `/profile/appearance`, ...). Every navigation entry that coolify-rs does not implement opens a "not available in
  coolify-rs yet" page inside the same chrome.
- Branding is coolify-rs's own: the wordmark "coolify-rs" and an original logo (`frontend/public/coolify-rs-logo.svg`).
  Coolify's name and logo are not used.
- Pages that need data the REST API does not expose use small read models under `/ui/*`, session-guarded like the
  `/ui/api/v1` bridge: `/ui/dashboard` (projects with resource counts, server status), `/ui/projects/{uuid}`
  (environments with resource counts, breadcrumb lists), `/ui/keys` (in-use status, generate, delete unused,
  derive public key) and `/ui/team` (team settings, member roles and removal). Their writes reproduce the Livewire
  actions they replace (validation messages, permission checks, token revocation, `HasSafeStringAttribute`).

## Verification

`frontend/` pages are compared against the reference instance by headless screenshots of the same URLs and the same
interactions (menus, modals, collapsed sidebar, mobile sheet, light theme). The endpoints are covered by
`crates/coolify-api/tests/ui_pages.rs`.

## Consequences

- Upstream UI changes reach coolify-rs as diffs to vendored CSS and to Blade views with a known Svelte counterpart,
  which the migration engine can classify per file.
- The vendored CSS is about 340 kB before compression; the generated icon table adds about 60 kB of JavaScript.
- Upstream features with no backend in coolify-rs (canvas views, icon and avatar uploads, invitations, custom color
  themes, global search) are left out of the ported pages and listed in KNOWN_LIMITATIONS.
