# coolify-rs/healthchecks

Pings [Healthchecks.io](https://healthchecks.io) for every run of a scheduled task and of a database or
storage backup. Healthchecks.io alerts when a check's pings stop arriving on time, so a job that silently stops
running (a deleted schedule, a stuck server, a paused queue) raises an alarm, not only a job that fails.

## Settings

Each team sets them under **Team → Plugins → Configure**. Until the ping key is set the plugin shows **Needs
configuration** and sends nothing.

| Setting | | |
|---|---|---|
| `ping_key` | secret, required | The project's ping key (Healthchecks.io → project → Settings → Ping key) |
| `create` | `yes` (default), `no` | `yes` adds `?create=1`, so the first ping of a job creates its check. With `no`, only checks you created are pinged; others answer 404 |
| `slug_prefix` | text, default `coolify` | The start of every check's slug; lowercased, other characters become `-` |

## What is sent

One check per job, named by slug:

| Job | Slug |
|---|---|
| Scheduled task | `<prefix>-task-<task uuid>` |
| Database backup | `<prefix>-backup-<database uuid>-<database name>` (one check per database dumped) |
| Storage backup | `<prefix>-volume-<resource uuid>-<volume or directory>` |

Slugs longer than 100 characters are cut and end with a hash.

| Event | Ping |
|---|---|
| `scheduled_task.succeeded`, `backup.succeeded`, `volume_backup.succeeded` | `POST https://hc-ping.com/<ping key>/<slug>` |
| `scheduled_task.failed`, `backup.failed`, `volume_backup.failed`, `volume_backup.recovery_failed` | `POST .../<slug>/fail` |
| `backup.succeeded_with_s3_warning` | `/fail`: the backup ran, but its off-site copy is missing |
| `volume_backup.missing` | `/fail` on the storage backup's check |
| `backup.missing` | `/fail` on the checks this plugin pinged before for a database of that name |

`backup.missing` names the database only by name (and its schedule by a uuid no other event carries), so the
plugin remembers which backup checks it pinged for each database name (at most 200 names, 10 checks each) and
fails those. Before any run of the backup was seen there is nothing to fail; the check itself, if it exists,
raises the alarm when its period passes.

The body is plain text: a line saying what happened and, for failures, the last 1000 characters of the
output. Never a whole log, never a setting. The ping key may only contain letters, digits, `-` and `_`, so it
can't change the URL; anything else is refused before a request is made.

Pings are sent one after another. No ping starts after 3.5 s of an event's 5 s, and coolify-rs cuts every
request off before the call's deadline, so a slow answer fails its ping, not the call.

## Panel and routes

A dashboard panel lists the checks pinged recently (the last 50, one line per check): what the job is, its
slug, its last status, when, and what Healthchecks.io answered.

- `GET /api/v1/ext/coolify-rs/healthchecks/recent`: the same as JSON
- `GET /api/v1/ext/coolify-rs/healthchecks/panel`

## Capabilities

`events` (the ten kinds above), `storage` (the recent pings and the database names), `routes`,
`ui: dashboard`, and `http` to exactly `hc-ping.com`.

## Building and testing

```bash
cd plugins
cargo test -p healthchecks                                        # events, slugs and requests, natively
cargo build --release --target wasm32-wasip2 -p healthchecks      # target/wasm32-wasip2/release/healthchecks.wasm
```

`crates/coolify-api/tests/official_alerts.rs` runs the component against a local stand-in for hc-ping.com.
