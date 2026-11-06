# Writing coolify-rs plugins

A plugin is a WebAssembly component that coolify-rs runs in a sandbox. It can react to events, keep its
own data and serve its own API routes, and it can do only what its manifest asks for and an admin grants.
[ADR 0010](decisions/0010-plugins.md) explains the design and the security model.

The interface is [`crates/coolify-plugins/wit/plugin.wit`](../crates/coolify-plugins/wit/plugin.wit)
(`coolify-rs:plugin@0.1.0`). Any language that can build a WebAssembly component can implement it. This
guide uses Rust and the SDK in [`plugins/sdk-rust`](../plugins/sdk-rust).

## A plugin in Rust

[`plugins/examples/deploy-stats`](../plugins/examples/deploy-stats) is a complete example. It counts
deployments per team and serves the totals:

```rust
use coolify_rs_plugin::{Caller, Event, Plugin, Request, Response, storage};

struct DeployStats;

impl Plugin for DeployStats {
    fn on_event(event: Event) -> Result<(), String> {
        let mut n: u64 = storage::get_json("count")?.unwrap_or(0);
        n += 1;
        storage::set_json("count", &n)
    }

    fn handle_request(_caller: Caller, request: Request) -> Response {
        let n: u64 = storage::get_json("count").ok().flatten().unwrap_or(0);
        Response::json(200, &serde_json::json!({ "deployments": n }))
    }
}

coolify_rs_plugin::export!(DeployStats);
```

Build it as a library (`crate-type = ["cdylib"]`) for `wasm32-wasip2`:

```bash
cargo build --release --target wasm32-wasip2
```

The component is `target/wasm32-wasip2/release/<name>.wasm`.

## The manifest

Next to the component, `plugin.toml` says who the plugin is and what it needs:

```toml
id = "acme/deploy-stats"          # publisher/name; the publisher owns the source repository
name = "Deploy stats"
version = "0.1.0"
description = "Counts successful and failed deployments per team."
license = "Apache-2.0"
source = "https://github.com/acme/deploy-stats"
api = "0.1"                       # the plugin interface version

[capabilities]
events = ["deployment.succeeded", "deployment.failed"]
storage = true
routes = true
http = ["hooks.slack.com"]
```

Ask for the least you need. The admin sees the list, reviewers check every entry, and coolify-rs refuses to
load a component that imports an interface its manifest did not ask for.

| Capability | Gives the plugin |
|---|---|
| `events` | Calls to `on-event` for the listed kinds, for teams that enabled the plugin |
| `storage` | A key-value store per team. Keys are up to 256 bytes and values up to 1 MiB, with 10 MiB per team |
| `routes` | Calls to `handle-request` for requests to `/api/v1/ext/<publisher>/<name>/...` |
| `http` | HTTPS requests to the listed hosts and no others |
| `ui` | Panels in the web interface, each a `GET` route of the plugin (needs `routes`; see [UI panels](#ui-panels)) |

Logging (`coolify_rs_plugin::log`) is always available.

## What a plugin can rely on, and what it can't do

- **Fresh instance per call.** Every call runs in a new instance, so nothing stays in memory between
  calls. Use `storage`.
- **One team per call.** Each call is for exactly one team. `storage` reads and writes that team's data
  only, and a route call carries the caller's team, user and token abilities.
- **Limits.** Instances get 64 MiB of memory. Events must finish within 5 s and requests within 10 s. After
  5 crashes or timeouts in a row, coolify-rs switches the plugin off and tells the admins.
- **No direct access to the system.** There are no files, no environment variables and no sockets. The
  standard library's file functions return errors.
- **Restricted HTTP.**
  - Requests go only to listed host names, over HTTPS on port 443.
  - Hosts that resolve to private, loopback or link-local addresses are refused.
  - Redirects are not followed, and bodies are limited to 1 MiB each way.
- **Limited view of the caller.** A route never sees the caller's API token or cookies.
- **Restricted responses.**
  - A route's response can set only `content-type`, `cache-control`, `etag` and `last-modified`.
  - JSON, plain text and CSV are served as they are. Any other content type becomes a download, so a
    plugin cannot serve pages that run in the browser.

## Events

Events come from the same places as Coolify's notifications, whether or not the team has a notification
channel enabled. `payload` is a JSON object with the notification's data, and `version` is 1 for every
kind.

| Kind | When |
|---|---|
| `deployment.succeeded`, `deployment.failed` | An application deployment finished |
| `application.status_changed` | An application stopped or started outside a deployment |
| `container.restarted`, `container.restart_limit_reached` | A container restarted, or hit its restart limit |
| `backup.succeeded`, `backup.succeeded_with_s3_warning`, `backup.failed`, `backup.missing` | Database backups |
| `volume_backup.succeeded`, `volume_backup.failed`, `volume_backup.missing`, `volume_backup.recovery_failed` | Volume backups |
| `scheduled_task.succeeded`, `scheduled_task.failed` | Scheduled tasks |
| `server.reachable`, `server.unreachable` | A server came back or went away |
| `server.force_disabled`, `server.force_enabled` | A server was disabled or re-enabled for the plan's limits |
| `server.docker_cleanup_succeeded`, `server.docker_cleanup_failed` | Docker cleanup |
| `server.high_disk_usage` | Disk usage passed the server's threshold |
| `server.patch_check` | OS updates are available |
| `server.hetzner_deletion_failed` | Deleting a Hetzner server failed |
| `proxy.traefik_outdated` | A newer Traefik version is available |
| `ssl.certificates_expiring` | Certificates are close to expiry |
| `api_token.expiring` | An API token expires soon |
| `general` | A general message |

New kinds may be added at any time. Renaming or removing one needs a new major version of the interface.

## UI panels

A plugin can show panels in the web interface. Each panel is the HTML answer of one of its `GET` routes,
declared in the manifest:

```toml
[capabilities]
routes = true

[[capabilities.ui]]
slot = "dashboard"            # dashboard, application, server or settings
title = "Deployments"         # the heading coolify-rs draws above the panel
path = "/panels/dashboard"    # a GET route of the plugin, without a query
```

| Slot | Where the panel shows | Query the route receives |
|---|---|---|
| `dashboard` | Below the dashboard's projects and servers | `slot=dashboard` |
| `application` | A Plugins entry in each application's menu | `slot=application&resource=<application uuid>` |
| `server` | A Plugins entry in each server's menu | `slot=server&resource=<server uuid>` |
| `settings` | At the end of Settings → Plugins | `slot=settings` |

A panel shows for a team only when the plugin is switched on for the instance and for that team. The route must
answer `200` with `text/html`, at most 512 KiB. The call is an ordinary route call for the signed-in user's
team, so `storage` holds that team's data.

**The sandbox.** coolify-rs shows the panel in a frame with `sandbox="allow-scripts"` and this policy, set both
as a header and as the document's first element
([ADR 0013](decisions/0013-plugin-ui-panels.md) explains why):

```
default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src data:; connect-src 'none'
```

So a panel:

- can use inline `<script>` and `<style>`, and images as `data:` URLs;
- can't load anything from a URL: no scripts, stylesheets, fonts, images, `fetch` or WebSocket;
- can't read cookies or storage, reach the page around it, open windows, submit forms or follow links. Show a
  URL as text if the user needs one;
- reaches its own plugin only through the bridge below.

**The bridge.** The panel asks the page to call the plugin's own routes for it. The page checks every message,
adds the session and CSRF token, and sends the request to `/ui/ext/<publisher>/<name><path>`, which reaches
`handle-request` exactly like `/api/v1/ext/...` does, for the signed-in user's team. Requests are limited to
`GET`, `POST`, `PUT`, `PATCH` and `DELETE`, a path inside the plugin's prefix, a 64 KiB body and a 1 MiB answer,
with at most 8 in flight. The page also sends the theme (`light` or `dark`) when the panel loads and whenever
it changes, and sizes the frame to the panel's height.

With the Rust SDK, `coolify_rs_plugin::ui::panel(body)` returns the panel with the SDK's styles and the bridge
script, and `Response::html` returns HTML of your own. In the page, `coolify.json(method, path, body?)` calls a
route and parses its JSON answer, `coolify.request(...)` returns `{status, contentType, body}`, and the root
element has the `dark` class in the dark theme:

```rust
("GET", "/panels/dashboard") => ui::panel(r#"
  <div class="stats"><div class="stat"><span class="muted">Succeeded</span><b id="n">…</b></div></div>
  <script>coolify.json('GET', '/stats').then(s => n.textContent = s.succeeded)</script>"#),
```

In another language, include this script after your markup. It is the whole panel side of the protocol:

```html
<script>
(function () {
  var next = 0, waiting = {}, root = document.documentElement;
  function send(m) { parent.postMessage(m, '*'); }
  var coolify = window.coolify = {
    theme: 'light', onTheme: null,
    request: function (method, path, body) {
      return new Promise(function (resolve, reject) {
        var id = ++next;
        waiting[id] = { resolve: resolve, reject: reject };
        if (body !== undefined && body !== null && typeof body !== 'string') body = JSON.stringify(body);
        send({ type: 'coolify-rs:request', id: id, method: method, path: path, body: body === undefined ? null : body });
      });
    },
    json: function (method, path, body) {
      return coolify.request(method, path, body).then(function (r) {
        if (r.status < 200 || r.status > 299) throw new Error('HTTP ' + r.status + ': ' + r.body);
        return JSON.parse(r.body);
      });
    }
  };
  window.addEventListener('message', function (e) {
    if (e.source !== parent || !e.data) return;
    var m = e.data;
    if (m.type === 'coolify-rs:theme') {
      coolify.theme = m.theme === 'dark' ? 'dark' : 'light';
      root.classList.toggle('dark', coolify.theme === 'dark');
      root.style.colorScheme = coolify.theme;
      if (typeof coolify.onTheme === 'function') coolify.onTheme(coolify.theme);
    } else if (m.type === 'coolify-rs:response' && waiting[m.id]) {
      var w = waiting[m.id];
      delete waiting[m.id];
      if (m.error) w.reject(new Error(m.error));
      else w.resolve({ status: m.status, contentType: m.contentType, body: m.body });
    }
  });
  // The content's height, not the frame's: scrollHeight never drops below the frame.
  function size() { send({ type: 'coolify-rs:resize', height: Math.ceil(root.getBoundingClientRect().height) }); }
  if (window.ResizeObserver) new ResizeObserver(size).observe(root);
  window.addEventListener('load', size);
  send({ type: 'coolify-rs:ready' });
})();
</script>
```

| Message | Direction |
|---|---|
| `{type: "coolify-rs:ready"}` | panel → page: the page answers with the theme |
| `{type: "coolify-rs:request", id, method, path, body}` | panel → page: `path` starts with `/` and is relative to the plugin's prefix; `body` is a string or `null` |
| `{type: "coolify-rs:resize", height}` | panel → page: the frame's height in pixels, 48 to 4000 |
| `{type: "coolify-rs:theme", theme}` | page → panel: `"light"` or `"dark"` |
| `{type: "coolify-rs:response", id, status, contentType, body}` or `{..., id, error}` | page → panel |

A panel that navigates its own frame to another page is closed. Escape everything you render that came from
users or events: application and server names are user input.

[`plugins/examples/deploy-board`](../plugins/examples/deploy-board) is a complete example with a dashboard panel
and an application panel.

## Installing a plugin

Instance admins manage plugins in the web interface under **Settings → Plugins**, and each team switches them on
under **Team → Plugins**. The menus only show these entries when there is something to manage: Settings when a
plugin is installed, development mode is on or the registry catalogue is available, and Team when a plugin is
switched on for the instance.

Until the [registry](decisions/0010-plugins.md#7-distribution-a-signed-registry-not-an-app-store) exists,
plugins are installed directly. Start coolify-rs with `COOLIFY_RS_ALLOW_UNVERIFIED_PLUGINS=true`. Settings →
Plugins then has **Install from files**: choose `plugin.toml` and the `.wasm`, review the capabilities, and
approve. Or, with an API token of an owner or admin of the root team:

```bash
curl -X POST https://coolify.example.com/api/v1/plugins -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d "$(jq -n --rawfile m plugin.toml --arg c "$(base64 < plugin.wasm)" '{manifest: $m, component: $c}')"
```

The first request fails with the list of capabilities the plugin asks for. Read the list, then send it back
as `grant` to approve and install:

```bash
curl -X POST https://coolify.example.com/api/v1/plugins -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d "$(jq -n --rawfile m plugin.toml --arg c "$(base64 < plugin.wasm)" \
       '{manifest: $m, component: $c, grant: ["events: deployment.succeeded, deployment.failed", "storage", "routes"]}')"
```

Each team then turns it on for itself:

```bash
curl -X POST https://coolify.example.com/api/v1/plugins/acme/deploy-stats/enable -H "Authorization: Bearer $TOKEN"
```

| Endpoint | Who can call it |
|---|---|
| `GET /api/v1/plugins`, `GET /api/v1/plugins/{publisher}/{name}` | Any token with `read` |
| `POST /api/v1/plugins` | Owners and admins of the root team, with `write` |
| `PATCH /api/v1/plugins/{publisher}/{name}` with `{"enabled": false}` | Owners and admins of the root team: switches the plugin off for everyone |
| `DELETE /api/v1/plugins/{publisher}/{name}` | Owners and admins of the root team: uninstalls the plugin and deletes its data |
| `POST /api/v1/plugins/{publisher}/{name}/enable`, `/disable` | Team owners and admins, for their own team |
| `POST /api/v1/plugins/{publisher}/{name}/test-event` with `{"kind": ..., "payload": {...}}` | Team owners and admins, in development mode only: delivers one event to the plugin for their team and reports whether it succeeded |
| `/api/v1/ext/{publisher}/{name}/...` | Any token with `read`, for a team that enabled the plugin |

The web interface uses session endpoints with the same rules (`crates/coolify-api/src/ui_plugins.rs`): instance
admins are owners and admins of the root team, whichever team they are working in.

| Endpoint | Who can call it |
|---|---|
| `GET /ui/plugins/nav` | Any signed-in user: the menu entries and the panels of their team |
| `GET /ui/plugins`, `POST /ui/plugins`, `PATCH` and `DELETE /ui/plugins/{publisher}/{name}` | Instance admins; installing from files needs development mode, uninstalling the password step |
| `GET /ui/plugins/catalog`, `POST /ui/plugins/catalog/refresh`, `POST /ui/plugins/catalog/install`, `POST /ui/plugins/{publisher}/{name}/update` | Instance admins, when the instance has the registry catalogue |
| `GET /ui/plugins/team`, `POST /ui/plugins/{publisher}/{name}/team` | Any team member reads; team owners and admins switch |
| `GET /ui/plugins/{publisher}/{name}/panels/{slot}` | Team members, for a team that enabled the plugin, loaded by a frame of the page |
| `/ui/ext/{publisher}/{name}/...` | Team members, for a team that enabled the plugin; writes need the CSRF token |

`COOLIFY_RS_PLUGINS=off` switches the plugin system off entirely. Plugin data lives in the
`coolify_rs_plugins` PostgreSQL schema, which Coolify never reads, so a database with plugins can still be
handed back to Coolify.
