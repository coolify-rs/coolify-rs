# Installing coolify-rs

coolify-rs installs the way Coolify does: one script, run as root on a server. It installs Docker and OpenSSH if
needed, creates the `/data/coolify` folders, generates secrets and an SSH key for the server, and starts coolify-rs with
Docker Compose.

Moving an existing Coolify server to coolify-rs is a different procedure: see [MIGRATING.md](MIGRATING.md).

## Requirements

- A clean server with at least 2 CPU cores, 2 GB of memory and 30 GB of disk.
- amd64 or arm64. Ubuntu LTS is tested. Like Coolify's installer, the script also handles Debian, the RHEL family,
  SUSE, Arch and Alpine.
- Root access, and ports 22 (SSH), 80 and 443 (the proxy) and 8000 (the dashboard) reachable.

## Install

```bash
curl -fsSL https://github.com/coolify-rs/coolify-rs/releases/latest/download/install.sh | sudo bash
```

This installs the newest release. To install a particular one, pass its version:

```bash
curl -fsSL https://github.com/coolify-rs/coolify-rs/releases/latest/download/install.sh | sudo bash -s 4.7.5-rs.2
```

When the script finishes, open `http://<your-server>:8000` and register. The first account owns the instance.
Registration stays open for 10 minutes after each start; `docker restart coolify` opens it again. Then add a server,
create a project and deploy.

**Back up `/data/coolify/source/.env` somewhere safe.** It holds the `APP_KEY` that encrypts your secrets. Without it,
the database cannot be read. Running the installer again keeps the secrets, the key and the data.

### Check the files before running them

Each release lists a `SHA256SUMS` file next to the installer files. To read the script before running it:

```bash
curl -fsSLO https://github.com/coolify-rs/coolify-rs/releases/latest/download/install.sh
curl -fsSLO https://github.com/coolify-rs/coolify-rs/releases/latest/download/SHA256SUMS
sha256sum --check --ignore-missing SHA256SUMS
less install.sh
sudo bash install.sh
```

### Installer settings

Set these in the environment of `bash`, for example `curl ... | sudo AUTOUPDATE=false bash`.

| Variable | Meaning |
|---|---|
| `COOLIFY_RS_CDN` | Where `install.sh`, `upgrade.sh`, the Compose files and `versions.json` come from. Default: the latest GitHub release. Kept for later upgrades |
| `COOLIFY_RS_REGISTRY`, `COOLIFY_RS_IMAGE` | Registry and name of the coolify-rs image. Default `ghcr.io` and `coolify-rs/coolify-rs` |
| `COOLIFY_RS_IMAGE_FILE` | Load the image from a `docker save` file instead of pulling it |
| `COOLIFY_RS_PULL=false` | Use the image already on the server |
| `REGISTRY_URL` | Registry of Coolify's helper image. Default `docker.io`, as in Coolify |
| `ROOT_USER_EMAIL`, `ROOT_USER_PASSWORD`, `ROOT_USERNAME` | Create the first user during the install |
| `AUTOUPDATE=false` | Turn off automatic updates |
| `DOCKER_ADDRESS_POOL_BASE`, `DOCKER_ADDRESS_POOL_SIZE`, `DOCKER_POOL_FORCE_OVERRIDE` | Docker's address pool, as in Coolify's installer |

## Upgrade

Use the Upgrade button in the dashboard, turn on automatic updates in Settings, or run on the server:

```bash
bash /data/coolify/source/upgrade.sh <version> <helper version>
```

The versions are in the newest release's
[`versions.json`](https://github.com/coolify-rs/coolify-rs/releases/latest/download/versions.json).
`upgrade.sh` first dumps the database to `/data/coolify/backups/coolify-rs-upgrades/`, and stops if that fails. To
restore a dump:

```bash
docker exec -i coolify-db pg_restore -U coolify -d coolify --clean --if-exists < <dump file>
```

Upgrading signs everyone out, because coolify-rs keeps sessions in memory.

## Versions

A coolify-rs version names the Coolify release it matches, plus its own counter: `4.6.0-rs.1` is the first coolify-rs
release that matches Coolify 4.6.0. The dashboard and the API report this version. The database schema is Coolify's,
so a coolify-rs release only works with a database at its Coolify version (see [MIGRATING.md](MIGRATING.md)).

Images: `ghcr.io/coolify-rs/coolify-rs:<version>` and `:latest`, for `linux/amd64` and `linux/arm64`.

## Install from a local build

To try a change before it is released, build the image on a machine with Docker and this repository. Its tag must match
the version in `scripts/install/versions.json`:

```bash
VERSION=$(python3 -c "import json; print(json.load(open('scripts/install/versions.json'))['coolify']['v4']['version'])")
docker build -t "coolify-rs/coolify-rs:$VERSION" .
docker save "coolify-rs/coolify-rs:$VERSION" -o coolify-rs.tar
```

Copy `coolify-rs.tar` and the `scripts/install` folder to the server. Then, as root on the server, publish the folder at
an address that both the server and its containers can reach, and run the installer from it:

```bash
grep -q host.docker.internal /etc/hosts || echo "127.0.0.1 host.docker.internal" >> /etc/hosts
(cd install && nohup python3 -m http.server 8899 --bind 0.0.0.0 > /tmp/coolify-rs-cdn.log 2>&1 &)
curl -fsSL http://host.docker.internal:8899/install.sh |
  COOLIFY_RS_CDN=http://host.docker.internal:8899 COOLIFY_RS_REGISTRY=docker.io COOLIFY_RS_IMAGE_FILE="$PWD/coolify-rs.tar" bash
```

A throwaway virtual machine (Multipass, OrbStack, UTM or a cloud server) is a good place for a first try.
