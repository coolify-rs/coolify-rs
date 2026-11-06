# Delivery metrics

`coolify-rs/delivery-metrics` measures the four DORA metrics for each team and each application from the
team's deployments, shows them on the dashboard and on each application's Plugins tab, and exports them,
the deployments and the team's inventory as CSV.

## What it asks for

| Capability | Why |
|---|---|
| `events: deployment.succeeded, deployment.failed` | Each finished deployment is recorded |
| `storage` | The recorded deployments |
| `routes` | The metrics, the CSV exports and the panels |
| `resources: projects, applications, services, databases, servers` | Current application names, and the inventory export. Read-only, as the API shows them to a token without `read:sensitive`, so never passwords, keys or secrets |
| `ui: dashboard, application` | The two panels |

It doesn't subscribe to `application.status_changed`. That event fires when an application is stopped or
started outside a deployment, and it can't tell an outage from someone stopping an application on purpose,
so using it for time to restore would count deliberate stops as incidents and restarts as recoveries.
Time to restore is measured from deployments only (below).

## Settings

Under **Team → Plugins → Configure**:

| Setting | Default | |
|---|---|---|
| Default window (days) | `30` | `30` or `90`: the window the panels and exports use when a request names none |
| Count preview deployments | `no` | Pull request previews are always recorded; `yes` counts them in every metric |
| Environments | empty | Only count deployments to these environments, separated by commas (`production`). Empty counts all |
| Retention (days) | `180` | How long deployments are kept, 7 to 730. Keep at least 90 for the 90-day window |

## The metrics

All four are computed over a window of 30 or 90 days ending now, from the deployments the settings count.
A deployment's time is when its event arrived, which is when it finished.

- **Deployment frequency**: successful deployments per day (and per week, ×7) over the window.
- **Change failure rate**: failed deployments ÷ all deployments in the window.
- **Time to restore**: for each application, a *failure streak* starts at a failed deployment that follows
  a success (or starts its history) and ends at the next successful deployment of the same application;
  later failures in the same streak don't start a new one. A pull request's previews are an application of
  their own. Streaks that start in the window count; the median and 90th percentile of the restored ones
  are shown, and streaks without a success yet are counted as `unrestored`.
- **Lead time for changes**: from the commit to its successful deployment, median and 90th percentile.
  **It shows "Not available"** today: Coolify's deployment events carry the commit's SHA, message and
  branch, but not when the commit was made, and a plugin can't ask Git. If an event's `commit` ever
  carries `committed_at` or `timestamp` (Unix seconds or RFC 3339), lead time is measured from it.

Percentiles use the nearest-rank method. Numbers are rounded to three decimals (rates to four).

### Bands

The bands are those of the [2021 Accelerate State of DevOps report](https://dora.dev/publications/pdf/state-of-devops-2021.pdf)
(DORA and Google Cloud), table "Software delivery performance metric", page 9:

| Metric | Elite | High | Medium | Low |
|---|---|---|---|---|
| Deployment frequency | On-demand (multiple deploys per day) | Between once per week and once per month | Between once per month and once every 6 months | Fewer than once per six months |
| Lead time for changes | Less than one hour | Between one day and one week | Between one month and six months | More than six months |
| Time to restore service | Less than one hour | Less than one day | Between one day and one week | More than six months |
| Change failure rate | 0%–15% | 16%–30% | 16%–30% | 16%–30% |

The published ranges leave gaps (what is a lead time of three hours?) and overlap (the last three change
failure rates). So each band is decided by its published limit on the side that matters, and a value
that meets several limits gets the best of those bands; values past the worst published limit are low:

| Metric | Elite | High | Medium | Low |
|---|---|---|---|---|
| Deployment frequency (successful per day) | ≥ 1 | ≥ 1/30 | ≥ 1/182.5 | less, including none |
| Lead time (median) | < 1 hour | ≤ 7 days | ≤ 183 days | longer |
| Time to restore (median) | < 1 hour | < 1 day | ≤ 7 days | longer |
| Change failure rate | ≤ 15% | ≤ 30% | | > 30% |

A metric without data (no deployments, no restored streak, no commit times) has no band.

## Storage

Deployments are stored per UTC month, under `d/<YYYY-MM>/<nnn>`, in parts of at most 256 KiB (a quarter of
the 1 MiB value limit), with an index under `index`. A record is about 200 bytes. All parts together stay
under 6 MiB, well inside the 10 MiB team quota; when a new deployment would pass that, the oldest parts are
deleted first. Retention deletes deployments older than the setting every time a deployment is recorded.
A full store never fails an event: the oldest part is deleted to make room, and if that doesn't help the
deployment waits for the next event.

A team's events run at the same time, so each deployment first goes to an inbox key of its own
(`q/<time>-<deployment>-<outcome>`) and every event merges the whole inbox into the parts; an entry leaves
the inbox a minute after it is in a part. Delivering the same deployment twice records it once.

## Routes

Under `/api/v1/ext/coolify-rs/delivery-metrics` (any token with `read` of a team that enabled the plugin),
and `/ui/ext/coolify-rs/delivery-metrics` for the panels:

| Route | Answers |
|---|---|
| `GET /metrics.json?window=30\|90&top=N` | The team's four metrics with bands, its applications (top `N` by deployments, 10 by default, at most 100), its latest deployments and what is stored |
| `GET /metrics.json?window=30\|90&app=<uuid>` | One application's metrics and its latest 20 deployments |
| `GET /metrics.csv?window=30\|90` | The metrics: one line for the team, one per application |
| `GET /deployments.csv?window=30\|90&app=<uuid>` | Every recorded deployment in the window, previews and other environments too, with a `counted` column |
| `GET /inventory.csv` | The team's projects, environments, applications, services, databases and servers: kind, name, uuid, type, status, address (domains or IP), project, environment |
| `GET /panels/dashboard`, `GET /panels/application?resource=<uuid>` | The panels |

`window` defaults to the team's setting; any other value answers 422. The CSV files follow RFC 4180 (CRLF,
quoted fields where needed). A field that starts with `=`, `+`, `-`, `@`, a tab or a carriage return gets a
leading `'`, so a spreadsheet shows names like `=HYPERLINK(...)` as text rather than running them.

### Downloading the CSV files

Panels can't download files, so use an API token with `read` (Keys & Tokens → API tokens) of the team:

```bash
BASE=https://coolify.example.com/api/v1/ext/coolify-rs/delivery-metrics
curl -fsS -H "Authorization: Bearer $TOKEN" "$BASE/metrics.csv?window=90" -o metrics.csv
curl -fsS -H "Authorization: Bearer $TOKEN" "$BASE/deployments.csv?window=30" -o deployments.csv
curl -fsS -H "Authorization: Bearer $TOKEN" "$BASE/inventory.csv" -o inventory.csv
```

The token's team is the team exported.

## Panels

The dashboard panel shows the team's four metrics with their bands, a 30/90-day toggle, and its ten busiest
applications. Each application's panel shows that application's metrics and its latest deployments. Both
follow the light and dark theme and load their data through the plugin's own routes.

## Building and testing

```bash
cd plugins
cargo test -p delivery-metrics                                   # recording, pruning, metrics, bands, CSV
cargo build --release --target wasm32-wasip2 -p delivery-metrics
coolify-rs plugin check target/wasm32-wasip2/release/delivery_metrics.wasm official/delivery-metrics/plugin.toml
```

`crates/coolify-api/tests/official_delivery_metrics.rs` runs it against the real API.
