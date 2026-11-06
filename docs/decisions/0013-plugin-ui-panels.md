# ADR 0013: Plugin UI panels in sandboxed frames

Status: proposed (2026-11-06)

## Context

[ADR 0010](0010-plugins.md) planned a third phase for plugins: panels on the dashboard, on resource pages
and in the settings, rendered in sandboxed frames that talk to the plugin's own routes. This ADR decides
how.

A panel is HTML and JavaScript written by the plugin's author, shown inside coolify-rs's web interface. That
interface runs with a signed-in session that can deploy, read secrets and run commands on servers. Anything a
panel's code can reach, a malicious or compromised plugin can reach. The design therefore starts from what a
panel must not be able to do, and makes each of those impossible in the browser rather than forbidden by
convention.

The parity rule of ADR 0010 also applies: with no plugin installed, every page renders exactly as it does
without the plugin system.

## Threat model

What a hostile panel would try, and what stops it:

| Threat | Stopped by |
|---|---|
| **XSS on coolify-rs's origin**: script that reads the session, calls `/ui/...` endpoints as the user, or rewrites the page | The panel never runs on coolify-rs's origin. Its frame has `sandbox="allow-scripts"` without `allow-same-origin`, and its document's response carries `Content-Security-Policy: sandbox allow-scripts`, so it runs in an opaque origin: no cookies, no storage of ours, no access to `parent.document`. |
| **Data exfiltration**: sending what the panel can see to a server of the author's | `connect-src 'none'`, `img-src data:`, `default-src 'none'`, `form-action 'none'`: no fetch, XHR, WebSocket, beacon, remote image, font, stylesheet or form. The frame has no `allow-popups`, `allow-forms` or `allow-top-navigation`. See the residual risk below for navigation of the frame itself. |
| **Acting as the user**: deploying, deleting or reading secrets through the user's session | The panel has no session. Through the bridge it can only reach `/ui/ext/<its own id>/...`, which calls the plugin's own `handle-request` for the user's team. A plugin has no capability that reaches Coolify's data or servers (ADR 0010, section 3), so the bridge gives the panel nothing the plugin could not already do. |
| **Clickjacking**: framing coolify-rs, or a panel, on another site | The app shell keeps `frame-ancestors 'none'` and `X-Frame-Options: DENY`. A panel document has `frame-ancestors 'self'`, and is refused when the browser says it was requested as a page or by another site (`Sec-Fetch-Dest`, `Sec-Fetch-Site`). |
| **Phishing**: a panel that imitates coolify-rs's login or confirmation dialogs | The panel is confined to its box under a heading coolify-rs draws (the panel's title and the plugin's name, with an **Unverified** badge for plugins not from the registry). It cannot cover the page, open windows or navigate the page. A password typed into a fake form inside the panel cannot leave the browser (no network), but could reach the plugin through the bridge; the registry review and the admin's grant are the controls for that, as for any plugin code. |
| **Escaping the bridge**: `..`, encoded dots, other plugins' routes, absolute URLs | The page builds the URL itself as `/ui/ext/<id>` plus a path that must start with `/` and has no `//`, `\`, `#`, dot segments or `%2e`/`%5c`. The plugin id comes from the panel list, never from the message. |
| **Abusing the bridge**: floods, huge bodies, responses to other frames | At most 8 requests in flight per panel, bodies up to 64 KiB, answers up to 1 MiB, only `GET`, `POST`, `PUT`, `PATCH`, `DELETE`. Messages are only accepted when `event.source` is that panel's own frame window; answers are posted only to it. |
| **CSRF against the bridge's endpoint** | `/ui/ext/...` is behind the session guard: state-changing methods need the session's `X-CSRF-Token`, which only the page holds. The panel never sees the token. |
| **A plugin serving script on coolify-rs's origin** | Unchanged from ADR 0010: `/api/v1/ext` and `/ui/ext` serve HTML as a download with `nosniff` and a sandboxing CSP. Only the panel endpoint serves a plugin's HTML as a document, and only with the policy above. |

**Residual risk: navigation of the frame.** A sandboxed frame may still navigate *itself*, and browsers have
no policy that stops it (`navigate-to` was never shipped). A panel could therefore send what it holds to
another site as part of a URL. What it holds is only what its own plugin's routes return: data the plugin
already keeps for that team. The page closes a panel whose frame loads a second document, which limits this to
one request. Reviewers treat `ui` as a way to send the plugin's own data out, like `http`, and the registry's
review tiers apply.

## Decision

### The `ui` capability

```toml
[capabilities]
routes = true

[[capabilities.ui]]
slot = "dashboard"     # dashboard | application | server | settings
title = "Deployments"  # the heading coolify-rs draws above the panel
path = "/panels/dashboard"
```

- `ui` needs `routes`: a panel is a `GET` route of the plugin.
- At most 8 panels; paths are plain (`/a/b`, no query, no dot segments); titles are 1 to 64 characters.
- The admin grants it like every other capability; it shows as `ui: dashboard, application`. Moving a panel to
  another slot or another path is a change of capability, so an update that does it waits for approval.

### Slots

| Slot | Where | Resource passed |
|---|---|---|
| `dashboard` | Below the dashboard's projects and servers, under a section heading | none |
| `application` | A **Plugins** entry in the application's menu, before Danger Zone | the application's uuid |
| `server` | A **Plugins** entry in the server's menu, before Danger Zone | the server's uuid |
| `settings` | At the end of Settings → Plugins | none |

A panel appears for a team only when the plugin is installed, switched on for the instance and switched on
for that team. The application and server entries only exist while the team has a panel in that slot; their
URLs go home like any unknown page otherwise.

### Rendering: a separate document, not `srcdoc`

The page points the frame at `GET /ui/plugins/{publisher}/{name}/panels/{slot}?panel=&resource=&theme=`. That
endpoint calls the plugin's `handle-request` for the session's team, requires `200` with `text/html` and at
most 512 KiB, and answers the HTML with:

- `Content-Security-Policy: sandbox allow-scripts; default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src data:; connect-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'self'`
- the same policy (without `sandbox` and `frame-ancestors`, which meta tags can't carry) as the document's first
  element, followed by the plugin's markup with its doctype removed, so a policy the plugin adds can only make
  it stricter;
- `X-Content-Type-Options: nosniff`, `Cache-Control: no-store`, `Referrer-Policy: no-referrer`,
  `Cross-Origin-Resource-Policy: same-origin`.

We first planned to fetch the HTML and put it in the frame's `srcdoc`. That does not work here: an
`about:srcdoc` document inherits the policy of the page that embeds it, and coolify-rs's shell has
`script-src 'self'`, so every inline script in a panel would be blocked. Loosening the shell's policy to allow
inline scripts would weaken the whole interface's XSS protection. A document of its own gets its own policy
from its own response, and the `sandbox` directive keeps it in an opaque origin even if someone opens its URL
directly.

### The bridge

The panel posts messages to `parent`; the page answers to the panel's frame only.

| From | Message |
|---|---|
| panel | `{type: "coolify-rs:ready"}` |
| panel | `{type: "coolify-rs:request", id, method, path, body}`; `path` is relative to the plugin's prefix, `body` a string or null |
| panel | `{type: "coolify-rs:resize", height}`; the page clamps it to 48 to 4000 px |
| page | `{type: "coolify-rs:theme", theme: "light" or "dark"}`, on load, on `ready` and whenever the theme changes |
| page | `{type: "coolify-rs:response", id, status, contentType, body}` or `{type: "coolify-rs:response", id, error}` |

The page sends the request to `/ui/ext/{publisher}/{name}{path}` with the session cookie and CSRF token. That
endpoint is the plugin's routes for the session's team, with exactly the response hardening of
`/api/v1/ext`. The SDK ships the panel side (`coolify_rs_plugin::ui::BRIDGE`), so authors write
`coolify.json('GET', '/stats')`.

### Pages of the web UI

- **Settings → Plugins** (instance admins): installed plugins with version, id, origin, license, hash, an
  **Unverified** badge, the granted capabilities, the instance switch, why a plugin was switched off, and
  uninstall through the usual confirmation modal (type the id, then the password). Upload install in
  development mode: the first request answers the capability list, a modal shows exactly what will be granted,
  and approving sends it back as the grant. The registry catalogue (ADR 0010, section 7) lists the signed
  index with Refresh, Install, Update and the approval of updates that ask for more, showing what they add.
  Before the first refresh it is an empty catalogue with the Refresh button. The registry's errors (never
  fetched, already installed, yanked, revoked, a bad signature or hash) are shown as the API words them.
  Uninstalling a plugin that still owns rows of Coolify's tables (ADR 0012) shows the 409's message.
- **Team → Plugins**: plugins switched on for the instance, with their capabilities and the team's switch.
  Owners and admins change it; members see it. For a plugin holding `deploy` (ADR 0011), switching it on
  opens a dialog that asks whether it may queue the team's deployments, and the page shows and can change
  that answer.
- The menus get these entries only when there is something there: Settings → Plugins for instance admins
  while plugins are on (the registry catalogue is always there, and it is where the first plugin is
  installed); Team → Plugins when an installed plugin is switched on. `GET /ui/plugins/nav` tells the UI, and
  answers 404 when plugins are switched off.

## Consequences

- With no plugin installed, pages show no Team entry, no section, no frame and no resource tab. Instance admins
  see one extra Settings entry, Plugins, unless `COOLIFY_RS_PLUGINS=off`. A test compares the dashboard, team,
  session and settings data with and without the plugin system.
- Panels can't use web fonts, remote images or stylesheets; they inline what they need or use `data:` URLs.
  The SDK's `ui::STYLE` gives them coolify-rs's look in both themes.
- Panels can't link out (`allow-popups` is not granted). A plugin that needs to send the user somewhere shows
  the URL as text.
- Over plain HTTP, browsers may not send `Sec-Fetch-*`, so the panel URL can be opened as a page there. It
  still runs sandboxed with no network, but shows the plugin's content under coolify-rs's address; HTTPS
  closes this.
