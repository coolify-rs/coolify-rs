# Migrating from Coolify to coolify-rs

coolify-rs uses Coolify's database, folders, secrets and SSH keys as they are. Moving a Coolify server to coolify-rs
replaces the Coolify containers and keeps everything else: your users, teams, projects, servers, applications,
databases, services, backups, environment variables and settings. Your applications and databases keep running during
the switch, because they are separate containers and the proxy is not restarted.

You can go back to Coolify the same way (see [Going back to Coolify](#going-back-to-coolify)).

## Which Coolify versions can move

A coolify-rs release matches exactly one Coolify release, named in its version: **coolify-rs `4.6.0-rs.3` takes over
Coolify `4.6.0`**.

| Your Coolify | What to do |
|---|---|
| 4.6.0 | Migrate as described below. |
| Older than 4.6.0 | Upgrade Coolify to 4.6.0 first: `curl -fsSL https://cdn.coollabs.io/coolify/install.sh \| sudo bash -s 4.6.0`. Then migrate. |
| Newer than 4.6.0 | Not yet. Coolify cannot be downgraded, and coolify-rs starts read-only on a database with migrations it does not know. Wait for the coolify-rs release that matches your version. |

On Coolify 4.5.3, coolify-rs would also start: on its first start it applies Coolify 4.6.0's own schema changes, as
Coolify's upgrade would. From then on only Coolify 4.6.0 can take the database back, so upgrading Coolify first is the
simpler way.

To see your version, open Settings in Coolify, or run:

```bash
docker inspect coolify --format '{{.Config.Image}}'
```

coolify-rs does not change Coolify's database tables. When coolify-rs starts on a database it was not built for, it
refuses writes and says why in the dashboard and the log, instead of risking your data.

## Before you start

1. **Read [KNOWN_LIMITATIONS.md](KNOWN_LIMITATIONS.md).** It lists every difference from Coolify we know of. Make sure
   none of them matters for you.
2. **Turn off Coolify's automatic updates** (Settings → Update), so Coolify does not move past 4.6.0 before you migrate.
3. **Wait until nothing is running:** no deployment, backup, scheduled task or server transfer in progress. Coolify's
   queue is in Redis, and coolify-rs runs its own, so work Coolify has queued but not started is not carried over.
4. **Back up the database and the secrets**, as root on the Coolify server:

   ```bash
   mkdir -p /root/coolify-backup
   docker exec coolify-db pg_dump -U coolify -d coolify -Fc > /root/coolify-backup/coolify-4.6.0.dump
   cp -a /data/coolify/source /root/coolify-backup/source
   ```

   `/data/coolify/source` holds the `.env` file with your `APP_KEY`. Without it, the database cannot be read. Copy the
   backup off the server too.
5. **Ask coolify-rs whether it can take this database**, without changing anything. This runs the coolify-rs image
   once, next to Coolify, and only reads the `migrations` table:

   ```bash
   docker run --rm --network coolify --env-file /data/coolify/source/.env -e DB_HOST=coolify-db \
     ghcr.io/coolify-rs/coolify-rs:4.6.0-rs.3 check-schema
   ```

   `"read_only_reason": null` means it can. Anything else says why not; nothing has changed, and Coolify keeps running.

## Migrate

On the Coolify server, as root:

```bash
curl -fsSL https://github.com/coolify-rs/coolify-rs/releases/latest/download/install.sh | sudo bash
```

This is the normal coolify-rs installer. On a Coolify server it:

- keeps `/data/coolify/source/.env`, with your `APP_KEY`, database password and other secrets, and saves a copy of it
  as `.env-<date>` first;
- keeps the `coolify-db` and `coolify-redis` volumes, the SSH keys in `/data/coolify/ssh`, and the proxy;
- replaces the Compose files in `/data/coolify/source` with coolify-rs's, and keeps your `docker-compose.custom.yml`;
- replaces the `coolify` container with coolify-rs, which also answers to the `coolify-realtime` name, so existing proxy
  settings keep working;
- adds its own settings to `.env`: the `COOLIFY_RS_*` lines.

The switch takes a minute or two. Everyone is signed out.

## Check it

1. Open the dashboard at the same address and sign in with your Coolify account. Settings shows version `4.6.0-rs.3`.
2. `docker exec coolify coolify-rs check-schema` shows `"read_only_reason": null`. If not, the reason is there (usually
   a Coolify version other than 4.6.0); [go back to Coolify](#going-back-to-coolify) and see the version table above.
3. Open each server and click **Validate**. They should be reachable with their existing keys.
4. Redeploy one application, and run one database backup, to see a full round trip.

The coolify-rs log: `docker logs coolify`.

## Going back to Coolify

coolify-rs writes rows exactly as Coolify does, so Coolify 4.6.0 can take the database back, including what you changed
while on coolify-rs. First let coolify-rs confirm it:

```bash
docker exec coolify coolify-rs check-portable
```

It exits 0 when Coolify can take the database back, and otherwise lists the rows to fix (for example a resource a
plugin created that Coolify would not understand). Then run Coolify's own installer for the same version:

```bash
curl -fsSL https://cdn.coollabs.io/coolify/install.sh | sudo bash -s 4.6.0
```

It keeps the same `.env`, database and keys, and brings back Coolify's containers. The `COOLIFY_RS_*` lines in `.env`
are ignored by Coolify.

If something went wrong with the data itself, restore your backup after going back:

```bash
docker exec -i coolify-db pg_restore -U coolify -d coolify --clean --if-exists < /root/coolify-backup/coolify-4.6.0.dump
docker restart coolify
```

## After migrating

Upgrades come from coolify-rs releases: the Upgrade button, automatic updates, or `upgrade.sh`, as in
[INSTALL.md](INSTALL.md#upgrade). Each coolify-rs upgrade dumps the database first. Coolify's own update channel no
longer applies; do not run Coolify's installer for a newer Coolify on this server unless you mean to leave coolify-rs.
