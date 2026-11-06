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

## Installing a plugin

Until the [registry](decisions/0010-plugins.md#7-distribution-a-signed-registry-not-an-app-store) exists,
plugins are installed directly. Start coolify-rs with `COOLIFY_RS_ALLOW_UNVERIFIED_PLUGINS=true`. Then,
with an API token of an owner or admin of the root team:

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
| `/api/v1/ext/{publisher}/{name}/...` | Any token with `read`, for a team that enabled the plugin |

`COOLIFY_RS_PLUGINS=off` switches the plugin system off entirely. Plugin data lives in the
`coolify_rs_plugins` PostgreSQL schema, which Coolify never reads, so a database with plugins can still be
handed back to Coolify.
