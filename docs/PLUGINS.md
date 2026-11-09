# Writing coolify-rs plugins

A plugin is a WebAssembly component that coolify-rs runs in a sandbox. It can react to events, keep its
own data and serve its own API routes, and it can do only what its manifest asks for and an admin grants.
[ADR 0010](decisions/0010-plugins.md) explains the design and the security model.

The interface is [`crates/coolify-plugins/wit/plugin.wit`](../crates/coolify-plugins/wit/plugin.wit)
(`coolify-rs:plugin@0.1.2`; components built for 0.1.0 and 0.1.1 load unchanged). Any language that can build a WebAssembly component can implement it. This
guide uses Rust and the SDK in [`plugins/sdk-rust`](../plugins/sdk-rust).

## First-party plugins

These are maintained in this repository under [`plugins/official`](../plugins/official) and listed in the
registry under [`plugins/coolify-rs`](https://github.com/coolify-rs/plugins/tree/main/plugins/coolify-rs). Each README lists
the settings, exactly what is sent where, and the capabilities.

| Plugin | What it does | Capabilities beyond events, storage and routes |
|---|---|---|
| [`coolify-rs/alerts`](../plugins/official/alerts) | Sends failed deployments, backups and scheduled tasks, unreachable servers, restart limits and expiring certificates to ntfy, Google Chat, PagerDuty or Opsgenie. PagerDuty and Opsgenie incidents resolve when the resource recovers. | `http` (the five service hosts), dashboard panel, encrypted settings for keys |
| [`coolify-rs/healthchecks`](../plugins/official/healthchecks) | Pings Healthchecks.io for every scheduled task and database or volume backup, so a job that silently stops running raises an alarm. | `http` (hc-ping.com), dashboard panel, encrypted ping key |
| [`coolify-rs/deploy-chains`](../plugins/official/deploy-chains) | After an application deploys successfully, deploys the applications that depend on it. Refuses loops and never runs for previews. | `resources: applications`, `deploy` (needs the team's consent), application and dashboard panels |
| [`coolify-rs/delivery-metrics`](../plugins/official/delivery-metrics) | The four DORA metrics per team and application, with bands, and CSV exports of metrics, deployments and the team's resources. | `resources` (all kinds, read only), dashboard and application panels |

The examples in [`plugins/examples`](../plugins/examples) are smaller and meant for reading: `deploy-stats`
(events and a route), `deploy-board` (panels), `inventory` (reading resources).

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
resources = ["applications"]      # read-only; ADR 0011
deploy = true                     # each team must also grant it; ADR 0011
```

Ask for the least you need. The admin sees the list, reviewers check every entry, and coolify-rs refuses to
load a component that imports an interface its manifest did not ask for.

| Capability | Gives the plugin |
|---|---|
| `events` | Calls to `on-event` for the listed kinds, for teams that enabled the plugin |
| `storage` | A key-value store per team. Keys are up to 256 bytes and values up to 1 MiB, with 10 MiB per team |
| `routes` | Calls to `handle-request` for requests to `/api/v1/ext/<publisher>/<name>/...` |
| `http` | HTTPS requests to the listed hosts and no others |
| `resources` | Read the call's team's resources of the listed kinds (`projects`, `servers`, `applications`, `services`, `databases`), as the REST API shows them to a token with `read` and without `read:sensitive`, so no secrets |
| `deploy` | Queue deployments of the call's team's applications through the same code path as `POST /api/v1/deploy`. Each team must also grant it when enabling the plugin, and a team's plugins together may queue at most 20 deployments in 10 minutes |
| `ui` | Panels in the web interface, each a `GET` route of the plugin (needs `routes`; see [UI panels](#ui-panels)) |

[ADR 0011](decisions/0011-plugin-resource-and-deploy-capabilities.md) explains `resources` and `deploy`.
Plugins cannot run commands on servers.

A manifest can also declare `[[settings]]` that each team fills in, such as an API key or a webhook URL. They
are not a capability; see [Team settings](#team-settings).

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
  - A request ends before the call's own deadline, so a slow host gives the plugin an ordinary error to
    handle instead of timing out the whole call.
- **Limited view of the caller.** A route never sees the caller's API token or cookies.
- **Restricted responses.**
  - A route's response can set only `content-type`, `cache-control`, `etag` and `last-modified`.
  - JSON, plain text and CSV are served as they are. Any other content type becomes a download, so a
    plugin cannot serve pages that run in the browser.

## Reading resources and deploying

[`plugins/examples/inventory`](../plugins/examples/inventory) lists the caller's team's applications:

```rust
use coolify_rs_plugin::{Caller, Plugin, Request, Response, resources};

struct Inventory;

impl Plugin for Inventory {
    fn handle_request(_caller: Caller, _request: Request) -> Response {
        match resources::applications() {
            Ok(apps) => Response::json(200, &serde_json::Value::Array(apps)),
            Err(e) => Response::json(502, &serde_json::json!({ "message": e })),
        }
    }
}

coolify_rs_plugin::export!(Inventory);
```

- **Always the call's team.** No function takes a team. coolify-rs answers for the team of the event or of
  the token that called the route, so a plugin cannot reach another team's resources even when it knows
  their uuids: they are simply not found.
- **The API's answers.** `resources::list(kind)` and `resources::find(kind, uuid)` return what
  `GET /api/v1/<kind>` and `GET /api/v1/<kind>/<uuid>` return to a `read` token of the team, byte for byte,
  and `resources::environments(project_uuid)` what `GET /api/v1/projects/<uuid>/environments` returns.
  Passwords, private keys, webhook secrets and the other fields that need `read:sensitive` are never there.
- **Deploying.** `deploy::queue(uuid, force)` queues a deployment of one of the team's applications and
  returns the API's answer (`{"deployments": [...]}`). From a route, it runs with the caller's token: if
  the caller could not deploy the application through the API, neither can the plugin. From an event, it
  runs on the authority of the team admin who granted `deploy`, checked again on every deployment. Every
  deployment, refused or not, is written to the plugin audit log with the plugin's id.

## Team settings

A plugin that needs values per team (a topic, an API key, a webhook URL) declares them in the manifest. Team
owners and admins fill them in under **Team → Plugins → Configure**, and the plugin reads them with
`config::get`. [ADR 0014](decisions/0014-plugin-settings.md) explains the design and the threat model.

```toml
[[settings]]
key = "topic"                     # lowercase snake case, unique; what the plugin asks for
label = "Topic"                   # the form field's label, up to 64 characters
help = "The ntfy topic alerts are published to."   # optional, up to 280 characters
required = true                   # "Needs configuration" until set

[[settings]]
key = "token"
label = "Access token"
secret = true                     # encrypted at rest, never shown again after saving

[[settings]]
key = "server"
label = "Server"
kind = "url"                      # text (the default), url (https only) or select
default = "https://ntfy.sh"       # answered while the team has not set one

[[settings]]
key = "priority"
label = "Priority"
options = ["low", "default", "high"]   # a select; kind = "select" is implied
default = "default"
```

| Field | |
|---|---|
| `key` | Required. `a-z`, `0-9` and `_`, starting with a letter, at most 64 characters, unique in the manifest |
| `label` | Required. 1 to 64 characters |
| `help` | Optional. 1 to 280 characters |
| `secret` | `false` by default. A secret can't have a `default` or `options` |
| `required` | `false` by default. A required setting can't have a `default` |
| `default` | Optional, must be a valid value |
| `options` | Optional. Up to 32 values, shown as a select |
| `kind` | `text`, `url` (an `https://` URL with a host and no user or password) or `select` (needs `options`) |

At most 32 settings. Values are trimmed, up to 4096 bytes, without line breaks or control characters.

Settings are not a capability: they don't appear in the capability list an admin approves, and adding them in
an update needs no new approval. A component may import the `config` interface only if its manifest declares
settings; otherwise coolify-rs refuses to install or load it.

```rust
use coolify_rs_plugin::{Event, Plugin, config};

struct Ntfy;

impl Plugin for Ntfy {
    fn on_event(event: Event) -> Result<(), String> {
        let Some(topic) = config::get("topic") else {
            coolify_rs_plugin::log::warn("not configured for this team");
            return Ok(());
        };
        let server = config::get_or("server", "https://ntfy.sh");
        let mut headers: Vec<(String, String)> = vec![];
        if let Some(token) = config::get("token") {
            headers.push(("authorization".into(), format!("Bearer {token}")));
        }
        // ... coolify_rs_plugin::http::fetch(...) to `{server}/{topic}` with `headers`
        Ok(())
    }
}
```

- `config::get(key)` returns the team's value, else the default, else `None`. Required settings can be unset
  too, so handle `None`. A key the manifest doesn't declare is always `None`. `config::get_or(key, default)`
  and `config::require(key)` (an error for `?`) are shortcuts.
- **Always the call's team and your own plugin.** As with `storage`, no function takes a team or a plugin.
- **Secrets come back in clear, to you only.** Never log them or put them in a route's response. Members,
  admins and other plugins never see them.
- **Where they live.** `coolify_rs_plugins.team_settings`. A secret is encrypted with `APP_KEY`, the key
  Coolify encrypts its own secrets with. Without `APP_KEY`, secrets can't be saved.
- **Switching off and uninstalling.** A team that switches the plugin off keeps its settings. Uninstalling the
  plugin deletes every team's settings.
- **Updates.** When a new version removes a setting, its values are deleted. When a setting becomes secret,
  stored values are encrypted. When a secret stops being secret, its values are deleted, because members would
  see them. Values the new declaration refuses (another kind or other options) are deleted.
- **Checking.** `coolify-rs plugin check` runs with defaults only, so don't crash when a setting is unset.

The API (`GET` and `PATCH /api/v1/plugins/{publisher}/{name}/settings`) is under [API](#team-settings-api).

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

## Managing plugins in the web interface

Instance admins manage plugins under **Settings → Plugins**, and each team switches them on under
**Team → Plugins**. The menus only show these entries when there is something to manage: Settings for instance admins while
plugins are on (the registry catalogue is always there), and Team when a plugin is switched
on for the instance.

- **Plugin registry.** The catalogue with Refresh, Install, Update and the approval of updates that ask for
  more. Install and update show exactly the capabilities that will be granted, and what an update adds.
- **Install from files.** In development mode only: choose `plugin.toml` and the `.wasm`, review the
  capabilities, and approve. The plugin is shown as **Unverified**.
- **Team → Plugins.** For a plugin holding `deploy`, switching it on asks whether it may queue the team's
  deployments; the page shows the answer and can change it. A plugin with settings has **Configure** (members
  see the settings read-only, without secrets) and a **Needs configuration** badge while a required setting
  is unset. A secret's field shows "••••• (set)": leave it empty to keep the secret, or Clear it.

## Checking a plugin

`coolify-rs plugin check` runs the same conformance check as the registry's CI, and it needs no database:

```bash
coolify-rs plugin check target/wasm32-wasip2/release/deploy_stats.wasm plugin.toml
```

The check runs these steps:

1. It parses the manifest and compiles the component.
2. It refuses imports the manifest did not ask for.
3. It loads the component in the real sandbox, with the same limits as an installed plugin. Storage is
   kept in memory. Calls to `http`, `resources` and `deploy` fail.
4. It sends every event kind the plugin subscribes to with a sample payload, then malformed input: a
   payload that isn't JSON, wrong types, a 256 KiB string, an unknown kind or version. If the plugin has
   routes, it also sends odd requests.

It prints a JSON report with each call's result, time and peak memory, and exits non-zero on failure.
A trap, a timeout or an invalid status fails the check. A plugin that returns an error for input it can't
use passes, and the error is noted.

## Installing from the catalogue

The [plugin registry](decisions/0010-plugins.md#7-distribution-a-signed-registry-not-an-app-store) is a
reviewed Git repository, `coolify-rs/plugins`. Its CI publishes `index.json` with an Ed25519 signature.

- **Fetching the index.** coolify-rs fetches the index a minute after it starts, then once a day. An admin
  can also refresh it.
- **Checking the index.** coolify-rs checks the signature against the registry keys compiled into it.
  Signatures by an unknown or expired key are refused, and so is an index older than the one it already
  has.
- **Checking downloads.** Every download goes through the plugins' HTTP rules: HTTPS, public addresses
  only, no redirects, at most 20 MiB. A component is installed only if its SHA-256 matches the index.
  Plugins installed this way show `origin: "registry"` and `verified: true`.

With an API token of an owner or admin of the root team:

```bash
# Fetch the index now, then look at the catalogue.
curl -X POST https://coolify.example.com/api/v1/plugins/catalog/refresh -H "Authorization: Bearer $TOKEN"
curl https://coolify.example.com/api/v1/plugins/catalog -H "Authorization: Bearer $TOKEN"

# The first install request answers 422 with the capability list. Send it back as grant to install.
curl -X POST https://coolify.example.com/api/v1/plugins/catalog/install -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"id": "coolify-rs/deploy-stats", "grant": ["events: deployment.succeeded, deployment.failed", "storage", "routes"]}'
```

Each team then turns the plugin on for itself with `POST /api/v1/plugins/{publisher}/{name}/enable`.

### Updates

An update never widens a plugin's capabilities without an admin's approval.

- **Same or fewer capabilities.** `POST /api/v1/plugins/{publisher}/{name}/update` installs the newest
  version directly, or the `version` you name. Per-team enablement and the plugin's stored data are kept.
- **More capabilities.** The refresh records the newer version as a *pending update*, and the catalogue
  shows what it adds.
  - An update request without a grant answers 422 with the new capability list and what it adds.
  - Sending that list back as `grant` approves and installs the version.
  - Until then the installed version keeps running.

### Yanked and revoked plugins

- **Yanked versions.** New installs and updates skip a yanked version, and naming one explicitly is
  refused. Installations already on that version keep running, and the catalogue flags them.
- **Revoked plugins.** At the next refresh, a revoked plugin is switched off on the instance.
  - `disabled_reason` reads `revoked by the plugin registry: <reason>`, and the audit log records it.
  - While the plugin stays revoked, it cannot be switched back on, installed, uploaded or updated.
  - If the registry lifts the revocation, an admin can switch the plugin on again.

## Installing a plugin outside the registry

For plugin authors and private in-house plugins, start coolify-rs with
`COOLIFY_RS_ALLOW_UNVERIFIED_PLUGINS=true` and upload the component directly. Then, with an API token of an
owner or admin of the root team:

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

Uploaded plugins show `origin: "upload"` and `verified: false`.

Each team then turns it on for itself:

```bash
curl -X POST https://coolify.example.com/api/v1/plugins/acme/deploy-stats/enable -H "Authorization: Bearer $TOKEN"
```

A plugin holding `deploy` also needs the team's own answer, because the instance admin's approval does not
speak for the team: `{"grant_deploy": true}` lets it queue the team's deployments, `{"grant_deploy": false}`
enables it without them. Without either, enabling fails with the plugin's capabilities. Disabling forgets
the answer, and so does an update that drops `deploy`.

```bash
curl -X POST https://coolify.example.com/api/v1/plugins/acme/auto-redeploy/enable -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" -d '{"grant_deploy": true}'
```

## API

| Endpoint | Who can call it |
|---|---|
| `GET /api/v1/plugins`, `GET /api/v1/plugins/{publisher}/{name}` | Any token with `read` |
| `GET /api/v1/plugins/catalog` | Any token with `read` |
| `POST /api/v1/plugins/catalog/refresh` | Owners and admins of the root team, with `write` |
| `POST /api/v1/plugins/catalog/install` with `{"id": ..., "version": ..., "grant": [...]}` | Owners and admins of the root team, with `write` |
| `POST /api/v1/plugins/{publisher}/{name}/update` with `{"version": ..., "grant": [...]}` (both optional) | Owners and admins of the root team, with `write` |
| `POST /api/v1/plugins` | Owners and admins of the root team, with `write`, in development mode only |
| `PATCH /api/v1/plugins/{publisher}/{name}` with `{"enabled": false}` | Owners and admins of the root team: switches the plugin off for everyone |
| `DELETE /api/v1/plugins/{publisher}/{name}` | Owners and admins of the root team: uninstalls the plugin and deletes its data |
| `POST /api/v1/plugins/{publisher}/{name}/enable`, `/disable` | Team owners and admins, for their own team; `enable` takes `{"grant_deploy": true\|false}` for a plugin holding `deploy` |
| `POST /api/v1/plugins/{publisher}/{name}/test-event` with `{"kind": ..., "payload": {...}}` | Team owners and admins, in development mode only: delivers one event to the plugin for their team and reports whether it succeeded |
| `GET /api/v1/plugins/{publisher}/{name}/settings` | Any token with `read`: the token's team's settings, secrets only as `set` |
| `PATCH /api/v1/plugins/{publisher}/{name}/settings` | Team owners and admins, with `write`, for their own team |
| `/api/v1/ext/{publisher}/{name}/...` | Any token with `read`, for a team that enabled the plugin |

### Team settings API

`GET /api/v1/plugins/{publisher}/{name}/settings` answers every declared setting for the token's team. A
setting that is not secret has `value` (`null` while unset); a secret only says whether it is `set`.

```json
{
  "plugin": "acme/ntfy-alerts",
  "settings": [
    {"key": "topic", "label": "Topic", "help": "The ntfy topic alerts are published to.", "kind": "text",
     "secret": false, "required": true, "options": [], "default": null, "set": true, "value": "deploys"},
    {"key": "token", "label": "Access token", "help": null, "kind": "text",
     "secret": true, "required": false, "options": [], "default": null, "set": true}
  ],
  "needs_configuration": false
}
```

`PATCH` the same path with an object of the settings to change: a string sets one, `null` or a blank string
clears it, and settings left out stay as they are. It answers the same shape. Nothing is written unless every
value is valid; otherwise it answers 422 with the errors by key, which never repeat a value:

```bash
curl -X PATCH https://coolify.example.com/api/v1/plugins/acme/ntfy-alerts/settings \
  -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
  -d '{"topic": "deploys", "token": "tk_..."}'
```

```json
{"message": "Validation failed.", "errors": {"server": ["Server must be an https:// URL."]}}
```

Every change is written to the plugin audit log as `team_settings_updated`, with the user and
`{"set": [keys], "cleared": [keys]}`, never values. 403 for members and tokens without `write`, 404 for a
plugin that is not installed.

### An installed plugin

`GET /api/v1/plugins/{publisher}/{name}` returns this shape. `POST .../catalog/install` (201) and
`POST .../update` (200) return it too.

```json
{
  "id": "coolify-rs/deploy-stats",
  "name": "Deploy stats",
  "version": "0.1.0",
  "description": "Counts successful and failed deployments per team and serves the totals.",
  "license": "Apache-2.0",
  "source": "https://github.com/coolify-rs/coolify-rs/tree/main/plugins/examples/deploy-stats",
  "sha256": "5296…",
  "origin": "registry",
  "verified": true,
  "enabled": true,
  "disabled_reason": null,
  "capabilities": ["events: deployment.succeeded, deployment.failed", "storage", "routes"],
  "enabled_for_team": false
}
```

### `GET /api/v1/plugins/catalog`

`index` is `null` and `plugins` is empty until the first refresh succeeds.

```json
{
  "index": {
    "url": "https://raw.githubusercontent.com/coolify-rs/plugins/index/dist",
    "key_id": "coolify-rs-2026",
    "generated_at": "2026-11-06T12:00:00Z",
    "fetched_at": "2026-11-06T12:01:00Z"
  },
  "plugins": [
    {
      "id": "coolify-rs/deploy-stats",
      "name": "Deploy stats",
      "description": "Counts successful and failed deployments per team and serves the totals.",
      "license": "Apache-2.0",
      "source": "https://github.com/coolify-rs/coolify-rs",
      "latest_version": "0.2.0",
      "capabilities": ["events: deployment.succeeded, deployment.failed", "storage", "routes"],
      "verified": true,
      "installed_version": "0.1.0",
      "installed_yanked": false,
      "update_available": true,
      "update_requires_approval": true,
      "pending_update": {
        "version": "0.2.0",
        "capabilities": ["events: deployment.succeeded, deployment.failed", "storage", "routes"],
        "added": ["events: deployment.failed"],
        "found_at": "2026-11-06T12:01:00Z"
      },
      "revoked": null,
      "versions": [
        {"version": "0.2.0", "yanked": false, "compatible": true, "capabilities": ["…"]},
        {"version": "0.1.0", "yanked": false, "compatible": true, "capabilities": ["…"]}
      ]
    }
  ]
}
```

These fields need explaining:

- `latest_version` is the newest version that isn't yanked and that this coolify-rs can run. Pre-releases
  count only when there is nothing else. It is `null` when no version qualifies.
- `capabilities` are those of `latest_version`.
- `update_available` is true when `latest_version` is newer than the installed version.
- `update_requires_approval` is true when that update asks for capabilities that weren't granted.
- `revoked` is `null` or `{"reason": "..."}`.
- `versions` lists the newest first. `compatible: false` marks a version that needs a newer coolify-rs.

### `POST /api/v1/plugins/catalog/refresh`

```json
{
  "url": "https://raw.githubusercontent.com/coolify-rs/plugins/index/dist",
  "key_id": "coolify-rs-2026",
  "generated_at": "2026-11-06T12:00:00Z",
  "fetched_at": "2026-11-06T12:01:00Z",
  "plugins": 12,
  "revoked": ["acme/bad-plugin"],
  "pending_updates": ["coolify-rs/deploy-stats"]
}
```

- `revoked` lists the installed plugins this refresh switched off.
- `pending_updates` lists the installed plugins for which this refresh found a newer version that asks
  for more.
- A download, signature or rollback failure answers 502 `{"message": ...}`, and the previous index
  stays in use.

### Approving capabilities

A missing or different `grant` on install, or an update that needs approval, answers 422 like this:

```json
{
  "message": "Approve the plugin's capabilities by sending them back as grant.",
  "id": "coolify-rs/deploy-stats",
  "version": "0.2.0",
  "capabilities": ["events: deployment.succeeded, deployment.failed", "storage", "routes"],
  "added": ["events: deployment.failed"],
  "pending_update": true
}
```

- On install, `added` lists every capability.
- On update, `added` lists only what the version asks for beyond the current grant.
- `pending_update` says whether the version is now recorded as a pending update.

Other errors answer `{"message": ...}`:

| Status | When |
|---|---|
| 404 | The plugin, or the version, isn't in the index. Updating a plugin that isn't installed |
| 409 | The index has never been fetched. The plugin is already installed, or already at that version |
| 422 | The version is yanked, can't run on this coolify-rs, or the plugin is revoked. For a revoked plugin the body also has `id` and `reason` |
| 502 | Fetching failed, the signature didn't verify, or the component's SHA-256 doesn't match the index |

### Web interface endpoints

The web interface uses session endpoints with the same rules (`crates/coolify-api/src/ui_plugins.rs`): instance
admins are owners and admins of the root team, whichever team they are working in. The catalogue endpoints call
the same code as the API's and answer the same bodies and errors.

| Endpoint | Who can call it |
|---|---|
| `GET /ui/plugins/nav` | Any signed-in user: the menu entries and the panels of their team |
| `GET /ui/plugins`, `POST /ui/plugins`, `PATCH` and `DELETE /ui/plugins/{publisher}/{name}` | Instance admins; installing from files needs development mode, uninstalling the password step |
| `GET /ui/plugins/catalog`, `POST /ui/plugins/catalog/refresh`, `POST /ui/plugins/catalog/install`, `POST /ui/plugins/{publisher}/{name}/update` | Instance admins |
| `GET /ui/plugins/team`, `POST /ui/plugins/{publisher}/{name}/team` | Any team member reads; team owners and admins switch with `{"enabled": ...}`. Enabling a plugin that holds `deploy` also needs `"grant_deploy": true` or `false`, as `/enable` does |
| `GET /ui/plugins/{publisher}/{name}/settings`, `PATCH` the same | Any team member reads, team owners and admins change; the same bodies as the API's `.../settings`. Each plugin of `GET /ui/plugins/team` also carries `settings` and `needs_configuration` |
| `GET /ui/plugins/{publisher}/{name}/panels/{slot}` | Team members, for a team that enabled the plugin, loaded by a frame of the page |
| `/ui/ext/{publisher}/{name}/...` | Team members, for a team that enabled the plugin; writes need the CSRF token |

## Command line

None of these need a database.

| Command | Does |
|---|---|
| `coolify-rs plugin check <component.wasm> <plugin.toml>` | The conformance check described above |
| `coolify-rs plugin keygen <secret-out> <public-out> --key-id <id> --not-after <YYYY-MM-DD>` | Creates a registry signing key. It writes the secret half with mode 0600, never overwrites a file, and prints only the public half |
| `coolify-rs plugin build-index <registry-dir> --key <secret> [--out <dir>]` | Builds and signs `index.json` from `plugins/*/*.toml` |
| `coolify-rs plugin verify-index <dist-dir> [--trust <public-key-file>]` | Verifies an index with the compiled-in keys, and any key you trust explicitly |
| `coolify-rs plugin review <registry-dir> [--base <main-checkout>] [--out <dir>]` | The registry's pull request checks: metadata, typosquatting, immutability, capability diff and review tier |

The registry repository, with its workflows and scripts, is [`coolify-rs/plugins`](https://github.com/coolify-rs/plugins).

## Settings

| Variable | Default | Effect |
|---|---|---|
| `COOLIFY_RS_PLUGINS` | on | `off` switches the plugin system off entirely |
| `COOLIFY_RS_ALLOW_UNVERIFIED_PLUGINS` | `false` | `true` turns on development mode: uploads, test events, extra registry keys |
| `COOLIFY_RS_PLUGIN_REGISTRY` | on | `off` stops the daily refresh. An admin can still refresh by hand. Without refreshes, revocations don't arrive |
| `COOLIFY_RS_PLUGIN_REGISTRY_URL` | `https://raw.githubusercontent.com/coolify-rs/plugins/index/dist` | The folder that holds `index.json` and `index.json.sig` |
| `COOLIFY_RS_PLUGIN_REGISTRY_KEYS` | none | Extra trusted keys, as `id:hex-public-key:YYYY-MM-DD` separated by commas. They are read in development mode only and ignored otherwise |

The development key, `coolify-rs-dev-insecure`, signs the test registries. Its secret half is in this
repository, so coolify-rs never trusts it by default. To try a local registry in development mode, pass
`signing::DEV_KEY_ENV` in `COOLIFY_RS_PLUGIN_REGISTRY_KEYS`.

Plugin data lives in the `coolify_rs_plugins` PostgreSQL schema, which Coolify never reads. That includes
the last verified index, revocations and pending updates, so a database with plugins can still be handed
back to Coolify.

`coolify-rs check-portable` (with `--json` for scripts) confirms it: it exits 0 when
Coolify can take the database back, and 1 with the rows to fix when it cannot
([ADR 0012](decisions/0012-provider-plugins-and-portability.md)).
