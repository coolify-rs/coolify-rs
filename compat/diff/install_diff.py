#!/usr/bin/env python3
"""Compares a clean upstream install with a clean coolify-rs install.

    install_diff.py <upstream dump folder> <coolify-rs dump folder>

The folders come from compat/diff/install/vm_dump.sh on each VM. Generated values are compared by shape.
A difference with a reason in EXPLAINED is printed as explained; any other is reported as unexplained
and makes the exit code non-zero. Values of secrets are never printed.
"""
import json
import re
import sys
from pathlib import Path

up_dir, rs_dir = Path(sys.argv[1]), Path(sys.argv[2])
checks, explained, unexplained = 0, [], []


def same(name, up, rs, reason=None):
    global checks
    checks += 1
    if up == rs:
        return True
    line = f"{name}\n    upstream: {str(up)[:600]}\n    rs:       {str(rs)[:600]}"
    (explained if reason else unexplained).append(line + (f"\n    reason: {reason}" if reason else ""))
    return False


# ---------------------------------------------------------------- schema
LITERAL_TS = re.compile(r"DEFAULT '\d{4}-\d\d-\d\d \d\d:\d\d:\d\d'::timestamp without time zone")


def schema(folder):
    text = (folder / "schema.sql").read_text()
    text = re.sub(r"^-- Dumped .*$", "", text, flags=re.M)
    return LITERAL_TS.sub("DEFAULT '<migration time>'::timestamp without time zone", text)


same("schema (pg_dump -s, literal migration-time defaults normalized)", schema(up_dir), schema(rs_dir))
same("migrations rows", (up_dir / "migrations.txt").read_text(), (rs_dir / "migrations.txt").read_text())
same("migrations sequence", (up_dir / "migrations_seq.txt").read_text(), (rs_dir / "migrations_seq.txt").read_text())

# ---------------------------------------------------------------- rows
TS = re.compile(r"^\d{4}-\d\d-\d\d[T ]\d\d:\d\d:\d\d(\.\d+)?(Z|\+00:00)?$")
CUID = re.compile(r"^[a-z0-9]{24}$")
FREQ = re.compile(r"^\d+ 0 \* \* \*$")
# Timing of what ran on the server since the install, not a seed value.
VOLATILE = {
    "server_settings": {"docker_version", "compose_version", "docker_version_checked_at", "compose_version_checked_at",
                        "patch_check_result", "patch_checked_at", "patch_check_error"},
    "servers": {"sentinel_updated_at", "unreachable_count", "unreachable_notification_sent", "high_disk_usage_notification_sent",
                "traefik_outdated_info", "log_drain_notification_sent", "swarm_cluster", "detected_traefik_version", "validation_logs",
                "server_metadata"},
    "instance_settings": {"public_ipv4", "public_ipv6"},
}


VERSIONS_REASON = (
    "upstream's image ships an older versions.json than its CDN and the comparison blocks the CDN; "
    "coolify-rs embeds the CDN's file (crates/coolify-jobs/versions.json), which upstream also has after its first update check"
)


def shape(table, key, value):
    if isinstance(value, str):
        if TS.match(value):
            return "<timestamp>"
        if CUID.match(value):
            return "<cuid>"
        if value.startswith("eyJ"):
            return "<encrypted>"
        if key == "fingerprint":
            return "<fingerprint %d>" % len(value)
        if key == "auto_update_frequency" and FREQ.match(value):
            return "<minute> 0 * * *"
        if key in ("last_saved_settings", "last_applied_settings") and re.fullmatch(r"[0-9a-f]{32}", value):
            return "<md5>"
    return value


def load_rows(folder):
    rows = {}
    for line in (folder / "rows.jsonl").read_text().splitlines():
        r = json.loads(line)
        if r["table"] == "migrations":
            continue
        rows.setdefault(r["table"], []).append(r["row"])
    return rows


up_rows, rs_rows = load_rows(up_dir), load_rows(rs_dir)
same("tables with rows", sorted(up_rows), sorted(rs_rows))
# Log of the proxy start; the output depends on the image pull.
for table in sorted(set(up_rows) | set(rs_rows)):
    a, b = up_rows.get(table, []), rs_rows.get(table, [])
    if table == "activity_log":
        same("activity_log row count", len(a), len(b))
        continue
    if not same(f"{table}: row count", len(a), len(b)):
        continue
    for i, (x, y) in enumerate(zip(a, b)):
        for key in sorted(set(x) | set(y)):
            if key in VOLATILE.get(table, ()):
                continue
            if table == "servers" and key == "proxy":
                px, py = dict(x[key] or {}), dict(y[key] or {})
                cfg = "last_saved_proxy_configuration"
                same("servers.proxy.configuration", px.pop(cfg, None), py.pop(cfg, None))
                for k in sorted(set(px) | set(py)):
                    same(f"servers.proxy.{k}", shape(table, k, px.get(k)), shape(table, k, py.get(k)))
                continue
            reason = VERSIONS_REASON if (table, key) == ("instance_settings", "helper_version") else None
            same(f"{table}[{i}].{key}", shape(table, key, x.get(key)), shape(table, key, y.get(key)), reason)

same("sequences", (up_dir / "sequences.txt").read_text(), (rs_dir / "sequences.txt").read_text())

# ---------------------------------------------------------------- folders and permissions
DATE = re.compile(r"\d{4}-?\d\d-?\d\d[-_]\d\d-?\d\d-?\d\d(-\d\d)?(\.\d+)?")
TREE_EXPLAINED = {
    "/data/coolify/ssh/keys/ssh_key@<cuid>": "upstream keeps each key on disk (PopulateSshKeysDirectorySeeder); coolify-rs writes a key only for the length of a command (ADR 0009)",
    "/data/coolify/ssh/keys/ssh_key@<cuid>.lock": "same: upstream's lock file for that key",
    "/data/coolify/ssh/mux/mux_<cuid>": "upstream's ssh control socket; coolify-rs does not hold connections open (ADR 0009)",
    "/data/coolify/ssh/home": "coolify-rs: HOME of the ssh client (ADR 0009)",
    "/data/coolify/ssh/known_hosts": "coolify-rs: pinned host keys (ADR 0009)",
}


def tree(folder):
    out = {}
    for line in (folder / "tree.txt").read_text().splitlines():
        kind, mode, owner, size, path = line.split(" ", 4)
        path = DATE.sub("<date>", path)
        path = re.sub(r"(ssh_key@|mux_)[a-z0-9]{24}", r"\1<cuid>", path)
        path = re.sub(r"known_hosts/[a-z0-9]{24}", "known_hosts/<cuid>", path)
        if "sentinel/metrics.sqlite" in path:
            mode = "<sentinel's own>"
        out[path] = f"{kind} {mode} {owner}"
    return out


up_tree, rs_tree = tree(up_dir), tree(rs_dir)
for path in sorted(set(up_tree) | set(rs_tree)):
    reason = next((r for p, r in TREE_EXPLAINED.items() if path == p or path.startswith(p + "/")), None)
    same(f"tree {path}", up_tree.get(path), rs_tree.get(path), reason)

for name in ("root_ssh.txt", "authorized_keys.txt", "daemon.json", "networks.txt", "volumes.txt"):
    same(name, (up_dir / name).read_text(), (rs_dir / name).read_text())


# ---------------------------------------------------------------- containers
def ps(folder):
    return [re.sub(r"\|.*coolify:.*", "|<coolify image>", l) for l in (folder / "ps.txt").read_text().splitlines()]


def sentinel_free(names):
    return [re.sub(r"(coollabsio/sentinel:).*", r"\1<version>", l) for l in names]


same("containers and images", sentinel_free(ps(up_dir)), sentinel_free(ps(rs_dir)))
same("sentinel image", ps(up_dir), ps(rs_dir), VERSIONS_REASON)
up_c = {c["Name"]: c for c in json.loads((up_dir / "containers.json").read_text())}
rs_c = {c["Name"]: c for c in json.loads((rs_dir / "containers.json").read_text())}
for name in sorted(up_c):
    a, b = up_c[name], rs_c[name]
    same(f"{name} restart policy", a["HostConfig"]["RestartPolicy"], b["HostConfig"]["RestartPolicy"])
    same(f"{name} networks", sorted(a["NetworkSettings"]["Networks"]), sorted(b["NetworkSettings"]["Networks"]))
    same(f"{name} extra hosts", sorted(a["HostConfig"]["ExtraHosts"] or []), sorted(b["HostConfig"]["ExtraHosts"] or []))
    same(f"{name} health", a["State"].get("Health", {}).get("Status"), b["State"].get("Health", {}).get("Status"))
    is_app = name == "/coolify"
    same(f"{name} ports", sorted(a["HostConfig"]["PortBindings"]), sorted(b["HostConfig"]["PortBindings"]),
         "upstream's 6001 and 6002 are its Reverb and node terminal servers; coolify-rs serves both on 8080" if is_app else None)
    same(f"{name} mounts", sorted(m["Destination"] for m in a["Mounts"]), sorted(m["Destination"] for m in b["Mounts"]),
         "the application's own paths inside the container" if is_app else None)
    same(f"{name} mount sources", sorted(m["Source"] for m in a["Mounts"]), sorted(m["Source"] for m in b["Mounts"]),
         "coolify-rs reaches applications, databases, services and backups over ssh, and reads no env file" if is_app else None)


# ---------------------------------------------------------------- secrets and settings, by shape
def env(container):
    return dict(e.split("=", 1) for e in container["Config"]["Env"])


def env_shape(v):
    cls = "hex" if re.fullmatch(r"[0-9a-f]+", v) else "base64" if re.fullmatch(r"[A-Za-z0-9+/]+=*", v) else "base64:" if v.startswith("base64:") else "other"
    return f"{cls}/{len(v)}"


SECRETS = ["APP_ID", "APP_KEY", "DB_PASSWORD", "REDIS_PASSWORD", "PUSHER_APP_ID", "PUSHER_APP_KEY", "PUSHER_APP_SECRET"]
up_env, rs_env = env(up_c["/coolify"]), env(rs_c["/coolify"])
for key in SECRETS:
    same(f"secret shape {key}", env_shape(up_env.get(key, "")), env_shape(rs_env.get(key, "")))
for key in ["DB_USERNAME", "DOCKER_ADDRESS_POOL_BASE", "DOCKER_ADDRESS_POOL_SIZE", "PUSHER_BACKEND_PORT", "REGISTRY_URL", "ROOT_USERNAME", "ROOT_USER_EMAIL", "ROOT_USER_PASSWORD"]:
    same(f"setting {key}", up_env.get(key), rs_env.get(key))
same("setting APP_NAME", up_env.get("APP_NAME"), rs_env.get("APP_NAME"), "own name")

print(f"{checks - len(unexplained) - len(explained)}/{checks} identical, {len(explained)} explained, {len(unexplained)} unexplained")
for line in explained:
    print("EXPLAINED", line)
for line in unexplained:
    print("DIFF", line)
sys.exit(1 if unexplained else 0)
