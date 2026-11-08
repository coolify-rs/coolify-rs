<p align="center">
  <img src="frontend/public/coolify-rs-logo.svg" alt="coolify-rs logo" width="120" height="120">
</p>

<h1 align="center">coolify-rs</h1>

<p align="center"><strong>Everything you love about Coolify, in one Rust binary that idles in 30 MB and answers the API up to 9× faster.</strong></p>

A Rust implementation of [Coolify](https://github.com/coollabsio/coolify), the open-source, self-hostable platform for
deploying applications, databases and services to your own servers.

coolify-rs keeps Coolify's behaviour and changes its foundations. It serves the same REST API, reads and writes the same
PostgreSQL database, encrypts secrets the same way, runs the same commands on your servers and shows the same interface.
Underneath, it is one compiled Rust service in place of PHP, Laravel, Horizon and their workers.

*coolify-rs is an independent project. It is not affiliated with or endorsed by coolLabs, the makers of Coolify.*

## Why we love Coolify

Coolify shows what self-hosting can feel like. Point it at a server you own, and you get the experience people pay
platforms for: git push deploys, preview deployments, one-click services, managed databases with backups, automatic
TLS, logs, metrics and a terminal, all on hardware and in a place you control. It is open source under Apache-2.0,
built in the open and shaped by a large, friendly community. It has taken a lot of people off platforms they were
locked into, and many of us learned how deployment works by reading its code.

coolify-rs exists because of that. Every behaviour here was first designed, built and tested by the Coolify
maintainers and contributors. Thank you, Andras and the whole Coolify community.

## Why coolify-rs

Coolify runs on PHP and Laravel, with a web server, queue workers, a scheduler and Redis next to the application.
That is a good stack for building a product quickly. coolify-rs explores what the same product looks like as a single
Rust service:

- **One process, small footprint.** The API, web UI, job runner and scheduler share one binary that idles in about
  30 MB, which helps on the small servers many people self-host on.
- **Safety by construction.** Remote commands are built without shell interpolation, secrets never reach the logs,
  every query is scoped to the team, and SSH host keys are pinned. Where coolify-rs deliberately differs from
  upstream for security, the difference is documented, never silent.
- **Compatibility you can check.** "Identical" is tested, not assumed. Differential scripts run the same requests,
  deployments and screens against an official Coolify image and coolify-rs side by side, and compare responses,
  database rows, commands on the server and screenshots.
- **No lock-in in either direction.** coolify-rs can adopt an existing Coolify database, and Coolify can take a
  coolify-rs database back at the same version.

## Performance

On a server the size of a small VPS, coolify-rs uses a twelfth of the memory, needs a twentieth of the
CPU per request and serves 6–9× more API requests:

| | Coolify | coolify-rs | |
|---|---|---|---|
| Memory at idle | 372 MiB | 30 MiB | 12× less |
| CPU per API request | 11–12 ms | 0.5 ms | 23× less |
| API requests per second, 10 clients | 165–178 | 1,155–1,430 | 6–9× more |
| Listing 113 applications (1.7 MB), 10 clients | 18 req/s | 124 req/s | 7× more |
| Median API response, 1 client | 10–11 ms | 1.2–1.3 ms | 8–9× faster |
| Startup | 1.62 s | 0.22 s | 7× faster |

Both ran as isolated stacks on 2 CPU cores, each with its own PostgreSQL and the same data, with their
schedulers running. Coolify's memory includes Redis. PostgreSQL itself uses a little more memory with
coolify-rs (118 against 83 MiB), because its connection pool keeps more connections open. The setup, every
endpoint at 1, 10 and 50 clients, and how to rerun it are in [compat/bench/RESULTS.md](compat/bench/RESULTS.md).

## What it includes

- The Coolify REST API (`/api/v1`): tokens, teams, projects, environments, servers, applications, databases,
  services, deployments, backups, notifications, cloud providers and more, with upstream's responses and errors.
- The web UI, a port of Coolify's interface: the same pages, layout, markup and stylesheet, with coolify-rs's own
  name and logo.
- Deployments for every build pack: Dockerfile, Docker Compose, Docker image, static, Nixpacks and Railpack, plus
  preview deployments, rollbacks, additional servers, build servers, registries and Swarm.
- Servers over SSH: validation, Traefik and Caddy proxies, Sentinel metrics, Docker cleanup, the terminal, server
  transfers and patch checks.
- Standalone databases (PostgreSQL, MySQL, MariaDB, MongoDB, Redis, KeyDB, Dragonfly, ClickHouse) with S3 backups
  and imports, and one-click services from Coolify's template catalogue.
- The scheduler: scheduled tasks, backups and the instance jobs from Coolify's `Kernel`.
- Git sources (GitHub Apps, GitLab, Gitea, Bitbucket and deploy keys), secret managers, managed DNS and Cloudflare
  Tunnels, notifications and GitHub runners.
- An installer and upgrader modelled on Coolify's own.

Where coolify-rs and Coolify still differ, [docs/KNOWN_LIMITATIONS.md](docs/KNOWN_LIMITATIONS.md) says so.

## Getting started

### Install on a server

On a clean server (amd64 or arm64, 30 GB of disk; Ubuntu is tested), as root:

```bash
curl -fsSL https://github.com/coolify-rs/coolify-rs/releases/latest/download/install.sh | sudo bash
```

Then open `http://<your-server>:8000` and register; the first account owns the instance. Back up
`/data/coolify/source/.env`: it holds the `APP_KEY` that encrypts your secrets. Requirements, installer settings,
upgrades and installing a local build: [docs/INSTALL.md](docs/INSTALL.md).

### Move from Coolify

coolify-rs takes over a Coolify server in place, with the same database, secrets, SSH keys and running resources, and
Coolify can take it back. Each release matches one Coolify release: `4.6.0-rs.2` matches Coolify 4.6.0. Back up, run
the dry-run check, then the same installer: [docs/MIGRATING.md](docs/MIGRATING.md).

### Run it against an existing Coolify database

coolify-rs uses Coolify's schema as it is. Read [docs/DATABASE_COMPATIBILITY.md](docs/DATABASE_COMPATIBILITY.md)
first, and try it on a copy of your database. A database from a different Coolify version puts coolify-rs into
read-only mode.

```bash
export DATABASE_URL=postgres://coolify:<password>@<host>:5432/coolify
export APP_KEY=<the APP_KEY of that Coolify instance>
cargo run -p coolify-rs -- check-schema
COOLIFY_RS_LISTEN=127.0.0.1:8080 cargo run -p coolify-rs
```

## Development

You need Rust (the toolchain in `rust-toolchain.toml`), Node.js, Docker and OpenSSH 7.6 or newer.

```bash
docker compose up -d --wait                  # development PostgreSQL on 127.0.0.1:25432
./scripts/test.sh                            # format check, clippy and every test
(cd frontend && npm ci && npm run build)     # web UI into frontend/dist, which coolify-rs serves
```

For the UI with hot reload, run coolify-rs on `127.0.0.1:18080`, then `cd frontend && npm run dev`. Vite on port
5173 forwards `/ui` and `/api` to it.

### Layout

```
apps/coolify-rs         the server binary: serve, boot, schema and operator commands
crates/coolify-api      HTTP API and web UI routes: middleware, validation, serialization, handlers
crates/coolify-jobs     deployments, server work, the scheduler, notifications, integrations
crates/coolify-remote   SSH execution with pinned host keys and short-lived key files
crates/coolify-db       Coolify's schema, the schema guard and queries
crates/coolify-compat   Laravel and PHP formats: encrypter, Sanctum tokens, json_encode, cron expressions
crates/coolify-domain   types and rules with no I/O
frontend/               the web UI (Svelte 5)
compat/                 baselines, route inventory, mapping registry, differential and screenshot tests
docs/                   architecture, compatibility, security model and decisions (docs/decisions)
```

### Configuration

| Variable | Default | Meaning |
|---|---|---|
| `DATABASE_URL`, or `DB_HOST`, `DB_PORT`, `DB_DATABASE`, `DB_USERNAME`, `DB_PASSWORD` | | PostgreSQL connection |
| `APP_KEY`, `APP_PREVIOUS_KEYS` | | Coolify's encryption key; needed for private keys, secrets and two-factor sign-in |
| `COOLIFY_RS_LISTEN` | `0.0.0.0:8080` | Listen address |
| `COOLIFY_RS_UI_DIR` | `frontend/dist` | Built web UI |
| `COOLIFY_RS_DATA_DIR` | `/data/coolify-rs` | Pinned host keys, short-lived key files and other state |
| `COOLIFY_RS_SCHEDULER` | on | `off` stops the scheduler, for example when Coolify shares the database |
| `COOLIFY_RS_SECURE_COOKIES` | `false` | Set to `true` behind HTTPS |
| `COOLIFY_RS_TRUSTED_PROXIES` | none | IPs or CIDRs whose `X-Forwarded-For` is trusted |
| `COOLIFY_RS_SSH_HOST_KEYS` | `pin` | `ignore` behaves like Coolify |
| `COOLIFY_RS_SSH_MAX_CONCURRENCY` | `32` | Concurrent SSH commands |
| `SSH_COMMAND_TIMEOUT` | `3600` | Seconds, as in Coolify |
| `SESSION_LIFETIME` | `10080` | Minutes |
| `COOLIFY_RS_LOG_FORMAT`, `RUST_LOG` | | `pretty` or `json`, and the log filter |

### Comparing with Coolify

The differential tests run an official Coolify image as the reference and compare it with coolify-rs, request by
request, deployment by deployment and screen by screen:

```bash
git clone --bare --no-single-branch https://github.com/coollabsio/coolify .upstream/coolify.git
./scripts/reference-up.sh                    # official Coolify image on 127.0.0.1:18000, throwaway secrets
./scripts/reference-seed.sh                  # fixture users, teams and tokens through Coolify's own models
scripts/dev-server.sh 18080                  # coolify-rs on 127.0.0.1:18080 against the same database
python3 compat/diff/differential.py --tokens compat/reference/secrets/tokens.json
```

Each script in `compat/diff/` covers one area and says what it needs at the top. The screenshot comparisons are in
`compat/ui/`. [compat/notes/porting-brief.md](compat/notes/porting-brief.md) describes how an area is ported and
when it counts as done.

## Contributing and security

Contributions are welcome; [CONTRIBUTING.md](CONTRIBUTING.md) explains how. Report vulnerabilities privately as
described in [SECURITY.md](SECURITY.md).

## License

Apache-2.0, the same as Coolify. coolify-rs includes files copied or generated from Coolify, which keep their
Apache-2.0 license; see [NOTICE](NOTICE). "Coolify" is a name of coolLabs, used here only to describe what this
project is compatible with.
