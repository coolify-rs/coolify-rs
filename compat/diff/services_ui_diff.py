#!/usr/bin/env python3
"""Diffs the rows of two services made through the two UIs: services_ui_diff.py <ref uuid> <rs uuid>.

Volatile values (ids, uuids, timestamps, generated secrets) are normalized."""
import json
import os
import re
import subprocess
import sys

VOLATILE = {"id", "uuid", "created_at", "updated_at", "resource_id", "service_id", "resourceable_id", "last_online_at", "order"}


def sql(q):
    return subprocess.run(
        ["docker", "exec", "coolify-rs-reference-postgres-1", "psql", "-U", "coolify", "-d", "coolify", "-At", "-c", q],
        check=True, capture_output=True, text=True,
    ).stdout.strip()


def rows(q):
    out = sql(f"SELECT coalesce(json_agg(t), '[]') FROM ({q}) t")
    return json.loads(out)


def norm(v, uuids):
    if isinstance(v, str):
        for u, name in uuids.items():
            v = v.replace(u, name)
    return v


def clean(row, uuids, drop=()):
    return {k: norm(v, uuids) for k, v in row.items() if k not in VOLATILE and k not in drop}


def dump(uuid):
    s = rows(f"SELECT * FROM services WHERE uuid = '{uuid}'")[0]
    uuids = {uuid: "<SERVICE>"}
    apps = rows(f"SELECT * FROM service_applications WHERE service_id = {s['id']} ORDER BY name")
    dbs = rows(f"SELECT * FROM service_databases WHERE service_id = {s['id']} ORDER BY name")
    for p in apps + dbs:
        uuids[p["uuid"]] = f"<PART {p['name']}>"
    out = {
        "service": clean(s, uuids, ("docker_compose", "config_hash")),
        "applications": [clean(a, uuids, ("fqdn",)) for a in apps],
        "databases": [clean(d, uuids) for d in dbs],
    }
    envs = rows(
        f"SELECT * FROM environment_variables WHERE resourceable_type = 'App\\Models\\Service' AND resourceable_id = {s['id']} ORDER BY key, is_preview"
    )
    out["envs"] = [clean(e, uuids, ("value",)) | {"has_value": bool(e["value"])} for e in envs]
    out["persistent"] = []
    out["files"] = []
    for kind, parts in (("App\\Models\\ServiceApplication", apps), ("App\\Models\\ServiceDatabase", dbs)):
        for p in parts:
            out["persistent"] += [
                clean(r, uuids)
                for r in rows(f"SELECT * FROM local_persistent_volumes WHERE resource_type = '{kind}' AND resource_id = {p['id']} ORDER BY name")
            ]
            out["files"] += [
                clean(r, uuids, ("content",)) | {"has_content": bool(r["content"])}
                for r in rows(f"SELECT * FROM local_file_volumes WHERE resource_type = '{kind}' AND resource_id = {p['id']} ORDER BY mount_path")
            ]
    return out


def walk(a, b, path, diffs):
    if isinstance(a, dict) and isinstance(b, dict):
        for k in list(a) + [k for k in b if k not in a]:
            walk(a.get(k, "<absent>"), b.get(k, "<absent>"), f"{path}.{k}", diffs)
    elif isinstance(a, list) and isinstance(b, list):
        if len(a) != len(b):
            diffs.append((path + ".len", len(a), len(b)))
        for i, (x, y) in enumerate(zip(a, b)):
            walk(x, y, f"{path}[{i}]", diffs)
    elif a != b:
        diffs.append((path, a, b))


def main():
    ref, rs = dump(sys.argv[1]), dump(sys.argv[2])
    diffs = []
    walk(ref, rs, "", diffs)
    for path, a, b in diffs:
        print(path, "\n  ref:", str(a)[:300], "\n  rs: ", str(b)[:300])
    print(f"{len(diffs)} differences; {len(ref['envs'])} variables, {len(ref['applications'])} applications, {len(ref['databases'])} databases, {len(ref['persistent'])} volumes, {len(ref['files'])} files")


if __name__ == "__main__":
    main()
