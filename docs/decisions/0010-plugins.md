# ADR 0010: Plugins as WebAssembly components, distributed through a signed registry

Status: proposed (2026-11-06)

## Context

People want to extend coolify-rs: a notification channel Coolify lacks, a dashboard of deploy statistics,
an integration with their own tooling. coolify-rs is behaviour-identical to Coolify, and that is checked
continuously, so any extension mechanism has to leave that property intact.

A plugin also runs next to the most sensitive data a self-hosted platform has. coolify-rs holds SSH keys
with root on every managed server, every application secret, and the `APP_KEY` that decrypts them all. A
plugin that can reach any of that is as dangerous as a compromised server. The design therefore starts
from security, and from what a reviewer can actually check.

## Decision

### 1. The parity rule

With no plugin installed, coolify-rs behaves exactly as it does without the plugin system. With plugins
installed, everything Coolify defines stays identical: API routes and responses, database rows in Coolify's
tables, commands sent to servers, and screens. Plugins can only:

- **observe** events Coolify already announces;
- **add** API routes under `/api/v1/ext/<publisher>/<name>/...`, and later UI panels in defined slots;
- **keep** their own data in coolify-rs's own schema, never in Coolify's tables.

The core stays two-way: Coolify can take a coolify-rs database back and simply never sees the plugin
schema. Parity becomes one-way only for a later class of *provider* plugins, such as build packs, that
must write a value Coolify does not know into one of its tables. Those resources are marked
plugin-owned, and `coolify-rs check-portable` lists them before a database is handed back. Provider
plugins are not part of the first version.

### 2. Runtime: WebAssembly components on wasmtime

Plugins are [WebAssembly components](https://component-model.bytecodealliance.org/) that implement the
`coolify-rs:plugin` world in `crates/coolify-plugins/wit/`, and coolify-rs runs them with wasmtime. We
rejected the alternatives:

- Native libraries: Rust has no stable binary interface, they run with the server's full privileges, and
  a crash takes the server down.
- Compiled-in crates: every plugin would mean a fork and a rebuild.
- Separate processes: they need a process manager, can reach the network and filesystem by default, and
  give a reviewer nothing to check about what the code can do.

A component suits the threat model:

- **What it can call is in the binary.** A component lists its imports by interface name. coolify-rs
  refuses to load a component that imports a `coolify-rs:` interface its manifest did not request and
  the admin did not grant. A reviewer, the registry's CI and the server all read the same list from the
  same bytes.
- **Nothing is granted by default.** WASI interfaces are linked so ordinary standard libraries work, but
  they are backed by an empty context: no files or directories, no environment, no arguments, no sockets
  and no name resolution. Clocks and random numbers are allowed. Network access exists only through
  coolify-rs's own `http` interface, which enforces the allowlist.
- **Limits.**
  - Every call runs in a fresh instance with a memory cap (64 MiB by default) and a deadline enforced
    by epoch interruption (5 s for events, 10 s for requests).
  - A plugin cannot keep state in memory between calls, so one team's data cannot leak into another
    team's call.
  - A trap or timeout ends that one call. After 5 failures in a row the plugin is disabled, and the
    audit log and the instance admins hear about it.
- **Any language.** Rust, Go (TinyGo), JavaScript (ComponentizeJS) and Python (componentize-py) can all
  produce components. The first SDK is Rust.

### 3. Capabilities

A plugin's manifest (`plugin.toml`) declares what it needs. The admin sees exactly that list when
installing, and each capability corresponds to one interface in the WIT world:

| Capability | Interface | Grants | Risk |
|---|---|---|---|
| (always) | `log` | write to coolify-rs's log, tagged with the plugin id | low |
| `events` | export `on-event` | receive the listed event kinds, for teams that enabled the plugin | low |
| `storage` | `storage` | a key-value store of its own, per team, with a quota (10 MiB) | low |
| `routes` | export `handle-request` | API routes under its prefix; requests arrive authenticated, with the team and token abilities | low |
| `http` | `http` | HTTPS requests to the hosts listed in the manifest, and no others | medium |

Rules for `http`:
- Only `https`, only hosts named exactly in the manifest (no wildcards), at most 1 MiB in each
  direction, and a 10 s timeout.
- Redirects are not followed.
- Addresses that resolve to loopback, private, link-local or unique-local ranges are refused, so a
  plugin cannot reach the database, the Docker socket proxy, cloud metadata endpoints or the managed
  servers' private networks.

Capabilities that reach Coolify's data or servers come later, each behind its own interface and review
tier: reading the team's resources through the same team-scoped queries the API uses, triggering
deployments with the abilities of an API token the team grants, and running commands on servers. There
will never be raw SQL access, because it would bypass team scoping, which is the strongest guarantee
coolify-rs makes.

### 4. Who decides what runs

- **Installing** is for instance admins: owners and admins of the root team, as with Coolify's instance
  settings. Installing shows the full capability list, and the grant is recorded.
- **Enabling** is per team, by that team's owners or admins. A plugin receives events, storage and
  requests only for teams that enabled it.
- **Updating** never widens capabilities silently. A version that asks for more is installed but stays
  on the old version until an admin approves the difference.
- Every install, grant, enable, disable, update and automatic disable is written to the audit log.

### 5. Data

- Plugin state lives in a PostgreSQL schema of coolify-rs's own, `coolify_rs_plugins`, created on first
  use. It holds the installed plugins, versions, grants, per-team enablement and the plugins' key-value
  storage.
- Coolify's migrations only touch `public`, and coolify-rs's schema guard only reads `public`, so the
  plugin schema is invisible to both.
- Dropping the schema removes every trace of plugins.

### 6. Events

- **Where they come from.** Events are emitted where coolify-rs sends Coolify's notifications
  (`notifications::notify`). That is the one place where Coolify announces deployments, backups,
  scheduled tasks, server reachability, Docker cleanup, disk usage and restarts.
- **No change for upstream.** Emitting is additive: the notification is still delivered, throttled and
  rate-limited exactly as before.
- **Delivery.**
  - Plugins receive events whether or not the team has a notification channel enabled.
  - Events are delivered in a separate task, so a slow plugin never delays a deployment.
- **Payloads.** Each payload is JSON with a stable name (`deployment.succeeded`, `server.unreachable`, ...)
  and a `version`, so plugins don't break when Coolify's notification classes change.

### 7. Distribution: a signed registry, not an app store

There are three ways to distribute plugins:

1. **Raw GitHub URLs.** Anyone can publish, but nobody has reviewed what runs, a tag can be moved to
   different code, and nothing can revoke a plugin found to be malicious.
2. **A full app store.** Accounts, uploads, ratings and payments mean a service to run, moderate and
   secure. It also becomes the single most valuable target for an attack on every coolify-rs instance.
3. **A curated, signed index in a Git repository**, the way Homebrew, crates.io's index and the Terraform
   registry work.

We choose (3), with (1) only as a development mode. The in-app catalogue gives users the "store"
experience without a store backend; a website can be generated from the same index later. A full store
can be reconsidered if paid plugins ever matter.

**The registry.**
- **Where it lives.** The registry is the public repository `coolify-rs/plugins`. Each plugin has a file
  `plugins/<publisher>/<name>.toml` listing its versions. For each version, the file records:
  - the source repository and the exact commit;
  - the language toolchain;
  - the component's SHA-256 and where it is published;
  - the manifest, which includes the capabilities.
- **Publishers.** A publisher is the GitHub user or organization that owns the source repository. Ids are
  `publisher/name`, and `coolify-rs/` is reserved for first-party plugins.
- **Signing and verifying.**
  - After a change is merged, the registry's CI publishes `index.json` with an Ed25519 signature.
  - coolify-rs ships the registry's public keys, each with an expiry date so keys can be rotated.
  - It verifies the signature, then the component's hash, before installing anything.
- **Refreshing and revoking.**
  - Instances refresh the index once a day, as the update check does, and admins can turn this off.
  - The index can *yank* a version, so new installs avoid it, or *revoke* a plugin. A revoked plugin is
    disabled on every instance at the next refresh, and the instance admins are told why.

**Submitting and reviewing.** A publisher opens a pull request that adds or bumps a version. The
registry's CI then runs these checks, and every one must pass:

1. **Reproducible build.** It builds the component from the pinned commit in a pinned builder image and
   checks the hash matches the submitted one, so the reviewed source is exactly what users run.
2. **Capability check.**
   - It reads the component's imports and fails if they go beyond the manifest.
   - It prints what changed compared with the previous version: new capabilities, new HTTP hosts and
     new events.
3. **Conformance.**
   - `coolify-rs plugin check` loads the component in the real host and sends it every event kind it
     subscribes to, plus malformed input.
   - It checks that the component stays within the memory and time limits.
4. **Metadata.**
   - The license must be OSI-approved.
   - The id must be new or owned by the same publisher, and must not look like an existing id
     (typosquatting).
   - The description must not claim affiliation with coolify-rs or Coolify.

Human review is tiered by risk:

| Change | Review |
|---|---|
| A new plugin | One maintainer reads the source and confirms the capabilities match the stated purpose |
| A new version, same capabilities | One maintainer checks the source diff since the approved version |
| A new version with more capabilities, or any `http` host added | Two maintainers |
| Later high-risk capabilities (server commands, deployments, resource data) | Two maintainers, and a security review of every code path that uses them |

**Development mode.**
- `COOLIFY_RS_ALLOW_UNVERIFIED_PLUGINS=true` turns it on; it is off by default. (It becomes an instance
  setting with the UI.)
- With it on, admins can upload a component (installing from a URL comes with the registry). The
  component is pinned by its hash, shown everywhere as **Unverified**, and goes through the same
  capability approval.
- This is for plugin authors and private in-house plugins.

**What phase 2 settled.** These points were left open above and are now decided:

- **Signature file.**
  - `index.json.sig` is a small JSON document, `{algorithm, key_id, signature}`. The signature covers
    the exact bytes of `index.json`.
  - Each key has a `not_after` date. The key is valid through that day (UTC) and refused after it, as is
    any key coolify-rs doesn't know.
  - The production key is added by the registry maintainers. `coolify-rs-dev-insecure`, whose secret
    half is committed for tests, is never trusted by default.
- **Rollback.** coolify-rs refuses an index whose `generated_at` is older than the one it already holds
  from the same URL. Otherwise an old signed index could bring back revoked plugins and yanked versions.
- **Forward compatibility.** A version whose manifest this coolify-rs can't run (a newer interface or an
  unknown capability) doesn't invalidate the index. It is listed as incompatible and never installed,
  so older instances still receive revocations.
- **Pending updates.**
  - "Installed but stays on the old version" works like this: the version is recorded in
    `coolify_rs_plugins.pending_updates` with its hash and manifest, and the old version keeps running.
  - On approval, the component is downloaded and checked against the index again, then installed.
  - Approving means sending the new capability list as `grant`, as for a first install.
- **Revocation.** A revoked plugin is switched off with the registry's reason as `disabled_reason`, and
  the switch is audited. While the plugin stays revoked, it can't be switched back on, reinstalled,
  uploaded or updated.
- **Downloads.** The index and the components are fetched under the `http` capability's rules, with the
  component size limit.
- **Turning refresh off.** `COOLIFY_RS_PLUGIN_REGISTRY=off` stops the daily refresh.
  `COOLIFY_RS_PLUGIN_REGISTRY_URL` points at another registry. Extra keys
  (`COOLIFY_RS_PLUGIN_REGISTRY_KEYS`) are trusted only in development mode, which is how a private
  registry works.
- **Review rules.**
  - Published versions are immutable apart from `yanked`.
  - Plugins and versions are never removed.
  - Lifting a revocation takes two maintainers.
  - The publisher name `catalog` is reserved, because it would collide with `/api/v1/plugins/catalog/...`.

### 8. Keeping parity checked

- **Plugins off.** The existing comparison suites run with no plugin installed.
- **Canary plugin.** A canary plugin subscribes to every event, writes to storage and serves routes. The
  same suites run again with it enabled for every team, and must match Coolify byte for byte on
  everything Coolify defines.
- **Round trip.** A test drops `coolify_rs_plugins` and checks that Coolify's own image accepts the
  database.
- **Upstream changes.** `compat/mapping/registry.toml` records the event sources, so a change upstream to
  a notification flags the event it feeds.

## Phases

1. **Host and events** (this change):
   - the `coolify-plugins` crate with the WIT world, wasmtime runtime, limits and capability checks;
   - the plugin schema;
   - events from `notify`;
   - storage, `log`, `http` with its allowlist, and plugin routes;
   - the admin API: install an uploaded component in development mode, list, grant, enable per team,
     disable, uninstall;
   - a Rust SDK and an example plugin;
   - tests for parity and for hostile plugins.
2. **Registry**: the `coolify-rs/plugins` repository and its CI, the signed index, the client in
   coolify-rs, the in-app catalogue, yanking and revocation.
3. **UI slots**: panels on the dashboard, resource pages and settings, rendered in sandboxed iframes that
   talk to the plugin's own routes.
4. **Higher-risk capabilities and provider plugins**, each with its own ADR.

## Consequences

- wasmtime adds about 15 MB to the binary and some build time. Memory is only used while a plugin is
  loaded.
- The plugin interface has its own version (`coolify-rs:plugin@0.1.0`), separate from Coolify's. Breaking
  changes need a new major version, and coolify-rs keeps the previous one working for a release.
- The registry needs maintainers who review. Until it has them, only first-party plugins and
  development-mode plugins exist.
