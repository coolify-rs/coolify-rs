# Deploy chains

A first-party coolify-rs plugin (`coolify-rs/deploy-chains`). When an application deploys successfully, it
deploys the applications that depend on it: after `api`, deploy `worker` and `scheduler`.

Chains follow on from each other. If `scheduler` has a chain of its own, it runs when `scheduler`'s
deployment succeeds, like any other deployment. The plugin deploys only the direct targets of the application
that just deployed.

## Using it

1. An instance admin installs it from the catalogue (or uploads it in development mode) and approves its
   capabilities.
2. A team owner or admin switches it on under **Team → Plugins** and answers whether it may queue the team's
   deployments. Without that consent it keeps the rules but deploys nothing, and each attempt shows up in
   the history with the reason.
3. In an application's menu, **Plugins → Deploy chain** shows "After this application deploys, deploy:"
   with the current targets. Admins add targets from a list of the team's other applications, choose
   **Force rebuild** (no build cache) for each, and can limit the chain to one branch. Members see the
   chain and its history without changing it.
4. The dashboard's **Deploy chains** panel lists every rule of the team and the recent triggered deploys.

## Safety rules

- **Previews never trigger a chain.** An event whose `preview` is anything but `null` is ignored.
- **No loops.** Saving a rule that would deploy in a loop is refused, whether the loop is a self-loop
  (`api → api`), two applications (`api → worker → api`) or longer. The error names the loop. Two saves at the
  same moment could still close one, so each event checks again and records the targets as skipped instead
  of deploying them.
- **The team's applications only.** The source and every target must be applications of the caller's team,
  checked against the `resources` capability on every save. A uuid of another team is refused like a uuid
  that doesn't exist. coolify-rs also scopes every deployment to the event's team.
- **Who can change rules.** Any member of a team that switched the plugin on can read the rules and the
  history. Saving or deleting needs `write` or `root` in the caller's abilities: a team owner or admin in the
  web interface, or an API token with `write` or `root`. Read-only tokens and members get 403.
- **Whose authority.** Deployments after an event run as the team admin who granted `deploy`, with the
  `deploy` ability. coolify-rs checks their membership and role again for every deployment
  ([ADR 0011](../../../docs/decisions/0011-plugin-resource-and-deploy-capabilities.md)).
- **Limits.** A rule deploys at most 10 applications, and a team has at most 200 rules. coolify-rs lets a
  team's plugins together queue at most 20 deployments in 10 minutes. Over that limit, or without
  consent, the refusal is recorded in the history. The event still succeeds and the next target still
  gets its turn.
- **Time.** Targets are deployed one at a time. If 3.5 s of the event's 5 s pass, the rest are recorded as
  skipped.
- **History.** Every attempt is recorded: the source's deployment, the target, `force`, the result
  (`queued`, `failed` or `skipped`), the target's deployment uuid and the message. The history keeps the
  newest 100 entries. coolify-rs also writes every deployment attempt to its plugin audit log.

## Capabilities

| Capability | Why |
|---|---|
| `events: deployment.succeeded` | To start a chain |
| `storage` | The rules and the history |
| `routes` | The API below, which the panels also use |
| `resources: applications` | To check that rules name the team's own applications, and to show their names |
| `deploy` | To queue the targets' deployments. Each team must also grant it |
| `ui: application, dashboard` | The Deploy chain panel of each application, and the overview on the dashboard |

## API

Under `/api/v1/ext/coolify-rs/deploy-chains`, with a token of a team that switched the plugin on (and under
`/ui/ext/...` for the panels):

| Route | |
|---|---|
| `GET /rules` | `{"can_write": bool, "rules": [rule]}` |
| `GET /rules/{source}` | One rule; `targets` is empty when the application has none |
| `PUT /rules/{source}` | Saves `{"targets": [{"uuid": ..., "force": false}], "branch": null}`. An empty `targets` removes the rule. Needs `write` or `root` |
| `DELETE /rules/{source}` | Removes the rule, or 404. Needs `write` or `root` |
| `GET /history` | `{"history": [entry]}`, newest first; `?application={uuid}` keeps the entries where it is source or target |
| `GET /applications` | The team's applications (`uuid`, `name`), for the panel's picker |
| `GET /panels/application?resource={uuid}`, `GET /panels/overview` | The panels |

A rule:

```json
{
  "source": {"uuid": "...", "name": "api", "missing": false},
  "branch": null,
  "targets": [{"uuid": "...", "name": "worker", "missing": false, "force": true}]
}
```

`missing` is true when the application no longer exists. `branch`, when set, limits the chain to deployments
whose `commit.branch` is that branch. A refused save answers 422 with
`{"message": "Validation failed.", "errors": {"targets": ["This would deploy in a loop: api → worker → api."]}}`.

## Storage

| Key | Value |
|---|---|
| `rule:<source uuid>` | `{"targets": [{"uuid", "force"}], "branch": null}` |
| `history` | Up to 100 entries, newest first: `{"at", "source", "source_deployment", "target", "force", "status", "deployment", "message"}` |

## Development

Every decision (validation, loop detection, what an event deploys, the history) is in `src/chain.rs` as
plain functions. The crate is also an `rlib`, and the plugin interface (`src/plugin.rs`) is compiled for
`wasm32` only, so the logic is tested natively:

```bash
cd plugins
cargo test -p deploy-chains                                    # unit tests, on the host
cargo build --release --target wasm32-wasip2 -p deploy-chains  # the component
coolify-rs plugin check target/wasm32-wasip2/release/deploy_chains.wasm official/deploy-chains/plugin.toml
```

`crates/coolify-api/tests/official_deploy_chains.rs` runs it against the real API: rules through its routes,
events through the test-event endpoint, the queued deployment rows, consent, the rate limit and the panels.
