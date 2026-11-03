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

- **One process, small footprint.** The API, web UI, job runner and scheduler share one binary, which helps on the
  small servers many people self-host on.
- **Safety by construction.** Remote commands are built without shell interpolation, secrets never reach the logs,
  every query is scoped to the team, and SSH host keys are pinned. Where coolify-rs deliberately differs from
  upstream for security, the difference is documented, never silent.
- **Compatibility you can check.** "Identical" is tested, not assumed. Differential scripts run the same requests,
  deployments and screens against an official Coolify image and coolify-rs side by side, and compare responses,
  database rows, commands on the server and screenshots.
- **No lock-in in either direction.** coolify-rs can adopt an existing Coolify database, and Coolify can take a
  coolify-rs database back at the same version.

## Performance

We measured both against each other on two isolated stacks shaped like a small VPS. Each had 2 CPU cores,
its own PostgreSQL, its scheduler running and the same data: 113 applications. The full setup and every
number are in [compat/bench/RESULTS.md](compat/bench/RESULTS.md):

| | Coolify | coolify-rs |
|---|---|---|
| Application memory at idle | 372 MiB (with Redis) | 30 MiB |
| CPU at idle, whole stack | 3.6% of a core | 0.5% of a core |
| Startup to healthy | 1.62 s | 0.22 s |
| Median API response, small endpoints, 1 client | 10–11 ms | 1.2–1.3 ms |
| CPU per API request, small endpoints | 11–12 ms | about 0.5 ms |
| Requests per second, small endpoints, 10 clients | 165–178 | 1,155–1,430 |
| `GET /api/v1/applications` (1.7 MB), 10 clients | 18 req/s, p50 544 ms | 124 req/s, p50 78 ms |

PostgreSQL itself uses a little more memory with coolify-rs (118 MiB against 83 MiB), because the
connection pool keeps more connections open. To rerun the comparison, use `compat/bench/stack.sh` and
`compat/bench/bench.py`.

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

coolify-rs installs the way Coolify does: one script, run as root on a clean server with at least 30 GB of disk.
It is tested on Ubuntu; like Coolify's installer, it also handles Debian, the RHEL family, SUSE, Arch and Alpine. It installs Docker and OpenSSH if needed, creates the
`/data/coolify` folders, generates secrets and an SSH key for the server, and starts coolify-rs with Docker Compose.

```bash
curl -fsSL "$COOLIFY_RS_CDN/install.sh" | COOLIFY_RS_CDN="$COOLIFY_RS_CDN" bash
```

`COOLIFY_RS_CDN` is the address the files in `scripts/install` are published at. No official release is published
yet, so build and serve your own; see [Install from a local build](#install-from-a-local-build).

When the script finishes, open `http://<your-server>:8000` and register. The first account owns the instance, and
registration stays open for 10 minutes after a start (`docker restart coolify` opens it again). Then add a server,
create a project and deploy.

**Back up `/data/coolify/source/.env`.** It holds the `APP_KEY` that encrypts your secrets, and the database cannot be
read without it. Running the installer again keeps the secrets, the key and the data.

| Variable | Meaning |
|---|---|
| `COOLIFY_RS_CDN` | Where `install.sh`, `upgrade.sh`, the Compose files and `versions.json` are published. Required on the first run, kept afterwards |
| `REGISTRY_URL`, `COOLIFY_RS_IMAGE` | Registry and image name. Default `docker.io` and `coolify-rs/coolify-rs` |
| `COOLIFY_RS_IMAGE_FILE` | Load the image from a `docker save` file instead of pulling it |
| `COOLIFY_RS_PULL=false` | Use the image already on the server |
| `ROOT_USER_EMAIL`, `ROOT_USER_PASSWORD`, `ROOT_USERNAME` | Create the first user during the install |
| `AUTOUPDATE=false`, `DOCKER_ADDRESS_POOL_*` | As in Coolify's installer |

### Install from a local build

On a machine with Docker and this repository, build the image. Its tag must match the version in
`scripts/install/versions.json`:

```bash
VERSION=$(python3 -c "import json; print(json.load(open('scripts/install/versions.json'))['coolify']['v4']['version'])")
docker build -t "coolify-rs/coolify-rs:$VERSION" .
docker save "coolify-rs/coolify-rs:$VERSION" -o coolify-rs.tar
```

Copy `coolify-rs.tar` and the `scripts/install` folder to the server. Then, as root on the server, publish the folder
on an address that both the server and its containers can reach, and run the installer:

```bash
grep -q host.docker.internal /etc/hosts || echo "127.0.0.1 host.docker.internal" >> /etc/hosts
(cd install && nohup python3 -m http.server 8899 --bind 0.0.0.0 > /tmp/coolify-rs-cdn.log 2>&1 &)
curl -fsSL http://host.docker.internal:8899/install.sh |
  COOLIFY_RS_CDN=http://host.docker.internal:8899 COOLIFY_RS_IMAGE_FILE="$PWD/coolify-rs.tar" bash
```

A throwaway virtual machine (Multipass, OrbStack, UTM or a cloud server) is a good place for a first try.

### Upgrade

Use the Upgrade button, turn on automatic updates, or run:

```bash
bash /data/coolify/source/upgrade.sh <version> <helper version>
```

`upgrade.sh` first dumps the database to `/data/coolify/backups/coolify-rs-upgrades/` and stops if that fails. To
restore a dump: `docker exec -i coolify-db pg_restore -U coolify -d coolify --clean --if-exists < <file>`.

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
