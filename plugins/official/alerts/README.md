# coolify-rs/alerts

Sends Coolify's failures to services Coolify does not notify itself: **ntfy.sh**, **Google Chat**,
**PagerDuty** (Events API v2) and **Opsgenie** (Alerts API, US and EU). Discord, Telegram, Slack, Pushover,
email and webhooks stay with Coolify's own notifications.

Each team configures it under **Team → Plugins → Configure**. A service whose setting is empty is not used,
so a team that sets nothing sends nothing.

## Settings

| Setting | | |
|---|---|---|
| `ntfy_topic` | secret | The topic on ntfy.sh. On ntfy.sh a topic is its own password, so it is stored encrypted and sent in the body, never in a URL |
| `ntfy_priority` | `min`, `low`, `default`, `high` (default), `urgent` | Priority of failures; successes go at most at `default` |
| `google_chat_webhook` | secret URL | A space's incoming webhook. The plugin refuses any URL that does not start with `https://chat.googleapis.com/` |
| `pagerduty_routing_key` | secret | The integration key of an Events API v2 integration |
| `opsgenie_api_key` | secret | The key of an Opsgenie API integration |
| `opsgenie_region` | `us` (default), `eu` | `api.opsgenie.com` or `api.eu.opsgenie.com` |
| `coolify_url` | URL | This instance's address. When set, alerts link to the deployment, database, task or server in Coolify |
| `send_deployments`, `send_backups`, `send_tasks`, `send_servers` | `failed` (default), `all`, `off` | What to send per category |
| `send_containers`, `send_certificates` | `on` (default), `off` | Restart limits; expiring database certificates |

`failed` sends problems, and to ntfy and Google Chat also the recovery of a problem they were told about. `all`
also sends every success to ntfy and Google Chat. PagerDuty and Opsgenie never get successes, only the
resolution of incidents this plugin opened.

## What is sent

| Event | Category | Severity | Incident key | Resolved by |
|---|---|---|---|---|
| `deployment.failed` | deployments | error | `coolify-rs-deployment-<application uuid>` (`-pr-<n>` for previews) | the next `deployment.succeeded` of the application |
| `backup.failed`, `backup.succeeded_with_s3_warning` (warning) | backups | error | `coolify-rs-backup-<database uuid>-<database name>` | the next `backup.succeeded` |
| `backup.missing` | backups | error | `coolify-rs-backup-schedule-<backup uuid>` | by hand (no later event names that schedule) |
| `volume_backup.failed`, `.missing`, `.recovery_failed` | backups | error | `coolify-rs-volume-backup-<resource>-<volume or directory>` | the next `volume_backup.succeeded` |
| `scheduled_task.failed` | scheduled tasks | error | `coolify-rs-task-<task uuid>` | the next `scheduled_task.succeeded` |
| `server.unreachable` | servers | critical | `coolify-rs-server-<server uuid>` | `server.reachable` |
| `server.high_disk_usage` | servers | warning | `coolify-rs-disk-<server uuid>` | by hand |
| `server.docker_cleanup_failed` | servers | warning | `coolify-rs-docker-cleanup-<server uuid>` | by hand |
| `container.restart_limit_reached` | restart limits | error | `coolify-rs-restart-limit-<resource uuid>` | by hand |
| `ssl.certificates_expiring` | certificates | warning | `coolify-rs-ssl-<hash of the databases>` | by hand |

Repeats of the same problem add to the open incident (PagerDuty `dedup_key`, Opsgenie `alias`). The plugin
remembers, per team, which keys it opened and with which services (at most 300), and resolves only those.

Messages are short: a title with the resource's name, a line or two, the last 400 characters of a command's
output (never a whole log), and a link when there is one: the Coolify page when `coolify_url` is set,
otherwise the application's domain or the backup's resource page. Secrets never appear in messages, logs,
responses or the plugin's storage.

- **PagerDuty**: `POST https://events.pagerduty.com/v2/enqueue` with `event_action` `trigger` (summary, source,
  severity, class, `custom_details`, links) or `resolve`.
- **Opsgenie**: `POST /v2/alerts` with `Authorization: GenieKey <key>` (message, alias, description,
  priority P1 to P5, entity, tags, details), and `POST /v2/alerts/<alias>/close?identifierType=alias` to close.
- **ntfy**: `POST https://ntfy.sh/` with a JSON message: topic, title, message, priority, an emoji tag and the
  link as `click`.
- **Google Chat**: `POST` to the webhook with `{"text": "..."}`.

Services are sent to one after another, incident tools first. No request starts after 3.5 s of an event's
5 s, and coolify-rs cuts every request off before the call's deadline, so a slow service fails its own
request (the event reports it) and never the whole call.

## Panel and routes

A dashboard panel shows which services are configured (never their values), what each category sends, how
many incidents are open, and, for team owners and admins, a **Send test alert** button. The test goes to every
configured service; PagerDuty and Opsgenie get it as an info incident that is resolved right away.

- `GET /api/v1/ext/coolify-rs/alerts/status`
- `POST /api/v1/ext/coolify-rs/alerts/test` (needs `write` or `root`)
- `GET /api/v1/ext/coolify-rs/alerts/panel`

## Capabilities

`events` (the kinds above, plus `deployment.succeeded`, `backup.succeeded`, `volume_backup.succeeded` and
`scheduled_task.succeeded` to resolve incidents), `storage` (the open incidents), `routes`, `ui: dashboard`,
and `http` to exactly `ntfy.sh`, `chat.googleapis.com`, `events.pagerduty.com`, `api.opsgenie.com` and
`api.eu.opsgenie.com`.

## Building and testing

```bash
cd plugins
cargo test -p alerts                                              # events, requests and incidents, natively
cargo build --release --target wasm32-wasip2 -p alerts            # target/wasm32-wasip2/release/alerts.wasm
```

`crates/coolify-api/tests/official_alerts.rs` runs the component against local stand-ins for every service.
