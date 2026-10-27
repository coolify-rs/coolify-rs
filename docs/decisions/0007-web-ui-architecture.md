# ADR 0007: Web UI architecture

Status: accepted (2026-10-27); refines ADR 0003. The look (own stylesheet from `DESIGN.md`) is superseded by
ADR 0008, which ports upstream's markup and stylesheet; the rest stands.

## Decision

- A new single-page UI in `frontend/` (Svelte 5, Vite, TypeScript, plain CSS). No CSS framework: the stylesheet
  transcribes the portable design language in upstream `DESIGN.md` (tokens, card/well, tactile buttons, tables), so the
  UI keeps upstream's look without reusing Livewire code, branding or logos.
- The UI has **no business logic**. It calls `/ui/api/v1/...`, which mounts the *same handlers* as the main `/api/v1`
  group behind a session guard (the equivalent of upstream's `/canvas/api` + `UseSessionTeamApiToken`). Validation
  messages, permissions and serialization come from the API; the UI renders them.
- Browser sessions: `POST /ui/session` reproduces Fortify's login checks (lowercased email, bcrypt, TOTP challenge for
  confirmed 2FA, 5 failed attempts per minute per email+IP and per email), then sets an `HttpOnly; SameSite=Lax` cookie.
  State-changing requests require the per-session `X-CSRF-Token`. Session abilities follow upstream:
  admins/owners `read, write, deploy, read:sensitive`, members `read`. Team membership, role, team 2FA policy and the
  password hash are re-checked on every request.
- The server serves the built UI from `COOLIFY_RS_UI_DIR` (default `frontend/dist`) with a strict CSP, SPA fallback
  to `index.html`, and immutable caching for hashed assets. `/api/*` and `/ui/*` never fall through to the UI.

## Alternatives considered

- Server-rendered HTML in Rust (askama + htmx): no Node toolchain, but duplicates validation/serialization logic in
  templates and diverges from the REST contract that external clients use.
- Reusing upstream Svelte canvases: they depend on Livewire pages around them; reuse is possible later per component.

## Consequences

- Node is a build-time dependency (Docker stage, CI job); the runtime stays a single Rust binary plus static files.
- Sessions are in memory for now (lost on restart). Moving them to a `coolify_rs` schema is planned.
