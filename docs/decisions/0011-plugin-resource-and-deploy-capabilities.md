# ADR 0011: Plugin capabilities `resources` and `deploy`

Status: proposed (2026-11-06)

## Context

[ADR 0010](0010-plugins.md) gave plugins events, their own storage, their own routes and allowlisted HTTPS,
and deferred everything that reaches Coolify's data or servers: "each behind its own interface and review
tier". The plugins people ask for next need two of those things: a view of the team's resources (an
inventory, a status board, a cost report) and a way to trigger deployments (redeploy when an upstream
image changes, deploy a chain of applications in order).

A third, running commands on servers, was asked for too. It is not part of this decision; section 6 says
why.

## Decision

Two new capabilities, each one WIT interface, added to the plugin interface as `coolify-rs:plugin@0.1.1`.

### 1. What every call already guarantees

- **The team comes from the call, never from the plugin.** An event carries the team it belongs to, and a
  route call the team of the authenticated token. No function of `resources` or `deploy` takes a team. The
  host passes the call's team to the API code itself.
- **Grant, then load.** A component importing `coolify-rs:plugin/resources` or `/deploy` without the
  capability is refused at install and at start, as for every other interface.
- **Fresh instance per call, limits and deadlines** as in ADR 0010.

### 2. `resources`: read the team's resources

Manifest:

```toml
[capabilities]
resources = ["applications", "servers"]
```

The value is a list of kinds, not `true`. The kinds are `projects` (which includes their environments),
`servers`, `applications`, `services` and `databases`. A list keeps the grant as small as the purpose:
an application inventory has no reason to see the servers' addresses, and a reviewer and an admin see
exactly which kinds a plugin reads. The capability summary an admin approves reads
`resources: applications, servers`; a new version asking for another kind is a capability escalation
under ADR 0010's update rule and review tiers.

The interface:

| Function | Answers like |
|---|---|
| `all(kind)` | `GET /api/v1/<kind>` |
| `get(kind, uuid)` | `GET /api/v1/<kind>/<uuid>`, or none |
| `environments(project-uuid)` | `GET /api/v1/projects/<uuid>/environments`, or none; needs `projects` |

What a plugin gets is the API's own answer, byte for byte, for a token of the call's team with `read`
and nothing else: a member's role, no `read:sensitive`. coolify-rs does this by running the API's
handlers in-process with that context (`crates/coolify-api/src/plugin_bridge.rs`), not by writing new
queries or serializers. So:

- team scoping is the API's: the same `WHERE team_id = ...` queries, and a uuid of another team is simply
  not found;
- redaction is the API's: every field that needs `read:sensitive` (database passwords and connection
  URLs, applications' basic auth password, webhook secrets, Dockerfiles and Compose files, services'
  Compose files, the servers' proxy secrets and sensitive settings) is absent, and stays absent when the
  serializers change, because they are the same serializers;
- the shape is the public API's, which plugin authors can read in Coolify's documentation.

Not exposed: environment variables, deployments and their logs, teams and members, API tokens, private
keys, S3 storages, notification settings, cloud provider tokens, and anything else not in the table.
Each of those would need its own decision.

Answers over 16 MiB are refused. A uuid that is not a plausible Coolify uuid (letters, digits, `-`, `_`,
at most 64 characters) is not found without a query. The API's "API enabled" switch and IP allowlist
guard HTTP clients and do not apply to plugin calls; a plugin's own routes stay behind them.

### 3. `deploy`: queue deployments of the team's applications

Manifest:

```toml
[capabilities]
deploy = true
```

The interface has one function, `application(uuid, force)`. It queues a deployment of one application of
the call's team through the same code path as `POST /api/v1/deploy` with `{"uuid": ..., "force": ...}`:
the same lookup, the same role check, the same queue limit and duplicate-commit check, `is_api` set on the
queued row. It returns the API's JSON answer, or the reason it was refused. The uuid must be an
application of the call's team; the API endpoint would also start services and databases, and accept a
comma-separated list, and the plugin function does neither.

**On whose authority.** A deployment always runs as someone the team trusts to deploy:

- From a route call, as the **caller**: their user, role and token abilities. If the caller's token could
  not deploy the application through the API (no `deploy` or `root` ability, or not a team admin), the
  plugin cannot either. A plugin route can never be used to deploy what the caller could not.
- From an event, which has no caller, as the **team admin who granted `deploy`** (below), with the
  `deploy` ability. Their membership and role are checked again for every deployment, so a grantor who
  left the team or is no longer an admin deploys nothing.

**The team's consent.** ADR 0010 section 4 lets the instance admin install a plugin and each team enable it.
For `deploy` the instance admin's approval does not speak for the team, so enabling a plugin that holds
`deploy` needs the team admin's explicit answer:

- `POST /api/v1/plugins/{publisher}/{name}/enable` with `{"grant_deploy": true}` enables it and lets it
  deploy; `{"grant_deploy": false}` enables it without deployments. Without an answer the request fails
  with 422 and the plugin's capabilities. Plugins without `deploy` need no body, as before.
- The answer is stored on the team's enablement row (`coolify_rs_plugins.team_plugins.deploy_granted_by`,
  `deploy_granted_at`) and written to the audit log as `team_deploy_granted` or `team_deploy_withdrawn`
  with the team and the user.
- Disabling the plugin for the team forgets it. An update that drops `deploy` forgets it for every team,
  so a later version that asks again needs every team to answer again. An update that adds `deploy` finds
  no consent and cannot deploy until a team admin grants it.
- The host checks the consent on every call, not only at enable time.

**Rate limit.** A team's plugins together may queue at most `Limits::deploy_limit` deployments in
`Limits::deploy_window`: 20 in 10 minutes by default. The limit stops runaway loops, such as a plugin
that redeploys on `deployment.succeeded`. The counter is the audit log itself, under a per-team advisory
lock, so it holds across restarts and across plugins. Each attempt that passes the consent check takes a
slot, including attempts the API then refuses, so a plugin hammering refused deployments is bounded too.

**Audit.** Every attempt is a `deploy` row in `coolify_rs_plugins.audit` with the plugin id, the team, the
acting user, the application uuid, `force`, `on_behalf_of` (`caller` or `grantor`) and the outcome (the
API's answer with the deployment uuid, or the error). It is also logged with the `audit` tracing target.
The queued deployment itself is Coolify's row, unchanged.

### 4. Threat model

| Threat | Mitigation |
|---|---|
| A malicious or compromised plugin reads secrets | Answers are the API's for a token without `read:sensitive`; no secret field is ever serialized. Tests compare plugin answers byte for byte with such a token's |
| It reads another team's resources | The team comes from the call; the API's team-scoped queries run; another team's uuids are not found. Tests try every kind |
| It sends what it reads elsewhere | Only with `http`, only to hosts in its manifest. Reviewers judge `resources` and `http` together: the combination is an export of the listed kinds to the listed hosts |
| It deploys another team's applications | Lookup is scoped to the call's team; tests try it |
| A route lets a member deploy through the plugin (confused deputy) | Route-triggered deployments run as the caller, with the caller's abilities and role |
| An event-triggered deployment outlives the admin who allowed it | The grantor's membership and admin role are checked on every deployment |
| A deploy loop, or a flood of deployments | Per-team rate limit; Coolify's own queue limit still applies |
| An update quietly gains `deploy` | Updates never widen capabilities silently (ADR 0010); a team's consent never carries over to a version that dropped and re-added `deploy` |
| Large answers exhaust memory | 16 MiB cap on answers, the instance's memory cap, deadlines and the per-plugin concurrency limit |
| Injection through a uuid | Uuids are checked for shape, and every query binds parameters |

### 5. Review tier

`resources` and `deploy` are high-risk capabilities in the sense of ADR 0010's review table: a new plugin
holding either, and any version that adds either or adds a resource kind, needs **two maintainers and a
security review of every code path that uses them**. The registry's capability check prints a new
resource kind or a new `deploy` as an escalation.

### 6. Why plugins cannot run commands on servers

- **No boundary to enforce.** Commands run as root over SSH. A root shell on a server can read every
  secret of every resource on it, Docker's state and coolify-rs's own files there. There is no
  equivalent of "without `read:sensitive`" for a shell.
- **No existing code path with checks.** `resources` and `deploy` reuse API endpoints whose team scoping
  and checks are already tested against Coolify. Coolify's API has no "run a command" endpoint to reuse;
  the terminal is a user-facing UI feature with a person at the keyboard.
- **Parity.** ADR 0010 promises that the commands coolify-rs sends to servers are Coolify's. A plugin's
  commands would break that promise on every server they touch, invisibly to the parity suites.
- **Audit after the fact.** A command's effect is done before anyone reads the log, and its output can't
  be redacted.

It can be reconsidered only as a closed, declarative set of operations, each reviewed like an API
endpoint, not as a shell.

### 7. Interface version

The WIT package becomes `coolify-rs:plugin@0.1.1`. The change is additive: two interfaces, nothing
removed or changed. wasmtime resolves a component's `@0.1.0` imports to the host's `@0.1.1` interfaces,
and `check_imports` accepts any `0.1.x`. `plugins/testing/compat-0-1-0` is built against a frozen copy of
the 0.1.0 interface and proves on every test run that 0.1.0 components still load and run.

## Consequences

- Plugins see what a read-only API token of the team sees, through the API's own code, so the API's
  guarantees and their tests cover plugins too. A change to a serializer changes what plugins read, the
  same way it changes what API clients read.
- The plugin host reaches the API through two traits defined in `coolify-plugins` (`ResourceSource`,
  `Deployer`) that `coolify-api` implements and connects when the state is built, so `coolify-plugins`
  does not depend on `coolify-api`.
- The plugin schema gains two columns on `team_plugins` and an index on the audit log; the plugin
  endpoints gain `grant_deploy` and `deploy_granted_for_team`. Nothing Coolify defines changes.
