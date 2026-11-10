# Known Limitations and Divergences

coolify-rs aims to behave exactly like upstream Coolify: the same API responses, database rows, commands on servers
and UI, with only the name and logo changed. It implements all of upstream's API operations and web pages. This page
lists the differences we know of that a user, operator or API client can notice, so you can decide whether coolify-rs
can replace Coolify for you. It is maintained with the code: an entry goes when its difference is fixed.

"Upstream" means Coolify at the revision in [UPSTREAM_BASELINE.md](UPSTREAM_BASELINE.md).

## Install and upgrades

- **Install and update address.** Upstream installs and updates from Coolify's CDN. coolify-rs installs and updates
  from its GitHub releases (`COOLIFY_RS_CDN`, default `https://github.com/coolify-rs/coolify-rs/releases/latest/download`)
  and pulls `ghcr.io/coolify-rs/coolify-rs` (`COOLIFY_RS_REGISTRY`). `REGISTRY_URL` applies to Coolify's helper image
  only. Update checks read coolify-rs's own version file (`COOLIFY_RS_VERSIONS_URL` or `CDN_URL`).
- **Update channels.** The Preview and Nightly channels of the update settings take a version only from a version
  file that lists channels (`{"stable": …, "preview": …, "nightly": …}`); with a stable-only file every channel
  gets the stable release. A custom image tag installs `ghcr.io/coolify-rs/coolify-rs:<tag>`.
- **Existing Coolify databases.** Each release takes over exactly one Coolify release (4.6.0 for `4.6.0-rs.5`; see
  [MIGRATING.md](MIGRATING.md)). `coolify-rs boot` runs the upstream migrations from Coolify 4.5.3 to that release (the
  ones that bring its own earlier installs up), whoever created the database: a Coolify 4.5.3 database is upgraded to
  the 4.6.0 schema on the first start, and from then on only Coolify 4.6.0 can take it back. A database from a Coolify
  release older than 4.5.3 is served read-only until Coolify itself is upgraded. A database with migrations newer than
  the build is read-only too (writes answer `503`) unless `COOLIFY_RS_ALLOW_UNKNOWN_SCHEMA=true`. Migration names that
  upstream later renamed or deleted (`compat/baseline/historical-migrations.txt`) are accepted.
- **Install layout.** No SSH key files or multiplexing sockets on disk, ports 6001 and 6002 are not published, and
  there are no `applications`, `databases`, `services` or `backups` mounts. The terminal websocket is served at
  `/terminal/ws` on the UI's port, and on port 6002 inside the `coolify` network, where the proxy configuration of an
  instance with a domain sends it (`COOLIFY_RS_TERMINAL_LISTEN`, `off` to turn it off). `TERMINAL_PROTOCOL`,
  `TERMINAL_HOST` and `TERMINAL_PORT` do not apply. Nothing listens on port 6001: live updates use the UI's port.
- **Startup tasks.** Of upstream's `app:init`, coolify-rs does not run `cleanup:names`, the environment-name slash
  fix, the Coolify database backup restore, the user email update or the cleanup of unused proxy networks.
  `cleanup:stucked-resources` runs on its schedule only. The alive signal to upstream's telemetry address is left out on
  purpose.
- **Interrupted deployments at startup.** Upstream marks every in-progress and queued deployment as failed. coolify-rs
  marks only the in-progress deployments its own host was running, and leaves queued ones queued. Two coolify-rs
  instances on one host would therefore fail each other's running deployments.
- **Root user from the environment.** `ROOT_USER_EMAIL` is checked for format only; upstream's `email:rfc,dns` rule
  also requires the domain to resolve. `ROOT_USER_PASSWORD` is not checked against the breached-password list, on
  purpose: coolify-rs contacts no third party while it starts.
- **Sessions are held in memory**, not in the `sessions` table, so a restart or an upgrade signs everyone out (the
  upgrade's progress view ends on the login page). So does sign-in state in progress (two-factor challenge, OAuth
  state, passkey challenge, flash messages), and rate-limit counters reset. Limits: 10,000 sessions, 7 days idle
  (`SESSION_LIFETIME`), 30 days absolute.
- **Background work runs in the coolify-rs process**, not on a Redis queue. A restart drops running deployments,
  server validations, database jobs, imports, backups, DNS jobs and queued notification retries. Running scheduled task
  executions are marked failed at startup ("The worker stopped..."), and a running server transfer shows "running" for
  up to 30 minutes. Locks and marks upstream keeps in Redis or its cache (backup and task overlap, the DNS hostname lock,
  pending backup downloads and their delayed S3 delete, GitHub App setup state, webhook failure counters, the combined
  multi-server deployment notification) are in memory and do not survive a restart.

## API

- **No MCP server.** Upstream serves an MCP endpoint at `/mcp` (`routes/ai.php`, `App\Mcp\Servers\CoolifyServer` and
  its tools). coolify-rs does not: `/mcp` and the MCP tools are not ported. Only the `POST /api/v1/mcp/enable` and
  `/mcp/disable` switch exists, which stores the setting and changes nothing else.
- **No API rate limiting.** Upstream applies `throttle:api` (200 requests a minute, `X-RateLimit-*` headers).
  coolify-rs has none; only sign-in is throttled.
- **Errors for clients that do not ask for JSON** get the JSON body upstream sends to JSON clients, instead of
  Laravel's HTML error page. Likewise an invalid `private_key` on `PATCH /security/keys/{uuid}` answers `422` JSON where
  upstream redirects (`302`).
- **Non-string values for string fields** report only "must be a string"; upstream also reports its min, max and regex
  messages.
- **Concurrent duplicate environment names** answer `409`; upstream fails with `500` on the unique index.
- **Failed creates leave nothing behind.** When a create fails halfway on a value PostgreSQL rejects (for example
  `gpu_driver: null`), both answer `500`, but upstream keeps the application row it had already saved.
- **`PATCH /databases/{uuid}/envs/bulk`** applies `key`, `value`, `is_literal`, `is_multiline`, `is_shown_once` and
  `comment`. Upstream's mass assignment also takes other fillable columns of an item, including `resourceable_*`.
- **`DELETE /applications/{uuid}/envs/bulk`** answers `405`; upstream answers `404` (it treats `bulk` as an env uuid).
- **Unknown columns** that a newer upstream release adds are left out of API output and logged until reviewed;
  upstream serializes every column that is not hidden.
- **Audit events.** `api.server.validated` and remote commands go to the log (`audit` target), not to `audit_events`.
  Failed API sign-ins are deduplicated per source in memory, not per source, reason, server and email in the cache.
- **Webhook failure pages** answer JSON with the same status where upstream renders its HTML 500 page. A missing
  `/project/{uuid}/icon` or unknown `/files/download/{token}` answers JSON `404` instead of the HTML error page.

## UI

- **Member access grants on resource pages.** The API checks the grants of members and tokens on every project,
  environment and resource endpoint, and the UI hides what a member cannot see and blocks team-level pages. Resource
  pages of applications and databases follow the grants; the pages of services, environment variables, storages,
  scheduled tasks, backups, previews and shared variables still need an owner or admin to change anything, as before
  4.6.0 (a member with write, deploy or delete grants there uses the API instead). Navigation entries for project
  and environment shared variables stay visible to a restricted member.
- **No real-time connection.** Upstream pushes updates over Laravel Echo; coolify-rs pages poll instead. Proxy status
  toasts, scheduled task results and server transfer progress appear on the next poll (the transfer list every 15
  seconds). Canvases show a status change after Refresh, or on the dashboard canvas within five seconds. The "real-time
  service unavailable" popup never appears, and `/realtime` answers "Look at your other tab." without sending a test
  event.
- **Signed-out visitors** on signed-in pages (except `/admin` and `/invitations/{uuid}`), and anyone on an unknown
  path, get `200` with the app shell where upstream answers `302`. The browser ends up on the same page.
- **Servers list** has no Patches column (for example "5 security updates").
- **Server page after linking a cloud provider** shows no provider status badge, refresh or power controls.
- **Connection retries**: upstream sends every idempotent outgoing HTTP request once more after a connection failure
  (`RetryHttpConnectionFailures`, global 10 s connect and 30 s timeouts). coolify-rs does this for Cloudflare only.
- **Metrics live refresh** is throttled in the browser (one refresh per poll period, none while paused); upstream also
  enforces it on the server. Loading a range is not limited on either side.
- **"Configuration changed" warning** on application, database and service pages has no list of the changes.
- **Remembered list or canvas view** is read in the browser, so a list page shows for an instant before it switches to
  the canvas.
- **SQLite on the deploy canvas** is offered (v4.6.0) but its create call fails, as upstream: the canvas route group has
  no `databases/sqlite` route.
- **Table column sorts** that upstream runs on the server (dashboard, server images and resources, tag deployments) run
  in the browser on the loaded rows; the order is the same, the header spinner only shows where the page reloads.
- **Cloud tokens, cloud-init scripts and integration tokens** pages stay empty for a member, where upstream shows its
  403 page.
- **Application General page instant saves** (site type, labels) store only that setting; upstream also saves unsaved
  edits of the form when it regenerates the Nginx configuration.
- **Saving Compose domains on the General page** does not run upstream's DNS and domain-usage checks; the Domains page
  does.
- **Command palette.** "new storage" and "new private key" go to their page and open the modal there, "new github"
  goes to `/sources`, and the "This page" scope searches sections only.
- **Instance backup page** selects storages with checkboxes instead of a multi-select list, and its executions have no
  Delete, Download, Clean failed or Clean deleted actions.
- **Deleting a user from `/team/admin`** is refused with a message when a team only that user belongs to still owns
  servers or projects; upstream deletes them.
- **DNS entries modal.** A conflict from "Add with" opens the Configure DNS modal so the record can be replaced;
  upstream stores the conflict and shows it only if that modal is open.
- **Smaller UI differences.** A failing dynamic proxy configuration with a missing private key shows a toast instead
  of a 404 dialog. A non-numeric Preview ID shows a message under the field instead of an error page. Resending a reset
  link within a minute shows the error in the form instead of a plain text page. A failed OIDC discovery answers a bare
  500 instead of the styled error page.
- **The name "Coolify" stays in places, on purpose.** Backend messages and stored strings (for example "Created by
  Coolify") keep upstream's wording so API responses stay identical, and lines that name upstream itself (the
  "Love Coolify?" popup, Coolify Cloud, Coolify v5, affiliate links) are kept.

## Deployments

- **Compose files with a parsing version below 3** (resources created by old Coolify releases) cannot be parsed or
  deployed.
- **Preview domains have no DNS check.** "Check DNS" and "Check all DNS" say checks are not available, and a new
  preview domain stays "Not checked".
- **Creating a Compose preview** through the API does not yet add its bind mounts or store the rendered Compose file;
  its deployment does. A clone of a Compose application with a named volume and a preview answers `500` like upstream
  but writes nothing, where upstream leaves a half-made clone.
- **Cloning an application from the UI** copies its previews without domains; the API clone regenerates them like
  upstream.
- **Clone volume data** (`CloneVolumeDataWithSourceStopped`) runs its stop, copies and start in the process, not on a
  queue: a restart of coolify-rs during the copy leaves the source stopped. Checked against a scripted `ssh` only.
- **TCP proxies of service databases** are not started, recreated or removed by the status checks (standalone
  databases are).
- **Deployment rows.** `current_process_id` stays empty, and commands upstream feeds on stdin carry their input inside
  the script.
- **A registry image name or tag that does not validate** (saved before the forms checked it) fails the deployment
  when its worker starts, with upstream's "Deployment could not be started" lines. Upstream already refuses it when the
  job is queued, so its Deploy button or API call answers with an error; coolify-rs answers "Deployment queued.".

## Servers

- **SSH connections are not reused.** coolify-rs opens a new SSH connection for each command (`ControlMaster=no`,
  `crates/coolify-remote/src/exec.rs`); upstream multiplexes them.
- **Validating a server from the UI** (server page and onboarding) does not install missing prerequisites or Docker;
  it stops with upstream's "install them manually" message. The API's `install=true` does install.
- **Container status polling** runs every minute on every server, also when the scheduler is off and when Sentinel is
  pushing updates, where upstream skips it.
- **Sentinel pushes** go through the same code as polling, so `additional_servers_status` is not updated from a push
  and containers on a Swarm are not told apart.
- **Traffic analytics on Caddy.** Turning analytics on or off does not regenerate stored read-only application labels;
  the next deployment does.
- **CA certificate.** The subject key identifier is SHA-256 based.
- **Server transfer finish checklist.** The S3 step is never tested (it stays "action"), the GitHub App webhook step
  does not ask GitHub for the App's webhook URL (only a received webhook counts), and the GitHub fix action is not
  available.

## Databases and services

- **Connection URLs** (`internal_db_url`, `external_db_url` and the URLs on the database page) leave out the SSL
  parameters (for example `?sslmode=`) when SSL is enabled.
- **`config_hash`** is stored after every database start; upstream stores it only when the database page is open after
  a start.
- **Service databases whose image names no known engine** have no backup or import pages (`404`), and their backup
  schedules end without a run.
- **Service Domains page** does not suggest the `www` or apex counterpart of a domain.
- **Storage pages of Compose resources** have no actions for stale Compose volumes or notes about replaced external
  volumes and driver options.
- **File downloads through S3.** The instance setting cannot be switched to an S3 storage other than the stored one,
  and the file browser always streams files from the server.
- **Database imports.** The cleanup upstream runs after a transport failure is not ported.

## Integrations

- **URL safety checks** for webhooks, S3 endpoints and other outbound URLs resolve hostnames with the system resolver,
  not the instance's custom DNS servers.
- **OIDC** reads the discovery document and the JWKS on every sign-in; upstream caches them for one and six hours.
- **Error texts from other libraries.** A failing SMTP test email shows lettre's text for errors other than "connection
  refused", and a failing S3 connection test is shaped like Flysystem's message but not identical to the AWS SDK's text.
- **Managed DNS records.** `ConfigureDnsRecordJob` marks the row as managed itself; upstream does that from a broadcast
  to an open page, so without one its row stays unchecked.
- **Avatars and project icons** are stored on the local disk (`{COOLIFY_RS_DATA_DIR}/images`). A row with an S3 avatar
  or icon answers `404`, files stored by upstream's container are not visible unless the disks are shared, and deleting a
  project leaves its icon file.
- **Deleting a team or account** removes its keys, tags, shared variables, S3 storages and sources without writing
  audit events for them.
- **Coolify Cloud features are not included**: Stripe billing and subscriptions, subscription and email-verification
  gates, Cloud-only throttles and jobs, and `CF-Connecting-IP` from any peer. coolify-rs is self-hosted only.

## Security

These differences are deliberate.

- **SSH host keys are verified.** Upstream disables checking (`StrictHostKeyChecking=no`). coolify-rs pins a server's
  key on first use, and a changed key fails with "Host key verification failed" (with the new fingerprint and how to
  accept it). The pin is dropped when the IP or port changes or the server is deleted.
  `COOLIFY_RS_SSH_HOST_KEYS=ignore` restores upstream's behaviour.
- **SSH key files** are written `0600` for the length of a command and then removed, instead of kept in
  `storage/app/ssh/keys`. Errors still name the key `ssh_key@<uuid>`.
- **SSH options.** coolify-rs adds `-F /dev/null`, `BatchMode`, `IdentitiesOnly`, no agent and no forwarding, and
  clears the environment, so a hostile server or operator config cannot change behaviour.
- **Remote output is capped** at 16 MiB for stdout and 1 MiB for stderr.
- **Client IP.** Upstream v4.5.3's `client_ip()` uses the forwarded address whenever the TCP peer is a private,
  loopback or link-local address, so anything on the Docker network or host can set it. coolify-rs keeps its trusted-proxy
  model (ADR 0006): it uses the peer address unless `COOLIFY_RS_TRUSTED_PROXIES` is set, and then the rightmost untrusted
  hop, so a private peer is trusted only when listed. The installer and upgrader list the `coolify` Docker network,
  which Coolify's proxy uses, when the setting is missing; containers attached to that network are trusted with it.
  The same rule applies to the scheme and host used for a new server's `sentinel_custom_url`, and to marking cookies
  Secure.
- **Database storage responses.** Upstream's `POST` and `PATCH /databases/{uuid}/storages` answers include the whole
  database with its server and saved proxy configuration, also for tokens without `read:sensitive`. coolify-rs leaves
  that relation out.
- **Log drain secrets** are not sent to members' browsers; upstream's page state carries them.

## Minor

- **Row order where upstream has none.** Where upstream's query has no `ORDER BY` (resources of an environment,
  service variables, applications and databases, file storage mounts in a Compose file), coolify-rs orders by `id`;
  upstream follows the table's physical order, which changes after updates. Ties in traffic analytics lists can
  reorder after a filter change.
- **Text handling.** `Str::slug` in container labels does not transliterate non-ASCII letters. Time zones use chrono-tz
  data, not PHP's. The 1000-column limit of a commit message in notifications uses its own table of wide characters.
- **Notification requests** carry `Accept: */*`, and SMTP mail differs in MIME framing (`Message-ID`, encoding choice,
  header folding); senders, recipients, subjects and bodies are the same.
- **Uploaded backups** get their `mime_type` from the file signature.
- **Scheduled tasks.** A manual execute during a run gives one execution, where upstream with one queue worker runs a
  second one afterwards. A starting service whose containers run counts as running.
- **Regenerated SSL certificates** of databases take their names from the database, not the stored `common_name`.
- **Onboarding audit events** record one `ui.server.updated` (`proxy`) of upstream's three, with only the name in
  `changes`.
- **Upstream glitches not reproduced**: the shared variables count lagging one request behind, stale rows in the S3
  resources table after a move, and a same-server service clone with volumes dying with "Nesting level too deep".
